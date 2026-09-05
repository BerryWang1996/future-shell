//! 会话纯文本日志（M4a：Xshell Logging 式转录；路线图 §M4）。
//!
//! 连接建立即开始把会话输出转录到文件；路径/命名模板/追加或覆盖可配；断线重连
//! 续写；与录屏（另立）独立开关。
//!
//! ## 三个设计取舍
//!
//! **① 剥 ANSI 但带状态。** 纯文本日志要的是「人能读的那一份」——转义序列写进
//! 文件会让 `less` 满屏乱码、`grep` 匹配不上（`ls` 的彩色输出里 `foo` 实际是
//! `\x1b[0;32mfoo\x1b[0m`）。而转义序列**会跨读块边界断开**（16 KiB 缓冲切在
//! `\x1b[3` 与 `2m` 之间是常态），故剥离器必须是**流式状态机**，不能对每块
//! 独立做正则——那会把半截序列当正文写进去，且下半截的 `2m` 变成可见字符。
//!
//! **② 写入不阻塞读取任务。** 转录是旁路：磁盘慢、盘满、路径失效都不该让终端
//! 卡住或掉字节。故 `SessionLog::write` 只做「剥离 + 追加到内存缓冲」，落盘由
//! 调用方在自己的节奏上 flush（app 层每帧后 flush 一次，见 pipe.rs tap 点）。
//! 写失败**只记一次**（`failed` 闩），不逐块刷屏、不上抛——转录坏了要提示，但
//! 不能把会话一起拖死。
//!
//! **③ 时间戳与命名模板是纯函数。** `{host}_{yyyy}{MM}{dd}_{HH}{mm}{ss}.log`
//! 这类模板的替换与钳位（路径穿越、非法字符、长度）离线可测；`SystemTime` 由
//! 调用方注入，测试才能给定时刻。

use std::io::Write;
use std::path::{Path, PathBuf};

/// 流式 ANSI 剥离器（跨块保持状态，见模块头 ①）。
///
/// 覆盖终端输出里实际会出现的形状：CSI（`\x1b[...` 终止于 `@`–`~`）、OSC
/// （`\x1b]...` 终止于 BEL 或 ST）、以及双字符 ESC 序列（`\x1bM`、`\x1b(B` 等）。
/// 其余控制字节保留 `\n`/`\t`（正文的一部分），丢弃 `\r`（CRLF 的 CR 会让
/// 文本文件在 Unix 工具下显示 `^M`）与 BEL/退格等噪声。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum StripState {
    #[default]
    Text,
    /// 刚见 ESC，等下一字节判类型
    Esc,
    /// CSI 参数中（`\x1b[`）
    Csi,
    /// OSC 字符串中（`\x1b]`），等 BEL 或 ESC \
    Osc,
    /// OSC 里见到 ESC，等 `\`
    OscEsc,
    /// 双字符 ESC 序列的第二字节（如 `\x1b(` 后的字符集标识）
    EscInter,
}

/// 剥离器：`feed` 逐块喂入，返回该块产出的可读文本（可能为空）。
#[derive(Debug, Default)]
pub struct AnsiStripper {
    state: StripState,
}

impl AnsiStripper {
    pub fn new() -> Self {
        Self::default()
    }

    /// 喂一块字节，产出剥离后的文本字节。**不做 UTF-8 边界处理**：多字节字符
    /// 被切开时两半各自原样透传，拼接后仍是合法 UTF-8（剥离只删 ASCII 范围的
    /// 控制序列，UTF-8 续字节恒 ≥ 0x80，不会被误判成序列的一部分）。
    pub fn feed(&mut self, chunk: &[u8], out: &mut Vec<u8>) {
        for &b in chunk {
            match self.state {
                StripState::Text => match b {
                    0x1b => self.state = StripState::Esc,
                    b'\r' => {} // CRLF 的 CR：文本文件里是 ^M 噪声
                    0x07 => {}  // BEL
                    0x08 => {}  // 退格（进度条常用，写进文件只是噪声）
                    _ => out.push(b),
                },
                StripState::Esc => match b {
                    b'[' => self.state = StripState::Csi,
                    b']' => self.state = StripState::Osc,
                    // 双字符序列的中间字节（字符集选择 `(`/`)`、DEC 私有 `#` 等）
                    b'(' | b')' | b'*' | b'+' | b'#' | b'%' => self.state = StripState::EscInter,
                    // 其余单字符 ESC 序列（ESC M 反向换行、ESC 7/8 存取光标…）就地结束
                    _ => self.state = StripState::Text,
                },
                StripState::Csi => {
                    // CSI 终止符 = 0x40..=0x7e；之前的都是参数/中间字节
                    if (0x40..=0x7e).contains(&b) {
                        self.state = StripState::Text;
                    }
                }
                StripState::Osc => match b {
                    0x07 => self.state = StripState::Text, // BEL 终止
                    0x1b => self.state = StripState::OscEsc,
                    _ => {}
                },
                StripState::OscEsc => {
                    // ESC \ = ST 终止；ESC 后接别的说明 OSC 被打断，回到文本态
                    self.state = StripState::Text;
                }
                StripState::EscInter => self.state = StripState::Text,
            }
        }
    }
}

/// 写入模式（Xshell Logging 的「追加/覆盖」）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogMode {
    /// 追加到已有文件尾（默认：断线重连续写靠它）
    Append,
    /// 每次开会话覆盖同名文件
    Overwrite,
}

/// 命名模板渲染（纯函数，见模块头 ③）。
///
/// 支持占位符：`{host}` `{user}` `{session}` `{yyyy}` `{MM}` `{dd}` `{HH}` `{mm}` `{ss}`。
/// 未知占位符**原样保留**（不静默吞——用户拼错了要能看出来，而不是得到一个
/// 少了一段的文件名）。
///
/// 文件名安全：渲染完对结果做一次清洗——路径分隔符（`/` `\`）、Windows 保留字符
/// （`: * ? " < > |`）与控制字符一律替换为 `_`，避免主机名里的冒号（IPv6）或
/// 用户输入的 `../` 把文件写到目标目录之外。清洗在**渲染之后**做，故模板本身
/// 也不能穿越目录：`{host}/x.log` 会变成 `host_x.log`（子目录要显式配置目录，
/// 不能靠模板隐式造）。
pub fn render_log_name(template: &str, ctx: &LogNameCtx<'_>) -> String {
    let mut s = template.to_string();
    for (k, v) in [
        ("{host}", ctx.host),
        ("{user}", ctx.user),
        ("{session}", ctx.session),
    ] {
        s = s.replace(k, v);
    }
    for (k, v) in [
        ("{yyyy}", format!("{:04}", ctx.year)),
        ("{MM}", format!("{:02}", ctx.month)),
        ("{dd}", format!("{:02}", ctx.day)),
        ("{HH}", format!("{:02}", ctx.hour)),
        ("{mm}", format!("{:02}", ctx.minute)),
        ("{ss}", format!("{:02}", ctx.second)),
    ] {
        s = s.replace(k, &v);
    }
    sanitize_file_name(&s)
}

/// 命名模板的替换上下文（时间由调用方给定，便于离线测试）。
#[derive(Debug, Clone, Copy)]
pub struct LogNameCtx<'a> {
    pub host: &'a str,
    pub user: &'a str,
    pub session: &'a str,
    pub year: u32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
}

/// 文件名清洗（见 `render_log_name` 的安全注）。空结果回落 `session.log`——
/// 空文件名会让 `dir.join("")` 指向目录本身，随后的写入撞 IsADirectory。
pub fn sanitize_file_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let trimmed = cleaned.trim().trim_matches('.').to_string();
    if trimmed.is_empty() {
        "session.log".to_string()
    } else if trimmed.chars().count() > 200 {
        trimmed.chars().take(200).collect()
    } else {
        trimmed
    }
}

/// 会话日志写入器：剥 ANSI → 缓冲 → flush 落盘（见模块头 ②）。
pub struct SessionLog {
    path: PathBuf,
    stripper: AnsiStripper,
    buf: Vec<u8>,
    /// 已写字节（诊断/上限判据）
    written: u64,
    /// 写失败闩：失败一次即置位并记一条日志，此后静默丢弃（不逐块刷屏、不拖死会话）
    failed: bool,
    /// 首次打开是否截断（Overwrite 模式；Append 模式恒 false）
    truncate_on_open: bool,
    opened: bool,
}

impl SessionLog {
    /// 建写入器。**不立即建文件**：会话可能一个字节都不输出（连上就被关掉），
    /// 那种情形下留一个 0 字节文件是垃圾。首次 `flush` 有内容时才创建。
    pub fn new(path: PathBuf, mode: LogMode) -> Self {
        Self {
            path,
            stripper: AnsiStripper::new(),
            buf: Vec::with_capacity(8 * 1024),
            written: 0,
            failed: false,
            truncate_on_open: matches!(mode, LogMode::Overwrite),
            opened: false,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn bytes_written(&self) -> u64 {
        self.written
    }

    pub fn is_failed(&self) -> bool {
        self.failed
    }

    /// 喂一块会话输出（剥 ANSI 后进缓冲）。恒不阻塞、恒不失败。
    pub fn write(&mut self, chunk: &[u8]) {
        if self.failed {
            return;
        }
        self.stripper.feed(chunk, &mut self.buf);
    }

    /// 落盘缓冲。失败只置闩 + 记一条 warn（首次），不上抛。
    pub fn flush(&mut self) {
        if self.failed || self.buf.is_empty() {
            return;
        }
        match self.append_to_file() {
            Ok(n) => {
                self.written += n;
                self.buf.clear();
            }
            Err(e) => {
                tracing::warn!(path = %self.path.display(), %e, "会话日志写入失败，本会话停止转录");
                self.failed = true;
                self.buf.clear(); // 丢掉缓冲，别让它无界增长
            }
        }
    }

    fn append_to_file(&mut self) -> std::io::Result<u64> {
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let mut opts = std::fs::OpenOptions::new();
        opts.create(true).write(true);
        // 覆盖模式只在**本会话首次**落盘时截断：之后的 flush 必须追加，
        // 否则每次 flush 都把前面写的内容抹掉，日志里只剩最后一帧。
        if self.truncate_on_open && !self.opened {
            opts.truncate(true);
        } else {
            opts.append(true);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600); // 会话转录含命令与输出明文，收紧到仅属主可读
        }
        let mut f = opts.open(&self.path)?;
        f.write_all(&self.buf)?;
        self.opened = true;
        Ok(self.buf.len() as u64)
    }
}

impl Drop for SessionLog {
    fn drop(&mut self) {
        // 会话关闭时把尾部缓冲落盘——不 flush 会丢掉最后一批输出（用户看到的
        // 日志比屏幕上少一截，而那一截往往正是他想留证的那条命令的结果）。
        self.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strip(chunks: &[&[u8]]) -> String {
        let mut s = AnsiStripper::new();
        let mut out = Vec::new();
        for c in chunks {
            s.feed(c, &mut out);
        }
        String::from_utf8(out).expect("剥离结果须仍是合法 UTF-8")
    }

    /// S315：CSI/OSC/双字符 ESC 三类序列剥净，正文与换行保留。
    #[test]
    fn strips_ansi_keeps_text() {
        assert_eq!(strip(&[b"\x1b[0;32mfoo\x1b[0m bar\n"]), "foo bar\n");
        assert_eq!(strip(&[b"\x1b]0;title\x07plain"]), "plain");
        assert_eq!(strip(&[b"\x1b]0;t\x1b\\after"]), "after"); // OSC 以 ST 终止
        assert_eq!(strip(&[b"\x1b(Bascii"]), "ascii"); // 字符集选择
        assert_eq!(strip(&[b"\x1bMscroll"]), "scroll"); // 单字符 ESC
        assert_eq!(strip(&[b"a\tb\n"]), "a\tb\n"); // 制表与换行是正文
        assert_eq!(strip(&[b"x\r\ny"]), "x\ny"); // CR 丢弃（^M 噪声）
    }

    /// S315：**跨块断开的序列**必须被完整吃掉——这是流式剥离器存在的理由。
    /// 逐字节喂是最坏情形（每个字节一块）。
    #[test]
    fn strips_across_chunk_boundaries() {
        // 序列切在参数中间
        assert_eq!(strip(&[b"\x1b[3", b"2mgreen\x1b[0", b"m"]), "green");
        // OSC 切在标题中间
        assert_eq!(strip(&[b"\x1b]0;my", b" title\x07txt"]), "txt");
        // 逐字节喂整串
        let raw = b"\x1b[1;31mERR\x1b[0m\n";
        let per_byte: Vec<&[u8]> = raw.iter().map(std::slice::from_ref).collect();
        assert_eq!(strip(&per_byte), "ERR\n");
    }

    /// UTF-8 多字节字符被切开时两半原样透传，拼接后合法。
    #[test]
    fn utf8_split_across_chunks_survives() {
        let s = "中文";
        let b = s.as_bytes();
        assert_eq!(strip(&[&b[..2], &b[2..]]), s);
    }

    /// S316：命名模板替换 + 未知占位符原样保留 + 文件名清洗。
    #[test]
    fn renders_and_sanitizes_log_name() {
        let ctx = LogNameCtx {
            host: "web-01",
            user: "root",
            session: "s1",
            year: 2026,
            month: 8,
            day: 9,
            hour: 7,
            minute: 5,
            second: 3,
        };
        assert_eq!(
            render_log_name("{host}_{yyyy}{MM}{dd}_{HH}{mm}{ss}.log", &ctx),
            "web-01_20260809_070503.log"
        );
        assert_eq!(
            render_log_name("{user}@{host}.log", &ctx),
            "root@web-01.log"
        );
        // 未知占位符原样保留（拼错要能看出来）
        assert_eq!(render_log_name("{nope}.log", &ctx), "{nope}.log");
        // 路径穿越与 IPv6 冒号被清洗（模板不能隐式造子目录）
        let ipv6 = LogNameCtx {
            host: "fe80::1",
            ..ctx
        };
        assert_eq!(render_log_name("{host}.log", &ipv6), "fe80__1.log");
        // 分隔符替换后再 trim 首尾的点：`../../etc/passwd` → `.._.._etc_passwd`
        // → 首部 `..` 被 trim 掉 → `_.._etc_passwd`。首部点被剥是额外收益
        // （`.` 开头在 Unix 是隐藏文件，日志文件不该隐身）。
        assert_eq!(render_log_name("../../etc/passwd", &ctx), "_.._etc_passwd");
        assert_eq!(render_log_name("{host}/sub.log", &ctx), "web-01_sub.log");
    }

    #[test]
    fn sanitize_edges() {
        assert_eq!(sanitize_file_name(""), "session.log");
        assert_eq!(sanitize_file_name("   "), "session.log");
        assert_eq!(sanitize_file_name("..."), "session.log");
        assert_eq!(sanitize_file_name(&"x".repeat(300)).chars().count(), 200);
        assert_eq!(sanitize_file_name("a\u{1}b"), "a_b");
    }

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("fs-slog-{}-{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// S317：追加模式跨「重连」续写（同路径二次打开不截断）。
    #[test]
    fn append_mode_resumes_across_reconnect() {
        let dir = scratch("append");
        let p = dir.join("a.log");
        {
            let mut log = SessionLog::new(p.clone(), LogMode::Append);
            log.write(b"first\n");
            log.flush();
        }
        {
            // 「重连」：新写入器、同路径
            let mut log = SessionLog::new(p.clone(), LogMode::Append);
            log.write(b"second\n");
            log.flush();
        }
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "first\nsecond\n");
    }

    /// S317：覆盖模式只在本会话首次落盘时截断——后续 flush 必须追加，
    /// 否则日志里只剩最后一帧。
    #[test]
    fn overwrite_truncates_once_then_appends() {
        let dir = scratch("overwrite");
        let p = dir.join("o.log");
        std::fs::write(&p, "OLD CONTENT\n").unwrap();
        let mut log = SessionLog::new(p.clone(), LogMode::Overwrite);
        log.write(b"one\n");
        log.flush();
        log.write(b"two\n");
        log.flush();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "one\ntwo\n");
    }

    /// 会话无输出时不留 0 字节垃圾文件。
    #[test]
    fn no_output_creates_no_file() {
        let dir = scratch("empty");
        let p = dir.join("e.log");
        {
            let mut log = SessionLog::new(p.clone(), LogMode::Append);
            log.flush();
        }
        assert!(!p.exists(), "零输出不该留下空文件");
    }

    /// drop 落尾部缓冲（不 flush 会丢掉最后一批输出）。
    #[test]
    fn drop_flushes_tail() {
        let dir = scratch("droptail");
        let p = dir.join("d.log");
        {
            let mut log = SessionLog::new(p.clone(), LogMode::Append);
            log.write(b"tail\n");
            // 不显式 flush，靠 Drop
        }
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "tail\n");
    }

    /// 写失败置闩后静默丢弃，不无界缓冲、不再重试。
    #[test]
    fn write_failure_latches_and_drops_quietly() {
        let dir = scratch("fail");
        // 路径指向一个**目录**：打开必失败
        let p = dir.clone();
        let mut log = SessionLog::new(p, LogMode::Append);
        log.write(b"x");
        log.flush();
        assert!(log.is_failed(), "首次写失败须置闩");
        log.write(b"more");
        log.flush();
        assert_eq!(log.bytes_written(), 0);
    }
}
