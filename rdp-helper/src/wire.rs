//! stdio 上的帧读写。
//!
//! 泛型于 `AsyncRead`/`AsyncWrite` 而不是写死 stdin/stdout：那样这一层可以用
//! 内存管道（`tokio::io::duplex`）离线测出来，不必起进程。
//! 进程边界那一半由主程序侧的子进程测试承担。
//!
//! # stdout 是**协议通道**，不是日志通道
//!
//! 往 stdout 写任何一个字节的非协议内容（一句 `println!` 调试、一个 panic 消息）
//! 都会把帧流冲垮，而表现是主程序那侧一个莫名其妙的 `BadHeader`——离真因很远。
//! 所以：**日志一律走 stderr**（`tracing` 的 writer 在 `main.rs` 里钉到 stderr）。

use fs_rdpproto::{decode, encode, CodecError, HasBody, Packet};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// 读取失败的原因。
#[derive(Debug)]
pub enum ReadError {
    /// 对端关闭且缓冲区里没有半条消息——**正常终止**。
    Eof,
    /// 对端关闭时缓冲区里还剩半条消息。
    ///
    /// 与 [`ReadError::Eof`] 分开：前者是「说完了」，后者是「话说到一半线断了」。
    /// 两者对调用方的意义不同——正常终止该安静退出，截断该留痕。
    Truncated {
        pending: usize,
    },
    Io(std::io::Error),
    Codec(CodecError),
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Eof => write!(f, "对端已关闭"),
            Self::Truncated { pending } => {
                write!(f, "对端在一条消息中途关闭，缓冲区尚余 {pending} 字节")
            }
            Self::Io(e) => write!(f, "读失败：{e}"),
            Self::Codec(e) => write!(f, "解码失败：{e}"),
        }
    }
}

/// 单次读取块大小。**只允许出现在堆上**——见 [`FramedReader::next`] 的文档。
const READ_CHUNK: usize = 64 * 1024;

/// 帧读取器。
pub struct FramedReader<R> {
    inner: R,
    buf: Vec<u8>,
}

impl<R: AsyncRead + Unpin> FramedReader<R> {
    pub fn new(inner: R) -> Self {
        Self {
            inner,
            buf: Vec::with_capacity(READ_CHUNK),
        }
    }

    /// 读出下一条完整消息。
    ///
    /// # 先解再读
    ///
    /// 每一轮**先**尝试从已有缓冲区里解一条，解不出来才去读。顺序反过来
    /// （每轮必读一次）会让粘包的第二条消息一直卡到下一次网络活动才被处理——
    /// 而在 RDP 这条路上，「下一次活动」可能要等用户动一下鼠标。
    ///
    /// # 读取必须用 `read_buf`（读进 Vec 的空闲容量），不能用栈上中转数组
    ///
    /// 这里曾经是 `let mut chunk = [0u8; 64 * 1024]`——与 app 侧
    /// `FrameReader::next` 曾经的那行逐字相同，也是 2026-08-27 整程序崩溃
    /// 的同一根因：跨 await 存活的 64 KiB 栈数组把 future 撑到 65,584 字节，
    /// 经 `rt.block_on(run())` 常驻 helper 主线程（1 MiB 栈、无 /STACK 抬高，
    /// `cargo:rustc-link-arg-bins` 也够不到这个独立工作区）。实测
    /// `FramedReader::next` 的 future 达 65,584 B、`run()` 达 66,160 B——
    /// 主循环每加一个带缓冲的分支就朝栈上限近一步。
    ///
    /// `read_buf` 只在**真正读到字节时**才把它们追加进 buf（spare capacity），
    /// 于是同时消掉了三类问题：栈数组（future 回到几十字节）、零填充不变量
    /// （根本没有 resize 零这回事）、以及取消安全（future 在 await 点被 drop
    /// 不会留下任何中间态——tokio 文档明示 `read_buf` 取消安全）。
    pub async fn next<H>(&mut self) -> Result<Packet<H>, ReadError>
    where
        H: for<'de> Deserialize<'de> + HasBody,
    {
        loop {
            match decode::<H>(&self.buf) {
                Ok(Some((pkt, used))) => {
                    // `drain` 而不是 `split_off`：后者会分配一个新 Vec 并把剩余
                    // 字节整体搬过去，在粘包高频时是一次白白的拷贝加分配。
                    self.buf.drain(..used);
                    return Ok(pkt);
                }
                Ok(None) => {}
                Err(e) => return Err(ReadError::Codec(e)),
            }
            self.buf.reserve(READ_CHUNK);
            let n = self
                .inner
                .read_buf(&mut self.buf)
                .await
                .map_err(ReadError::Io)?;
            if n == 0 {
                return Err(if self.buf.is_empty() {
                    ReadError::Eof
                } else {
                    ReadError::Truncated {
                        pending: self.buf.len(),
                    }
                });
            }
        }
    }
}

/// 帧写入器。
pub struct FramedWriter<W> {
    inner: W,
    scratch: Vec<u8>,
}

impl<W: AsyncWrite + Unpin> FramedWriter<W> {
    pub fn new(inner: W) -> Self {
        Self {
            inner,
            scratch: Vec::with_capacity(64 * 1024),
        }
    }

    /// 写一条消息并 flush。
    ///
    /// # 为什么每条都 flush
    ///
    /// 不 flush 的话，一条 `CertPresented`（几百字节）会躺在缓冲区里等下一条
    /// 消息来把它挤出去——而 helper 此刻正**在等主程序的裁决**，下一条消息
    /// 永远不会来。那是一个教科书式的死锁，且现场看起来像「连接卡住了」。
    ///
    /// 帧本身通常几十 KB 起步，多一次 flush 的开销可以忽略。
    pub async fn send<H>(&mut self, header: &H, body: &[u8]) -> std::io::Result<()>
    where
        H: Serialize + HasBody,
    {
        self.scratch.clear();
        encode(header, body, &mut self.scratch);
        self.inner.write_all(&self.scratch).await?;
        self.inner.flush().await
    }
}
