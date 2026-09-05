//! 会话日志 tap 的管道级验收（M4a 出口标准「开启日志的会话产出转录文件
//! （目录/命名/追加可配），断线重连续写，与录屏可独立开关」）。
//!
//! 这里测的是**管道到文件**这一段：`PipeOpts.session_log` 接上之后，source
//! 字节是否真的落进文件、是否剥净 ANSI、重连（新 pipe 同路径）是否续写。
//! 命名模板/剥离状态机的单元判据在 `src/sessionlog.rs` 的 mod tests（S315–S317）。
//!
//! 为什么在 tests/ 而不是单元测试里：判据涉及 `SessionPipe::spawn` 的 fan-out
//! 次序（日志 tap 必须在背压丢弃**之前**），只有跑真管道才证得到。

use fs_terminal::pipe::{PipeOpts, SessionPipe};
use fs_terminal::sessionlog::{LogMode, SessionLog};
use std::time::Duration;
use tokio::io::{duplex, AsyncWriteExt};

fn scratch(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("fs-slog-pipe-{}-{}", std::process::id(), tag));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn opts_with_log(path: std::path::PathBuf, mode: LogMode) -> PipeOpts {
    PipeOpts {
        grid_rows: 24,
        grid_cols: 80,
        scrollback_lines: fs_terminal::grid::DEFAULT_SCROLLBACK_LINES,
        flow: Default::default(),
        ring_bytes: 4096,
        session_log: Some(SessionLog::new(path, mode)),
        tap: None,
        decoder: Default::default(),
        record: None,
    }
}

/// S318：开启日志的会话产出转录文件，内容剥净 ANSI（人可读、可 grep）。
#[tokio::test]
async fn session_with_logging_produces_plain_transcript() {
    let dir = scratch("basic");
    let path = dir.join("t.log");
    let (mut writer, reader) = duplex(64 * 1024);
    let mut pipe = SessionPipe::spawn(reader, opts_with_log(path.clone(), LogMode::Append));
    let _rx = pipe.render_rx();

    // 典型带色输出：`ls` 的绿色文件名 + 标题设置 OSC + CRLF
    writer
        .write_all(b"\x1b]0;root@web-01\x07\x1b[0;32mfoo.txt\x1b[0m\r\n$ ")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(60)).await;

    let got = std::fs::read_to_string(&path).expect("转录文件须已生成");
    assert_eq!(
        got, "foo.txt\n$ ",
        "转录须剥净 ANSI/OSC 与 CR（否则 less 满屏乱码、grep 匹配不上）"
    );
}

/// S318：**断线重连续写**——新 pipe 用同路径 + Append 接着写，旧内容不丢。
#[tokio::test]
async fn reconnect_appends_to_same_file() {
    let dir = scratch("reconnect");
    let path = dir.join("r.log");
    {
        let (mut w, r) = duplex(4096);
        let mut pipe = SessionPipe::spawn(r, opts_with_log(path.clone(), LogMode::Append));
        let _rx = pipe.render_rx();
        w.write_all(b"before-drop\n").await.unwrap();
        tokio::time::sleep(Duration::from_millis(60)).await;
        pipe.shutdown();
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    {
        // 「重连」：同一 session、新管道、同路径
        let (mut w, r) = duplex(4096);
        let mut pipe = SessionPipe::spawn(r, opts_with_log(path.clone(), LogMode::Append));
        let _rx = pipe.render_rx();
        w.write_all(b"after-reconnect\n").await.unwrap();
        tokio::time::sleep(Duration::from_millis(60)).await;
        pipe.shutdown();
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "before-drop\nafter-reconnect\n",
        "重连须续写同一文件（Append 模式），旧内容不得被截断"
    );
}

/// S318：日志 tap 在**背压丢弃之前**——屏幕可以丢一帧重绘，日志不能缺证据。
///
/// 构造：极低水位 + 不消费渲染帧 ⇒ 背压激活、滞留超时后 drop_backlog 丢弃积压；
/// 判据：被丢弃的那些字节**仍在日志文件里**。
#[tokio::test]
async fn log_tap_precedes_backpressure_drop() {
    let dir = scratch("beforedrop");
    let path = dir.join("d.log");
    let (mut writer, reader) = duplex(256 * 1024);
    let mut o = opts_with_log(path.clone(), LogMode::Append);
    o.flow.queue_bytes_high = 4 * 1024;
    o.flow.queue_bytes_low = 2 * 1024;
    o.flow.stall_timeout = Duration::from_millis(80); // 快速进入丢弃
    let mut pipe = SessionPipe::spawn(reader, o);
    let _rx = pipe.render_rx(); // 取走但**不消费不 ack** → 水位只升不降

    let payload = vec![b'x'; 32 * 1024];
    writer.write_all(&payload).await.unwrap();
    // 等背压激活 → 滞留超时 → drop_backlog → 读取恢复
    tokio::time::sleep(Duration::from_millis(400)).await;

    let logged = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    assert!(
        logged >= 4 * 1024,
        "日志须录到背压期间的字节（tap 在丢弃之前），实得 {logged} 字节"
    );
    // 渲染侧确实经历了丢弃（水位被复位），证明本例真的走到了丢弃路径
    assert_eq!(
        pipe.queue_bytes(),
        0,
        "前置：本例须真的触发 drop_backlog（水位复位为 0）"
    );
}

/// 未开启日志（session_log: None）时不产生任何文件——「与录屏可独立开关」的
/// 关字一侧：关掉就是一个字节都不写。
#[tokio::test]
async fn logging_disabled_writes_nothing() {
    let dir = scratch("off");
    let (mut writer, reader) = duplex(4096);
    let mut pipe = SessionPipe::spawn(
        reader,
        PipeOpts {
            grid_rows: 24,
            grid_cols: 80,
            scrollback_lines: fs_terminal::grid::DEFAULT_SCROLLBACK_LINES,
            flow: Default::default(),
            ring_bytes: 4096,
            session_log: None,
            tap: None,
            decoder: Default::default(),
            record: None,
        },
    );
    let _rx = pipe.render_rx();
    writer.write_all(b"secret command\n").await.unwrap();
    tokio::time::sleep(Duration::from_millis(60)).await;
    let entries: Vec<_> = std::fs::read_dir(&dir).unwrap().collect();
    assert!(
        entries.is_empty(),
        "未开启转录时目录须为空，实得 {entries:?}"
    );
}
