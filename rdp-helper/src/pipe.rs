//! NetIn/NetOut 管道与 `AsyncRead`/`AsyncWrite` 之间的桥。
//!
//! IronRDP 的连接辅助（`connect_begin`/`connect_finalize`）和 TLS 升级都要一个
//! 「流」可读可写；而这个 helper 的字节实际走 stdio 上的 fs_rdpproto 帧。
//! `PipeStream` 就是那座桥：
//!
//! - 主循环收到 `NetIn(body)` → [`PipeStream::push_net`]；
//! - 主循环收到 `NetEof` → [`PipeStream::close`]（此后读端返回 0 = EOF）；
//! - IronRDP 往流里写 → `poll_write` 进发出队列 → 主循环 [`PipeStream::drain_out`]
//!   取走、封装成 `NetOut` 帧发往主程序。
//!
//! # 写端永不阻塞
//!
//! `poll_write` 只往无界队列里塞字节，永远立刻返回 `Ready`。RDP 的写出量
//! （输入事件、确认帧）与读入量（像素）相比小得多，无界队列不会失控；
//! 真正的背压在主程序那侧（它知道前端跟不跟得上，helper 不知道也不该知道）。

use std::collections::VecDeque;
use std::io;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::Notify;

#[derive(Default)]
struct Shared {
    /// 到达的线上字节（NetIn 的体），按序排队
    inbound: VecDeque<Vec<u8>>,
    /// 读端已关闭（NetEof）
    eof: bool,
    /// 读端有新数据时的唤醒（写端在主循环线程）
    read_waker: Option<Waker>,
    /// 待发出的线上字节（将由主循环封装成 NetOut）
    outbound: Vec<u8>,
}

/// 单端共享句柄。`PipeStream` 本体被 IronRDP/TLS 持有；主循环持 `PipeHandle`
/// 从另一侧推入/取走字节。
#[derive(Clone)]
pub struct PipeHandle {
    shared: Arc<Mutex<Shared>>,
    /// 有待发出字节时点亮（poll_write 之后）。主循环在 select! 里等它，
    /// 而不是定时轮询——输入往返对延迟敏感，20ms 的轮询间隔会原样加进
    /// 每一次击键的传播时延里。
    notify: Arc<Notify>,
}

impl PipeHandle {
    /// 线上字节到达（`ToHelper::NetIn` 的体）。
    pub fn push_net(&self, bytes: Vec<u8>) {
        let mut s = self.shared.lock().unwrap();
        s.inbound.push_back(bytes);
        if let Some(w) = s.read_waker.take() {
            w.wake();
        }
    }

    /// 传输终结（`ToHelper::NetEof`）。此后读端表现为 EOF。
    pub fn close(&self) {
        let mut s = self.shared.lock().unwrap();
        s.eof = true;
        if let Some(w) = s.read_waker.take() {
            w.wake();
        }
    }

    /// 取走待发出的字节（主循环封装成 `FromHelper::NetOut`）。
    pub fn drain_out(&self) -> Vec<u8> {
        std::mem::take(&mut self.shared.lock().unwrap().outbound)
    }

    /// 等待「有待发出字节」信号（select! 用）。`notify_one` 的许可语义：
    /// 写发生在等之前也能立刻醒。
    pub async fn wait_outbound(&self) {
        self.notify.notified().await;
    }
}

/// 建一对（流, 句柄）。
pub fn pipe() -> (PipeStream, PipeHandle) {
    let shared = Arc::new(Mutex::new(Shared::default()));
    let notify = Arc::new(Notify::new());
    (
        PipeStream {
            shared: shared.clone(),
            notify: notify.clone(),
        },
        PipeHandle { shared, notify },
    )
}

/// 给 IronRDP/TLS 看的「流」。
pub struct PipeStream {
    shared: Arc<Mutex<Shared>>,
    notify: Arc<Notify>,
}

impl AsyncRead for PipeStream {
    fn poll_read(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let mut s = self.shared.lock().unwrap();
        match s.inbound.pop_front() {
            Some(chunk) => {
                let n = chunk.len().min(buf.remaining());
                buf.put_slice(&chunk[..n]);
                if n < chunk.len() {
                    // 残余放回队首——下一次 poll_read 继续。ReadBuf 满而块更大
                    // 时必须这样切，否则字节丢失。
                    s.inbound.push_front(chunk[n..].to_vec());
                }
                Poll::Ready(Ok(()))
            }
            None if s.eof => Poll::Ready(Ok(())), // EOF：0 字节即对端关闭
            None => {
                // 没数据也没 EOF：挂起，等 push_net/close 唤醒。
                // 已注册的旧 waker 直接覆盖——同一线程串行 poll，语义等价。
                s.read_waker = Some(_cx.waker().clone());
                Poll::Pending
            }
        }
    }
}

impl AsyncWrite for PipeStream {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        if buf.is_empty() {
            return Poll::Ready(Ok(0));
        }
        self.shared.lock().unwrap().outbound.extend_from_slice(buf);
        // 通知主循环来取。notify_one 存许可：主循环此刻不在等也不丢信号。
        self.notify.notify_one();
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(())) // 缓冲由主循环的 drain_out 决定何时上行，无独立 flush 语义
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一写一读：字节原样穿过，顺序不变。
    #[tokio::test]
    async fn bytes_round_trip_in_order() {
        let (mut stream, handle) = pipe();
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        stream.write_all(b"hello").await.unwrap();
        stream.write_all(b" world").await.unwrap();
        let drained = handle.drain_out();
        assert_eq!(drained, b"hello world");

        // 把排出的字节当 NetIn 灌回去，再从流里读出来
        handle.push_net(drained);
        let mut buf = vec![0u8; 64];
        let n = stream.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"hello world");
    }

    /// 无数据时读挂起；push_net 之后被唤醒。
    #[tokio::test]
    async fn read_parks_until_data_arrives() {
        let (mut stream, handle) = pipe();
        use tokio::io::AsyncReadExt;

        let mut buf = vec![0u8; 8];
        let read = stream.read(&mut buf);
        tokio::pin!(read);

        // 还没数据：应该 Pending
        tokio::select! {
            r = &mut read => panic!("不该有数据可读：{r:?}"),
            _ = tokio::time::sleep(std::time::Duration::from_millis(20)) => {}
        }

        handle.push_net(b"abc".to_vec());
        let n = read.await.unwrap();
        assert_eq!(n, 3);
        assert_eq!(&buf[..3], b"abc");
    }

    /// close 之后读返回 0（EOF），不是永远挂起。
    #[tokio::test]
    async fn close_yields_eof_not_eternal_parking() {
        let (mut stream, handle) = pipe();
        use tokio::io::AsyncReadExt;
        handle.close();
        let mut buf = [0u8; 8];
        let n = stream.read(&mut buf).await.unwrap();
        assert_eq!(n, 0, "close 后应读到 EOF（0 字节）");
    }

    /// **写出即通知**：poll_write 之后 wait_outbound 必须立刻就绪（notify_one 的
    /// 许可语义）——这是主循环 select! 那条路径的唤醒保证，静默丢失就是
    /// 「线上字节卡在 helper 里出不去」的死法。
    #[tokio::test]
    async fn writing_signals_the_drain_side_immediately() {
        let (mut stream, handle) = pipe();
        use tokio::io::AsyncWriteExt;
        // 未写过：50ms 内不该被唤醒
        tokio::select! {
            _ = handle.wait_outbound() => panic!("没写过字节就被唤醒了"),
            _ = tokio::time::sleep(std::time::Duration::from_millis(50)) => {}
        }
        // 写一笔：wait_outbound 立刻完成（此前没有等待者，靠的是许可）
        stream.write_all(b"xyz").await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(1), handle.wait_outbound())
            .await
            .expect("poll_write 之后 wait_outbound 必须立刻就绪（许可语义）");
        assert_eq!(handle.drain_out(), b"xyz");
    }

    /// 比读缓冲更大的块要被切开：先填满，残余留在队首等下一次。
    #[tokio::test]
    async fn an_oversized_chunk_is_split_not_truncated() {
        let (mut stream, handle) = pipe();
        use tokio::io::AsyncReadExt;

        handle.push_net(vec![7u8; 10]);
        let mut buf = [0u8; 4];
        let n = stream.read(&mut buf).await.unwrap();
        assert_eq!(n, 4);
        assert_eq!(&buf, &[7, 7, 7, 7]);

        let mut rest = vec![0u8; 8];
        let n2 = stream.read(&mut rest).await.unwrap();
        assert_eq!(n2, 6, "剩余 6 字节应完整可读，实得 {n2}");
    }
}
