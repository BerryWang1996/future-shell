//! 字节拦截器在 fan-out 里的位置（M4a ZMODEM 的前置条件）。
//!
//! 钉的不是「拦截器能被调用」，而是它**恰好**接在日志之后、网格/环形/渲染之前。
//! 位置错一格就各有一种真实故障：接在日志之前 → 转录缺掉传输那一段；接在网格
//! 之后 → 传输期间屏幕刷乱码且终端状态可能被帧里的转义字节改坏。

use fs_terminal::pipe::{ByteTap, PipeOpts, SessionPipe};
use fs_terminal::sessionlog::{LogMode, SessionLog};
use std::borrow::Cow;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{duplex, AsyncWriteExt};

fn scratch(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!(
        "fs-tap-{tag}-{:?}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// 记录看到的字节；`swallow` 为真时吞掉全部输入。
struct RecordingTap {
    seen: Mutex<Vec<u8>>,
    swallow: AtomicBool,
    /// 吞掉状态下要交还终端的尾巴（模拟 ZFIN 之后的 shell 字节）
    give_back: Mutex<Option<Vec<u8>>>,
}

impl RecordingTap {
    fn new(swallow: bool) -> Arc<Self> {
        Arc::new(Self {
            seen: Mutex::new(Vec::new()),
            swallow: AtomicBool::new(swallow),
            give_back: Mutex::new(None),
        })
    }
}

impl ByteTap for RecordingTap {
    fn filter<'a>(&self, chunk: &'a [u8]) -> Cow<'a, [u8]> {
        self.seen.lock().unwrap().extend_from_slice(chunk);
        if !self.swallow.load(Ordering::SeqCst) {
            return Cow::Borrowed(chunk);
        }
        match self.give_back.lock().unwrap().take() {
            Some(v) => Cow::Owned(v),
            None => Cow::Borrowed(&[]),
        }
    }
}

fn opts(tap: Option<Arc<dyn ByteTap>>, log: Option<SessionLog>) -> PipeOpts {
    PipeOpts {
        grid_rows: 24,
        grid_cols: 80,
        scrollback_lines: fs_terminal::grid::DEFAULT_SCROLLBACK_LINES,
        flow: Default::default(),
        ring_bytes: 4096,
        session_log: log,
        record: None,
        tap,
        decoder: Default::default(),
    }
}

/// S337：拦截器吞掉的字节不进网格、不进环形缓冲、不进渲染队列。
///
/// 这是 ZMODEM 必须拦截而非旁观的核心判据：二进制帧喂给 vt100 会刷乱码，
/// 帧里的转义字节还可能改掉终端状态，传完之后屏幕就废了。
#[tokio::test]
async fn swallowed_bytes_reach_neither_grid_ring_nor_render() {
    let tap = RecordingTap::new(true);
    let (mut writer, reader) = duplex(64 * 1024);
    let mut pipe = SessionPipe::spawn(reader, opts(Some(tap.clone()), None));
    let mut rx = pipe.render_rx();

    // 里面刻意夹了会改终端状态的转义序列（切换备用屏 + 换字符集）
    let frame = b"*\x18C\x1b[?1049h\x1b(0binary-junk\x00\xff";
    writer.write_all(frame).await.unwrap();
    tokio::time::sleep(Duration::from_millis(80)).await;

    assert_eq!(
        tap.seen.lock().unwrap().as_slice(),
        frame,
        "拦截器须看到原始字节"
    );
    assert_eq!(pipe.grid_text().trim(), "", "被吞的字节不得进网格");
    assert_eq!(
        pipe.ring_snapshot_text(),
        "",
        "被吞的字节不得进环形缓冲（否则调试快照里是乱码）"
    );
    assert!(
        rx.try_recv().is_err(),
        "被吞的字节不得产生渲染帧（否则前端 xterm 收到二进制）"
    );
}

/// S337：拦截器**之前**的日志仍然记录全量字节。
///
/// 转录的价值在「链路上发生过什么」，一次 rz/sz 的原始帧属于该记录。
/// 若把拦截器接在日志之前，传输那一段在转录里就是空洞。
#[tokio::test]
async fn session_log_still_records_swallowed_bytes() {
    let dir = scratch("log");
    let path = dir.join("t.log");
    let tap = RecordingTap::new(true);
    let (mut writer, reader) = duplex(64 * 1024);
    let mut pipe = SessionPipe::spawn(
        reader,
        opts(
            Some(tap.clone()),
            Some(SessionLog::new(path.clone(), LogMode::Append)),
        ),
    );
    let _rx = pipe.render_rx();

    writer.write_all(b"before\r\n").await.unwrap();
    writer.write_all(b"SWALLOWED-PAYLOAD").await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    let text = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        text.contains("SWALLOWED-PAYLOAD"),
        "被拦截器吞掉的字节仍须出现在转录里（拦截点必须在日志之后），实得 {text:?}"
    );
}

/// S337：拦截器交还的字节（传输收尾时属于 shell 的那一段）正常渲染。
///
/// 不交还的表现是「传完文件提示符消失、敲回车才回来」，用户读作卡死。
#[tokio::test]
async fn returned_tail_bytes_are_rendered() {
    let tap = RecordingTap::new(true);
    *tap.give_back.lock().unwrap() = Some(b"user@host:~$ ".to_vec());
    let (mut writer, reader) = duplex(64 * 1024);
    let mut pipe = SessionPipe::spawn(reader, opts(Some(tap.clone()), None));
    let mut rx = pipe.render_rx();

    writer.write_all(b"*\x18Cframe-tail-OO").await.unwrap();
    tokio::time::sleep(Duration::from_millis(80)).await;

    let (_seq, frame) = rx.try_recv().expect("交还的尾字节须产生渲染帧");
    assert_eq!(
        String::from_utf8_lossy(&frame),
        "user@host:~$ ",
        "渲染的须是交还的字节，而不是原始帧"
    );
    assert!(
        pipe.grid_text().contains("user@host"),
        "交还的字节也要进网格，实得 {:?}",
        pipe.grid_text()
    );
}

/// S337：不吞时原样透传——常态路径零改写。
#[tokio::test]
async fn passthrough_tap_does_not_alter_normal_output() {
    let tap = RecordingTap::new(false);
    let (mut writer, reader) = duplex(64 * 1024);
    let mut pipe = SessionPipe::spawn(reader, opts(Some(tap.clone()), None));
    let mut rx = pipe.render_rx();

    writer
        .write_all(b"hello \x1b[31mred\x1b[0m\r\n")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(80)).await;

    let (_seq, frame) = rx.try_recv().expect("透传须产生渲染帧");
    assert_eq!(
        frame,
        b"hello \x1b[31mred\x1b[0m\r\n".to_vec(),
        "透传不得改写字节（含 ANSI 原样保留）"
    );
    assert!(pipe.grid_text().contains("hello red"));
}

/// S337：`tap: None` 与接一个纯透传拦截器行为一致（引入拦截点没改变默认语义）。
#[tokio::test]
async fn no_tap_matches_passthrough_tap() {
    async fn render_once(tap: Option<Arc<dyn ByteTap>>) -> (Vec<u8>, String) {
        let (mut writer, reader) = duplex(64 * 1024);
        let mut pipe = SessionPipe::spawn(reader, opts(tap, None));
        let mut rx = pipe.render_rx();
        writer.write_all(b"abc\x1b[1mdef\r\n").await.unwrap();
        tokio::time::sleep(Duration::from_millis(80)).await;
        let (_s, f) = rx.try_recv().expect("须有渲染帧");
        (f, pipe.grid_text())
    }
    let a = render_once(None).await;
    let b = render_once(Some(RecordingTap::new(false))).await;
    assert_eq!(a.0, b.0, "渲染帧须一致");
    assert_eq!(a.1, b.1, "网格文本须一致");
}
