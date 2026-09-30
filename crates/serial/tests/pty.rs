//! 对**真串口驱动**的集成测试（M7.4 出口标准①「真串口设备（或虚拟串口对）跑通：
//! 打开/收发/改波特率/断开重连」）。
//!
//! # 载体：socat 造的伪终端对
//!
//! 用户裁定（2026-09-04）：用 Docker 虚拟环境。`socat` 把两个伪终端连成一对，两侧都是
//! **真的 tty 设备**——`serialport` 走的是与真 USB 转串口完全相同的那条路：`open(2)` +
//! `termios` + `read/write`。它证不了的只有硬件那一层（真实波特率是否生效、RTS/CTS 电平），
//! 那部分在容器里不可能有，如实标注、不代签。
//!
//! # 为什么由测试自己拉起 socat
//!
//! 「断开重连」要能**杀掉设备再让它回来**。测试自己 spawn socat 就拿到了这个开关：
//! kill 掉 = 拔线，重新 spawn = 插回去。交给外部脚本编排的话，这一条只能靠 sleep 猜时序。
//!
//! # 不设 FS_SERIAL_PTY 时整组跳过
//!
//! Windows 开发机上没有 socat，也没有 `/dev/pts`。跳过而不是失败——但**跳过必须打印出来**，
//! 否则「没跑」会被读成「跑过了」。

#![cfg(unix)]

use fs_serial::params::SerialParams;
use fs_serial::reconnect::Disconnect;
use fs_serial::session::{SerialEvent, TokioSerialFactory};
use std::process::{Child, Command, Stdio};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const A: &str = "/tmp/fs-serial-a";
const B: &str = "/tmp/fs-serial-b";

/// 本组测试要不要跑。
fn enabled() -> bool {
    if std::env::var("FS_SERIAL_PTY").is_ok() {
        return true;
    }
    eprintln!("skip: 需要 socat 伪终端对；设 FS_SERIAL_PTY=1 并保证 socat 在 PATH 上（见 scripts/serial-itest.sh）");
    false
}

/// 拉起一对伪终端。返回 socat 进程句柄（drop 即杀 = 拔线）。
struct Pty(Child);

impl Drop for Pty {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
        let _ = std::fs::remove_file(A);
        let _ = std::fs::remove_file(B);
    }
}

fn start_pty() -> Pty {
    let _ = std::fs::remove_file(A);
    let _ = std::fs::remove_file(B);
    let child = Command::new("socat")
        .args([
            "-d",
            "-d",
            &format!("pty,raw,echo=0,link={A}"),
            &format!("pty,raw,echo=0,link={B}"),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("拉不起 socat（镜像里没装？）");
    // spawn 之后**立刻**交给 Pty：下面等链接超时会 panic，那条路上子进程若没人
    // wait 就成了僵尸（clippy::zombie_processes 在 Linux CI 上拦下的正是这一条，
    // Windows 上本文件整体 cfg(unix) 编译不到，本地 clippy 看不见）。
    // 交给 Pty 之后，panic 展开时 Drop 负责 kill + wait，所有路径都收尸。
    let pty = Pty(child);
    // 等两条符号链接出现。socat 建链接要几十毫秒，直接开会撞上 ENOENT。
    for _ in 0..200 {
        if std::path::Path::new(A).exists() && std::path::Path::new(B).exists() {
            std::thread::sleep(Duration::from_millis(30));
            return pty;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("socat 没有在 4 秒内建出 {A} / {B}");
}

fn params(port: &str) -> SerialParams {
    SerialParams {
        port: port.into(),
        baud: 115_200,
        ..Default::default()
    }
}

/// 读满 n 字节（带超时）。
async fn read_exact_timeout<R: tokio::io::AsyncRead + Unpin>(
    r: &mut R,
    n: usize,
    ms: u64,
) -> Vec<u8> {
    let mut out = vec![0u8; n];
    let mut got = 0;
    let deadline = tokio::time::Instant::now() + Duration::from_millis(ms);
    while got < n {
        let k = tokio::time::timeout_at(deadline, r.read(&mut out[got..]))
            .await
            .expect("读超时")
            .expect("读失败");
        assert!(k > 0, "对端 EOF");
        got += k;
    }
    out
}

/// ① 打开 + 双向收发。**二进制原样**：串口上跑的不都是文本（固件、ZMODEM 帧），
/// 任何一处按文本处理都会毁掉高位字节。
#[tokio::test]
async fn open_and_exchange_bytes_both_ways() {
    if !enabled() {
        return;
    }
    let _pty = start_pty();
    let mut a = fs_serial::link::open(&params(A)).expect("打不开 A");
    let mut b = fs_serial::link::open(&params(B)).expect("打不开 B");

    let payload: Vec<u8> = (0u8..=255).collect();
    a.write_all(&payload).await.unwrap();
    a.flush().await.unwrap();
    assert_eq!(
        read_exact_timeout(&mut b, payload.len(), 3000).await,
        payload
    );

    b.write_all(b"pong\r\n").await.unwrap();
    b.flush().await.unwrap();
    assert_eq!(read_exact_timeout(&mut a, 6, 3000).await, b"pong\r\n");
}

/// ② 改波特率：**不重开端口**，改完还能继续收发。
///
/// 伪终端不会真的按 9600 传输（没有硬件），但 `tcsetattr` 这条路径与真设备完全相同——
/// 参数组合非法时它同样会失败。这条测的是「这条路径通、且改完端口还活着」。
#[tokio::test]
async fn change_baud_on_an_open_port() {
    if !enabled() {
        return;
    }
    let _pty = start_pty();
    let mut a = fs_serial::link::open(&params(A)).expect("打不开 A");
    let mut b = fs_serial::link::open(&params(B)).expect("打不开 B");

    fs_serial::link::set_baud(&mut a, 9600).expect("改波特率失败");
    assert_eq!(
        tokio_serial::SerialPort::baud_rate(&a).unwrap(),
        9600,
        "驱动没有真的记下新波特率"
    );
    a.write_all(b"after").await.unwrap();
    a.flush().await.unwrap();
    assert_eq!(read_exact_timeout(&mut b, 5, 3000).await, b"after");

    // 越界值必须被挡在系统调用之前（0 波特在 Linux 上是「挂断」，不是「很慢」）
    assert!(fs_serial::link::set_baud(&mut a, 0).is_err());
}

/// ③ 拔线 → 插回来：会话自己重连，**读流不断**。
///
/// 这是出口标准里「断开重连」那一档的真实证据：kill 掉 socat 就是把 USB 转换器拔了。
#[tokio::test]
async fn unplug_then_replug_reconnects_on_its_own() {
    if !enabled() {
        return;
    }
    let pty = start_pty();
    let (tx, mut ev) = tokio::sync::mpsc::channel::<SerialEvent>(64);
    let (link, mut reader) =
        fs_serial::session::spawn(TokioSerialFactory, params(A), tx).expect("首次打开失败");

    // 板子那一侧
    let mut board = fs_serial::link::open(&params(B)).expect("打不开 B");
    board.write_all(b"boot\r\n").await.unwrap();
    board.flush().await.unwrap();
    assert_eq!(read_exact_timeout(&mut reader, 6, 3000).await, b"boot\r\n");

    // 拔线
    drop(board);
    drop(pty);
    let d = tokio::time::timeout(Duration::from_secs(5), ev.recv())
        .await
        .expect("没等到断开事件")
        .expect("事件通道断了");
    assert!(
        matches!(
            d,
            SerialEvent::Disconnected(Disconnect::DeviceGone | Disconnect::IoError)
        ),
        "断开原因不对：{d:?}"
    );

    // 插回来
    let _pty2 = start_pty();
    loop {
        match tokio::time::timeout(Duration::from_secs(10), ev.recv())
            .await
            .expect("没等到重连事件")
            .expect("事件通道断了")
        {
            SerialEvent::Reconnected => break,
            SerialEvent::RetryFailed { .. } => continue,
            other => panic!("意外事件 {other:?}"),
        }
    }

    // 同一个 reader 继续出字节——终端、滚动缓冲、标签全程没断
    let mut board2 = fs_serial::link::open(&params(B)).expect("重连后打不开 B");
    board2.write_all(b"again\r\n").await.unwrap();
    board2.flush().await.unwrap();
    assert_eq!(read_exact_timeout(&mut reader, 7, 5000).await, b"again\r\n");

    // 写路径也回来了
    link.write(b"ls\r".to_vec()).await.unwrap();
    assert_eq!(read_exact_timeout(&mut board2, 3, 3000).await, b"ls\r");
}

/// ④ 端上来的 GBK 字节经整条管道之后是 UTF-8 的「中文」。
///
/// 出口标准②在真设备上的闭合：国产裸板输出 GBK，用户在档案里选了 GBK，屏幕上就该是中文。
#[tokio::test]
async fn gbk_from_a_real_port_reaches_the_terminal_as_utf8() {
    if !enabled() {
        return;
    }
    let _pty = start_pty();
    let (tx, _ev) = tokio::sync::mpsc::channel::<SerialEvent>(8);
    let (_link, reader) =
        fs_serial::session::spawn(TokioSerialFactory, params(A), tx).expect("首次打开失败");

    let mut pipe = fs_terminal::SessionPipe::spawn(
        reader,
        fs_terminal::PipeOpts {
            grid_rows: 24,
            grid_cols: 80,
            scrollback_lines: fs_terminal::grid::DEFAULT_SCROLLBACK_LINES,
            flow: Default::default(),
            ring_bytes: 4096,
            session_log: None,
            record: None,
            tap: None,
            decoder: fs_terminal::decode::StreamDecoder::new(Some("gbk")).unwrap(),
        },
    );
    let mut rx = pipe.render_rx();

    // 板子那一侧按**两次**写，正好把「中」切成两半——真实链路上这是常态。
    let mut board = fs_serial::link::open(&params(B)).expect("打不开 B");
    board.write_all(&[0xD6]).await.unwrap();
    board.flush().await.unwrap();
    tokio::time::sleep(Duration::from_millis(80)).await;
    board.write_all(&[0xD0, 0xCE, 0xC4]).await.unwrap();
    board.flush().await.unwrap();

    let mut got = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_millis(3000);
    while got.len() < "中文".len() {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Some((_seq, f))) => got.extend_from_slice(&f),
            _ => break,
        }
    }
    assert_eq!(String::from_utf8(got).unwrap(), "中文");
    pipe.shutdown();
}
