//! 会话管道编排（spec §2.2：fan-out 三路 + 双水位背压读取 + EOF/shutdown 关闭传播）。
//!
//! 单一读取任务把 source 字节按序 fan-out：① headless 网格（剥 ANSI 供 AI 语义通道）
//! → ② 环形缓冲（原样录制供调试快照）→ ③ 渲染队列（合批 emit、帧保序）。
//! `backpressure_active()` 为真时暂停从 source 读取（russh channel window 耗尽即传压到
//! TCP）。关闭传播四入口：EOF、读错（S179 与 EOF 同臂）、用户 shutdown、未显式
//! shutdown 的 drop（S176 守卫），一律走合批收尾路径——收尾模式正常发末帧后退出，
//! 消费者停摆且满时丢帧退出（flow.rs 收尾分支，S87），render_tx 随任务退出 drop，
//! 下游 `recv()` 得 `None`（EOF 泵仅自退、不代发事件；`session:closed` 仅 app 层
//! user_closed / watchdog remote_exit 两来源，Task 19 定案，S220）。
//! 用户 shutdown/Drop 走协同停止信号（S180）：读取任务在 stop 臂退出并发起合批收尾
//! （S214 次序握手：读取先退出、合批后收尾，收尾与最后一次推送保持 happens-after），
//! 已读字节必然完成 fan-out、未读字节随退出弃置——旧 `abort` 直杀在「read 返回→push」
//! 窗口有丢已读尾块的竞态，已废（S181 abort 面随之移除）。
use crate::flow::{Batcher, BatcherIn, FlowConfig};
use crate::grid::Grid;
use crate::ring::RingBuffer;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::sync::mpsc;

/// 毒锁恢复（审计4）：grid/ring 的 std Mutex 仅在持锁线程 panic 时染毒——vt100 在
/// `grid.feed` 内 panic 会染毒 grid 锁。读取任务内部已用 `into_inner` 恢复（S183），
/// 但 `grid_rows`/`grid_cols`/`grid_text`/`resize`/`ring_snapshot_text` 等 accessor 若仍
/// `.unwrap()`，重连路径读旧 pipe 的 grid 尺寸会撞毒锁 panic、重连静默死亡（审计4
/// CONFIRMED）。故 accessor 统一走此恢复：`into_inner` 取回内部状态继续，活性优先于带病状态。
fn recover_poison<T>(r: Result<T, std::sync::PoisonError<T>>) -> T {
    r.unwrap_or_else(|p| p.into_inner())
}

/// 会话管道构造参数。
///
/// 运行时阈值全部在 `flow` 内，且由 `Batcher::new` 消毒（周期钳 [1 ms, 1 h]、
/// 水位/帧上限 ≥ 1、low ≤ high）。
pub struct PipeOpts {
    /// 网格初始行数（转 `Grid::new`，u16 极端值由其内部钳位）。
    pub grid_rows: u16,
    /// 网格初始列数（同上）。
    pub grid_cols: u16,
    /// 滚动回看行数（审计2 #35：档案 `term.scrollback_lines` 贯通至此，旧实现恒用
    /// `DEFAULT_SCROLLBACK_LINES`）。转 `Grid::new`，极端值由其内部钳位。
    pub scrollback_lines: usize,
    /// 合批与双水位背压配置（见 `FlowConfig` 各字段语义）。
    pub flow: FlowConfig,
    /// 环形缓冲字节容量（录制窗口大小，满则环覆旧字节）。
    pub ring_bytes: usize,
    /// 会话纯文本日志（M4a）：`Some` 即开启转录。**先于一切丢弃/合批**接在
    /// fan-out 首位——背压丢帧（drop_backlog）与合批不该让转录缺字节：屏幕上
    /// 可以丢一帧重绘，日志里丢一段命令输出是证据缺口。
    ///
    /// 写入恒不阻塞读取任务（`SessionLog::write` 只剥离+缓冲），落盘由本任务在
    /// 每次 fan-out 后 flush；失败置闩后静默丢弃（见 sessionlog 模块头 ②）。
    pub session_log: Option<crate::sessionlog::SessionLog>,
    /// 会话录屏（M4a，asciicast v2）：接在**拦截器之后、网格之前**——录的是
    /// 「用户看到的字节」，ZMODEM 传输期的二进制帧不进录屏（进了回放就是乱码），
    /// 这与 sessionlog「录链路上发生的一切」是刻意相反的两个口径。
    /// 常驻挂载、按需启停（内部 active 标志），idle 时零开销。
    pub record: Option<std::sync::Arc<std::sync::Mutex<crate::record::Recorder>>>,
    /// 字节拦截器（M4a ZMODEM）：接在**日志之后、网格之前**，可改写送往终端的字节。
    ///
    /// 位置是有讲究的：
    /// - 在日志**之后**——转录要如实记录链路上发生过什么，包括一次 rz/sz 的原始帧。
    /// - 在网格与渲染**之前**——传输期间的字节是二进制帧，喂给 vt100 会把屏幕
    ///   刷成乱码，还可能被其中的转义序列改掉终端状态（换字符集、动光标），
    ///   传完之后屏幕就废了。这条是 ZMODEM 必须做拦截而非旁观的根本原因。
    ///
    /// 为 `None` 时整条链路零开销（连 `Cow` 都不构造）。
    pub tap: Option<Arc<dyn ByteTap>>,
    /// 终端编码解码器（M7.4）：接在**拦截器之后、其余 fan-out 之前**。
    ///
    /// 位置与拦截器的关系是硬约束：解码必须在拦截**之后**。反过来的话 GBK 解码器会去啃
    /// ZMODEM 的二进制帧，把帧头里的字节换成 U+FFFD，拦截器再也认不出帧——传输直接坏掉。
    ///
    /// 会话日志（⓪）刻意留在解码**之前**（即记原始字节）：它与拦截器的关系已经定死了
    /// 「记录链路上发生的一切」，那就该是链路上真实流过的字节，而不是我们转写过的版本。
    ///
    /// 默认值是 UTF-8 恒等透传，一个字节都不碰（见 `decode::StreamDecoder` 头注）。
    pub decoder: crate::decode::StreamDecoder,
}

/// 终端字节拦截器：可吞掉/改写送往终端的字节。
///
/// 刻意是**同步**接口。拦截判定必须与字节到达同步完成——「先放过去、稍后异步
/// 决定要不要吞」在终端里没有撤回操作，乱码一旦渲染就在屏幕上了。实现方若需要
/// 做 IO（落盘、回写 SSH），应把副作用投递到队列由别处执行，而不是在这里阻塞
/// 读取任务。
pub trait ByteTap: Send + Sync {
    /// 返回仍应送往终端（网格 + 环形缓冲 + 渲染）的字节。
    ///
    /// 常态返回 `Cow::Borrowed(chunk)`（零拷贝原样透传）；传输期间返回
    /// `Cow::Borrowed(&[])` 吞掉；传输刚结束的那一块可能返回 `Cow::Owned`
    /// （帧尾之后属于 shell 的残留字节）。
    fn filter<'a>(&self, chunk: &'a [u8]) -> std::borrow::Cow<'a, [u8]>;
}

/// 读取任务 panic 兜底守卫：任务任何退出路径（含 panic）都关闭渲染流。
///
/// 正常路径已显式调 `shutdown`（幂等，本守卫再调无害）；此守卫只补 **panic** 路径——
/// 否则读取任务在 `grid.feed`（vt100 上游状态机非本层可穷举证明的面，grid.rs S98）等处
/// panic 时静默死亡、合批任务仍持 render_tx、渲染流永不关闭、终端永久冻结且无断线
/// banner（审计3）。Drop 内仅 `BatcherIn::shutdown`（原子存 + notify，不 panic、不依赖
/// 运行时，可安全地在解卷栈上执行）。
struct ReadTaskGuard {
    batcher: BatcherIn,
}

impl Drop for ReadTaskGuard {
    fn drop(&mut self) {
        self.batcher.shutdown();
    }
}

/// 会话管道：fan-out 三路 + 背压读取 + 关闭传播（spec §2.2）。
///
/// 关闭有两入口、共享同一信号路径：显式 `shutdown`（app 层 `session_close`）与
/// `Drop` 守卫（S176）——后者保证未显式关闭的 drop 也不泄漏读取/合批任务、
/// 不悬空渲染流（旧实现无 Drop 守卫：直接 drop 则读取任务继续读、合批任务
/// 永持 render_tx，下游 `recv()` 永久挂死）。
pub struct SessionPipe {
    /// 录屏器（resize 时要通知；读取任务里也用它写事件）
    record: Option<std::sync::Arc<std::sync::Mutex<crate::record::Recorder>>>,
    grid: Arc<Mutex<Grid>>,
    ring: Arc<Mutex<RingBuffer>>,
    batcher_in: BatcherIn,
    // S214：不再持有 BatcherHandle——合批收尾由读取任务的 stop 臂发起（次序握手：
    // 读取先退出、合批后收尾），SessionPipe/Drop 仅通知停止；句柄于构造处即弃
    //（`let (batcher_in, _)`），JoinHandle 之 drop 仅 detach、任务不受影响。
    stop: Arc<tokio::sync::Notify>, // 用户关闭协同停止信号（S180）：读取任务侧 select 臂
    render_rx: Option<mpsc::Receiver<(u64, Vec<u8>)>>,
    /// 与读取任务共享的解码器（M7.4）。放在这里就是为了**运行时换编码不重连**：
    /// 换掉锁里的那个对象，下一块字节就按新编码解——会话、滚动缓冲、正在跑的程序全不受影响。
    decoder: Arc<Mutex<crate::decode::StreamDecoder>>,
}

impl SessionPipe {
    /// 构造即 spawn：读取任务 + 合批任务（spec §2.2 分裂句柄式）。
    ///
    /// `source` 的读须可取消（S180：shutdown/Drop 协同停止臂会取消在途
    /// `read` await——未读字节随任务退出弃置，已读字节必然已完成 fan-out，
    /// 见 `shutdown`）。生产 source 有二形：Task 19 的 mpsc 桥接
    /// ChannelReader（可取消性在桥接 recv），与 Task 24 perf.rs 的直读形
    ///（`SessionPipe::spawn(channel.into_stream(), …)`，可取消性在底层读取）；
    /// 二者皆须满足本前置（注 30 F10：旧文「duplex / russh channel 读取均可
    /// 取消」枚举不全且与桥接形相矛盾）。
    pub fn spawn<S: AsyncRead + Unpin + Send + 'static>(mut source: S, opts: PipeOpts) -> Self {
        // record 先于 async move 取走（Self 结构体留一份；任务里另 clone 一份）
        let record_for_self = opts.record.clone();
        let grid = Arc::new(Mutex::new(Grid::new(
            opts.grid_rows,
            opts.grid_cols,
            opts.scrollback_lines,
        )));
        let ring = Arc::new(Mutex::new(RingBuffer::new(opts.ring_bytes)));
        let (render_tx, render_rx) = mpsc::channel::<(u64, Vec<u8>)>(64);
        let (batcher_in, _) = Batcher::new(opts.flow, render_tx);

        let decoder = Arc::new(Mutex::new(opts.decoder));
        let decoder_c = decoder.clone();
        let grid_c = grid.clone();
        let ring_c = ring.clone();
        let batcher_c = batcher_in.clone();
        let stop = Arc::new(tokio::sync::Notify::new());
        let stop_c = stop.clone();
        tokio::spawn(async move {
            // panic 兜底：任务解卷/正常返回都经此守卫关闭渲染流（见 ReadTaskGuard）。
            // 置于首个局部量之前；解卷时局部量逆序 drop、被捕获的参数（source）最后 drop，
            // 即 source 在守卫之后 drop——但次序无关：合批收尾（守卫）与桥接终结
            // （source drop）两信号彼此独立，都会发出（审计4 订正：旧注「source 先 drop」误）。
            let _guard = ReadTaskGuard {
                batcher: batcher_c.clone(),
            };
            let mut session_log = opts.session_log;
            let record = opts.record.clone();
            let tap = opts.tap;
            // 拦截器返回 Owned 时的落脚处：借用要跨越下面三路 fan-out，所以不能
            // 是临时值。延迟初始化——只有传输收尾那一两块会走到这条路径。
            let mut tap_owned: Vec<u8>;
            // 解码产物的落脚处（同 tap_owned：借用要跨越三路 fan-out）。UTF-8 档不走这里。
            let mut dec_owned: Vec<u8>;
            let mut buf = [0u8; 16 * 1024];
            // 背压诊断（审计2 交叉对抗）：终端「几十秒后卡死」需定位是否卡在背压排水。
            // 仅在状态翻转时各打一条（激活记水位、解除记水位），不逐循环刷屏。
            let mut bp_was = false;
            loop {
                // 水位背压：超 high-water 暂停读取（russh channel window 耗尽 → TCP 传压）。
                // 释放在 low-water（滞回带见 flow 双水位不变量），避免临界振荡。
                // 暂停点水位 ∈ [high, high + 一读块]：检查先于 read（S184 钉例）。
                // S180：等待点带 stop 臂——背压中 shutdown 亦即时退出（纵深防御；
                // 该形状经 ack 释放后可被主 select 的 stop 臂兜住，单独不可钉，
                // 诚实记录同 S87 类）。S214：stop 臂发起合批收尾（与 EOF 臂对称）。
                let bp = batcher_c.backpressure_active();
                if bp != bp_was {
                    // 两维并列采样：背压可由字节（queue_bytes ≥ high）或帧数（frames ≥
                    // frames-high）单独触发，帧维锁存时字节水位可能很低；只记字节会把
                    // 「帧数卡在 16」的锁存误读成低水位，故一并记录帧数。
                    if bp {
                        tracing::warn!(
                            queue_bytes = batcher_c.queue_bytes(),
                            frames = batcher_c.frames_pending(),
                            head_frame_len = batcher_c.head_frame_len().unwrap_or(0),
                            ack_calls = batcher_c.ack_calls(),
                            "渲染队列超 high-water，暂停读取（背压激活）"
                        );
                    } else {
                        tracing::info!(
                            queue_bytes = batcher_c.queue_bytes(),
                            frames = batcher_c.frames_pending(),
                            "渲染队列排水完成，恢复读取（背压解除）"
                        );
                    }
                    bp_was = bp;
                }
                // 背压滞留检测：读任务停读期间 `queue_bytes` 单调非增（无 push、仅 ack
                // 可降）。水位持续不降即丢弃积压恢复读取，把永久锁存封顶为有界恢复
                // （见 drop_backlog：渲染泵/合批恒不阻塞，读任务是唯一停驻点，丢弃即
                // 恢复整条链路）。慢速排水仍降水位，不误触发。
                //
                // S301：**本处不得断言成因**。原文案写死「（前端 ack 停发）」，而
                // 2026-08-19 那次线上滞留里前端全程在正常 ack——真因是队首帧投递丢失
                // 导致的账本锁存（模块 flow.rs 头有完整因果链）。一句写死成因的日志把
                // 连续几轮对抗审计全带向前端，是这次定位耗时的直接原因。故此处只陈述
                // 观测事实，成因交由 `ack_calls`（前端是否还在 ack）与 `head_frame_len`
                // （欠账的那一笔多大）两个字段现场判别。
                let mut stall_start: Option<tokio::time::Instant> = None;
                let mut last_qb = batcher_c.queue_bytes();
                let stall_timeout = batcher_c.stall_timeout();
                let ack_calls_at_start = batcher_c.ack_calls();
                while batcher_c.backpressure_active() {
                    tokio::select! {
                        _ = stop_c.notified() => {
                            batcher_c.shutdown();
                            return;
                        }
                        _ = tokio::time::sleep(std::time::Duration::from_millis(8)) => {
                            let qb = batcher_c.queue_bytes();
                            if qb < last_qb {
                                // 有排水进展，复位滞留计时
                                last_qb = qb;
                                stall_start = None;
                            } else {
                                let start = *stall_start.get_or_insert_with(tokio::time::Instant::now);
                                if start.elapsed() >= stall_timeout {
                                    tracing::error!(
                                        queue_bytes = qb,
                                        frames = batcher_c.frames_pending(),
                                        head_frame_len =
                                            batcher_c.head_frame_len().unwrap_or(0),
                                        ack_calls = batcher_c.ack_calls(),
                                        ack_calls_during_stall =
                                            batcher_c.ack_calls() - ack_calls_at_start,
                                        stall_secs = stall_timeout.as_secs(),
                                        "渲染队列滞留未排水，丢弃积压恢复读取"
                                    );
                                    batcher_c.drop_backlog();
                                    break;
                                }
                            }
                        }
                    }
                }
                tokio::select! {
                    // S180：用户关闭协同停止——read 臂与 push 之间无 await 点，
                    // 已读字节必完成 fan-out、未读字节随退出弃置；旧 `abort` 直杀
                    // 在「read 返回→push」窗口可丢 ≤16 KiB 已读尾块，故改此臂
                    // （abort 路径随之移除，S181）。
                    // S214 次序握手：stop 臂发起合批收尾（与 EOF 臂对称）——读取
                    // 先退出、合批后收尾；旧形（SessionPipe 同发 stop 与
                    // batcher.shutdown）在多线程运行时下可让最后一次 fan-out 与收尾
                    // 交错，违反 flow.rs push 前置（迟到推送落孤儿 pending：grid/ring
                    // 有账而渲染独失 + queue_bytes 幽灵水位，注 30 F2）。
                    _ = stop_c.notified() => {
                        batcher_c.shutdown();
                        return;
                    }
                    r = source.read(&mut buf) => match r {
                        Ok(0) | Err(_) => {
                            // EOF/读错 → 传播关闭：合批进入收尾模式，正常发末帧后退出
                            //（消费者停摆且满时丢帧退出，flow.rs 收尾分支，S87），render_tx
                            // 随任务结束 drop → 下游 recv() 得 None（EOF 泵仅自退、不代发
                            // 事件；session:closed 仅 app 层 user_closed / watchdog
                            // remote_exit 两来源，Task 19 定案）。
                            // S179：Err 与 EOF 同臂——读错同样关闭渲染流，
                            // 不循环重试、不挂死。
                            batcher_c.shutdown();
                            return;
                        }
                        Ok(n) => {
                            let chunk = &buf[..n];
                            // fan-out 顺序（spec §2.2）：
                            // ⓪ 会话纯文本日志 tap（M4a）：**先于一切丢弃/合批**——
                            //    屏幕可以丢一帧重绘（背压 drop_backlog），日志丢一段
                            //    命令输出是证据缺口。写入只剥离+缓冲、恒不阻塞。
                            // [Phase 4b] 录屏 tap（全量字节+时序）将并列插在此处
                            if let Some(log) = session_log.as_mut() {
                                log.write(chunk);
                                // 每块即 flush：会话日志的价值在「出事时文件里已经有」，
                                // 攒着等 drop 会在崩溃/断电时丢掉最后一段——而那一段
                                // 恰是最需要的。8 KiB 缓冲 + 顺序追加，代价可接受。
                                log.flush();
                            }
                            // ⓪′ 字节拦截器（M4a ZMODEM）：日志之后、网格之前。
                            //    传输期间返回空切片吞掉二进制帧——否则 vt100 既刷
                            //    乱码又可能被帧里的转义字节改掉终端状态。
                            let chunk: &[u8] = match tap.as_ref() {
                                None => chunk,
                                Some(t) => match t.filter(chunk) {
                                    std::borrow::Cow::Borrowed(b) => b,
                                    std::borrow::Cow::Owned(v) => {
                                        // 借用期跨越下面三路 fan-out，需就地存活
                                        tap_owned = v;
                                        &tap_owned
                                    }
                                },
                            };
                            if chunk.is_empty() {
                                continue; // 全被吞掉：三路 fan-out 都无事可做
                            }
                            // ⓪‴ 编码解码（M7.4）：拦截器**之后**（见 PipeOpts::decoder 的
                            //    次序论证），其余 fan-out **之前**——网格（vt100/AI 取文）、
                            //    环形缓冲、录屏、渲染要看到的是同一份已经转成 UTF-8 的字节。
                            //    UTF-8 档在此零拷贝借用原块，与接这一段之前逐字节一致。
                            let decoded = {
                                let mut d = recover_poison(decoder_c.lock());
                                d.decode(chunk)
                            };
                            let chunk: &[u8] = match decoded {
                                std::borrow::Cow::Borrowed(b) => b,
                                std::borrow::Cow::Owned(v) => {
                                    dec_owned = v;
                                    &dec_owned
                                }
                            };
                            if chunk.is_empty() {
                                continue; // 整块都是半个多字符，留在解码器状态里等下一块
                            }
                            // ⓪″ 录屏 tap（M4a）：拦截器**之后**——录用户看到的字节。
                            // 时间在**此处**取而不是喂入时传进来：读块到达的时刻就是
                            // 事件时刻，调用方没有更早的时间戳可给。
                            if let Some(rec) = record.as_ref() {
                                let now_ms = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .map(|d| d.as_millis() as i64)
                                    .unwrap_or(0);
                                rec.lock()
                                    .unwrap_or_else(|p| p.into_inner())
                                    .write(chunk, now_ms);
                            }
                            // S183：毒锁恢复——std Mutex 仅在持锁线程 panic 时染毒；
                            // into_inner 取回内部状态继续（feed/push 为尽力面，活性优先：
                            // panic 上抛会静默杀死读取任务，渲染流永不关闭而挂死，
                            // 比带病状态更糟）。该恢复经 pub API 不可达（染毒需跨
                            // 线程同锁 panic），纵深防御无钉例，先例 S87 类。
                            let mut g = match grid_c.lock() {
                                Ok(g) => g,
                                Err(p) => p.into_inner(),
                            };
                            g.feed(chunk); // ① headless 网格
                            drop(g);
                            let mut rb = match ring_c.lock() {
                                Ok(r) => r,
                                Err(p) => p.into_inner(),
                            };
                            rb.push(chunk); // ② 环形缓冲（录制/调试）
                            drop(rb);
                            batcher_c.push(chunk); // ③ 渲染队列（合批 emit）
                        }
                    }
                }
            }
        });

        Self {
            record: record_for_self,
            grid,
            ring,
            batcher_in,
            stop,
            render_rx: Some(render_rx),
            decoder,
        }
    }

    /// 取出渲染帧接收端——单消费者，只能取一次（二次调用 panic，S186 钉例）。
    pub fn render_rx(&mut self) -> mpsc::Receiver<(u64, Vec<u8>)> {
        self.render_rx.take().expect("render_rx 只能取一次")
    }

    /// 前端 xterm.js write 回调回报**已渲染帧的 seq**（累计确认排水；
    /// 语义与丢帧免疫性见 `BatcherIn::ack`）。
    pub fn ack(&self, seq: u64) {
        self.batcher_in.ack(seq);
    }

    /// 当前屏可见文本（剥 ANSI，AI 语义通道；见 `Grid::screen_text`）。
    pub fn grid_text(&self) -> String {
        recover_poison(self.grid.lock()).screen_text()
    }

    /// 回看文本（至多 `max_lines` 行；见 `Grid::scrollback_text` 显示偏移口径）。
    pub fn scrollback_text(&self, max_lines: usize) -> String {
        recover_poison(self.grid.lock()).scrollback_text(max_lines)
    }

    /// 转发几何变化到内部 grid（外部 pty-req 重发回调在 app 层，Task 19）。
    pub fn resize(&self, rows: u16, cols: u16) {
        recover_poison(self.grid.lock()).resize(rows, cols);
        // 尺寸标记进录屏（回放器据此还原当时的屏幕尺寸）
        if let Some(rec) = self.record.as_ref() {
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            rec.lock()
                .unwrap_or_else(|p| p.into_inner())
                .resize(cols, rows, now_ms);
        }
    }

    /// 渲染队列字节水位采样（Task 24 吞吐验收用）。口径承 flow.rs
    /// `BatcherIn::queue_bytes` 的 pushed−acked 账目（含已发未 ack 字节与丢帧
    /// 幽灵水位，见该处例注；S216 交叉引用）。
    pub fn queue_bytes(&self) -> usize {
        self.batcher_in.queue_bytes()
    }

    /// 渲染队列**帧数**水位采样。
    ///
    /// 与 `queue_bytes` 并列而不是二选一：背压是**双维**的（`FlowConfig` 的
    /// `queue_bytes_high` 与 `queue_frames_high`），哪一维先到就先触发。帧维默认只有 16，
    /// 大帧场景下它会**远早于**字节维触发——只看字节水位会得出「背压从没起过作用」的
    /// 错误结论（itest `render_pipeline.rs` 的水位自检就在此翻过一次）。
    /// 背压累计触发次数（转发 [`crate::flow::BatcherIn::backpressure_episodes`]，诊断用）。
    pub fn backpressure_episodes(&self) -> u64 {
        self.batcher_in.backpressure_episodes()
    }

    pub fn frames_pending(&self) -> usize {
        self.batcher_in.frames_pending()
    }

    /// 环形缓冲快照（调试/录制视图）：尽力 UTF-8 文本——与 `RingBuffer::snapshot_text`
    /// 同口径，非法/残缺字节序列按 U+FFFD 掩码（S185：旧文案「原样 source 字节」
    /// 与有损口径对冲——此为观看面而非字节恢复面；全量录制 tap 见 Phase 4a）。
    pub fn ring_snapshot_text(&self) -> String {
        recover_poison(self.ring.lock()).snapshot_text()
    }

    /// 用户主动关闭（app 层 Task 19 `session_close` 入口）：通知读取任务协同停止
    /// 臂，由其发起合批收尾（S214 次序握手：读取先退出、合批后收尾——已读字节
    /// 完成 fan-out、未读弃置）；收尾模式正常发末帧后退出，消费者停摆且满时丢帧
    /// 退出（flow.rs S87），render_tx drop → 下游 `recv()` 得 `None`（泵仅自退、
    /// 不代发 `session:closed`，app 层自 user_closed 发之，Task 19 定案）。幂等。
    /// 当前终端编码的规范名（状态栏/设置页回显用）。
    pub fn encoding(&self) -> &'static str {
        recover_poison(self.decoder.lock()).name()
    }

    /// 运行时换编码。**不需要重连**（出口标准原文）：换掉共享解码器，下一块即生效。
    ///
    /// 认不出的标签原样报错，不静默回落——静默回落的表现是「我明明选了 GBK，还是乱码」。
    pub fn set_encoding(&self, label: Option<&str>) -> Result<(), String> {
        recover_poison(self.decoder.lock()).set_label(label)
    }

    pub fn shutdown(&self) {
        self.stop.notify_one();
    }
}

impl Drop for SessionPipe {
    /// S176：未显式 shutdown 的 drop 同样触发关闭传播——否则读取任务继续读
    /// source、合批任务永持 render_tx，下游 `recv()` 永久挂死（任务泄漏 +
    /// 悬挂渲染流）。与 `shutdown` 共享信号路径（S214：仅通知停止，合批收尾
    /// 由读取任务 stop 臂发起），幂等；Task 19 正常路径先调 `shutdown`，此为
    /// 错误路径/直接 drop 的兜底。运行时无关：`Notify::notify_one` 不依赖运行
    /// 时上下文，pipe 可 outlive 其出生运行时（S219 钉例；Drop 内禁 spawn/
    /// block_on）。
    fn drop(&mut self) {
        self.stop.notify_one();
    }
}

impl SessionPipe {
    pub fn grid_rows(&self) -> u16 {
        recover_poison(self.grid.lock()).rows()
    }
    pub fn grid_cols(&self) -> u16 {
        recover_poison(self.grid.lock()).cols()
    }
}
