//! 终端编码在**整条管道**上的行为（M7.4）。
//!
//! `decode.rs` 的单测只证明解码器本身对；这里证明它真的接在了管道里、接在了**正确的位置**，
//! 而且换编码不需要重连——出口标准的原话是「GBK 输出正确显示，且**编码切换不需要重连**」。
//!
//! 三路 fan-out 都要看到解码后的字节：渲染（用户看的）、网格（vt100 → AI 取文）、
//! 环形缓冲（诊断快照）。少接一路的表现是「屏幕上是中文、AI 读到的是乱码」这种极难自证的错位。

use fs_terminal::decode::StreamDecoder;
use fs_terminal::pipe::{ByteTap, PipeOpts};
use fs_terminal::SessionPipe;
use std::sync::Arc;
use tokio::io::duplex;

/// GBK 的「中文」：D6 D0 CE C4。
const ZHONGWEN_GBK: &[u8] = &[0xD6, 0xD0, 0xCE, 0xC4];

fn opts(decoder: StreamDecoder) -> PipeOpts {
    PipeOpts {
        grid_rows: 24,
        grid_cols: 80,
        scrollback_lines: fs_terminal::grid::DEFAULT_SCROLLBACK_LINES,
        flow: Default::default(),
        ring_bytes: 4096,
        session_log: None,
        record: None,
        tap: None,
        decoder,
    }
}

/// 收齐渲染队列里的字节，直到超时为止。
async fn drain(rx: &mut tokio::sync::mpsc::Receiver<(u64, Vec<u8>)>, ms: u64) -> Vec<u8> {
    let mut out = Vec::new();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(ms);
    while let Ok(Some((_seq, f))) = tokio::time::timeout_at(deadline, rx.recv()).await {
        out.extend_from_slice(&f);
    }
    out
}

#[tokio::test]
async fn gbk_bytes_reach_the_terminal_as_utf8() {
    let (mut w, r) = duplex(1024);
    let mut pipe = SessionPipe::spawn(r, opts(StreamDecoder::new(Some("gbk")).unwrap()));
    let mut rx = pipe.render_rx();
    tokio::io::AsyncWriteExt::write_all(&mut w, ZHONGWEN_GBK)
        .await
        .unwrap();
    let got = drain(&mut rx, 300).await;
    assert_eq!(String::from_utf8(got).unwrap(), "中文");
    // 网格（AI 取文面）与环形缓冲（诊断面）也必须是解码后的
    assert!(pipe.grid_text().contains("中文"), "网格里不是解码后的字节");
    assert!(
        pipe.ring_snapshot_text().contains("中文"),
        "环形缓冲里不是解码后的字节"
    );
    pipe.shutdown();
}

/// 一次 read 恰好切在双字节字符中间——这是长输出里每几 KiB 就会发生一次的常态。
#[tokio::test]
async fn split_multibyte_across_reads_is_not_corrupted() {
    let (mut w, r) = duplex(1024);
    let mut pipe = SessionPipe::spawn(r, opts(StreamDecoder::new(Some("gbk")).unwrap()));
    let mut rx = pipe.render_rx();
    tokio::io::AsyncWriteExt::write_all(&mut w, &ZHONGWEN_GBK[..1])
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(60)).await;
    tokio::io::AsyncWriteExt::write_all(&mut w, &ZHONGWEN_GBK[1..])
        .await
        .unwrap();
    let got = drain(&mut rx, 300).await;
    assert_eq!(String::from_utf8(got).unwrap(), "中文");
    pipe.shutdown();
}

/// 出口标准：**编码切换不需要重连**。同一个 pipe、同一个会话，切完下一块就按新编码解。
#[tokio::test]
async fn encoding_switch_takes_effect_without_reconnect() {
    let (mut w, r) = duplex(1024);
    let mut pipe = SessionPipe::spawn(r, opts(StreamDecoder::default()));
    let mut rx = pipe.render_rx();
    assert_eq!(pipe.encoding(), "UTF-8");

    // 先按 UTF-8 收一段 UTF-8 正文（此时 GBK 字节会是乱码，正是用户遇到的场景）
    tokio::io::AsyncWriteExt::write_all(&mut w, "abc".as_bytes())
        .await
        .unwrap();
    assert_eq!(String::from_utf8(drain(&mut rx, 200).await).unwrap(), "abc");

    // 用户在设置里改成 GBK——不重连
    pipe.set_encoding(Some("gbk")).unwrap();
    assert_eq!(pipe.encoding(), "GBK");
    tokio::io::AsyncWriteExt::write_all(&mut w, ZHONGWEN_GBK)
        .await
        .unwrap();
    assert_eq!(
        String::from_utf8(drain(&mut rx, 300).await).unwrap(),
        "中文"
    );

    // 再切回 UTF-8 同样即时生效
    pipe.set_encoding(Some("utf-8")).unwrap();
    assert_eq!(pipe.encoding(), "UTF-8");
    tokio::io::AsyncWriteExt::write_all(&mut w, "中".as_bytes())
        .await
        .unwrap();
    assert_eq!(String::from_utf8(drain(&mut rx, 300).await).unwrap(), "中");
    pipe.shutdown();
}

/// 认不出的标签报错，且**不改变当前编码**——半途改成一个用户没要求的编码比报错更糟。
#[tokio::test]
async fn bad_label_is_rejected_and_leaves_encoding_untouched() {
    let (_w, r) = duplex(64);
    let pipe = SessionPipe::spawn(r, opts(StreamDecoder::new(Some("gbk")).unwrap()));
    let e = pipe.set_encoding(Some("no-such-encoding")).unwrap_err();
    assert!(e.contains("no-such-encoding"));
    assert_eq!(pipe.encoding(), "GBK");
    pipe.shutdown();
}

/// UTF-8 档必须与「没有解码器那会儿」逐字节一致：终端流里混着二进制是常态，
/// 而 U+FFFD 替换不可逆。
#[tokio::test]
async fn utf8_path_passes_arbitrary_bytes_through_unchanged() {
    let (mut w, r) = duplex(1024);
    let mut pipe = SessionPipe::spawn(r, opts(StreamDecoder::default()));
    let mut rx = pipe.render_rx();
    let raw: &[u8] = &[0x41, 0xff, 0xfe, 0x80, 0x42];
    tokio::io::AsyncWriteExt::write_all(&mut w, raw)
        .await
        .unwrap();
    assert_eq!(drain(&mut rx, 300).await, raw);
    pipe.shutdown();
}

/// 吞掉一切的拦截器 + GBK 解码器：解码站在拦截**之后**，所以解码器根本不该看到这些字节。
///
/// 次序反了的表现是 ZMODEM 传输坏掉——GBK 解码器会把帧头里的非法字节换成 U+FFFD，
/// 拦截器再也认不出帧。这条测试从「吞掉的字节一个都不出现」这一侧钉住次序。
struct SwallowAll;
impl ByteTap for SwallowAll {
    fn filter<'a>(&self, _chunk: &'a [u8]) -> std::borrow::Cow<'a, [u8]> {
        std::borrow::Cow::Borrowed(&[])
    }
}

#[tokio::test]
async fn decoder_sits_after_the_tap() {
    let (mut w, r) = duplex(1024);
    let mut o = opts(StreamDecoder::new(Some("gbk")).unwrap());
    o.tap = Some(Arc::new(SwallowAll) as Arc<dyn ByteTap>);
    let mut pipe = SessionPipe::spawn(r, o);
    let mut rx = pipe.render_rx();
    tokio::io::AsyncWriteExt::write_all(&mut w, ZHONGWEN_GBK)
        .await
        .unwrap();
    assert!(
        drain(&mut rx, 200).await.is_empty(),
        "被吞的字节不该经解码器冒出来"
    );
    assert_eq!(pipe.grid_text().trim(), "");
    pipe.shutdown();
}
