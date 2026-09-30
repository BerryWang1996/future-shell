use fs_terminal::pipe::SessionPipe;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter};
use tokio::sync::mpsc;

pub struct LiveSession {
    pub pipe: Arc<SessionPipe>,
    /// 写半部：data_bytes / window_change / close。russh 0.62 的 Channel 非 Clone，
    /// split() 后写半部入注册表；Mutex 串行化保证 term_input 字节顺序（F5）。
    pub write: tokio::sync::Mutex<russh::ChannelWriteHalf<russh::client::Msg>>,
    /// 连接句柄（方案甲单存——P2-15/30）：russh 0.62.4 的 client::Handle **非 Clone**，
    /// 不能在注册表外另设句柄表双存。Task 21 SFTP 经 `registry.get(session_id)` 取本结构，
    /// `handle.lock().await.channel_open_session()`（&self 共享引用 API）开 SFTP 通道后
    /// 即释放锁；Mutex 兼保 Send+Sync。
    // S289：Task 19 内尚无读取方（Task 21 SFTP 消费），binary crate dead_code 需抑制（同 S248 口径）
    #[allow(dead_code)]
    pub handle: tokio::sync::Mutex<russh::client::Handle<fs_sshengine::connect::Connector>>,
    pub profile_id: String,
    /// per-target 传输锁的端点键（`host:port`，见 `state::transfer_endpoint_of`）。
    ///
    /// 在**装配时**定格而不是提交传输时回查数据库：profile 是可改的，连接存活期间用户改掉
    /// host，回查就会让同一条连接上的前后两件传输拿到两把锁。连接是按哪个 host:port 建起来的，
    /// 锁就按哪个走——这是唯一与「文件实际落在哪台机器上」严格对齐的事实。
    pub target_endpoint: String,
    /// 与 `target_endpoint` 同指一台服务器的其它端点拼法（审计2 #13）。
    ///
    /// 与 `target_endpoint` 一样在装配时定格，理由也一样：解析结果会变（DNS 轮询、
    /// 记录更新），而这条连接的字节一直落在同一台机器上。装配时解析一次，
    /// 之后所有传输共用同一份别名集——否则同一条连接上的前后两件传输可能各拿到一份
    /// 不同的别名集，交集不含地址维，退化回本条审计要修的缺陷。
    ///
    /// 见 `state::transfer_endpoint_aliases_of`：走跳板或解析失败时是空 `Vec`。
    pub endpoint_aliases: Vec<String>,
    /// 重连/等待取消信号：session_close 与「停止重连」置 true（F20③）。
    pub stop: tokio::sync::watch::Sender<bool>,
    /// per-session 重连锁（P2-26）：断线 watchdog 与手动 session_reconnect 以 CAS 互斥——
    /// true = 有重连循环在跑；`reconnect_loop` 的 Drop 守卫保证任一退出路径释放。
    pub reconnecting: std::sync::atomic::AtomicBool,
    /// 首断原因缓存（补丁 M-4）：watchdog 启动自动重连前置入；手动 `session_reconnect`（Task 22）
    /// 读取复用（None 时按 TransportEof）。std Mutex：持锁仅取值、无 await。
    pub last_reason: std::sync::Mutex<Option<DisconnectReason>>,
}

/// 进程级单调「连接代次」计数器（审计 P0-4）。
///
/// 会话 id 在重连前后**不变**（标签、日志、前端状态都靠它串起来），可它背后的 SSH 连接已经
/// 换了一条。SFTP 通道与传输管理器都是长在具体连接上的对象，按 session_id 缓存等于把它们
/// 钉死在首次连接上：重连成功后终端好端端地在跑，SFTP 却还在往一条已经断掉的 russh Handle
/// 上发包，用户看到的是「终端能用、文件列表转圈超时」这种极难自证的故障。
///
/// 代次即「这是该 session_id 的第几条连接」，由注册表在 `insert` 时分配——insert 正是连接
/// 被替换的那一刻，语义与事实严格对齐，调用方无从遗漏也无从伪造。
/// 从 1 起：0 留作「非法/未赋值」哨兵。
static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);

/// 注册表条目：会话本体 + 其所属连接代次。
///
/// 代次刻意放在**注册表**而非 `LiveSession` 里：`LiveSession` 由 `session_cmd` 以结构体字面量
/// 构造，把代次做成字段就等于要求每个构造点自己去取号——取错（复用旧号）或忘取都不会被编译器
/// 拦住，而这两种错都直接退化成本条审计要修的缺陷本身。
pub struct RegistryEntry<S = LiveSession> {
    pub generation: u64,
    pub session: Arc<S>,
}

/// 会话注册表。类型参数 `S` 默认就是 `LiveSession`，产品代码里写 `SessionRegistry` 即可，
/// 与泛型化之前逐字相同。
///
/// 之所以泛型化：代次分配是审计 P0-4 的机制本体（「重连必得新号」是所有 per-代次 缓存失效的
/// 唯一依据），而它此前零测试覆盖——原因不是没人想测，是 `LiveSession` 里躺着一个 russh
/// `Handle` 和一个 `ChannelWriteHalf`，两者都只能由一次真实的 SSH 握手产出，单测里造不出来。
/// 加一个默认类型参数让测试可以拿任意占位类型登记，代次逻辑本身与会话内容无关，
/// 这正是它该被单独钉住的理由。
pub struct SessionRegistry<S = LiveSession> {
    pub sessions: Mutex<HashMap<String, RegistryEntry<S>>>,
}

// 手写而非 `#[derive(Default)]`：derive 会给 `S` 加上 `S: Default` 约束，而 `LiveSession`
// 不可能实现 Default（它持有真实连接）。注册表的默认值只是一张空表，与 `S` 无关。
impl<S> Default for SessionRegistry<S> {
    fn default() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
        }
    }
}

impl<S> SessionRegistry<S> {
    /// 登记会话并**分配新代次**。重连路径（`session_cmd` 的「remove 旧 + insert 新」）由此
    /// 自动获得一个新号，所有按 (session_id, generation) 缓存的子系统随之整体失效。
    pub fn insert(&self, id: String, s: Arc<S>) -> u64 {
        let generation = NEXT_GENERATION.fetch_add(1, Ordering::SeqCst);
        self.sessions.lock().unwrap().insert(
            id,
            RegistryEntry {
                generation,
                session: s,
            },
        );
        generation
    }
    pub fn get(&self, id: &str) -> Option<Arc<S>> {
        self.sessions
            .lock()
            .unwrap()
            .get(id)
            .map(|e| e.session.clone())
    }
    /// 会话 + 代次一并取出。子系统装配必须走这个入口：分两次取（先 `generation_of` 再 `get`）
    /// 会在两次取之间被重连插队，拿到「新代次 + 旧连接」的错配组合。
    pub fn get_with_generation(&self, id: &str) -> Option<(u64, Arc<S>)> {
        self.sessions
            .lock()
            .unwrap()
            .get(id)
            .map(|e| (e.generation, e.session.clone()))
    }
    /// 仅取当前代次（缓存查表用，不需要会话本体时避免多克隆一个 Arc）。
    pub fn generation_of(&self, id: &str) -> Option<u64> {
        self.sessions.lock().unwrap().get(id).map(|e| e.generation)
    }
    pub fn remove(&self, id: &str) -> Option<Arc<S>> {
        self.sessions.lock().unwrap().remove(id).map(|e| e.session)
    }

    /// 按谓词找一个会话，**代次最大者优先**（= 最近建立的那条连接）。
    ///
    /// 计划任务要「在某个连接上执行命令」，而同一个档案可能同时开着好几个会话。
    /// 直接取 HashMap 的第一个匹配项是不可复现的：迭代序随哈希种子变，同一份任务
    /// 在两次运行里可能落到不同会话上，而排查「命令跑到哪去了」时这种不确定性最要命。
    ///
    /// 取代次最大者而不是按 id 排序：代次即「这是第几条连接」，最大者就是最新建立的
    /// 那条，也最可能是活着的（旧的那条可能正处在断线待重连的窗口里）。
    pub fn find_latest_by<F: Fn(&S) -> bool>(&self, pred: F) -> Option<(String, Arc<S>)> {
        self.sessions
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, e)| pred(&e.session))
            .max_by_key(|(_, e)| e.generation)
            .map(|(id, e)| (id.clone(), e.session.clone()))
    }
}

/// 渲染帧 → Tauri event（每会话一个泵任务；rx 由调用方经 SessionPipe::render_rx() 取出）。
/// EOF（连接断开或管道关闭）仅使泵任务自行退出，**不代发任何会话事件**（P2-16）：
/// `session:closed` 仅两个来源——`session_close` 显式（reason=`user_closed`）与 watchdog 见
/// 通道 EOF + ExitStatus(0)（reason=`remote_exit`，补丁 M-4）；其余一切断链由 watchdog 走
/// `session:disconnected{reason}` + 自动重连路径——关闭/断线两语义不再被泵 EOF 混为一谈。
///
/// 载荷带 `seq`（S295）：前端 xterm write 回调据此回 `term_ack(seq)`，后端做累计
/// 确认前缀弹出。投递失败**必须落日志**——此处曾是 `let _ = app.emit(...)`，帧已
/// 记进 `sent_frames` 却永远收不到 ack，而错误被整个丢弃、零痕迹；2026-08-19 的
/// 会话冻结正是从这类丢帧起头，事后无从指认出口（S298）。
pub fn spawn_render_pump(
    app: AppHandle,
    session_id: String,
    mut rx: mpsc::Receiver<(u64, Vec<u8>)>,
) {
    tauri::async_runtime::spawn(async move {
        // 泵任务 panic 守卫：与 pipe.rs 的 ReadTaskGuard 对称。泵静默死亡 =
        // 渲染流不再有人搬运、终端永久黑屏且零日志——这类「静默冻结」正是
        // Task 57/58 在读任务侧修掉的形状，泵侧此前仍是空白。
        struct PumpGuard(String);
        impl Drop for PumpGuard {
            fn drop(&mut self) {
                if std::thread::panicking() {
                    tracing::error!(session_id = %self.0, "渲染泵 panic 退出，该会话渲染流已断");
                }
            }
        }
        let _guard = PumpGuard(session_id.clone());
        while let Some((seq, frame)) = rx.recv().await {
            use base64::Engine;
            let len = frame.len();
            let b64 = base64::engine::general_purpose::STANDARD.encode(&frame);
            // S289：tauri 2.11.5 Emitter::emit 收 &str，format! 的 String 需显式借用。
            // `format!` 必须**内联**在 emit 实参里：event-contract.test.ts 的扫描器
            // 从调用点字面量提取事件名，提成变量会让本站点在契约表里整个消失
            // （表现为「前端 listen 无人发送」的假阳性，而真正的契约缺口反而被淹没）。
            if let Err(e) = app.emit(
                &format!("term:data:{session_id}"),
                serde_json::json!({ "seq": seq, "data_b64": b64 }),
            ) {
                // 不 break：单帧投递失败不代表通道已死，且累计确认令这笔债
                // 在下一个成功 ack 时自动结清（BatcherIn::ack）。只需留痕。
                tracing::error!(%e, session_id = %session_id, seq, len, "渲染帧投递失败");
            }
        }
    });
}

/// 会话链路终结信号（补丁 M-4）：桥接任务观察通道终结后恰发一次，watchdog 据此决定事件语义——
/// 通道 EOF 且 ExitStatus(0) = 正常结束（watchdog 发 `session:closed{reason:"remote_exit"}`，
/// 前端移除标签）；其余一切断链（非零退出 / 传输级 EOF / IO 错误 / 服务器拒绝）= 断线
///（`session:disconnected{reason}`，前端**不得移除标签**，走 banner + 自动重连）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkEnd {
    /// 通道级 Eof/Close 且 ExitStatus 为 0（或服务器未发 ExitStatus 的通道正常关闭）
    RemoteExitZero,
    /// 通道级 Eof/Close 且 ExitStatus 非零（远端 shell/进程异常退出）
    RemoteExitNonZero(u32),
    /// 远端进程被信号杀死（服务器发 exit-signal 而非 exit-status，RFC 4254 §6.10；
    /// OOM-killer / kill -9 场景）。S290：属异常终结 → 断线语义，不得归 RemoteExitZero。
    RemoteSignaled,
    /// 传输级 EOF：网络中断 / 连接死亡 / russh keepalive_max 超时
    TransportEof,
}

impl LinkEnd {
    /// 非正常终结映射为断线原因（RemoteExitZero 走 session:closed，无断线原因）
    pub fn disconnect_reason(self) -> Option<DisconnectReason> {
        match self {
            Self::RemoteExitZero => None,
            Self::RemoteExitNonZero(_) => Some(DisconnectReason::ExitNonZero),
            // S290：信号死亡按异常退出归类，保持 4 值 reason 前端契约不变；exit_code() 为 None 不附假码
            Self::RemoteSignaled => Some(DisconnectReason::ExitNonZero),
            Self::TransportEof => Some(DisconnectReason::TransportEof),
        }
    }
    /// 仅 RemoteExitNonZero 携远端退出码（写入 disconnected 载荷 `exit_code`）
    pub fn exit_code(self) -> Option<u32> {
        match self {
            Self::RemoteExitNonZero(code) => Some(code),
            _ => None,
        }
    }
}

/// 终结归类（S290：提取为纯函数供单测钉全矩阵；新增信号支）。
/// graceful = 见过通道级 Eof/Close；signaled = 见过 ExitSignal（远端进程被信号杀死）。
/// 信号死亡属**异常终结** → 断线语义（保留标签 + banner + 自动重连）；若误归
/// RemoteExitZero 则标签静默消失、无提示、不重连（复审一波中危 #1）。
pub fn classify_link_end(graceful: bool, exit_code: Option<u32>, signaled: bool) -> LinkEnd {
    if !graceful {
        // wait() 未经 Eof/Close 直接返回 None = 传输级 EOF（连接死亡/keepalive 超限），无视 exit/signal
        return LinkEnd::TransportEof;
    }
    if signaled {
        return LinkEnd::RemoteSignaled; // exit-signal：异常终结（信号优先于可能并存的 exit code）
    }
    match exit_code {
        Some(0) | None => LinkEnd::RemoteExitZero, // 正常退出 / 未携 ExitStatus 的通道关闭
        Some(code) => LinkEnd::RemoteExitNonZero(code),
    }
}

/// 断线原因枚举（补丁 M-4）：`session:disconnected` 载荷 `reason` 字段取值（`as_str` 与前端逐字对偶）。
/// 前端收到 disconnected 一律保留标签、置断线态（banner / 标签灰空心 / 侧栏灰灯 / 状态栏「已断开」，Task 22 Step 3）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisconnectReason {
    /// 传输级 EOF：网络中断 / 连接死亡 / keepalive 超时
    TransportEof,
    /// 通道 EOF 且 ExitStatus 非零：远端 shell/进程异常退出
    ExitNonZero,
    /// 服务器拒绝连接或认证失败（connect() 阶段 Connect/Auth/HostKey/Ssh 类错误，重连达上限时归类）
    Refused,
    /// 其他 IO 错误（本地 IO / 存储层）
    IoError,
}

impl DisconnectReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TransportEof => "transport_eof",
            Self::ExitNonZero => "exit_nonzero",
            Self::Refused => "refused",
            Self::IoError => "io_error",
        }
    }
}

/// russh 0.62 的 `Channel` 非 Clone、`ChannelReadHalf` 只有借用的 `make_reader()`，
/// 拿不到 owned `'static` 的 `AsyncRead` 喂给 `SessionPipe::spawn`。这里用专职任务把
/// `ChannelMsg::Data` 桥接成 `mpsc<Vec<u8>>`，再适配为 owned `AsyncRead`（F5）。
/// 有界通道(16)保留背压：pipe 暂停读取 → 桥接任务阻塞在 send → 停止 wait() →
/// russh channel window 耗尽 → 传压到 TCP（spec §2.2）。
/// `end_tx`（补丁 M-4 终结信号，置换 P2-16 裸 `Notify`）：通道终结时恰发一次 `LinkEnd`，
/// 归类矩阵见 `classify_link_end`（正常退出 / 非零退出 / 信号死亡 / 传输级 EOF）——
/// 通道级 Eof/Close 携 `ExitStatus`（shell 正常退出如键入 `exit` → `RemoteExitZero`；非零 →
/// `RemoteExitNonZero(code)`）；`ExitSignal`（远端被信号杀死，S290）→ `RemoteSignaled`；
/// `wait()` 未经 Eof/Close 直接返回 None = 传输级 EOF（连接死亡，含 russh keepalive_max
/// 超时——keepalive 回包连续失败达上限 russh 即终止连接任务）→ `TransportEof`。
/// watchdog 持 `end_rx` 据 `LinkEnd` 决定事件语义（closed vs disconnected）。
pub struct ChannelReader {
    rx: mpsc::Receiver<Vec<u8>>,
    /// 当前未消费完的 chunk 与已消费偏移
    cur: Option<(Vec<u8>, usize)>,
}

impl ChannelReader {
    pub fn spawn(mut half: russh::ChannelReadHalf, end_tx: mpsc::Sender<LinkEnd>) -> Self {
        let (tx, rx) = mpsc::channel::<Vec<u8>>(16);
        tauri::async_runtime::spawn(async move {
            let mut graceful = false;
            let mut exit_code: Option<u32> = None;
            let mut signaled = false;
            loop {
                tokio::select! {
                    // S109 同款偏置：half.wait（优雅退出）居首。读取任务 panic（tx.closed 就绪）
                    // 与远端优雅退出（half.wait 得 Eof/Close）同拍就绪时，默认随机择序会约 50%
                    // 误走 tx.closed 丢弃优雅信号 → watchdog None 分支误判 TransportEof → 虚报
                    // 断线+重连（审计6）。偏置后优雅退出优先，panic+空闲时 tx.closed 仍即时触发。
                    biased;
                    msg = half.wait() => match msg {
                        Some(russh::ChannelMsg::Data { data }) => {
                            // send 失败 = reader 已 drop（session_close → pipe.shutdown）→ 静默退出：
                            // end_tx 随本任务 drop，watchdog recv 得 None 亦静默退出（用户主动关闭路径不发任何事件）
                            if tx.send(data.to_vec()).await.is_err() {
                                return;
                            }
                        }
                        Some(russh::ChannelMsg::ExitStatus { exit_status }) => {
                            exit_code = Some(exit_status);
                        }
                        Some(russh::ChannelMsg::ExitSignal { signal_name, .. }) => {
                            // S290：远端进程被信号杀死（OOM/kill -9，sshd 发 exit-signal 而非 exit-status）。
                            // 记信号支，终结时经 classify_link_end 归断线语义，不再被 Some(_)=>{} 静默吞掉。
                            tracing::warn!(signal = ?signal_name, "remote shell terminated by signal");
                            signaled = true;
                        }
                        // Eof/Close → 正常结束桥接，drop tx → reader 读到 EOF
                        Some(russh::ChannelMsg::Eof) | Some(russh::ChannelMsg::Close) => {
                            graceful = true;
                            break;
                        }
                        Some(_) => {}
                        None => break, // 传输级 EOF = 连接死亡
                    },
                    // reader（读取任务）已 drop：session_close 或读取任务 panic。此前仅在
                    // 下一个 Data 的 send 失败时才察觉，远端空闲（停在提示符、keepalive 保活）
                    // 时永久漏报 → watchdog 永不收到 None → 静默冻结（审计4）。tx.closed()
                    // 在 reader drop 时即时触发，return 丢弃 end_tx，watchdog 的 None 分支据此
                    // 区分 session_close（静默）与异常死亡（断线）。
                    _ = tx.closed() => {
                        return;
                    }
                }
            }
            // 终结信号恰发一次（容量 1 不阻塞；watchdog 已弃则 send 失败静默）。
            // 归类矩阵见 classify_link_end（正常退出 / 非零退出 / 信号死亡 / 传输级 EOF）。
            let end = classify_link_end(graceful, exit_code, signaled);
            let _ = end_tx.send(end).await;
        });
        Self { rx, cur: None }
    }
}

impl tokio::io::AsyncRead for ChannelReader {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 补丁 M-4 断线语义归类：通道 EOF + ExitStatus(0) → 正常结束（走 session:closed，无断线原因）；
    /// 非零退出 / 信号死亡 / 传输级 EOF → session:disconnected 原因枚举。
    #[test]
    fn link_end_classification() {
        assert_eq!(LinkEnd::RemoteExitZero.disconnect_reason(), None);
        assert_eq!(LinkEnd::RemoteExitZero.exit_code(), None);
        assert_eq!(
            LinkEnd::RemoteExitNonZero(3).disconnect_reason(),
            Some(DisconnectReason::ExitNonZero)
        );
        assert_eq!(LinkEnd::RemoteExitNonZero(3).exit_code(), Some(3));
        assert_eq!(
            LinkEnd::TransportEof.disconnect_reason(),
            Some(DisconnectReason::TransportEof)
        );
        assert_eq!(LinkEnd::TransportEof.exit_code(), None);
        // S290：信号死亡 → 断线语义（不得归 RemoteExitZero），无假退出码
        assert_eq!(
            LinkEnd::RemoteSignaled.disconnect_reason(),
            Some(DisconnectReason::ExitNonZero)
        );
        assert_eq!(LinkEnd::RemoteSignaled.exit_code(), None);
    }

    /// S290：终结归类全矩阵（提取为纯函数后本支可测；信号支不得落 RemoteExitZero——复审一波中危 #1）
    #[test]
    fn classify_link_end_matrix() {
        use LinkEnd::*;
        // graceful + exit 0 → 正常结束
        assert_eq!(classify_link_end(true, Some(0), false), RemoteExitZero);
        // graceful + 无 exit + 无信号 → 正常结束（未携 ExitStatus 的通道关闭）
        assert_eq!(classify_link_end(true, None, false), RemoteExitZero);
        // graceful + 非零 exit → 远端异常退出
        assert_eq!(
            classify_link_end(true, Some(3), false),
            RemoteExitNonZero(3)
        );
        // graceful + 信号 → 断线（即使无/并存 exit code，信号优先）——不得落 RemoteExitZero
        assert_eq!(classify_link_end(true, None, true), RemoteSignaled);
        assert_eq!(classify_link_end(true, Some(0), true), RemoteSignaled);
        // 非 graceful（wait() 直接 None = 传输级 EOF）→ TransportEof，无视 exit/signal
        assert_eq!(classify_link_end(false, None, false), TransportEof);
        assert_eq!(classify_link_end(false, Some(0), false), TransportEof);
        assert_eq!(classify_link_end(false, None, true), TransportEof);
    }

    // ───────────────────────── 连接代次分配（审计 P0-4 的机制本体）
    //
    // 「重连必得新号」是所有 per-(会话, 代次) 缓存整体失效的**唯一**依据：号一旦被复用，
    // `state.rs` 那两张子系统表就会在重连后继续命中旧连接上的 SFTP 通道与传输管理器，
    // 而终端因为走的是另一条路依旧好用——用户看到的是「终端能用、文件列表转圈超时」，
    // 一种极难自证的故障。这几条测试守的就是这个号。

    /// 代次与会话内容无关，测试用占位类型登记即可（`LiveSession` 里的 russh `Handle` 与
    /// `ChannelWriteHalf` 只能由一次真实握手产出，单测造不出来——那正是本处此前零覆盖的原因）。
    struct Placeholder;

    /// 重连（同一个 session_id 再登记一次）必须换号。
    ///
    /// 这条是 P0-4 的正向回归。反面实现——`insert` 沿用已有条目的代次，或干脆由调用方传号进来
    /// ——都不会被编译器拦住，而两者都直接把缓存钉死在首次连接上。
    /// **两条路都要走**：产品里的重连路径是「remove 旧 + insert 新」，但取号绝不能依赖那次
    /// remove。只测 remove 那条会留下一个真实的缺口——实测过：把 `insert` 改成「id 已在表里就
    /// 沿用旧号」，只走 remove 的版本照样绿（remove 之后表里当然找不到，于是照常取新号），
    /// 缺陷要等到有人写出不 remove 的直接覆盖路径才爆出来。
    #[test]
    fn reinserting_the_same_session_id_allocates_a_fresh_generation() {
        for with_remove in [true, false] {
            let reg: SessionRegistry<Placeholder> = SessionRegistry::default();
            let first = reg.insert("s1".into(), Arc::new(Placeholder));
            if with_remove {
                reg.remove("s1");
            }
            let second = reg.insert("s1".into(), Arc::new(Placeholder));

            assert_ne!(
                first, second,
                "重连后必须换号（with_remove={with_remove}）：号被复用，则 \
                 (session_id, generation) 键在重连前后相同，\
                 旧连接上的 SFTP 通道与传输管理器会被继续命中"
            );
            assert_eq!(
                reg.generation_of("s1"),
                Some(second),
                "查表必须给出**最新**那一代，否则装配会落到已被替换的连接上"
            );
        }
    }

    /// 代次从 1 起：0 被 `NEXT_GENERATION` 的文档注留作「非法/未赋值」哨兵。
    /// 若哪天有人把计数器初值改成 0，`(sid, 0)` 就会同时是合法键和哨兵值。
    #[test]
    fn generations_are_never_the_zero_sentinel() {
        let reg: SessionRegistry<Placeholder> = SessionRegistry::default();
        assert!(
            reg.insert("s".into(), Arc::new(Placeholder)) >= 1,
            "0 是哨兵，不得作为真实代次发出"
        );
    }

    /// 代次是**进程级**单调的，不是 per-session 计数：两个会话各自的号也不得相同。
    ///
    /// per-session 计数看起来同样能满足「重连换号」，但它会让不同会话的第一代都拿到 1。
    /// 子系统表的键是 `(session_id, generation)`，session_id 已经把会话分开了，撞号本身
    /// 不会立刻出错——可一旦将来有任何按代次单独索引的地方（日志关联、事件去重），
    /// 撞号就会把两个会话的记录混在一起。进程级单调是更强也更便宜的保证。
    #[test]
    fn generations_are_process_wide_monotonic_not_per_session() {
        let reg: SessionRegistry<Placeholder> = SessionRegistry::default();
        let a = reg.insert("a".into(), Arc::new(Placeholder));
        let b = reg.insert("b".into(), Arc::new(Placeholder));
        assert!(b > a, "代次必须严格递增（进程级单调），实际 a={a} b={b}");

        // 另起一张表也不重置——计数器是 static，不属于任何一张注册表
        let other: SessionRegistry<Placeholder> = SessionRegistry::default();
        let c = other.insert("a".into(), Arc::new(Placeholder));
        assert!(c > b, "计数器不随注册表实例重置，实际 b={b} c={c}");
    }

    /// `get_with_generation` 必须**一次取出**会话与代次。
    ///
    /// 分两次取（先 `generation_of` 再 `get`）会在两次之间被重连插队，拿到「新代次 + 旧连接」
    /// 的错配组合——比不缓存还糟，因为它会把新键映射到死连接上并长期驻留。
    /// 这条测试钉的是「两者同源」这个性质：取到的代次必须与取到的那个会话对象属于同一次登记。
    #[test]
    fn generation_and_session_are_taken_together() {
        let reg: SessionRegistry<Placeholder> = SessionRegistry::default();
        let old_session = Arc::new(Placeholder);
        let old_gen = reg.insert("s1".into(), old_session.clone());

        let (gen, session) = reg
            .get_with_generation("s1")
            .expect("刚登记的会话必须查得到");
        assert_eq!(gen, old_gen);
        assert!(
            Arc::ptr_eq(&session, &old_session),
            "取出的会话必须就是登记进去的那一个"
        );

        // 重连：换连接、换号，两者必须同步翻新
        let new_session = Arc::new(Placeholder);
        let new_gen = reg.insert("s1".into(), new_session.clone());
        let (gen, session) = reg.get_with_generation("s1").unwrap();
        assert_eq!(gen, new_gen, "代次没跟上换连接");
        assert!(
            Arc::ptr_eq(&session, &new_session),
            "会话没跟上换代次——「新代次 + 旧连接」正是要杜绝的错配"
        );
    }

    /// 会话不存在时三个查询入口一律 None，调用方据此 fail-closed
    ///（`AppState::current_generation` 把 None 翻成 Err，绝不给一个编造的代次）。
    #[test]
    fn queries_on_an_unknown_session_all_return_none() {
        let reg: SessionRegistry<Placeholder> = SessionRegistry::default();
        assert!(reg.generation_of("nope").is_none());
        assert!(reg.get("nope").is_none());
        assert!(reg.get_with_generation("nope").is_none());
        assert!(reg.remove("nope").is_none());
        assert!(reg.find_latest_by(|_: &Placeholder| true).is_none());
    }

    /// `find_latest_by` 取**代次最大者**，而不是 HashMap 碰巧先给出的那个。
    ///
    /// 计划任务（M4a）要在「某个档案的一个会话」上执行命令，而同一档案可能开着好几个
    /// 会话。取迭代序的第一个是不可复现的（迭代序随哈希种子变），排查「命令跑到哪去了」
    /// 时这种不确定性最要命。取最新建立的那条也最可能是活着的。
    #[test]
    fn find_latest_by_picks_the_newest_generation_not_hash_order() {
        // 带一个可判定的负载，好写谓词
        struct Tagged(&'static str);
        let reg: SessionRegistry<Tagged> = SessionRegistry::default();
        // 登记顺序 = 代次递增顺序；插入足够多条，让「碰巧命中最后一条」不成立
        for id in ["a", "b", "c", "d", "e", "f", "g", "h"] {
            reg.insert(id.into(), Arc::new(Tagged("p1")));
        }
        let newest = reg.insert("newest".into(), Arc::new(Tagged("p1")));
        // 再插一条**不匹配**的，且它的代次最大——不该被选中
        reg.insert("other".into(), Arc::new(Tagged("p2")));

        let (id, _s) = reg
            .find_latest_by(|t: &Tagged| t.0 == "p1")
            .expect("应找到 p1 的会话");
        assert_eq!(id, "newest", "须取代次最大的那条 p1 会话");
        assert_eq!(reg.generation_of("newest"), Some(newest));

        // 谓词无匹配时返回 None（而不是随便给一个）
        assert!(reg.find_latest_by(|t: &Tagged| t.0 == "不存在").is_none());
        // 只剩不匹配的那条时也是 None
        assert_eq!(
            reg.find_latest_by(|t: &Tagged| t.0 == "p2").map(|(i, _)| i),
            Some("other".to_string())
        );
    }

    #[test]
    fn disconnect_reason_payload_strings() {
        // 与 session:disconnected 载荷 reason 字段逐字对偶（Task 22 Step 3 banner 按此消费）
        assert_eq!(DisconnectReason::TransportEof.as_str(), "transport_eof");
        assert_eq!(DisconnectReason::ExitNonZero.as_str(), "exit_nonzero");
        assert_eq!(DisconnectReason::Refused.as_str(), "refused");
        assert_eq!(DisconnectReason::IoError.as_str(), "io_error");
    }
}
