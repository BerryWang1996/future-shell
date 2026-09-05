//! 串口会话的运行时（M7.4）：一个专职任务持有端口，负责读、写、改参数、断开重连。
//!
//! # 为什么是「一个任务独占端口」而不是把流拆成读写两半
//!
//! 改波特率要拿到端口本体（`SerialPort::set_baud_rate`），而 `tokio::io::split` 之后
//! 两个半边都碰不到它。再开一个句柄（`try_clone`）在三个平台上的语义各不相同，
//! 还要处理「两个句柄的设置谁说了算」。一个任务独占端口把这些全消掉：读、写、改参数
//! 都是这个任务里的分支，天然串行，不需要任何跨句柄的一致性论证。
//!
//! 代价是读写各多一次内存搬运（经 mpsc）。串口最高不过几 Mbaud，这点开销可以忽略——
//! 而它换来的是**断开重连时终端不受影响**：端口没了就换一个新的塞回同一个任务，
//! [`SerialReader`] 那一侧的通道从头到尾没断过，于是 `SessionPipe` 不知道底下发生过什么，
//! 网格、滚动缓冲、标签、录制全部原样活着。这正是出口标准要的「断开重连」。
//!
//! # 为什么要有 [`PortFactory`]
//!
//! 真串口不可能在单元测试里存在。把「怎么开一个口」抽成 trait 之后，重连循环、写路径、
//! 改波特率这些**逻辑**可以用一对内存管道跑完；真硬件那一路（`TokioSerialFactory`）
//! 只剩几行映射，由容器里的 socat 伪终端去证（见 `crates/itest`）。

use crate::params::SerialParams;
use crate::reconnect::{backoff, classify_io, Disconnect};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

/// 「怎么打开一个端口」。真实现见 [`TokioSerialFactory`]；测试用内存管道实现它。
pub trait PortFactory: Send + 'static {
    type Port: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send;
    /// 打开。失败返回给用户看的原因。
    fn open(&mut self, params: &SerialParams) -> Result<Self::Port, String>;
    /// 改波特率（不重开端口，见 `link::set_baud` 的理由）。
    fn set_baud(&mut self, port: &mut Self::Port, baud: u32) -> Result<(), String>;
}

/// 真实现：`tokio_serial`。
pub struct TokioSerialFactory;

impl PortFactory for TokioSerialFactory {
    type Port = tokio_serial::SerialStream;
    fn open(&mut self, params: &SerialParams) -> Result<Self::Port, String> {
        crate::link::open(params)
    }
    fn set_baud(&mut self, port: &mut Self::Port, baud: u32) -> Result<(), String> {
        crate::link::set_baud(port, baud)
    }
}

/// 会话的对外事件。app 层把它翻成 `session:status` / `session:disconnected`，
/// 与 SSH 那一路**同名同义**——出口标准要的「公共设施上行为一致」从这里开始。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SerialEvent {
    /// 端口打开了（首次连接不发，只有重连成功才发——首次成功由 `spawn` 的返回值表达）。
    Reconnected,
    /// 断了，正在等着重来。
    Disconnected(Disconnect),
    /// 重连第 n 次尝试失败，原因。
    RetryFailed { attempt: u32, error: String },
    /// 任务结束（用户关闭）。
    Closed,
}

/// 控制指令。
enum Ctl {
    SetBaud(u32, tokio::sync::oneshot::Sender<Result<(), String>>),
    Close,
}

/// 会话句柄：写字节、改参数、关。
pub struct SerialLink {
    writes: mpsc::Sender<Vec<u8>>,
    ctl: mpsc::Sender<Ctl>,
    params: Arc<Mutex<SerialParams>>,
}

impl SerialLink {
    /// 往串口写。写队列满 = 端口正堵着（对端没在收 / 硬件流控压着），此时**阻塞等待**
    /// 而不是丢弃：终端输入丢一个字节就是用户敲的命令少一个字符，比等一会儿糟得多。
    pub async fn write(&self, bytes: Vec<u8>) -> Result<(), String> {
        self.writes
            .send(bytes)
            .await
            .map_err(|_| "串口会话已结束".to_string())
    }

    /// 改波特率。成功后同步更新本地参数快照（状态栏读它）。
    pub async fn set_baud(&self, baud: u32) -> Result<(), String> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.ctl
            .send(Ctl::SetBaud(baud, tx))
            .await
            .map_err(|_| "串口会话已结束".to_string())?;
        let r = rx.await.map_err(|_| "串口会话已结束".to_string())?;
        if r.is_ok() {
            self.params.lock().unwrap_or_else(|p| p.into_inner()).baud = baud;
        }
        r
    }

    /// 当前参数快照（含改过的波特率）。
    pub fn params(&self) -> SerialParams {
        self.params
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// 用户主动关闭：任务退出且**不重连**。
    pub async fn close(&self) {
        let _ = self.ctl.send(Ctl::Close).await;
    }
}

/// 串口字节流 → `AsyncRead`（喂给 `fs_terminal::SessionPipe::spawn`）。
///
/// 有界通道保留背压：管道暂停读取 → 这里 `send` 阻塞 → 串口任务停止 read →
/// 硬件 FIFO 自己顶着（有流控时还会传压到对端）。与 SSH 那一路的 `ChannelReader` 同构。
pub struct SerialReader {
    rx: mpsc::Receiver<Vec<u8>>,
    cur: Option<(Vec<u8>, usize)>,
}

impl tokio::io::AsyncRead for SerialReader {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        loop {
            if let Some((chunk, idx)) = self.cur.as_mut() {
                if *idx < chunk.len() {
                    let n = buf.remaining().min(chunk.len() - *idx);
                    buf.put_slice(&chunk[*idx..*idx + n]);
                    *idx += n;
                    return std::task::Poll::Ready(Ok(()));
                }
            }
            self.cur = None;
            match self.rx.poll_recv(cx) {
                std::task::Poll::Ready(Some(chunk)) => self.cur = Some((chunk, 0)),
                std::task::Poll::Ready(None) => return std::task::Poll::Ready(Ok(())), // EOF
                std::task::Poll::Pending => return std::task::Poll::Pending,
            }
        }
    }
}

/// 起一个串口会话。
///
/// **首次打开必须成功**：打不开就返回 `Err`，由调用方原样呈现给用户（「COM3 被占着」这种话
/// 只有在这一刻说才有用）。此后的断开一律走自动重连——用户已经知道这个口是通的了，
/// 拔线插回来不该要他再点一次「连接」。
pub fn spawn<F: PortFactory>(
    mut factory: F,
    params: SerialParams,
    events: mpsc::Sender<SerialEvent>,
) -> Result<(SerialLink, SerialReader), String> {
    let mut port = factory.open(&params)?;
    let (data_tx, data_rx) = mpsc::channel::<Vec<u8>>(16);
    let (write_tx, mut write_rx) = mpsc::channel::<Vec<u8>>(16);
    let (ctl_tx, mut ctl_rx) = mpsc::channel::<Ctl>(4);
    let shared = Arc::new(Mutex::new(params.clone()));
    let shared_c = shared.clone();

    tokio::spawn(async move {
        let mut buf = vec![0u8; 8 * 1024];
        loop {
            let disconnect = loop {
                tokio::select! {
                    // biased：控制指令优先。用户点了「关闭」而端口正在狂吐数据时，
                    // 随机择序会让关闭指令排在几十毫秒的数据后面——表现是「点了关不掉」。
                    biased;
                    ctl = ctl_rx.recv() => match ctl {
                        Some(Ctl::Close) | None => break Disconnect::UserClosed,
                        Some(Ctl::SetBaud(b, reply)) => {
                            let _ = reply.send(factory.set_baud(&mut port, b));
                        }
                    },
                    w = write_rx.recv() => match w {
                        // 写半部全部 drop = 会话已被弃用，按用户关闭收尾
                        None => break Disconnect::UserClosed,
                        Some(bytes) => {
                            if let Err(e) = port.write_all(&bytes).await {
                                break classify_io(&e);
                            }
                            // 每次写完就 flush：终端输入是逐键的，攒着会让回显延迟到下一次按键
                            let _ = port.flush().await;
                        }
                    },
                    r = port.read(&mut buf) => match r {
                        // 串口的 EOF 不是「正常结束」：设备被拔掉时读到的就是 0/错误。
                        // 按设备消失处理并重连——这正是用户拔了再插要发生的事。
                        Ok(0) => break Disconnect::DeviceGone,
                        Ok(n) => {
                            if data_tx.send(buf[..n].to_vec()).await.is_err() {
                                // 读取端（SessionPipe）没了：会话已经被拆了，静默收尾
                                break Disconnect::UserClosed;
                            }
                        }
                        Err(e) => break classify_io(&e),
                    },
                }
            };

            let _ = events.send(SerialEvent::Disconnected(disconnect)).await;
            if !disconnect.should_reconnect() {
                let _ = events.send(SerialEvent::Closed).await;
                return;
            }

            // 重连：不设次数上限（见 reconnect 模块头）。用户的「关闭」在等待期间照样要立刻生效。
            let mut attempt: u32 = 0;
            loop {
                attempt = attempt.saturating_add(1);
                let wait = backoff(attempt);
                tokio::select! {
                    biased;
                    ctl = ctl_rx.recv() => {
                        // 等待期间用户关掉了标签：立刻收尾，不要等完这一轮退避
                        if matches!(ctl, Some(Ctl::Close) | None) {
                            let _ = events.send(SerialEvent::Closed).await;
                            return;
                        }
                    }
                    _ = tokio::time::sleep(wait) => {}
                }
                let p = shared_c.lock().unwrap_or_else(|x| x.into_inner()).clone();
                match factory.open(&p) {
                    Ok(fresh) => {
                        port = fresh;
                        let _ = events.send(SerialEvent::Reconnected).await;
                        break;
                    }
                    Err(e) => {
                        let _ = events
                            .send(SerialEvent::RetryFailed { attempt, error: e })
                            .await;
                    }
                }
            }
        }
    });

    Ok((
        SerialLink {
            writes: write_tx,
            ctl: ctl_tx,
            params: shared,
        },
        SerialReader {
            rx: data_rx,
            cur: None,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// 内存里的假端口：一对 duplex，另一头留给测试当「板子」。
    struct FakeFactory {
        /// 每次 open 交出去的端口（用完即弹）；空了就报错，用来演「插不回来」。
        ports: Arc<Mutex<Vec<tokio::io::DuplexStream>>>,
        opens: Arc<AtomicU32>,
        bauds: Arc<Mutex<Vec<u32>>>,
        /// 非 None 时 set_baud 直接失败（演硬件不支持这个速率）。
        baud_err: Option<String>,
    }

    impl PortFactory for FakeFactory {
        type Port = tokio::io::DuplexStream;
        fn open(&mut self, _p: &SerialParams) -> Result<Self::Port, String> {
            self.opens.fetch_add(1, Ordering::SeqCst);
            self.ports
                .lock()
                .unwrap()
                .pop()
                .ok_or_else(|| "没有可用端口".to_string())
        }
        fn set_baud(&mut self, _port: &mut Self::Port, baud: u32) -> Result<(), String> {
            if let Some(e) = &self.baud_err {
                return Err(e.clone());
            }
            self.bauds.lock().unwrap().push(baud);
            Ok(())
        }
    }

    fn params() -> SerialParams {
        SerialParams {
            port: "FAKE".into(),
            ..Default::default()
        }
    }

    /// 造 n 个端口对之后交出来的一整套把手：假工厂、板子那一侧（按**打开顺序**排列）、
    /// open 次数、被设过的波特率。具名成 struct 而不是四元组——四元组在调用点是
    /// `let (f, mut theirs, opens, bauds) = ...`，顺序记错了编译器也未必拦得住。
    struct Rig {
        factory: FakeFactory,
        boards: Vec<tokio::io::DuplexStream>,
        opens: Arc<AtomicU32>,
        bauds: Arc<Mutex<Vec<u32>>>,
    }

    fn factory(n: usize) -> Rig {
        let mut ours = Vec::new();
        let mut theirs = Vec::new();
        for _ in 0..n {
            let (a, b) = tokio::io::duplex(4096);
            ours.push(a);
            theirs.push(b);
        }
        ours.reverse(); // pop() 取出的顺序 = 打开顺序
        let opens = Arc::new(AtomicU32::new(0));
        let bauds = Arc::new(Mutex::new(Vec::new()));
        Rig {
            factory: FakeFactory {
                ports: Arc::new(Mutex::new(ours)),
                opens: opens.clone(),
                bauds: bauds.clone(),
                baud_err: None,
            },
            boards: theirs,
            opens,
            bauds,
        }
    }

    async fn read_some(r: &mut SerialReader, want: usize) -> Vec<u8> {
        let mut out = vec![0u8; want];
        let mut got = 0;
        while got < want {
            let n = tokio::time::timeout(
                std::time::Duration::from_millis(500),
                r.read(&mut out[got..]),
            )
            .await
            .expect("读超时")
            .expect("读失败");
            if n == 0 {
                break;
            }
            got += n;
        }
        out.truncate(got);
        out
    }

    #[tokio::test]
    async fn bytes_flow_both_ways() {
        let Rig {
            factory: f,
            boards: mut theirs,
            ..
        } = factory(1);
        let (tx, _rx) = mpsc::channel(8);
        let (link, mut reader) = spawn(f, params(), tx).unwrap();
        let mut board = theirs.pop().unwrap();

        board.write_all(b"boot ok\r\n").await.unwrap();
        assert_eq!(read_some(&mut reader, 9).await, b"boot ok\r\n");

        link.write(b"help\r".to_vec()).await.unwrap();
        let mut got = [0u8; 5];
        tokio::time::timeout(
            std::time::Duration::from_millis(500),
            board.read_exact(&mut got),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(&got, b"help\r");
    }

    /// 首次打开失败要**当场报错**，不能吞掉转成静默重连——用户点了「连接」却什么都没发生。
    #[tokio::test]
    async fn first_open_failure_is_reported_not_swallowed() {
        let Rig {
            factory: f, opens, ..
        } = factory(0);
        let (tx, _rx) = mpsc::channel(8);
        let e = match spawn(f, params(), tx) {
            Ok(_) => panic!("首次打开本该失败"),
            Err(e) => e,
        };
        assert!(e.contains("没有可用端口"), "{e}");
        assert_eq!(
            opens.load(Ordering::SeqCst),
            1,
            "失败了就不该再试（首次不重连）"
        );
    }

    /// 拔线 → 插回来：终端那一侧的流**没有断**（同一个 SerialReader 继续出字节），
    /// 所以 SessionPipe / 网格 / 滚动缓冲 / 标签全都原样活着。
    #[tokio::test(start_paused = true)]
    async fn unplug_and_replug_keeps_the_same_stream() {
        let Rig {
            factory: f,
            boards: mut theirs,
            opens,
            ..
        } = factory(2);
        let (tx, mut ev) = mpsc::channel(8);
        let (_link, mut reader) = spawn(f, params(), tx).unwrap();
        let mut first = theirs.remove(0);
        let mut second = theirs.remove(0);

        first.write_all(b"A").await.unwrap();
        assert_eq!(read_some(&mut reader, 1).await, b"A");

        drop(first); // 拔线
        assert_eq!(
            ev.recv().await.unwrap(),
            SerialEvent::Disconnected(Disconnect::DeviceGone)
        );
        assert_eq!(ev.recv().await.unwrap(), SerialEvent::Reconnected);
        assert_eq!(opens.load(Ordering::SeqCst), 2);

        second.write_all(b"B").await.unwrap();
        assert_eq!(
            read_some(&mut reader, 1).await,
            b"B",
            "重连后仍是同一条读流"
        );
    }

    /// 插不回来时**一直等**，不放弃。退避涨到上限就按上限巡检。
    #[tokio::test(start_paused = true)]
    async fn reconnect_never_gives_up() {
        let Rig {
            factory: f,
            boards: mut theirs,
            opens,
            ..
        } = factory(1);
        let (tx, mut ev) = mpsc::channel(64);
        let (_link, _reader) = spawn(f, params(), tx).unwrap();
        drop(theirs.pop().unwrap());
        assert_eq!(
            ev.recv().await.unwrap(),
            SerialEvent::Disconnected(Disconnect::DeviceGone)
        );
        for want in 1..=6u32 {
            match ev.recv().await.unwrap() {
                SerialEvent::RetryFailed { attempt, .. } => assert_eq!(attempt, want),
                other => panic!("意外事件 {other:?}"),
            }
        }
        assert!(opens.load(Ordering::SeqCst) >= 7, "还在试");
    }

    /// 用户在等待重连期间关掉标签：**立刻**收尾，不要等完这一轮退避。
    #[tokio::test(start_paused = true)]
    async fn close_during_reconnect_wait_is_immediate() {
        let Rig {
            factory: f,
            boards: mut theirs,
            ..
        } = factory(1);
        let (tx, mut ev) = mpsc::channel(64);
        let (link, _reader) = spawn(f, params(), tx).unwrap();
        drop(theirs.pop().unwrap());
        assert_eq!(
            ev.recv().await.unwrap(),
            SerialEvent::Disconnected(Disconnect::DeviceGone)
        );
        link.close().await;
        loop {
            match ev.recv().await.unwrap() {
                SerialEvent::Closed => break,
                SerialEvent::RetryFailed { .. } => continue,
                other => panic!("意外事件 {other:?}"),
            }
        }
    }

    /// 主动关闭不触发重连——重连会让「关掉的标签自己活过来」。
    #[tokio::test]
    async fn user_close_does_not_reconnect() {
        let Rig {
            factory: f,
            boards: theirs,
            opens,
            ..
        } = factory(2);
        let (tx, mut ev) = mpsc::channel(8);
        let (link, _reader) = spawn(f, params(), tx).unwrap();
        link.close().await;
        assert_eq!(
            ev.recv().await.unwrap(),
            SerialEvent::Disconnected(Disconnect::UserClosed)
        );
        assert_eq!(ev.recv().await.unwrap(), SerialEvent::Closed);
        drop(theirs);
        assert_eq!(opens.load(Ordering::SeqCst), 1);
    }

    /// 改波特率：不重开端口（重开会抖 DTR，而很多板子的 DTR 接在复位脚上）。
    #[tokio::test]
    async fn set_baud_does_not_reopen_the_port() {
        let Rig {
            factory: f,
            boards: mut theirs,
            opens,
            bauds,
        } = factory(1);
        let (tx, _rx) = mpsc::channel(8);
        let (link, mut reader) = spawn(f, params(), tx).unwrap();
        let mut board = theirs.pop().unwrap();
        link.set_baud(9600).await.unwrap();
        assert_eq!(*bauds.lock().unwrap(), vec![9600]);
        assert_eq!(opens.load(Ordering::SeqCst), 1, "改波特率不该重开端口");
        assert_eq!(link.params().baud, 9600, "参数快照要跟上（状态栏读它）");
        // 端口还活着
        board.write_all(b"Z").await.unwrap();
        assert_eq!(read_some(&mut reader, 1).await, b"Z");
    }

    /// 改波特率失败时**参数快照不能跟着变**：状态栏显示 9600 而硬件还在 115200，
    /// 会让用户以为是板子的问题，去查一个根本不存在的故障。
    #[tokio::test]
    async fn failed_baud_change_leaves_the_snapshot_alone() {
        let Rig { factory: mut f, .. } = factory(1);
        f.baud_err = Some("这块硬件不支持 9600".into());
        let (tx, _rx) = mpsc::channel(8);
        let (link, _reader) = spawn(f, params(), tx).unwrap();
        let e = link.set_baud(9600).await.unwrap_err();
        assert!(e.contains("不支持"));
        assert_eq!(link.params().baud, 115_200);
    }

    /// 会话结束之后再写：报错而不是静默丢弃（静默丢弃 = 用户敲的命令凭空消失）。
    #[tokio::test]
    async fn writing_after_close_reports_an_error() {
        let Rig { factory: f, .. } = factory(1);
        let (tx, mut ev) = mpsc::channel(8);
        let (link, _reader) = spawn(f, params(), tx).unwrap();
        link.close().await;
        while !matches!(ev.recv().await, Some(SerialEvent::Closed) | None) {}
        // 任务已退出，写请求无人接收
        for _ in 0..40 {
            if link.write(b"x".to_vec()).await.is_err() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("会话已结束却仍然接受写入");
    }
}
