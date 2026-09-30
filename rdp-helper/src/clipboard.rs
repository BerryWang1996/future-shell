//! 剪贴板（CLIPRDR）后端：把 IronRDP 的剪贴板回调代理到 stdio 管道。
//!
//! # 这一层为什么存在
//!
//! `CliprdrBackend` 的契约是「你是操作系统剪贴板」——可 helper **没有**操作
//! 系统剪贴板（它是个沙箱进程，碰不到 GUI 会话，也不该碰）。真正的剪贴板在
//! 主程序那侧。于是本后端只做转译：远端的每个动作变成一条上行消息，
//! 主程序的每个动作变成一次方法调用。
//!
//! # 只做文本，且明确不通告别的
//!
//! 阶段 2 只支持 `CF_UNICODETEXT`。图片与文件列表要 RDPDR 与格式协商
//! （阶段 3）。**不通告**比「通告了再传空」诚实得多：后者会让用户在远端
//! 按下粘贴、看到一个空白，然后怀疑是自己复制错了。
//!
//! # 编码：UTF-16LE ↔ UTF-8
//!
//! `CF_UNICODETEXT` 是 UTF-16LE 且**以 NUL 结尾**。两侧各错一半是这条路上的
//! 经典缺陷：漏掉结尾 NUL，Windows 的记事本粘出来会带一个尾随乱码；
//! 不转编码则整段变成间隔的问号。转换函数单列在下面，单测逐条钉。
//!
//! # 主程序的剪贴板与本机剪贴板是两回事
//!
//! 本后端不判断「内容变没变」「要不要覆盖本机剪贴板」——那些是主程序的策略
//! （它才知道用户在不在这个标签上）。这里只忠实转译。

use fs_rdpproto::FromHelper;
// ClipboardMessageProxy 不实现：那个 trait 服务于「OS 剪贴板事件循环把消息
// 推回主循环」的形状（winit 等）。本 helper 没有 OS 剪贴板、也没有那种循环——
// CliprdrClient::new 只要 backend，proxy 是另一条路径的东西。
use ironrdp_cliprdr::backend::CliprdrBackend;
use ironrdp_cliprdr::pdu::{
    ClipboardFormat, ClipboardFormatId, ClipboardGeneralCapabilityFlags, FileContentsRequest,
    FileContentsResponse, FormatDataRequest, FormatDataResponse, LockDataId,
};
use tokio::sync::mpsc;

use crate::engine::ToMain;

/// UTF-8 → `CF_UNICODETEXT` 载荷（UTF-16LE + 结尾 NUL）。
///
/// 结尾 NUL 不是可选的：MS-RDPECLIP 2.2.5.2.2 要求 `CF_UNICODETEXT` 的数据
/// 以 NUL 结尾，Windows 侧按 C 字符串读。漏了它，粘贴出来会拖一截内存垃圾。
pub fn utf8_to_cf_unicodetext(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len() * 2 + 2);
    for unit in s.encode_utf16() {
        out.extend_from_slice(&unit.to_le_bytes());
    }
    out.extend_from_slice(&[0, 0]); // 结尾 NUL（UTF-16 的一个码元）
    out
}

/// `CF_UNICODETEXT` 载荷 → UTF-8。
///
/// 三件防御：奇数长度（截断的 UTF-16）丢掉最后半个码元而不是 panic；
/// 结尾 NUL 剥掉（连同其后的一切——某些服务器会在 NUL 后补填充）；
/// 非法代理对用 `from_utf16_lossy` 兜住（换成替换字符，不吞整段）。
pub fn cf_unicodetext_to_utf8(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
    String::from_utf16_lossy(&units[..end])
}

/// 把 CLIPRDR 的回调转成上行消息。
#[derive(Debug)]
pub struct PipeClipboard {
    to_main: mpsc::UnboundedSender<ToMain>,
    /// 远端最近一次通告的格式里是否有 `CF_UNICODETEXT`。
    ///
    /// **共享给引擎**（`Arc<AtomicBool>`）而不是自己存一个 bool：
    /// `Cliprdr` 把 backend 装箱进私有字段，引擎拿不到它的引用；
    /// 而「远端有没有文本」正是引擎决定要不要发取数请求的依据。
    /// 不共享就只能盲发——远端没通告时那是一个必然失败的请求。
    remote_has_text: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl PipeClipboard {
    pub fn new(
        to_main: mpsc::UnboundedSender<ToMain>,
        remote_has_text: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> Self {
        Self {
            to_main,
            remote_has_text,
        }
    }

    fn up(&self, header: FromHelper, body: Vec<u8>) {
        let _ = self.to_main.send(ToMain { header, body });
    }
}

ironrdp_core::impl_as_any!(PipeClipboard);

impl CliprdrBackend for PipeClipboard {
    fn temporary_directory(&self) -> &str {
        // 文件复制（阶段 3）才会用到；阶段 2 不通告文件能力，这里给的是
        // 一个明确不存在的相对名而不是真实临时目录——helper 不写磁盘。
        ".fs-rdp-no-files"
    }

    fn client_capabilities(&self) -> ClipboardGeneralCapabilityFlags {
        // 只要最基本的：不要 LOCK_CLIPDATA（那是文件传输的锁），
        // 不要 FILECLIP（阶段 3）。能力谈得越少，能出错的地方越少。
        ClipboardGeneralCapabilityFlags::empty()
    }

    fn on_ready(&mut self) {
        // 通道就绪。本机此刻的剪贴板内容由主程序在它认为合适时通告
        //（用户切到这个标签、或本机剪贴板变化），helper 不主动索取。
    }

    fn on_request_format_list(&mut self) {
        // CLIPRDR 要求客户端在初始化阶段通告一次本机可用格式。
        // 主程序不一定此刻就有内容——空通告是合法的，等它 ClipboardOffer 再补。
    }

    fn on_process_negotiated_capabilities(&mut self, _caps: ClipboardGeneralCapabilityFlags) {}

    fn on_remote_copy(&mut self, available_formats: &[ClipboardFormat]) {
        let has_text = available_formats
            .iter()
            .any(|f| f.id == ClipboardFormatId::CF_UNICODETEXT);
        self.remote_has_text
            .store(has_text, std::sync::atomic::Ordering::SeqCst);
        if has_text {
            // 只通告「远端有文本」，不带内容——取数由主程序按需发起
            // （见 fs_rdpproto::FromHelper::ClipboardOffer 的文档）。
            self.up(FromHelper::ClipboardOffer, Vec::new());
        }
    }

    fn on_format_data_request(&mut self, request: FormatDataRequest) {
        if request.format == ClipboardFormatId::CF_UNICODETEXT {
            // 远端要本机的文本 → 请主程序给
            self.up(FromHelper::ClipboardRequest, Vec::new());
        } else {
            // 没通告过的格式也可能被请求（对端实现宽松）。这里**不**应答——
            // 引擎侧在收不到主程序数据时会回 new_error()，语义正确。
            eprintln!(
                "fs-rdp-helper: 远端请求了未通告的剪贴板格式 {:?}，忽略",
                request.format
            );
        }
    }

    fn on_format_data_response(&mut self, response: FormatDataResponse<'_>) {
        if response.is_error() {
            eprintln!("fs-rdp-helper: 远端剪贴板取数失败（对端报错）");
            return;
        }
        let text = cf_unicodetext_to_utf8(response.data());
        self.up(FromHelper::ClipboardData, text.into_bytes());
    }

    fn on_file_contents_request(&mut self, _request: FileContentsRequest) {}
    fn on_file_contents_response(&mut self, _response: FileContentsResponse<'_>) {}
    fn on_lock(&mut self, _data_id: LockDataId) {}
    fn on_unlock(&mut self, _data_id: LockDataId) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    /// UTF-8 → CF_UNICODETEXT：UTF-16LE 且**带结尾 NUL**。
    /// 漏 NUL 是这条路上的经典缺陷（Windows 按 C 字符串读，会拖出内存垃圾）。
    #[test]
    fn text_encodes_to_utf16le_with_trailing_nul() {
        let out = utf8_to_cf_unicodetext("Hi");
        assert_eq!(out, vec![b'H', 0, b'i', 0, 0, 0], "实得 {out:?}");
    }

    /// 非 BMP 字符（emoji）走代理对，两个码元四字节。
    #[test]
    fn astral_chars_survive_as_surrogate_pairs() {
        let out = utf8_to_cf_unicodetext("😀");
        // U+1F600 → D83D DE00（小端各两字节）+ 结尾 NUL
        assert_eq!(out, vec![0x3D, 0xD8, 0x00, 0xDE, 0, 0]);
        assert_eq!(cf_unicodetext_to_utf8(&out), "😀", "往返必须无损");
    }

    /// 中文往返（最常用的一路）。
    #[test]
    fn chinese_round_trips() {
        let s = "你好，世界";
        assert_eq!(cf_unicodetext_to_utf8(&utf8_to_cf_unicodetext(s)), s);
    }

    /// **结尾 NUL 之后的一切都要丢掉**：某些服务器在 NUL 后补填充，
    /// 不截断的话粘贴出来会带一串不可见字符。
    #[test]
    fn everything_after_the_nul_is_dropped() {
        let mut bytes = utf8_to_cf_unicodetext("ok");
        bytes.extend_from_slice(&[b'X', 0, b'Y', 0]); // NUL 之后的填充
        assert_eq!(cf_unicodetext_to_utf8(&bytes), "ok");
    }

    /// 奇数长度（截断的 UTF-16）不 panic——这是解析远端字节的路径。
    /// 半个码元被丢掉：宁可少一个字符，不可 panic 掉整条会话。
    #[test]
    fn an_odd_length_payload_drops_the_half_unit_without_panicking() {
        let bytes = vec![b'a', 0, b'b']; // "a" + 半个码元
        assert_eq!(cf_unicodetext_to_utf8(&bytes), "a");
    }

    /// 空载荷 → 空串（不是 panic，也不是一个 NUL 字符）。
    #[test]
    fn empty_payload_is_empty_string() {
        assert_eq!(cf_unicodetext_to_utf8(&[]), "");
        assert_eq!(cf_unicodetext_to_utf8(&[0, 0]), "");
    }
}
