//! asciicast v2 录制（M4a：会话录屏与回放，asciinema 兼容）。
//!
//! 格式（v2，<https://github.com/asciinema/asciinema/blob/develop/doc/asciicast-v2.md>）：
//! 首行 JSON 头 `{version:2, width, height, timestamp, env}`，其后每行一个事件
//! `[流逝秒数(浮点), "o"|"r", 数据]`——`o` 是输出文本（JSON 字符串，控制字符转义），
//! `r` 是尺寸标记 `"80x24"`。
//!
//! ## 与 sessionlog 的分工
//!
//! 录屏存**原始字节流**（含 ANSI 转义与相对时序），用于逐帧回放；会话日志存剥净的
//! 纯文本转录，用于 grep 与取证。同一份入口字节、两种用途，谁也不能替代谁——
//! 「用日志回放」做不到时序与颜色，「用录屏 grep」满屏转义码。
//!
//! ## 多字节边界
//!
//! SSH 读块会把一个 UTF-8 字符劈在两块之间（中文输出是常态，不是例外）。v2 的
//! 数据是 JSON 字符串，直接对半截序列做 lossy 会把一个字变两个替换符，回放时
//! 屏幕上就是永久乱码。[`utf8_carry`] 把不完整的尾部留到下一块——与 sessionlog
//! 的流式剥离器同一个道理，只是保的方向相反（那边剥控制序列，这边保多字节）。
//!
//! ## 时间
//!
//! 事件时间 = 相对录制起点的秒数。起点与当前时刻由调用方注入（`now_ms`），
//! 测试才能钉时序而不睡真时钟。

use std::io::Write as _;

/// 录屏的单文件大小上限。
///
/// 终端输出可以非常大（`yes` 一分钟就是几百 MB 的转义与文本）。回放器把整个文件读进
/// 内存，无上限的录屏文件会把回放器本身变成事故。256 MB 足够几小时的正常会话；
/// 超限即停录并说明，而不是静默继续撑爆磁盘。
pub const CAST_BYTES_MAX: u64 = 256 * 1024 * 1024;

/// 一个回放事件（解析后的形态）。
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct CastEvent {
    /// 相对起点的秒数
    pub t: f64,
    /// `o` = 输出；`r` = 尺寸标记
    pub kind: String,
    /// `o`：文本；`r`：`"80x24"`
    pub data: String,
}

/// asciicast 文件头。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CastHeader {
    pub version: u32,
    pub width: u32,
    pub height: u32,
    /// unix 秒
    pub timestamp: i64,
    #[serde(default)]
    pub env: std::collections::BTreeMap<String, String>,
}

/// 逐行把字节流写成 asciicast v2 事件。
///
/// 持有文件句柄与起点毫秒；跨块的多字节尾部由内部缓冲承担。
/// `active` 关掉后 write 静默丢弃（会话存活期间常驻挂载、按需启停，见 app 层）。
pub struct Recorder {
    file: Option<std::fs::File>,
    start_ms: i64,
    carry: Vec<u8>,
    active: bool,
    written: u64,
    path: std::path::PathBuf,
}

impl Recorder {
    /// 新建并写文件头。`now_ms` 为注入的当前毫秒时刻。
    pub fn start(
        path: std::path::PathBuf,
        cols: u16,
        rows: u16,
        now_ms: i64,
    ) -> std::io::Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let header = CastHeader {
            version: 2,
            width: cols as u32,
            height: rows as u32,
            timestamp: now_ms / 1000,
            env: [("TERM".to_string(), "xterm-256color".to_string())]
                .into_iter()
                .collect(),
        };
        let mut file = std::fs::File::create(&path)?;
        let line = serde_json::to_string(&header)? + "\n";
        file.write_all(line.as_bytes())?;
        let written = line.len() as u64;
        Ok(Self {
            file: Some(file),
            start_ms: now_ms,
            carry: Vec::new(),
            active: true,
            written,
            path,
        })
    }

    /// 常驻挂载用的空录制器：write 全部丢弃，直到 [`Recorder::start_into`] 真正开录。
    pub fn idle(path: std::path::PathBuf) -> Self {
        Self {
            file: None,
            start_ms: 0,
            carry: Vec::new(),
            active: false,
            written: 0,
            path,
        }
    }

    /// 在常驻挂载的录制器上开始一段新录制（换目标文件、重置起点）。
    pub fn start_into(
        &mut self,
        path: std::path::PathBuf,
        cols: u16,
        rows: u16,
        now_ms: i64,
    ) -> std::io::Result<()> {
        *self = Self::start(path, cols, rows, now_ms)?;
        Ok(())
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    pub fn written_bytes(&self) -> u64 {
        self.written
    }

    /// 喂入一段输出字节（相对时序由 `now_ms` 与起点的差给出）。
    pub fn write(&mut self, chunk: &[u8], now_ms: i64) {
        if !self.active {
            return;
        }
        // 上限检查在写**之前**：超量后这一块整个不落盘（检查放后面会把溢出块
        // 先写进去再停，文件反而超出上限）。
        if self.written >= CAST_BYTES_MAX {
            self.stop(now_ms);
            return;
        }
        let elapsed = (now_ms - self.start_ms) as f64 / 1000.0;
        let (text, rest) = utf8_carry(&self.carry, chunk);
        self.carry = rest;
        let Some(file) = self.file.as_mut() else {
            return;
        };
        if !text.is_empty() {
            if let Ok(line) = event_line(elapsed, "o", &text) {
                if file
                    .write_all(line.as_bytes())
                    .and_then(|_| file.flush())
                    .is_ok()
                {
                    self.written += line.len() as u64;
                }
            }
        }
        // 写后若到量，下一块的入口检查会停录
    }

    /// 记一条尺寸标记（终端 resize 时调用）。
    pub fn resize(&mut self, cols: u16, rows: u16, now_ms: i64) {
        if !self.active {
            return;
        }
        let elapsed = (now_ms - self.start_ms) as f64 / 1000.0;
        let Some(file) = self.file.as_mut() else {
            return;
        };
        if let Ok(line) = event_line(elapsed, "r", &format!("{cols}x{rows}")) {
            if file
                .write_all(line.as_bytes())
                .and_then(|_| file.flush())
                .is_ok()
            {
                self.written += line.len() as u64;
            }
        }
    }

    /// 停录。残留的多字节尾部按 lossy 落最后一笔（否则丢字符），关闭句柄。
    pub fn stop(&mut self, now_ms: i64) {
        if !self.active {
            return;
        }
        self.active = false;
        if !self.carry.is_empty() {
            let elapsed = (now_ms - self.start_ms) as f64 / 1000.0;
            let tail = String::from_utf8_lossy(&self.carry).into_owned();
            self.carry.clear();
            if let (Some(file), Ok(line)) = (self.file.as_mut(), event_line(elapsed, "o", &tail)) {
                let _ = file.write_all(line.as_bytes()).and_then(|_| file.flush());
                self.written += line.len() as u64;
            }
        }
        self.file = None;
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        // 兜底 flush：正常路径已显式 stop，这里只补 panic/提前 drop 的尾巴
        self.stop(0);
    }
}

/// 一行事件的序列化：`[1.234,"o","…"]`。
///
/// 数据段经 `serde_json::Value::String` 转义——控制字符（ESC/CR/BS）会写成
/// `` 等合法 JSON 转义，这正是 v2 的要求；手写 `format!("\"{s}\"")` 会在
/// 引号与反斜杠上炸掉。
pub fn event_line(elapsed: f64, kind: &str, data: &str) -> Result<String, serde_json::Error> {
    let data = serde_json::Value::String(data.to_string());
    // 三段数组手拼（serde 对 [f64, &str, Value] 的异构数组要靠 json!，用它也一样）
    Ok(format!("[{elapsed:.3},{kind:?},{data}]\n", kind = kind))
}

/// 跨块多字节处理：把 `carry`（上一块的残留）与 `chunk` 拼起来，返回
/// `(完整文本, 新残留)`。残留为空表示本块以完整字符收尾。
pub fn utf8_carry(carry: &[u8], chunk: &[u8]) -> (String, Vec<u8>) {
    let mut buf = carry.to_vec();
    buf.extend_from_slice(chunk);
    // 从尾部向上找一个 UTF-8 序列的起点（后续字节 10xxxxxx 前缀 11xx）。
    // 最多回退 3 字节（UTF-8 最长 4 字节）。
    let mut split = buf.len();
    if !buf.is_empty() {
        for back in 1..=3.min(buf.len()) {
            let b = buf[buf.len() - back];
            if b & 0xC0 != 0x80 {
                // 序列起点：算出它声明的长度
                let expect = if b >= 0xF0 {
                    4
                } else if b >= 0xE0 {
                    3
                } else if b >= 0xC0 {
                    2
                } else {
                    1 // ASCII（或独立续字节——损坏输入，按单字节处理交给 lossy）
                };
                if back < expect {
                    // 序列没到齐：整段留作残留
                    split = buf.len() - back;
                }
                break;
            }
            // 仍是续字节，继续向前找起点；back 到 3 都是续字节说明输入已损坏，
            // 不再保护（避免把任意字节流无限囤积）
        }
    }
    let (complete, rest) = buf.split_at(split);
    (
        String::from_utf8_lossy(complete).into_owned(),
        rest.to_vec(),
    )
}

/// 解析整个 cast 文件（回放与往返校验共用）。
///
/// 头行不是 v2 / 事件行畸形 → 报错并指出第几行（「能被 asciinema play 播放」
/// 的另一半：自己的解析器与真实工具同一严格度，坏文件在我们这里就红）。
pub fn parse_cast(content: &str) -> Result<(CastHeader, Vec<CastEvent>), String> {
    let mut lines = content.lines().filter(|l| !l.trim().is_empty());
    let head_line = lines.next().ok_or("cast 文件为空")?;
    let header: CastHeader =
        serde_json::from_str(head_line).map_err(|e| format!("cast 头解析失败：{e}"))?;
    if header.version != 2 {
        return Err(format!("仅支持 asciicast v2（实得 v{}）", header.version));
    }
    let mut events = Vec::new();
    for (i, line) in lines.enumerate() {
        let v: serde_json::Value =
            serde_json::from_str(line).map_err(|e| format!("第 {} 个事件解析失败：{e}", i + 2))?;
        let arr = v.as_array().ok_or(format!("第 {} 行不是事件数组", i + 2))?;
        if arr.len() != 3 {
            return Err(format!("第 {} 行事件应为三元素数组", i + 2));
        }
        let t = arr[0]
            .as_f64()
            .ok_or(format!("第 {} 行时间不是数字", i + 2))?;
        let kind = arr[1]
            .as_str()
            .ok_or(format!("第 {} 行类型不是字符串", i + 2))?;
        let data = arr[2]
            .as_str()
            .ok_or(format!("第 {} 行数据不是字符串", i + 2))?;
        if kind != "o" && kind != "r" {
            return Err(format!("第 {} 行未知事件类型 {kind:?}（只认 o/r）", i + 2));
        }
        events.push(CastEvent {
            t,
            kind: kind.to_string(),
            data: data.to_string(),
        });
    }
    Ok((header, events))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "fs-cast-{tag}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// 头行 + 事件行都是合法 JSON，且能被自己的解析器原样读回。
    #[test]
    fn writes_valid_v2_cast_that_parses_back() {
        let dir = tmp("roundtrip");
        let path = dir.join("a.cast");
        let mut r = Recorder::start(path.clone(), 80, 24, 100_000).unwrap();
        r.write(b"hello ", 100_500);
        r.write("\x1b[31mred\x1b[0m\r\n".as_bytes(), 101_000);
        r.stop(102_000);

        let content = std::fs::read_to_string(&path).unwrap();
        let (h, ev) = parse_cast(&content).unwrap();
        assert_eq!(h.version, 2);
        assert_eq!((h.width, h.height), (80, 24));
        assert_eq!(h.timestamp, 100);
        assert_eq!(ev.len(), 2, "两段输出各一行");
        assert_eq!(ev[0].t, 0.5);
        assert_eq!(ev[0].data, "hello ");
        assert_eq!(ev[1].t, 1.0);
        assert_eq!(
            ev[1].data, "\u{1b}[31mred\u{1b}[0m\r\n",
            "转义序列须原样保留"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// **出口标准**：回放与原始输出 diff 为空——所有 `o` 事件拼接 == 喂入的文本。
    #[test]
    fn concatenated_output_events_equal_the_input() {
        let dir = tmp("diff");
        let path = dir.join("a.cast");
        let mut r = Recorder::start(path.clone(), 80, 24, 0).unwrap();
        let input = "uptime\r\nload average: 0.15, 0.20, 0.18\r\n\x1b[32mOK\x1b[0m";
        // 刻意按不齐整的边界切块
        for piece in input.as_bytes().chunks(7) {
            r.write(piece, 10);
        }
        r.stop(20);
        let (_, ev) = parse_cast(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let replayed: String = ev
            .iter()
            .filter(|e| e.kind == "o")
            .map(|e| e.data.as_str())
            .collect();
        assert_eq!(replayed, input, "文本层面 diff 必须为空");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 多字节字符劈在块边界：不产生替换符，拼接后与原文逐字相等。
    #[test]
    fn multibyte_split_across_chunks_survives() {
        // 「中文」两字 6 字节，按 1 字节一块喂——最极端的劈法
        let input = "中文测试";
        let mut r = Recorder::start(tmp("mb").join("a.cast"), 80, 24, 0).unwrap();
        for b in input.as_bytes() {
            r.write(&[*b], 1);
        }
        r.stop(2);
        let content = std::fs::read_to_string(r.path()).unwrap();
        let (_, ev) = parse_cast(&content).unwrap();
        let joined: String = ev.iter().map(|e| e.data.as_str()).collect();
        assert_eq!(joined, input, "劈块不得产生替换符（实得 {joined:?}）");
        assert!(
            !content.contains('\u{FFFD}'),
            "录屏文件里不得出现 U+FFFD——那是回放屏幕上的永久乱码"
        );
        std::fs::remove_dir_all(r.path().parent().unwrap()).ok();
    }

    /// utf8_carry 的残留判定：不完整序列留下、下一块补齐后输出完整字符。
    #[test]
    fn utf8_carry_holds_incomplete_tail() {
        // 「中」= E4 B8 AD；劈成 [E4] 与 [B8 AD]
        let (t1, rest) = utf8_carry(b"", &[0xE4]);
        assert!(t1.is_empty(), "不完整序列不得输出");
        assert_eq!(rest, vec![0xE4]);
        let (t2, rest2) = utf8_carry(&rest, &[0xB8, 0xAD, b'x']);
        assert_eq!(t2, "中x");
        assert!(rest2.is_empty());
        // 纯 ASCII 直通
        let (t3, r3) = utf8_carry(b"", b"abc");
        assert_eq!((t3.as_str(), r3.len()), ("abc", 0));
        // 4 字节 emoji（🙂 = F0 9F 99 82）劈三处
        let (t4a, r4a) = utf8_carry(b"", &[0xF0, 0x9F]);
        assert!(t4a.is_empty());
        let (t4b, r4b) = utf8_carry(&r4a, &[0x99]);
        assert!(t4b.is_empty());
        let (t4c, _) = utf8_carry(&r4b, &[0x82]);
        assert_eq!(t4c, "🙂");
        // 损坏输入（孤立续字节）不过度囤积
        let (t5, r5) = utf8_carry(b"", &[0x80, 0x80, 0x80, 0x80, b'a']);
        assert_eq!(
            t5.chars().count(),
            5,
            "损坏序列交给 lossy，不当残留囤着（4 个替换符 + a）"
        );
        assert!(r5.is_empty());
    }

    /// resize 事件与 idle/active 生命周期。
    #[test]
    fn resize_marker_and_lifecycle() {
        let dir = tmp("resize");
        let mut r = Recorder::idle(dir.join("x.cast"));
        assert!(!r.is_active());
        r.write(b"ignored", 5); // idle：丢弃
        r.start_into(dir.join("x.cast"), 100, 30, 1_000).unwrap();
        assert!(r.is_active());
        r.write(b"data", 1_500);
        r.resize(120, 40, 2_000);
        r.stop(3_000);
        assert!(!r.is_active());
        r.write(b"after stop", 4_000); // 停录后丢弃

        let (h, ev) = parse_cast(&std::fs::read_to_string(dir.join("x.cast")).unwrap()).unwrap();
        assert_eq!((h.width, h.height), (100, 30));
        // idle 与停录后的写入都不在文件里
        assert_eq!(ev.len(), 2);
        assert_eq!(ev[0].data, "data");
        assert_eq!(ev[1].kind, "r");
        assert_eq!(ev[1].data, "120x40");
        assert_eq!(ev[1].t, 1.0);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 事件行的 JSON 转义：引号、反斜杠、控制字符都合法。
    #[test]
    fn event_line_escapes_json_properly() {
        let line = event_line(1.5, "o", "a\"b\\c\u{1b}[2J").unwrap();
        let v: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(v[0], serde_json::json!(1.5));
        assert_eq!(v[1], "o");
        assert_eq!(v[2], "a\"b\\c\u{1b}[2J");
        // 时间格式化为三位小数
        assert!(line.starts_with("[1.500,"), "实得 {line:?}");
    }

    /// 解析器对坏文件报出**第几行**错在哪。
    #[test]
    fn parse_cast_rejects_and_names_the_line() {
        let (h, _) =
            parse_cast("{\"version\":2,\"width\":1,\"height\":1,\"timestamp\":0}\n").unwrap();
        assert_eq!(h.version, 2);
        for bad in [
            "",
            "not json",
            "{\"version\":1,\"width\":1,\"height\":1,\"timestamp\":0}", // v1
            "{\"version\":2,\"width\":1,\"height\":1,\"timestamp\":0}\n[1]", // 两元素
            "{\"version\":2,\"width\":1,\"height\":1,\"timestamp\":0}\n[1,\"x\",\"y\"]", // 未知类型
            "{\"version\":2,\"width\":1,\"height\":1,\"timestamp\":0}\n[\"a\",\"o\",\"b\"]", // 时间非数字
        ] {
            assert!(parse_cast(bad).is_err(), "应拒绝：{bad:?}");
        }
        // 报错带行号
        let e = parse_cast(
            "{\"version\":2,\"width\":1,\"height\":1,\"timestamp\":0}\n[1,\"o\",\"a\"]\ngarbage",
        )
        .unwrap_err();
        assert!(e.contains("第 3"), "实得 {e}");
    }

    /// 超上限停录而不是无限写。
    #[test]
    fn oversized_recording_stops_instead_of_filling_the_disk() {
        // 上限按字节计；测试里不真写 256MB——改为直接构造 written 超标的录制器，
        // 验证 write 到点即停。256MB 上限的数值本身由 CAST_BYTES_MAX 断言钉住。
        assert_eq!(CAST_BYTES_MAX, 256 * 1024 * 1024);
        let dir = tmp("cap");
        let mut r = Recorder::start(dir.join("x.cast"), 1, 1, 0).unwrap();
        r.written = CAST_BYTES_MAX; // 模拟已到量
        r.write(b"more", 1);
        assert!(!r.is_active(), "到量后 write 应自行停录");
        let content = std::fs::read_to_string(dir.join("x.cast")).unwrap();
        assert!(!content.contains("more"), "停录后的字节不得落盘");
        std::fs::remove_dir_all(&dir).ok();
    }
}
