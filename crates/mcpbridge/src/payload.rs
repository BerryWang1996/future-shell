//! `terminal.send` 的 payload 不透明度判定（总设计 §4.5）。
//!
//! 规格原文：「分类目标 = 完整 payload 字符串；payload 含控制/不可打印字符
//! （CR、ESC、Ctrl 序列、ANSI CSI）或超过阈值（默认 4 KiB）→ 按 `dangerous` 处理」。
//!
//! ## 这条规则为什么不是多余的
//!
//! 直觉上「反正 payload 会过 `fs_policy::classify`，危险的自然会被判危险」。
//! **实测不成立**——分类器对控制字符有系统性盲点，下面每一条都在
//! `the_classifier_alone_is_fooled_by_every_one_of_these` 里跑真分类器复现：
//!
//! | payload | `classify` 判 | 终端实际执行 |
//! |---|---|---|
//! | `ls\r` + `rm -rf /` | `ReadOnly` | 两条命令，第二条删根 |
//! | `echo safe` + 4×退格 + `rm -rf /` | `ReadOnly` | 退格擦掉伪装 |
//! | `echo ` + RLO + 反写的命令 | `ReadOnly` | 显示与内容相反 |
//! | `ESC[2K` + `rm -rf /` | `Write` | 危险规则被前缀打掉 |
//! | Ctrl-C + `rm -rf /` | `Write` | 同上 |
//! | `r` + 零宽空格 + `m -rf /` | `Write` | 同上 |
//!
//! 也就是说：**不加这条规则，`terminal.send` 就是一个绕开策略引擎的通道。**
//!
//! ## 换行是唯一的例外，且这个例外有实证
//!
//! LF 是「回车键」——把它判成不透明，等于每一次正常的 `terminal.send("ls\n")`
//! 都要走强确认，而那会把强确认训练成一个「照点不误」的动作，比不设强确认更坏。
//!
//! 放行 LF 的**前提**是分类器真的看得懂多行脚本。上表最后一行给了反证：
//! `"ls\nrm -rf /"` → `Dangerous`（两条规则同时命中）。换行不是盲点，
//! 它是分类器的正常输入。这条前提由 `newline_is_the_one_control_char_the_classifier_understands`
//! 每次跑测试时重新验证——哪天 shell 解析器改了口径，那条会先红。
//!
//! ## TAB 不在例外里
//!
//! 制表符是人会敲的键，但它在终端里触发**补全**：最终跑的是哪串字节由远端
//! shell 按远端文件系统状态决定，不由 payload 决定。那正是「分类器看不到将要
//! 执行什么」的定义，所以 TAB 归不透明（由 `char::is_control` 自然覆盖）。

/// 超过这个字节数即按不透明处理（规格：「默认 4 KiB」）。
///
/// 它**小于** `fs_policy::shell::MAX_INPUT_LEN`（16 KiB，超过即解析失败→
/// `Dangerous`）。这个大小关系是刻意的：超长 payload 在这里就拿到一个说得出
/// 口的理由（「超过 4 KiB」），而不是掉进分类器的 `parse_error` 兜底——
/// 两者都导向 `Dangerous`，但确认框上写「命令没法解析」会让用户以为是我们的
/// bug，写「这段有 9 KiB」他才知道该看什么。
pub const MAX_PAYLOAD_BYTES: usize = 4 * 1024;

/// payload 能不能交给分类器判断。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Opacity {
    /// 可以交给 `fs_policy::classify`——它看到的与终端将要执行的是同一串。
    Transparent,
    /// 看不透。恒 `Dangerous`，**不再问分类器的意见**：分类器对这种输入的
    /// 结论已被证明不可信（见模块头那张表），拿一个不可信的 `ReadOnly`
    /// 去和 `Dangerous` 取严没有意义，取严的结果永远是 `Dangerous`，
    /// 而多跑一次分类只会在审计里留下一个误导性的「分类结论」。
    Opaque(OpaqueReason),
}

impl Opacity {
    /// 是否不透明。
    pub fn is_opaque(&self) -> bool {
        matches!(self, Self::Opaque(_))
    }

    /// 不透明的理由（透明时 `None`）。
    pub fn reason(&self) -> Option<&OpaqueReason> {
        match self {
            Self::Transparent => None,
            Self::Opaque(r) => Some(r),
        }
    }
}

/// 为什么看不透。**带位置**——确认框要能告诉用户「第几个字节上有个 CR」，
/// 光说「含控制字符」用户无从判断这是自己粘错了还是被人塞了东西。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpaqueReason {
    /// C0/C1 控制字符（LF 除外）。`at` 是字节偏移。
    ControlChar { at: usize, code: u32 },
    /// 不可见的格式字符：双向覆盖、零宽、BOM、TAG 区等。
    InvisibleFormat { at: usize, code: u32 },
    /// 超过 [`MAX_PAYLOAD_BYTES`]。
    TooLong { bytes: usize },
}

impl OpaqueReason {
    /// 给人看的一句话。直接进确认框与审计行。
    ///
    /// **只报码位与位置，不报周围的原文。** payload 可能是用户正要敲进终端的
    /// 密码；控制字符的码位本身不是秘密（CR 就是 CR），但它左右的字节是。
    pub fn detail(&self) -> String {
        match self {
            Self::ControlChar { at, code } => format!(
                "第 {at} 字节处有控制字符 U+{code:04X}——策略引擎看到的命令与终端将要执行的不是同一串"
            ),
            Self::InvisibleFormat { at, code } => format!(
                "第 {at} 字节处有不可见格式字符 U+{code:04X}（双向覆盖/零宽/TAG 区之类）——屏幕上显示的与实际内容可能不同"
            ),
            Self::TooLong { bytes } => format!(
                "payload 有 {bytes} 字节，超过 {MAX_PAYLOAD_BYTES} 字节的阈值"
            ),
        }
    }

    /// 稳定规则 id，进审计行的 `rule` 位。与 `fs_policy` 的 `RULE_*` 同一命名风格。
    pub fn rule(&self) -> &'static str {
        match self {
            Self::ControlChar { .. } => "mcp_payload_control_char",
            Self::InvisibleFormat { .. } => "mcp_payload_invisible_format",
            Self::TooLong { .. } => "mcp_payload_too_long",
        }
    }
}

/// Unicode `Cf`（格式）类中会造成「显示 ≠ 内容」的码位区间，闭区间。
///
/// `char::is_control()` 只覆盖 `Cc`（C0 0x00–0x1F、DEL 0x7F、C1 0x80–0x9F），
/// 这张表补的是 `Cf`。三族各有各的攻击面：
///
/// - **双向覆盖/隔离**（202A–202E、2066–2069、200E–200F、061C）：让终端把一串
///   字符按相反顺序显示。用户在确认框里看到的是 `echo hello`，实际发出去的是
///   别的东西。这是六条实测绕过里最难靠肉眼发现的一条。
/// - **零宽**（200B–200D、2060–2064、00AD、FEFF）：插在 `rm` 中间让规则匹配失败，
///   而屏幕上仍然是 `rm`。
/// - **TAG 区**（E0000–E007F）：整个 ASCII 字符集在这里有一份不可见的副本。
///   它是「不可见指令走私」用的信道——一段看起来无害的文本里可以夹带任意
///   长度的隐藏内容，人眼与大多数日志都看不见。
const INVISIBLE_FORMAT: &[(u32, u32)] = &[
    (0x00AD, 0x00AD),   // SOFT HYPHEN
    (0x0600, 0x0605),   // 阿拉伯数字标记族
    (0x061C, 0x061C),   // ARABIC LETTER MARK（双向）
    (0x06DD, 0x06DD),   // ARABIC END OF AYAH
    (0x070F, 0x070F),   // SYRIAC ABBREVIATION MARK
    (0x0890, 0x0891),   // 阿拉伯数字标记（Unicode 14 新增）
    (0x08E2, 0x08E2),   // ARABIC DISPUTED END OF AYAH
    (0x180E, 0x180E),   // MONGOLIAN VOWEL SEPARATOR
    (0x200B, 0x200F),   // 零宽空格 / ZWNJ / ZWJ / LRM / RLM
    (0x202A, 0x202E),   // LRE / RLE / PDF / LRO / RLO ← 显示与内容相反
    (0x2060, 0x2064),   // WORD JOINER … INVISIBLE PLUS
    (0x2066, 0x206F),   // 双向隔离 LRI/RLI/FSI/PDI + 已弃用的成形选择符
    (0xFEFF, 0xFEFF),   // ZWNBSP / BOM
    (0xFFF9, 0xFFFB),   // 行间注释锚点
    (0x110BD, 0x110BD), // KAITHI NUMBER SIGN
    (0x110CD, 0x110CD), // KAITHI NUMBER SIGN ABOVE
    (0x13430, 0x1343F), // 埃及圣书体格式控制
    (0x1BCA0, 0x1BCA3), // 速记格式控制
    (0x1D173, 0x1D17A), // 乐谱格式控制
    (0xE0001, 0xE0001), // LANGUAGE TAG
    (0xE0020, 0xE007F), // TAG 区 ← 不可见指令走私的信道
];

fn is_invisible_format(c: char) -> bool {
    let u = c as u32;
    INVISIBLE_FORMAT.iter().any(|&(lo, hi)| u >= lo && u <= hi)
}

/// 判定一段 payload 能不能交给分类器。
///
/// **检查顺序是个定义，不是实现巧合**：先逐字符扫，后看长度。
/// 一段既超长又含 CR 的 payload 两条都成立，报哪一条是我们的选择——
/// 报「第 2 字节有个 CR」比报「有 9 KiB」信息量大得多，后者用户还得自己去找。
pub fn opacity_of(payload: &str) -> Opacity {
    for (at, c) in payload.char_indices() {
        // LF 放行：它是回车键，且分类器看得懂多行脚本（见模块头）。
        if c == '\n' {
            continue;
        }
        if c.is_control() {
            return Opacity::Opaque(OpaqueReason::ControlChar { at, code: c as u32 });
        }
        if is_invisible_format(c) {
            return Opacity::Opaque(OpaqueReason::InvisibleFormat { at, code: c as u32 });
        }
    }
    if payload.len() > MAX_PAYLOAD_BYTES {
        return Opacity::Opaque(OpaqueReason::TooLong {
            bytes: payload.len(),
        });
    }
    Opacity::Transparent
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **本模块存在的全部理由**：这六条里的每一条，分类器单独看都会给出一个
    /// 比实际危险程度低的结论。
    ///
    /// 断言写成「分类器判的 < Dangerous」而不是写死具体档位——档位是分类器的
    /// 实现细节（改一条规则就可能从 ReadOnly 变 Write），而「它没看出这是删根」
    /// 才是这条测试要钉的事实。
    ///
    /// 这条一旦变绿失败（即分类器自己也判 Dangerous 了），说明盲点被补上了，
    /// 那时该来重新审视这个模块是否还需要——**而不是**把断言删掉。
    #[test]
    fn the_classifier_alone_is_fooled_by_every_one_of_these() {
        let bypasses: &[(&str, String)] = &[
            ("CR 分隔出第二条命令", "ls\rrm -rf /".to_string()),
            (
                "退格擦掉伪装",
                "echo safe\u{8}\u{8}\u{8}\u{8}rm -rf /".to_string(),
            ),
            ("RLO 让显示与内容相反", "echo \u{202e}/ fr- mr".to_string()),
            ("ANSI CSI 前缀打掉规则", "\u{1b}[2Krm -rf /".to_string()),
            ("Ctrl-C 前缀打掉规则", "\u{3}rm -rf /".to_string()),
            ("零宽空格拆开 rm", "r\u{200b}m -rf /".to_string()),
        ];
        for (name, p) in bypasses {
            let alone = fs_policy::classify(p).tier;
            assert!(
                alone < fs_policy::Tier::Dangerous,
                "{name}：分类器已经能独立判危（{alone:?}），本模块的必要性需要重新评估"
            );
            assert!(
                opacity_of(p).is_opaque(),
                "{name}：本模块必须判它不透明，否则这就是一条绕开策略引擎的通道"
            );
        }
        assert_eq!(bypasses.len(), 6, "做空防护：六条实测绕过一条都不能少");
    }

    /// 放行 LF 的**前提**：分类器真的看得懂多行脚本。
    ///
    /// 这条红了就说明 shell 解析器改了口径，那时 `opacity_of` 里放行 LF 的
    /// `continue` 立刻变成一个和 CR 一样大的洞。
    #[test]
    fn newline_is_the_one_control_char_the_classifier_understands() {
        assert_eq!(
            fs_policy::classify("ls\nrm -rf /").tier,
            fs_policy::Tier::Dangerous,
            "分类器必须看得懂换行分隔的多行脚本，否则 LF 不能放行"
        );
        // 反向对照：同样的两条命令用 CR 分隔，分类器就瞎了。
        assert!(fs_policy::classify("ls\rrm -rf /").tier < fs_policy::Tier::Dangerous);
        // 于是：LF 透明，CR 不透明。
        assert_eq!(opacity_of("ls\nrm -rf /"), Opacity::Transparent);
        assert!(opacity_of("ls\rrm -rf /").is_opaque());
    }

    #[test]
    fn ordinary_commands_are_transparent() {
        for p in [
            "ls -la",
            "ls\n",
            "cd /var/log && tail -n 100 syslog\n",
            "echo 你好世界\n",
            "echo 'a b'\necho c\n",
            "",
        ] {
            assert_eq!(opacity_of(p), Opacity::Transparent, "输入 {p:?}");
        }
    }

    /// TAB 归不透明——补全让最终执行的字节由远端决定。
    #[test]
    fn tab_is_opaque_because_completion_is_decided_remotely() {
        let o = opacity_of("cat /etc/pas\t");
        assert!(o.is_opaque());
        assert!(matches!(
            o.reason(),
            Some(OpaqueReason::ControlChar { code: 9, .. })
        ));
    }

    /// 位置是真的位置（字节偏移，多字节字符前缀下也要对）。
    #[test]
    fn the_reported_offset_is_a_real_byte_offset() {
        // "你好" 是 6 字节，CR 在第 6 字节。
        let o = opacity_of("你好\rrm -rf /");
        assert_eq!(
            o.reason(),
            Some(&OpaqueReason::ControlChar { at: 6, code: 0x0D })
        );
    }

    /// 长度阈值闭合在哪一侧是个契约：4096 放行，4097 拦。
    #[test]
    fn the_length_threshold_is_closed_on_the_allow_side() {
        let ok = "a".repeat(MAX_PAYLOAD_BYTES);
        assert_eq!(ok.len(), 4096);
        assert_eq!(opacity_of(&ok), Opacity::Transparent);

        let over = "a".repeat(MAX_PAYLOAD_BYTES + 1);
        assert_eq!(
            opacity_of(&over),
            Opacity::Opaque(OpaqueReason::TooLong { bytes: 4097 })
        );
    }

    /// 阈值算的是**字节**不是字符——否则一段 4000 个汉字（12000 字节）会溜过去。
    #[test]
    fn the_threshold_counts_bytes_not_chars() {
        let s = "字".repeat(2000); // 2000 字符 / 6000 字节
        assert_eq!(s.chars().count(), 2000);
        assert!(s.len() > MAX_PAYLOAD_BYTES);
        assert!(opacity_of(&s).is_opaque(), "6000 字节必须被拦");
    }

    /// 顺序定义：既超长又含控制字符时，报控制字符。
    #[test]
    fn a_control_char_outranks_the_length_complaint() {
        let mut s = "a".repeat(MAX_PAYLOAD_BYTES + 100);
        s.insert(3, '\r');
        assert!(matches!(
            opacity_of(&s).reason(),
            Some(OpaqueReason::ControlChar { at: 3, .. })
        ));
    }

    /// 三族不可见字符各点一个代表，外加一个正常字符做反向对照。
    ///
    /// 反向对照是这条的重点：只断言「这些是不可见的」的话，把
    /// `is_invisible_format` 写成 `true` 常量也能全绿。
    #[test]
    fn each_family_of_invisible_chars_is_caught_and_normal_text_is_not() {
        let caught = [
            ('\u{202e}', "RLO 双向覆盖"),
            ('\u{2066}', "LRI 双向隔离"),
            ('\u{200b}', "零宽空格"),
            ('\u{feff}', "BOM"),
            ('\u{00ad}', "软连字符"),
            ('\u{e0041}', "TAG 区的大写 A"),
            ('\u{1d173}', "乐谱格式控制"),
        ];
        for (c, name) in caught {
            let p = format!("echo{c}hi");
            assert!(
                opacity_of(&p).is_opaque(),
                "{name}（U+{:04X}）必须被判不可见",
                c as u32
            );
            assert!(matches!(
                opacity_of(&p).reason(),
                Some(OpaqueReason::InvisibleFormat { .. })
            ));
        }
        // 反向：普通字符（含 CJK、emoji、重音）一个都不许被误判。
        for c in ['a', '中', 'é', '🙂', ' ', '/', '-', '$'] {
            assert_eq!(
                opacity_of(&format!("echo{c}hi")),
                Opacity::Transparent,
                "U+{:04X} 被误判成不可见",
                c as u32
            );
        }
        assert_eq!(caught.len(), 7);
    }

    /// 区间表本身的自洽性：每个区间 lo <= hi，且整体按 lo 严格递增（无重叠、无乱序）。
    ///
    /// 手写码位表最容易犯的错是把 hi/lo 写反——那样 `u >= lo && u <= hi`
    /// 恒 false，整个区间静默失效，而所有「正常字符不被误判」的断言照样绿。
    #[test]
    fn the_range_table_is_sorted_and_non_overlapping() {
        let mut prev_hi = None::<u32>;
        for &(lo, hi) in INVISIBLE_FORMAT {
            assert!(lo <= hi, "区间 U+{lo:04X}..U+{hi:04X} 的两端写反了");
            if let Some(p) = prev_hi {
                assert!(lo > p, "区间 U+{lo:04X} 与前一段重叠或乱序");
            }
            prev_hi = Some(hi);
        }
        assert!(INVISIBLE_FORMAT.len() >= 20, "做空防护");
    }

    /// C1 控制区（0x80–0x9F）也算控制字符——它在 UTF-8 里是两字节，
    /// 容易被「只看 ASCII 范围」的实现漏掉。
    #[test]
    fn c1_controls_are_control_chars_too() {
        // U+0085 NEXT LINE：某些终端把它当换行处理。
        let o = opacity_of("ls\u{85}rm -rf /");
        assert!(o.is_opaque());
        assert!(matches!(
            o.reason(),
            Some(OpaqueReason::ControlChar { code: 0x85, .. })
        ));
    }

    /// 每种理由都有非空的 detail 与稳定的 rule id，且三个 rule id 互不相同。
    #[test]
    fn every_reason_carries_a_usable_detail_and_a_distinct_rule_id() {
        let all = [
            OpaqueReason::ControlChar { at: 1, code: 13 },
            OpaqueReason::InvisibleFormat {
                at: 2,
                code: 0x202e,
            },
            OpaqueReason::TooLong { bytes: 9999 },
        ];
        let ids: std::collections::BTreeSet<_> = all.iter().map(|r| r.rule()).collect();
        assert_eq!(ids.len(), 3, "三个 rule id 必须互不相同");
        for r in &all {
            assert!(!r.detail().is_empty());
            assert!(r.rule().starts_with("mcp_payload_"));
        }
        // detail 要真的把数值说出来，而不是一句笼统的「有问题」。
        assert!(all[0].detail().contains("U+000D"));
        assert!(all[1].detail().contains("U+202E"));
        assert!(all[2].detail().contains("9999"));
    }
}
