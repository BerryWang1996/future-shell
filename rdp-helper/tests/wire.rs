//! 帧读写层。用 `tokio::io::duplex` 在内存里对接，不起进程。
//!
//! 进程边界那一半由主程序侧的子进程测试承担（同 `crates/itest/tests/mcp_client.rs`
//! 的分工：协议在内存里测得细，进程边界另有专门的用例）。

// 被测模块是二进制 crate 的私有模块，集成测试够不到它。
// `#[path]` 直接把源文件拉进来编第二份——这是 Rust 里测 bin crate 内部模块的
// 常规做法。代价是编译两次，而这个文件只有一百多行。
#[path = "../src/wire.rs"]
mod wire;

use fs_rdpproto::{FromHelper, PixelFormat, Rect, ToHelper, PROTOCOL_VERSION};
use wire::{FramedReader, FramedWriter, ReadError};

/// 一条消息写进去、读出来，逐字相等。
#[tokio::test]
async fn a_message_survives_the_round_trip() {
    let (a, b) = tokio::io::duplex(64 * 1024);
    let mut tx = FramedWriter::new(a);
    let mut rx = FramedReader::new(b);

    let msg = ToHelper::Hello {
        version: PROTOCOL_VERSION,
    };
    tx.send(&msg, &[]).await.unwrap();
    let pkt = rx.next::<ToHelper>().await.unwrap();
    assert_eq!(pkt.header, msg);
}

/// 带体的消息：像素逐字节原样穿过。
#[tokio::test]
async fn a_frame_body_survives_byte_for_byte() {
    let (a, b) = tokio::io::duplex(1024 * 1024);
    let mut tx = FramedWriter::new(a);
    let mut rx = FramedReader::new(b);

    // 32×32 的 RGBA，用一个不重复的模式填——全 0 或全同值的载荷
    // 会让「错位一个字节」这类 bug 完全看不出来。
    let body: Vec<u8> = (0..32u32 * 32 * 4).map(|i| (i % 251) as u8).collect();
    let msg = FromHelper::Frame {
        rect: Rect {
            x: 7,
            y: 9,
            width: 32,
            height: 32,
        },
        format: PixelFormat::Rgba,
    };
    tx.send(&msg, &body).await.unwrap();
    let pkt = rx.next::<FromHelper>().await.unwrap();
    assert_eq!(pkt.header, msg);
    assert_eq!(pkt.body, body, "帧体被改动了");
}

/// **粘包**：一次写三条，读三次，一条不落、顺序不乱。
///
/// duplex 会把三次 `write_all` 合并到接收端的一次 `read` 里，
/// 所以这条测试真的在走「先解再读」的那条路径。
#[tokio::test]
async fn three_messages_written_back_to_back_are_read_in_order() {
    let (a, b) = tokio::io::duplex(64 * 1024);
    let mut tx = FramedWriter::new(a);
    let mut rx = FramedReader::new(b);

    tx.send(&ToHelper::NetIn, &[1, 2, 3]).await.unwrap();
    tx.send(&ToHelper::NetEof, &[]).await.unwrap();
    tx.send(&ToHelper::Shutdown, &[]).await.unwrap();

    // **每一次读都套超时。** 「先读再解」写反的实现在这里不是读出错的数据，
    // 而是**挂住**——最后一条消息已经完整躺在缓冲区里，它却还要先去读一个
    // 永远不会来的字节。没有超时的话这条测试会挂死而不是转红，
    // 而挂死的测试在 CI 上表现为「跑不完」，没人能一眼看出是哪条判据破了。
    async fn one(rx: &mut FramedReader<tokio::io::DuplexStream>) -> ToHelper {
        tokio::time::timeout(std::time::Duration::from_secs(5), rx.next::<ToHelper>())
            .await
            .expect("读挂住了——缓冲区里已有完整消息却还在等新字节（先读再解写反了）")
            .unwrap()
            .header
    }

    let p1 = tokio::time::timeout(std::time::Duration::from_secs(5), rx.next::<ToHelper>())
        .await
        .expect("第一条就挂住了")
        .unwrap();
    assert_eq!(p1.header, ToHelper::NetIn);
    assert_eq!(p1.body, vec![1, 2, 3]);
    assert_eq!(one(&mut rx).await, ToHelper::NetEof);
    assert_eq!(one(&mut rx).await, ToHelper::Shutdown);
}

/// **分片**：一条大消息被拆成很多次小写入，仍然读得出来。
///
/// duplex 的缓冲区故意开得比消息小，逼它分多次到达。
#[tokio::test]
async fn a_message_larger_than_the_pipe_buffer_is_reassembled() {
    // 管道缓冲 4 KiB，消息体 64 KiB —— 必然分片。
    let (a, b) = tokio::io::duplex(4096);
    let body: Vec<u8> = (0..65536u32).map(|i| (i % 253) as u8).collect();
    let expect = body.clone();

    let writer = tokio::spawn(async move {
        let mut tx = FramedWriter::new(a);
        tx.send(&ToHelper::NetIn, &body).await.unwrap();
    });

    let mut rx = FramedReader::new(b);
    let pkt = rx.next::<ToHelper>().await.unwrap();
    assert_eq!(pkt.header, ToHelper::NetIn);
    assert_eq!(pkt.body.len(), expect.len());
    assert_eq!(pkt.body, expect, "分片重组后内容不符");
    writer.await.unwrap();
}

/// 对端干净关闭 ⇒ `Eof`，不是错。
#[tokio::test]
async fn a_clean_close_is_eof_not_an_error() {
    let (a, b) = tokio::io::duplex(1024);
    drop(a);
    let mut rx = FramedReader::new(b);
    assert!(
        matches!(rx.next::<ToHelper>().await, Err(ReadError::Eof)),
        "干净关闭该是 Eof"
    );
}

/// **对端在一条消息中途关闭 ⇒ `Truncated`，与 `Eof` 分开。**
///
/// 两者对调用方的意义完全不同：`Eof` 是「说完了」，安静退出；
/// `Truncated` 是「话说到一半线断了」，要留痕。合成一个的话，
/// 一次真的崩溃会被当成正常退出咽下去。
#[tokio::test]
async fn a_close_mid_message_is_truncated_not_eof() {
    let (mut a, b) = tokio::io::duplex(64 * 1024);
    // 手工写半条：前缀声明 100 字节的头，只给 10 字节。
    use tokio::io::AsyncWriteExt;
    a.write_all(&100u32.to_be_bytes()).await.unwrap();
    a.write_all(&0u32.to_be_bytes()).await.unwrap();
    a.write_all(&[b'x'; 10]).await.unwrap();
    a.flush().await.unwrap();
    drop(a);

    let mut rx = FramedReader::new(b);
    match rx.next::<ToHelper>().await {
        Err(ReadError::Truncated { pending }) => {
            assert_eq!(pending, 18, "残留字节数不对（8 前缀 + 10 头）");
        }
        other => panic!("该是 Truncated，实得 {other:?}"),
    }
}

/// 坏字节流 ⇒ `Codec` 错，不 panic、不挂起。
#[tokio::test]
async fn a_garbage_stream_is_a_codec_error() {
    let (mut a, b) = tokio::io::duplex(64 * 1024);
    use tokio::io::AsyncWriteExt;
    // 声明一个超上限的头长度：解码器必须在**只有前缀**时就拒，
    // 而不是回 Ok(None) 去等 4 GB（那会让这条测试挂死，而挂死正是判据）。
    a.write_all(&u32::MAX.to_be_bytes()).await.unwrap();
    a.write_all(&0u32.to_be_bytes()).await.unwrap();
    a.flush().await.unwrap();

    let mut rx = FramedReader::new(b);
    let got = tokio::time::timeout(std::time::Duration::from_secs(5), rx.next::<ToHelper>()).await;
    let got = got.expect("解码器挂住了——上限没有在分配之前检查");
    assert!(
        matches!(got, Err(ReadError::Codec(_))),
        "该是 Codec 错，实得 {got:?}"
    );
}

/// **每条消息都要 flush。**
///
/// 不 flush 的话，一条 `CertPresented` 会躺在缓冲区里等下一条消息把它挤出去
/// ——而 helper 此刻正在等主程序的裁决，下一条永远不会来。教科书式死锁。
///
/// # 这条测试的第一版是**无法失败的判据**
///
/// 原来直接把 `duplex` 交给 `FramedWriter`。而 `duplex` 根本不缓冲——
/// `write_all` 一返回字节就已经在对端了，`flush()` 是空操作。于是变异
/// 「把 flush 删掉」照样全绿：这条判据当时证明不了任何东西。
///
/// 生产里 inner 是 `tokio::io::stdout()`，它被 `LineWriter` 包着，
/// 而二进制帧几乎不会以换行结尾——**那里的 flush 是真必需的**。
/// 所以测试也必须垫一层真会缓冲的 writer，否则测的就不是生产的那条路径。
#[tokio::test]
async fn a_single_message_is_flushed_without_a_second_one_to_push_it() {
    let (a, b) = tokio::io::duplex(64 * 1024);
    // 缓冲区开得比消息大：不 flush 就一个字节也出不去。
    let mut tx = FramedWriter::new(tokio::io::BufWriter::with_capacity(64 * 1024, a));
    let mut rx = FramedReader::new(b);

    tx.send(
        &FromHelper::CertPresented {
            fingerprint: "ab:cd".into(),
            subject: "CN=win-01".into(),
            issuer: "CN=win-01".into(),
            not_after: "2030-01-01T00:00:00Z".into(),
        },
        &[],
    )
    .await
    .unwrap();

    let got = tokio::time::timeout(std::time::Duration::from_secs(5), rx.next::<FromHelper>())
        .await
        .expect("没有第二条消息来推，第一条就出不去——说明没 flush");
    assert!(matches!(
        got.unwrap().header,
        FromHelper::CertPresented { .. }
    ));
}
