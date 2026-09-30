//! 终端字节流的**增量解码**（M7.4 同批必做项：非 UTF-8 编码）。
//!
//! # 缺陷本体
//!
//! `Profile.term.encoding` 这个字段一直存在、界面上可填可存，却**没有任何消费方**——
//! 全仓找不到一处读它去改变字节解释的代码。于是国产板子/老服务器输出 GBK 时是整屏乱码，
//! 而用户明明在连接属性里选过 GBK。串口那条路上这不是「体验差」而是「做了也用不了」：
//! 裸板的中文提示全是 GBK。
//!
//! # 为什么必须是**增量**解码器
//!
//! 一次 read 返回的块是按**网络/驱动**边界切的，不是按字符边界切的：一个 GBK 汉字的两个
//! 字节完全可能落在两次 read 里。逐块独立解码的写法（`GBK.decode(chunk)`）会把落单的那个
//! 字节当成非法序列换成 U+FFFD，然后下一块的后半个字节又是非法的——**每次切割损坏两个字符**，
//! 而这在长输出里每几 KiB 就发生一次。`encoding_rs::Decoder` 把这半个字符留在自己状态里，
//! 下一块进来时接上，所以它必须**跨块存活**（即每个会话一个，不是每块新建一个）。
//!
//! # UTF-8 是**恒等透传**，不是「解码成 UTF-8」
//!
//! 默认路径（UTF-8）一个字节都不碰、零拷贝原样返回。理由有二：
//! ① 性能——这是每一帧都要过的路；
//! ② **正确性**：走一遍解码器等于把非法 UTF-8 字节替换成 U+FFFD。今天 xterm.js 自己处理
//!    不完整/非法序列（它同样是增量的），而终端流里混着的二进制（ZMODEM 残帧、`cat` 了一个
//!    可执行文件）在替换之后就再也回不来了。**不改变现有行为**是这一档的硬要求。
//!
//! # 与 ZMODEM 拦截器的次序
//!
//! 解码站在 `ByteTap` **之后**（见 `pipe.rs` 的读取循环）：传输期间拦截器已经把协议帧吞掉了，
//! 解码器看不到二进制帧。反过来（先解码后拦截）会让 GBK 解码器啃 ZMODEM 的二进制头，
//! 把 `**\x18B00` 里的字节换成 U+FFFD，拦截器再也认不出帧头——传输直接坏掉。

use std::borrow::Cow;

/// 一个会话的解码状态。
///
/// `None` 的 `dec` = UTF-8 恒等透传（见模块头注）。非 UTF-8 时持有一个**有状态**的
/// `encoding_rs::Decoder`，跨块保住半个多字节字符。
pub struct StreamDecoder {
    /// 当前编码的规范名（`"UTF-8"` / `"GBK"` / `"Big5"`…），用于回报给界面。
    name: &'static str,
    dec: Option<encoding_rs::Decoder>,
}

impl std::fmt::Debug for StreamDecoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // encoding_rs::Decoder 不是 Debug；只打编码名足够定位问题。
        f.debug_struct("StreamDecoder")
            .field("name", &self.name)
            .finish()
    }
}

impl Default for StreamDecoder {
    fn default() -> Self {
        Self {
            name: "UTF-8",
            dec: None,
        }
    }
}

impl StreamDecoder {
    /// 按标签建一个解码器。标签按 WHATWG 编码标准解析，故 `gb2312`/`gbk`/`GB2312` 等
    /// 常见写法都落到同一个编码上——用户在连接属性里填哪种写法都不该有区别。
    ///
    /// `None` 或空串 = UTF-8（恒等透传）。认不出的标签**报错而不是静默回落 UTF-8**：
    /// 静默回落的表现是「我明明选了 GBK，还是乱码」，而用户无从知道是标签拼错了。
    pub fn new(label: Option<&str>) -> Result<Self, String> {
        match label.map(str::trim).filter(|s| !s.is_empty()) {
            None => Ok(Self::default()),
            Some(l) => {
                let enc = encoding_rs::Encoding::for_label(l.as_bytes())
                    .ok_or_else(|| format!("未知的终端编码「{l}」"))?;
                Ok(if enc == encoding_rs::UTF_8 {
                    Self::default()
                } else {
                    Self {
                        name: enc.name(),
                        // without_bom_handling：终端流不是文件，开头那几个字节是 shell 的输出而不是
                        // BOM；带 BOM 处理会在极少数情况下吞掉正文的头三个字节。
                        dec: Some(enc.new_decoder_without_bom_handling()),
                    }
                })
            }
        }
    }

    /// 当前编码的规范名。
    pub fn name(&self) -> &'static str {
        self.name
    }

    /// 是不是恒等透传（UTF-8）。
    pub fn is_passthrough(&self) -> bool {
        self.dec.is_none()
    }

    /// 换编码。**状态一并重置**：留着的半个字符属于旧编码，接到新编码上只会解出一个错字。
    ///
    /// 切换不需要重连——这是出口标准的原话。解码站在读取循环里，换掉它下一块就生效，
    /// 而会话、滚动缓冲、正在跑的程序全都不受影响。
    pub fn set_label(&mut self, label: Option<&str>) -> Result<(), String> {
        *self = Self::new(label)?;
        Ok(())
    }

    /// 解一块。返回可直接送进终端的 UTF-8 字节。
    ///
    /// UTF-8 档零拷贝借用原块；其余档返回新分配的 UTF-8。空块直接借用返回——
    /// 空块喂给解码器不会有输出，但会白走一趟。
    pub fn decode<'a>(&mut self, chunk: &'a [u8]) -> Cow<'a, [u8]> {
        let Some(dec) = self.dec.as_mut() else {
            return Cow::Borrowed(chunk);
        };
        if chunk.is_empty() {
            return Cow::Borrowed(chunk);
        }
        // 上界由 encoding_rs 给：它保证这个容量足够装下本块的全部输出（含把非法序列换成
        // U+FFFD 之后的膨胀）。拿不到上界（理论上的 usize 溢出）时按 4 倍兜底。
        let cap = dec
            .max_utf8_buffer_length(chunk.len())
            .unwrap_or(chunk.len().saturating_mul(4));
        let mut out = String::with_capacity(cap);
        // last=false：流还没结束。结束时那半个字符会随会话一起消失——那是对的，
        // 它本来就没有完整地到达过。
        let (_res, _read, _had_errors) = dec.decode_to_string(chunk, &mut out, false);
        Cow::Owned(out.into_bytes())
    }
}

/// 界面上给用户挑的编码清单（值 = 传给 [`StreamDecoder::new`] 的标签）。
///
/// 只列**终端上真的会遇到**的几种，不把 WHATWG 的全表倒出来：一屏几十个编码名里找 GBK
/// 比自己打字还慢。顺序按中文用户的命中率排。
pub const TERM_ENCODINGS: &[(&str, &str)] = &[
    ("utf-8", "UTF-8（默认）"),
    ("gbk", "GBK / GB2312（简体中文）"),
    ("gb18030", "GB18030（简体中文，含生僻字）"),
    ("big5", "Big5（繁体中文）"),
    ("shift_jis", "Shift_JIS（日文）"),
    ("euc-kr", "EUC-KR（韩文）"),
    ("windows-1252", "Windows-1252（西欧）"),
];

#[cfg(test)]
mod tests {
    use super::*;

    /// GBK 的「中文」两个字：D6 D0 CE C4。
    const ZHONGWEN_GBK: &[u8] = &[0xD6, 0xD0, 0xCE, 0xC4];

    #[test]
    fn utf8_is_byte_for_byte_passthrough() {
        let mut d = StreamDecoder::new(None).unwrap();
        assert!(d.is_passthrough());
        let raw = b"\x1b[31m\xe4\xb8\xad\xe6\x96\x87\x1b[0m";
        assert!(
            matches!(d.decode(raw), Cow::Borrowed(_)),
            "UTF-8 档必须零拷贝借用"
        );
        assert_eq!(&*d.decode(raw), raw);
    }

    /// UTF-8 档**不得**把非法字节改成 U+FFFD：终端流里混着二进制是常态，
    /// 而替换是不可逆的（今天 xterm 自己处理这些，行为不能变）。
    #[test]
    fn utf8_does_not_sanitize_invalid_bytes() {
        let mut d = StreamDecoder::new(Some("utf-8")).unwrap();
        let broken = &[0x41, 0xff, 0xfe, 0x42];
        assert_eq!(&*d.decode(broken), broken);
    }

    #[test]
    fn gbk_decodes_to_utf8() {
        let mut d = StreamDecoder::new(Some("gbk")).unwrap();
        assert_eq!(d.name(), "GBK");
        assert_eq!(
            String::from_utf8(d.decode(ZHONGWEN_GBK).into_owned()).unwrap(),
            "中文"
        );
    }

    /// 本模块存在的理由：一个汉字被切成两块。
    #[test]
    fn multibyte_char_split_across_chunks_survives() {
        let mut d = StreamDecoder::new(Some("gbk")).unwrap();
        let mut out = Vec::new();
        out.extend_from_slice(&d.decode(&ZHONGWEN_GBK[..1])); // 半个「中」
        out.extend_from_slice(&d.decode(&ZHONGWEN_GBK[1..3])); // 「中」的后半 + 半个「文」
        out.extend_from_slice(&d.decode(&ZHONGWEN_GBK[3..])); // 「文」的后半
        assert_eq!(String::from_utf8(out).unwrap(), "中文");
    }

    /// 逐字节喂（最坏切法）也必须完整。
    #[test]
    fn byte_by_byte_still_decodes() {
        let mut d = StreamDecoder::new(Some("gbk")).unwrap();
        let mut out = Vec::new();
        for b in ZHONGWEN_GBK {
            out.extend_from_slice(&d.decode(&[*b]));
        }
        assert_eq!(String::from_utf8(out).unwrap(), "中文");
    }

    /// ASCII 与 ANSI 转义序列在 GBK 档下必须原样通过——否则颜色/光标控制全废。
    #[test]
    fn ansi_escapes_pass_through_under_gbk() {
        let mut d = StreamDecoder::new(Some("gbk")).unwrap();
        let esc = b"\x1b[1;32muser@host\x1b[0m:~$ ";
        assert_eq!(&*d.decode(esc), esc);
    }

    #[test]
    fn labels_are_whatwg_aliases() {
        for l in ["gbk", "GBK", "gb2312", "GB2312", " gbk "] {
            assert_eq!(StreamDecoder::new(Some(l)).unwrap().name(), "GBK", "{l}");
        }
        assert_eq!(
            StreamDecoder::new(Some("gb18030")).unwrap().name(),
            "gb18030"
        );
        assert_eq!(StreamDecoder::new(Some("big5")).unwrap().name(), "Big5");
    }

    /// 空串/None = UTF-8；而**认不出的标签要报错**，不是静默回落——
    /// 静默回落的表现是「我明明选了 GBK，还是乱码」。
    #[test]
    fn unknown_label_is_an_error_not_a_silent_fallback() {
        assert!(StreamDecoder::new(Some("")).unwrap().is_passthrough());
        assert!(StreamDecoder::new(None).unwrap().is_passthrough());
        let e = StreamDecoder::new(Some("gbk2")).unwrap_err();
        assert!(e.contains("gbk2"), "错误里要带上用户填的那个标签：{e}");
    }

    /// 切换编码要**丢掉**旧编码留下的半个字符：接到新编码上只会解出一个错字。
    #[test]
    fn switching_encoding_resets_pending_state() {
        let mut d = StreamDecoder::new(Some("gbk")).unwrap();
        assert!(d.decode(&ZHONGWEN_GBK[..1]).is_empty()); // 半个字符留在状态里
        d.set_label(Some("utf-8")).unwrap();
        assert!(d.is_passthrough());
        // 切回 GBK 之后，前面那半个字节不得再冒出来
        d.set_label(Some("gbk")).unwrap();
        assert_eq!(
            String::from_utf8(d.decode(ZHONGWEN_GBK).into_owned()).unwrap(),
            "中文"
        );
    }

    #[test]
    fn empty_chunk_is_borrowed_and_empty() {
        let mut d = StreamDecoder::new(Some("gbk")).unwrap();
        assert!(matches!(d.decode(&[]), Cow::Borrowed(_)));
        assert!(d.decode(&[]).is_empty());
    }

    /// 界面清单里的每个标签都要真的能建出解码器（列一个建不出来的等于给用户挖坑）。
    #[test]
    fn every_offered_encoding_is_constructible() {
        for (label, human) in TERM_ENCODINGS {
            let d = StreamDecoder::new(Some(label)).unwrap_or_else(|e| panic!("{label}: {e}"));
            assert!(!human.is_empty());
            if *label == "utf-8" {
                assert!(d.is_passthrough());
            } else {
                assert!(!d.is_passthrough(), "{label} 不该是透传");
            }
        }
    }
}
