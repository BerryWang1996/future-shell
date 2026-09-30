//! Task 16：会话管道编排回归例（spec §2.2）。
//!
//! 钉住的契约点：fan-out 三路各自可达且字节一致（grid 剥离 ANSI 供
//! AI 语义通道 / ring 原样录制供调试快照 / 渲染帧合批保序；S221：ring
//! 例 payload 用非 ASCII + ANSI 判别跨线变异）、双水位背压的读侧紧界为
//! 暂停点水位 ∈ [high, high + 一读块]（S184；S223：检查先于 read，写侧
//! 另加 duplex 缓冲——写入量 = pipe 读取量 + duplex 滞留量）、low-water
//! 释放后恢复读取全量续传（S178）、EOF / 读错 / 用户 shutdown 都必须关闭
//! 渲染流（下游 `recv()` 得 `None`，不得挂死；S179 读错与 EOF 同路关闭）、
//! 关闭的次序握手——合批收尾由读取任务 stop 臂发起（S214，含背压等待点
//! stop 臂）、shutdown/Drop 协同停止：已读字节必入 fan-out、关闭后写入
//! 不得入 grid/ring 且须收 BrokenPipe（S180/S181；S218）、未 shutdown 的
//! drop 亦触发关闭传播（S176）、Drop 运行时无关（S219）、100 KiB 大块跨
//! 16 KiB 读缓冲逐字节重组（S177；S217 增 16 KiB 级块 fan-out ① grid /
//! ② ring 腿保真）、`resize` / `ack` 委托接线承重、`render_rx` 单消费者
//! 仅取一次（二次取 panic，S186）。
//!
//! 断言纪律（S52）：一切断言钉**内容与状态**（逐字节相等 / 水位值 /
//! `None`），不钉类型系统已保证的性质。

use fs_terminal::flow::FlowConfig;
use fs_terminal::pipe::{PipeOpts, SessionPipe};
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::io::{duplex, AsyncRead, AsyncWriteExt, ReadBuf};

fn opts() -> PipeOpts {
    PipeOpts {
        grid_rows: 24,
        grid_cols: 80,
        scrollback_lines: fs_terminal::grid::DEFAULT_SCROLLBACK_LINES,
        flow: FlowConfig::default(),
        ring_bytes: 4096,
        session_log: None,
        tap: None,
        decoder: Default::default(),
        record: None,
    }
}

#[tokio::test]
async fn bytes_flow_source_to_render_and_grid() {
    let (mut writer, reader) = duplex(64 * 1024);
    let mut pipe = SessionPipe::spawn(reader, opts());
    let mut rx = pipe.render_rx();
    let payload: &[u8] = b"hello \x1b[31mred\x1b[0m\r\n";

    writer.write_all(payload).await.unwrap();
    let frame = tokio::time::timeout(Duration::from_secs(1), rx.recv())
        .await
        .expect("1 s 内首帧必须到达")
        .expect("渲染流不得提前关闭")
        .1;
    assert_eq!(
        frame, payload,
        "单次写入须合为单帧、逐字节保序（ANSI 原样保留——渲染帧 destined for xterm.js）"
    );

    tokio::time::sleep(Duration::from_millis(20)).await;
    let text = pipe.grid_text();
    assert!(
        text.contains("hello red"),
        "grid 须显示可见文本，实得 {text:?}"
    );
    assert!(
        !text.contains("\x1b["),
        "grid 须剥离 ANSI 控制序列（AI 语义通道不得吃控制码），实得 {text:?}"
    );
}

#[tokio::test]
async fn backpressure_pauses_source_read() {
    let (mut writer, reader) = duplex(64 * 1024);
    let mut o = opts();
    o.flow.queue_bytes_high = 8 * 1024; // 调低 high-water 便于触发
    o.flow.queue_bytes_low = 4 * 1024; // 双水位不变量：low < high
    let mut pipe = SessionPipe::spawn(reader, o);
    let _rx = pipe.render_rx(); // 取走但不 ack、不消费 → 水位只升不降

    // 持续灌入 1 MiB：背压生效时 pipe 停止读取，duplex 写侧最终阻塞；
    // 无背压时 500 ms 足够把 1 MiB 全部读走。
    let write_task = tokio::spawn(async move {
        let mut total = 0;
        let chunk = vec![b'z'; 64 * 1024];
        loop {
            tokio::select! {
                r = writer.write_all(&chunk) => { if r.is_err() { break total; } total += chunk.len(); }
                _ = tokio::time::sleep(Duration::from_millis(500)) => break total,
            }
        }
    });
    let written = write_task.await.unwrap();
    // 背压把 pipe 端读取量（暂停点水位）钉在 [high, high + 一读块]（S184）；
    // 写侧量级 = 读取量 + duplex 缓冲(64K)。写入以整 64K 块计数，落点必为 64K
    //（首块必成、次块被阻），放宽上界容噪声。
    assert!(
        (64 * 1024..=256 * 1024).contains(&written),
        "背压须把写入量限制在 [64 KiB, 256 KiB]，实得 {written}"
    );
    // S184：暂停点水位钉——背压检查先于每次 read，故锁存水位 ∈ [high, high + 一读块]。
    // 变异体「删除暂停循环」会把 duplex 全量读走并放行全部写入，上界断言与此断言双出界。
    let q = pipe.queue_bytes();
    assert!(
        (8 * 1024..=8 * 1024 + 16 * 1024).contains(&q),
        "S184：暂停点水位须在 [high, high + 一读块]，实得 {q}"
    );
}

#[tokio::test]
async fn eof_closes_render_stream() {
    let (writer, reader) = duplex(1024);
    let mut pipe = SessionPipe::spawn(reader, opts());
    let mut rx = pipe.render_rx();
    drop(writer); // EOF → 读取任务通知合批退出 → render_tx drop
    assert!(
        tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("EOF 后渲染流必须关闭（recv() 得 None），不得挂死")
            .is_none(),
        "EOF 后渲染流须断开"
    );
}

#[tokio::test]
async fn shutdown_flushes_tail_and_closes() {
    let (mut writer, reader) = duplex(64 * 1024);
    let mut pipe = SessionPipe::spawn(reader, opts());
    let mut rx = pipe.render_rx();
    writer.write_all(b"tail-bytes").await.unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await; // 等字节入管并合帧（tick 16 ms）
    pipe.shutdown(); // 用户主动关闭（app 层 session_close 入口）
    let mut got = Vec::new();
    while let Some((_, frame)) = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("shutdown 后渲染流必须关闭（timeout 即未关闭 → 挂死）")
    {
        got.extend_from_slice(&frame);
    }
    assert_eq!(got, b"tail-bytes", "末帧不得丢字节、恰发一次");
}

/// S221（第二裁判，low——判别增强）：payload 用非 ASCII + ANSI——ring 视图
/// 保留 ANSI 而 grid 视图（已剥离）取不同值，跨线变异「ring_snapshot_text
/// 读 grid.screen_text」在此例红；反向跨线由例 1 `!contains("\x1b[")` 捕获
///（不对称判别，注 30 F9）。
#[tokio::test]
async fn fanout_ring_receives_same_bytes() {
    let (mut writer, reader) = duplex(64 * 1024);
    let mut pipe = SessionPipe::spawn(reader, opts());
    let mut rx = pipe.render_rx();
    writer
        .write_all("ring-测-\x1b[31mRED\x1b[0m".as_bytes())
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await; // 等 fan-out 完成
    pipe.shutdown();
    while tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("渲染流必须关闭")
        .is_some()
    {}
    assert_eq!(
        pipe.ring_snapshot_text(),
        "ring-测-\x1b[31mRED\x1b[0m",
        "ring 须原样录到 source 字节（fan-out ② 可达且字节一致，ANSI 保留；跨线变异读 grid 剥 ANSI 视图则红）"
    );
}

#[tokio::test]
async fn resize_is_forwarded_to_grid() {
    let (mut writer, reader) = duplex(64 * 1024);
    let mut pipe = SessionPipe::spawn(reader, opts());
    let _rx = pipe.render_rx();
    pipe.resize(2, 80); // 须转发到内部 grid（24 行 → 2 行）
    writer.write_all(b"l1\r\nl2\r\nl3\r\nl4\r\n").await.unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await;
    let text = pipe.grid_text();
    assert!(text.contains("l4"), "2 行屏须显示最新行，实得 {text:?}");
    assert!(
        !text.contains("l1"),
        "2 行屏须已滚出 l1（resize 未转发则会留在 24 行屏上），实得 {text:?}"
    );
    let back = pipe.scrollback_text(100);
    assert!(
        back.contains("l1"),
        "滚出的行须落入回看（证明是真滚屏而非丢字），实得 {back:?}"
    );
}

#[tokio::test]
async fn ack_drains_queue_watermark() {
    let (mut writer, reader) = duplex(64 * 1024);
    let mut pipe = SessionPipe::spawn(reader, opts());
    let _rx = pipe.render_rx();
    writer.write_all(&[b'w'; 1000]).await.unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(
        pipe.queue_bytes() >= 1000,
        "未 ack 字节须计入水位，实得 {}",
        pipe.queue_bytes()
    );
    pipe.ack(1000);
    assert_eq!(pipe.queue_bytes(), 0, "ack 须把字节水位排至 0");
}

/// S176：未调 shutdown 的 drop 同样必须触发关闭传播——旧实现无 Drop 守卫：
/// 读取任务继续读、合批任务永持 render_tx，下游 recv() 永久挂死。
/// 变异体「删 Drop impl」在此红（recv 超时）；writer 刻意存活到最后，
/// 确保变异体无 EOF 救援。
#[tokio::test]
async fn drop_without_shutdown_closes_render_stream() {
    let (mut writer, reader) = duplex(64 * 1024);
    let mut pipe = SessionPipe::spawn(reader, opts());
    let mut rx = pipe.render_rx();
    writer.write_all(b"pre-drop").await.unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await; // 等字节入管
    drop(pipe); // 不显式 shutdown——Drop 守卫须触发关闭传播
    let mut got = Vec::new();
    while let Some((_, frame)) = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("Drop 后渲染流必须关闭（S176 Drop 守卫），timeout 即挂死")
    {
        got.extend_from_slice(&frame);
    }
    assert_eq!(got, b"pre-drop", "Drop 前已入管字节须经收尾末帧送达");
    drop(writer); // 断言完成后才放行 writer（防变异体借 EOF 假绿）
}

/// S180/S181：shutdown 走协同停止——shutdown 前已读字节恰送达一次，
/// shutdown 后写入不得进入 grid/ring（读取任务必须已停）。
/// 变异体「删 shutdown 的 stop.notify_one」在此红（reader 存活、AFTER 入 grid）。
#[tokio::test]
async fn shutdown_stops_fanout_for_later_writes() {
    let (mut writer, reader) = duplex(64 * 1024);
    let mut pipe = SessionPipe::spawn(reader, opts());
    let mut rx = pipe.render_rx();
    writer.write_all(b"before-close").await.unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await; // 等 fan-out 完成
    pipe.shutdown();
    // 短暂等待令 reader 在 stop 臂退出（消除 select 双就绪竞态）：
    // reader 已退 → 读半 drop → 写侧收 BrokenPipe（这恰是协同停止的证据）；
    // 变异体若令 reader 存活，写入将成功、AFTER 入 grid/ring，在下方断言红。
    tokio::time::sleep(Duration::from_millis(10)).await;
    let r = writer.write_all(b"AFTER-close").await;
    assert!(
        r.is_err(),
        "S181：shutdown 后读取任务必须已退出（写入应收 BrokenPipe）"
    );
    tokio::time::sleep(Duration::from_millis(30)).await;
    let mut got = Vec::new();
    while let Some((_, frame)) = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("shutdown 后渲染流必须关闭")
    {
        got.extend_from_slice(&frame);
    }
    assert_eq!(got, b"before-close", "shutdown 前字节须恰送达一次");
    let text = pipe.grid_text();
    assert!(
        text.contains("before-close"),
        "grid 须有 shutdown 前内容：{text:?}"
    );
    assert!(
        !text.contains("AFTER"),
        "S180：shutdown 后写入不得入 grid（reader 须已停）：{text:?}"
    );
    let ring = pipe.ring_snapshot_text();
    assert!(
        !ring.contains("AFTER"),
        "S180：shutdown 后写入不得入 ring：{ring:?}"
    );
}

/// S178：背压释放后读取必须恢复——duplex 里剩余字节须全量续传。
/// 变异体「背压即退出（while→if return）」在此红（只送达首读块，总量缺口）。
#[tokio::test]
async fn backpressure_release_resumes_delivery() {
    let (mut writer, reader) = duplex(64 * 1024);
    let mut o = opts();
    o.flow.queue_bytes_high = 8 * 1024;
    o.flow.queue_bytes_low = 4 * 1024;
    let mut pipe = SessionPipe::spawn(reader, o);
    let mut rx = pipe.render_rx();
    writer.write_all(&vec![b'x'; 64 * 1024]).await.unwrap(); // 全量进 duplex 缓冲，不阻塞
    tokio::time::sleep(Duration::from_millis(100)).await; // reader 读走首块后锁存背压
    assert!(
        pipe.queue_bytes() >= 8 * 1024,
        "背压须已触发（字节 ≥ high-water），实得 {}",
        pipe.queue_bytes()
    );
    // S295 迁移注：原为 `pipe.ack(queue_bytes())`——一次性按字节把水位打到 0。
    // 序号口径下没有「凭空排水」这回事：确认必须指向真实收到的帧。故删去这行
    // 预排，改由下面的收取循环逐帧确认——首帧一被确认背压即释放，读取恢复。
    // 这反而更贴近生产链路（渲染泵收一帧、xterm 渲一帧、回一个 ack）。
    let mut total = 0usize;
    loop {
        match tokio::time::timeout(Duration::from_secs(2), rx.recv()).await {
            Ok(Some((seq, frame))) => {
                total += frame.len();
                pipe.ack(seq); // 边收边排水（S295：回 seq），避免续传中途再锁存
                if total >= 64 * 1024 {
                    break;
                }
            }
            Ok(None) => break,
            Err(_) => break,
        }
    }
    assert_eq!(
        total,
        64 * 1024,
        "S178：背压释放后读取须恢复，64 KiB 全量送达，实得 {total}"
    );
}

/// 滞留检测（Task 56 的运行时修复本体）：前端 ack 停发（水位不降）越过
/// `FlowConfig::stall_timeout` 后，读任务须丢弃积压、复位水位、恢复读取。
/// 变异体「删滞留检测（永久停读）」「删 drop_backlog」在此红：越过滞留时长后水位
/// 仍 ≥ high 而非 0。用短 stall_timeout + 真实等待避免等 30 s。
#[tokio::test]
async fn stall_detector_drops_backlog_and_resumes_reading() {
    let (mut writer, reader) = duplex(64 * 1024);
    let mut o = opts();
    o.flow.queue_bytes_high = 8 * 1024;
    o.flow.queue_bytes_low = 4 * 1024;
    o.flow.stall_timeout = Duration::from_millis(50);
    let mut pipe = SessionPipe::spawn(reader, o);
    let _rx = pipe.render_rx(); // 取走但不 ack → 水位只升不降

    writer.write_all(&vec![b'z'; 16 * 1024]).await.unwrap();
    // 让读任务首读一整块（16 KiB）后进入背压等待
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(
        pipe.queue_bytes() >= 8 * 1024,
        "前置：背压须已触发（字节 ≥ high-water），实得 {}",
        pipe.queue_bytes()
    );

    // 越过 50 ms 滞留阈值：滞留检测须 drop_backlog + 恢复读取（源已空、水位复位后保持 0）
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        pipe.queue_bytes(),
        0,
        "滞留超时须丢弃积压、复位水位（源已空、恢复读后水位保持 0）"
    );
}

/// S179 读错注入源：前 N 字节正常返回，随后每次 read 都 Err。
struct ErrAfter(usize);

impl AsyncRead for ErrAfter {
    fn poll_read(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        if this.0 == 0 {
            return Poll::Ready(Err(std::io::Error::other("S179 注入读错")));
        }
        let n = buf.remaining().min(this.0);
        buf.put_slice(&vec![b'e'; n]);
        this.0 -= n;
        Poll::Ready(Ok(()))
    }
}

/// S179：读错与 EOF 同路关闭渲染流——不得循环重试、不得挂死。
/// 变异体「Err 臂改 continue」在此红（死循环永不关闭，timeout）。
#[tokio::test]
async fn read_error_closes_render_stream() {
    let mut pipe = SessionPipe::spawn(ErrAfter(100), opts());
    let mut rx = pipe.render_rx();
    let mut got = Vec::new();
    while let Some((_, frame)) = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("S179：读错后渲染流必须关闭，不得挂死/死循环重试")
    {
        got.extend_from_slice(&frame);
    }
    assert_eq!(got, vec![b'e'; 100], "读错前已读字节须经收尾末帧送达");
}

/// S177：100 KiB 大块须跨 16 KiB 读缓冲分 ≥7 次读取、逐字节重组。
/// 变异体「腐化 >8 KiB 读块内容（计数不变）」在此红——唯此例对大读块
/// 做逐字节内容比对（截断类变异体会同时红背压续传例，不满足恰一红纪律）。
#[tokio::test]
async fn large_payload_reassembles_across_reads() {
    let (mut writer, reader) = duplex(256 * 1024);
    let mut pipe = SessionPipe::spawn(reader, opts());
    let mut rx = pipe.render_rx();
    let payload: Vec<u8> = (0..100 * 1024).map(|i| (i % 251) as u8).collect();
    writer.write_all(&payload).await.unwrap();
    let mut got = Vec::new();
    while got.len() < payload.len() {
        let frame = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("S177：100 KiB 跨读重组须全量送达（timeout 即中断）")
            .expect("流不得提前关闭（writer 存活，无 EOF）");
        pipe.ack(frame.0);
        got.extend_from_slice(&frame.1);
    }
    assert_eq!(got, payload, "跨读重组须逐字节一致（100 KiB = ≥7 次读取）");
}

/// S186：头注「render_rx 仅取一次」由本例钉住——二次取必须 panic（文案恰符）。
/// 变异体「截短 expect 文案」在此红（should_panic(expected) 失配）。
#[tokio::test]
#[should_panic(expected = "render_rx 只能取一次")]
async fn render_rx_second_take_panics() {
    let (_writer, reader) = duplex(1024);
    let mut pipe = SessionPipe::spawn(reader, opts());
    let _ = pipe.render_rx();
    let _ = pipe.render_rx();
}

/// S214（第二裁判，med）：**关闭的次序握手——背压锁存下 shutdown 亦须由读取任务
/// stop 臂发起合批收尾**。旧形由 SessionPipe 同发 stop 与 batcher.shutdown（且背压
/// 等待点 stop 臂只退不收），违反 flow.rs push 前置（读取先退出、合批后收尾）：
/// 多线程运行时下最后一次 fan-out 与收尾可交错，迟到推送落孤儿 pending（grid/ring
/// 有账而渲染独失 + queue_bytes 幽灵水位）。整改后两 stop 臂（主 select + 背压等待
/// 点）皆 `batcher_c.shutdown(); return;`，与 EOF 臂对称。本钉锁等待点臂：背压锁存
///（不 ack、水位 ≥ high）+ shutdown → 关闭必经背压等待点 stop 臂传播（ack 释放后
/// 主 select 臂永不达——此形状是握手在 current_thread 唯一可钉投影）。变异体
/// M-BPSTOP-NOBS（背压 stop 臂删合批收尾）恰红 1 例（渲染流永不关闭，timeout）。
/// 竞态窗口本体为多线程交错形，current_thread 不可确定性复现，S87 类诚实记录——
/// 裁判探针 conc_2 / flow_1 为其见证（红态留档）。
#[tokio::test]
async fn shutdown_during_backpressure_close_is_reader_initiated() {
    let (mut writer, reader) = duplex(64 * 1024);
    let mut o = opts();
    o.flow.queue_bytes_high = 8 * 1024;
    o.flow.queue_bytes_low = 4 * 1024;
    let mut pipe = SessionPipe::spawn(reader, o);
    let mut rx = pipe.render_rx(); // 取走但不 ack、不消费 → 水位只升不降
    writer.write_all(&vec![b'p'; 64 * 1024]).await.unwrap(); // 全量进 duplex
    tokio::time::sleep(Duration::from_millis(100)).await; // reader 读走首块后锁存背压
    assert!(
        pipe.queue_bytes() >= 8 * 1024,
        "背压须已锁存（水位 ≥ high），实得 {}",
        pipe.queue_bytes()
    );
    pipe.shutdown(); // 锁存中关闭：reader 停泊于背压等待点 stop 臂
    let mut got = 0usize;
    while let Some((_, frame)) = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("S214：背压锁存下 stop 臂亦须发起合批收尾；timeout 即无收尾者")
    {
        got += frame.len();
    }
    assert!(got > 0, "关闭前已读字节须经收尾帧送达，实得 {got}");
    drop(writer); // 断言完成后才放行 writer（防变异体借 EOF 假绿）
}

/// S217（第二裁判，low）：**16 KiB 级大读块的 fan-out ① grid / ② ring 腿保真**——
/// 旧 13 例对 >4 KiB 读块无 grid/ring 腿内容断言（S177 仅渲染腿），尺寸条件截断
/// 变异（`min(len, 4096)` 形，注 30 F5）遂全存活。本钉 payload 恰 16 KiB（一读块
/// 上界）：grid 腿钉尾标可见（screen_text 含 TAILMARK）、ring 腿钉长度相等 + 尾标
/// 匹配、渲染腿钉总量相等。变异体 M-GRID-TRUNC4K / M-RING-TRUNC4K（尺寸条件截断
/// `len > 4096 → [..4096]`）各恰红 1 例。
#[tokio::test]
async fn large_chunk_fanout_fidelity() {
    let (mut writer, reader) = duplex(64 * 1024);
    let mut o = opts();
    o.ring_bytes = 32 * 1024; // 容下 16 KiB 免环覆
    let mut pipe = SessionPipe::spawn(reader, o);
    let mut rx = pipe.render_rx();
    let mut payload = vec![b'x'; 16 * 1024 - 12];
    payload.extend_from_slice(b"TAILMARK-END"); // 恰 16 KiB = 一读块上界
    writer.write_all(&payload).await.unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await; // 等 fan-out 完成
    pipe.shutdown();
    let mut got = Vec::new();
    while let Some((_, frame)) = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("渲染流必须关闭")
    {
        got.extend_from_slice(&frame);
    }
    assert_eq!(got.len(), payload.len(), "渲染腿总量须相等");
    assert!(
        pipe.grid_text().contains("TAILMARK"),
        "grid 腿须保真到尾标（截断变异体丢尾标）"
    );
    let ring = pipe.ring_snapshot_text();
    assert_eq!(
        ring.len(),
        payload.len(),
        "ring 腿长度须相等（全 ASCII 无掩码）"
    );
    assert!(
        ring.ends_with("TAILMARK-END"),
        "ring 腿须保真到尾标（截断变异体丢尾标）"
    );
}

/// S218（第二裁判，low）：**Drop 协同停止的读取面**——drop(pipe) 后写入必须收到
/// BrokenPipe（读取任务必须已停），且 drop 前字节经收尾末帧恰送达一次。旧 13 例
/// 无 Drop 后写入断言：变异体「Drop 改直接 batcher_in.shutdown」（渲染流能关、
/// reader 存活）在 S176 例绿（该例只钉关闭传播）、在此例红——判别对偶。变异体
/// M-DROP-NOBREAK 恰红 1 例。
#[tokio::test]
async fn drop_stops_reader_for_later_writes() {
    let (mut writer, reader) = duplex(64 * 1024);
    let mut pipe = SessionPipe::spawn(reader, opts());
    let mut rx = pipe.render_rx();
    writer.write_all(b"pre-drop-stop").await.unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await; // 等 fan-out 完成
    drop(pipe); // Drop 守卫：通知 stop，读取任务 stop 臂发起合批收尾
    tokio::time::sleep(Duration::from_millis(10)).await; // 令 reader 在 stop 臂退出
    let r = writer.write_all(b"AFTER-drop").await;
    assert!(
        r.is_err(),
        "S218：drop 后读取任务必须已退出（写入应收 BrokenPipe）；变异体 Drop 直发合批收尾则 reader 存活、写入成功"
    );
    let mut got = Vec::new();
    while let Some((_, frame)) = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("Drop 后渲染流必须关闭")
    {
        got.extend_from_slice(&frame);
    }
    assert_eq!(got, b"pre-drop-stop", "drop 前字节须经收尾末帧恰送达一次");
}

/// S219（第二裁判，low）：**Drop 运行时无关**——pipe 可 outlive 其出生运行时
///（裁判探针例 4：drop(rt) 先于 drop(pipe)，join().is_ok() 见证）；变异体「Drop 内
/// spawn/block_on」无运行时上下文必 panic。正形 Drop 仅原子 notify
///（`Notify::notify_one` 运行时无关），本钉锁此边界。变异体 M-DROP-SPAWN（Drop
/// notify 前插 tokio::spawn）在此红（运行时外 panic）、余例绿（运行时内）。
#[test]
fn drop_after_runtime_gone_does_not_panic() {
    let pipe = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("运行时构造必须成功")
        .block_on(async {
            let (mut writer, reader) = duplex(1024);
            writer.write_all(b"rt-gone").await.unwrap();
            let mut pipe = SessionPipe::spawn(reader, opts());
            let _rx = pipe.render_rx();
            pipe
        });
    // 运行时已 drop（block_on 表达式结束即毁）——pipe outlive 其出生运行时；
    // Drop 不得 panic（内部不得有运行时依赖操作）。
    drop(pipe);
}
