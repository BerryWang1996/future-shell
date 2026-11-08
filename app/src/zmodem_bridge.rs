//! ZMODEM 终端内传输的应用层桥接（M4a：rz/sz）。
//!
//! 协议在 [`fs_terminal::zmodem`]，状态机在 [`fs_terminal::zsession`]，本模块只做
//! 三件外部世界的事：**从终端字节流里认出传输**、**把副作用送出去**（回写 SSH、
//! 落盘、发前端事件）、**在沙箱里定文件名**。
//!
//! ## 为什么拆成「同步拦截器 + 异步驱动」
//!
//! 拦截判定必须与字节到达同步完成：终端没有撤回操作，二进制帧一旦渲染，屏幕上
//! 的乱码就已经在那里了，还可能被帧里的转义字节改掉终端状态（换字符集、切备用
//! 屏），传完之后整块屏幕都是废的。所以 [`ZmodemTap::filter`] 必须当场决定吞或放。
//!
//! 但吞下去之后要做的事全是异步的（往 SSH 写字节要拿会话写锁）。于是状态机在
//! 拦截器里**同步**推进（纯字节逻辑，纳秒级），产出的副作用投进有界队列，由驱动
//! 任务串行执行。队列保序，所以「开文件 → 写数据 → 关文件」的次序天然正确。
//!
//! ## 沙箱路径在装配时定格
//!
//! 落盘根目录在**建会话时**解析一次存进拦截器，而不是每次传输回查设置。理由与
//! `LiveSession::target_endpoint` 同源：设置是可改的，回查会让同一条连接上前后
//! 两次传输落到不同的根目录下——用户找不到文件，而且这种漂移无法自证。
//!
//! ## 不做的事（不假装做了）
//!
//! - 不做断点续传：对端请求非零偏移时明确取消（见 zsession 的 ZRPOS 分支）。
//! - 不做多文件批量：一次会话只处理一个文件，第二个 ZFILE 会被当作新会话开始。
//! - 不自动决定覆盖：目标同名时改名为 `name (1).ext` 而不是覆盖或询问——传输
//!   是异步发生的，弹窗询问期间对端在等，超时会让整条传输失败得莫名其妙。

use crate::mcp::tasks::{
    Snapshot, Task, TaskBackend, TerminalMetadata, TerminalOutcome, TerminalUpdate,
};
use crate::zmodem_mcp_io::{DownloadSink, UploadSource};
use fs_terminal::pipe::ByteTap;
use fs_terminal::zmodem::detect_start;
use fs_terminal::zsession::{Action, Direction, ZmodemSession};
use std::borrow::Cow;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};
use tokio::sync::Notify;

/// 检测滑窗长度：帧头可能被 SSH 读块切开，需带上一块的尾巴一起嗅探。
///
/// 取 64 字节的依据：最长的帧头形态是十六进制头（`**\x18B` + 10 个 hex + 4 个 CRC
/// hex + CR LF XON ≈ 21 字节），64 有三倍余量，又小到不值得担心其代价。
const SNIFF_WINDOW: usize = 64;

/// 检出起始帧后该做什么。
///
/// 这个映射极易搞反，搞反的表现还很隐蔽（远端 `rz` 在等文件，我们却开了个接收
/// 会话，双方各等对方先说话，用户看到终端「卡住」直到 rz 超时）：
/// - `sz`（远端**发**）先发 **ZRQINIT** → 我们接收。
/// - `rz`（远端**收**）先发 **ZRINIT** → 我们发送，但发什么得先问人。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Detected {
    /// 立刻开始接收
    Receive,
    /// 远端在等文件：请前端弹「选择本地文件」，本端**不**进入传输态
    NeedLocalFile,
}

fn classify_start(kind: fs_terminal::zmodem::FrameKind) -> Option<Detected> {
    use fs_terminal::zmodem::FrameKind as K;
    match kind {
        K::ZRQINIT => Some(Detected::Receive),
        K::ZRINIT => Some(Detected::NeedLocalFile),
        // detect_start 只认这两种；其余不该走到这里，走到了也不猜
        _ => None,
    }
}

/// 副作用：拦截器（同步）产出，驱动任务（异步）消费。
enum Effect {
    /// 回写 SSH 通道
    Send(Vec<u8>),
    /// 取消控制字节：失败后仍须给对端一次退出机会。
    Cancel,
    CancelDone {
        message: String,
    },
    /// 开始接收：在沙箱里定名并建文件
    Begin {
        name: String,
        size: Option<u64>,
    },
    /// 追加落盘
    Write(Vec<u8>),
    /// 当前文件收完
    Finish {
        bytes: u64,
    },
    /// 索要待发文件在该偏移处的一块。
    ///
    /// 偏移必须带着走、不能假定顺序读：ZRPOS 是 ZMODEM 的重传机制，对端校验出错
    /// 就会要求**回退**到某个偏移重发。顺序读会从上次读到的地方继续，于是重发的
    /// 内容错位——落地文件长度正确、内容从错位点起全错，且不报任何错。
    NeedChunk {
        offset: u64,
    },
    /// 发送侧：当前文件被对端收妥。驱动任务据此出队下一个或 finish。
    FileSent,
    /// 会话终结
    Done {
        ok: bool,
        message: String,
    },
}

const EFFECT_BUDGET: std::time::Duration = std::time::Duration::from_secs(5);

fn effect_admitted(effect: &Effect, cancelling: bool, failed: bool) -> bool {
    if cancelling {
        matches!(effect, Effect::Cancel | Effect::CancelDone { .. })
    } else if failed {
        matches!(
            effect,
            Effect::Cancel | Effect::Done { .. } | Effect::CancelDone { .. }
        )
    } else {
        true
    }
}

fn effect_from_action(a: Action, cancelling: &AtomicBool) -> Effect {
    match a {
        Action::Send(b) if b == fs_terminal::zmodem::cancel_sequence() => Effect::Cancel,
        Action::Send(b) => Effect::Send(b),
        Action::BeginRecv(info) => Effect::Begin {
            name: info.name,
            size: info.size,
        },
        Action::Write(d) => Effect::Write(d),
        Action::FinishRecv { bytes } => Effect::Finish { bytes },
        Action::NeedChunk { offset } => Effect::NeedChunk { offset },
        Action::FileSent => Effect::FileSent,
        Action::Done { ok: false, message } if cancelling.load(Ordering::SeqCst) => {
            Effect::CancelDone { message }
        }
        Action::Done { ok, message } => Effect::Done { ok, message },
    }
}

struct QueuedEffect {
    transfer_id: u64,
    effect: Effect,
}

// 常见 1 KiB 包积压约 1 MiB 时先触及 1024 项；8 MiB 另限制非标准大包。
// 只预算排队载荷：不包括当前执行的一个效果和本次 feed 已产出的动作。
const MAX_QUEUED_EFFECTS: usize = 1024;
const MAX_QUEUED_BYTES: usize = 8 * 1024 * 1024;
// 留一半硬预算给已读的 16 KiB 及尚未解析完的单包；排到双低水位才恢复。
const READ_HIGH_EFFECTS: usize = 512;
const READ_LOW_EFFECTS: usize = 256;
const READ_HIGH_BYTES: usize = 4 * 1024 * 1024;
const READ_LOW_BYTES: usize = 2 * 1024 * 1024;
// 控制槽额外最多一个 CAN 和 4 KiB UTF-8 原因，不受普通队列挤占。
const MAX_CONTROL_MESSAGE_BYTES: usize = 4096;
const QUEUE_OVERFLOW: &str = "终端传输效果队列超过上限，已取消；对端退出状态未知";

impl Effect {
    fn payload_bytes(&self) -> usize {
        match self {
            Self::Send(b) | Self::Write(b) => b.len(),
            Self::Begin { name, .. } => name.len(),
            Self::Done { message, .. } | Self::CancelDone { message } => message.len(),
            _ => 0,
        }
    }
}

#[derive(Default)]
struct EffectQueue {
    round: u64,
    data: std::collections::VecDeque<QueuedEffect>,
    bytes: usize,
    failed: bool,
    // 独立的两个控制槽，数据满时仍可取消和终结；归属 round。
    cancel: bool,
    terminal: Option<Effect>,
    closed: bool,
    read_paused: bool,
    read_stopped: bool,
    read_ready: Arc<Notify>,
}

impl EffectQueue {
    fn reset(&mut self, round: u64) {
        let closed = self.closed;
        let read_ready = self.read_ready.clone();
        *self = Self {
            round,
            closed,
            read_ready,
            ..Self::default()
        };
        self.read_ready.notify_one(); // 旧轮等待者须醒来重查新轮状态。
    }

    fn release_read(&mut self) {
        self.read_stopped = true;
        self.read_paused = false;
        self.read_ready.notify_one();
    }

    fn close(&mut self) {
        self.closed = true;
        self.release_read();
    }

    fn update_read_pressure(&mut self) {
        if self.read_stopped || self.closed || self.failed {
            return;
        }
        if self.read_paused {
            if self.data.len() <= READ_LOW_EFFECTS && self.bytes <= READ_LOW_BYTES {
                self.read_paused = false;
                self.read_ready.notify_one();
            }
        } else if self.data.len() >= READ_HIGH_EFFECTS || self.bytes >= READ_HIGH_BYTES {
            self.read_paused = true;
        }
    }

    fn push(
        &mut self,
        transfer_id: u64,
        effect: Effect,
        cancelling: &AtomicBool,
    ) -> Result<(), String> {
        if transfer_id != self.round {
            return Ok(());
        }
        if self.closed {
            return Err("终端传输驱动不可用，结果未知".into());
        }
        if self.failed {
            return if matches!(effect, Effect::Cancel | Effect::CancelDone { .. }) {
                Ok(()) // 已保留唯一 CAN/失败终态，不重复通知或覆盖原因。
            } else {
                Err(QUEUE_OVERFLOW.into())
            };
        }
        match effect {
            Effect::Cancel => {
                // CAN 会抢先于普通队列；必须同时封住所有旧数据效果。
                cancelling.store(true, Ordering::SeqCst);
                self.release_read();
                self.cancel = true;
                return Ok(());
            }
            Effect::CancelDone { mut message } => {
                self.release_read();
                let mut end = message.len().min(MAX_CONTROL_MESSAGE_BYTES);
                while !message.is_char_boundary(end) {
                    end -= 1;
                }
                message.truncate(end);
                self.terminal = Some(Effect::CancelDone { message });
                return Ok(());
            }
            _ => {}
        }
        let bytes = effect.payload_bytes();
        if self.data.len() >= MAX_QUEUED_EFFECTS
            || bytes > MAX_QUEUED_BYTES.saturating_sub(self.bytes)
        {
            self.failed = true;
            self.release_read();
            self.data.clear();
            self.bytes = 0;
            self.cancel = true;
            self.terminal = Some(Effect::CancelDone {
                message: QUEUE_OVERFLOW.into(),
            });
            return Err(QUEUE_OVERFLOW.into());
        }
        if matches!(effect, Effect::Done { ok: false, .. }) {
            self.release_read();
        }
        self.bytes += bytes;
        self.data.push_back(QueuedEffect {
            transfer_id,
            effect,
        });
        self.update_read_pressure();
        Ok(())
    }

    fn pop(&mut self) -> Option<QueuedEffect> {
        let control = if std::mem::take(&mut self.cancel) {
            Some(Effect::Cancel)
        } else {
            self.terminal.take()
        };
        if let Some(effect) = control {
            return Some(QueuedEffect {
                transfer_id: self.round,
                effect,
            });
        }
        let queued = self.data.pop_front()?;
        self.bytes -= queued.effect.payload_bytes();
        self.update_read_pressure();
        Some(queued)
    }
}

struct EffectChannel {
    queue: Mutex<EffectQueue>,
    ready: Notify,
    // 原 pipe 唯一等待者；不能与 driver 的 ready 争抢通知。
    read_ready: Arc<Notify>,
}

impl Default for EffectChannel {
    fn default() -> Self {
        let read_ready = Arc::new(Notify::new());
        Self {
            queue: Mutex::new(EffectQueue {
                read_ready: read_ready.clone(),
                ..Default::default()
            }),
            ready: Notify::new(),
            read_ready,
        }
    }
}

impl EffectChannel {
    fn read_backpressure(&self) -> Option<&Notify> {
        self.queue
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .read_paused
            .then_some(self.read_ready.as_ref())
    }

    async fn recv(&self) -> QueuedEffect {
        loop {
            let notified = self.ready.notified();
            if let Some(queued) = self.queue.lock().unwrap().pop() {
                return queued;
            }
            notified.await;
        }
    }
}

// 保留旧通道的断开语义；drive 返回后同步生产者明确失败。
struct EffectReceiver(Arc<EffectChannel>);
impl Drop for EffectReceiver {
    fn drop(&mut self) {
        let mut queue = self.0.queue.lock().unwrap();
        let round = queue.round;
        queue.reset(round);
        queue.close();
    }
}

/// 多文件上传的队列项。句柄在入队时打开（begin_send 全量校验的产物），
/// 排队期间一直持有——排队时再开，文件可能已经被人挪走，而那要等到
/// 轮到它才发现，报错时用户已经忘了当初选了什么。
struct PendingFile {
    path: PathBuf,
    /// begin_send 校验时记下的字节数。排队期间文件可能被人改掉——按**入队时**
    /// 的大小发，发不完（文件变短）协议自己会以 ZRPOS 重传暴露；按现查的大小发
    /// 则要在驱动的热路径上做一次 metadata，换不来任何正确性。
    size: u64,
    file: Option<std::io::BufReader<std::fs::File>>,
}

/// 传输态：状态机 + 两端的文件句柄。
struct Transfer {
    id: u64,
    session: ZmodemSession,
    /// 接收落盘句柄与最终路径
    sink: Option<std::io::BufWriter<std::fs::File>>,
    sink_path: Option<PathBuf>,
    /// 发送来源句柄
    source: Option<std::io::BufReader<std::fs::File>>,
    /// 接收侧：对端 ZFILE 声明的原始文件名（finalize 时清洗落定）
    recv_name: Option<String>,
    /// 进度节流用：上次已上报的字节数
    reported: u64,
    /// 多文件上传：其余待发文件（含已打开的句柄）
    pending: std::collections::VecDeque<PendingFile>,
    /// 当前是第几个文件（1 起数）
    file_index: u32,
    /// 共几个文件
    file_count: u32,
}

pub(crate) fn mcp_terminal_busy(state: &crate::state::AppState, session_id: &str) -> bool {
    state
        .zmodem
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(session_id)
        .is_some_and(|tap| owner_slot_busy(&tap.mcp))
}

fn owner_slot_busy(slot: &Mutex<Option<McpOwner>>) -> bool {
    // finish 可持 owner 同步落盘；拒忙不能反过来等待该 IO，也不能阻塞取消请求。
    match slot.try_lock() {
        Ok(owner) => owner.is_some(),
        Err(std::sync::TryLockError::WouldBlock) => true,
        Err(std::sync::TryLockError::Poisoned(owner)) => owner.into_inner().is_some(),
    }
}

/// 所有普通输入与长操作共用原 gate；predicate 只查询当前 MCP owner，不持锁 await。
pub(crate) async fn idle_operation_gate(
    gate: Arc<tokio::sync::Mutex<()>>,
    busy: impl Fn() -> bool,
) -> Result<tokio::sync::OwnedMutexGuard<()>, String> {
    if busy() {
        return Err(fs_mcpbridge::ports::SESSION_BUSY.into());
    }
    let guard = gate.lock_owned().await;
    if busy() {
        return Err(fs_mcpbridge::ports::SESSION_BUSY.into());
    }
    Ok(guard)
}

pub(crate) enum McpTransferIo {
    Upload(UploadSource),
    Download {
        basename: String,
        sink: DownloadSink,
        expected: Option<u64>,
    },
}
pub(crate) struct McpReservation {
    pub handle: String,
    pub generation: u64,
    pub remote_path: String,
    pub local_relative_path: Option<String>,
    pub io: McpTransferIo,
}
// 一个 guard 只绑定原 driver 的通道和 owner 槽，不会清理新 tap 的任务。
struct McpDriverGuard(
    Weak<Mutex<Option<McpOwner>>>,
    Weak<EffectChannel>,
    Weak<TailOutput>,
);
impl Drop for McpDriverGuard {
    fn drop(&mut self) {
        let slot = self.0.upgrade();
        let mut owner = slot
            .as_ref()
            .map(|slot| slot.lock().unwrap_or_else(|p| p.into_inner()));
        // Owner → Tail → Queue；发布旧前缀后 closed 才可见，且不触碰 cur。
        if let (Some(owner), Some(tail)) = (
            owner.as_mut().and_then(|slot| slot.as_mut()),
            self.2.upgrade(),
        ) {
            tail.reclaim(owner);
        }
        if let Some(effects) = self.1.upgrade() {
            effects
                .queue
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .close();
        }
        if let Some(mut slot) = owner {
            slot.take();
        }
    }
}

#[derive(Default)]
struct TailOutput {
    bytes: Mutex<Vec<u8>>,
    ready: Notify,
}
impl TailOutput {
    fn publish(&self, bytes: Vec<u8>) {
        if bytes.is_empty() {
            return;
        }
        self.bytes
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .extend(bytes);
        self.ready.notify_one();
    }
    fn reclaim(&self, owner: &mut McpOwner) {
        self.publish(owner.take_passive_prefix());
    }
    fn drain(&self) -> Vec<u8> {
        std::mem::take(&mut *self.bytes.lock().unwrap_or_else(|p| p.into_inner()))
    }
}
enum McpProbe {
    Echo(Vec<u8>),
    Start { echo: Vec<u8>, protocol: Vec<u8> },
    Reject { echo: Vec<u8>, reason: &'static str },
}
struct McpOwner {
    request: McpReservation,
    cancel: tokio::sync::watch::Receiver<bool>,
    started: Instant,
    activity: Instant,
    active: bool,
    attempted: bool,
    stop: Option<(bool, String)>,
    done: u64,
    total: Option<u64>,
    pending: Vec<u8>,
}
impl McpOwner {
    fn new(request: McpReservation, cancel: tokio::sync::watch::Receiver<bool>) -> Self {
        let total = match &request.io {
            McpTransferIo::Upload(source) => Some(source.size()),
            _ => None,
        };
        Self {
            request,
            cancel,
            started: Instant::now(),
            activity: Instant::now(),
            active: false,
            attempted: false,
            stop: None,
            done: 0,
            total,
            pending: Vec::new(),
        }
    }
    fn mark_command_attempted(&mut self, handle: &str, generation: u64) -> Result<(), String> {
        if self.request.handle != handle
            || self.request.generation != generation
            || self.stop.is_some()
            || *self.cancel.borrow()
        {
            return Err("终端传输预约已变化或取消".into());
        }
        self.attempted = true;
        Ok(())
    }
    fn fail_submission(&mut self, handle: &str, generation: u64, message: &str) -> bool {
        if self.request.handle != handle || self.request.generation != generation {
            return false;
        }
        let cancelled = *self.cancel.borrow();
        self.stop.get_or_insert((
            cancelled,
            if cancelled {
                "本地取消已受理"
            } else {
                message
            }
            .into(),
        ));
        true
    }
    fn direction(&self) -> Direction {
        match self.request.io {
            McpTransferIo::Upload(_) => Direction::Send,
            _ => Direction::Receive,
        }
    }
    /// pending 只存未向终端输出的潜在头前缀，不能沿用 GUI 的已输出滑窗语义。
    fn probe(&self, mut bytes: Vec<u8>, pending: &mut Vec<u8>) -> McpProbe {
        use fs_terminal::zmodem::{parse_header, Parsed, ZPAD};
        for at in 0..bytes.len() {
            if bytes[at] != ZPAD {
                continue;
            }
            match parse_header(&bytes[at..]) {
                Parsed::Incomplete => {
                    *pending = bytes.split_off(at);
                    return if pending.len() <= SNIFF_WINDOW {
                        McpProbe::Echo(bytes)
                    } else {
                        McpProbe::Reject {
                            echo: bytes,
                            reason: "握手前缀超过上限",
                        }
                    };
                }
                Parsed::Header(header, _) if classify_start(header.kind).is_some() => {
                    let matches = matches!(
                        (self.direction(), classify_start(header.kind)),
                        (Direction::Send, Some(Detected::NeedLocalFile))
                            | (Direction::Receive, Some(Detected::Receive))
                    );
                    let protocol = bytes.split_off(at);
                    return if matches {
                        McpProbe::Start {
                            echo: bytes,
                            protocol,
                        }
                    } else {
                        McpProbe::Reject {
                            echo: bytes,
                            reason: "握手方向与预约不一致",
                        }
                    };
                }
                _ => (),
            }
        }
        McpProbe::Echo(bytes)
    }
    fn take_passive_prefix(&mut self) -> Vec<u8> {
        if self.active {
            Vec::new()
        } else {
            std::mem::take(&mut self.pending)
        }
    }
    fn paths(&self) -> (Option<String>, Option<String>) {
        match &self.request.io {
            McpTransferIo::Download { sink, .. } => (
                Some(sink.local_relative_path().to_string_lossy().into_owned()),
                sink.partial_path().file_name().map(|name| {
                    Path::new(".incoming")
                        .join(name)
                        .to_string_lossy()
                        .into_owned()
                }),
            ),
            _ => (self.request.local_relative_path.clone(), None),
        }
    }
    fn session(&self) -> Result<ZmodemSession, String> {
        match &self.request.io {
            McpTransferIo::Upload(source) => {
                Ok(ZmodemSession::new_send(source.basename(), source.size()))
            }
            McpTransferIo::Download { basename, .. } => {
                ZmodemSession::new_receive_single(basename).map_err(str::to_string)
            }
        }
    }
    fn activate_session(
        &mut self,
        protocol: &[u8],
    ) -> Result<(ZmodemSession, Vec<Action>), String> {
        let mut session = self.session()?;
        // 已收到完整首握手；feed 负责唯一应答，不再额外发主动 hello。
        let actions = session.feed(protocol);
        self.active = true;
        self.activity = Instant::now();
        self.done = session.transferred();
        self.total = session.declared_size();
        Ok((session, actions))
    }
    fn poll_stop(&mut self, now: Instant) -> bool {
        if self.stop.is_some() {
            return false;
        }
        let cancelled = *self.cancel.borrow(); // bool copy；不把 watch guard 带到其他锁/await。
        let expired = if self.active {
            now.duration_since(self.activity) >= Duration::from_secs(30)
        } else {
            now.duration_since(self.started) >= Duration::from_secs(15)
        };
        if !cancelled && !expired {
            return false;
        }
        self.stop = Some((
            cancelled,
            if cancelled {
                "本地取消已受理"
            } else {
                "终端传输握手/空闲等待超时"
            }
            .into(),
        ));
        true
    }
    fn complete(
        &mut self,
        ok: bool,
        message: &str,
        commit: impl FnOnce(&mut McpTransferIo) -> Result<(), String>,
    ) -> TerminalOutcome {
        if self.stop.is_none() && *self.cancel.borrow() {
            self.stop = Some((true, "本地取消已受理".into()));
        }
        if let Some((cancelled, reason)) = &self.stop {
            let message = format!(
                "{reason}；{}",
                if self.attempted {
                    "对端退出状态未知"
                } else {
                    "尚未尝试发送远端命令"
                }
            );
            return if *cancelled {
                TerminalOutcome::Cancelled(message)
            } else {
                TerminalOutcome::Failed(message)
            };
        }
        if !ok {
            return TerminalOutcome::Failed(format!("{message}；传输/对端退出未验证"));
        }
        // 此处最后一次核取消后同步提交。提交期间迟到的 request 不能推翻 Ok Receipt。
        match commit(&mut self.request.io) {
            Ok(()) => TerminalOutcome::Completed,
            Err(error) => {
                TerminalOutcome::Failed(format!("{error}；暂存/目标文件可能已保留，请核对"))
            }
        }
    }
}

/// 终端字节拦截器 + 传输态持有者。
pub struct ZmodemTap {
    session_id: String,
    app: AppHandle,
    /// 落盘根目录（装配时定格，见模块头）
    sandbox: PathBuf,
    /// 自动检测开关：关闭时完全透传（用户不想让终端里的字节触发传输）
    auto: bool,
    /// 当前传输；None = 空闲
    cur: Mutex<Option<Transfer>>,
    // 所有组合访问严格 cur → mcp；整个预约/活动轮次共用同一 owner。
    mcp: Arc<Mutex<Option<McpOwner>>>,
    /// 跨读块嗅探滑窗
    sniff: Mutex<Vec<u8>>,
    /// 快路径判定：空闲且不需嗅探时避免加锁
    busy: AtomicBool,
    /// 「远端在等文件」是否已通知过前端。
    ///
    /// `rz` 每隔约十秒重发一次 ZRINIT，逐次弹框会把用户埋在对话框里。置位后
    /// 只在传输结束（或取消）时复位。
    need_file_announced: AtomicBool,
    /// 当前传输的代次；队列效果与它绑定，不得作用到下一轮。
    transfer_id: AtomicU64,
    /// 取消已受理，等待驱动处理终态；重复取消幂等。
    cancelling: AtomicBool,
    /// 首次有效登记的 SSH 连接代次；0 表示尚未登记。
    connection_generation: AtomicU64,
    /// 通道/驱动失败，不得被后续成功状态覆盖。
    failure: Mutex<Option<String>>,
    tail: Arc<TailOutput>,
    /// 本会话已确定的下载目录（2026-08-26 两段式）。
    ///
    /// 一次 `sz` 批量传多个文件时，第一个文件做的「去哪」决定对**本次会话**
    /// 生效：后续文件不再问去向（冲突仍逐个问）。会话结束即作废——下次重新
    /// 按设置/引导走，不让一个临时决定悄悄变成永久设置。
    session_download_dir: Mutex<Option<PathBuf>>,
    /// 收完待归置的下载（spool 路径 + 对端声明的原名）。
    ///
    /// `Effect::Finish` 收完一个文件时填入；`zmodem_finalize` 命令消费它。
    /// 最多两个：队首未决时下一个已收完，先登记后归置队首；失败立即取消。
    /// 空闲时拒绝新传输，恢复记录不跨传输累积。
    pending_finalize: Mutex<std::collections::VecDeque<PendingFinalize>>,
    /// 本会话真正落地的最终路径集合（reveal 的白名单，见 [`Self::resolve_received`]）。
    finalized: Mutex<std::collections::HashSet<PathBuf>>,
    effects: Arc<EffectChannel>,
}

fn cancellation_actions(
    cur: &mut Option<Transfer>,
    waiting: &AtomicBool,
    cancelling: &AtomicBool,
) -> Result<Vec<Action>, String> {
    if cancelling.load(Ordering::SeqCst) {
        return Ok(Vec::new());
    }
    if let Some(transfer) = cur.as_mut() {
        let acts = transfer.session.cancel();
        if !acts.is_empty() {
            cancelling.store(true, Ordering::SeqCst);
        }
        return Ok(acts);
    }
    if waiting.load(Ordering::SeqCst) {
        cancelling.store(true, Ordering::SeqCst);
        return Ok(vec![
            Action::Send(fs_terminal::zmodem::cancel_sequence()),
            Action::Done {
                ok: false,
                message: "已请求取消；对端退出状态未知".into(),
            },
        ]);
    }
    Err("当前没有进行中的传输".into())
}

/// 一次待归置的下载。
#[derive(Clone)]
struct PendingFinalize {
    decision_id: String,
    /// spool 文件（sandbox/.incoming/recv-<纳秒>.part）
    spool: PathBuf,
    /// 对端 ZFILE 声明的原始文件名（尚未清洗——清洗在 finalize 时做，
    /// 因为清洗规则属于「最终落地」这一侧，spool 名与它无关）。
    orig_name: String,
    /// 对端声明的字节数（saved 事件的 total 用）。
    bytes: u64,
    /// 同一决策最近一次呈现的完整载荷，恢复时原样重放。
    presentation: Option<serde_json::Value>,
}

impl ZmodemTap {
    /// 建拦截器并启动驱动任务。
    pub fn new(
        app: AppHandle,
        state: Arc<crate::state::AppState>,
        session_id: String,
        sandbox: PathBuf,
        auto: bool,
    ) -> Arc<Self> {
        let effects = Arc::new(EffectChannel::default());
        let rx = EffectReceiver(effects.clone());
        let tap = Arc::new(Self {
            session_id: session_id.clone(),
            app: app.clone(),
            sandbox,
            auto,
            cur: Mutex::new(None),
            mcp: Arc::new(Mutex::new(None)),
            sniff: Mutex::new(Vec::new()),
            busy: AtomicBool::new(false),
            need_file_announced: AtomicBool::new(false),
            transfer_id: AtomicU64::new(0),
            cancelling: AtomicBool::new(false),
            connection_generation: AtomicU64::new(0),
            failure: Mutex::new(None),
            tail: Arc::new(TailOutput::default()),
            session_download_dir: Mutex::new(None),
            pending_finalize: Mutex::new(Default::default()),
            finalized: Mutex::new(Default::default()),
            effects,
        });
        tokio::spawn(drive(app, state, session_id, Arc::downgrade(&tap), rx));
        tap
    }

    /// 返回同会话 gate，调用方须持有到固定命令写入结束。预约本身不发协议字节。
    pub(crate) async fn reserve_mcp(
        self: &Arc<Self>,
        state: &crate::state::AppState,
        request: McpReservation,
    ) -> Result<tokio::sync::OwnedMutexGuard<()>, String> {
        let (generation, live) = state
            .registry
            .get_with_generation(&self.session_id)
            .ok_or("会话已失效")?;
        let gate = live.operation_gate.clone().lock_owned().await;
        let same_tap = state
            .zmodem
            .lock()
            .unwrap()
            .get(&self.session_id)
            .is_some_and(|t| Arc::ptr_eq(t, self));
        if generation != request.generation
            || state.registry.generation_of(&self.session_id) != Some(generation)
            || !same_tap
        {
            return Err("会话代次或终端传输入口已变化".into());
        }
        let cur = self
            .cur
            .lock()
            .map_err(|_| "终端传输状态异常，driver 不可用")?;
        let mut owner = self
            .mcp
            .lock()
            .map_err(|_| "终端传输 owner 异常，driver 不可用")?;
        if self
            .effects
            .queue
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .closed
        {
            return Err("终端传输 driver 已停止".into());
        }
        if cur.is_some()
            || owner.is_some()
            || self.active()
            || self.need_file_announced.load(Ordering::SeqCst)
            || !self.pending_finalize.lock().unwrap().is_empty()
        {
            return Err("终端已有传输或待保存文件".into());
        }
        // 预校验严格构造，取消通道与 task 在同一临界区内创建，不存在无 owner 的任务。
        let (cancel, receiver) = tokio::sync::watch::channel(false);
        let next = McpOwner::new(request, receiver);
        next.session()?;
        let (local, partial) = next.paths();
        state.mcp_tasks.register(Task {
            snapshot: Snapshot {
                transfer_id: next.request.handle.clone(),
                session_id: self.session_id.clone(),
                direction: if next.direction() == Direction::Send {
                    "up"
                } else {
                    "down"
                }
                .into(),
                state: "queued".into(),
                bytes_done: 0,
                bytes_total: next.total,
                error: None,
                verification: "pending".into(),
                remote_path: Some(next.request.remote_path.clone()),
                local_relative_path: local,
                terminal: Some(TerminalMetadata::zmodem(partial)),
            },
            profile_id: live.profile_id.clone(),
            generation,
            backend: TaskBackend::Zmodem { cancel },
            settled_at: None,
        });
        self.next_round();
        *self.failure.lock().unwrap() = None;
        self.connection_generation
            .store(generation, Ordering::SeqCst);
        self.sniff.lock().unwrap().clear();
        *owner = Some(next);
        self.busy.store(true, Ordering::SeqCst);
        Ok(gate)
    }

    /// B2 必须已持 gate 和 SSH write 锁，随后紧接 data_bytes，不能提前标记尝试。
    pub(crate) fn mark_mcp_command_attempted(
        &self,
        handle: &str,
        generation: u64,
    ) -> Result<(), String> {
        let mut slot = self.mcp.lock().unwrap();
        let owner = slot.as_mut().ok_or("终端传输预约已结束")?;
        owner.mark_command_attempted(handle, generation)
    }

    /// Only the original reservation may fail; late submitters cannot stop a new owner.
    pub(crate) fn fail_mcp_submission(&self, handle: &str, generation: u64, message: &str) {
        let mut cur = self.cur.lock().unwrap();
        let mut slot = self.mcp.lock().unwrap();
        if let Some(owner) = slot.as_mut() {
            if owner.fail_submission(handle, generation, message) {
                if let Err(error) = self.stop_mcp(&mut cur, owner) {
                    *self.failure.lock().unwrap() = Some(error);
                }
            }
        }
    }

    pub(crate) fn mcp_busy(&self) -> bool {
        self.mcp.lock().unwrap().is_some()
    }

    fn settle_mcp(&self, state: &crate::state::AppState, ok: bool, message: &str) {
        let mut slot = self.mcp.lock().unwrap();
        if let Some(owner) = slot.as_mut() {
            self.tail.reclaim(owner);
            if let Some(error) = self.failure_message() {
                owner.stop = Some((false, error));
            }
            let outcome = owner.complete(ok, message, |io| match io {
                McpTransferIo::Download { sink, expected, .. } => expected
                    .ok_or_else(|| "未取得完整文件计数".to_string())
                    .and_then(|n| sink.finish(n)),
                _ => Ok(()),
            });
            let (local, mut partial) = owner.paths();
            if matches!(&outcome, TerminalOutcome::Completed) {
                partial = None;
            }
            state.mcp_tasks.terminal_update(
                &owner.request.handle,
                owner.request.generation,
                TerminalUpdate {
                    bytes_done: owner.done,
                    bytes_total: owner.total,
                    outcome: Some(outcome),
                    local_relative_path: local,
                    partial_relative_path: partial,
                },
            );
        }
        // settle-before-drop：读查询遇 closed receiver 时不会把已知成功覆盖成未知。
        *slot = None;
    }

    fn stop_mcp(&self, cur: &mut Option<Transfer>, owner: &mut McpOwner) -> Result<(), String> {
        self.tail.reclaim(owner); // 先于 cancelling；fresh 不能越过旧前缀。
        if self.cancelling.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        let peer_active = !cur.as_ref().is_some_and(|t| t.session.is_finished());
        if let Some(t) = cur.as_mut() {
            let _ = t.session.cancel();
        }
        let round = self.transfer_id.load(Ordering::SeqCst);
        if owner.attempted && peer_active {
            self.queue(round, Effect::Cancel)?;
        }
        self.queue(
            round,
            Effect::CancelDone {
                message: "本地终止；对端退出未验证".into(),
            },
        )
    }

    /// 用户选定本地文件，开始上传（对端已在跑 `rz`）。多文件：一次入队，逐个发。
    ///
    /// **入队前全部校验**，任何一个不合法整批拒绝（错误里点名是哪个）。不是
    /// 「传到一半发现下一个打不开」——传一半再报错，用户已经付出的等待就废了，
    /// 而且远端 rz 已经收了一半文件。
    pub fn begin_send(&self, paths: &[PathBuf]) -> Result<(), String> {
        if paths.is_empty() {
            return Err("没有选中任何文件".into());
        }
        // 先全部校验并打开：开句柄失败的那个文件点名报错，一个都不开始传。
        let mut opened = Vec::with_capacity(paths.len());
        for p in paths {
            let meta = match std::fs::metadata(p) {
                Ok(m) => m,
                Err(e) => return Err(format!("读不到本地文件 {}：{e}", p.display())),
            };
            if !meta.is_file() {
                return Err(format!(
                    "{} 不是普通文件（目录请用 SFTP 面板传输）",
                    p.display()
                ));
            }
            match std::fs::File::open(p) {
                Ok(f) => opened.push((p.clone(), meta.len(), f)),
                Err(e) => return Err(format!("打不开本地文件 {}：{e}", p.display())),
            }
        }
        let total = opened.len() as u32;
        let mut cur = self.cur.lock().unwrap();
        if cur.is_some() || self.mcp_busy() || self.cancelling.load(Ordering::SeqCst) {
            return Err("该会话已有传输在进行或正在取消".into());
        }
        if !self.pending_finalize.lock().unwrap().is_empty() {
            return Err("请先保存已接收的文件，再开始新传输".into());
        }
        let transfer_id = self.next_round();
        *self.failure.lock().unwrap() = None;
        let (first_path, first_size, first_file) = opened.remove(0);
        let name = first_path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("file")
            .to_string();
        let session = ZmodemSession::new_send(&name, first_size);
        let hello = session.start();
        *cur = Some(Transfer {
            id: transfer_id,
            session,
            sink: None,
            sink_path: None,
            source: Some(std::io::BufReader::new(first_file)),
            recv_name: None,
            reported: 0,
            pending: opened
                .into_iter()
                .map(|(path, size, file)| PendingFile {
                    path,
                    size,
                    file: Some(std::io::BufReader::new(file)),
                })
                .collect(),
            file_index: 1,
            file_count: total,
        });
        self.busy.store(true, Ordering::SeqCst);
        if let Err(error) = self.queue(transfer_id, Effect::Send(hello)) {
            *self.failure.lock().unwrap() = Some(error.clone());
            *cur = None;
            self.busy.store(false, Ordering::SeqCst);
            return Err(error);
        }
        drop(cur);
        self.emit_progress_files("sending", &name, 0, Some(first_size), 1, total);
        Ok(())
    }

    /// 带「第 i/N 个」字段的进度事件（多文件上传用）。
    fn emit_progress_files(
        &self,
        phase: &str,
        name: &str,
        done: u64,
        total: Option<u64>,
        file_index: u32,
        file_count: u32,
    ) {
        emit_progress_full(
            &self.app,
            &self.session_id,
            phase,
            name,
            None,
            done,
            total,
            "",
            Some(file_index),
            Some(file_count),
            None,
            None,
            None,
            None,
        );
    }

    /// 用户取消当前传输。
    pub fn cancel(&self) -> Result<(), String> {
        let mut cur = self.cur.lock().unwrap();
        if let Some(owner) = self.mcp.lock().unwrap().as_mut() {
            owner.stop.get_or_insert((true, "本地取消已受理".into()));
            return self.stop_mcp(&mut cur, owner);
        }
        let transfer_id = cur
            .as_ref()
            .map(|t| t.id)
            .unwrap_or_else(|| self.transfer_id.load(Ordering::SeqCst));
        let acts = cancellation_actions(&mut cur, &self.need_file_announced, &self.cancelling)?;
        if acts.is_empty() {
            return Ok(());
        }
        self.busy.store(true, Ordering::SeqCst);
        self.sniff.lock().unwrap().clear();
        if let Err(error) = self.dispatch(transfer_id, acts) {
            *self.failure.lock().unwrap() = Some(error.clone());
            *cur = None;
            self.busy.store(false, Ordering::SeqCst);
            self.cancelling.store(false, Ordering::SeqCst);
            return Err(error);
        }
        drop(cur);
        Ok(())
    }

    /// 是否有传输在进行（前端进度条的存在依据）。
    pub fn active(&self) -> bool {
        self.busy.load(Ordering::SeqCst)
    }

    pub fn failure_message(&self) -> Option<String> {
        self.failure.lock().unwrap().clone()
    }

    /// 落盘根目录（给前端显示「下载到哪」与「打开所在目录」用）。
    pub fn sandbox_display(&self) -> String {
        display_path(&self.sandbox)
    }

    /// 自动检测是否开启。
    pub fn auto_enabled(&self) -> bool {
        self.auto
    }

    /// 校验一条「已接收文件」的路径，供 reveal 用。
    ///
    /// 入参来自前端，而 reveal 会打开文件管理器并选中目标——拿它去探测任意路径
    /// 是否存在是可行的，所以必须校验。**2026-08-26 起改为 finalized 白名单**：
    /// 旧判据是「路径在沙箱内」，但两段式之后最终落点可以在任何用户目录，
    /// 沙箱前缀判据失效。白名单只认**本会话真的保存过**的那些精确路径——
    /// 这比旧判据更严（从「沙箱内任意路径」收窄到「实际落地过的文件」），
    /// 安全上是收窄不是让步。
    pub fn resolve_received(&self, path: &str) -> Result<PathBuf, String> {
        if path.trim().is_empty() {
            return Err("路径为空".into());
        }
        let p = PathBuf::from(path);
        if self.finalized.lock().unwrap().contains(&p) {
            Ok(p)
        } else {
            Err("该路径不是本会话保存过的文件".into())
        }
    }

    /// IPC 侧的归置入口（`zmodem_finalize` 命令转调）。
    ///
    /// 命令层负责 remember 写设置与 emit `saved`——那两件要 AppHandle/数据库，
    /// 而本方法保持纯文件操作，便于单测。
    pub fn finalize_from_ipc(
        &self,
        expected_decision_id: &str,
        dir: &str,
        name: Option<&str>,
        on_conflict: &str,
    ) -> Result<Option<(String, PathBuf, u64)>, String> {
        finalize_download(
            self,
            &self.session_id,
            expected_decision_id,
            Path::new(dir),
            name,
            on_conflict,
        )
    }

    /// 仅展示恢复队首；调用方须先发前一项 saved，再发下一项提示。
    pub fn present_pending_download(&self) {
        let mut pending_guard = self.pending_finalize.lock().unwrap();
        let Some(pending) = pending_guard.front_mut() else {
            return;
        };
        let dir = self
            .session_download_dir
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| default_download_dir(&self.sandbox));
        let payload = pending_presentation(pending, &self.session_id, &dir);
        emit_decision_presentation(&self.app, &payload);
    }

    /// 把状态机动作翻译成副作用并投递。
    fn queue(&self, transfer_id: u64, effect: Effect) -> Result<(), String> {
        let mut queue = self.effects.queue.lock().unwrap();
        let was_failed = queue.failed;
        let result = queue.push(transfer_id, effect, &self.cancelling);
        let overflow = !was_failed && queue.failed;
        if overflow {
            // queue 入口也用于持 cur 的同步 filter；这里决不递归 cancel/获取 cur。
            let mut failure = self.failure.lock().unwrap();
            if transfer_id == self.transfer_id.load(Ordering::SeqCst) {
                failure.get_or_insert_with(|| QUEUE_OVERFLOW.into());
                self.cancelling.store(true, Ordering::SeqCst);
            }
        }
        // 后续失败数据不产生 ready 风暴；timer 始终位于 select 的优先分支。
        if !was_failed && (result.is_ok() || overflow) {
            self.effects.ready.notify_one();
        }
        result
    }

    fn next_round(&self) -> u64 {
        let mut queue = self.effects.queue.lock().unwrap();
        let round = self.transfer_id.fetch_add(1, Ordering::SeqCst) + 1;
        queue.reset(round);
        round
    }

    fn dispatch(&self, transfer_id: u64, acts: Vec<Action>) -> Result<(), String> {
        for a in acts {
            let eff = effect_from_action(a, &self.cancelling);
            self.queue(transfer_id, eff)?;
        }
        Ok(())
    }

    fn emit_state(&self, phase: &str, name: &str, done: u64, total: Option<u64>) {
        emit_progress(
            &self.app,
            &self.session_id,
            phase,
            name,
            None,
            done,
            total,
            "",
        );
    }
}

/// 唯一的 `zmodem:progress` 发送点。
///
/// 三处调用方（拦截器、驱动的开始/收尾）走同一个函数，是为了让载荷**键集恒定**：
/// 各处各发一份自己需要的键，前端就得对每个键都写 `?? undefined` 的兜底，而
/// 「这个 phase 到底有没有 path」会变成只能靠读后端代码才能知道的隐性契约。
/// 键恒定之后，前端的类型就是完整的事实。
///
/// `file_index` / `file_count` 同理：多文件上传（2026-08-26）加的两键**每个事件都带**，
/// 非上传场景为 `null`——不是缺席。缺席会让前端在「这条事件有没有这两个键」上
/// 分叉，而 `null` 的语义就是「本事件不适用」，类型上一目了然。
///
/// 事件名保持字面量内联：仓库的跨语言契约扫描从调用点提取事件名，抽成常量它就
/// 扫不到，前后端事件名不一致的守卫随之失效。
#[allow(clippy::too_many_arguments)] // 参数即载荷字段，合并成结构体只是把同样的东西挪个地方
fn emit_progress(
    app: &AppHandle,
    session_id: &str,
    phase: &str,
    name: &str,
    path: Option<&Path>,
    done: u64,
    total: Option<u64>,
    message: &str,
) {
    emit_progress_full(
        app, session_id, phase, name, path, done, total, message, None, None, None, None, None,
        None,
    )
}

/// [`emit_progress`] 的全参版（多文件上传的「第 i/N 个」+ 下载决策的三个补充键）。
///
/// 三组补充键**每个事件都带**（null = 不适用）：file_index/file_count（多文件上传）、
/// suggestedDir/spoolPath/conflict（`needs-decision` 阶段）。键集恒定的理由见
/// [`emit_progress`] 的模块注释。
#[allow(clippy::too_many_arguments)]
fn emit_progress_full(
    app: &AppHandle,
    session_id: &str,
    phase: &str,
    name: &str,
    path: Option<&Path>,
    done: u64,
    total: Option<u64>,
    message: &str,
    file_index: Option<u32>,
    file_count: Option<u32>,
    suggested_dir: Option<&str>,
    spool_path: Option<&str>,
    conflict: Option<bool>,
    decision_id: Option<&str>,
) {
    let _ = app.emit(
        "zmodem:progress",
        serde_json::json!({
            "sessionId": session_id,
            "phase": phase,
            "name": name,
            "path": path.map(display_path),
            "done": done,
            "total": total,
            "message": message,
            "file_index": file_index,
            "file_count": file_count,
            "suggestedDir": suggested_dir,
            "spoolPath": spool_path,
            "conflict": conflict,
            "decisionId": decision_id,
        }),
    );
}

impl ByteTap for ZmodemTap {
    fn read_backpressure(&self) -> Option<&Notify> {
        self.effects.read_backpressure()
    }
    fn output_ready(&self) -> Option<&tokio::sync::Notify> {
        Some(&self.tail.ready)
    }
    fn drain_output(&self, eof: bool) -> Vec<u8> {
        let mut cur = self.cur.lock().unwrap_or_else(|p| p.into_inner());
        let mut owner = self.mcp.lock().unwrap_or_else(|p| p.into_inner());
        if eof {
            if let Some(owner) = owner.as_mut() {
                self.tail.reclaim(owner);
                if cur.is_none() {
                    let _ = self.dispatch(
                        self.transfer_id.load(Ordering::SeqCst),
                        vec![Action::Done {
                            ok: false,
                            message: "终端连接关闭，传输结果未知".into(),
                        }],
                    );
                }
            }
            if let Some(t) = cur.as_mut() {
                self.tail.publish(t.session.receive_tail_at_eof());
                let _ = self.dispatch(
                    t.id,
                    vec![Action::Done {
                        ok: false,
                        message: "终端连接关闭，传输结果未知".into(),
                    }],
                );
            }
        }
        self.tail.drain()
    }
    fn filter<'a>(&self, chunk: &'a [u8]) -> Cow<'a, [u8]> {
        // driver 已结束时旧 cur 不再拥有输入；普通 SSH 输出必须恢复透传。
        if self
            .effects
            .queue
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .closed
        {
            return Cow::Borrowed(chunk);
        }
        // 空闲且不自动检测：整条链路零成本透传
        if !self.auto && !self.busy.load(Ordering::SeqCst) {
            return Cow::Borrowed(chunk);
        }
        let mut cur = self.cur.lock().unwrap_or_else(|p| p.into_inner());
        let mut mcp = self.mcp.lock().unwrap_or_else(|p| p.into_inner());
        if self
            .effects
            .queue
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .closed
        {
            return Cow::Borrowed(chunk);
        }
        if cur.is_none() && self.cancelling.load(Ordering::SeqCst) {
            return Cow::Borrowed(chunk);
        }
        if let Some(t) = cur.as_mut() {
            // 终态效果仍在队列中时保留资源供驱动消费；后续 shell 字节直接透传。
            if self.cancelling.load(Ordering::SeqCst) {
                return Cow::Borrowed(&[]);
            }
            if t.session.is_finished() {
                return Cow::Borrowed(chunk);
            }
            let transfer_id = t.id;
            // 传输中：全部字节喂状态机，屏幕上什么都不出
            let acts = t.session.feed(chunk);
            let finished = t.session.is_finished();
            let tail = if finished {
                t.session.take_remainder()
            } else {
                Vec::new()
            };
            let (done, total) = (t.session.transferred(), t.session.declared_size());
            let dir = t.session.direction();
            if let Some(owner) = mcp.as_mut() {
                owner.done = done;
                owner.total = total;
                if !acts.is_empty() {
                    owner.activity = Instant::now();
                }
            }
            // 里程碑事件带上「第 i/N 个」：不带的话，多文件传输中每个 64 KiB 里程碑
            // 都会把进度条上的计数洗回 null（前端有 prev 兜底，但事件本身说全才是对的）。
            let (fi, fc) = (t.file_index, t.file_count);
            // 进度节流：每 64 KiB 或有里程碑动作时才发事件。逐块发会在千兆链路上
            // 每秒糊上千条 IPC，前端忙于重绘反而拖慢传输。
            let milestone = done.saturating_sub(t.reported) >= 64 * 1024;
            if milestone {
                t.reported = done;
            }
            if finished {
                self.sniff.lock().unwrap().clear();
            }
            // 排队与读状态在同一临界区内排序，取消不能插到这些效果之前。
            if let Err(error) = self.dispatch(transfer_id, acts) {
                *self.failure.lock().unwrap() = Some(error);
            }
            drop(mcp);
            drop(cur);
            if milestone && dir == Direction::Send {
                self.emit_progress_files("sending", "", done, total, fi, fc);
            } else if milestone {
                self.emit_state("receiving", "", done, total);
            }
            return if tail.is_empty() {
                Cow::Borrowed(&[])
            } else {
                Cow::Owned(tail)
            };
        }

        if !self.pending_finalize.lock().unwrap().is_empty() {
            // 已收数据先交用户保存，不接纳另一轮以免恢复队列累积。
            self.sniff.lock().unwrap().clear();
            return Cow::Borrowed(chunk);
        }

        if let Some(owner) = mcp.as_mut() {
            let mut pending = std::mem::take(&mut owner.pending);
            pending.extend_from_slice(chunk);
            let mut retained = Vec::new();
            let probe = owner.probe(pending, &mut retained);
            owner.pending = retained;
            return match probe {
                McpProbe::Echo(echo) => Cow::Owned(echo),
                McpProbe::Reject { echo, reason } => {
                    owner.stop = Some((false, reason.into()));
                    let _ = self.stop_mcp(&mut cur, owner);
                    Cow::Owned(echo)
                }
                McpProbe::Start { echo, protocol } => {
                    let (session, acts) = match owner.activate_session(&protocol) {
                        Ok(started) => started,
                        Err(error) => {
                            *self.failure.lock().unwrap() = Some(error);
                            return Cow::Owned(echo);
                        }
                    };
                    let round = self.transfer_id.load(Ordering::SeqCst);
                    *cur = Some(Transfer {
                        id: round,
                        session,
                        sink: None,
                        sink_path: None,
                        source: None,
                        recv_name: None,
                        reported: 0,
                        pending: Default::default(),
                        file_index: 1,
                        file_count: 1,
                    });
                    if let Err(error) = self.dispatch(round, acts) {
                        *self.failure.lock().unwrap() = Some(error);
                    }
                    Cow::Owned(echo)
                }
            };
        }
        drop(mcp);
        // 空闲：嗅探帧起始。滑窗与本块拼一起再找，否则被读块切开的帧头认不出来。
        let mut sniff = self.sniff.lock().unwrap();
        let s = sniff.len();
        let mut probe = std::mem::take(&mut *sniff);
        probe.extend_from_slice(chunk);
        match detect_start(&probe) {
            None => {
                // 只留尾巴，别让滑窗随输出无界增长
                let keep = probe.len().saturating_sub(SNIFF_WINDOW);
                *sniff = probe[keep..].to_vec();
                Cow::Borrowed(chunk)
            }
            Some((kind, at)) => match classify_start(kind) {
                None => {
                    // detect_start 认了但分类不认：宁可透传也不猜
                    let keep = probe.len().saturating_sub(SNIFF_WINDOW);
                    *sniff = probe[keep..].to_vec();
                    Cow::Borrowed(chunk)
                }
                Some(Detected::NeedLocalFile) => {
                    // 远端 `rz` 在等文件。**不**进入传输态：进去就得一直吞字节等人
                    // 选文件，人不选就永远吞下去，终端从此哑掉。让 ZRINIT 原样显示
                    // （屏幕上闪一串 `**B0100...`，Xshell 同样如此），同时通知前端
                    // 弹选择框；用户选完由 `begin_send` 正式起会话。
                    sniff.clear();
                    let first = !self.need_file_announced.swap(true, Ordering::SeqCst);
                    if first {
                        self.next_round();
                        *self.failure.lock().unwrap() = None;
                    }
                    drop(sniff);
                    drop(cur);
                    if first {
                        tracing::info!(
                            session_id = %self.session_id,
                            "检出 ZRINIT：远端在等文件（rz），请求前端选择本地文件"
                        );
                        self.emit_state("need-file", "", 0, None);
                    }
                    Cow::Borrowed(chunk)
                }
                Some(Detected::Receive) => {
                    sniff.clear();
                    drop(sniff);
                    let mut session = ZmodemSession::new_receive();
                    let hello = session.start();
                    let acts = session.feed(&probe[at..]);
                    let (done, total) = (session.transferred(), session.declared_size());
                    let transfer_id = self.next_round();
                    *self.failure.lock().unwrap() = None;
                    *cur = Some(Transfer {
                        id: transfer_id,
                        session,
                        sink: None,
                        sink_path: None,
                        source: None,
                        recv_name: None,
                        reported: 0,
                        pending: Default::default(),
                        file_index: 1,
                        file_count: 1,
                    });
                    self.busy.store(true, Ordering::SeqCst);
                    if let Err(error) = self
                        .queue(transfer_id, Effect::Send(hello))
                        .and_then(|_| self.dispatch(transfer_id, acts))
                    {
                        *self.failure.lock().unwrap() = Some(error);
                    }
                    drop(cur);
                    tracing::info!(
                        session_id = %self.session_id, ?kind, at,
                        "检出 ZRQINIT：远端 sz 发文件，转入接收模式"
                    );
                    self.emit_state("receiving", "", done, total);
                    // 帧起始之前的字节仍属终端。`at` 是在 `probe` 里的下标：落在滑窗
                    // 段内（at < s）说明帧头始于上一块，本块无一字节属于终端。
                    if at >= s {
                        Cow::Borrowed(&chunk[..at - s])
                    } else {
                        Cow::Borrowed(&[])
                    }
                }
            },
        }
    }
}

async fn acquire_operation_gate(
    gate: Arc<tokio::sync::Mutex<()>>,
    deadline: tokio::time::Instant,
) -> Result<tokio::sync::OwnedMutexGuard<()>, String> {
    tokio::time::timeout_at(deadline, gate.lock_owned())
        .await
        .map_err(|_| "终端传输登记/操作锁超时，结果未知".to_string())
}

/// 等待本 tap 首次登记，并固定其 SSH 连接代次。
/// 仅首次登记可等待；已经绑定的 tap 一旦失效决不能绑定新连接。
async fn bound_session(
    state: &Arc<crate::state::AppState>,
    session_id: &str,
    tap: &Arc<ZmodemTap>,
) -> Result<
    (
        Arc<crate::sessions::LiveSession>,
        tokio::sync::OwnedMutexGuard<()>,
    ),
    String,
> {
    let deadline = tokio::time::Instant::now() + EFFECT_BUDGET;
    loop {
        let bound = tap.connection_generation.load(Ordering::SeqCst);
        if let Some((generation, session)) = state.registry.get_with_generation(session_id) {
            if bound != 0 && bound != generation {
                return Err("终端连接已变化，传输结果未知".into());
            }
            let gate = session.operation_gate.clone();
            let guard = acquire_operation_gate(gate, deadline).await?;
            let same_generation = state.registry.generation_of(session_id) == Some(generation);
            let same_tap = state
                .zmodem
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .get(session_id)
                .is_some_and(|current| Arc::ptr_eq(current, tap));
            if same_generation && same_tap {
                tap.connection_generation
                    .store(generation, Ordering::SeqCst);
                return Ok((session, guard));
            }
            drop(guard);
            if bound != 0 {
                return Err("终端传输会话已失效，结果未知".into());
            }
        } else if bound != 0 {
            return Err("终端会话已关闭，传输结果未知".into());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err("终端传输首次登记超时，结果未知".into());
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

fn publish_tail(tap: &ZmodemTap, tail: Vec<u8>) {
    tap.tail.publish(tail);
}

/// 驱动任务：串行执行副作用。
async fn drive(
    app: AppHandle,
    state: Arc<crate::state::AppState>,
    session_id: String,
    tap: std::sync::Weak<ZmodemTap>,
    rx: EffectReceiver,
) {
    let _mcp_lifetime = tap.upgrade().map(|t| {
        McpDriverGuard(
            Arc::downgrade(&t.mcp),
            Arc::downgrade(&t.effects),
            Arc::downgrade(&t.tail),
        )
    });
    let mut close_timer = tokio::time::interval(std::time::Duration::from_millis(100));
    close_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        let queued = tokio::select! {
            biased;
            _ = close_timer.tick() => {
                let Some(tap) = tap.upgrade() else { return };
                let mut cur = tap.cur.lock().unwrap();
                if let Some(owner) = tap.mcp.lock().unwrap().as_mut() {
                    if owner.poll_stop(Instant::now()) {
                        let _ = tap.stop_mcp(&mut cur, owner);
                    }
                }
                if let Some(transfer) = cur.as_mut() {
                    let acts = transfer.session.expire_close(std::time::Instant::now());
                    if acts.iter().any(|a| matches!(a, Action::Done { ok: true, .. })) {
                        let tail = transfer.session.take_remainder();
                        if !tail.is_empty() {
                            publish_tail(&tap, tail);
                        }
                    }
                    if let Err(error) = tap.dispatch(transfer.id, acts) {
                        *tap.failure.lock().unwrap() = Some(error);
                    }
                }
                continue;
            }
            queued = rx.0.recv() => queued,
        };
        let Some(tap) = tap.upgrade() else { return };
        let transfer_id = queued.transfer_id;
        if transfer_id != tap.transfer_id.load(Ordering::SeqCst) {
            continue;
        }
        let (session, _lifecycle) = match bound_session(&state, &session_id, &tap).await {
            Ok(binding) => binding,
            Err(error) => {
                tracing::warn!(%session_id, %error, "ZMODEM 驱动停止");
                *tap.failure.lock().unwrap() = Some(error.clone());
                if state
                    .zmodem
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .get(&session_id)
                    .is_some_and(|current| Arc::ptr_eq(current, &tap))
                {
                    emit_progress(&app, &session_id, "failed", "", None, 0, None, &error);
                }
                tap.settle_mcp(&state, false, &error);
                *tap.cur.lock().unwrap() = None;
                tap.busy.store(false, Ordering::SeqCst);
                tap.cancelling.store(false, Ordering::SeqCst);
                return;
            }
        };
        // Waiting for the lifecycle gate may span completion and a new round.
        if transfer_id != tap.transfer_id.load(Ordering::SeqCst) {
            continue;
        }
        if !effect_admitted(
            &queued.effect,
            tap.cancelling.load(Ordering::SeqCst),
            tap.failure_message().is_some(),
        ) {
            continue;
        }
        // MCP IO 仍由这一条 driver 按原效果顺序执行，不走 GUI 保存决策。
        let mcp_io = {
            let mut slot = tap.mcp.lock().unwrap();
            slot.as_mut().and_then(|owner| {
                state.mcp_tasks.terminal_update(
                    &owner.request.handle,
                    owner.request.generation,
                    TerminalUpdate {
                        bytes_done: owner.done,
                        bytes_total: owner.total,
                        outcome: None,
                        local_relative_path: None,
                        partial_relative_path: None,
                    },
                );
                match (&mut owner.request.io, &queued.effect) {
                    (McpTransferIo::Download { .. }, Effect::Begin { size, .. }) => {
                        owner.total = *size;
                        Some(Ok(()))
                    }
                    (McpTransferIo::Download { sink, .. }, Effect::Write(bytes)) => {
                        Some(sink.write_chunk(bytes))
                    }
                    (McpTransferIo::Download { sink, expected, .. }, Effect::Finish { bytes }) => {
                        *expected = Some(*bytes);
                        Some(if sink.bytes() == *bytes {
                            Ok(())
                        } else {
                            Err("落盘与协议计数不一致".into())
                        })
                    }
                    _ => None,
                }
            })
        };
        if let Some(result) = mcp_io {
            if let Err(error) = result {
                *tap.failure.lock().unwrap() = Some(error);
                let _ = tap.cancel();
            }
            continue;
        }
        match queued.effect {
            effect @ (Effect::Send(_) | Effect::Cancel) => {
                let is_cancel = matches!(effect, Effect::Cancel);
                let bytes = match effect {
                    Effect::Send(bytes) => bytes,
                    Effect::Cancel => fs_terminal::zmodem::cancel_sequence(),
                    _ => unreachable!(),
                };
                // 写锁与通道背压共用预算。超时后的写入结果未知，绝不重发。
                let result = tokio::time::timeout(EFFECT_BUDGET, async {
                    let guard = session.write.lock().await;
                    if !is_cancel && tap.cancelling.load(Ordering::SeqCst) {
                        return Ok(());
                    }
                    guard.data_bytes(bytes).await.map_err(|e| e.to_string())
                })
                .await
                .unwrap_or_else(|_| Err("写锁/通道写入超时".into()));
                if let Err(e) = result {
                    let error = format!("ZMODEM 写入失败，传输结果未知：{e}");
                    tracing::warn!(%session_id, %error);
                    *tap.failure.lock().unwrap() = Some(error.clone());
                    emit_progress(&app, &session_id, "failed", "", None, 0, None, &error);
                    let mut cur = tap.cur.lock().unwrap();
                    if let Some(t) = cur.as_mut() {
                        let _ = t.session.cancel();
                    }
                    tap.cancelling.store(true, Ordering::SeqCst);
                    let _ = tap.queue(transfer_id, Effect::CancelDone { message: error });
                }
            }
            Effect::Begin { name, size } => {
                match open_spool(&tap.sandbox) {
                    Ok((file, path)) => {
                        tracing::info!(%session_id, ?path, size, "ZMODEM 开始接收（spool）");
                        let mut cur = tap.cur.lock().unwrap();
                        if let Some(t) = cur.as_mut() {
                            t.sink = Some(std::io::BufWriter::new(file));
                            t.sink_path = Some(path.clone());
                            t.recv_name = Some(name.clone());
                        }
                        drop(cur);
                        // spool 期间给用户看的是**对端声明的名字**——.incoming 里的
                        // recv-*.part 对人毫无意义；真正落定的名字 finalize 时再报。
                        emit_progress(&app, &session_id, "receiving", &name, None, 0, size, "");
                    }
                    Err(e) => {
                        tracing::warn!(%session_id, name, error = %e, "ZMODEM 建暂存文件失败，取消传输");
                        let error = format!("ZMODEM 建暂存文件失败，传输结果未知：{e}");
                        *tap.failure.lock().unwrap() = Some(error.clone());
                        emit_progress(&app, &session_id, "failed", "", None, 0, None, &error);
                        let _ = tap.cancel();
                    }
                }
            }
            Effect::Write(data) => {
                let result = {
                    let mut cur = tap.cur.lock().unwrap();
                    cur.as_mut()
                        .and_then(|t| t.sink.as_mut())
                        .ok_or_else(|| "接收文件尚未打开".to_string())
                        .and_then(|sink| sink.write_all(&data).map_err(|e| e.to_string()))
                };
                if let Err(e) = result {
                    let error = format!("ZMODEM 落盘写失败，传输结果未知：{e}");
                    *tap.failure.lock().unwrap() = Some(error.clone());
                    emit_progress(&app, &session_id, "failed", "", None, 0, None, &error);
                    let _ = tap.cancel();
                }
            }
            Effect::Finish { bytes } => {
                let result = {
                    let mut cur = tap.cur.lock().unwrap();
                    match cur.as_mut() {
                        Some(t) => match t.sink.take() {
                            Some(mut sink) => {
                                sink.flush().map_err(|e| e.to_string()).and_then(|()| {
                                    match (t.sink_path.clone(), t.recv_name.clone()) {
                                        (Some(path), Some(name)) => Ok((path, name)),
                                        _ => Err("暂存文件信息缺失".into()),
                                    }
                                })
                            }
                            None => Err("接收文件尚未打开".into()),
                        },
                        None => Err("传输已失效".into()),
                    }
                }
                .and_then(|(path, name)| {
                    let disk = std::fs::metadata(&path).map_err(|e| e.to_string())?.len();
                    if disk != bytes {
                        return Err(format!("落盘字节数 {disk} 与协议计数 {bytes} 不符"));
                    }
                    Ok((path, name))
                });
                let (spool, orig_name) = match result {
                    Ok(value) => value,
                    Err(e) => {
                        let error = format!("ZMODEM 收尾失败，传输结果未知：{e}");
                        *tap.failure.lock().unwrap() = Some(error.clone());
                        emit_progress(&app, &session_id, "failed", "", None, 0, None, &error);
                        let _ = tap.cancel();
                        continue;
                    }
                };
                let dir = tap
                    .session_download_dir
                    .lock()
                    .unwrap()
                    .clone()
                    .unwrap_or_else(|| default_download_dir(&tap.sandbox));
                // 登记与归置持同一把锁，IPC 不会抢先消费 A 导致 fallback 误保存 B。
                let result = retain_received(
                    &mut tap.pending_finalize.lock().unwrap(),
                    &tap.session_download_dir,
                    &tap.finalized,
                    &session_id,
                    PendingFinalize {
                        decision_id: uuid::Uuid::new_v4().to_string(),
                        spool,
                        orig_name,
                        bytes,
                        presentation: None,
                    },
                    &dir,
                );
                match result {
                    Ok(Some((name, path, bytes, decision_id))) => emit_progress_full(
                        &app,
                        &session_id,
                        "saved",
                        &name,
                        Some(&path),
                        bytes,
                        Some(bytes),
                        "",
                        None,
                        None,
                        None,
                        None,
                        None,
                        Some(&decision_id),
                    ),
                    Ok(None) => {}
                    Err(error) => {
                        *tap.failure.lock().unwrap() = Some(error.clone());
                        emit_progress(&app, &session_id, "failed", "", None, 0, None, &error);
                        let _ = tap.cancel();
                        continue;
                    }
                }
                // 归置决策。设置读取要查库（async），这里正好在驱动任务里。
                decide_and_maybe_finalize(&app, &state, &tap, &session_id).await;
            }
            Effect::FileSent => {
                // 出队下一个或收尾。整段持锁：next_file 与换 source 必须原子，
                // 中途放开的话 filter() 会在「新 ZFILE 已发、旧句柄还在」的窗口里
                // 喂数据——喂的还是上一个文件的内容。
                let acts = {
                    let mut cur = tap.cur.lock().unwrap();
                    if tap.cancelling.load(Ordering::SeqCst) {
                        continue;
                    }
                    let Some(t) = cur.as_mut() else { continue };
                    match t.pending.pop_front() {
                        Some(next) => {
                            let name = next
                                .path
                                .file_name()
                                .and_then(|s| s.to_str())
                                .unwrap_or("file")
                                .to_string();
                            let size = next.size;
                            t.source = next.file;
                            t.reported = 0;
                            t.file_index += 1;
                            let acts = t.session.next_file(&name, size);
                            emit_progress_full(
                                &app,
                                &session_id,
                                "sending",
                                &name,
                                None,
                                0,
                                Some(size),
                                "",
                                Some(t.file_index),
                                Some(t.file_count),
                                None,
                                None,
                                None,
                                None,
                            );
                            acts
                        }
                        None => t.session.finish(),
                    }
                };
                let _ = tap.dispatch(transfer_id, acts);
            }
            Effect::NeedChunk { offset } => {
                let mut buf = vec![0u8; fs_terminal::zsession::SUBPACKET_BYTES];
                let result = {
                    let mut cur = tap.cur.lock().unwrap();
                    if tap.cancelling.load(Ordering::SeqCst) {
                        continue;
                    }
                    let Some(t) = cur.as_mut() else { continue };
                    let mut owner = tap.mcp.lock().unwrap();
                    if let Some(McpOwner {
                        request:
                            McpReservation {
                                io: McpTransferIo::Upload(source),
                                ..
                            },
                        activity,
                        done,
                        ..
                    }) = owner.as_mut()
                    {
                        source.read_chunk(offset, buf.len()).map(|bytes| {
                            *activity = Instant::now();
                            *done = offset + bytes.len() as u64;
                            t.session.feed_file_chunk(&bytes)
                        })
                    } else {
                        match t.source.as_mut() {
                            None => Err("待发文件不可用".to_string()),
                            Some(src) => {
                                use std::io::Seek as _;
                                src.seek(std::io::SeekFrom::Start(offset))
                                    .and_then(|_| read_full(src, &mut buf))
                                    .and_then(|n| {
                                        if n == 0
                                            && t.session
                                                .declared_size()
                                                .is_some_and(|size| offset < size)
                                        {
                                            return Err(std::io::Error::new(
                                                std::io::ErrorKind::UnexpectedEof,
                                                "待发文件提前结束",
                                            ));
                                        }
                                        Ok(t.session.feed_file_chunk(&buf[..n]))
                                    })
                                    .map_err(|e| e.to_string())
                            }
                        }
                    }
                };
                let acts = match result {
                    Ok(acts) => acts,
                    Err(e) => {
                        let error = format!("ZMODEM 读取待发文件失败，传输结果未知：{e}");
                        *tap.failure.lock().unwrap() = Some(error.clone());
                        emit_progress(&app, &session_id, "failed", "", None, 0, None, &error);
                        let _ = tap.cancel();
                        continue;
                    }
                };
                let _ = tap.dispatch(transfer_id, acts);
            }
            effect @ (Effect::Done { .. } | Effect::CancelDone { .. }) => {
                let cancellation_terminal = matches!(effect, Effect::CancelDone { .. });
                let (ok, message) = match effect {
                    Effect::Done { ok, message } => (ok, message),
                    Effect::CancelDone { message } => (false, message),
                    _ => unreachable!(),
                };
                let mut cur = tap.cur.lock().unwrap();
                if transfer_id != tap.transfer_id.load(Ordering::SeqCst)
                    || (tap.cancelling.load(Ordering::SeqCst) && !cancellation_terminal)
                {
                    continue;
                }
                if tap.mcp_busy() {
                    let failure = tap.failure_message();
                    tap.settle_mcp(
                        &state,
                        ok && failure.is_none(),
                        failure.as_deref().unwrap_or(&message),
                    );
                }
                let path = cur.as_mut().and_then(|t| {
                    if let Some(mut sink) = t.sink.take() {
                        if let Err(error) = sink.flush() {
                            *tap.failure.lock().unwrap() =
                                Some(format!("ZMODEM 收尾刷新失败：{error}"));
                        }
                    }
                    t.sink_path.clone()
                });
                *cur = None;
                // 关闭本轮：已排队的迟到 Done/数据不得复活它。
                tap.next_round();
                tap.busy.store(false, Ordering::SeqCst);
                tap.cancelling.store(false, Ordering::SeqCst);
                tap.sniff.lock().unwrap().clear();
                // 复位「已通知」闩：下一次 rz 要能再弹一次选择框
                tap.need_file_announced.store(false, Ordering::SeqCst);
                let failure = tap.failure_message();
                let ok = ok && failure.is_none();
                let message = failure.unwrap_or_else(|| {
                    if ok
                        && !message.contains("未观察到 OO")
                        && !tap.pending_finalize.lock().unwrap().is_empty()
                    {
                        "协议传输完成，文件等待保存".into()
                    } else {
                        message
                    }
                });
                tracing::info!(%session_id, ok, message, "ZMODEM 传输结束");
                emit_progress(
                    &app,
                    &session_id,
                    if ok { "done" } else { "failed" },
                    "",
                    path.as_deref(),
                    0,
                    None,
                    &message,
                );
                drop(cur);
                if !ok {
                    tap.present_pending_download();
                }
            }
        }
    }
}

/// 面向用户显示的路径文本。
///
/// Windows 上 `resolve_within` 走 `canonicalize`，返回的是 verbatim 形式
/// `\\?\C:\Users\...`。那个前缀是给 Win32 API 看的（绕过 260 字符与路径规范化），
/// 直接摆到界面上用户只会以为程序出错了，复制去资源管理器地址栏也不一定认。
/// 落盘仍用原始 `PathBuf`（前缀有其用处，长路径要靠它），只有显示走这里剥掉。
pub(crate) fn display_path(p: &Path) -> String {
    let s = p.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        // UNC 的 verbatim 形式是 `\\?\UNC\server\share`，剥掉前缀后要把
        // `UNC\` 还原成 `\\`，否则得到一个 `UNC\server\share` 的怪路径。
        Some(rest) => match rest.strip_prefix(r"UNC\") {
            Some(unc) => format!(r"\\{unc}"),
            None => rest.to_string(),
        },
        None => s.to_string(),
    }
}

/// 读满一块（`Read::read` 允许短读，直接用会把文件切成大量小子包）。
fn read_full(src: &mut impl std::io::Read, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match src.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

/// 在沙箱的 `.incoming/` 下建一个 spool 文件（两段式下载的第一段）。
///
/// **为什么先落 spool 再归置（2026-08-26 重设计）**：rz/sz 的协议超时是几十秒级，
/// 不能弹框等用户选目录再应答协议帧——用户想一下「存哪」，对端就超时了。
/// 所以协议侧照旧即时收（沙箱恒可写），收完再问；重命名/移动是本地操作，没有时限。
///
/// spool 名是 `recv-<纳秒>.part`：不含对端提供的任何字符（对端名字里的 `..`、
/// 分隔符、Windows 保留名在这里全部失效），真正的文件名在 finalize 时才清洗落定。
fn open_spool(sandbox: &Path) -> Result<(std::fs::File, PathBuf), String> {
    let incoming = sandbox.join(".incoming");
    std::fs::create_dir_all(&incoming).map_err(|e| format!("建暂存目录失败：{e}"))?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let dest = incoming.join(format!("recv-{nanos}.part"));
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&dest)
        .map(|f| (f, dest))
        .map_err(|e| format!("建暂存文件失败：{e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    impl EffectQueue {
        fn test_push(&mut self, round: u64, effect: Effect) -> Result<(), String> {
            self.push(round, effect, &AtomicBool::new(false))
        }
    }

    fn transfer(session: ZmodemSession) -> Transfer {
        Transfer {
            id: 1,
            session,
            sink: None,
            sink_path: None,
            source: None,
            recv_name: None,
            reported: 0,
            pending: Default::default(),
            file_index: 1,
            file_count: 1,
        }
    }

    #[test]
    fn read_pressure_has_two_dimensional_hysteresis_and_keeps_hard_limits() {
        let mut queue = EffectQueue::default();
        for _ in 0..2 {
            for _ in 0..READ_HIGH_EFFECTS {
                queue.test_push(0, Effect::FileSent).unwrap();
            }
            assert!(queue.read_paused);
            for _ in 0..READ_HIGH_EFFECTS - READ_LOW_EFFECTS - 1 {
                queue.pop().unwrap();
            }
            assert!(queue.read_paused);
            queue.pop().unwrap();
            assert!(!queue.read_paused);
            while queue.pop().is_some() {}
        }
        // 字节已低但项数未低，和项数已低但字节未低，两种都不能提前恢复。
        for byte_first in [true, false] {
            queue.reset(0);
            if byte_first {
                queue
                    .test_push(0, Effect::Write(vec![0; READ_HIGH_BYTES]))
                    .unwrap();
            }
            for _ in 0..READ_HIGH_EFFECTS - 1 {
                queue.test_push(0, Effect::FileSent).unwrap();
            }
            if !byte_first {
                queue
                    .test_push(0, Effect::Write(vec![0; READ_HIGH_BYTES]))
                    .unwrap();
            }
            assert!(queue.read_paused);
            for _ in 0..READ_HIGH_EFFECTS - READ_LOW_EFFECTS - 1 {
                queue.pop().unwrap();
            }
            assert!(queue.read_paused);
            queue.pop().unwrap();
            assert_eq!(queue.read_paused, !byte_first);
            while queue.pop().is_some() {}
            assert!(!queue.read_paused);
        }
        queue.reset(0);
        for _ in 0..4 {
            queue
                .test_push(0, Effect::Write(vec![0; 1024 * 1024]))
                .unwrap();
        }
        assert!(queue.read_paused);
        queue.pop().unwrap();
        assert!(queue.read_paused);
        queue.pop().unwrap();
        assert!(!queue.read_paused);
        queue.reset(0);
        assert!(queue
            .test_push(0, Effect::Write(vec![0; MAX_QUEUED_BYTES + 1]))
            .is_err());
        assert!(queue.failed && !queue.read_paused);
        assert!(matches!(queue.pop().unwrap().effect, Effect::Cancel));
        assert!(matches!(
            queue.pop().unwrap().effect,
            Effect::CancelDone { .. }
        ));
    }

    #[tokio::test]
    async fn paused_reader_wakes_on_low_water_cancel_failure_reset_and_close() {
        for mode in 0..6 {
            let channel = Arc::new(EffectChannel::default());
            let receiver = EffectReceiver(channel.clone());
            {
                let mut queue = channel.queue.lock().unwrap();
                for _ in 0..READ_HIGH_EFFECTS {
                    queue.test_push(0, Effect::FileSent).unwrap();
                }
            }
            let ready = channel.read_backpressure().unwrap();
            // 唤醒发生在 notified 注册之前仍须保留；driver 的 ready 不得被读者消费。
            channel.ready.notify_one();
            match mode {
                0 => {
                    for _ in 0..READ_HIGH_EFFECTS - READ_LOW_EFFECTS {
                        receiver.0.recv().await;
                    }
                }
                1 => {
                    channel
                        .queue
                        .lock()
                        .unwrap()
                        .test_push(0, Effect::Cancel)
                        .unwrap();
                }
                2 => {
                    channel
                        .queue
                        .lock()
                        .unwrap()
                        .test_push(
                            0,
                            Effect::CancelDone {
                                message: "cancel".into(),
                            },
                        )
                        .unwrap();
                }
                3 => {
                    channel.queue.lock().unwrap().reset(1);
                }
                4 => {
                    assert!(channel
                        .queue
                        .lock()
                        .unwrap()
                        .test_push(0, Effect::Write(vec![0; MAX_QUEUED_BYTES + 1]))
                        .is_err());
                }
                _ => {
                    channel.queue.lock().unwrap().close();
                }
            }
            tokio::time::timeout(Duration::from_secs(1), ready.notified())
                .await
                .unwrap();
            assert!(channel.read_backpressure().is_none());
            tokio::time::timeout(Duration::from_secs(1), channel.ready.notified())
                .await
                .unwrap();
            // reset 后新轮再暂停；旧轮控制效果不得误解除新轮水位。
            channel.queue.lock().unwrap().reset(9);
            {
                let mut queue = channel.queue.lock().unwrap();
                for _ in 0..READ_HIGH_EFFECTS {
                    if !queue.closed {
                        queue.test_push(9, Effect::FileSent).unwrap();
                    }
                }
                if !queue.closed {
                    queue.test_push(0, Effect::Cancel).unwrap();
                    assert!(queue.read_paused);
                }
            }
            drop(receiver);
            assert!(channel.read_backpressure().is_none());
            tokio::time::timeout(Duration::from_secs(1), channel.read_ready.notified())
                .await
                .unwrap();
        }
    }

    struct BudgetPipeTap {
        channel: Arc<EffectChannel>,
        peak: std::sync::atomic::AtomicUsize,
    }
    impl ByteTap for BudgetPipeTap {
        fn read_backpressure(&self) -> Option<&Notify> {
            self.channel.read_backpressure()
        }
        fn filter<'a>(&self, bytes: &'a [u8]) -> Cow<'a, [u8]> {
            let mut queue = self.channel.queue.lock().unwrap();
            for part in bytes.chunks(1024) {
                queue.test_push(0, Effect::Write(part.to_vec())).unwrap();
            }
            self.peak.fetch_max(queue.data.len(), Ordering::SeqCst);
            self.channel.ready.notify_one();
            Cow::Borrowed(&[])
        }
    }

    #[tokio::test]
    async fn real_pipe_backpressures_four_and_128_mib_without_render_ack() {
        use tokio::io::AsyncWriteExt;
        for total in [4 * 1024 * 1024, 128 * 1024 * 1024] {
            let channel = Arc::new(EffectChannel::default());
            let tap = Arc::new(BudgetPipeTap {
                channel: channel.clone(),
                peak: Default::default(),
            });
            let (mut writer, reader) = tokio::io::duplex(65536);
            let mut pipe = fs_terminal::pipe::SessionPipe::spawn(
                reader,
                fs_terminal::pipe::PipeOpts {
                    grid_rows: 24,
                    grid_cols: 80,
                    scrollback_lines: 10,
                    flow: Default::default(),
                    ring_bytes: 1024,
                    session_log: None,
                    record: None,
                    tap: Some(tap.clone()),
                    decoder: Default::default(),
                },
            );
            let mut rx = pipe.render_rx();
            let producer = tokio::spawn(async move {
                let chunk = vec![7; 16384];
                for _ in 0..total / chunk.len() {
                    writer.write_all(&chunk).await.unwrap();
                }
                writer.shutdown().await.unwrap();
            });
            tokio::time::timeout(Duration::from_secs(2), async {
                while channel.read_backpressure().is_none() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            // 消费者起步前，真正 pipe 已暂停；此后故意逐批让出调度，反复高低水位。
            tokio::time::timeout(Duration::from_secs(10), async {
                let mut received = 0;
                while received < total {
                    match channel.recv().await.effect {
                        Effect::Write(bytes) => {
                            assert!(bytes.iter().all(|b| *b == 7));
                            received += bytes.len();
                        }
                        _ => panic!("normal stream overflowed"),
                    }
                    if received % 16384 == 0 {
                        tokio::task::yield_now().await;
                    }
                }
                producer.await.unwrap();
                assert!(rx.recv().await.is_none());
            })
            .await
            .unwrap();
            assert!(tap.peak.load(Ordering::SeqCst) <= READ_HIGH_EFFECTS + 16);
            assert!(!channel.queue.lock().unwrap().failed);
            assert_eq!(pipe.ring_snapshot_text(), "");
        }
    }

    #[test]
    fn protocol_cancel_blocks_queued_data_and_routes_failure_to_control_slot() {
        use fs_terminal::zmodem::{FrameHeader, FrameKind};
        let mut skip = ZmodemSession::new_receive();
        let skip_actions = skip.feed(
            &FrameHeader {
                kind: FrameKind::ZSKIP,
                flags: [0; 4],
            }
            .to_hex(),
        );
        let mut cache = ZmodemSession::new_receive();
        cache.feed(
            &FrameHeader {
                kind: FrameKind::ZDATA,
                flags: [0; 4],
            }
            .to_bin32(),
        );
        let cache_actions = cache.feed(&vec![b'x'; 1024 * 1024 + 1]);
        let mut close = ZmodemSession::new_send("empty", 0);
        close.finish();
        let close_actions =
            close.expire_close(std::time::Instant::now() + std::time::Duration::from_secs(20));
        for acts in [skip_actions, cache_actions, close_actions] {
            let cancelling = AtomicBool::new(false);
            let mut queue = EffectQueue::default();
            queue.reset(7);
            queue
                .test_push(7, Effect::Write(b"pending".to_vec()))
                .unwrap();
            queue.test_push(7, Effect::Send(b"ZACK".to_vec())).unwrap();
            for action in acts {
                queue
                    .push(7, effect_from_action(action, &cancelling), &cancelling)
                    .unwrap();
            }
            let mut admitted = Vec::new();
            while let Some(queued) = queue.pop() {
                if effect_admitted(&queued.effect, cancelling.load(Ordering::SeqCst), false) {
                    admitted.push(queued.effect);
                }
            }
            assert!(matches!(
                &admitted[..],
                [Effect::Cancel, Effect::CancelDone { .. }]
            ));
            assert!(cancelling.load(Ordering::SeqCst));
        }
    }

    #[test]
    fn stale_protocol_cancel_does_not_set_new_round_cancellation_gate() {
        let cancelling = AtomicBool::new(false);
        let mut queue = EffectQueue::default();
        queue.reset(9);
        for action in [
            Action::Send(fs_terminal::zmodem::cancel_sequence()),
            Action::Done {
                ok: false,
                message: "old".into(),
            },
        ] {
            queue
                .push(7, effect_from_action(action, &cancelling), &cancelling)
                .unwrap();
        }
        assert!(!cancelling.load(Ordering::SeqCst));
        queue
            .push(9, Effect::Send(b"new".to_vec()), &cancelling)
            .unwrap();
        let queued = queue.pop().unwrap();
        assert_eq!(queued.transfer_id, 9);
        assert!(effect_admitted(
            &queued.effect,
            cancelling.load(Ordering::SeqCst),
            false
        ));
        assert!(queue.pop().is_none());
    }

    #[test]
    fn paused_consumer_count_and_payload_overflow_keep_control_reachable() {
        for payload_limit in [false, true] {
            let mut queue = EffectQueue::default();
            queue.reset(7);
            if payload_limit {
                queue
                    .test_push(7, Effect::Write(vec![1; MAX_QUEUED_BYTES]))
                    .unwrap();
            } else {
                for _ in 0..MAX_QUEUED_EFFECTS {
                    queue.test_push(7, Effect::Write(vec![1; 1024])).unwrap();
                }
            }
            assert!(queue
                .test_push(7, Effect::Write(vec![2]))
                .unwrap_err()
                .contains("上限"));
            assert!(queue.failed);
            assert_eq!(queue.bytes, 0);
            assert!(queue.data.is_empty());
            queue.test_push(7, Effect::Cancel).unwrap();
            queue
                .test_push(
                    7,
                    Effect::CancelDone {
                        message: "user cancel".into(),
                    },
                )
                .unwrap();
            assert!(queue
                .test_push(
                    7,
                    Effect::Done {
                        ok: true,
                        message: "late".into()
                    }
                )
                .is_err());
            assert!(matches!(queue.pop().unwrap().effect, Effect::Cancel));
            assert!(matches!(queue.pop().unwrap().effect,
                Effect::CancelDone { message } if message == QUEUE_OVERFLOW));
            assert!(queue.pop().is_none());
            // 消费完控制通知后仍粘滞，不能被新数据重新填充。
            assert!(queue.test_push(7, Effect::FileSent).is_err());
            assert!(queue.pop().is_none());
            queue.reset(9);
            queue.test_push(7, Effect::Cancel).unwrap();
            queue
                .test_push(
                    7,
                    Effect::CancelDone {
                        message: "old".into(),
                    },
                )
                .unwrap();
            queue
                .test_push(
                    7,
                    Effect::Done {
                        ok: true,
                        message: "old".into(),
                    },
                )
                .unwrap();
            queue
                .test_push(
                    9,
                    Effect::Begin {
                        name: "empty".into(),
                        size: Some(0),
                    },
                )
                .unwrap();
            queue.test_push(9, Effect::Finish { bytes: 0 }).unwrap();
            queue
                .test_push(
                    9,
                    Effect::Done {
                        ok: true,
                        message: "new".into(),
                    },
                )
                .unwrap();
            for _ in 0..3 {
                assert_eq!(queue.pop().unwrap().transfer_id, 9);
            }
            assert_eq!(queue.bytes, 0);
            assert!(queue.pop().is_none());
        }
    }

    #[test]
    fn draining_regular_chunks_preserves_empty_and_four_mib_batch_order() {
        let mut queue = EffectQueue::default();
        queue.reset(1);
        let mut written = 0;
        for size in [0, 4 * 1024 * 1024] {
            queue
                .test_push(
                    1,
                    Effect::Begin {
                        name: "batch".into(),
                        size: Some(size),
                    },
                )
                .unwrap();
            assert!(matches!(queue.pop().unwrap().effect, Effect::Begin { .. }));
            // 模拟每次 64 KiB 读块，消费者在下一读块之前可排空。
            for _ in 0..size / 65536 {
                for _ in 0..64 {
                    queue.test_push(1, Effect::Write(vec![1; 1024])).unwrap();
                }
                while let Some(queued) = queue.pop() {
                    if let Effect::Write(data) = queued.effect {
                        written += data.len();
                    } else {
                        panic!("batch write order changed");
                    }
                }
                assert_eq!(queue.bytes, 0);
            }
            queue.test_push(1, Effect::Finish { bytes: size }).unwrap();
            assert!(
                matches!(queue.pop().unwrap().effect, Effect::Finish { bytes } if bytes == size)
            );
        }
        queue
            .test_push(
                1,
                Effect::Done {
                    ok: true,
                    message: "done".into(),
                },
            )
            .unwrap();
        assert!(matches!(
            queue.pop().unwrap().effect,
            Effect::Done { ok: true, .. }
        ));
        assert_eq!(written, 4 * 1024 * 1024);
        assert!(!queue.failed);
        assert!(queue.pop().is_none());
    }

    #[test]
    fn control_payload_is_bounded_at_utf8_boundary() {
        let mut queue = EffectQueue::default();
        queue
            .test_push(
                0,
                Effect::CancelDone {
                    message: "中".repeat(4096),
                },
            )
            .unwrap();
        assert!(
            matches!(queue.pop().unwrap().effect, Effect::CancelDone { message }
            if message.len() <= MAX_CONTROL_MESSAGE_BYTES && message.ends_with('中'))
        );
    }

    #[tokio::test]
    async fn queue_wakes_once_for_overflow_and_receiver_drop_releases_payload() {
        let channel = Arc::new(EffectChannel::default());
        let receiver = EffectReceiver(channel.clone());
        channel.queue.lock().unwrap().reset(3);
        {
            let mut queue = channel.queue.lock().unwrap();
            queue
                .test_push(3, Effect::Write(vec![0; MAX_QUEUED_BYTES]))
                .unwrap();
            assert!(queue.test_push(3, Effect::Write(vec![1])).is_err());
        }
        channel.ready.notify_one();
        assert!(matches!(receiver.0.recv().await.effect, Effect::Cancel));
        assert!(matches!(
            receiver.0.recv().await.effect,
            Effect::CancelDone { .. }
        ));
        // 有限控制槽耗尽后无 ready 风暴；调用真正 recv，验证会休眠。
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), receiver.0.recv())
                .await
                .is_err()
        );
        channel.queue.lock().unwrap().reset(4);
        channel
            .queue
            .lock()
            .unwrap()
            .test_push(4, Effect::Write(vec![1; 100]))
            .unwrap();
        drop(receiver);
        let mut queue = channel.queue.lock().unwrap();
        assert!(queue.data.is_empty());
        assert!(queue
            .test_push(4, Effect::FileSent)
            .unwrap_err()
            .contains("驱动不可用"));
    }

    #[tokio::test]
    async fn blocked_operation_gate_times_out_and_later_close_can_acquire() {
        let gate = Arc::new(tokio::sync::Mutex::new(()));
        let held = gate.clone().lock_owned().await;
        let result = acquire_operation_gate(
            gate.clone(),
            tokio::time::Instant::now() + std::time::Duration::from_millis(20),
        )
        .await;
        assert!(result.unwrap_err().contains("结果未知"));
        drop(held);
        let acquired = acquire_operation_gate(
            gate.clone(),
            tokio::time::Instant::now() + std::time::Duration::from_secs(1),
        )
        .await
        .unwrap();
        drop(acquired);
        assert!(gate.try_lock().is_ok(), "超时任务不得留下操作锁");
    }

    #[test]
    fn cancellation_blocks_queued_file_sent_and_late_success() {
        // 审查反例：FileSent 已排队，用户 cancel 先于驱动消费它。
        let queue = [
            Effect::FileSent,
            Effect::Cancel,
            Effect::CancelDone {
                message: "cancelled".into(),
            },
            Effect::Send(b"ZFIN".to_vec()),
            Effect::Done {
                ok: true,
                message: "success".into(),
            },
        ];
        let admitted: Vec<_> = queue
            .iter()
            .enumerate()
            .filter(|(_, effect)| effect_admitted(effect, true, false))
            .map(|(index, _)| index)
            .collect();
        assert_eq!(admitted, vec![1, 2], "取消后只能发CAN和取消终态");
        assert!(!effect_admitted(
            &Effect::NeedChunk { offset: 0 },
            true,
            false
        ));
        assert!(!effect_admitted(&Effect::Write(vec![1]), true, false));
    }

    #[test]
    fn io_failure_still_allows_cancel_control() {
        assert!(
            effect_admitted(&Effect::Cancel, true, true),
            "文件失败也必须发送CAN"
        );
        assert!(effect_admitted(
            &Effect::CancelDone {
                message: "io failed".into()
            },
            true,
            true
        ));
        assert!(!effect_admitted(
            &Effect::Send(b"data".to_vec()),
            false,
            true
        ));
    }

    #[test]
    fn cancel_after_skip_keeps_queued_failure_admitted() {
        let mut session = ZmodemSession::new_send("protected", 1);
        let skip = fs_terminal::zmodem::FrameHeader {
            kind: fs_terminal::zmodem::FrameKind::ZSKIP,
            flags: [0; 4],
        }
        .to_hex();
        session.feed(&skip);
        let mut cur = Some(transfer(session));
        let waiting = AtomicBool::new(false);
        let cancelling = AtomicBool::new(false);
        assert!(cancellation_actions(&mut cur, &waiting, &cancelling)
            .unwrap()
            .is_empty());
        assert!(!cancelling.load(Ordering::SeqCst));
        assert!(effect_admitted(
            &Effect::Done {
                ok: false,
                message: "skipped".into()
            },
            cancelling.load(Ordering::SeqCst),
            false
        ));
    }

    #[test]
    fn io_failure_after_protocol_finished_can_cancel_once() {
        let mut session = ZmodemSession::new_receive();
        let header = |kind| {
            fs_terminal::zmodem::FrameHeader {
                kind,
                flags: [0; 4],
            }
            .to_hex()
        };
        let mut file = header(fs_terminal::zmodem::FrameKind::ZFILE);
        file.extend(fs_terminal::zmodem::encode_subpacket(
            b"empty\x000 0 0 0",
            fs_terminal::zmodem::SubpacketEnd::AckEnd,
            false,
        ));
        session.feed(&file);
        session.feed(&header(fs_terminal::zmodem::FrameKind::ZEOF));
        let mut end = fs_terminal::zmodem::FrameHeader {
            kind: fs_terminal::zmodem::FrameKind::ZFIN,
            flags: [0; 4],
        }
        .to_hex();
        end.extend_from_slice(b"OO");
        session.feed(&end); // 协议终态先到达，文件效果尚在驱动队列。
        let mut cur = Some(transfer(session));
        let waiting = AtomicBool::new(false);
        let cancelling = AtomicBool::new(false);
        let first = cancellation_actions(&mut cur, &waiting, &cancelling).unwrap();
        assert_eq!(
            first
                .iter()
                .filter(|a| matches!(a, Action::Send(_)))
                .count(),
            1
        );
        assert!(cancellation_actions(&mut cur, &waiting, &cancelling)
            .unwrap()
            .is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn download_finalize_home_does_not_depend_on_app_cwd() {
        let home = tempfile::tempdir().unwrap();
        for _ in 0..2 {
            let cwd = tempfile::tempdir().unwrap();
            let result = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "zmodem_bridge::tests::download_finalize_child",
                    "--nocapture",
                ])
                .env("HOME", home.path())
                .env("FS111_TEST_HOME", home.path())
                .current_dir(cwd.path())
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stdout)
            );
            assert_eq!(std::fs::read_dir(cwd.path()).unwrap().count(), 0);
        }
    }

    #[cfg(unix)]
    #[test]
    fn download_finalize_child() {
        let Some(home) = std::env::var_os("FS111_TEST_HOME") else {
            return;
        };
        let home = PathBuf::from(home).canonicalize().unwrap();
        for raw in ["", "new/deep", "空 格/  spaced  ", "~"] {
            let spool = home.join("fixture.part");
            std::fs::write(&spool, b"HOME payload").unwrap();
            let pending = PendingFinalize {
                decision_id: uuid::Uuid::new_v4().to_string(),
                spool,
                orig_name: "payload.bin".into(),
                bytes: 12,
                presentation: None,
            };
            let id = pending.decision_id.clone();
            let mut slot = std::collections::VecDeque::from([pending]);
            let selected = Mutex::new(None);
            let finalized = Mutex::new(Default::default());
            let (_, path, _) = finalize_pending(
                &mut slot,
                &selected,
                &finalized,
                "s",
                &id,
                SaveChoice {
                    dir: Path::new(raw),
                    name_override: None,
                    policy: "keep-both",
                },
            )
            .unwrap()
            .unwrap();
            let expected = home.join(raw);
            assert_eq!(path.parent().unwrap(), expected);
            assert_eq!(std::fs::read(path).unwrap(), b"HOME payload");
            assert_eq!(*selected.lock().unwrap(), Some(expected.clone()));
            assert_eq!(
                crate::commands::sftp_cmd::resolve_download_dir(raw).unwrap(),
                expected
            );
        }
    }

    #[test]
    fn stale_refresh_replays_the_presented_decision_without_side_effects() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("out");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("B"), b"existing B").unwrap();
        let spool = root.path().join("B.part");
        std::fs::write(&spool, b"received B").unwrap();
        let selected = Mutex::new(Some(dir.clone()));
        let finalized = Mutex::new(Default::default());
        let mut slot = std::collections::VecDeque::from([PendingFinalize {
            decision_id: "B-id".into(),
            spool: spool.clone(),
            orig_name: "B".into(),
            bytes: 10,
            presentation: None,
        }]);
        let original = decision_presentation(
            slot.front_mut().unwrap(),
            "s1",
            &dir,
            "conflict",
            Some(true),
        );
        let forbidden = root.path().join("must-not-create");
        assert!(finalize_pending(
            &mut slot,
            &selected,
            &finalized,
            "s1",
            "stale-A-id",
            SaveChoice {
                dir: &forbidden,
                name_override: None,
                policy: "skip"
            }
        )
        .unwrap()
        .is_none());
        // 与命令 stale 分支的 present_pending_download 使用完全相同的取值函数。
        let replay = pending_presentation(slot.front_mut().unwrap(), "s1", &forbidden);
        assert_eq!(
            replay, original,
            "stale A must replay B's complete conflict presentation"
        );
        assert_eq!(std::fs::read(&spool).unwrap(), b"received B");
        assert_eq!(std::fs::read(dir.join("B")).unwrap(), b"existing B");
        assert_eq!(*selected.lock().unwrap(), Some(dir.clone()));
        assert!(finalized.lock().unwrap().is_empty());
        assert_eq!(slot.len(), 1);
        assert_eq!(slot.front().unwrap().decision_id, "B-id");
        assert!(!forbidden.exists());
        let pending = slot.front_mut().unwrap();
        let first_run = decision_presentation(pending, "s1", &dir, "first-run", None);
        assert_eq!(pending_presentation(pending, "s1", &forbidden), first_run);
        pending.presentation = None;
        let initial = pending_presentation(pending, "s1", &forbidden);
        assert_eq!(initial["message"], "choose-dir");
        assert_eq!(initial["conflict"], serde_json::Value::Null);
        assert_eq!(initial["decisionId"], "B-id");
        assert!(!forbidden.exists());
        assert_eq!(std::fs::read(&spool).unwrap(), b"received B");
        // 显式输出生产 refresh 载荷供前端契约测试读取，不在仓库保存临时数据。
        if let Some(path) = std::env::var_os("FS111_PRESENTATION_FIXTURE") {
            std::fs::write(path, serde_json::to_vec(&serde_json::json!({
                "original": original, "replay": replay, "firstRun": first_run, "initial": initial,
            })).unwrap()).unwrap();
        }
    }

    #[test]
    fn decision_identity_survives_both_finalize_lock_orders() {
        for ipc_first in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let selected = Mutex::new(None);
            let finalized = Mutex::new(Default::default());
            let make = |name: &str| {
                let spool = root.path().join(format!("{name}.part"));
                std::fs::write(&spool, name).unwrap();
                PendingFinalize {
                    decision_id: uuid::Uuid::new_v4().to_string(),
                    spool,
                    orig_name: name.into(),
                    bytes: 1,
                    presentation: None,
                }
            };
            let a = make("A");
            let b = make("B");
            let a_id = a.decision_id.clone();
            let b_id = b.decision_id.clone();
            let b_spool = b.spool.clone();
            let out = root.path().join("out");
            let mut slot = std::collections::VecDeque::from([a]);
            if ipc_first {
                finalize_pending(
                    &mut slot,
                    &selected,
                    &finalized,
                    "s",
                    &a_id,
                    SaveChoice {
                        dir: &out,
                        name_override: None,
                        policy: "keep-both",
                    },
                )
                .unwrap()
                .unwrap();
            }
            retain_received(&mut slot, &selected, &finalized, "s", b, &out).unwrap();
            assert_eq!(slot.front().unwrap().decision_id, b_id);
            std::fs::write(out.join("B"), b"existing B").unwrap();
            // A 的旧选择、重放、未知/旧连接代次 ID 不能改 B 或定格新目录。
            let before = selected.lock().unwrap().clone();
            for stale in [&a_id, "unknown", "old-connection-id"] {
                for policy in ["skip", "overwrite", "keep-both"] {
                    let forbidden = root.path().join("must-not-create");
                    assert!(finalize_pending(
                        &mut slot,
                        &selected,
                        &finalized,
                        "s",
                        stale,
                        SaveChoice {
                            dir: &forbidden,
                            name_override: None,
                            policy
                        }
                    )
                    .unwrap()
                    .is_none());
                    assert!(!forbidden.exists());
                    assert_eq!(*selected.lock().unwrap(), before);
                    assert_eq!(std::fs::read(&b_spool).unwrap(), b"B");
                    assert_eq!(std::fs::read(out.join("B")).unwrap(), b"existing B");
                    assert_eq!(slot.len(), 1);
                }
            }
            let saved = finalize_pending(
                &mut slot,
                &selected,
                &finalized,
                "s",
                &b_id,
                SaveChoice {
                    dir: &out,
                    name_override: None,
                    policy: "keep-both",
                },
            )
            .unwrap()
            .unwrap();
            assert_eq!(std::fs::read(saved.1).unwrap(), b"B");
            assert!(slot.is_empty());
            assert!(finalize_pending(
                &mut slot,
                &selected,
                &finalized,
                "s",
                &b_id,
                SaveChoice {
                    dir: &out,
                    name_override: None,
                    policy: "skip"
                }
            )
            .unwrap()
            .is_none());
        }
    }

    #[test]
    fn failed_finalize_keeps_pending_for_retry() {
        let dir = tempfile::tempdir().unwrap();
        let spool = dir.path().join("spool");
        std::fs::write(&spool, b"received").unwrap();
        let blocked = dir.path().join("blocked");
        std::fs::write(&blocked, b"not a directory").unwrap();
        let mut slot = std::collections::VecDeque::from([PendingFinalize {
            decision_id: uuid::Uuid::new_v4().to_string(),
            spool: spool.clone(),
            orig_name: "payload".into(),
            bytes: 8,
            presentation: None,
        }]);
        let selected = Mutex::new(None);
        let finalized = Mutex::new(Default::default());
        let id = slot.front().unwrap().decision_id.clone();
        assert!(finalize_pending(
            &mut slot,
            &selected,
            &finalized,
            "s",
            &id,
            SaveChoice {
                dir: &blocked,
                name_override: None,
                policy: "keep-both"
            }
        )
        .is_err());
        assert_eq!(slot.front().unwrap().spool, spool);
        assert_eq!(std::fs::read(&spool).unwrap(), b"received");
        assert!(finalized.lock().unwrap().is_empty());
        let destination = dir.path().join("out");
        let saved = finalize_pending(
            &mut slot,
            &selected,
            &finalized,
            "s",
            &id,
            SaveChoice {
                dir: &destination,
                name_override: None,
                policy: "keep-both",
            },
        )
        .unwrap()
        .unwrap();
        assert!(slot.is_empty());
        assert_eq!(std::fs::read(saved.1).unwrap(), b"received");
    }

    #[test]
    fn failed_previous_finalize_preserves_both_received_files_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let blocked = dir.path().join("blocked");
        std::fs::write(&blocked, b"not a directory").unwrap();
        let mut pending = std::collections::VecDeque::new();
        let selected = Mutex::new(None);
        let finalized = Mutex::new(Default::default());
        for (name, data) in [
            ("a.bin", b"first".as_slice()),
            ("b.bin", b"second".as_slice()),
        ] {
            let spool = dir.path().join(format!("{name}.part"));
            std::fs::write(&spool, data).unwrap();
            let result = retain_received(
                &mut pending,
                &selected,
                &finalized,
                "s",
                PendingFinalize {
                    decision_id: uuid::Uuid::new_v4().to_string(),
                    spool,
                    orig_name: name.into(),
                    bytes: data.len() as u64,
                    presentation: None,
                },
                &blocked,
            );
            if name == "a.bin" {
                assert!(result.unwrap().is_none());
            } else {
                assert!(result.is_err());
            }
        }
        let id = pending.front().unwrap().decision_id.clone();
        // B 完成后，A 的 fallback 失败；终结传输也不能删除任一恢复归属。
        assert!(finalize_pending(
            &mut pending,
            &selected,
            &finalized,
            "s",
            &id,
            SaveChoice {
                dir: &blocked,
                name_override: None,
                policy: "keep-both"
            }
        )
        .is_err());
        assert_eq!(pending.len(), 2);
        assert_eq!(
            pending
                .iter()
                .map(|p| (p.orig_name.as_str(), p.bytes))
                .collect::<Vec<_>>(),
            vec![("a.bin", 5), ("b.bin", 6)]
        );
        for (name, data) in [
            ("a.bin", b"first".as_slice()),
            ("b.bin", b"second".as_slice()),
        ] {
            let id = pending.front().unwrap().decision_id.clone();
            let (saved_name, path, bytes) = finalize_pending(
                &mut pending,
                &selected,
                &finalized,
                "s",
                &id,
                SaveChoice {
                    dir: &dir.path().join("out"),
                    name_override: None,
                    policy: "keep-both",
                },
            )
            .unwrap()
            .unwrap();
            assert_eq!(saved_name, name);
            assert_eq!(bytes, data.len() as u64);
            assert_eq!(std::fs::read(path).unwrap(), data);
        }
        assert!(pending.is_empty());
        assert_eq!(finalized.lock().unwrap().len(), 2);
    }

    #[test]
    fn waiting_for_local_file_cancel_sends_can_and_reports_terminal() {
        let mut cur = None;
        let waiting = AtomicBool::new(true);
        let cancelling = AtomicBool::new(false);
        let acts = cancellation_actions(&mut cur, &waiting, &cancelling)
            .expect("waiting rz must be cancellable");
        assert!(acts.iter().any(
            |a| matches!(a, Action::Send(bytes) if bytes == &fs_terminal::zmodem::cancel_sequence())
        ));
        assert!(acts
            .iter()
            .any(|a| matches!(a, Action::Done { ok: false, .. })));
        assert!(waiting.load(Ordering::SeqCst));
        assert!(cancelling.load(Ordering::SeqCst));
        assert!(cancellation_actions(&mut cur, &waiting, &cancelling)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn duplicate_waiting_cancel_is_idempotent() {
        let mut cur = None;
        let waiting = AtomicBool::new(true);
        let cancelling = AtomicBool::new(false);
        assert!(cancellation_actions(&mut cur, &waiting, &cancelling).is_ok());
        assert!(
            cancellation_actions(&mut cur, &waiting, &cancelling).is_ok(),
            "accepted cancellation should remain idempotent until terminal event"
        );
    }

    #[test]
    fn first_download_directory_choice_does_not_deadlock() {
        for setting in [None, Some(PathBuf::from("/tmp/fixture"))] {
            let expected = setting.clone();
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let dir = Mutex::new(None);
                let got = choose_download_dir(&dir, setting);
                tx.send((got, dir.lock().unwrap().clone())).unwrap();
            });
            let (got, stored) = rx
                .recv_timeout(std::time::Duration::from_millis(500))
                .expect("directory choice deadlocked");
            assert_eq!(got, expected);
            assert_eq!(got, stored);
        }
    }

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "fs-zbridge-{tag}-{:?}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// S339：起始帧到方向的映射不能反。
    ///
    /// 搞反的表现很隐蔽：远端 `rz` 在等文件，我们却开了个接收会话，双方各等对方
    /// 先说话，用户看到终端「卡住」直到 rz 超时——既不报错也没有任何提示。
    /// 我在初版实现里就是对两种起始帧一律开接收会话。
    #[test]
    fn start_frame_maps_to_the_right_direction() {
        use fs_terminal::zmodem::FrameKind as K;
        assert_eq!(
            classify_start(K::ZRQINIT),
            Some(Detected::Receive),
            "ZRQINIT = 远端 sz 要发文件 → 我们接收"
        );
        assert_eq!(
            classify_start(K::ZRINIT),
            Some(Detected::NeedLocalFile),
            "ZRINIT = 远端 rz 在等文件 → 要先问人选哪个文件"
        );
        // 其余帧不猜方向
        for k in [K::ZFILE, K::ZDATA, K::ZFIN, K::ZACK, K::ZNAK] {
            assert_eq!(classify_start(k), None, "{k:?} 不该被当作起始帧分类");
        }
    }

    /// S339：`detect_start` 认的帧集合与 `classify_start` 能分类的集合一致。
    ///
    /// 两处一旦分叉，就会出现「检出了但不知道该干什么」的字节——那时透传是唯一
    /// 安全选择，但更该做的是不让它们分叉。
    #[test]
    fn every_detectable_start_frame_is_classifiable() {
        use fs_terminal::zmodem::{FrameHeader, FrameKind as K};
        for k in [K::ZRQINIT, K::ZRINIT] {
            let bytes = FrameHeader {
                kind: k,
                flags: [0; 4],
            }
            .to_hex();
            let (got, _at) = fs_terminal::zmodem::detect_start(&bytes)
                .unwrap_or_else(|| panic!("{k:?} 须被 detect_start 认出"));
            assert!(
                classify_start(got).is_some(),
                "detect_start 认出 {got:?} 却无法分类——两处判定已分叉"
            );
        }
    }

    /// S338（2026-08-26 迁到归置侧）：keep-both 下同名不覆盖，改名避让。
    ///
    /// 覆盖是静默数据丢失：用户传第二个同名文件，第一个就没了，而 UI 上两次都
    /// 显示「成功」。旧 open_sink 的这条判定随逻辑整体迁到 apply_policy。
    #[test]
    fn same_name_does_not_overwrite() {
        let dir = scratch("dup");
        std::fs::write(dir.join("a.txt"), b"original").unwrap();
        let spool = dir.join(".spool-source");
        std::fs::write(&spool, b"new").unwrap();
        let p = apply_policy(&spool, &dir, "a.txt", ConflictPolicy::KeepBoth).unwrap();
        assert_eq!(p.file_name().unwrap().to_str().unwrap(), "a (1).txt");
        assert_eq!(
            std::fs::read(dir.join("a.txt")).unwrap(),
            b"original",
            "原文件内容不得被动过"
        );
        assert_eq!(std::fs::read(&p).unwrap(), b"new", "新内容落到避让名上");

        let spool2 = dir.join(".spool-source2");
        std::fs::write(&spool2, b"new2").unwrap();
        let p2 = apply_policy(&spool2, &dir, "a.txt", ConflictPolicy::KeepBoth).unwrap();
        assert_eq!(p2.file_name().unwrap().to_str().unwrap(), "a (2).txt");
    }

    /// overwrite 真的覆盖：名字不变、内容换新。
    #[test]
    fn overwrite_replaces_content_under_the_same_name() {
        let dir = scratch("ow");
        std::fs::write(dir.join("a.txt"), b"old").unwrap();
        let spool = dir.join(".spool-ow");
        std::fs::write(&spool, b"new").unwrap();
        let p = apply_policy(&spool, &dir, "a.txt", ConflictPolicy::Overwrite).unwrap();
        assert_eq!(
            p.file_name().unwrap().to_str().unwrap(),
            "a.txt",
            "覆盖不改名"
        );
        assert_eq!(std::fs::read(&p).unwrap(), b"new");
        assert!(!spool.exists(), "spool 应已被移走，不得留双份");
    }

    /// 无扩展名与多点名的改名形态（随 S338 迁移）。
    #[test]
    fn rename_keeps_extension_shape() {
        let dir = scratch("ext");
        std::fs::write(dir.join("noext"), b"x").unwrap();
        let spool = dir.join(".spool-a");
        std::fs::write(&spool, b"y").unwrap();
        let p = apply_policy(&spool, &dir, "noext", ConflictPolicy::KeepBoth).unwrap();
        assert_eq!(p.file_name().unwrap().to_str().unwrap(), "noext (1)");

        std::fs::write(dir.join("a.tar.gz"), b"x").unwrap();
        let spool2 = dir.join(".spool-b");
        std::fs::write(&spool2, b"y").unwrap();
        let p2 = apply_policy(&spool2, &dir, "a.tar.gz", ConflictPolicy::KeepBoth).unwrap();
        assert_eq!(
            p2.file_name().unwrap().to_str().unwrap(),
            "a.tar (1).gz",
            "只按最后一个点切分（与资源管理器一致）"
        );
    }

    /// 归置文件名清洗（随 S338 迁移）。
    #[test]
    fn final_name_is_sanitized_and_path_components_rejected() {
        let safe = sanitize_final_name("we:ird?name*.txt").unwrap();
        assert!(
            !safe.contains(':') && !safe.contains('?') && !safe.contains('*'),
            "非法字符须被清洗，实得 {safe:?}"
        );
        // 空名落到占位名，不造隐藏怪文件
        assert_eq!(sanitize_final_name("").unwrap(), "received");
        // **路径成分是拒绝，不是清洗**：清洗一个 ".." 出来一个别的名字，
        // 用户找不到也理解不了他的文件去了哪。
        assert!(sanitize_final_name("..").is_err());
        assert!(sanitize_final_name("a/b.txt").is_err());
        assert!(sanitize_final_name("a\\b.txt").is_err());
    }

    /// S338：面向用户的路径文本不带 `\\?\` 前缀。
    ///
    /// 落盘要用 verbatim 形式（长路径靠它），但把 `\\?\C:\...` 摆到界面上，
    /// 用户只会以为程序出错了，复制去资源管理器也不一定认。
    #[test]
    fn display_path_strips_windows_verbatim_prefix() {
        assert_eq!(
            display_path(Path::new(r"\\?\C:\Users\me\downloads\a.txt")),
            r"C:\Users\me\downloads\a.txt"
        );
        // UNC 的 verbatim 形式要还原成 `\\server\share`，不能剩一个 `UNC\` 前缀
        assert_eq!(
            display_path(Path::new(r"\\?\UNC\server\share\a.txt")),
            r"\\server\share\a.txt"
        );
        // 普通路径原样返回（含 Unix 形态）
        assert_eq!(display_path(Path::new("/home/me/a.txt")), "/home/me/a.txt");
        assert_eq!(display_path(Path::new(r"C:\a\b.txt")), r"C:\a\b.txt");
    }

    /// S338：真实归置路径经过 `display_path` 后不含 verbatim 前缀
    /// （上一条钉的是纯函数，这条钉的是它确实接在了实际路径上）。
    #[test]
    fn real_sink_path_displays_without_prefix() {
        let dir = scratch("disp");
        let spool = dir.join(".spool");
        std::fs::write(&spool, b"x").unwrap();
        let p = apply_policy(&spool, &dir, "shown.txt", ConflictPolicy::KeepBoth).unwrap();
        let shown = display_path(&p);
        assert!(
            !shown.starts_with(r"\\?\"),
            "显示用路径不得带 verbatim 前缀，实得 {shown:?}"
        );
        assert!(shown.ends_with("shown.txt"), "实得 {shown:?}");
    }

    /// **skip 策略：spool 被删、目标目录不出现该文件。**
    ///
    /// 「跳过」的用户语义是「不要这个文件」。把它改成「也落过去」的变异
    /// 必须在这里转红——那等于把用户明确丢弃的东西写进他的下载目录。
    #[test]
    fn skip_deletes_the_spool_and_lands_nothing() {
        let dir = scratch("skip");
        let spool = dir.join(".incoming").join("recv-1.part");
        std::fs::create_dir_all(spool.parent().unwrap()).unwrap();
        std::fs::write(&spool, b"payload").unwrap();
        let sd = Mutex::new(None::<PathBuf>);
        let fin = Mutex::new(std::collections::HashSet::<PathBuf>::new());
        let out = run_finalize(
            &sd,
            &fin,
            "t",
            PendingFinalize {
                decision_id: uuid::Uuid::new_v4().to_string(),
                spool: spool.clone(),
                orig_name: "a.txt".into(),
                bytes: 7,
                presentation: None,
            },
            &dir,
            None,
            "skip",
        )
        .unwrap()
        .expect("skip 也算一次完成（字节计 0）");
        assert_eq!(out.2, 0, "skip 的字节数为 0");
        assert!(!spool.exists(), "spool 应被删除");
        assert!(
            !dir.join("a.txt").exists(),
            "skip 不得把文件落到目标目录——用户明确不要它"
        );
        assert!(
            fin.lock().unwrap().is_empty(),
            "skip 的路径不得进 reveal 白名单（没有这个文件可 reveal）"
        );
    }

    /// spool 名不含对端提供的任何字符：`..`、分隔符、保留名在 spool 阶段全部无效。
    #[test]
    fn spool_name_is_generated_not_derived_from_peer() {
        let dir = scratch("spoolname");
        let (_f, p) = open_spool(&dir).unwrap();
        let name = p.file_name().unwrap().to_str().unwrap();
        assert!(
            name.starts_with("recv-") && name.ends_with(".part"),
            "spool 名应是 recv-<纳秒>.part，实得 {name:?}"
        );
        assert!(
            p.starts_with(dir.join(".incoming")),
            "spool 应落在 .incoming/ 下"
        );
    }

    /// 归置后：目录决定被记住（同会话后续文件不再问去向），
    /// 且最终路径进了 reveal 白名单。
    #[test]
    fn finalize_records_session_dir_and_reveal_whitelist() {
        let dir = scratch("record");
        let spool = dir.join(".spool");
        std::fs::write(&spool, b"x").unwrap();
        let sd = Mutex::new(None::<PathBuf>);
        let fin = Mutex::new(std::collections::HashSet::<PathBuf>::new());
        let (name, path, bytes) = run_finalize(
            &sd,
            &fin,
            "t",
            PendingFinalize {
                decision_id: uuid::Uuid::new_v4().to_string(),
                spool,
                orig_name: "rec.bin".into(),
                bytes: 1,
                presentation: None,
            },
            &dir,
            None,
            "keep-both",
        )
        .unwrap()
        .unwrap();
        assert_eq!(name, "rec.bin");
        assert_eq!(bytes, 1);
        assert_eq!(
            sd.lock().unwrap().as_deref(),
            Some(dir.as_path()),
            "归置目录应被记住为本会话的下载目录"
        );
        assert!(
            fin.lock().unwrap().contains(&path),
            "最终路径应进 reveal 白名单"
        );
    }

    #[test]
    fn read_full_propagates_source_error() {
        struct Broken;
        impl std::io::Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("synthetic read error"))
            }
        }
        assert!(read_full(&mut Broken, &mut [0; 8]).is_err());
    }

    /// S338：`read_full` 补齐短读——否则一次短读就把文件切成大量小子包，
    /// 传输速率会掉一个数量级。
    #[test]
    fn read_full_fills_the_buffer_across_short_reads() {
        let dir = scratch("read");
        let path = dir.join("blob.bin");
        let payload: Vec<u8> = (0u8..=255).cycle().take(4096).collect();
        std::fs::write(&path, &payload).unwrap();
        // BufReader 容量刻意小于目标块，制造短读
        let mut src = std::io::BufReader::with_capacity(7, std::fs::File::open(&path).unwrap());
        let mut buf = vec![0u8; 1024];
        let n = read_full(&mut src, &mut buf).unwrap();
        assert_eq!(n, 1024, "须读满整块（容量 7 的 BufReader 会逐次短读）");
        assert_eq!(&buf[..n], &payload[..1024]);
        // 末块短于目标：返回实际字节数，不得死循环
        let mut src2 = std::io::BufReader::new(std::fs::File::open(&path).unwrap());
        let mut big = vec![0u8; 5000];
        assert_eq!(
            read_full(&mut src2, &mut big).unwrap(),
            4096,
            "文件末尾返回实读字节数"
        );
    }
    fn upload_owner() -> (
        McpOwner,
        tokio::sync::watch::Sender<bool>,
        tempfile::TempDir,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("source");
        std::fs::write(&path, b"data").unwrap();
        let io = McpTransferIo::Upload(
            UploadSource::new(std::fs::File::open(path).unwrap(), "source".into(), 4).unwrap(),
        );
        let (sender, receiver) = tokio::sync::watch::channel(false);
        (
            McpOwner::new(
                McpReservation {
                    handle: "opaque".into(),
                    generation: 7,
                    remote_path: "intent".into(),
                    local_relative_path: Some("source".into()),
                    io,
                },
                receiver,
            ),
            sender,
            dir,
        )
    }

    #[test]
    fn submission_identity_cancel_and_late_failures_cannot_touch_new_owner() {
        let (mut owner, cancel, _dir) = upload_owner();
        assert!(owner.mark_command_attempted("old", 7).is_err());
        assert!(owner.mark_command_attempted("opaque", 6).is_err());
        assert!(!owner.fail_submission("old", 7, "late failure"));
        assert!(!owner.fail_submission("opaque", 6, "late generation"));
        assert!(owner.stop.is_none() && !owner.attempted);
        cancel.send(true).unwrap();
        assert!(owner.mark_command_attempted("opaque", 7).is_err());
        assert!(!owner.attempted);
        assert!(owner.fail_submission("opaque", 7, "write error"));
        assert!(
            owner.stop.as_ref().unwrap().0,
            "cancellation remains sticky"
        );
        let (mut owner, _cancel, _dir2) = upload_owner();
        owner.mark_command_attempted("opaque", 7).unwrap();
        assert!(owner.fail_submission("opaque", 7, "unknown write"));
        assert!(owner.attempted);
        assert!(matches!(
            owner.complete(true, "late success", |_| panic!("no commit")),
            TerminalOutcome::Failed(_)
        ));
    }

    #[test]
    fn mcp_owner_handshake_and_idle_budgets_and_repeated_cancel_are_latched() {
        let (mut owner, sender, _dir) = upload_owner();
        assert!(!owner.attempted && !owner.active);
        assert_eq!(owner.direction(), Direction::Send);
        assert!(!owner.poll_stop(owner.started + Duration::from_secs(14)));
        assert!(owner.poll_stop(owner.started + Duration::from_secs(15)));
        assert!(!owner.poll_stop(owner.started + Duration::from_secs(16)));
        assert!(matches!(
            owner.complete(false, "timeout", |_| panic!("must not commit")),
            TerminalOutcome::Failed(_)
        ));
        let (mut owner, _, _dir2) = upload_owner();
        owner.active = true;
        assert!(!owner.poll_stop(owner.activity + Duration::from_secs(29)));
        assert!(owner.poll_stop(owner.activity + Duration::from_secs(30)));
        let (mut owner, sender2, _dir3) = upload_owner();
        sender2.send(true).unwrap();
        assert!(owner.poll_stop(Instant::now()));
        sender2.send(true).unwrap();
        assert!(!owner.poll_stop(Instant::now()));
        assert!(
            matches!(owner.complete(true, "done", |_| panic!("must not commit")), TerminalOutcome::Cancelled(message) if message.contains("尚未尝试"))
        );
        assert!(!*sender.borrow());
    }

    #[test]
    fn mcp_commit_receipt_wins_cancel_arriving_after_last_watch_check() {
        let (mut owner, sender, _dir) = upload_owner();
        owner.attempted = true;
        let result = owner.complete(true, "done", |_| {
            sender.send(true).unwrap(); // 精确注入：最后 watch 检查后、同步 commit 内。
            Ok(())
        });
        assert!(matches!(result, TerminalOutcome::Completed));
        assert!(owner.stop.is_none());
        let (mut before, sender, _dir2) = upload_owner();
        sender.send(true).unwrap();
        assert!(matches!(
            before.complete(true, "done", |_| panic!("cancel before commit")),
            TerminalOutcome::Cancelled(_)
        ));
    }

    #[test]
    fn mcp_download_uses_strict_session_and_real_spool_commit_without_gui_policy() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let mut sink = DownloadSink::new(&root, "a:b", "a%3Ab").unwrap();
        sink.write_chunk(b"abc").unwrap();
        let (sender, receiver) = tokio::sync::watch::channel(false);
        let mut owner = McpOwner::new(
            McpReservation {
                handle: "download".into(),
                generation: 1,
                remote_path: "a:b".into(),
                local_relative_path: None,
                io: McpTransferIo::Download {
                    basename: "a:b".into(),
                    sink,
                    expected: Some(3),
                },
            },
            receiver,
        );
        assert_eq!(owner.direction(), Direction::Receive);
        let mut session = owner.session().unwrap();
        let wrong = [
            fs_terminal::zmodem::FrameHeader {
                kind: fs_terminal::zmodem::FrameKind::ZFILE,
                flags: [0; 4],
            }
            .to_hex(),
            fs_terminal::zmodem::encode_subpacket(
                b"wrong\0",
                fs_terminal::zmodem::SubpacketEnd::AckEnd,
                false,
            ),
        ]
        .concat();
        assert!(session
            .feed(&wrong)
            .iter()
            .any(|a| matches!(a, Action::Done { ok: false, .. })));
        let result = owner.complete(true, "protocol confirmed", |io| {
            sender.send(true).unwrap(); // Receipt 竞争同时使用真实 finish/硬链/同FD计数。
            match io {
                McpTransferIo::Download { sink, expected, .. } => sink.finish(expected.unwrap()),
                _ => unreachable!(),
            }
        });
        assert!(matches!(result, TerminalOutcome::Completed));
        assert_eq!(std::fs::read(root.join("a%3Ab")).unwrap(), b"abc");
        assert!(owner.stop.is_none());
    }
    #[test]
    fn mcp_passive_probe_preserves_echo_and_buffers_every_header_boundary() {
        let (owner, _sender, _dir) = upload_owner();
        let header = fs_terminal::zmodem::FrameHeader {
            kind: fs_terminal::zmodem::FrameKind::ZRINIT,
            flags: [0; 4],
        }
        .to_hex();
        let all = [b"ordinary echo\r\n".as_slice(), &header].concat();
        for split in 0..=all.len() {
            let mut pending = Vec::new();
            let mut echo = Vec::new();
            let mut protocol = Vec::new();
            for chunk in [&all[..split], &all[split..]] {
                if !protocol.is_empty() {
                    protocol.extend_from_slice(chunk);
                    continue;
                }
                let mut bytes = std::mem::take(&mut pending);
                bytes.extend_from_slice(chunk);
                match owner.probe(bytes, &mut pending) {
                    McpProbe::Echo(bytes) => echo.extend(bytes),
                    McpProbe::Start {
                        echo: bytes,
                        protocol: frame,
                    } => {
                        echo.extend(bytes);
                        protocol = frame;
                    }
                    McpProbe::Reject { reason, .. } => panic!("{reason}"),
                }
            }
            assert_eq!(echo, b"ordinary echo\r\n", "split={split}");
            assert_eq!(
                protocol, header,
                "first handshake cannot leak or disappear split={split}"
            );
            assert!(pending.is_empty());
        }
        let wrong = fs_terminal::zmodem::FrameHeader {
            kind: fs_terminal::zmodem::FrameKind::ZRQINIT,
            flags: [0; 4],
        }
        .to_hex();
        assert!(
            matches!(owner.probe(wrong, &mut Vec::new()), McpProbe::Reject {reason, ..} if reason.contains("方向"))
        );
        assert!(!owner.active && !owner.attempted);
        assert!(
            matches!(owner.probe(b"literal * ordinary".to_vec(), &mut Vec::new()), McpProbe::Echo(bytes) if bytes==b"literal * ordinary")
        );
    }

    #[test]
    fn mcp_driver_drop_closes_original_slot_and_status_reconciles_unknown() {
        let (owner, sender, _dir) = upload_owner();
        let registry = crate::mcp::tasks::TaskRegistry::default();
        registry.register(Task {
            snapshot: Snapshot {
                transfer_id: "opaque".into(),
                session_id: "session".into(),
                direction: "up".into(),
                state: "queued".into(),
                bytes_done: 0,
                bytes_total: Some(4),
                error: None,
                verification: "pending".into(),
                remote_path: Some("intent".into()),
                local_relative_path: None,
                terminal: Some(TerminalMetadata::zmodem(None)),
            },
            profile_id: "profile".into(),
            generation: 7,
            backend: TaskBackend::Zmodem {
                cancel: sender.clone(),
            },
            settled_at: None,
        });
        let old_slot = Arc::new(Mutex::new(Some(owner)));
        let old_effects = Arc::new(EffectChannel::default());
        let guard = McpDriverGuard(
            Arc::downgrade(&old_slot),
            Arc::downgrade(&old_effects),
            Weak::new(),
        );
        let (new, new_sender, _dir2) = upload_owner();
        let new_slot = Arc::new(Mutex::new(Some(new)));
        let new_effects = Arc::new(EffectChannel::default());
        drop(guard);
        assert!(old_slot.lock().unwrap().is_none());
        assert!(
            old_effects.queue.lock().unwrap().closed,
            "reserve checks this exact closed flag"
        );
        assert!(sender.is_closed());
        assert!(new_slot.lock().unwrap().is_some());
        assert!(!new_sender.is_closed());
        assert!(!new_effects.queue.lock().unwrap().closed);
        assert_eq!(
            registry
                .inspect("opaque", |t| t.snapshot.state.clone())
                .unwrap(),
            "failed"
        );
    }

    #[test]
    fn mcp_driver_guard_recovers_poison_without_double_panic() {
        let (mut owner, sender, _dir) = upload_owner();
        owner.pending = b"*".to_vec();
        let slot = Arc::new(Mutex::new(Some(owner)));
        let effects = Arc::new(EffectChannel::default());
        let tail = Arc::new(TailOutput::default());
        let guard = McpDriverGuard(
            Arc::downgrade(&slot),
            Arc::downgrade(&effects),
            Arc::downgrade(&tail),
        );
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _locked = slot.lock().unwrap();
            panic!("controlled owner poison");
        }));
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _locked = effects.queue.lock().unwrap();
            panic!("controlled channel poison");
        }));
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _locked = tail.bytes.lock().unwrap();
            panic!("controlled tail poison");
        }));
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(guard))).is_ok());
        assert_eq!(tail.drain(), b"*");
        assert!(sender.is_closed());
        assert!(
            effects
                .queue
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .closed
        );
    }
    #[test]
    fn mcp_download_driver_drop_preserves_initial_partial_recovery_evidence() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let mut sink = DownloadSink::new(&root, "target", "target").unwrap();
        sink.write_chunk(b"partial").unwrap();
        let actual_partial = sink.partial_path().to_path_buf();
        let (sender, receiver) = tokio::sync::watch::channel(false);
        let owner = McpOwner::new(
            McpReservation {
                handle: "download".into(),
                generation: 1,
                remote_path: "intent".into(),
                local_relative_path: None,
                io: McpTransferIo::Download {
                    basename: "target".into(),
                    sink,
                    expected: None,
                },
            },
            receiver,
        );
        let (local, partial) = owner.paths(); // reserve 实际共用的路径证据构造。
        let registry = crate::mcp::tasks::TaskRegistry::default();
        registry.register(Task {
            snapshot: Snapshot {
                transfer_id: "download".into(),
                session_id: "s".into(),
                direction: "down".into(),
                state: "running".into(),
                bytes_done: 7,
                bytes_total: None,
                error: None,
                verification: "pending".into(),
                remote_path: Some("intent".into()),
                local_relative_path: local,
                terminal: Some(TerminalMetadata::zmodem(partial.clone())),
            },
            profile_id: "p".into(),
            generation: 1,
            backend: TaskBackend::Zmodem { cancel: sender },
            settled_at: None,
        });
        let slot = Arc::new(Mutex::new(Some(owner)));
        let channel = Arc::new(EffectChannel::default());
        drop(McpDriverGuard(
            Arc::downgrade(&slot),
            Arc::downgrade(&channel),
            Weak::new(),
        ));
        let wire = registry
            .inspect("download", |task| {
                serde_json::to_value(&task.snapshot).unwrap()
            })
            .unwrap();
        assert_eq!(wire["state"], "failed");
        assert_eq!(wire["verification"], "unverified");
        assert_eq!(wire["local_relative_path"], "target");
        assert_eq!(wire["partial_relative_path"], partial.unwrap());
        assert_eq!(std::fs::read(actual_partial).unwrap(), b"partial");
        assert!(!root.join("target").exists());
    }

    #[test]
    fn mcp_commit_failure_keeps_known_failure_even_if_cancel_arrives_inside_commit() {
        let (mut owner, sender, _dir) = upload_owner();
        let result = owner.complete(true, "done", |_| {
            sender.send(true).unwrap();
            Err("target may exist; cleanup failed".into())
        });
        assert!(
            matches!(result, TerminalOutcome::Failed(message) if message.contains("cleanup failed"))
        );
    }
    #[test]
    fn mcp_first_matching_handshake_has_one_response_at_every_split() {
        use fs_terminal::zmodem::{parse_header, FrameHeader, FrameKind, Parsed};
        for receive in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().canonicalize().unwrap();
            let request_kind = if receive {
                FrameKind::ZRQINIT
            } else {
                FrameKind::ZRINIT
            };
            let response_kind = if receive {
                FrameKind::ZRINIT
            } else {
                FrameKind::ZFILE
            };
            let header = FrameHeader {
                kind: request_kind,
                flags: [0; 4],
            }
            .to_hex();
            for split in 0..=header.len() {
                let (mut owner, _sender, _source) = upload_owner();
                if receive {
                    owner.request.io = McpTransferIo::Download {
                        basename: "target".into(),
                        sink: DownloadSink::new(&root, "target", "target").unwrap(),
                        expected: None,
                    };
                }
                let mut pending = Vec::new();
                let mut sends = Vec::new();
                let mut active: Option<ZmodemSession> = None;
                for chunk in [&header[..split], &header[split..]] {
                    if let Some(session) = active.as_mut() {
                        sends.extend(session.feed(chunk).into_iter().filter_map(|a| {
                            if let Action::Send(bytes) = a {
                                Some(bytes)
                            } else {
                                None
                            }
                        }));
                        continue;
                    }
                    let mut bytes = std::mem::take(&mut pending);
                    bytes.extend_from_slice(chunk);
                    match owner.probe(bytes, &mut pending) {
                        McpProbe::Echo(echo) => {
                            assert!(echo.is_empty());
                            assert!(sends.is_empty());
                        }
                        McpProbe::Start { echo, protocol } => {
                            assert!(echo.is_empty());
                            let (session, actions) = owner.activate_session(&protocol).unwrap();
                            active = Some(session);
                            sends.extend(actions.into_iter().filter_map(|a| {
                                if let Action::Send(bytes) = a {
                                    Some(bytes)
                                } else {
                                    None
                                }
                            }));
                        }
                        McpProbe::Reject { reason, .. } => panic!("{reason}"),
                    }
                }
                assert_eq!(sends.len(), 1, "receive={receive} split={split}");
                assert!(
                    matches!(parse_header(&sends[0]), Parsed::Header(h, _) if h.kind==response_kind)
                );
            }
        }
    }
    #[test]
    fn mcp_driver_drop_returns_unrecognized_prefix_before_closed_fresh_output() {
        for prefix in [b"*".as_slice(), b"**\x18B0".as_slice()] {
            let (mut owner, _sender, _dir) = upload_owner();
            owner.pending = prefix.to_vec();
            let slot = Arc::new(Mutex::new(Some(owner)));
            let effects = Arc::new(EffectChannel::default());
            let tail = Arc::new(TailOutput::default());
            drop(McpDriverGuard(
                Arc::downgrade(&slot),
                Arc::downgrade(&effects),
                Arc::downgrade(&tail),
            ));
            assert!(effects.queue.lock().unwrap().closed);
            assert_eq!(
                [tail.drain(), b"FRESH".to_vec()].concat(),
                [prefix, b"FRESH"].concat()
            );
            assert!(tail.drain().is_empty());
        }
    }
    struct OwnerTailTap {
        slot: Arc<Mutex<Option<McpOwner>>>,
        tail: Arc<TailOutput>,
    }
    impl ByteTap for OwnerTailTap {
        fn filter<'a>(&self, bytes: &'a [u8]) -> Cow<'a, [u8]> {
            Cow::Borrowed(bytes)
        }
        fn output_ready(&self) -> Option<&Notify> {
            Some(&self.tail.ready)
        }
        fn drain_output(&self, eof: bool) -> Vec<u8> {
            if eof {
                if let Some(owner) = self.slot.lock().unwrap().as_mut() {
                    self.tail.reclaim(owner);
                }
            }
            self.tail.drain()
        }
    }

    #[tokio::test]
    async fn mcp_unrecognized_prefix_cancel_timeout_eof_and_drop_use_original_pipe_once() {
        use tokio::io::AsyncWriteExt;
        for prefix in [b"*".as_slice(), b"**\x18B0".as_slice()] {
            for mode in 0..4 {
                let (mut owner, sender, _dir) = upload_owner();
                owner.pending = prefix.to_vec();
                let slot = Arc::new(Mutex::new(Some(owner)));
                let tail = Arc::new(TailOutput::default());
                let effects = Arc::new(EffectChannel::default());
                if mode == 3 {
                    drop(McpDriverGuard(
                        Arc::downgrade(&slot),
                        Arc::downgrade(&effects),
                        Arc::downgrade(&tail),
                    ));
                    assert!(effects.queue.lock().unwrap().closed);
                } else if mode != 2 {
                    let mut guard = slot.lock().unwrap();
                    let owner = guard.as_mut().unwrap();
                    if mode == 0 {
                        sender.send(true).unwrap();
                    }
                    let now = if mode == 1 {
                        owner.started + Duration::from_secs(15)
                    } else {
                        Instant::now()
                    };
                    assert!(owner.poll_stop(now));
                    tail.reclaim(owner); // stop_mcp 的实际共用发布点，先于 cancelling。
                }
                let tap = Arc::new(OwnerTailTap {
                    slot: slot.clone(),
                    tail: tail.clone(),
                });
                let (mut writer, reader) = tokio::io::duplex(128);
                if mode != 2 {
                    writer.write_all(b"FRESH").await.unwrap();
                }
                writer.shutdown().await.unwrap();
                let mut pipe = fs_terminal::pipe::SessionPipe::spawn(
                    reader,
                    fs_terminal::pipe::PipeOpts {
                        grid_rows: 24,
                        grid_cols: 80,
                        scrollback_lines: 10,
                        flow: Default::default(),
                        ring_bytes: 1024,
                        session_log: None,
                        record: None,
                        tap: Some(tap),
                        decoder: fs_terminal::decode::StreamDecoder::new(None).unwrap(),
                    },
                );
                let mut rendered = pipe.render_rx();
                let mut output = Vec::new();
                while let Some((_, bytes)) =
                    tokio::time::timeout(Duration::from_secs(1), rendered.recv())
                        .await
                        .unwrap()
                {
                    output.extend(bytes);
                }
                let expected = if mode == 2 {
                    prefix.to_vec()
                } else {
                    [prefix, b"FRESH"].concat()
                };
                assert_eq!(output, expected, "mode={mode} prefix={prefix:?}");
                assert_eq!(pipe.ring_snapshot_text().as_bytes(), expected);
                assert!(tail.drain().is_empty());
                if let Some(owner) = slot.lock().unwrap().as_mut() {
                    tail.reclaim(owner);
                }
                assert!(tail.drain().is_empty(), "same round cannot replay prefix");
                let (mut next, _sender2, _dir2) = upload_owner();
                next.pending = b"NEXT".to_vec();
                tail.reclaim(&mut next);
                assert_eq!(tail.drain(), b"NEXT");
            }
        }
    }

    #[test]
    fn mcp_active_protocol_is_not_dumped_and_strict_activation_finishes_one_file() {
        use fs_terminal::zmodem::{encode_subpacket, FrameHeader, FrameKind, SubpacketEnd};
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let (mut owner, _sender, _source) = upload_owner();
        owner.request.io = McpTransferIo::Download {
            basename: "target".into(),
            sink: DownloadSink::new(&root, "target", "target").unwrap(),
            expected: None,
        };
        let init = FrameHeader {
            kind: FrameKind::ZRQINIT,
            flags: [0; 4],
        }
        .to_hex();
        let (mut session, first) = owner.activate_session(&init).unwrap();
        assert_eq!(
            first
                .iter()
                .filter(|a| matches!(a, Action::Send(_)))
                .count(),
            1
        );
        owner.pending = b"recognized binary must stay hidden".to_vec();
        let tail = TailOutput::default();
        tail.reclaim(&mut owner);
        assert!(tail.drain().is_empty());
        let mut protocol = FrameHeader {
            kind: FrameKind::ZFILE,
            flags: [0, 0, 0, 1],
        }
        .to_bin32();
        protocol.extend(encode_subpacket(
            b"target\x003 0 100644",
            SubpacketEnd::AckEnd,
            true,
        ));
        protocol.extend(
            FrameHeader {
                kind: FrameKind::ZDATA,
                flags: [0; 4],
            }
            .to_bin32(),
        );
        protocol.extend(encode_subpacket(b"abc", SubpacketEnd::EndFrame, true));
        protocol.extend(
            FrameHeader {
                kind: FrameKind::ZEOF,
                flags: 3u32.to_le_bytes(),
            }
            .to_hex(),
        );
        protocol.extend(
            FrameHeader {
                kind: FrameKind::ZFIN,
                flags: [0; 4],
            }
            .to_hex(),
        );
        protocol.extend(b"OO");
        let actions = session.feed(&protocol);
        assert_eq!(
            actions
                .iter()
                .filter(|a| matches!(a, Action::BeginRecv(_)))
                .count(),
            1
        );
        assert_eq!(
            actions
                .iter()
                .filter(|a| matches!(a, Action::FinishRecv { bytes: 3 }))
                .count(),
            1
        );
        assert!(actions
            .iter()
            .any(|a| matches!(a, Action::Done { ok: true, .. })));
    }
    #[tokio::test]
    async fn busy_gate_rechecks_owner_reserved_after_precheck_before_acquisition() {
        use std::sync::atomic::AtomicBool;
        let gate = Arc::new(tokio::sync::Mutex::new(()));
        let reserved = Arc::new(AtomicBool::new(false));
        let prior = gate.clone().lock_owned().await;
        let checked = Arc::new(Notify::new());
        let waiter = tokio::spawn({
            let gate = gate.clone();
            let reserved = reserved.clone();
            let checked = checked.clone();
            async move {
                idle_operation_gate(gate, || {
                    checked.notify_one();
                    reserved.load(Ordering::SeqCst)
                })
                .await
            }
        });
        checked.notified().await; // actual helper 的前检已完成，但旧长操作仍持 gate。
        reserved.store(true, Ordering::SeqCst); // 先前持 gate 的提交在释放前原子登记预约。
        drop(prior);
        let result = waiter.await.unwrap();
        assert!(matches!(result, Err(ref error) if error == fs_mcpbridge::ports::SESSION_BUSY));
    }
    #[tokio::test]
    async fn busy_gate_fast_rejection_writes_nothing_and_other_session_remains_usable() {
        let blocked = Arc::new(tokio::sync::Mutex::new(()));
        let _holder = blocked.clone().lock_owned().await;
        let writes = AtomicU64::new(0);
        let result = tokio::time::timeout(Duration::from_millis(100), async {
            let _guard = idle_operation_gate(blocked, || true).await?;
            writes.fetch_add(1, Ordering::SeqCst);
            Ok::<_, String>(())
        })
        .await
        .unwrap();
        assert!(matches!(result,Err(error) if error==fs_mcpbridge::ports::SESSION_BUSY));
        assert_eq!(writes.load(Ordering::SeqCst), 0);
        let other = Arc::new(tokio::sync::Mutex::new(()));
        let _usable = idle_operation_gate(other, || false).await.unwrap();
    }

    #[tokio::test]
    async fn busy_reservation_waits_for_prior_long_operation_gate() {
        let gate = Arc::new(tokio::sync::Mutex::new(()));
        let ordinary = idle_operation_gate(gate.clone(), || false).await.unwrap();
        let reserved = Arc::new(AtomicBool::new(false));
        let started = Arc::new(Notify::new());
        let mut reserve = tokio::spawn({
            let gate = gate.clone();
            let reserved = reserved.clone();
            let started = started.clone();
            async move {
                started.notify_one();
                let guard = gate.lock_owned().await; // reserve_mcp 实际使用同一原 gate。
                reserved.store(true, Ordering::SeqCst);
                guard
            }
        });
        started.notified().await;
        assert!(
            tokio::time::timeout(Duration::from_millis(30), &mut reserve)
                .await
                .is_err()
        );
        assert!(!reserved.load(Ordering::SeqCst));
        drop(ordinary);
        let reservation = reserve.await.unwrap();
        assert!(reserved.load(Ordering::SeqCst));
        let result = idle_operation_gate(gate, || reserved.load(Ordering::SeqCst)).await;
        assert!(matches!(result,Err(error) if error==fs_mcpbridge::ports::SESSION_BUSY));
        drop(reservation);
    }
    #[test]
    fn busy_query_does_not_wait_for_owner_io_and_recovers_empty_poison() {
        let (owner, _sender, _dir) = upload_owner();
        let slot = Mutex::new(Some(owner));
        let held = slot.lock().unwrap();
        assert!(
            owner_slot_busy(&slot),
            "same-thread held owner must not deadlock on busy query"
        );
        drop(held);
        assert!(owner_slot_busy(&slot));
        slot.lock().unwrap().take();
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _held = slot.lock().unwrap();
            panic!("controlled empty slot poison");
        }));
        assert!(
            !owner_slot_busy(&slot),
            "closed driver empty poison must not block ordinary input forever"
        );
    }
}

// ───────────────── 下载两段式：归置决策与执行（2026-08-26 重设计） ─────────────────

/// 下载去向的出厂建议：系统下载目录下的 FutureShell 子目录；取不到下载目录
/// （极少见的无 HOME 环境）回落本会话沙箱——那里**一定**可写，因为协议侧刚刚
/// 就是在它里面收的。
fn default_download_dir(sandbox: &Path) -> PathBuf {
    dirs::download_dir()
        .map(|d| d.join("FutureShell"))
        .unwrap_or_else(|| sandbox.to_path_buf())
}

/// 生成并记住用户已经看到的语义；所有决策展示共用此入口。
fn decision_presentation(
    pending: &mut PendingFinalize,
    session_id: &str,
    dir: &Path,
    reason: &str,
    conflict: Option<bool>,
) -> serde_json::Value {
    let name = fs_sshengine::sandbox::escape_local_component(&pending.orig_name);
    let name = if name.trim().is_empty() {
        "received"
    } else {
        &name
    };
    let payload = serde_json::json!({
        "sessionId": session_id, "phase": "needs-decision", "name": name,
        "path": null, "done": pending.bytes, "total": pending.bytes,
        "message": reason, "file_index": null, "file_count": null,
        "suggestedDir": display_path(dir), "spoolPath": display_path(&pending.spool),
        "conflict": conflict, "decisionId": pending.decision_id,
    });
    pending.presentation = Some(payload.clone());
    payload
}

/// 沿用唯一事件发送点，保持事件契约扫描与其他进度事件一致。
fn emit_decision_presentation(app: &AppHandle, payload: &serde_json::Value) {
    emit_progress_full(
        app,
        payload["sessionId"].as_str().unwrap(),
        "needs-decision",
        payload["name"].as_str().unwrap(),
        None,
        payload["done"].as_u64().unwrap(),
        payload["total"].as_u64(),
        payload["message"].as_str().unwrap(),
        None,
        None,
        payload["suggestedDir"].as_str(),
        payload["spoolPath"].as_str(),
        payload["conflict"].as_bool(),
        payload["decisionId"].as_str(),
    );
}

/// 恢复不重新决定去向、检测冲突或落盘；未呈现过的项只提示选择目录。
fn pending_presentation(
    pending: &mut PendingFinalize,
    session_id: &str,
    initial_dir: &Path,
) -> serde_json::Value {
    if let Some(payload) = &pending.presentation {
        return payload.clone();
    }
    decision_presentation(pending, session_id, initial_dir, "choose-dir", None)
}

fn choose_download_dir(
    session_dir: &Mutex<Option<PathBuf>>,
    from_setting: Option<PathBuf>,
) -> Option<PathBuf> {
    let mut selected = session_dir.lock().unwrap();
    if selected.is_none() {
        *selected = from_setting;
    }
    selected.clone()
}

/// 收完一个文件后的四路归置决策。
///
/// 优先级：**本会话已定的目录 > 设置（fixed） > 引导/询问**。会话目录在第一次
/// finalize 时定格——一次 `sz` 批量传多个文件，第二个起不再问去向（冲突仍逐个问）。
/// 冲突判定在「要不要问」之后：fixed/会话目录且无冲突 → 直接落，不打扰。
async fn decide_and_maybe_finalize(
    app: &AppHandle,
    state: &std::sync::Arc<crate::state::AppState>,
    tap: &ZmodemTap,
    session_id: &str,
) {
    let cfg = state.zmodem_config().await;
    // 优先级 ①②：本会话已定 > 设置的 fixed 模式。
    // 读取和定格在同一把锁内完成，不能在 match 临时守卫存活时再次取锁。
    let from_setting = match cfg
        .download
        .as_ref()
        .filter(|d| d.mode == crate::state::ZmodemDownloadMode::Fixed)
    {
        Some(d) => match crate::commands::sftp_cmd::resolve_download_dir(&d.dir) {
            Ok(dir) => Some(dir),
            Err(error) => {
                emit_progress(app, session_id, "failed", "", None, 0, None, &error);
                tap.present_pending_download();
                return;
            }
        },
        None => None,
    };
    let dir = choose_download_dir(&tap.session_download_dir, from_setting);

    let mut pending_guard = tap.pending_finalize.lock().unwrap();
    let Some(pending) = pending_guard.front_mut() else {
        return;
    };
    // 展示名沿用旧避让逻辑的清洗（不改变盘上事实，只让文案里不出现裸控制字符）
    let display_name = fs_sshengine::sandbox::escape_local_component(&pending.orig_name);
    let display_name = if display_name.trim().is_empty() {
        "received".to_string()
    } else {
        display_name
    };

    let Some(dir) = dir else {
        // 优先级 ③：没有去向决定——首次（无设置）或每次询问。
        let reason = if cfg.download.is_none() {
            "first-run"
        } else {
            "choose-dir"
        };
        let suggested = default_download_dir(&tap.sandbox);
        let payload = decision_presentation(pending, session_id, &suggested, reason, None);
        emit_decision_presentation(app, &payload);
        return;
    };

    // 有去向：看冲突。目录本身不存在不打断——归置时建（用户挑过的目录可能
    // 只是还没建出来，比如 Downloads/FutureShell 首次使用）。
    let target = dir.join(&display_name);
    if !target.exists() {
        // 无冲突：身份随调用传递，释放展示锁后也不消费别项。
        let pending = pending.clone();
        drop(pending_guard);
        let (name, path, bytes) = match finalize_download(
            tap,
            session_id,
            &pending.decision_id,
            &dir,
            None,
            "keep-both",
        ) {
            Ok(Some((n, p, b))) => (n, p, b),
            Ok(None) => return,
            Err(e) => {
                tracing::warn!(%session_id, error = %e, "ZMODEM 直接归置失败");
                emit_progress(app, session_id, "failed", &display_name, None, 0, None, &e);
                let _ = tap.cancel();
                return;
            }
        };
        emit_progress_full(
            app,
            session_id,
            "saved",
            &name,
            Some(&path),
            bytes,
            Some(bytes),
            "",
            None,
            None,
            None,
            None,
            None,
            Some(&pending.decision_id),
        );
        return;
    }

    // 冲突：问。带上建议目录与冲突标记；spool 路径让前端在「关闭=兜底归置」时回传场景使用。
    let payload = decision_presentation(pending, session_id, &dir, "conflict", Some(true));
    emit_decision_presentation(app, &payload);
}

/// 归置的冲突策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictPolicy {
    Overwrite,
    KeepBoth,
    Skip,
}

impl ConflictPolicy {
    fn parse(s: &str) -> Result<Self, String> {
        match s {
            "overwrite" => Ok(Self::Overwrite),
            "keep-both" => Ok(Self::KeepBoth),
            "skip" => Ok(Self::Skip),
            other => Err(format!(
                "未知的冲突处理方式 {other:?}（应为 overwrite/keep-both/skip）"
            )),
        }
    }
}

/// 执行一次归置。消费 `pending_finalize`；返回 `Ok(Some((名字, 最终路径, 字节数)))`。
/// `Ok(None)` = 没有待归置的下载（不是错误——前端重复确认时会发生）。
///
/// 这是 `zmodem_finalize` 命令与「无冲突直接落」共用的唯一实现：两份归置逻辑
/// 分叉的那一侧，就是能把文件挪到任意地方的那一侧。
fn finalize_download(
    tap: &ZmodemTap,
    session_id: &str,
    expected_decision_id: &str,
    dir: &Path,
    name_override: Option<&str>,
    policy: &str,
) -> Result<Option<(String, PathBuf, u64)>, String> {
    let mut slot = tap.pending_finalize.lock().unwrap();
    finalize_pending(
        &mut slot,
        &tap.session_download_dir,
        &tap.finalized,
        session_id,
        expected_decision_id,
        SaveChoice {
            dir,
            name_override,
            policy,
        },
    )
    .inspect_err(|error| *tap.failure.lock().unwrap() = Some(error.clone()))
}

/// 驱动串行调用；第二项归置失败即取消，后续 Finish 不再准入，最多两项。
fn retain_received(
    slot: &mut std::collections::VecDeque<PendingFinalize>,
    session_dir: &Mutex<Option<PathBuf>>,
    finalized: &Mutex<std::collections::HashSet<PathBuf>>,
    session_id: &str,
    received: PendingFinalize,
    fallback_dir: &Path,
) -> Result<Option<(String, PathBuf, u64, String)>, String> {
    let previous = slot.front().map(|p| p.decision_id.clone());
    slot.push_back(received);
    if let Some(id) = previous {
        finalize_pending(
            slot,
            session_dir,
            finalized,
            session_id,
            &id,
            SaveChoice {
                dir: fallback_dir,
                name_override: None,
                policy: "keep-both",
            },
        )
        .map(|result| result.map(|(n, p, b)| (n, p, b, id)))
    } else {
        Ok(None)
    }
}

struct SaveChoice<'a> {
    dir: &'a Path,
    name_override: Option<&'a str>,
    policy: &'a str,
}

fn finalize_pending(
    slot: &mut std::collections::VecDeque<PendingFinalize>,
    session_dir: &Mutex<Option<PathBuf>>,
    finalized: &Mutex<std::collections::HashSet<PathBuf>>,
    session_id: &str,
    expected_decision_id: &str,
    decision: SaveChoice<'_>,
) -> Result<Option<(String, PathBuf, u64)>, String> {
    let SaveChoice {
        dir,
        name_override,
        policy,
    } = decision;
    let Some(pending) = slot.front() else {
        return Ok(None);
    };
    if pending.decision_id != expected_decision_id {
        return Ok(None);
    }
    let dir = crate::commands::sftp_cmd::resolve_download_dir(&dir.to_string_lossy())?;
    let result = run_finalize(
        session_dir,
        finalized,
        session_id,
        pending.clone(),
        &dir,
        name_override,
        policy,
    )?;
    slot.pop_front();
    Ok(result)
}

/// [`finalize_download`] 的核心：归置一个**显式给定**的待决下载。
/// 单独存在是因为「上一个决策未答、下一个文件已收完」的兜底路径手里已经有
/// PendingFinalize，不能再走 take（那会拿到刚放进去的新值或 None）。
///
/// 收锁引用而不是 `&ZmodemTap`：这样单测能直接构造两个 `Mutex` 把 skip/keep-both
/// 全路径钉住，不必起一个真 Tauri 应用。
fn run_finalize(
    session_dir: &Mutex<Option<PathBuf>>,
    finalized: &Mutex<std::collections::HashSet<PathBuf>>,
    session_id: &str,
    pending: PendingFinalize,
    dir: &Path,
    name_override: Option<&str>,
    policy: &str,
) -> Result<Option<(String, PathBuf, u64)>, String> {
    let policy = ConflictPolicy::parse(policy)?;
    // 目录不存在则建（出厂建议 Downloads/FutureShell 首次使用时还没有）；
    // 建完再验「确实是目录」——建出来的是目录，已存在的是文件则拒绝。
    // 不限制在浏览根内：下载去向是用户的自由，与上传源（必须在家目录内）
    // 方向相反；但名字要清洗且拒绝任何路径成分。
    std::fs::create_dir_all(dir).map_err(|e| format!("建目标目录失败：{e}"))?;
    let meta = std::fs::metadata(dir).map_err(|e| format!("读不到目标目录：{e}"))?;
    if !meta.is_dir() {
        return Err(format!("{} 不是目录", dir.display()));
    }
    let raw_name = name_override
        .map(str::to_string)
        .unwrap_or(pending.orig_name);
    let safe = sanitize_final_name(&raw_name)?;

    // 记住本会话的目录决定（后续文件不再问去向）
    *session_dir.lock().unwrap() = Some(dir.to_path_buf());

    match policy {
        ConflictPolicy::Skip => {
            // 跳过 = 删 spool。用户明确不要这个文件；留着只是 .incoming 里的垃圾。
            std::fs::remove_file(&pending.spool).map_err(|e| format!("清理暂存失败：{e}"))?;
            tracing::info!(%session_id, name = %safe, "ZMODEM 下载被跳过，暂存已清理");
            let p = dir.join(&safe);
            Ok(Some((safe, p, 0)))
        }
        ConflictPolicy::Overwrite | ConflictPolicy::KeepBoth => {
            let dest = apply_policy(&pending.spool, dir, &safe, policy)?;
            finalized.lock().unwrap().insert(dest.clone());
            let final_name = dest
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(&safe)
                .to_string();
            Ok(Some((final_name, dest, pending.bytes)))
        }
    }
}

/// 最终文件名的清洗与路径成分拒绝（纯函数，供单测钉）。
///
/// ① 拒绝任何含分隔符或「..」的名字——escape_local_component 管非法字符，
///    管不了把 ".." 当整个名字用的写法；
/// ② 清洗 Windows 非法字符与控制字符；
/// ③ 空/纯点名字落到占位名 `received`。
fn sanitize_final_name(raw: &str) -> Result<String, String> {
    if raw.contains('/') || raw.contains('\\') || raw == ".." {
        return Err(format!("文件名 {raw:?} 含路径成分，已拒绝"));
    }
    let safe = fs_sshengine::sandbox::escape_local_component(raw);
    Ok(if safe.trim().is_empty() {
        "received".to_string()
    } else {
        safe
    })
}

/// 按冲突策略把 spool 落到 `dir/safe`（纯文件操作，供单测钉）。
fn apply_policy(
    spool: &Path,
    dir: &Path,
    safe: &str,
    policy: ConflictPolicy,
) -> Result<PathBuf, String> {
    match policy {
        ConflictPolicy::Overwrite => {
            let dest = dir.join(safe);
            if dest.exists() {
                std::fs::remove_file(&dest).map_err(|e| format!("移除旧文件失败：{e}"))?;
            }
            move_spool(spool, &dest)?;
            Ok(dest)
        }
        // 同名避让：沿用旧 open_sink 的 (n) 序列。create_new 不可用（目标在
        // 沙箱外），用 exists 探测——这里没有并发写者（同一会话串行归置）。
        ConflictPolicy::KeepBoth => {
            let (stem, ext) = match safe.rsplit_once('.') {
                Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
                _ => (safe.to_string(), String::new()),
            };
            for n in 0..1000u32 {
                let candidate = if n == 0 {
                    format!("{stem}{ext}")
                } else {
                    format!("{stem} ({n}){ext}")
                };
                let dest = dir.join(&candidate);
                if dest.exists() {
                    continue;
                }
                move_spool(spool, &dest)?;
                return Ok(dest);
            }
            Err("同名文件过多（已试 1000 个候选名）".into())
        }
        ConflictPolicy::Skip => unreachable!("skip 在调用方分支处理，不落文件"),
    }
}

/// 移动 spool 到最终位置：先试 rename（同盘零拷贝），失败（跨盘/权限）回落
/// copy + delete。copy 的删除在 copy 成功之后——半路失败时宁可留双份也不留零份。
fn move_spool(spool: &Path, dest: &Path) -> Result<(), String> {
    if std::fs::rename(spool, dest).is_ok() {
        return Ok(());
    }
    std::fs::copy(spool, dest).map_err(|e| format!("复制到 {} 失败：{e}", dest.display()))?;
    let _ = std::fs::remove_file(spool);
    Ok(())
}
