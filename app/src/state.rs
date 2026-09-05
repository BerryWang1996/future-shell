use crate::events::PendingPrompts;
use crate::sessions::SessionRegistry;
use fs_connmgr::Db;
use fs_vault::Store;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tauri::AppHandle;
use tokio::sync::{Mutex, OnceCell};

/// 子系统缓存键（审计 P0-4）：`(session_id, generation)`。
///
/// 只用 session_id 作键的旧实现在重连后是「看起来对、实际全错」：`establish_session` 换掉了
/// 注册表里的 russh Handle，却没动这两张缓存表，于是 SFTP 与传输继续跑在**已断开的旧连接**上。
/// 把代次并进键里，重连一发生，旧代次的条目就再也不会被任何查表命中——失效是查表的天然结果，
/// 不依赖任何人记得去清缓存。
pub type SubsystemKey = (String, u64);

/// 子系统惰性槽位：`Arc<OnceCell<Arc<T>>>` 这三层不是包装癖，每一层各挡一件具体的事故——
///
/// - 外层 `Arc`：槽位要能在**放开表锁之后**继续被持有。装配一条 SFTP 通道含两次网络往返，
///   若持表锁等它，其他会话的 SFTP/传输入口会被这一次往返整个冻住（P1-4 的修法是收口装配，
///   不是拉长临界区）。取到 `Arc<OnceCell<..>>` 就立刻还锁，装配在锁外进行。
/// - `OnceCell`：把「查表 → 装配 → 落表」收成对同一格子的原子 get-or-create。裸 `Arc<T>`
///   的旧实现在 get 与 insert 之间放了锁，两个并发调用会各装配一个对象、后写入者覆盖前者，
///   被覆盖的那个仍活着却再也拿不到引用——既关不掉也取消不了（审计 P1-4 的孤儿通道/孤儿
///   管理器）。`get_or_try_init` 让并发者阻塞在同一个 cell 上，最终共享**同一个**对象。
/// - 内层 `Arc`：对象本身的共享句柄。`RemoteSftp` / `TransferManager` 要同时交给命令处理器、
///   事件泵和 worker 使用，取出后必须能脱离表独立存活。
pub type SubsystemSlot<T> = Arc<OnceCell<Arc<T>>>;

/// per-(会话, 代次) 子系统表：`Arc` 让多个 tokio 任务共享同一张表；`Mutex` 只保护**表结构本身**
///（增删查，临界区内无 await），槽位内容的装配由 [`SubsystemSlot`] 在锁外收口。
pub type SubsystemMap<T> = Arc<Mutex<HashMap<SubsystemKey, SubsystemSlot<T>>>>;

/// 会话子系统关停宽限期（审计 P0-5）。
///
/// 5 秒的依据：分块粒度是 256 KiB，取消标志在每个分块边界检查一次，即便链路慢到 100 KiB/s，
/// 单块也就 2.5 s；给到 5 s 足以让绝大多数在途任务走完当前块并释放文件句柄。再长则会把
/// 「关闭标签」这个用户预期瞬时完成的操作拖成可感知的卡顿——超时不是数据损失（临时件全部
/// 保留、可续传），只是收尾晚一点，不值得让 UI 陪着等。
const SUBSYSTEM_SHUTDOWN_GRACE: std::time::Duration = std::time::Duration::from_secs(5);

/// 会话拆除时需要「先收尾、再摘表」的子系统（审计 P0-5）。
///
/// 抽成 trait 不是为了多态——产品里只有 `TransferManager` 一个实现——而是为了让
/// [`shutdown_and_clear`] 的**顺序**能被测试看见。这段逻辑原先内联在
/// `shutdown_session_subsystems` 里，唯一的实现类型要连一台真的 SSH 服务器才建得出来，
/// 于是 P0-5 修的那条顺序在整个仓库里没有任何东西守着：把两句调换回去，全部测试照样绿。
#[async_trait::async_trait]
pub trait SubsystemShutdown: Send + Sync {
    /// 在宽限期内有序关停。返回 false = 超时，仍有任务未到达终态。
    async fn shutdown_within(&self, grace: std::time::Duration) -> bool;
    /// 超时后仍未收尾的件数，仅用于日志。
    async fn unfinished(&self) -> usize;
}

#[async_trait::async_trait]
impl SubsystemShutdown for fs_sshengine::transfer::TransferManager {
    async fn shutdown_within(&self, grace: std::time::Duration) -> bool {
        self.shutdown(grace).await
    }
    async fn unfinished(&self) -> usize {
        self.active_len().await
    }
}

/// 取（或建）指定 `(会话, 代次)` 的子系统槽位。
///
/// 键里带代次是审计 P0-4 的全部内容：重连一发生，注册表分配的新号让旧代次的条目再也不会被
/// 任何查表命中——失效是查表的天然结果，不依赖任何人记得去清缓存。
///
/// 只在锁内取 cell（临界区内无 await），装配放到锁外：装配一条 SFTP 通道含两次网络往返，
/// 持表锁等它会把其他会话的 SFTP/传输入口整个冻住（P1-4 的修法是收口装配，不是拉长临界区）。
pub async fn subsystem_slot<T>(
    map: &SubsystemMap<T>,
    session_id: &str,
    generation: u64,
) -> SubsystemSlot<T> {
    map.lock()
        .await
        .entry((session_id.to_string(), generation))
        .or_default()
        .clone()
}

/// 若表里该键**仍指向** `stale` 这一格，就摘掉它；否则原样不动。返回是否真的摘了。
///
/// 用途是超时判死后的驱逐（审计2 #11）：`OnceCell` 一经写入不可改写，所以「换一条通道」
/// 只能是「摘掉这一格，让下一次调用建一格新的」。
///
/// 判据是 `Arc::ptr_eq` 而不是「键存在就删」，因为并发调用会同时发现同一条死通道：
/// 若按键删，甲摘掉死格、建了新格并落表之后，乙的删除会把**甲刚建好的活通道**一并摘走，
/// 于是那条通道成了谁也拿不到的孤儿（审计 P1-4 里已经付过一次代价的正是这种覆盖）。
/// 按身份删则天然幂等：只有第一个到场的人删得掉，后来者看到的已经是新格，什么也不做。
pub async fn evict_slot_if_current<T>(
    map: &SubsystemMap<T>,
    session_id: &str,
    generation: u64,
    stale: &SubsystemSlot<T>,
) -> bool {
    let key = (session_id.to_string(), generation);
    let mut m = map.lock().await;
    match m.get(&key) {
        Some(cur) if Arc::ptr_eq(cur, stale) => {
            m.remove(&key);
            true
        }
        _ => false,
    }
}

/// 取一格**可用**的槽位：若表里那一格里的对象已经死了，就摘掉它并重取一格空的。
///
/// 这是超时判死之后唯一的换通道路径（审计2 #11）。`OnceCell` 一经写入不可改写，所以
/// 「换一条通道」只能表达成「摘掉这一格，让下一次装配建一格新的」。
///
/// 死活判据由调用方以闭包给出，而不是给 `T` 加 trait 约束：这里真正需要的只有一个
/// `&T -> bool`，为它引入一条跨 crate 的 trait 只会让 `fs_sshengine` 多一个仅供 app 使用的
/// 公开接口。抽成独立函数则是为了**可测**——`sftp_ops_for` 的其余部分要一条真实的 russh
/// 会话才跑得起来，而「判死了要不要换一格」这个判断与连接无关；审计2 #11 要的是
/// 「**可验证**的超时和退出语义」，塞在不可测函数里的退出语义算不上已验证。
pub async fn live_slot<T>(
    map: &SubsystemMap<T>,
    session_id: &str,
    generation: u64,
    dead: impl Fn(&T) -> bool,
) -> SubsystemSlot<T> {
    let cell = subsystem_slot(map, session_id, generation).await;
    if cell.get().is_some_and(|v| dead(&**v)) {
        evict_slot_if_current(map, session_id, generation, &cell).await;
        return subsystem_slot(map, session_id, generation).await;
    }
    cell
}

/// 关停某会话在**所有代次**上的子系统，然后才摘表（审计 P0-4 / P0-5）。
///
/// 两条性质，缺一条都退化回原缺陷：
///
/// **① 顺序：先 shutdown、再 remove。** 原实现只做 remove——而派发任务与每个传输任务都是
/// `tokio::spawn` 出去的独立任务，各自持有 `Arc<dyn SftpOps>` 与本地文件句柄，从 map 里删掉
/// 不会中断它们中的任何一个。用户关掉标签之后，后台还在继续改他的文件，直到 SSH 连接自己
/// 断开为止。remove 只是让引用消失，shutdown 才是让任务停下；顺序反过来，摘表还会让仍在跑的
/// worker 再也拿不到引用，既关不掉也取消不了。
///
/// **② 覆盖所有代次，不只是当前代次。** 重连过 N 次就可能残留 N 个管理器，只清当前代次等于
/// 把前面几代全部漏掉——那正是 P0-4 制造出来的那批孤儿。
///
/// 关停期间**不持表锁**：`shutdown_within` 要 await 到宽限期结束，持锁等它会把所有会话的
/// SFTP/传输入口一并冻住。故先取快照再放锁。
///
/// 不返回 Result：调用方（关闭/重连路径）无论如何都要继续往下走，超时也只是收尾晚一点
///（临时件全部保留，重连后可续传，不存在数据损失），故失败信息以 warn 落日志而非上抛。
pub async fn shutdown_and_clear<T: SubsystemShutdown>(
    map: &SubsystemMap<T>,
    session_id: &str,
    grace: std::time::Duration,
) {
    let slots: Vec<(u64, SubsystemSlot<T>)> = {
        let table = map.lock().await;
        table
            .iter()
            .filter(|((sid, _), _)| sid == session_id)
            .map(|((_, generation), cell)| (*generation, cell.clone()))
            .collect()
    };
    for (generation, cell) in &slots {
        // 未初始化的 cell = 装配失败或从未用过，没有任务需要收尾。
        let Some(obj) = cell.get() else { continue };
        if !obj.shutdown_within(grace).await {
            // 未收尾件数必须先 await 出来再进宏：`tracing::warn!` 的参数会被展开成
            // `&dyn Value` 临时量，在其中 await 会把非 Send 的临时量跨到 await 点之后，
            // 整个 future 随之失去 Send，而 tauri 的命令处理器要求 Send（编译期直接报错）。
            let remaining = obj.unfinished().await;
            tracing::warn!(
                session_id,
                generation,
                remaining,
                grace_secs = grace.as_secs(),
                "子系统关停超时：仍有任务未到达终态；临时件保留，重连后可续传"
            );
        }
    }
    // 任务确已停下（或已超时并记账）之后才摘表——顺序反过来就退化成原来的 P0-5。
    // 超时也照摘：留着只会让下一次查表命中一个已被放弃的对象。
    map.lock().await.retain(|(sid, _), _| sid != session_id);
}

pub struct AppState {
    /// 应用数据目录（由 lib.rs 在装配时经 `data_dir_or_err()` 解析注入）。
    /// S248 时期本字段无人读取、靠 `#[allow(dead_code)]` 压着；P1-15 之后 `download_sandbox_for`
    /// 的第三级兜底（`data_dir/downloads/<profile_id>`）实打实消费它，压制随之删除——
    /// 留着 `allow` 会把将来真正死掉的字段一并遮住。
    pub data_dir: PathBuf,
    pub db: Arc<Db>,
    pub vault: Arc<Mutex<Option<Store>>>, // None = 未解锁/未初始化
    /// Task 19：tauri AppHandle 克隆，供 GuiEvents emit 事件与 watchdog/重连循环 emit session:disconnected/closed。
    pub app: AppHandle,
    /// Task 19：会话注册表（session_id -> LiveSession），term_input/resize/ack/close 与 Task 21 SFTP 共用。
    pub registry: Arc<SessionRegistry>,
    /// M7.4：串口会话注册表。**另起一张**而不是塞进 `SessionRegistry<LiveSession>`——
    /// 后者的每个字段都是 SSH 的（russh 写半部、连接句柄、跳板端点），串口一个都没有。
    /// 终端那一侧仍然共用：`term_input`/`term_ack`/`term_resize`/`session_close`
    /// 在 SSH 表落空时回落到这里（见 `session_cmd` 的 serial 回落分支）。
    pub serial: Arc<SessionRegistry<crate::serial_session::SerialSession>>,
    /// Task 19：待决 TOFU/kbd-interactive prompt 的 oneshot tx 表，auth_respond/hostkey_decide 取出 send。
    pub pending: Arc<PendingPrompts>,
    /// Task 21：per-(会话, 代次) SFTP 操作句柄表（惰性创建：首次调用 sftp_* IPC 时装配）。
    ///
    /// 值是 `OnceCell` 而非裸 `Arc<RemoteSftp>`（审计 P1-4）：装配过程含网络往返
    ///（`channel_open_session` + `request_subsystem`），原实现在 `get` 与 `insert` 之间放了锁，
    /// 两个并发的 `sftp_list` 会各开一条 SFTP 通道，后写入者覆盖前者——被覆盖的那条通道仍活着
    /// 却再也拿不到引用，既关不掉也取消不了。`OnceCell::get_or_try_init` 让「查表 + 装配 + 落表」
    /// 成为对同一格子的原子操作：并发者阻塞在同一个 cell 上，最终共享**同一个**通道。
    pub sftp_ops: SubsystemMap<fs_sshengine::timeouts::TimedSftp>,
    /// Task 21：per-(会话, 代次) 传输管理器（首次 transfer_submit 时创建，同时 spawn 事件泵）。
    /// 同样用 `OnceCell` 收口并发装配——传输管理器被覆盖的后果比 SFTP 通道更重：孤儿管理器
    /// 的 worker 仍在写文件，而 `cancel`/`shutdown` 都打不到它（P1-4）。
    pub transfers: SubsystemMap<fs_sshengine::transfer::TransferManager>,
    /// Task 21：传后校验槽位（transfer_submit 登记，事件泵在终态取出执行）。
    pub verify_plans: Arc<Mutex<HashMap<u64, VerifySlot>>>,
    /// 连接期采集的 host facts（M2 出口第 15 项），按 session_id。
    ///
    /// 放在这里而不是 LiveSession 里：那个结构的每个字段都是「这条连接是什么」，
    /// 而 host facts 是**可选、可失败、迟到**的观测结果——塞进去就得给它一个
    /// `Mutex<Option<_>>`，等于在「连接本体」里承认「这一项可能没有」。
    /// 采集见本文件末尾的 `spawn_host_facts_collection`。
    pub host_facts: HostFactsCache,
    /// M3：Agent 监督器（run 表 + 急停令牌 + 待答确认表）。
    pub agents: std::sync::Arc<crate::agent::confirm_port::AgentSupervisor>,
    /// M3：MCP 确认待答表（§4.5 阻塞式确认契约的 app 侧落点）。
    /// 与 `agents` 的确认表分开：两边票据形状与超时上限不同，分表防答错。
    pub mcp_confirms: std::sync::Arc<crate::mcp::McpConfirms>,
    /// 前端错误上报去重表（S300）：`"source\u{1}message" -> 首次上报时刻`。
    /// 前端异常常以「每帧一次」的频率复发，无节流会把 8 MiB 日志上限撑爆、
    /// 把出事前的现场卷进 `.old` 覆盖掉。条目在 `log_frontend_error` 内按
    /// `FRONTEND_ERROR_DEDUP_WINDOW` 惰性清理——错误文本可含变量、基数无上限，
    /// 不清理即是内存泄漏面。
    ///
    /// 用 `std::sync::Mutex` 而非本文件其余字段的 `tokio::sync::Mutex`：临界区是
    /// 纯 map 读写、不跨任何 await，异步锁在此只增开销与「忘了 .await」的失误面。
    pub frontend_error_seen: Arc<std::sync::Mutex<HashMap<String, std::time::Instant>>>,
    /// 运行中的隧道（M4a）：session_id → 该会话的隧道列表。会话拆除时一并停止
    /// （隧道生命周期绑会话，见 fs_sshengine::tunnel 模块头③）。
    pub tunnels: Arc<Mutex<HashMap<String, Vec<fs_sshengine::tunnel::RunningTunnel>>>>,
    /// 监控 CPU% 的每会话上次 `/proc/stat` 采样（M4a，见 monitor_cmd.rs 的 CpuPrevMap
    /// 文档——为何存后端、为何会话关闭要清、为何键不含 generation）。
    pub cpu_prev: Arc<std::sync::Mutex<crate::commands::monitor_cmd::CpuPrevMap>>,
    /// 每会话的录屏器（M4a asciicast v2）：session_id → 常驻 idle 录制器。
    /// idle 时零开销，手动启停只翻内部标志（见 record_cmd 模块头）。
    pub recorders: crate::commands::record_cmd::RecorderMap,
    /// 每会话的 ZMODEM 拦截器（M4a rz/sz）：session_id → 拦截器。
    ///
    /// 单独存一张表而不是塞进 `LiveSession`：拦截器必须在 `SessionPipe::spawn`
    /// **之前**造好（它是 `PipeOpts.tap`），而 `LiveSession` 是在 pipe 造好之后
    /// 才组装的——塞进去就得先建一个半成品再回填，那种两段式初始化是本仓一直在
    /// 消除的形状（见 `sessions::RegistryEntry` 关于代次为何不做成字段的说明）。
    ///
    /// 用 `std::sync::Mutex`：临界区只做一次 map 取放，不跨 await。
    pub zmodem: Arc<std::sync::Mutex<HashMap<String, Arc<crate::zmodem_bridge::ZmodemTap>>>>,
    /// 正在用外部编辑器编辑的远端文件（M4a）：键 = 会话 + 远端路径。
    /// 值里存着开局时两侧的快照——回传前的冲突判定就是拿它们和「现在」比
    /// （见 commands::editor_cmd 与 fs_sshengine::editsync）。
    pub edits: crate::commands::editor_cmd::EditMap,
}

/// 传后校验意图（UI 规格 §1.4）：id 对应 TransferManager 作业 id，VerifyEntry 携期望哈希/降级标志。
pub struct PendingVerify {
    pub session_id: String,
    /// `None` = 本作业**不做**传后校验（用户关掉了 `transfer.verifyAfterTransfer`）。
    ///
    /// 「不校验」也必须登记（审计 P2「`Settled` 孤儿」路径一）：登记是握手的一半，
    /// 缺了它事件泵在终态查表落空，就会留下一枚永远等不到认领者的 `Settled`——
    /// 作业 id 由 `next_transfer_id` 进程级单调分配、永不复用，没有第二次机会来取。
    pub entry: Option<fs_sshengine::verify::VerifyEntry>,
}

/// 校验槽位（审计 P1-2 完成竞态）。
///
/// 作业 id 由引擎在 `submit` 内部分配，调用方只能在 `submit` **返回之后**才知道该往哪一格登记
/// 意图；而一个几 KiB 的小文件完全可能在 `submit` 返回前就跑完并发出 Done。原实现「先 submit
/// 后 insert」于是有一段真实存在的窗口：事件泵在这段窗口里查表落空、直接跳过，登记进去的计划
/// 从此没人来取，用户看到的是「传输成功但永远没有校验结果」，而校验恰是这条链路的价值所在。
///
/// 解法不是把窗口调小（那只是把偶发变成更偶发），而是让两侧在**同一把锁内**做一次握手：谁先到
/// 谁留下痕迹，后到的一方负责收尾。故槽位有两种形态——
pub enum VerifySlot {
    /// 登记先到：校验意图就位，等事件泵送来终态。
    Planned(Box<PendingVerify>),
    /// 终态先到：事件泵没找到计划，留下这枚标记说明「本作业已终结、终态是 done」，
    /// 由随后完成登记的 `transfer_submit` 取走并自行驱动校验。
    ///
    /// `session_id` 是审计 P2「`Settled` 孤儿」的修法之一：标记原先不携身份，
    /// `shutdown_session_subsystems` 的 retain 因此无从判断该不该收，只能一律保留。
    Settled { done: bool, session_id: String },
}

/// 登记侧（`transfer_submit`）握手的结果。
pub enum RegisterOutcome {
    /// 表里原先空着：意图已落 `Planned`，等事件泵来取。
    Pending,
    /// 终态先到：事件泵留下的标记已被取走，`done` 是那次终态是否成功。
    AlreadySettled { done: bool },
}

/// 终态侧（事件泵）握手的结果。
pub enum SettleOutcome {
    /// 登记方先到：意图已取走，由调用方执行（`entry` 为 None 即本作业不校验）。
    Plan(Box<PendingVerify>),
    /// 无人登记：已留下 `Settled` 标记等登记方来取。
    /// **调用方必须记下该 id**，在事件流结束时回收（见 `verify_reap_settled`）。
    Marked,
    /// 同一 id 的第二次终态（理论不可达）：原样放回，不动。
    Duplicate,
}

/// 登记侧握手（`transfer_submit`）。见 `VerifySlot` 的文档注。
///
/// `make_plan` 惰性构造而非直接收一个 `PendingVerify`：`AlreadySettled` 分支下调用方要拿
/// **自己手里**那份意图直接驱动校验，不能在这里先把它消耗掉。
pub fn verify_register(
    plans: &mut HashMap<u64, VerifySlot>,
    id: u64,
    make_plan: impl FnOnce() -> PendingVerify,
) -> RegisterOutcome {
    match plans.remove(&id) {
        Some(VerifySlot::Settled { done, .. }) => RegisterOutcome::AlreadySettled { done },
        Some(slot) => {
            // 理论不可达（id 进程级唯一、本函数是唯一登记点）：放回，不丢已有意图。
            plans.insert(id, slot);
            RegisterOutcome::Pending
        }
        None => {
            plans.insert(id, VerifySlot::Planned(Box::new(make_plan())));
            RegisterOutcome::Pending
        }
    }
}

/// 终态侧握手（事件泵）。查不到计划不代表没有校验意图，只代表登记方还没跑到——
/// 留下 `Settled` 让它接手，绝不静默跳过（审计 P1-2）。
pub fn verify_settle(
    plans: &mut HashMap<u64, VerifySlot>,
    id: u64,
    session_id: &str,
    done: bool,
) -> SettleOutcome {
    match plans.remove(&id) {
        Some(VerifySlot::Planned(p)) => SettleOutcome::Plan(p),
        Some(other) => {
            plans.insert(id, other);
            SettleOutcome::Duplicate
        }
        None => {
            plans.insert(
                id,
                VerifySlot::Settled {
                    done,
                    session_id: session_id.to_string(),
                },
            );
            SettleOutcome::Marked
        }
    }
}

/// 事件泵退出时回收它自己留下的 `Settled` 标记（审计 P2「`Settled` 孤儿」路径二）。
///
/// 泵的事件流结束 = 管理器与它派生的**全部** worker 都已消失（每个 worker 持有一份 sender
/// 克隆，最后一份 drop 掉 `recv` 才返回 None），此后本代次不可能再有事件。
/// 此刻还留在表里的标记就是**确定的**孤儿：登记方若还没来，它永远不会来了。
///
/// 为什么非要泵自己收：路径二的稳定触发是关停超时——`TransferManager::shutdown` 在
/// `SUBSYSTEM_SHUTDOWN_GRACE` 内没收完就返回 false，`shutdown_session_subsystems` 把这当作
/// 预期结果（warn 后继续）并 retain 掉 `Planned`；仍在跑的 worker 之后才发出终态，泵必然落到
/// 「无人登记」分支留下标记。而那时会话已经拆完，**不会再有第二次 teardown** 来收它。
///
/// 只收传进来的这批 id，不按会话批量清：重连会为同一 `session_id` 起新一代管理器与新泵，
/// 旧泵按会话清会把新一代正在等终态的 `Planned` 一并抹掉——校验静默失效比留几个孤儿糟得多。
pub fn verify_reap_settled(plans: &mut HashMap<u64, VerifySlot>, ids: &[u64]) {
    for id in ids {
        // 作业 id 进程级单调、永不复用：此处不可能误删别人的槽位。
        // 登记方若已把标记取走，这一下就是无操作。
        plans.remove(id);
    }
}

/// 会话拆除时该不该留下这枚槽位（审计 P2）。
///
/// 原实现的 `Settled { .. } => true` 配了一句「它由 `transfer_submit` 侧在同一轮内取走」的注释，
/// 而这个前提在**登记方已经跑过**的情形下根本不成立——那才是孤儿的来源。
/// 现在标记携会话身份，判据与 `Planned` 一致：本会话的意图与标记一律作废（作业都停了）。
pub fn verify_slot_survives_teardown(slot: &VerifySlot, session_id: &str) -> bool {
    match slot {
        VerifySlot::Planned(p) => p.session_id != session_id,
        VerifySlot::Settled {
            session_id: owner, ..
        } => owner != session_id,
    }
}

impl AppState {
    /// 当前连接代次（审计 P0-4）：会话不存在即 Err，调用方一律 fail-closed。
    pub fn current_generation(&self, session_id: &str) -> Result<u64, String> {
        self.registry
            .generation_of(session_id)
            .ok_or_else(|| "no session".to_string())
    }

    /// 取指定会话**当前代次**的 SFTP 操作句柄，若无则通过注册表内的 Handle 新建 SFTP 通道并装配
    ///（方案甲 P2-15/30）。旧代次的条目不会被本查表命中，等同于已失效（审计 P0-4）。
    pub async fn sftp_ops_for(
        &self,
        session_id: &str,
    ) -> Result<Arc<fs_sshengine::timeouts::TimedSftp>, String> {
        use fs_sshengine::timeouts::{with_timeout, TimedSftp, CONTROL_TIMEOUT};
        let generation = self.current_generation(session_id)?;
        // 缓存里那条通道若已被一次超时判死，就当它不存在（审计2 #11）：判死之后它的每个方法
        // 都只会就地返回 `Poisoned`，留在表里等于让这个会话的 SFTP 功能永久不可用——而代次
        // 没变，`shutdown_session_subsystems` 那条清理路径也不会被触发。摘表后重取一格空的，
        // 由本次调用负责重建；重建走的是真实的 channel open，「通道又好了」因此是有证据的。
        let cell = live_slot(&self.sftp_ops, session_id, generation, |ops| {
            ops.is_poisoned()
        })
        .await;
        let ops = cell
            .get_or_try_init(|| async {
                // 代次复核：从取号到装配之间仍可能被重连插队，此时该拿的是新代次的连接。
                // 不复核就会把「新代次的键」映射到「旧连接的通道」，比不缓存还糟。
                let (gen_now, session) = self
                    .registry
                    .get_with_generation(session_id)
                    .ok_or_else(|| "no session".to_string())?;
                if gen_now != generation {
                    return Err("session reconnected during SFTP setup".to_string());
                }
                // 以下三段都在 `handle` 锁内（russh 的 Handle 非 Sync，必须独占）。这正是它们
                // 非有超时不可的原因：这里挂死冻住的不只是本次装配，而是同一条会话上排在后面
                // 的每一个操作——终端输入、其他 SFTP 请求、关闭会话，全都要过这把锁。
                let handle = session.handle.lock().await;
                let channel = with_timeout("SFTP 开通道", CONTROL_TIMEOUT, async {
                    handle
                        .channel_open_session()
                        .await
                        .map_err(|e| e.to_string())
                })
                .await
                .map_err(|e| e.to_string())??;
                with_timeout("SFTP 子系统握手", CONTROL_TIMEOUT, async {
                    channel
                        .request_subsystem(true, "sftp")
                        .await
                        .map_err(|e| e.to_string())
                })
                .await
                .map_err(|e| e.to_string())??;
                let sftp = with_timeout("SFTP 版本协商", CONTROL_TIMEOUT, async {
                    fs_sshengine::sftp::RemoteSftp::new(channel)
                        .await
                        .map_err(|e| e.to_string())
                })
                .await
                .map_err(|e| e.to_string())??;
                Ok::<_, String>(Arc::new(TimedSftp::new(Arc::new(sftp))))
            })
            .await?;
        Ok(ops.clone())
    }

    /// 取指定会话**当前代次**的传输管理器，若无则创建并 spawn 事件泵（首次调用处）。
    pub async fn transfer_manager_for(
        &self,
        session_id: &str,
    ) -> Result<Arc<fs_sshengine::transfer::TransferManager>, String> {
        let generation = self.current_generation(session_id)?;
        let cell = subsystem_slot(&self.transfers, session_id, generation).await;
        let mgr = cell
            .get_or_try_init(|| async {
                let ops = self.sftp_ops_for(session_id).await?;
                // 装配完成前再确认一次代次：`sftp_ops_for` 内部有 await，期间可能已重连。
                if self.current_generation(session_id)? != generation {
                    return Err("session reconnected during transfer manager setup".to_string());
                }
                // exec 通道给的是**提交前**闸门用的（审计2 #10）：上传方向要在 rename 之前
                // 让服务端算一次 `<remote>.fspart` 的 sha256。取不到（会话不支持 exec、
                // 已重连）就传 None，闸门自动降级到「源未变 + 临时件长度」两层，不阻断传输。
                let exec = self.exec_adapter_for(session_id).await.ok();
                let mgr = Arc::new(
                    fs_sshengine::transfer::TransferManager::spawn_with_verifier(ops, 3, exec),
                );
                let rx = mgr.events().await;
                // 事件泵必须知道自己服务的是哪个会话：`transfer:progress` 载荷缺 sessionId 时
                // 前端只能把所有会话的进度混进一张表（审计 P0-1）。
                crate::commands::sftp_cmd::spawn_transfer_pump(
                    self.app.clone(),
                    session_id.to_string(),
                    rx,
                );
                Ok::<_, String>(mgr)
            })
            .await?;
        Ok(mgr.clone())
    }

    /// 关停并清除某会话的全部子系统（审计 P0-4 / P0-5）。会话关闭与重连**都**必须先调它。
    ///
    /// 三张表的清理**顺序**本身也是修复的一部分，见各段注释；每张表内部的
    /// 「先 shutdown、再 remove、且覆盖所有代次」由 [`shutdown_and_clear`] 保证。
    pub async fn shutdown_session_subsystems(&self, session_id: &str) {
        shutdown_and_clear(&self.transfers, session_id, SUBSYSTEM_SHUTDOWN_GRACE).await;
        // SFTP 通道随会话一并丢弃：它长在已被关闭/替换的 russh Handle 上，留着只会让下一次
        // 查表命中一条死通道。放在传输之后清，是因为传输 worker 收尾时还在用它。
        self.sftp_ops
            .lock()
            .await
            .retain(|(sid, _), _| sid != session_id);
        // 该会话尚未兑现的校验意图与终态标记一并作废（作业都停了，没有终态事件会再来取）。
        // 判据见 `verify_slot_survives_teardown`——`Settled` 此前被无条件保留，是审计 P2
        // 「`Settled` 孤儿」的一半来源。剩下那一半（本次 retain 之后才由仍在跑的 worker 触发的
        // 标记）由事件泵退出时的 `verify_reap_settled` 收尾。
        self.verify_plans
            .lock()
            .await
            .retain(|_, slot| verify_slot_survives_teardown(slot, session_id));
        // 监控 CPU% 的前值采样一并作废（monitor_cmd.rs CpuPrevMap 文档：重连后旧前值
        // 属于旧机器时刻，差量是垃圾，重连从空态重新起步）。
        self.cpu_prev
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(session_id);
        // 隧道随会话停（fs_sshengine::tunnel 模块头③）：SSH 连接没了，监听端口只会
        // accept 出一堆立刻失败的连接——留着比停掉更误导，还占着端口不让用户重建。
        if let Some(list) = self.tunnels.lock().await.remove(session_id) {
            for t in &list {
                t.stop();
            }
            if !list.is_empty() {
                tracing::info!(session_id, count = list.len(), "会话拆除：隧道已全部停止");
            }
        }
        // 录屏器随会话作废（Drop 会把在录的尾巴 flush 掉；重连是新的录制起点）。
        if self
            .recorders
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(session_id)
            .is_some()
        {
            tracing::info!(session_id, "会话拆除：录屏器已作废");
        }
        // ZMODEM 拦截器随会话作废：它持着旧代次的落盘句柄与状态机，留着会让重连后
        // 的新连接命中一个半途而废的传输态（表现为「新会话里一开就说已有传输在进行」）。
        // 拦截器 drop 后其驱动任务的 Weak 升级失败即自行退出。
        if self
            .zmodem
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(session_id)
            .is_some()
        {
            tracing::info!(session_id, "会话拆除：ZMODEM 拦截器已作废");
        }
        // 编辑会话随连接作废：远端句柄没了，回传无处可去。**暂存文件保留不删**——
        // 用户可能正在编辑器里改着没存，或者存了但连接先断了；那份内容是他的劳动成果，
        // 断线不是删掉它的理由。清理交给下次 editor_close 或用户自己（路径已在 UI 显示过）。
        {
            let mut map = self.edits.lock().unwrap_or_else(|p| p.into_inner());
            let before = map.len();
            map.retain(|_, e| e.session_id != session_id);
            let dropped = before - map.len();
            if dropped > 0 {
                tracing::info!(
                    session_id,
                    dropped,
                    "会话拆除：编辑会话已作废（本地暂存文件保留，未存盘的改动不代为丢弃）"
                );
            }
        }
    }

    /// 取指定会话的 Exec 适配器（传后校验 remote_sha256_via 消费；下方 RusshExecAdapter 实现 ExecChannel）。
    ///
    /// 交出去的是套了 [`fs_sshengine::timeouts::TimedExec`] 的句柄（审计2 #11）：exec 同样持
    /// 会话锁，一次不回话的远端命令会冻住整条会话。包在这里而不是在每个消费方，是因为消费方
    /// 会增加（提交前闸门、传后校验，以后还有别的），而这里是唯一的出口。
    pub async fn exec_adapter_for(
        &self,
        session_id: &str,
    ) -> Result<Arc<dyn fs_sshengine::verify::ExecChannel>, String> {
        let session = self.registry.get(session_id).ok_or("no session")?;
        // LiveSession.handle 是 tokio::sync::Mutex<Handle>（非 Arc 包装），而 Handle 自身非 Clone。
        // RusshExecAdapter 需持有 Arc<Mutex<Handle>>，故取 Arc<LiveSession> 本身。
        Ok(Arc::new(fs_sshengine::timeouts::TimedExec::new(Arc::new(
            RusshExecAdapter { session },
        ))))
    }

    /// 取指定会话的下载沙箱（spec §3.3 远端文件名服务端可控）。
    ///
    /// 三级回退（审计 P1-15）：连接配置 `Profile.sftp.download_sandbox` → 全局设置
    /// `sftp.sandboxRoot` → `data_dir/downloads/<profile_id>`。原实现把前两级读进来存着却从不消费，
    /// 用户在 UI 上改完沙箱目录、保存、下载，文件仍然落在默认目录里——一个静默失效的安全设置
    /// 比没有这个设置更危险，因为用户会据此放心地下载他本不愿放进默认目录的东西。
    ///
    /// 每一级都必须过 `validate_sandbox_root`，不过就 warn 并**回落下一级**（fail-safe），
    /// 而不是直接拿用户给的可疑路径去写（fail-open）：沙箱根本身就是这条链路唯一的越权写防线，
    /// 让一个指向 `C:\Windows\System32` 的符号链接当沙箱根，等于把防线拱手让出。
    pub async fn download_sandbox_for(&self, session_id: &str) -> Result<PathBuf, String> {
        let session = self.registry.get(session_id).ok_or("no session")?;
        Ok(self.sandbox_root_for_profile(&session.profile_id).await)
    }

    /// 三级回退的本体，按 **profile_id** 取（`download_sandbox_for` 与 ZMODEM 装配共用）。
    ///
    /// 抽出来而不是在 ZMODEM 那边照抄一份：这段回退链是越权写的唯一防线，复制一份
    /// 就有两处要同步维护，而「两处沙箱规则不一致」正是最难自证的一类漏洞——用户
    /// 在 UI 上改了沙箱，SFTP 生效、终端内传输不生效，两条路径的落点从此分岔。
    ///
    /// 按 profile_id 而不是 session_id 取，是因为拦截器必须在会话登记**之前**造好
    /// （它是 `PipeOpts.tap`），那时注册表里还查不到这个 session。
    pub async fn sandbox_root_for_profile(&self, profile_id: &str) -> PathBuf {
        // ① 连接级：本连接自己的沙箱设定，优先级最高（用户对单台主机的显式意图）
        if let Some(raw) = self.profile_download_sandbox(profile_id).await {
            if let Some(root) = validate_sandbox_root(&raw, "连接配置 sftp.download_sandbox") {
                return root;
            }
        }
        // ② 全局设置：settings 表 `sftp.sandboxRoot`
        if let Some(raw) = self.settings_string("sftp.sandboxRoot").await {
            if let Some(root) = validate_sandbox_root(&raw, "全局设置 sftp.sandboxRoot") {
                return root;
            }
        }
        // ③ 兜底：per-profile 默认沙箱。目录由调用方 create_dir_all（此处不建，保持纯查询语义）。
        self.data_dir.join("downloads").join(profile_id)
    }

    /// ZMODEM 落盘根（M4a）：与 SFTP 下载同一套三级回退、同一个根目录。
    ///
    /// 刻意**不**另开一个 `zmodem/` 子目录：用户心里「从这台机器下载的东西」只有
    /// 一个地方，分两处只会让人找不到刚传完的文件。
    pub async fn zmodem_sandbox_for(&self, profile_id: &str) -> Result<PathBuf, String> {
        Ok(self.sandbox_root_for_profile(profile_id).await)
    }

    /// 终端内传输配置（M4a；settings 键 `term.zmodem`）。
    ///
    /// 读侧兜底：键缺失一律按**默认开启 + 自动接收**。这与 `session.log` 的「缺失
    /// 即关闭」相反，理由是两者的默认风险不同——日志是往用户磁盘上写他没要求的
    /// 转录，传输是响应用户在终端里亲手敲的 `sz`。后者不响应才是意外。
    pub async fn zmodem_config(&self) -> ZmodemConfig {
        let raw = fs_connmgr::SettingsRepo::new(self.db.pool())
            .get("term.zmodem")
            .await
            .ok()
            .flatten();
        let Some(raw) = raw else {
            return ZmodemConfig::default();
        };
        serde_json::from_str::<ZmodemConfig>(&raw).unwrap_or_default()
    }

    /// 读连接配置里的下载沙箱设定。读不到（profile_id 非法 UUID / 行已删 / 该字段为空）一律
    /// 返回 None 走下一级——沙箱解析不该因为一次数据库读失败就把整个下载功能拖垮。
    async fn profile_download_sandbox(&self, profile_id: &str) -> Option<String> {
        let id = uuid::Uuid::parse_str(profile_id).ok()?;
        let profile = fs_connmgr::ProfileRepo::new(self.db.pool())
            .get(id)
            .await
            .ok()?;
        profile.sftp.download_sandbox
    }

    /// 读 settings 表 boolean 项（Task 20 复用点；spec §2.4）；不存在时返回 fallback。
    pub async fn settings_bool(&self, key: &str, fallback: bool) -> bool {
        let repo = fs_connmgr::SettingsRepo::new(self.db.pool());
        match repo.get(key).await {
            Ok(Some(v)) if v == "true" => true,
            Ok(Some(v)) if v == "false" => false,
            _ => fallback,
        }
    }

    /// 读 settings 表字符串项。settings 的约定值是 JSON（见 `settings_cmd`），但历史写入里
    /// 也有裸串（`settings_bool` 就按裸串 `"true"` 比对）；两种都认，免得一个引号差异让用户
    /// 配的沙箱路径静默失效——那正是 P1-15 要杜绝的那类「设置存了但没生效」。
    async fn settings_string(&self, key: &str) -> Option<String> {
        let raw = fs_connmgr::SettingsRepo::new(self.db.pool())
            .get(key)
            .await
            .ok()??;
        Some(serde_json::from_str::<String>(&raw).unwrap_or(raw))
    }

    /// 会话纯文本日志配置（M4a；settings 键 `session.log`）。
    ///
    /// 读侧兜底：键缺失/烂值一律回落「关闭」——转录是可选功能，配置读不出来时
    /// 沉默关闭比擅自往用户磁盘上写文件正确（后者是把「不确定」解释成「同意」）。
    pub async fn session_log_config(&self) -> SessionLogConfig {
        let Some(raw) = fs_connmgr::SettingsRepo::new(self.db.pool())
            .get("session.log")
            .await
            .ok()
            .flatten()
        else {
            return SessionLogConfig::default();
        };
        serde_json::from_str::<SessionLogConfig>(&raw).unwrap_or_default()
    }

    /// 按配置为一个会话建日志写入器；未开启/目录不可用时返回 `None`（不阻断连接）。
    ///
    /// 目录不主动深层创建到任意位置，但**会** `create_dir_all` 配置的目录本身：
    /// 与沙箱根（P1-15 拒绝创建）不同口径的理由是——沙箱根是用户指定的**既有**
    /// 下载区，写错一个字母不该凭空造树；而日志目录是本程序的产物目录，首次开启
    /// 转录时它本来就不存在，要求用户先手工建目录纯属刁难。
    /// 会话转录目录：配置里那一个，空则回落数据目录下 `logs/sessions`。
    ///
    /// 见 [`session_log_dir`]——这里只是转发，**不要**在本方法里重写回落规则。
    pub async fn session_log_dir_resolved(&self) -> std::path::PathBuf {
        let cfg = self.session_log_config().await;
        session_log_dir(&self.data_dir, &cfg.dir)
    }

    pub async fn build_session_log(
        &self,
        session_id: &str,
        host: &str,
        user: &str,
    ) -> Option<fs_terminal::sessionlog::SessionLog> {
        let cfg = self.session_log_config().await;
        if !cfg.enabled {
            return None;
        }
        let dir = session_log_dir(&self.data_dir, &cfg.dir);
        // 时间戳用 `SystemTime` 自算，不引 time/chrono：app crate 未依赖它们，
        // 为一个文件名加一条依赖不划算（供应链卫生是审计2 #3/#4 的未决条目）。
        // 代价诚实记下：本实现按 **UTC** 拆分年月日时分秒——不做本地时区换算
        // （无时区库即无法正确处理 DST 与偏移）。文件名里的时间因此可能与用户
        // 墙上时钟差几小时；模板可用 {session} 规避歧义，且日志内容本身不含时间戳。
        let now = utc_parts(std::time::SystemTime::now());
        let name = fs_terminal::sessionlog::render_log_name(
            if cfg.template.trim().is_empty() {
                DEFAULT_SESSION_LOG_TEMPLATE
            } else {
                cfg.template.trim()
            },
            &fs_terminal::sessionlog::LogNameCtx {
                host,
                user,
                session: session_id,
                year: now.0,
                month: now.1,
                day: now.2,
                hour: now.3,
                minute: now.4,
                second: now.5,
            },
        );
        let mode = if cfg.append {
            fs_terminal::sessionlog::LogMode::Append
        } else {
            fs_terminal::sessionlog::LogMode::Overwrite
        };
        tracing::info!(path = %dir.join(&name).display(), append = cfg.append, "会话日志已开启");
        Some(fs_terminal::sessionlog::SessionLog::new(
            dir.join(name),
            mode,
        ))
    }
}

/// 会话日志默认命名模板（Xshell 同款「主机_时间戳」形状）。
pub const DEFAULT_SESSION_LOG_TEMPLATE: &str = "{host}_{yyyy}{MM}{dd}_{HH}{mm}{ss}.log";

/// `SystemTime` → UTC 的 (年, 月, 日, 时, 分, 秒)。
///
/// 这里**只做单位换算**，历法算术全在 [`fs_connmgr::civil`]。此前本函数内联了一份
/// 完整的 days-from-civil 逆运算，而计划任务（M4a）也需要同一套算术——两份同样的
/// 历法代码是典型的「两套机制做同一件事」，改一处漏一处。合并之后顺带白拿了那边的
/// 测试覆盖（世纪闰年、2038/2106 溢出点、负偏移、纪元前、正逆互逆往返 1900..2200）。
///
/// 偏移传 0 = UTC：会话日志的文件名口径不变（见调用点关于「不做本地时区换算」的说明）。
pub(crate) fn utc_parts(t: std::time::SystemTime) -> (u32, u32, u32, u32, u32, u32) {
    let secs = t
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let p = fs_connmgr::civil::parts_at(secs, 0);
    (p.year as u32, p.month, p.day, p.hour, p.minute, p.second)
}

/// 终端内传输配置（settings 键 `term.zmodem`；前端 SettingsDialog 写入）。
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub struct ZmodemConfig {
    /// 总开关。关闭时连拦截器都不装（fan-out 上没有这一环，零开销）。
    pub enabled: bool,
    /// 自动接收：终端字节流里认出 `sz` 的起始帧就自动开始收。
    ///
    /// 关掉它并不等于关掉整个功能——上传（`rz`）仍可用，因为上传是由人点「选择
    /// 文件」发起的，不依赖自动检测。想要的是「别让远端单方面往我盘上写东西」
    /// 的用户，关这一项即可。
    pub auto_receive: bool,
    /// 下载去向（2026-08-26 重设计）。
    ///
    /// `None` = 用户还没做过第一次选择（「首次下载时引导」的判定依据）。
    /// 旧设置 JSON 里没有这个键，`#[serde(default)]` 落到 `None`，天然把
    /// 存量用户全部视作「首次」，平滑进入引导而不是沿用旧行为。
    pub download: Option<ZmodemDownloadConfig>,
}

/// 下载去向的两种模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ZmodemDownloadMode {
    /// 固定目录：无冲突直接落，不打扰。
    Fixed,
    /// 每次都问一次目录。
    Ask,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct ZmodemDownloadConfig {
    pub mode: ZmodemDownloadMode,
    /// 目标目录（`mode: fixed` 时有意义）。
    ///
    /// 由 `zmodem_finalize` / 设置页经 `local_list` 体系选出的路径——用户主目录
    /// 之内的绝对路径。**不**在读取侧再校验一次：写入口只有一个（本字段随整个
    /// 设置 JSON 落库），读取侧若再判，两份判定迟早分叉，而分叉的那一侧就是
    /// 能把文件挪到任意位置的另一侧。
    pub dir: String,
}

impl Default for ZmodemConfig {
    fn default() -> Self {
        // 默认全开：用户在终端里敲 `sz`/`rz` 就是显式意图，不响应才是意外。
        // download 刻意默认 None：第一次下载必须引导用户做一次去向选择，
        // 而不是替他猜一个目录（猜错了用户根本不知道文件去了哪——那正是
        // 这次重设计要修的问题 2）。
        Self {
            enabled: true,
            auto_receive: true,
            download: None,
        }
    }
}

/// 会话纯文本日志配置（settings 键 `session.log`；前端 SettingsDialog 写入）。
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub struct SessionLogConfig {
    /// 总开关。默认**关**：转录会把命令与输出明文落盘，必须是用户显式选择。
    pub enabled: bool,
    /// 落盘目录；空 = 数据目录下 `logs/sessions`。
    pub dir: String,
    /// 命名模板；空 = [`DEFAULT_SESSION_LOG_TEMPLATE`]。
    pub template: String,
    /// true = 追加（断线重连续写同一文件），false = 覆盖。
    pub append: bool,
}

impl Default for SessionLogConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            dir: String::new(),
            template: DEFAULT_SESSION_LOG_TEMPLATE.to_string(),
            append: true,
        }
    }
}

/// 沙箱根校验（审计 P1-15）：绝对路径 + 存在 + 是目录 + 末段不是符号链接。
///
/// 三条校验各自挡一类事故：
/// ① 相对路径——会随进程 cwd 漂移，今天落在安装目录、明天落在用户双击时所在的任意目录；
/// ② 非目录/不存在——不主动 `create_dir_all`：用户配错一个字母就凭空造出一棵目录树，且掩盖了
///    「这条配置是错的」这个事实；配置指向的目录必须是用户自己确实准备好的；
/// ③ 末段符号链接——沙箱根若本身是软链，`resolve_within` 里以它 canonicalize 出的 root
///    就是软链的目标，「沙箱内」的判定随之整体平移到软链指向的地方，围墙原地搬家。
///    只查末段：中间分量的软链由 canonicalize 一并解析进 root，仍落在用户显式配置的那条路径上。
///
/// 任一不过返回 None（调用方回落下一级），并记 warn——静默回落会让用户永远查不出
/// 「我明明配了沙箱，文件怎么还在默认目录」。
fn validate_sandbox_root(raw: &str, origin: &str) -> Option<PathBuf> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None; // 空串 = 未配置，不算错误，静默走下一级
    }
    let path = Path::new(trimmed);
    if !path.is_absolute() {
        tracing::warn!(
            origin,
            path = trimmed,
            "下载沙箱根不是绝对路径，已忽略并回落下一级"
        );
        return None;
    }
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => {
            tracing::warn!(
                origin,
                path = trimmed,
                "下载沙箱根是符号链接（沙箱边界会随其目标平移），已忽略并回落下一级"
            );
            return None;
        }
        Ok(_) => {}
        Err(e) => {
            tracing::warn!(origin, path = trimmed, error = %e, "下载沙箱根不可访问，已忽略并回落下一级");
            return None;
        }
    }
    let canon = match path.canonicalize() {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(origin, path = trimmed, error = %e, "下载沙箱根无法规范化，已忽略并回落下一级");
            return None;
        }
    };
    if !canon.is_dir() {
        tracing::warn!(
            origin,
            path = trimmed,
            "下载沙箱根不是目录，已忽略并回落下一级"
        );
        return None;
    }
    Some(canon)
}

/// per-target 锁的端点键（审计 P0-3 的修正）：规范化的 `host:port`。
///
/// 引擎按 `remote:{端点}:{规范化远端路径}` 组键，串行化对同一个远端文件的并发写
/// （见 `fs_sshengine::transfer::TransferJob::target_endpoint`）。这个键的粒度必须与
/// **物理文件**对齐，而「物理文件」的身份是 `(主机, 路径)`——不含任何连接身份。
///
/// 本函数的前身是 `transfer_ns(session_id, generation)`，填的是连接身份，比物理文件细一层，
/// 于是对同一台主机的两条连接会拿到两把不同的锁，同时写同一个 `.fspart`，最后各自 rename
/// 一次，**两件都报 Done**——用户得到的是两份内容按块拼起来的文件，而 UI 上一切正常。
/// 这不是要构造才出现的边角：同一台服务器开两个标签页是日常用法，
/// 「一个连接用来跑命令、另一个专门传文件」更是 Xshell/Xftp 用户的固定习惯；
/// 重连也会撞上（`shutdown_session_subsystems` 的宽限期是有界的，超时即放行旧 worker）。
///
/// 归一化只做能确定的那几步：
/// - 去首尾空白；主机名按 ASCII 转小写（DNS 大小写不敏感，`Example.COM` 与 `example.com`
///   是同一台机器，profile 里两种写法都会出现）；
/// - 去掉 FQDN 的结尾点（`example.com.` 与 `example.com` 同义）；
/// - IPv6 统一收进方括号：`::1` 与 `[::1]` 两种写法归一，同时让 `host:port` 的最后一段
///   永远是端口，键不会有歧义。
///
/// 做不到的（如实记下，不假装解决）：`example.com` 与它解析出的 IP 仍是两把锁——判定它们
/// 相等需要一次 DNS 往返，而轮询 DNS 下这个往返的结果本身还不稳定。
///
/// 跳板机也刻意**不进键**：经不同跳板抵达的同一个 `10.0.0.5:22` 有可能是两台不同的机器
/// （两个私网），把它们判成同一个目标最多多报一次「目标忙」；反过来判成两个则会写坏文件。
/// 用户名同理不进键——同一路径不会因为换个用户去写就变成另一个文件。
/// 这两处都往「宁可多锁」的一侧偏。
pub fn transfer_endpoint(host: &str, port: u16) -> String {
    let h = host.trim().trim_end_matches('.');
    let h = h.strip_prefix('[').unwrap_or(h);
    let h = h.strip_suffix(']').unwrap_or(h);
    let h = h.to_ascii_lowercase();
    if h.contains(':') {
        format!("[{h}]:{port}") // IPv6 字面量
    } else {
        format!("{h}:{port}")
    }
}

/// 连接配置对应的端点键。会话装配时取一次存进 `LiveSession`，之后不再回查数据库。
///
/// 取一次而不是每次提交现查，是因为 profile 可改：用户在连接存活期间把 host 改掉，
/// 现查就会让同一条连接上的前后两件传输拿到两把锁，退化成本条审计要修的缺陷本身。
/// 连接建立时用的是哪个 host:port，锁就该按那个走。
pub fn transfer_endpoint_of(profile: &fs_connmgr::Profile) -> String {
    // 只取 host/port：id、name、username、认证方式都不参与——它们区分的是「谁去连」，
    // 而锁要区分的是「写哪个文件」。两个 profile 指向同一台服务器（例如 prod-root 与
    // prod-deploy）是常见配置，它们必须共用一把锁。
    transfer_endpoint(&profile.host, profile.port)
}

/// 把一组已解析的对端地址折成端点键（审计2 #13）。
///
/// 拆成纯函数是为了可测：`lookup_host` 要 DNS，拿它当被测面写出来的就是会随网络环境
/// 变红的用例。真正需要钉死的逻辑全在这里——IPv6 要进方括号、端口要跟着地址走、
/// 同一地址出现多次（A 记录与 AAAA 记录指向同一台、或解析器重复返回）只留一份。
fn endpoint_aliases_from(addrs: impl IntoIterator<Item = std::net::SocketAddr>) -> Vec<String> {
    let mut out: Vec<String> = addrs
        .into_iter()
        .map(|a| transfer_endpoint(&a.ip().to_string(), a.port()))
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// 解析 profile 的 host，产出与 `transfer_endpoint_of` **同义**的其它端点拼法。
///
/// 这是 `transfer_endpoint` 文档里那句「做不到的：`example.com` 与它解析出的 IP 仍是两把锁」
/// 的兑现。做法不是「用 IP 取代域名」——那会引入一个新洞：解析成功的一件按 IP 上锁、
/// 解析失败的一件按域名上锁，两者反而完全错开。做法是**并集**：域名那一维始终保留，
/// 解析出的地址各加一维（见 `TransferJob::endpoint_aliases`）。这样 DNS 轮询最多让两次
/// 解析给出不同的地址集，交集里仍有域名那一维；而域名与 IP 两种连法则在地址那一维重合。
///
/// 走跳板时**不解析**：那时的对端是跳板机，本机解析出的地址与真正落字节的那台没有关系。
/// 更糟的是两个不同私网里同为 `10.0.0.5` 的两台机器会被折成一个键，从「漏挡」变成「误挡」。
/// 宁可退回单维。
///
/// 解析失败一律当空处理：这一维是加固，不是前置条件，绝不能让一次 DNS 抖动挡住传输。
pub async fn transfer_endpoint_aliases_of(profile: &fs_connmgr::Profile) -> Vec<String> {
    if !profile.jump.is_empty() {
        return Vec::new();
    }
    let host = profile.host.trim().trim_end_matches('.');
    let host = host.strip_prefix('[').unwrap_or(host);
    let host = host.strip_suffix(']').unwrap_or(host);
    match tokio::net::lookup_host((host, profile.port)).await {
        Ok(addrs) => {
            let out = endpoint_aliases_from(addrs);
            // 已经与主键相同的那一份（host 本来就是 IP 字面量）留着无害：`try_lock_targets`
            // 会去重。这里不提前剔，是为了让「解析成功但只得到自己」与「解析失败」在日志上
            // 仍然可分。
            tracing::debug!(
                aliases = out.len(),
                "传输目标锁：端点别名解析完成（不记录地址本身，见审计 P1-19）"
            );
            out
        }
        Err(e) => {
            tracing::debug!(error = %e, "传输目标锁：端点别名解析失败，退回单维");
            Vec::new()
        }
    }
}

/// RusshExecAdapter 实现：满足 verify::ExecChannel，单测免注入容器（Task 21 后端唯一新增适配器）。
struct RusshExecAdapter {
    session: std::sync::Arc<crate::sessions::LiveSession>,
}

#[async_trait::async_trait]
impl fs_sshengine::verify::ExecChannel for RusshExecAdapter {
    async fn exec_once(
        &self,
        cmd: &str,
    ) -> Result<fs_sshengine::verify::ExecOutput, fs_sshengine::Error> {
        use fs_sshengine::timeouts::{with_timeout, CONTROL_TIMEOUT};
        // 开通道与发 exec 请求都是控制面往返（响应体几十字节，与命令耗时无关），按控制面预算；
        // 命令本体的等待在 `run_exec_channel` 内部按 EXEC_TOTAL_TIMEOUT 计。三段都在会话锁内。
        let handle = self.session.handle.lock().await;
        let mut channel = with_timeout(
            "exec 开通道",
            CONTROL_TIMEOUT,
            handle.channel_open_session(),
        )
        .await?
        .map_err(|e| fs_sshengine::Error::Transfer(e.to_string()))?;
        with_timeout("exec 请求", CONTROL_TIMEOUT, channel.exec(true, cmd))
            .await?
            .map_err(|e| fs_sshengine::Error::Transfer(e.to_string()))?;
        Ok(fs_sshengine::verify::run_exec_channel(&mut channel).await)
    }
}

/* ── 连接期 host facts 采集（M2 出口第 15 项的接线半边）─────────────────────
 *
 * `fs_ai::context` 早就有 `HOST_FACTS_COMMAND` 与 `parse_host_facts`（7 例覆盖降级路径），
 * 缺的一直是「连接建立后真的去 exec 一次」这条接线。
 *
 * # 为什么缓存放 AppState 而不是 LiveSession
 *
 * `LiveSession` 由 `session_cmd` 以结构体字面量构造，里面躺着 russh 的 `Handle` 与
 * `ChannelWriteHalf`——加字段要改所有构造点，而那几处牵连一次真实握手的产物。
 *
 * 更本质的理由是语义不同：`LiveSession` 的每个字段都是「这条连接是什么」，
 * 而 host facts 是**可选、可失败、迟到**的观测结果。把它塞进 LiveSession 就得给它一个
 * `Mutex<Option<_>>`，那等于在一个「连接本体」结构里承认「这一项可能没有」——
 * 那不是它该承担的语义。
 */

/// 已采集的 host facts，按 session_id。
///
/// 一条会话只采一次：`uname`/`shell` 这些在连接存活期内不会变，而每次组装上下文
/// 都去 exec 一遍等于给每次 AI 请求加一次 SSH 往返。
pub type HostFactsCache = Arc<Mutex<HashMap<String, fs_ai::context::HostFacts>>>;

/// 连接建立后采集一次 host facts。
///
/// # 一律 spawn，绝不 await
///
/// 出口原文：「采集失败**不阻塞连接**」。这条要求不只是「捕获错误」——
/// 即便成功，一次 exec 也是几百毫秒的 SSH 往返，而这段时间用户正盯着「正在连接」。
/// 所以整个采集在后台跑，`session_open` 不等它。
///
/// 代价是「刚连上就问 AI」的第一个请求可能拿不到 facts。那是可接受的：
/// 上下文少一行 uname 只是让模型少一点信息，而让连接慢半秒是每次都付的成本。
pub fn spawn_host_facts_collection(state: Arc<AppState>, session_id: String) {
    tokio::spawn(async move {
        // 取 adapter 本身可能失败（会话在我们 spawn 之后就关了）——那不是错误，
        // 是正常的竞态，静默返回。
        let Ok(exec) = state.exec_adapter_for(&session_id).await else {
            return;
        };
        let out = match exec.exec_once(fs_ai::context::HOST_FACTS_COMMAND).await {
            Ok(o) => o,
            Err(e) => {
                // 记一行就够。这是**降级路径**，不是故障：极简容器里没有 uname、
                // 受限 shell 不给 exec 通道，都会走到这里，而连接本身好得很。
                tracing::debug!(session = %session_id, error = %e, "host facts 采集失败（不影响连接）");
                return;
            }
        };
        // `HOST_FACTS_COMMAND` 自带 `2>/dev/null || true`，所以非零退出码本身不说明什么；
        // 真正的判据是解析出了几项。
        let facts = fs_ai::context::parse_host_facts(&out.stdout);
        if facts.is_empty() {
            tracing::debug!(session = %session_id, "host facts 一项都没采到（可能是极简容器）");
            return;
        }
        state
            .host_facts
            .lock()
            .await
            .insert(session_id.clone(), facts);
        tracing::debug!(session = %session_id, "host facts 已缓存");
    });
}

/// 便携模式的标记文件名（M4b「便携打包」）。
///
/// 放在**可执行文件同目录**。存在即启用便携模式，数据落到旁边的 [`PORTABLE_DATA_DIR`]。
///
/// 用标记文件而不是命令行开关，是因为便携模式必须在**双击启动**时也生效——
/// U 盘上的程序是被双击的，没人会去配启动参数。也不用环境变量：那是全局的，
/// 一个装了两份（一份安装、一份便携）的用户会让两份都跟着变。
pub const PORTABLE_MARKER: &str = "portable.txt";

/// 便携模式下数据落在可执行文件旁的哪个子目录。
pub const PORTABLE_DATA_DIR: &str = "data";

/// 应用数据目录（Vault / 日志 / SQLite 库的落点），解析失败即 Err（审计 P1-21）。
///
/// 原实现在 `dirs::config_dir()` 返回 None 时回落 `PathBuf::from(".")`。cwd 是进程启动方决定的、
/// 完全不可控的目录——用户从桌面双击是桌面、从文件管理器某个目录打开就是那个目录，于是
/// 加密的 Vault、连接库和日志会散落一地；更糟的是下一次启动 cwd 变了，程序看不到上次的 Vault，
/// 表现为「我的连接全没了」，而旧文件仍带着密钥材料留在原处。
/// 这类目录只有两种正确结局：落在确定的位置，或者明确失败。绝不能是「随便找个地方落下」。
///
/// # 便携模式（M4b）
///
/// 可执行文件同目录下有 [`PORTABLE_MARKER`] 时，数据改落 `<exe 目录>/data`。
///
/// **这不是上面那个「回落 cwd」的复活。** 两者的差别是全部要点所在：
/// - 锚点是**可执行文件所在目录**，不是 cwd。exe 的位置由用户放在哪儿决定，
///   双击、命令行、快捷方式启动都是同一个答案；cwd 三种启动方式三个答案。
/// - 是**用户显式放了一个文件**，不是解析失败后的兜底。没有那个文件就绝不启用。
///
/// 解析不出 exe 路径时**不悄悄退回安装模式**：用户放了标记文件却被当成没放，
/// 数据会落进 `%APPDATA%` 而他以为在 U 盘上——拔下 U 盘走人，数据留在别人的机器上。
/// 那是这个功能最坏的失败方式，所以宁可明确失败。
pub fn data_dir_or_err() -> Result<PathBuf, String> {
    if let Some(dir) = portable_data_dir()? {
        return Ok(dir);
    }
    dirs::config_dir()
        .map(|d| d.join("future-shell"))
        .ok_or_else(|| {
            "无法解析用户配置目录（%APPDATA% / $XDG_CONFIG_HOME / ~/Library 均不可用）：\
             Vault、日志与数据库必须落在确定目录，拒绝回落当前工作目录"
                .to_string()
        })
}

/// 便携模式的数据目录；未启用返回 `Ok(None)`。
fn portable_data_dir() -> Result<Option<PathBuf>, String> {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        // 拿不到 exe 路径就无从判断有没有标记文件——这时按「没启用」走安装模式是对的：
        // 我们并不知道用户放没放标记，而 %APPDATA% 至少是个确定的位置。
        Err(_) => return Ok(None),
    };
    let Some(dir) = exe.parent() else {
        return Ok(None);
    };
    let marker = dir.join(PORTABLE_MARKER);
    if !marker.is_file() {
        return Ok(None);
    }
    let data = dir.join(PORTABLE_DATA_DIR);
    // 目录建不出来 = 这个位置不可写（典型：解压到了 Program Files）。
    // 这时**必须报错**，不能退回 %APPDATA%：用户放了标记文件，他的意图是
    // 「数据跟着程序走」。悄悄落到别处等于在他不知情时把数据留在这台机器上。
    std::fs::create_dir_all(&data).map_err(|e| {
        format!(
            "便携模式已启用（发现 {}），但数据目录 {} 建不出来：{e}。\
             便携模式要求程序所在目录可写——把整个程序目录解压到 U 盘或用户目录下再试。",
            marker.display(),
            data.display()
        )
    })?;
    Ok(Some(data))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 造一个只有 host/port/身份字段有意义的连接配置。
    ///
    /// 走 serde 而不是结构体字面量：`Profile` 的可选字段都带 `#[serde(default)]`，用 JSON 建
    /// 只需写出本用例真正关心的那几个，日后给 `Profile` 加字段也不会把这里编译坏——
    /// 而那种"加字段导致一堆无关测试要改"的摩擦，最后总是以删测试收场。
    fn profile(id: &str, name: &str, user: &str, host: &str, port: u16) -> fs_connmgr::Profile {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "name": name,
            "group_path": null,
            "host": host,
            "port": port,
            "username": user,
        }))
        .expect("测试用 Profile 必须能反序列化（字段名或必填项变了就该在这里炸）")
    }

    /// 审计 P1 的核心回归：目标锁的键必须由**物理目标**决定，与「谁去连、连了第几次」无关。
    ///
    /// 旧实现填的是 `"{session_id}#{generation}"`。同一台服务器开两个标签页——
    /// 这是 Xshell/Xftp 用户的日常（一个跑命令、一个专管传文件）——两条连接拿到两把不同的锁，
    /// 同时上传同一路径就会交错写同一个 `.fspart`，各自 rename 一次，**两件都报 Done**。
    /// 用户拿到的是两份内容按块拼起来的文件，UI 上一切正常，传后校验也只对得上其中一件。
    #[test]
    fn same_server_reached_by_different_identities_shares_one_key() {
        // 两条不同的连接配置（不同 id / 名字 / 登录用户），指向同一台机器
        let root = profile(
            "11111111-1111-4111-8111-111111111111",
            "prod-root",
            "root",
            "prod.example.com",
            22,
        );
        let deploy = profile(
            "22222222-2222-4222-8222-222222222222",
            "prod-deploy",
            "deploy",
            "prod.example.com",
            22,
        );
        assert_eq!(
            transfer_endpoint_of(&root),
            transfer_endpoint_of(&deploy),
            "指向同一台服务器的两个连接配置必须共用一把目标锁：\
             键里一旦掺进连接身份（session_id / generation / profile id / 用户名），\
             两条连接就会同时写同一个 .fspart 并双双报 Done"
        );
    }

    /// 另一侧：粒度不能粗到把不同机器并成一个目标，否则互不相干的传输互相报「目标忙」。
    #[test]
    fn different_servers_get_different_keys() {
        let a = profile(
            "11111111-1111-4111-8111-111111111111",
            "a",
            "u",
            "a.example.com",
            22,
        );
        let b = profile(
            "11111111-1111-4111-8111-111111111111",
            "a",
            "u",
            "b.example.com",
            22,
        );
        // 同一台机器上的两个 sshd（22 与 2222）当作两个端点：多锁只是多报一次「忙」，
        // 而端口本就是端点身份的一部分，丢掉它没有任何收益。
        let a_alt = profile(
            "11111111-1111-4111-8111-111111111111",
            "a",
            "u",
            "a.example.com",
            2222,
        );
        assert_ne!(
            transfer_endpoint_of(&a),
            transfer_endpoint_of(&b),
            "两台不同主机不得共用目标锁"
        );
        assert_ne!(
            transfer_endpoint_of(&a),
            transfer_endpoint_of(&a_alt),
            "端口必须进键，否则 22 与 2222 上的同名路径会被误锁成一个目标"
        );
    }

    /// 同一台主机的几种写法要折成同一个键——profile 是用户手打的，大小写与结尾点都会出现。
    #[test]
    fn host_spellings_fold_together() {
        let canonical = transfer_endpoint("prod.example.com", 22);
        for spelling in [
            "prod.example.com",
            "PROD.Example.COM",
            "prod.example.com.", // FQDN 的结尾点
            "  prod.example.com  ",
        ] {
            assert_eq!(
                transfer_endpoint(spelling, 22),
                canonical,
                "{spelling} 与 prod.example.com 是同一台机器，必须组出同一把锁的键"
            );
        }
    }

    /// IPv6：两种写法折成一个，且键末段永远是端口（`[..]:port`），不与地址里的冒号混淆。
    #[test]
    fn ipv6_literals_are_folded_and_unambiguous() {
        assert_eq!(transfer_endpoint("::1", 22), transfer_endpoint("[::1]", 22));
        assert_eq!(transfer_endpoint("[::1]", 22), "[::1]:22");
        assert_ne!(
            transfer_endpoint("::1", 22),
            transfer_endpoint("::1", 2222),
            "IPv6 字面量自带冒号，端口仍须可区分"
        );
    }

    // ───────────────────────── 端点别名（审计2 #13）
    //
    // 被测的是 `endpoint_aliases_from` 而不是 `transfer_endpoint_aliases_of`：后者要 DNS，
    // 拿它当被测面写出来的用例会随网络环境、解析器缓存、CI 沙箱的出网策略变红——
    // 那种用例最终一定会被 `#[ignore]` 掉，等于没有。解析这一步交给 `tokio::net`，
    // 我们钉死的是拿到地址之后的那段纯逻辑。

    fn sock(s: &str) -> std::net::SocketAddr {
        s.parse().expect("测试里的地址字面量必须可解析")
    }

    /// 别名键必须与 `transfer_endpoint` 同一套拼法——否则两条连接各按各的拼，永远不相交。
    ///
    /// IPv6 是这里唯一真正会写错的一处：`SocketAddr` 的 `Display` 自己就带方括号
    /// （`[::1]:22`），若直接用它的字符串，键会变成 `[[::1]:22]:22` 之类的东西。
    /// 走 `ip()` + `port()` 分别取、再交给 `transfer_endpoint` 拼，才是同一条路径。
    #[test]
    fn endpoint_aliases_use_the_same_spelling_as_the_primary_key() {
        assert_eq!(
            endpoint_aliases_from([sock("10.9.9.9:22")]),
            vec![transfer_endpoint("10.9.9.9", 22)],
            "IPv4 别名必须与主键同拼法"
        );
        assert_eq!(
            endpoint_aliases_from([sock("[2001:db8::1]:22")]),
            vec![transfer_endpoint("2001:db8::1", 22)],
            "IPv6 别名必须与主键同拼法：方括号只能来自 transfer_endpoint，不能叠加"
        );
    }

    /// 端口跟着地址走，不是写死的 22——非标准端口的服务器在生产里很常见。
    #[test]
    fn endpoint_aliases_carry_the_port_from_the_address() {
        let out = endpoint_aliases_from([sock("10.9.9.9:2222")]);
        assert_eq!(out, vec!["10.9.9.9:2222".to_string()]);
        assert!(
            !out.contains(&"10.9.9.9:22".to_string()),
            "端口丢了会让 2222 上的传输去锁 22 上的目标，既漏挡又误挡"
        );
    }

    /// 重复地址只留一份。解析器返回重复项、或多条 A 记录指向同一台，都会走到这里。
    ///
    /// 留着重复项不会写坏文件（`try_lock_targets` 还会再去重一次），但会让这一层的输出
    /// 随解析器实现抖动，日志里的 `aliases = N` 也不再有意义。
    #[test]
    fn endpoint_aliases_are_deduped_and_sorted() {
        let out = endpoint_aliases_from([
            sock("10.0.0.2:22"),
            sock("10.0.0.1:22"),
            sock("10.0.0.2:22"),
        ]);
        assert_eq!(
            out,
            vec!["10.0.0.1:22".to_string(), "10.0.0.2:22".to_string()],
            "别名集必须去重且有序"
        );
    }

    /// 一台机器同时有 A 和 AAAA 记录时，两个地址族都要进别名——只留一个的话，
    /// 一条走 IPv4、一条走 IPv6 的连接就再也不相交了。
    #[test]
    fn endpoint_aliases_keep_both_address_families() {
        let out = endpoint_aliases_from([sock("10.9.9.9:22"), sock("[2001:db8::1]:22")]);
        assert_eq!(out.len(), 2, "双栈主机的两个地址族都必须留下");
        assert!(out.contains(&"10.9.9.9:22".to_string()));
        assert!(out.contains(&"[2001:db8::1]:22".to_string()));
    }

    /// 空输入产出空集，不是一条空串键。`format!("remote:{ep}:{p}")` 里塞进空端点
    /// 会造出 `remote::/path` 这种键，跨主机误撞。
    #[test]
    fn endpoint_aliases_from_nothing_is_empty() {
        assert!(endpoint_aliases_from([]).is_empty());
    }

    /// 走跳板的 profile 一律不解析：本机解析出的地址是「本机看到的那个名字」，
    /// 而字节实际落在跳板另一侧的机器上，两者没有关系。
    ///
    /// 这不是保守起见的多余判断，而是一条**会造成误挡**的边：两个不同私网里同为
    /// `10.0.0.5:22` 的两台机器，经各自跳板抵达时会被折成同一把键，此后对它们的并发传输
    /// 会互相报「目标忙」。漏挡只是回到修复前，误挡是新引入的故障。
    ///
    /// 用 IP 字面量做输入，`lookup_host` 对纯数字地址直接短路，全程不出网、不查 DNS——
    /// 这条用例在断网的 CI 沙箱里也必须是确定的。
    #[tokio::test]
    async fn jump_host_profiles_get_no_endpoint_aliases() {
        let mut direct = profile(
            "33333333-3333-4333-8333-333333333333",
            "direct",
            "u",
            "127.0.0.1",
            22,
        );
        assert_eq!(
            transfer_endpoint_aliases_of(&direct).await,
            vec!["127.0.0.1:22".to_string()],
            "直连 profile 必须解析出别名，否则下面那条断言用空集也能过"
        );
        // 只有「跳板链非空」这一件事被判定，跳板本身的字段与本用例无关
        direct.jump = vec![fs_connmgr::JumpHop {
            host: "bastion.example.com".into(),
            port: 22,
            ..Default::default()
        }];
        assert!(
            transfer_endpoint_aliases_of(&direct).await.is_empty(),
            "经跳板抵达时本机解析出的地址不是对端，必须退回单维"
        );
    }

    // ───────────────────────── 传后校验槽位握手（审计 P1-2 / P2「Settled 孤儿」）
    //
    // 这套判定此前只以闭包形式内联在两个 async Tauri 命令里，没有任何测试能碰到它——
    // 审计对两条泄漏路径的结论正是「都无任何测试覆盖」。抽成纯函数后，整台状态机可以在
    // 一个 HashMap 上直接演，不需要 SSH 会话、不需要 Tauri 运行时。
    //
    // 每个用例的**收尾断言都是 `plans.is_empty()`**：这条缺陷的本体不是「某次校验没跑」，
    // 而是「表里留下了一条永远没人来收的记录」，所以判据只能是表的最终大小。

    /// 造一份带/不带校验意图的登记。`entry` 的内容与本组用例无关，只关心 Some/None。
    fn plan_for(session_id: &str, with_entry: bool) -> PendingVerify {
        PendingVerify {
            session_id: session_id.to_string(),
            entry: with_entry.then(|| fs_sshengine::verify::VerifyEntry {
                direction: fs_sshengine::transfer::Direction::Down,
                remote: "/tmp/x".into(),
                local: std::path::PathBuf::from("/tmp/x"),
                plan: fs_sshengine::verify::VerifyPlan {
                    expect: None,
                    degraded: true,
                },
            }),
        }
    }

    /// 常规顺序（登记先到、终态后到）：一来一回，表必须空。
    #[test]
    fn register_then_settle_leaves_nothing_behind() {
        let mut plans = HashMap::new();
        assert!(matches!(
            verify_register(&mut plans, 1, || plan_for("s1", true)),
            RegisterOutcome::Pending
        ));
        assert_eq!(plans.len(), 1, "登记必须留痕，否则终态侧无从判断该不该等");

        let out = verify_settle(&mut plans, 1, "s1", true);
        assert!(
            matches!(out, SettleOutcome::Plan(_)),
            "登记好的意图必须取得回"
        );
        assert!(plans.is_empty(), "取走之后表里不该留任何东西");
    }

    /// 竞态顺序（终态先到、登记后到）：标记必须被登记方取走，且带出正确的 done。
    /// 这是 P1-2 那个「传输成功但永远没有校验结果」窗口的正向回归。
    #[test]
    fn settle_then_register_hands_the_terminal_state_over() {
        for done in [true, false] {
            let mut plans = HashMap::new();
            assert!(matches!(
                verify_settle(&mut plans, 7, "s1", done),
                SettleOutcome::Marked
            ));
            assert_eq!(plans.len(), 1, "无人登记时必须留标记，绝不静默跳过");

            let out = verify_register(&mut plans, 7, || plan_for("s1", true));
            match out {
                RegisterOutcome::AlreadySettled { done: d } => assert_eq!(d, done),
                _ => panic!("终态先到时登记方必须拿到 AlreadySettled{{done: {done}}}"),
            }
            assert!(plans.is_empty(), "标记被取走后表必须空");
        }
    }

    /// 审计 P2 路径一：用户关掉 `transfer.verifyAfterTransfer`。
    ///
    /// 原实现在「没有校验意图」时**什么都不登记**，于是事件泵在终态查表落空、插入一枚
    /// `Settled`，而登记方早已跑完、永远不会回来取（作业 id 进程级单调，永不复用）。
    /// 每完成一件传输泄漏一条。修法是「不校验」也照样登记——占位本身就是握手的一半。
    #[test]
    fn a_job_with_no_verify_intent_still_registers_and_settles_clean() {
        let mut plans = HashMap::new();
        verify_register(&mut plans, 3, || plan_for("s1", false));
        assert_eq!(
            plans.len(),
            1,
            "「不校验」也必须占位，否则终态侧会留下孤儿标记"
        );

        match verify_settle(&mut plans, 3, "s1", true) {
            SettleOutcome::Plan(p) => assert!(p.entry.is_none(), "占位槽不该凭空造出校验意图"),
            _ => panic!("占位槽必须被终态侧取走"),
        }
        assert!(
            plans.is_empty(),
            "关掉传后校验后，一件传输不得在表里留下任何残渣"
        );
    }

    /// 审计 P2 路径二：关停超时 → retain 清掉 Planned → 仍在跑的 worker 之后才发终态。
    ///
    /// 这条不是调度巧合：`TransferManager::shutdown` 在宽限期内没收完就返回 false，而
    /// `shutdown_session_subsystems` 把这当作预期结果继续往下走。孤儿由事件泵退出时自收。
    #[test]
    fn late_terminal_after_teardown_is_reaped_when_the_pump_exits() {
        let mut plans = HashMap::new();
        verify_register(&mut plans, 5, || plan_for("s1", true));

        // 会话拆除：本会话的意图作废。
        plans.retain(|_, slot| verify_slot_survives_teardown(slot, "s1"));
        assert!(plans.is_empty());

        // 超时后仍在跑的 worker 现在才发出终态——登记方早已跑完，不会再来。
        let mut settled_here = Vec::new();
        if let SettleOutcome::Marked = verify_settle(&mut plans, 5, "s1", false) {
            settled_here.push(5);
        }
        assert_eq!(
            plans.len(),
            1,
            "此刻确实留下了一枚标记（修复前它就停在这里）"
        );

        // 事件流结束：管理器与全部 worker 都没了，标记是确定的孤儿。
        verify_reap_settled(&mut plans, &settled_here);
        assert!(plans.is_empty(), "泵退出后不得留下任何 Settled 孤儿");
    }

    /// 拆除只作废**本会话**的槽位：`Planned` 与 `Settled` 同一判据。
    ///
    /// 原实现的 `Settled { .. } => true` 把标记无条件保留，理由是「登记方会在同一轮内取走」——
    /// 在登记方已经跑过的情形下这个前提不成立，那正是孤儿的来源。
    #[test]
    fn teardown_discards_only_this_sessions_slots_including_settled_marks() {
        let mut plans = HashMap::new();
        verify_register(&mut plans, 1, || plan_for("doomed", true));
        verify_register(&mut plans, 2, || plan_for("survivor", true));
        verify_settle(&mut plans, 3, "doomed", true); // 标记
        verify_settle(&mut plans, 4, "survivor", true); // 标记

        plans.retain(|_, slot| verify_slot_survives_teardown(slot, "doomed"));

        let mut left: Vec<u64> = plans.keys().copied().collect();
        left.sort_unstable();
        assert_eq!(left, vec![2, 4], "只有 doomed 的意图与标记该被收走");
    }

    /// 回收只认传进来的这批 id——**两个方向都要钉**。
    ///
    /// 反面一是「按 session_id 批量清」：重连会为同一会话起新一代管理器与新泵，旧泵那样清
    /// 会把新一代正在等终态的 `Planned` 一并抹掉——校验静默失效比留几个孤儿糟得多。
    ///
    /// 反面二是「把表里所有 `Settled` 一扫而空」。这条更隐蔽，只留 `Planned` 作对照是抓不住的
    ///（把 `verify_reap_settled` 改成 `retain(|_, s| !matches!(s, Settled{..}))` 曾经全绿）。
    /// 它的后果恰恰与孤儿相反：把**别的泵刚留下、其登记方还在路上**的标记提前抹掉，那位登记方
    /// 随后落到「表里空着」分支登记一枚 `Planned`，而那一代的事件流早已结束，再没有终态来认领
    /// ——校验就此无声地不执行，且换来一枚真正永久的 `Planned` 孤儿。
    #[test]
    fn reaping_never_touches_slots_the_pump_did_not_create() {
        let mut plans = HashMap::new();
        verify_register(&mut plans, 10, || plan_for("s1", true)); // 新一代正在等终态
        verify_settle(&mut plans, 11, "s1", true); // 本泵留下的标记（该收）
        verify_settle(&mut plans, 12, "s1", true); // 另一代的泵刚留下，登记方还在路上
        verify_settle(&mut plans, 13, "s2", true); // 另一会话的泵留下的

        verify_reap_settled(&mut plans, &[11]);

        let mut left: Vec<u64> = plans.keys().copied().collect();
        left.sort_unstable();
        assert_eq!(
            left,
            vec![10, 12, 13],
            "只收本泵记下的那一枚；别人的 Planned 与 Settled 一律原封不动"
        );
    }

    /// 同一 id 的第二次终态（理论不可达）：原样放回，不覆盖也不新增。
    #[test]
    fn a_second_terminal_for_the_same_id_puts_the_slot_back_untouched() {
        let mut plans = HashMap::new();
        verify_settle(&mut plans, 9, "s1", true);
        assert!(matches!(
            verify_settle(&mut plans, 9, "s1", false),
            SettleOutcome::Duplicate
        ));
        match plans.get(&9) {
            Some(VerifySlot::Settled { done, session_id }) => {
                assert!(done, "第二次终态不得把已记下的 done 改掉");
                assert_eq!(session_id, "s1");
            }
            _ => panic!("标记必须还在原处"),
        }
    }

    // ───────────────────────── 子系统生命周期（审计 P0-4 键控失效 / P0-5 关停顺序）
    //
    // 审计对这段的结论是「零测试覆盖」，原因不是没人想测：`shutdown_session_subsystems` 里
    // 唯一被关停的类型是 `TransferManager`，而它要一条真的 SFTP 通道才建得出来。于是 P0-5
    // 修的那条顺序（先 shutdown、再 remove）把两句调换回去，全仓库照样绿。
    //
    // 现在关停对象经 `SubsystemShutdown` 抽象，测试可以塞一个**会回头观察那张表**的替身进去：
    // 顺序不再靠「代码看起来是对的」，而是由替身在被关停的那一刻亲眼看到自己还在表里来证明。

    /// 替身在被关停的那一刻看到的表况。三种取值各对应一种排布错误。
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
    enum Seen {
        /// 自己仍在表里 —— 正确顺序（shutdown 在 remove 之前）
        StillInMap(u64),
        /// 自己已被摘掉 —— 摘表跑到了 shutdown 前面，即原始的 P0-5：
        /// 摘表只让引用消失，`tokio::spawn` 出去的 worker 仍握着文件句柄继续写，
        /// 而此后再没有人拿得到它去 cancel。
        AlreadyGone(u64),
        /// 表锁仍被持有 —— 关停跨了临界区，所有会话的 SFTP/传输入口会被这一次
        /// 最长 5 秒的等待一并冻住。
        TableLocked(u64),
    }

    struct FakeSubsystem {
        generation: u64,
        /// false = 宽限期内没收完（走超时分支）
        finishes: bool,
        /// 回指自己所在的那张表。Weak 而非 Arc：表 → OnceCell → 本替身 → 表 会成环。
        map: std::sync::Weak<Mutex<HashMap<SubsystemKey, SubsystemSlot<FakeSubsystem>>>>,
        key: SubsystemKey,
        log: Arc<std::sync::Mutex<Vec<Seen>>>,
        unfinished_asked: Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl SubsystemShutdown for FakeSubsystem {
        async fn shutdown_within(&self, _grace: std::time::Duration) -> bool {
            let map = self.map.upgrade().expect("关停期间那张表不该已被丢弃");
            // try_lock 而非 lock：产品代码若在关停期间仍持表锁，lock 会死等，测试表现为挂起；
            // try_lock 把同一个错误变成一条可读的断言失败。
            let seen = match map.try_lock() {
                Err(_) => Seen::TableLocked(self.generation),
                Ok(table) if table.contains_key(&self.key) => Seen::StillInMap(self.generation),
                Ok(_) => Seen::AlreadyGone(self.generation),
            };
            self.log.lock().unwrap().push(seen);
            self.finishes
        }
        async fn unfinished(&self) -> usize {
            self.unfinished_asked
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            3
        }
    }

    struct Fixture {
        map: SubsystemMap<FakeSubsystem>,
        log: Arc<std::sync::Mutex<Vec<Seen>>>,
        unfinished_asked: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl Fixture {
        /// `seeds` 的每一项是 `(session_id, generation, 是否装配过, 关停是否在宽限期内完成)`。
        ///
        /// 用 `Arc::new_cyclic` 建表：替身要持有回指这张表的 Weak，而 Weak 只有在 Arc 构造期间
        /// 才拿得到。
        fn new(seeds: &[(&str, u64, bool, bool)]) -> Self {
            let log: Arc<std::sync::Mutex<Vec<Seen>>> = Arc::default();
            let unfinished_asked: Arc<std::sync::atomic::AtomicUsize> = Arc::default();
            let map = Arc::new_cyclic(|weak| {
                let mut table: HashMap<SubsystemKey, SubsystemSlot<FakeSubsystem>> = HashMap::new();
                for (sid, generation, assembled, finishes) in seeds {
                    let key = (sid.to_string(), *generation);
                    let cell: SubsystemSlot<FakeSubsystem> = Arc::default();
                    if *assembled {
                        cell.set(Arc::new(FakeSubsystem {
                            generation: *generation,
                            finishes: *finishes,
                            map: weak.clone(),
                            key: key.clone(),
                            log: log.clone(),
                            unfinished_asked: unfinished_asked.clone(),
                        }))
                        .unwrap_or_else(|_| unreachable!("新建的 cell 不可能已有值"));
                    }
                    table.insert(key, cell);
                }
                Mutex::new(table)
            });
            Self {
                map,
                log,
                unfinished_asked,
            }
        }

        /// 排序后取出观察记录：表的遍历顺序是随机的，断言不能依赖它。
        fn seen(&self) -> Vec<Seen> {
            let mut v = self.log.lock().unwrap().clone();
            v.sort_unstable();
            v
        }

        async fn remaining_keys(&self) -> Vec<SubsystemKey> {
            let mut keys: Vec<SubsystemKey> = self.map.lock().await.keys().cloned().collect();
            keys.sort();
            keys
        }
    }

    /// P0-5 的正向回归：每一代都必须在**自己还在表里**的时候被关停。
    ///
    /// 同时钉住 P0-4 的清扫面：s1 重连过两次 → 三代管理器同时残留，只清当前代次就会漏掉
    /// 前两代——它们各自的 worker 仍在往用户的文件里写。
    #[tokio::test]
    async fn every_generation_is_shut_down_while_still_in_the_table() {
        let fx = Fixture::new(&[
            ("s1", 1, true, true), // 第一代（重连后残留）
            ("s1", 2, true, true), // 第二代（重连后残留）
            ("s1", 3, true, true), // 当前代
            ("s2", 9, true, true), // 另一个会话，不得被牵连
        ]);

        shutdown_and_clear(&fx.map, "s1", std::time::Duration::from_millis(1)).await;

        assert_eq!(
            fx.seen(),
            vec![
                Seen::StillInMap(1),
                Seen::StillInMap(2),
                Seen::StillInMap(3)
            ],
            "三代都必须被关停，且关停时自己仍在表里；\
             AlreadyGone = 摘表跑到了 shutdown 前面（原始 P0-5）；\
             TableLocked = 关停跨了临界区，会冻住其他会话；\
             少了哪一代 = 只清当前代次，前几代的 worker 成孤儿（P0-4）"
        );
        assert_eq!(
            fx.remaining_keys().await,
            vec![("s2".to_string(), 9)],
            "本会话的所有代次都必须摘净，别的会话一条都不许动"
        );
    }

    /// 关停超时（宽限期内没收完）：仍必须摘表，并且把未收尾件数问出来记进日志。
    ///
    /// 摘表这一步在超时路径上尤其要紧：留着只会让下一次查表命中一个已被放弃的管理器。
    /// 而问件数这一步是这条路径唯一的用户可见产物——`shutdown_and_clear` 不返回 Result，
    /// 超时信息只从那条 warn 出去；不问就等于静默吞掉。
    #[tokio::test]
    async fn a_timed_out_shutdown_is_still_swept_and_still_reported() {
        let fx = Fixture::new(&[("s1", 1, true, false)]);

        shutdown_and_clear(&fx.map, "s1", std::time::Duration::from_millis(1)).await;

        assert_eq!(fx.seen(), vec![Seen::StillInMap(1)]);
        assert_eq!(
            fx.unfinished_asked
                .load(std::sync::atomic::Ordering::SeqCst),
            1,
            "超时必须问出未收尾件数——那是这条路径唯一的日志产物，不问就是静默吞掉"
        );
        assert!(
            fx.remaining_keys().await.is_empty(),
            "超时也照摘：留着只会让下一次查表命中一个已被放弃的管理器"
        );
    }

    /// 从未装配过的槽位（`OnceCell` 是空的）：跳过关停，但照样摘表。
    ///
    /// 空 cell 的来源是真实的：`sftp_ops_for` / `transfer_manager_for` 先 `or_default()` 落一个
    /// 空格子再装配，装配失败（比如装配途中发生重连）时格子就那么留着。对它调 `get().unwrap()`
    /// 会 panic，而 panic 跨 IPC 会中止整个 core 进程。
    #[tokio::test]
    async fn an_unassembled_slot_is_skipped_but_still_swept() {
        let fx = Fixture::new(&[("s1", 1, false, true), ("s1", 2, true, true)]);

        shutdown_and_clear(&fx.map, "s1", std::time::Duration::from_millis(1)).await;

        assert_eq!(
            fx.seen(),
            vec![Seen::StillInMap(2)],
            "空槽位没有任务需要收尾，不该也不能对它调关停"
        );
        assert!(
            fx.remaining_keys().await.is_empty(),
            "空槽位同样要摘干净，否则重连累积的空格子会一直长下去"
        );
    }

    /// 会话没有任何子系统时是无操作，且不得碰别人的条目。
    #[tokio::test]
    async fn clearing_a_session_with_no_subsystems_touches_nothing() {
        let fx = Fixture::new(&[("other", 1, true, true)]);

        shutdown_and_clear(&fx.map, "s1", std::time::Duration::from_millis(1)).await;

        assert!(fx.seen().is_empty(), "不得关停别的会话的子系统");
        assert_eq!(fx.remaining_keys().await, vec![("other".to_string(), 1)]);
    }

    /// P0-4 的键控失效：**重连换号 → 查表自然落空**，不依赖任何人记得去清缓存。
    ///
    /// 这条把两半合起来验：注册表分配代次（`sessions.rs`）+ 子系统按 `(会话, 代次)` 取槽位
    ///（本文件）。缺了任一半，重连后 SFTP 与传输都会继续跑在已断开的旧连接上——
    /// 而终端走的是另一条路，照常可用，故障表现为「终端能用、文件列表转圈超时」。
    #[tokio::test]
    async fn a_reconnect_invalidates_the_cache_by_construction() {
        use crate::sessions::SessionRegistry;
        struct Placeholder;

        let registry: SessionRegistry<Placeholder> = SessionRegistry::default();
        let map: SubsystemMap<FakeSubsystem> = Arc::default();

        registry.insert("s1".into(), Arc::new(Placeholder));
        let before = subsystem_slot(&map, "s1", registry.generation_of("s1").unwrap()).await;

        // 同一格子重复取到的必须是同一个 —— `OnceCell` 的并发收口全靠这条
        //（裸 `Arc<T>` 的旧实现会让两个并发装配各造一个对象，后写入者覆盖前者，
        //  被覆盖的那个仍活着却再也拿不到引用：审计 P1-4 的孤儿通道/孤儿管理器）。
        let again = subsystem_slot(&map, "s1", registry.generation_of("s1").unwrap()).await;
        assert!(
            Arc::ptr_eq(&before, &again),
            "同一 (会话, 代次) 必须始终映射到同一个槽位，否则并发装配会各造一个对象"
        );

        // 重连：注册表换号
        registry.insert("s1".into(), Arc::new(Placeholder));
        let after = subsystem_slot(&map, "s1", registry.generation_of("s1").unwrap()).await;
        assert!(
            !Arc::ptr_eq(&before, &after),
            "重连后必须落到一个新槽位：命中旧槽位 = 继续往已断开的连接上发包"
        );

        // 旧槽位仍留在表里等 `shutdown_and_clear` 收，但已不可能被任何查表命中
        assert_eq!(
            map.lock().await.len(),
            2,
            "旧代次的条目仍在表里（由拆除流程统一收尾），只是再也不会被命中"
        );
    }

    /// 键的另一侧：不同会话即便代次相同也必须是不同槽位。
    /// （代次是进程级单调的，实践中撞不上；这条守的是键的形状本身。）
    #[tokio::test]
    async fn different_sessions_never_share_a_slot() {
        let map: SubsystemMap<FakeSubsystem> = Arc::default();
        let a = subsystem_slot(&map, "s1", 7).await;
        let b = subsystem_slot(&map, "s2", 7).await;
        assert!(
            !Arc::ptr_eq(&a, &b),
            "会话 id 必须进键，否则两个会话会共用同一条 SFTP 通道"
        );
    }

    // ───────────────────────── 下载沙箱根校验（审计 P1-15）
    //
    // 这条链路是「远端服务器控制下载文件名」这一威胁下唯一的越权写防线，而它此前零覆盖。
    // 校验的每一条都必须 **fail-safe**：不过就回落下一级，绝不拿用户给的可疑路径去写。

    fn tmp_dir(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "fs-sandbox-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).expect("建临时目录失败");
        p
    }

    /// 正常目录必须被接受，且返回的是**规范化后**的路径。
    ///
    /// 规范化不是可有可无的整洁：`resolve_within` 拿这个 root 与目标路径逐组件比较，
    /// 两边的规范化程度必须一致，否则 `C:\Users\X\..\X\dl` 这类写法会让「在沙箱内」的判定失效。
    #[test]
    fn a_real_directory_is_accepted_and_canonicalized() {
        let dir = tmp_dir("ok");
        let got = validate_sandbox_root(dir.to_str().unwrap(), "测试").expect("正常目录必须被接受");
        assert_eq!(got, dir.canonicalize().unwrap(), "返回值必须是规范化路径");

        // 未规范化的等价写法要折到同一个结果
        let indirect = dir.join("..").join(dir.file_name().unwrap());
        assert_eq!(
            validate_sandbox_root(indirect.to_str().unwrap(), "测试").expect("等价写法也该被接受"),
            got,
            "`<dir>/../<dir>` 与 `<dir>` 是同一个目录，必须折成同一个沙箱根"
        );

        // 首尾空白是用户从别处粘贴时的常态，不该因此把配置判废
        assert_eq!(
            validate_sandbox_root(&format!("  {}  ", dir.to_str().unwrap()), "测试")
                .expect("首尾空白不该让配置失效"),
            got
        );
    }

    /// 空串 = 未配置，静默回落下一级（不是错误，不该有告警噪音）。
    #[test]
    fn an_empty_setting_is_simply_absent() {
        assert!(validate_sandbox_root("", "测试").is_none());
        assert!(validate_sandbox_root("   ", "测试").is_none());
        assert!(validate_sandbox_root("\t\n", "测试").is_none());
    }

    /// 相对路径必须被拒：它会随进程 cwd 漂移，今天落在安装目录、明天落在用户双击时所在的
    /// 任意目录。一个「随启动方式改变落点」的沙箱不是沙箱。
    #[test]
    fn a_relative_path_is_rejected() {
        for raw in ["downloads", "./downloads", "..", "~/dl"] {
            assert!(
                validate_sandbox_root(raw, "测试").is_none(),
                "相对路径 {raw:?} 必须被拒（cwd 不可控）"
            );
        }
    }

    /// 不存在的路径必须被拒，且**不得**顺手建出来。
    ///
    /// 主动 `create_dir_all` 会让用户配错一个字母就凭空造出一棵目录树，同时掩盖「这条配置是错的」
    /// 这个事实——他会以为文件正落在自己想要的地方。
    #[test]
    fn a_missing_path_is_rejected_and_not_created() {
        let missing = tmp_dir("missing").join("no-such-dir");
        assert!(validate_sandbox_root(missing.to_str().unwrap(), "测试").is_none());
        assert!(
            !missing.exists(),
            "校验必须是纯查询：配错一个字母不该凭空造出目录树"
        );
    }

    /// 指向普通文件的配置必须被拒（否则下载会试图往文件里建子路径）。
    #[test]
    fn a_regular_file_is_not_a_sandbox_root() {
        let dir = tmp_dir("file");
        let f = dir.join("not-a-dir.txt");
        std::fs::write(&f, b"x").unwrap();
        assert!(validate_sandbox_root(f.to_str().unwrap(), "测试").is_none());
    }

    /// 沙箱根本身是软链时必须被拒 —— 这条是整组里唯一的安全性判定。
    ///
    /// 放行的后果不是「多解析一层」：`resolve_within` 会以软链 canonicalize 出的目标当 root，
    /// 于是「沙箱内」的判定整体平移到软链指向的地方，围墙原地搬家。攻击面是现成的——
    /// 用户下载目录里本就可能有指向别处的链接。
    ///
    /// Windows 上建**文件**软链要 SeCreateSymbolicLinkPrivilege，普通账户建不出来；
    /// 目录联接（junction）不需要特权，而 Rust 的 `FileType::is_symlink()` 对
    /// `IO_REPARSE_TAG_MOUNT_POINT` 同样返回 true，走的是同一条 `symlink_metadata` 分支
    ///（口径同 `crates/sshengine/tests/sandbox.rs`）。建不出来即判红而非跳过：静默跳过是假绿。
    #[test]
    fn a_symlinked_root_is_rejected() {
        let real = tmp_dir("lnk-target");
        let holder = tmp_dir("lnk");
        let link = holder.join("sandbox");

        #[cfg(unix)]
        std::os::unix::fs::symlink(&real, &link).unwrap();
        #[cfg(windows)]
        {
            let status = std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(&link)
                .arg(&real)
                .status()
                .expect("mklink 应可执行");
            assert!(
                status.success(),
                "目录联接创建失败（TEMP 是否在 NTFS 上？）"
            );
        }

        assert!(
            link.symlink_metadata().unwrap().file_type().is_symlink(),
            "装置本身要先成立：这里必须真的是一个重解析点/软链"
        );
        assert!(
            validate_sandbox_root(link.to_str().unwrap(), "测试").is_none(),
            "软链沙箱根必须被拒：放行等于让沙箱边界随软链目标整体平移"
        );
    }

    // ───────────── 超时判死后的通道驱逐（审计2 #11） ─────────────

    /// 一条从不回话的 SFTP 通道。写成 13 个一行方法是因为 `pending()` 的类型是 `!`，
    /// 可以塞进任何返回位置——这里不需要任何假数据，只需要「永远不回」这一件事。
    struct Deaf;

    async fn never() -> ! {
        std::future::pending().await
    }

    #[async_trait::async_trait]
    impl fs_sshengine::sftp::SftpOps for Deaf {
        async fn list(
            &self,
            _: &str,
        ) -> Result<fs_sshengine::sftp::ListResult, fs_sshengine::Error> {
            never().await
        }
        async fn stat_size(&self, _: &str) -> Result<u64, fs_sshengine::Error> {
            never().await
        }
        async fn stat_meta(
            &self,
            _: &str,
        ) -> Result<fs_sshengine::sftp::FileMeta, fs_sshengine::Error> {
            never().await
        }
        async fn read_range(
            &self,
            _: &str,
            _: u64,
            _: usize,
        ) -> Result<Vec<u8>, fs_sshengine::Error> {
            never().await
        }
        async fn write_at(&self, _: &str, _: u64, _: &[u8]) -> Result<(), fs_sshengine::Error> {
            never().await
        }
        async fn truncate(&self, _: &str, _: u64) -> Result<(), fs_sshengine::Error> {
            never().await
        }
        async fn mkdir(&self, _: &str) -> Result<(), fs_sshengine::Error> {
            never().await
        }
        async fn remove(&self, _: &str) -> Result<(), fs_sshengine::Error> {
            never().await
        }
        async fn remove_dir(&self, _: &str) -> Result<(), fs_sshengine::Error> {
            never().await
        }
        async fn rename(&self, _: &str, _: &str) -> Result<(), fs_sshengine::Error> {
            never().await
        }
        async fn lstat(
            &self,
            _: &str,
        ) -> Result<fs_sshengine::sftp::FileMeta, fs_sshengine::Error> {
            never().await
        }
        async fn read_link(&self, _: &str) -> Result<String, fs_sshengine::Error> {
            never().await
        }
        async fn symlink(&self, _: &str, _: &str) -> Result<(), fs_sshengine::Error> {
            never().await
        }
        async fn canonicalize(&self, _: &str) -> Result<String, fs_sshengine::Error> {
            never().await
        }
    }

    /// 造一格已被超时判死的通道。预算给 1 ms 走的是真实时钟——app 侧没开 tokio 的 test-util，
    /// 而这条路径要的只是「让 guard 真的走一次超时分支」，不是要验证预算的具体数值
    /// （那由 `fs_sshengine` 的 timeouts 用例按生产常量钉死）。
    async fn poisoned_slot() -> Arc<fs_sshengine::timeouts::TimedSftp> {
        use fs_sshengine::sftp::SftpOps;
        let t = Arc::new(fs_sshengine::timeouts::TimedSftp::with_budgets(
            Arc::new(Deaf),
            std::time::Duration::from_millis(1),
            std::time::Duration::from_millis(1),
        ));
        assert!(t.stat_size("/p").await.is_err());
        assert!(t.is_poisoned(), "装置本身要先成立：这一格必须真的被判死了");
        t
    }

    /// 判死之后必须把那一格从缓存里摘掉，否则这个会话的 SFTP 永久不可用。
    ///
    /// 这一条不是理论风险：判死是**每通道**的标志，而缓存键是 `(会话, 代次)`——代次没变，
    /// `shutdown_session_subsystems` 那条清理路径压根不会被触发。不摘表的话，之后每一次
    /// 列目录、每一次传输都会立刻拿到 `Poisoned`，而用户看到的现象是「重连也没用」
    /// （重连才换代次，但用户没有理由为一次列目录失败去重连）。
    #[tokio::test]
    async fn a_poisoned_channel_is_evicted_so_the_next_call_can_rebuild() {
        let map: SubsystemMap<fs_sshengine::timeouts::TimedSftp> =
            Arc::new(Mutex::new(HashMap::new()));
        let cell = subsystem_slot(&map, "s1", 7).await;
        cell.set(poisoned_slot().await).ok().unwrap();

        assert!(
            evict_slot_if_current(&map, "s1", 7, &cell).await,
            "缓存里那一格已经判死，必须摘掉"
        );

        let fresh = subsystem_slot(&map, "s1", 7).await;
        assert!(
            fresh.get().is_none(),
            "摘表之后重取该是一格空的，由下一次调用负责重建"
        );
        assert!(
            !Arc::ptr_eq(&fresh, &cell),
            "重取拿到的还是原来那一格——摘表没有真的生效"
        );
    }

    /// 并发驱逐必须幂等：只有第一个到场的人摘得掉，后来者不得碰新格子。
    ///
    /// 按键删（而不是按身份删）会在这里出事：甲摘掉死格、建好新通道落表之后，乙的删除会把
    /// **甲刚建好的活通道**一并摘走，那条通道就成了谁也拿不到、也关不掉的孤儿——
    /// 审计 P1-4 已经为同一种覆盖付过一次代价。
    #[tokio::test]
    async fn a_second_evictor_does_not_take_out_the_replacement() {
        let map: SubsystemMap<fs_sshengine::timeouts::TimedSftp> =
            Arc::new(Mutex::new(HashMap::new()));
        let stale = subsystem_slot(&map, "s1", 7).await;
        stale.set(poisoned_slot().await).ok().unwrap();

        // 甲：摘掉死格，重建一格（模拟它随后装配成功）。
        assert!(evict_slot_if_current(&map, "s1", 7, &stale).await);
        let replacement = subsystem_slot(&map, "s1", 7).await;

        // 乙：手上还攥着那一格死的，此刻才来摘。
        assert!(
            !evict_slot_if_current(&map, "s1", 7, &stale).await,
            "表里已经不是那一格了，不该报告「摘掉了」"
        );
        let now = subsystem_slot(&map, "s1", 7).await;
        assert!(
            Arc::ptr_eq(&now, &replacement),
            "第二次驱逐把替补的那一格也摘走了：并发时会造出拿不到、关不掉的孤儿通道"
        );
    }

    /// 驱逐只碰点名的那个键，别的会话与别的代次一条都不许动。
    #[tokio::test]
    async fn eviction_touches_exactly_one_key() {
        let map: SubsystemMap<fs_sshengine::timeouts::TimedSftp> =
            Arc::new(Mutex::new(HashMap::new()));
        let target = subsystem_slot(&map, "s1", 7).await;
        subsystem_slot(&map, "s1", 8).await; // 同会话的另一代次
        subsystem_slot(&map, "s2", 7).await; // 另一个会话

        assert!(evict_slot_if_current(&map, "s1", 7, &target).await);

        let mut left: Vec<_> = map.lock().await.keys().cloned().collect();
        left.sort();
        assert_eq!(
            left,
            vec![("s1".to_string(), 8), ("s2".to_string(), 7)],
            "驱逐波及了不该动的条目"
        );
    }

    /// 表里根本没有这个键时是无操作（会话已被拆掉、槽位已被 teardown 摘走）。
    #[tokio::test]
    async fn evicting_an_absent_key_is_a_no_op() {
        let map: SubsystemMap<fs_sshengine::timeouts::TimedSftp> =
            Arc::new(Mutex::new(HashMap::new()));
        let orphan = subsystem_slot(&map, "s1", 7).await;
        map.lock().await.clear();
        assert!(!evict_slot_if_current(&map, "s1", 7, &orphan).await);
        assert!(map.lock().await.is_empty());
    }

    fn poisoned(ops: &fs_sshengine::timeouts::TimedSftp) -> bool {
        ops.is_poisoned()
    }

    /// 判死的那一格必须被换掉，否则这个会话的 SFTP 永久不可用。
    ///
    /// 这一条不是理论风险：判死是**每通道**的标志，而缓存键是 `(会话, 代次)`——代次没变，
    /// `shutdown_session_subsystems` 那条清理路径压根不会被触发。不换格的话，之后每一次
    /// 列目录、每一次传输都会立刻拿到 `Poisoned`，而用户看到的现象是「重连也没用」
    /// （重连才换代次，但用户没有理由为一次列目录失败去重连）。
    #[tokio::test]
    async fn live_slot_replaces_a_poisoned_channel() {
        let map: SubsystemMap<fs_sshengine::timeouts::TimedSftp> =
            Arc::new(Mutex::new(HashMap::new()));
        let dead = subsystem_slot(&map, "s1", 7).await;
        dead.set(poisoned_slot().await).ok().unwrap();

        let got = live_slot(&map, "s1", 7, poisoned).await;
        assert!(!Arc::ptr_eq(&got, &dead), "拿回来的还是那一格死的");
        assert!(
            got.get().is_none(),
            "换来的该是一格空的，由本次调用负责重建"
        );
        assert!(
            Arc::ptr_eq(&subsystem_slot(&map, "s1", 7).await, &got),
            "新的那一格没有落表：下一个调用者会再建一格，缓存等于失效"
        );
    }

    /// 没判死的通道必须原样留用——否则每次调用都重开一条 SFTP 通道，`OnceCell` 缓存等于失效，
    /// 而「装配被并发重复执行、后写入者覆盖前者」正是审计 P1-4 的孤儿通道。
    #[tokio::test]
    async fn live_slot_keeps_a_healthy_channel() {
        let map: SubsystemMap<fs_sshengine::timeouts::TimedSftp> =
            Arc::new(Mutex::new(HashMap::new()));
        let cell = subsystem_slot(&map, "s1", 7).await;
        let healthy = Arc::new(fs_sshengine::timeouts::TimedSftp::new(Arc::new(Deaf)));
        cell.set(healthy.clone()).ok().unwrap();

        let got = live_slot(&map, "s1", 7, poisoned).await;
        assert!(Arc::ptr_eq(&got, &cell), "好端端的一格被换掉了");
        assert!(
            got.get().is_some_and(|v| Arc::ptr_eq(v, &healthy)),
            "缓存里的通道对象被丢了：重开通道要两次网络往返，这是 P1-4 修掉的东西"
        );
    }

    /// 还没装配的空格子同样要原样交回：把它摘掉会让并发的两个调用者拿到不同的格子，
    /// 于是各装配一条通道，其中一条成为谁也拿不到、也关不掉的孤儿（审计 P1-4）。
    #[tokio::test]
    async fn live_slot_leaves_an_unassembled_slot_alone() {
        let map: SubsystemMap<fs_sshengine::timeouts::TimedSftp> =
            Arc::new(Mutex::new(HashMap::new()));
        let cell = subsystem_slot(&map, "s1", 7).await;
        let got = live_slot(&map, "s1", 7, poisoned).await;
        assert!(
            Arc::ptr_eq(&got, &cell),
            "空格子被换掉了：并发装配会各拿一格"
        );
    }
}

/// 会话转录目录的**唯一**解析实现：配置里那一个，空（或纯空白）则回落
/// `<data_dir>/logs/sessions`。
///
/// 单源很要紧：这条规则此前在两处逐字重复——写转录文件的 `build_session_log` 与
/// 「打开会话日志目录」的 `reveal_session_log_dir`。两处一旦走散，那个入口就会打开
/// 一个空目录而转录文件躺在别处，是典型的「功能在、但指错地方」，而且不会报错。
pub fn session_log_dir(data_dir: &std::path::Path, configured: &str) -> std::path::PathBuf {
    if configured.trim().is_empty() {
        data_dir.join("logs").join("sessions")
    } else {
        std::path::PathBuf::from(configured.trim())
    }
}

#[cfg(test)]
mod session_log_dir_tests {
    use super::session_log_dir;
    use std::path::Path;

    #[test]
    fn empty_config_falls_back_under_data_dir() {
        let d = session_log_dir(Path::new("/data"), "");
        assert_eq!(d, Path::new("/data").join("logs").join("sessions"));
        // 纯空白与空串同义：用户在设置框里敲了几个空格不该产出一个名叫 " " 的目录
        assert_eq!(session_log_dir(Path::new("/data"), "   "), d);
        assert_eq!(
            session_log_dir(
                Path::new("/data"),
                "	
"
            ),
            d
        );
    }

    #[test]
    fn configured_dir_wins_and_is_trimmed() {
        assert_eq!(
            session_log_dir(Path::new("/data"), "  /var/log/fs  "),
            Path::new("/var/log/fs")
        );
    }

    #[test]
    fn fallback_matches_the_documented_layout() {
        // 「打开会话日志目录」与写转录文件必须落到同一处：这条断言把布局钉住，
        // 任何一侧擅自改名（logs/session、transcripts…）都会在这里红。
        let d = session_log_dir(Path::new("/data"), "");
        assert!(
            d.ends_with(Path::new("logs").join("sessions")),
            "实得 {d:?}"
        );
    }
}

/// 便携模式（M4b「便携打包」）。
///
/// 单独一个 mod 而不是塞进上面某个：它测的是**进程外**的东西（可执行文件旁的标记文件），
/// 与那些测会话/日志的用例没有共享夹具，混在一起只会让两边的 use 互相污染。
#[cfg(test)]
mod portable_tests {
    use super::*;
    use std::path::PathBuf;

    // ── 便携模式（M4b「便携打包」）─────────────────────────────────────────
    //
    // 这几条不能直接调 `data_dir_or_err()`：它读的是**当前进程**的 exe 路径，
    // 而测试进程的 exe 在 target/debug/deps 下。所以测的是把 exe 目录作为参数的
    // 那一层逻辑——判据（标记文件在不在）与后果（目录建不建得出来）都在那一层。

    /// `portable_data_dir` 的可测形态：exe 目录由调用方给。
    ///
    /// 生产路径 `portable_data_dir()` 只多做一件事——从 `current_exe()` 取那个目录。
    /// 把「取路径」与「据此决定落点」分开，是因为后者才是会出错的部分，
    /// 而前者在测试里根本无从伪造。
    fn portable_for(dir: &std::path::Path) -> Result<Option<PathBuf>, String> {
        let marker = dir.join(PORTABLE_MARKER);
        if !marker.is_file() {
            return Ok(None);
        }
        let data = dir.join(PORTABLE_DATA_DIR);
        std::fs::create_dir_all(&data).map_err(|e| {
            format!(
                "便携模式已启用（发现 {}），但数据目录 {} 建不出来：{e}。\
                 便携模式要求程序所在目录可写——把整个程序目录解压到 U 盘或用户目录下再试。",
                marker.display(),
                data.display()
            )
        })?;
        Ok(Some(data))
    }

    #[test]
    fn without_the_marker_portable_mode_stays_off() {
        // 默认必须是关的。误开的后果是数据落到程序旁边——对一个装在
        // Program Files 里的副本，那要么失败要么把数据写进一个全局可见的位置。
        let tmp = std::env::temp_dir().join(format!("fs-portable-off-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        assert_eq!(portable_for(&tmp).unwrap(), None);
        // 也没有顺手建出 data 目录来
        assert!(!tmp.join(PORTABLE_DATA_DIR).exists());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn the_marker_turns_it_on_and_data_lands_next_to_the_exe() {
        let tmp = std::env::temp_dir().join(format!("fs-portable-on-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join(PORTABLE_MARKER), "").unwrap();

        let got = portable_for(&tmp).unwrap().expect("标记文件在，应当启用");
        assert_eq!(got, tmp.join(PORTABLE_DATA_DIR));
        assert!(got.is_dir(), "数据目录应当已经建好");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn a_marker_directory_does_not_count() {
        // `portable.txt` 是个**目录**时不算启用。判据用 `is_file()` 而不是 `exists()`：
        // 一个同名目录多半是解压出错或用户手滑，把它当成「开启便携模式」
        // 会让数据落到一个用户没打算用的地方。
        let tmp = std::env::temp_dir().join(format!("fs-portable-dir-{}", std::process::id()));
        std::fs::create_dir_all(tmp.join(PORTABLE_MARKER)).unwrap();
        assert_eq!(portable_for(&tmp).unwrap(), None);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn an_unwritable_location_fails_loudly_instead_of_falling_back() {
        // 用户放了标记文件，他的意图是「数据跟着程序走」。这时悄悄落回
        // %APPDATA% 是这个功能最坏的失败方式：他拔下 U 盘走人，
        // 数据留在别人的机器上，而界面上什么都没说。
        //
        // 造不可写目录的可移植做法：把 data 这个**名字**先占成一个文件。
        // create_dir_all 在同名文件存在时必失败，而这与「目录不可写」
        // 走的是同一条错误分支。
        let tmp = std::env::temp_dir().join(format!("fs-portable-ro-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join(PORTABLE_MARKER), "").unwrap();
        std::fs::write(tmp.join(PORTABLE_DATA_DIR), "占位").unwrap();

        let err = portable_for(&tmp).unwrap_err();
        assert!(err.contains("便携模式"), "{err}");
        // 错误里要说清怎么办，而不只是说失败了
        assert!(err.contains("可写"), "{err}");
        assert!(err.contains("U 盘") || err.contains("解压"), "{err}");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn the_production_path_uses_the_same_two_constants() {
        // 上面测的是 `portable_for`，生产走的是 `portable_data_dir`。
        // 两者若用了不同的文件名/子目录名，测试全绿而功能是坏的。
        const SRC: &str = include_str!("state.rs");
        let prod = &SRC[..SRC.find("\n#[cfg(test)]").expect("本文件必须有测试段")];
        assert!(
            prod.contains("dir.join(PORTABLE_MARKER)"),
            "portable_data_dir 不再用 PORTABLE_MARKER 常量"
        );
        assert!(
            prod.contains("dir.join(PORTABLE_DATA_DIR)"),
            "portable_data_dir 不再用 PORTABLE_DATA_DIR 常量"
        );
        // 且判据仍是 is_file（目录不算）
        assert!(prod.contains("marker.is_file()"), "判据不再是 is_file");
    }
}
