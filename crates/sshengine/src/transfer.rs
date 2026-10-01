//! 传输管理器：分块上传/下载、进度事件、指数退避重试、断点续传、单件取消（spec §2.3）。
//! 下载写盘前一律经 `sandbox::resolve_within` 校验——Rust 侧是唯一可信边界。
//! 校验的落点是 `<dest>` 与 `<dest>.fspart` **两个**路径：铁律 ② 让字节全部落在临时件上，
//! 只查 `<dest>` 就成了守着一条没人写的路（审计 P2，见 `sandbox::reject_symlink_leaf`）。
//!
//! 数据完整性三条铁律（生产级审计 P0-1/P0-2/P0-3）：
//! ① 任务 id 取**进程级**全局单调序列（`next_transfer_id`）。每个管理器各自从 0 起会让
//!    两个会话的首件传输都拿到 id=1，取消/校验/UI 全部串单；
//! ② 一切写入先落**临时件**（`<目标>.fspart`），成功后才 rename 覆盖最终目标。最终目标在
//!    传输真正完成前一个字节都不碰——中途断线、进程被杀、用户取消，用户原有的文件都完好；
//! ③ 同一目标同时只允许一个任务写入（进程级 per-target 锁），并发写同一目标不再交错。
// 大小写折叠的开关来自 `sandbox`——那里还有 `folding_conflict` 用同一个取值判定落点碰撞。
// 两处必须同源：判据一旦分叉，就会出现「锁认为是两个目标、文件系统认为是一个」的窗口。
use crate::sandbox::CASE_INSENSITIVE_FS;
use crate::sftp::SftpOps;
use crate::verify::ExecChannel;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex as StdMutex};
use tokio::sync::mpsc;

pub type TransferId = u64;
/// 传输分块：255 KiB（261 120 字节），不是 256 KiB。
///
/// OpenSSH 的 sftp-server 通过 `limits@openssh.com` 通告单次读写上限 261 120 字节
///（`SFTP_MAX_MSG_LENGTH` 256 KiB 减去 1024 字节头部余量），russh-sftp 按它切请求。
/// 分块取 256 KiB 时每块都被切成「255 KiB + 1 KiB」两个请求：读是顺序的，于是每块
/// 白白多一次往返（1.0.0 候选实测下行 13–15 MB/s → 对齐后 18.7–19.8 MB/s）。
/// 服务端通告更小的上限时仍会被切分——对齐的是事实上最常见的那一种服务端。
/// 测试引用本常量而不是写死字节数：分块改了，断点、偏移类断言要跟着一起变。
pub const CHUNK: usize = 261_120;

/// 临时件后缀。刻意选一个不像常规扩展名的串：它会短暂出现在用户的下载目录/远端目录里，
/// 得让人一眼看出「这是没传完的半成品」，而不是误当成正经文件双击打开。
const PART_SUFFIX: &str = ".fspart";

/// 让位备份后缀（审计2 #8）。提交阶段若必须给新文件腾地方，旧的最终目标被**改名**到
/// `<目标>.fsbak` 而不是被删除；成功后才删备份，失败则原样挪回。它只在 `commit` 的
/// 失败重试路径上短暂存在，正常提交一次都不会出现。
const BAK_SUFFIX: &str = ".fsbak";

/// 续传身份记录的后缀，挂在临时件旁边：`<临时件>.fsmeta`（审计2 #9）。
const META_SUFFIX: &str = ".fsmeta";

/// 身份记录的读取上限。它是一份几百字节的定长 JSON，4 KiB 已经宽绰；不设上限就等于允许
/// 「把该文件换成 10 GB 的同名文件」变成一次内存耗尽——而这个路径上的文件名恰恰由**远端**
/// 的文件名决定。
const META_MAX: usize = 4096;

/// 进程级全局单调传输 id（审计 P0-1）。
///
/// 原实现把计数器挂在 `TransferManager` 上，而管理器是 **per-session** 的：两个会话各自的
/// 首件传输都拿到 id=1，于是「取消 id=1」会打到错误的会话、传后校验表 `HashMap<id, _>` 互相
/// 覆盖、UI 传输列表两行合并成一行。id 的唯一性域必须与它被使用的域一致——事件、取消、
/// 校验都是跨会话在同一张表里流转的，那 id 就必须是进程级唯一。
///
/// 从 1 起：0 留作「非法/未赋值」哨兵，前端拿到 0 一眼可判是漏传了参数。
/// u64 单调递增即便每秒一百万件也要五十万年才溢出，不做回绕处理。
pub fn next_transfer_id() -> TransferId {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::SeqCst)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Direction {
    Up,
    Down,
}

#[derive(Debug, Clone)]
pub struct TransferJob {
    pub direction: Direction,
    pub local: PathBuf,
    pub remote: String,
    /// 续传「意愿」而非权威偏移（审计 P1-5）。
    ///
    /// 原字段是 `resume_offset: u64`，直接来自前端 IPC 参数并被无条件信任：偏移比实际落盘
    /// 前缀大就在文件里留一个稀疏空洞（下载）或跳过一段源数据（上传），得到的文件长度对、
    /// 哈希错，用户往往到解压/启动服务时才发现。现在前端只表达「想续传」，真实起点一律由
    /// 引擎按 `.fspart` 临时件的**实际长度**重算——那是唯一有权威性的事实来源。
    pub resume: bool,
    /// 下载沙箱根（spec §3.3）：Down 方向必填，写入本地前强制校验 `local` 位于其内
    /// （fail-closed：None 即拒绝）；Up 方向为 None（上传源由用户主动选择，
    /// app 层 local_list 已约束浏览根）。
    pub sandbox_root: Option<PathBuf>,
    /// per-target 锁的命名空间：**物理端点**（规范化的 `host:port`，app 层 `transfer_endpoint` 产出）。
    ///
    /// 远端路径只在同一台主机上才有唯一性：两台主机各自的 `/tmp/a.bin` 是两个文件，
    /// 用同一把锁串行化它们纯属误伤。故远端目标键 = `"remote:{target_endpoint}:{规范化 remote}"`。
    ///
    /// 这里收的必须是**端点**，不能是会话/连接身份（审计 P1：本字段原名 `target_ns`，
    /// app 层填的是 `"{session_id}#{generation}"`）。锁的目的是「同一个物理文件同时只被一个
    /// 任务写」，而 `session_id` 和 `generation` 都比物理文件细：
    /// ① 同一台服务器开两个标签页（两个 session_id，很常见：一个跑命令一个传文件），
    ///    同时上传同一路径 → 两把不同的锁 → 两个 worker 交错写同一个 `.fspart`，
    ///    最后各自 rename 一次，**两件都报 Done**，用户拿到的是两份内容按块拼起来的垃圾文件；
    /// ② 就算只有一个标签页也躲不掉：重连时 `shutdown_session_subsystems` 只等一个**有界**
    ///    宽限期，超时会打印「仍有任务未到达终态；临时件保留」然后放行——此刻旧代次的 worker
    ///    还在写，新代次拿着新的 generation 提交同一路径，同样是两把锁写一个文件。
    /// 反过来，把粒度放粗到「所有会话共用一个键」也是错的：那会把两台主机上同名的
    /// `/tmp/a.bin` 误锁成一个目标。端点恰好是与「物理文件」对齐的那一层。
    ///
    /// 本地路径在进程内已全局唯一（同一个文件系统），本地目标键 = `"local:{已解析绝对路径}"`，
    /// 不掺端点——否则两个会话下载到同一个本地文件反而锁不住，正是要防的那种交错。
    pub target_endpoint: String,
    /// 与 `target_endpoint` **指向同一台服务器**的其它端点拼法（审计2 #13）。
    ///
    /// `target_endpoint` 只做得到词法归一（大小写、结尾点、IPv6 方括号）。它做不到的那一类
    /// 是审计点名的：`example.com:22` 与 `10.0.0.5:22` 是同一台机器，但两串字面量毫无关系，
    /// 于是两个标签页——一个按域名连、一个按 IP 连——会拿到两把不同的锁，照旧交错写同一个
    /// `.fspart`。判定它们相等需要一次 DNS 往返，那不属于本 crate（这里没有连接层）。
    ///
    /// 故本字段由 app 层在**会话装配时**填：把 profile 的 host 解析一次，每个对端地址
    /// 折成一个规范端点串。锁按「`target_endpoint` ∪ 本字段」的**并集**取（见
    /// `try_lock_targets`），多一维只会多挡，绝不会少挡——DNS 轮询让两次解析给出不同答案时，
    /// 主机名那一维仍然重合。
    ///
    /// 空 `Vec` 是完全合法的取值：解析失败、走跳板（此时本地解析出的地址不是对端）、
    /// 或者调用方还没接线，都退回到「只有 `target_endpoint` 一维」的旧行为。
    pub endpoint_aliases: Vec<String>,
    /// 传输完成后的校验期望内容哈希（路线图 M1 出口 / UI 规格 §1.4）。本管理器只携带不执行。
    /// Up = 传前本地哈希（app 层经 `verify::file_sha256`）；Down = 传前远端哈希
    ///（`verify::remote_sha256_via`，失败 → app 层置 `VerifyPlan.degraded`、本字段 None）。
    /// 执行体 = 本 crate `verify.rs`，app 层在 Done 后驱动。
    pub verify: Option<Sha256Hex>,
}

/// 小写 hex 的 SHA256（64 字符）。构造即校验，之后按值比较。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Sha256Hex(pub String);

impl Sha256Hex {
    pub fn new(s: impl Into<String>) -> Result<Self, crate::Error> {
        let s = s.into();
        // 只收小写：与服务端 `sha256sum` 输出逐字比对，大小写混收会让「相等」依赖归一化时机。
        if s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
            Ok(Self(s))
        } else {
            Err(crate::Error::Transfer(format!(
                "invalid sha256 hex (need 64 lowercase hex chars): {s}"
            )))
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub enum TransferState {
    Running,
    Retrying {
        attempt: u32,
    },
    Done,
    Failed(String),
    /// 用户单件取消的终态
    Cancelled,
}

#[derive(Debug, Clone, Serialize)]
pub struct TransferEvent {
    pub id: TransferId,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub state: TransferState,
}

// ---------------------------------------------------------------------------
// per-target 锁注册表（审计 P0-3）
// ---------------------------------------------------------------------------

/// 目标锁注册表：进程级，键见 `TransferJob::target_endpoint` 的说明。
///
/// 用 `std::sync::Mutex` 而不是 tokio 的：注册表的临界区只有一次 HashMap 查表，不含 await，
/// 用异步锁反而多一次任务让出。真正需要跨 await 持有的是表里的 `tokio::sync::Mutex`。
static TARGET_LOCKS: LazyLock<StdMutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> =
    LazyLock::new(|| StdMutex::new(HashMap::new()));

/// 持有目标锁的 RAII 句柄。释放顺序必须是「先放互斥量、再清注册表项」，
/// 否则清理时自己那份 `Arc` 还在计数里，判不出「无人引用」。
///
/// `Debug` 只印键名（`OwnedMutexGuard` 本就没有 `Debug`）：单测用 `expect_err` 判「整组抢锁
/// 失败」时要求成功侧可打印，而失败信息里真正有用的就是键名本身。
#[derive(Debug)]
struct TargetGuard {
    key: String,
    guard: Option<tokio::sync::OwnedMutexGuard<()>>,
}

impl Drop for TargetGuard {
    fn drop(&mut self) {
        self.guard.take(); // 先释放互斥量，让 Arc 计数只剩注册表自己那一份
        let mut map = TARGET_LOCKS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // 无人引用即摘表。不摘会让注册表随「传输过的不同目标数」无界增长——Xftp 式批量
        // 传输单次可达十万个不同路径，每条表项 = String + Arc + Mutex 常驻不释放（同 S50）。
        // 计数 == 1 的判断在注册表锁内做，与 `try_lock_target` 的插入/克隆互斥，不会漏摘也不会误摘。
        if map
            .get(&self.key)
            .is_some_and(|a| Arc::strong_count(a) == 1)
        {
            map.remove(&self.key);
        }
    }
}

/// 抢占目标锁。**不等待**：拿不到立刻返回 None。
///
/// 刻意不用等待式加锁：worker 槽位有限（`TransferManager::spawn` 的 concurrency），
/// 一批任务若都排队等同一个目标，槽位会被它们占满，其他目标的任务一件也跑不动——
/// 一个本该「两件冲突」的局部问题升级成全队列饿死。快速失败并把原因明说
/// （「目标忙」），比静默交错写坏文件正确得多，也比饿死可诊断得多。
fn try_lock_target(key: &str) -> Option<TargetGuard> {
    let arc = {
        let mut map = TARGET_LOCKS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        map.entry(key.to_string())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    };
    // 抢不到说明另一个 TargetGuard 正持有它，表项由那一位负责摘除，此处无需回滚。
    arc.try_lock_owned().ok().map(|g| TargetGuard {
        key: key.to_string(),
        guard: Some(g),
    })
}

/// 一次抢占**一组**目标键，全有或全无。抢不到时 `Err` 里是那把被别人占着的键。
///
/// 为什么是一组（审计2 #13）：同一个远端文件有多种拼法（软链目录、相对路径、域名 vs IP），
/// 而**没有哪一种拼法是权威的**——服务端 REALPATH 会失败，DNS 会轮询，于是「先算出唯一
/// 真名再按它上锁」这条路走不通：两件传输只要有一件退化到词法名，就又各锁各的了。
/// 取并集则不同：只要两件传输在**任意一维**上重合，就必然撞在同一把锁上。多一维只会多挡。
///
/// 按键值**升序**抢，是这组语义能成立的前提，不是排版偏好。乱序抢会出现「互相踩死」：
/// A 持 k1 求 k2、B 持 k2 求 k1，两边**同时失败**，同一个目标一件都传不动，而两件的报错
/// 都是「目标忙」——用户看到的是一个没人在传却永远说忙的目标，重试多少次都一样。
/// 升序则不可能：设 k 是 A、B 键集交集里最小的那一把，两边都是升序请求，故都在只持有
/// 「小于 k 的键」时抵达 k，其中恰有一位拿到。反证另一侧也失败：设 A 失败在 kA（被 B 持有）、
/// B 失败在 kB（被 A 持有），不妨 kA ≤ kB；B 持有 kA 说明 B 已越过 kA 走到 kB，
/// 而 A 此刻只持有小于 kA 的键，持不了 kB > kA，矛盾。
///
/// 失败时已抢到的守卫随 `held` 被丢弃而释放（`TargetGuard::drop`），不存在半持有状态。
fn try_lock_targets(keys: &[String]) -> Result<Vec<TargetGuard>, String> {
    let mut sorted: Vec<&str> = keys.iter().map(String::as_str).collect();
    sorted.sort_unstable();
    // 去重是必要的而非优化：同一把键抢两次，第二次必然撞上自己刚拿的那一份，
    // 一件孤零零的传输会报「目标忙」。别名折叠到与主键相同时（例如路径本来就是规范的）
    // 就正好是这个局面，而那恰恰是最常见的情形。
    //
    // 它**依赖上面那次排序**：`dedup` 只删相邻重复项。把 `sort_unstable` 拿掉不只是丢掉
    // 死锁自由，还会让「端点别名与主键同值、但中间隔着一条别的键」这种最常见的排布漏网，
    // 于是每一次这样的上传都当场自锁。两行要一起看，别把排序当成纯粹的性能修饰。
    sorted.dedup();
    let mut held = Vec::with_capacity(sorted.len());
    for k in sorted {
        match try_lock_target(k) {
            Some(g) => held.push(g),
            None => return Err(k.to_string()),
        }
    }
    Ok(held)
}

/// 把归一化后的远端路径拆成（父目录, 叶子名）。拆不出可用叶子（`/`、`.`、`..`）时 None。
///
/// 只拆不解析：解析交给服务端的 REALPATH，而且**只解析父目录**。见 `remote_lock_keys`。
fn split_parent_leaf(normalized: &str) -> Option<(&str, &str)> {
    let (parent, leaf) = match normalized.rsplit_once('/') {
        // `/a.bin` → ("", "a.bin")：父目录是根，不是空串
        Some(("", leaf)) => ("/", leaf),
        Some((p, leaf)) => (p, leaf),
        // 无斜杠的相对路径，父目录就是「SFTP 会话的起始目录」
        None => (".", normalized),
    };
    // `normalize_remote` 已消解掉所有中间的 `.`/`..`，故这三种叶子只可能来自整条路径本身
    // 就是 `/`、`.`、`..`——三者都是目录而非传输目标。拼回去只会造出 `/home/u/.` 这种
    // 既不指向文件、又与真目标不相等的假键。
    if leaf.is_empty() || leaf == "." || leaf == ".." {
        return None;
    }
    Some((parent, leaf))
}

/// 上传目标的全部锁键（审计2 #13）。
///
/// 两个维度取笛卡尔积：端点（`target_endpoint` + `endpoint_aliases`）× 路径（词法名 + 规范名）。
///
/// 规范名向服务端要一次 SSH_FXP_REALPATH，但**只问父目录**，再把叶子名原样拼回去。
/// 三条理由，每条都单独足以否掉「直接 realpath 整条路径」：
///
/// 1. 上传的目标常常**还不存在**，而各服务端对不存在路径的 REALPATH 应答没有统一约定
///    （见 `SftpOps::canonicalize`）。父目录则一定存在——不存在的话这次传输本来也要失败。
/// 2. 叶子若是软链，`commit` 的 `rename` **不跟随**它：往 `/tmp/link.bin` 传是替换这条链
///    本身，往它指向的 `/tmp/real.bin` 传是替换那个文件，两者写的是**两个不同的对象**，
///    临时件也分别落在 `/tmp/link.bin.fspart` 与 `/tmp/real.bin.fspart`。把叶子折叠掉
///    反而会把两件本就互不干扰的传输误锁成一件。
/// 3. 解析结果与真正写入用的路径不是同一个字符串，中间隔着一个 TOCTOU 窗口。锁键不怕
///    （错了只是互斥粒度变化），线上路径怕。
///
/// 拿不到规范名就只留词法名——**降级而不是中止**。少一维互斥不等于少传一个文件；
/// 而这一维即便全失效，提交前闸门第 ① 关（伴生身份记录，见 `precommit_gate`）仍会在
/// 提交那一刻发现临时件已经不属于本次传输，最终目标不会被写坏。
///
/// 代价照实记：每件上传多一次控制面往返。批量传输里父目录高度重复，此处**刻意不做缓存**
/// ——缓存要么随不同父目录数无界增长，要么在软链改指向后给出陈旧答案，而这一维本来就是
/// 「尽力而为的加固」，不值得用一个新的失效模式去换。
async fn remote_lock_keys(job: &TransferJob, ops: &Arc<dyn SftpOps>) -> Vec<String> {
    let lexical = normalize_remote(&job.remote);
    let mut paths = vec![lexical.clone()];
    if let Some((parent, leaf)) = split_parent_leaf(&lexical) {
        if let Ok(real_parent) = ops.canonicalize(parent).await {
            // 空应答按失败处置：拼进去会把 `leaf` 变成 `/leaf`，凭空指向根目录。
            if !real_parent.is_empty() {
                let joined = format!("{}/{leaf}", real_parent.trim_end_matches('/'));
                paths.push(normalize_remote(&joined));
            }
        }
    }
    let mut keys = Vec::new();
    for ep in std::iter::once(&job.target_endpoint).chain(job.endpoint_aliases.iter()) {
        for p in &paths {
            keys.push(format!("remote:{ep}:{p}"));
        }
    }
    keys
}

/// 下载目标的全部锁键（审计2 #13 的本地一侧）。
///
/// 路径本身已由 `sandbox::resolve_within` 规范化过父目录（软链、`..` 全解），末段软链更是
/// 直接被拒——所以本地这一侧的目录别名早已折叠。**但那是沙箱防越权顺带带来的**，
/// 与本条互斥没有任何契约关系，故在此写明依赖，别在重构沙箱时把它悄悄搬走。
///
/// 剩下的一类是大小写：`resolve_within` 只 canonicalize 父目录，叶子名是 `file_name()` 原样
/// join 回去的，于是在 Windows/macOS 上 `Report.pdf` 与 `report.pdf` 是同一个文件、两把锁。
/// 折叠成小写后**并集**上锁：同名不同拼法必然重合；反过来在大小写敏感的卷上（Windows 也能
/// 逐目录开启）两个真正不同的文件会被误判成一个，代价是一句「目标忙」，方向偏保守。
fn local_lock_keys(local: &Path, fold_case: bool) -> Vec<String> {
    let exact = local.display().to_string();
    let mut keys = vec![format!("local:{exact}")];
    if fold_case {
        let folded = exact.to_lowercase();
        if folded != exact {
            keys.push(format!("local:{folded}"));
        }
    }
    keys
}

// ---------------------------------------------------------------------------
// 临时件路径
// ---------------------------------------------------------------------------

/// `<remote>.fspart`。远端路径是普通字符串，直接拼后缀。
fn remote_part(remote: &str) -> String {
    format!("{remote}{PART_SUFFIX}")
}

/// 远端路径的**词法**归一化——只用于组目标锁的键，绝不用于真正发给服务端的路径。
///
/// 锁键是按字符串相等来判「是不是同一个目标」的，于是同一个文件的两种写法就是两把锁：
/// `/tmp/a.bin`、`/tmp//a.bin`、`/tmp/./a.bin`、`/tmp/x/../a.bin`、`/tmp/a.bin/` 全指同一个
/// 文件，却能拿到五把互不相干的锁，并发写同一个 `.fspart`。前端的路径由「当前目录 + 条目名」
/// 拼出，重复斜杠正是这类拼接最常见的产物（`cwd = "/"` 时 `"/" + "/tmp"`）。
///
/// 只做词法消解、不查服务端：
/// - 归一化是个**纯函数**，相等的输入必得相等的输出，因此它只会把原本分开的键**合并**，
///   绝不可能把原本相同的键拆开。合并的代价上限是一次「目标忙」（用户重试即可），
///   而拆开的代价是静默写坏文件——方向必须往安全的一侧偏。
/// - 也正因如此，`..` 用词法消解即可：软链存在时 `/a/b/../c` 与 `/a/c` 未必同一个文件，
///   但把它们判成同一个只会多报一次「忙」。**真实路径一个字节都不改**，
///   软链语义仍由服务端自己解释。
///
/// 无法消解的残余（如实记下，不假装解决）：`example.com` 与它的 IP、相对路径与绝对路径、
/// 经不同软链抵达同一文件——这些都需要一次服务端 realpath 往返才能判定，本函数不做。
fn normalize_remote(remote: &str) -> String {
    let absolute = remote.starts_with('/');
    let mut out: Vec<&str> = Vec::new();
    for seg in remote.split('/') {
        match seg {
            // 空段来自重复斜杠与结尾斜杠；`.` 是自身
            "" | "." => {}
            ".." => {
                // 能弹就弹。弹不动时分两种：绝对路径的 `/..` 按 POSIX 就是根本身（丢弃即可）；
                // 相对路径的前导 `..` 没有基准可消解，原样留着，免得 `../a` 与 `a` 被当成一个。
                if out.last().is_some_and(|s| *s != "..") {
                    out.pop();
                } else if !absolute {
                    out.push("..");
                }
            }
            s => out.push(s),
        }
    }
    let joined = out.join("/");
    match (absolute, joined.is_empty()) {
        (true, _) => format!("/{joined}"),
        // 相对路径消解到空（`"."`、`"a/.."`、`""`）即「当前目录」本身
        (false, true) => ".".into(),
        (false, false) => joined,
    }
}

/// `<dest>.fspart`，**同目录**——rename 必须同卷才具备原子性，换个临时目录就退化成
/// 跨卷拷贝（既不原子又慢，且可能因为目标卷空间不足在最后一刻失败）。
/// 用 `OsString::push` 追加而非 `set_extension`：后者会吃掉原有扩展名
///（`a.tar.gz` → `a.tar.fspart`），提交时 rename 回去就成了另一个文件名。
fn local_part(dest: &Path) -> PathBuf {
    let mut s = dest.as_os_str().to_os_string();
    s.push(PART_SUFFIX);
    PathBuf::from(s)
}

/// 身份记录的路径：`<临时件>.fsmeta`。见 `PartIdentity`。
fn remote_meta(remote: &str) -> String {
    format!("{}{}", remote_part(remote), META_SUFFIX)
}

fn local_meta(dest: &Path) -> PathBuf {
    let mut s = local_part(dest).into_os_string();
    s.push(META_SUFFIX);
    PathBuf::from(s)
}

/// 「文件不存在」的启发式判定（审计 P1-6）。
///
/// 本 crate 的 `Error::Sftp(String)` 没有承载状态码的字段，`sftp.rs` 的每个方法都以
/// `map_err(|e| Error::Sftp(e.to_string()))` 收口，故到这里只剩错误串可匹配。
/// **这层折叠是本 crate 自己做的，不是 russh-sftp 的限制**（该库的 `Error::Status(Status)`
/// 保留了结构化状态码）——原注释把它记成库的限制，会让下一个人以为结构化没得选，故更正。
/// 之所以仍不值得为此改造错误类型：russh-sftp 是硬编码的 SFTP v3 客户端，而 v3 的
/// `StatusCode` 里**没有** FILE_ALREADY_EXISTS（那是 v4+ 才有的 11 号），OpenSSH 对
/// 「目标已存在」回的是通用的 SSH_FX_FAILURE。也就是说结构化了也依然区分不出 `commit`
/// 最想区分的那一种失败——所以 `commit` 的正确性绝不能建立在「判得出失败原因」上，
/// 它必须对任何原因都不丢数据（见 `commit`）。ENOENT 这一种倒是有独立状态码，
/// 但它在各服务端的文案高度一致，字符串匹配已经够用。
///
/// 判定方向刻意保守：宁可漏判（真 ENOENT 被当成硬错误 → 传输失败并报错，用户重试即可），
/// 也绝不能误判——把权限拒绝/网络中断当成「文件不存在」就会静默按 offset=0 重新起跑，
/// 悄悄抹掉一个 9 GB 的断点。原实现的 `unwrap_or(0)` 正是后者。
fn is_not_found(e: &crate::Error) -> bool {
    if let crate::Error::Io(io) = e {
        if io.kind() == std::io::ErrorKind::NotFound {
            return true;
        }
    }
    let s = e.to_string().to_ascii_lowercase();
    s.contains("no such file") || s.contains("nosuchfile") || s.contains("enoent")
}

/// 源身份：`.fspart` 里那段前缀究竟是**从哪个源、哪一版内容**上抄下来的（审计2 #9）。
///
/// 原实现判断能否续传只看一个不等式：临时件长度 ≤ 源长度。可 `.fspart` 的路径完全由**目标**
/// 决定，于是同一个目标上任何一次旧任务、旧版本、另一个进程留下的残件都长着同一个名字。
/// 长度这一项对得上纯属巧合，续传于是从别人的前缀上接着写，拼出「旧前缀 + 新后缀」——
/// 长度正确、内容错乱。更狠的是 `n == total` 那一格：它同样落进续传分支，`exec_once` 首轮
/// 就读到 EOF 判 Completed，于是**零字节传输**把整份陈旧内容 rename 成了成品，UI 报 100%。
/// 传后校验关掉、或降级成 size-only 时（两边都是 total，判 `SizeOnlyMatch` 黄标通过），
/// 这份垃圾会被静默交付。
///
/// 判据因此必须是**身份**而不是长度。本记录随临时件一起落地（`<临时件>.fsmeta`），续传前
/// 逐字段比对：任何一项不等、记录缺失、读不动、解不开——一律不续传，从 0 重来。fail-closed，
/// 因为两侧代价完全不对等：多传一遍的代价是时间，认错前缀的代价是一个坏文件。
///
/// 升级尾巴（有意为之，须写进发布说明）：老版本留下的 `.fspart` 没有伴生记录，一律不再被
/// 续传，而是全量重传。这不是「断点续传坏了」，是它第一次真正开始验身份。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PartIdentity {
    /// 记录格式版本。以后改字段时旧记录一律判不匹配（=重传），而不是按新结构错解旧字节。
    v: u32,
    /// 方向。同一个路径既可能是某次上传的目标，也可能是某次下载的源。
    /// 用 `String` 而不是 `Direction`：给 `Direction` 加 `Deserialize` 会改动一个**公共**
    /// 枚举的 derive 行，而它同时是前端契约门禁扫的对象；这份记录是纯内部的落盘格式，
    /// 不值得为它把公共类型的序列化面拉宽。
    dir: String,
    /// 源的规范化标识：Up = 本地源路径，Down = 归一化后的远端源路径。
    src: String,
    /// 源大小。
    size: u64,
    /// 源的修改时间（Unix 秒）。取不到（服务端不回该属性、或时间早于 epoch）时为 None。
    mtime: Option<i64>,
    /// 源内容哈希 = `TransferJob::verify`。传后校验关闭时为 None，此时身份只由上面几项确定。
    ///
    /// 这个字段是本记录的分量所在：它让身份判据是**内容**而不只是一组元数据。而它是白拿的
    /// ——app 层为了传后校验本来就已经在传前算好了它（Up 本地算、Down 远端预取），
    /// 引擎这边此前只是把它接过来放着从没读过（`grep job.verify` 曾零命中）。
    hash: Option<String>,
}

/// `std::fs::Metadata` 的 mtime（Unix 秒）。取不到一律 None——**不可取不等于判不匹配**，
/// 比对方按 Option 处理（见 `precommit_gate`）。
fn mtime_secs_of(m: &std::fs::Metadata) -> Option<i64> {
    m.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
}

/// 量一次源的身份。**同时**是 `total` 的来源：本函数取代了原先那次单独的
/// `metadata`/`stat_size` 往返，所以身份记录没有增加任何一次 I/O。
async fn source_identity(
    job: &TransferJob,
    local: &Path,
    ops: &Arc<dyn SftpOps>,
) -> Result<PartIdentity, crate::Error> {
    let hash = job.verify.as_ref().map(|h| h.0.clone());
    match job.direction {
        Direction::Up => {
            let m = tokio::fs::metadata(local).await?;
            Ok(PartIdentity {
                v: 1,
                dir: "up".into(),
                src: local.display().to_string(),
                size: m.len(),
                mtime: mtime_secs_of(&m),
                hash,
            })
        }
        Direction::Down => {
            // `stat_meta` 而不是 `lstat`：要的是**真正被读的那个文件**的大小与 mtime。
            // 源是软链时，链自身的 mtime 在目标被改写时纹丝不动——拿它当变更探针等于探不到，
            // 而探不到会以「一切正常」的形式呈现，是这里最不能接受的一种错。
            let m = ops.stat_meta(&job.remote).await?;
            Ok(PartIdentity {
                v: 1,
                dir: "down".into(),
                src: normalize_remote(&job.remote),
                size: m.size,
                // 服务端不回 mtime 时 russh-sftp 给 0；0 在这里当「不可取」处理，
                // 代价是 1970-01-01 那一秒的文件失去 mtime 判据（size + hash 仍在）。
                mtime: (m.mtime != 0).then_some(m.mtime),
                hash,
            })
        }
    }
}

fn identity_bytes(ident: &PartIdentity) -> Vec<u8> {
    serde_json::to_vec(ident).expect("PartIdentity 全是标量字段，序列化不会失败")
}

/// 建一个全新的空文件，Unix 上直接以 0600 落地（审计2 #16）。
///
/// 权限在 `open` 那一刻就定死（`OpenOptionsExt::mode`），而不是建完再 `set_permissions`：
/// 后者留下一个「文件已存在、权限还是 umask 给的 0644」的窗口，同机别的账户在那一瞬就能
/// 打开它并一直读下去。`.fspart` 里躺的正是用户从远端取回的东西——配置、备份、私钥。
/// 与 `vault::store` 的原子写同口径（那里的注释是这条论据的原文）。
///
/// 收权同时覆盖**最终交付文件**：`commit` 走 rename，而 rename 不改 inode 的 mode，
/// 于是这一次收权一路继承到用户最后拿到的那个文件。这是有意的行为变更（0644 → 0600），
/// 须写进发布说明——一个 SSH 客户端下载下来的东西默认只有属主可读，是正确的那一侧。
///
/// `create_new` = `O_CREAT|O_EXCL`：路径是软链时直接失败（悬空软链也算），于是「跟随软链
/// 写到沙箱外」在结构上不可能发生，不依赖时序。详见 `prepare_part` 的长注释。
async fn create_private_file(path: &Path) -> Result<tokio::fs::File, crate::Error> {
    match tokio::fs::remove_file(path).await {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    let mut opts = tokio::fs::OpenOptions::new();
    opts.create_new(true).write(true);
    #[cfg(unix)]
    {
        // 这里**不**导入 `std::os::unix::fs::OpenOptionsExt`：`opts` 是 tokio 的
        // `OpenOptions`，它在 unix 上自带 `mode()` 固有方法。导入 std 的扩展 trait 既用不上，
        // 又会在 Linux 上触发 `unused_imports`——而 CI 跑的是 `clippy -- -D warnings`，
        // 那是一次硬失败。Windows 上这个块被 cfg 掉，所以本机永远看不到（M4a.1 T87 实测）。
        opts.mode(0o600);
    }
    // Windows 侧无需额外处理：沙箱根在 %LOCALAPPDATA% 下，默认继承「仅本用户 + SYSTEM」ACL。
    Ok(opts.open(path).await?)
}

async fn write_local_identity(path: &Path, ident: &PartIdentity) -> Result<(), crate::Error> {
    use tokio::io::AsyncWriteExt;
    crate::sandbox::reject_symlink_leaf(path)?;
    // 建好句柄后**就用这个句柄写**，不重新按路径打开：那会在 `create_new` 与写之间重新引入
    // 一道可被换成软链的缝，而 `create_new` 的全部价值正是消掉这道缝。
    let mut f = create_private_file(path).await?;
    f.write_all(&identity_bytes(ident)).await?;
    f.flush().await?;
    f.sync_all().await?;
    drop(f.into_std().await);
    Ok(())
}

/// 读身份记录。**任何**失败都折成 `None`（= 判不匹配 = 重传），因为这里没有一种失败值得
/// 让整件传输报错：记录读不出来时正确的动作就是别信它。
async fn read_local_identity(path: PathBuf) -> Option<PartIdentity> {
    use tokio::io::AsyncReadExt;
    crate::sandbox::reject_symlink_leaf(&path).ok()?;
    let f = tokio::fs::File::open(&path).await.ok()?;
    let mut buf = Vec::new();
    f.take(META_MAX as u64).read_to_end(&mut buf).await.ok()?;
    serde_json::from_slice(&buf).ok()
}

async fn write_remote_identity(
    ops: &Arc<dyn SftpOps>,
    path: &str,
    ident: &PartIdentity,
) -> Result<(), crate::Error> {
    let bytes = identity_bytes(ident);
    ops.write_at(path, 0, &bytes).await?;
    // `write_at` 刻意不带 TRUNCATE（那是续传的硬要求，见 `SftpOps::truncate`），于是更长的
    // 旧记录会留下残尾把 JSON 撑坏。解不开 = 判不匹配 = 重传，安全但白费一整轮，故显式截断。
    ops.truncate(path, bytes.len() as u64).await
}

/// `path` 按值传：调用点是 `|| read_remote_identity(ops, remote_meta(&job.remote))` 这种
/// 闭包，借用形参会让返回的 future 借着一个在闭包体末尾就死掉的临时 `String`（E0515）。
/// 与 `read_local_identity` 收 `PathBuf` 同因。
async fn read_remote_identity(ops: &Arc<dyn SftpOps>, path: String) -> Option<PartIdentity> {
    let bytes = ops.read_range(&path, 0, META_MAX).await.ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// 提交成功后清掉身份记录（临时件本身已被 rename 消耗掉）。
///
/// 尽力而为：删不掉只会在目录里留下一份几百字节的 `.fspart.fsmeta`，它旁边已经没有
/// `.fspart` 了，下一次传输会照常重建。把它升级成失败，等于让一件**已经成功**的传输报错，
/// 用户会去重传一个本已正确的文件——那才是真的害处。
async fn cleanup_identity(job: &TransferJob, local: &Path, ops: &Arc<dyn SftpOps>) {
    match job.direction {
        Direction::Up => {
            let _ = ops.remove(&remote_meta(&job.remote)).await;
        }
        Direction::Down => {
            let _ = tokio::fs::remove_file(local_meta(local)).await;
        }
    }
}

pub struct TransferManager {
    tx: mpsc::Sender<(TransferId, TransferJob, Arc<AtomicBool>)>,
    events: Arc<tokio::sync::Mutex<Option<mpsc::Receiver<TransferEvent>>>>,
    /// per-id 取消标志（用户单件取消）：分块循环边界检查此标志，
    /// 命中即发 Cancelled 终态事件并 return。同时兼作「在途集合」，`active_len` 读它。
    cancels: Arc<tokio::sync::Mutex<HashMap<TransferId, Arc<AtomicBool>>>>,
    /// 关停闸门（审计 P0-5）：置位后 `submit` 一律拒收。单独用一个标志而不是 drop 掉 `tx`，
    /// 是因为 `tx` 还要供已在途的路径使用，且拒收要给调用方一个**可读的理由**。
    closed: Arc<AtomicBool>,
}

impl TransferManager {
    /// 不带 exec 通道的构造。等价于 `spawn_with_verifier(ops, concurrency, None)`：
    /// 提交前闸门仍会跑「源未变 + 临时件长度」两层，只是上传方向拿不到远端哈希这一层证据
    /// （下载方向的哈希在本地算，不依赖 exec，照跑）。见 `precommit_gate`。
    pub fn spawn(ops: Arc<dyn SftpOps>, concurrency: usize) -> Self {
        Self::spawn_with_verifier(ops, concurrency, None)
    }

    /// 带 exec 通道的构造（审计2 #10）。
    ///
    /// `exec` 的唯一用途是**提交前**在远端算 `<remote>.fspart` 的 sha256，把「传坏了」挡在
    /// rename 之前。app 层的传后校验（`verify::run_verify`）仍然照常跑、仍然是 UI 那四种
    /// 结局的唯一来源——那一遍如今是**冗余**的第二次哈希，这是有意保留的成本，理由记在
    /// `precommit_gate` 的文档注里。
    pub fn spawn_with_verifier(
        ops: Arc<dyn SftpOps>,
        concurrency: usize,
        exec: Option<Arc<dyn ExecChannel>>,
    ) -> Self {
        let (job_tx, mut job_rx) = mpsc::channel::<(TransferId, TransferJob, Arc<AtomicBool>)>(256);
        let (ev_tx, ev_rx) = mpsc::channel::<TransferEvent>(256);
        let sem = Arc::new(tokio::sync::Semaphore::new(concurrency.max(1)));
        let cancels: Arc<tokio::sync::Mutex<HashMap<TransferId, Arc<AtomicBool>>>> =
            Arc::new(tokio::sync::Mutex::new(HashMap::new()));
        let cancels_reaper = cancels.clone();
        tokio::spawn(async move {
            while let Some((id, job, cancelled)) = job_rx.recv().await {
                let ops = ops.clone();
                let ev = ev_tx.clone();
                let cancels = cancels_reaper.clone();
                let exec = exec.clone();
                let permit = sem.clone().acquire_owned().await.unwrap();
                tokio::spawn(async move {
                    let _permit = permit;
                    run_transfer(id, job, ops, exec, ev, cancelled).await;
                    // 终态即摘除取消标志。`submit` 只插不删会让 `cancels` 随会话寿命无界增长
                    //（Xftp 式批量传输单次可达十万件，每条 HashMap 表项 + Arc 分配常驻不释放
                    // ——S50）。摘除后 `cancel(id)` 退化为静默 no-op，语义正确：已达终态的
                    // 传输本就不可取消。摘除也是 `shutdown` 判「已收尾」的唯一依据，故必须
                    // 排在 `run_transfer` 之后——那时文件句柄与目标锁都已随栈帧释放。
                    cancels.lock().await.remove(&id);
                });
            }
        });
        Self {
            tx: job_tx,
            events: Arc::new(tokio::sync::Mutex::new(Some(ev_rx))),
            cancels,
            closed: Arc::new(AtomicBool::new(false)),
        }
    }

    /// 提交作业。返回 Err 即**作业未被受理**（审计 P1-1）。
    ///
    /// 原实现 `let _ = self.tx.send(..).await; id` —— 派发任务已死（panic 过、或会话正在关停）
    /// 时发送必然失败，但仍照常返回一个 id：UI 上多出一行永远停在 0% 的「传输中」，用户等到
    /// 天荒地老也等不到终态事件，退出确认框还会因为 `active_len > 0` 一直拦着他。发送失败必须
    /// 回滚 `cancels` 表项（否则那条幽灵登记会把 `active_len`/`shutdown` 永久卡住）并如实报错。
    pub async fn submit(&self, job: TransferJob) -> Result<TransferId, crate::Error> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(crate::Error::Transfer(
                "传输管理器已关停，拒绝新作业".into(),
            ));
        }
        let id = next_transfer_id();
        let cancelled = Arc::new(AtomicBool::new(false));
        self.cancels.lock().await.insert(id, cancelled.clone());
        if self.tx.send((id, job, cancelled)).await.is_err() {
            self.cancels.lock().await.remove(&id);
            return Err(crate::Error::Transfer(
                "传输作业未能入队：派发任务已退出（会话正在关闭或已崩溃）".into(),
            ));
        }
        Ok(id)
    }

    /// 单消费者：取走事件接收端（只能调用一次）。
    pub async fn events(&self) -> mpsc::Receiver<TransferEvent> {
        self.events
            .lock()
            .await
            .take()
            .expect("events 接收端只能取一次")
    }

    /// 用户单件取消：置 per-id 取消标志；分块循环边界检查此标志，
    /// 命中即发 `TransferEvent { state: Cancelled }` 并 return。
    pub async fn cancel(&self, id: TransferId) {
        if let Some(flag) = self.cancels.lock().await.get(&id) {
            flag.store(true, Ordering::SeqCst);
        }
    }

    /// 取消全部在途任务（审计 P0-5 关停序列第 3 步）。
    /// 一次性把标志全置位再放锁：逐个 `cancel` 会在每件之间放锁，关停途中新达终态的任务
    /// 反复重排 HashMap，且拉长「已停收新作业但旧作业还在跑」的窗口。
    pub async fn cancel_all(&self) {
        for flag in self.cancels.lock().await.values() {
            flag.store(true, Ordering::SeqCst);
        }
    }

    /// 有序关停（审计 P0-5）：① 停收新作业 ② 取消全部在途 ③ 有界等待收尾。
    ///
    /// 返回 true = 宽限期内全部到达终态；false = 超时，调用方应把未完成件数记进日志并
    /// 提示用户（临时件都还在，重连后可续传，不会有数据损失）。
    ///
    /// 原实现根本没有关停入口：app 层「关闭会话」只是把 `TransferManager` 从 map 里删掉，
    /// 而派发任务和每个传输任务都是 `tokio::spawn` 出去的独立任务、各自持有 `Arc<dyn SftpOps>`
    /// ——从 map 里删掉不会中断任何一个，它们会继续往一个用户以为已经关掉的会话上写数据，
    /// 直到 SSH 连接自己断开为止。
    ///
    /// 判据用 `active_len`（终态后摘除取消标志）而不是自己数任务：那一步排在 `run_transfer`
    /// 返回之后，此时本地文件句柄、目标锁都已随栈帧释放，正是「收尾完成」的定义。
    pub async fn shutdown(&self, grace: std::time::Duration) -> bool {
        self.closed.store(true, Ordering::SeqCst);
        self.cancel_all().await;
        let deadline = tokio::time::Instant::now() + grace;
        loop {
            if self.active_len().await == 0 {
                return true;
            }
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            // 轮询而非 Notify：在途件数本就是 UI 每秒都在读的量，20 ms 一次的查表开销可忽略，
            // 换来的是不用维护一套「最后一件完成时通知」的唤醒状态机。
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }

    /// 尚未到达终态（Done/Failed/Cancelled）的传输件数：UI 传输面板计数，
    /// 以及退出前「仍有传输进行中，确定关闭？」确认框的判据。
    pub async fn active_len(&self) -> usize {
        self.cancels.lock().await.len()
    }
}

async fn run_transfer(
    id: TransferId,
    job: TransferJob,
    ops: Arc<dyn SftpOps>,
    exec: Option<Arc<dyn ExecChannel>>,
    ev: mpsc::Sender<TransferEvent>,
    cancelled: Arc<AtomicBool>,
) {
    // 前置阶段（沙箱、目标锁、元数据、临时件）的失败出口都是同一套动作：发一条 Failed 终态
    // 事件再 return。抽成宏而不是函数，是因为它必须能 `return` 出**外层**函数——写成函数就得
    // 在每个调用点重复 `{ f(..).await; return; }`，而漏掉 return 的那一处就是一个「报了失败却
    // 继续往下传」的静默缺陷。末尾用不带分号的 `return`，块类型为 `!`，可直接用在 match 臂上。
    macro_rules! fail {
        ($total:expr, $($arg:tt)*) => {{
            let _ = ev
                .send(TransferEvent {
                    id,
                    bytes_done: 0,
                    bytes_total: $total,
                    state: TransferState::Failed(format!($($arg)*)),
                })
                .await;
            return
        }};
    }

    // 下载沙箱校验（spec §3.3）：Rust 侧是唯一可信边界——任何本地写入前先解析为沙箱内的
    // 规范化路径。放在重试循环**之外**：沙箱拒绝是策略性永久失败，重复三次结论恒定，
    // 只会白等 1 s + 2 s 退避，并向 UI 推两条「正在重试」的假象（S47）。只有 I/O 类
    // 瞬时错误才配重试。
    let local: PathBuf = match job.direction {
        Direction::Up => job.local.clone(),
        Direction::Down => {
            let resolved = match &job.sandbox_root {
                Some(root) => crate::sandbox::resolve_within(root, &job.local),
                None => Err(crate::Error::Transfer(
                    "download rejected: sandbox_root not set".into(),
                )),
            };
            match resolved {
                Ok(p) => p,
                Err(e) => fail!(0, "{e}"),
            }
        }
    };

    // per-target 锁（审计 P0-3、审计2 #13）。放在沙箱解析**之后**：本地键必须用解析后的
    // 规范化绝对路径，否则 `./a.bin` 与 `<root>/a.bin` 会被当成两个目标，锁了等于没锁。
    //
    // 键是一**组**而不是一把：同一个文件的多种拼法各占一维，取并集（见 `try_lock_targets`）。
    let (target_keys, target_label) = match job.direction {
        // 键用归一化路径（见 `normalize_remote`），给用户看的标签用原样路径——
        // 报「目标忙：另一传输正在写入 /tmp/a.bin」时，写他键入的那一串才对得上号。
        Direction::Up => (remote_lock_keys(&job, &ops).await, job.remote.clone()),
        Direction::Down => (
            local_lock_keys(&local, CASE_INSENSITIVE_FS),
            local.display().to_string(),
        ),
    };
    let _target_guards = match try_lock_targets(&target_keys) {
        Ok(g) => g,
        // 不重试、不排队，理由见 `try_lock_target`。守卫随本函数栈帧释放，因此覆盖了
        // 下面**所有**终态路径（Done/Failed/Cancelled/提前 return），不存在漏放锁的分支。
        //
        // 把撞上的那把键一并报出来：这条修复的全部意义就是「两串看起来不一样的路径其实是
        // 同一个文件」，只说「目标忙」会让用户对着一个自己没在传的路径百思不解。
        Err(busy) => fail!(
            0,
            "目标忙：另一传输正在写入 {target_label}（同目标键 {busy}）"
        ),
    };

    // 源身份（审计2 #9）**兼**传输总字节。任一侧读不到元数据都直接失败（审计 P1-6）：
    // 原实现 `unwrap_or(0)` 把权限拒绝、网络中断一律当成「文件不存在/空文件」，于是 total=0
    // 传出去，UI 进度条恒为「0/0 已完成」，下载侧还会因为 `offset >= total` 当场判 EOF、
    // 把一个空文件提交成成品。
    //
    // 这一次量取同时供三处使用：`total`、续传身份判据、以及提交前的「源变了没有」比对基准。
    // 它取代的是原先那次只拿 size 的往返，故身份记录没有多花任何一次 I/O。
    let ident = match source_identity(&job, &local, &ops).await {
        Ok(i) => i,
        Err(e) => match job.direction {
            Direction::Up => fail!(0, "读取本地源文件元数据失败 {}: {e}", local.display()),
            Direction::Down => fail!(0, "读取远端源文件大小失败 {}: {e}", job.remote),
        },
    };
    let total = ident.size;

    // 临时件准备 + 真实起点重算（审计 P0-2 / P1-5 / 审计2 #9）。前端给的只是 `resume: bool`，
    // 起点一律以临时件实际长度为准，且只有伴生身份记录逐字段吻合时才认这个起点。
    let mut offset = match prepare_part(&job, &local, &ops, total, &ident).await {
        Ok(o) => o,
        Err(e) => fail!(total, "准备临时件失败: {e}"),
    };

    // 已完成字节游标，跨重试**共享**。原实现每次尝试都从 `job.resume_offset` 重新起跑，
    // 9 GB 传到 8.9 GB 断一次就白扔 8.9 GB —— 断点续传是本 crate 的招牌能力，
    // 而重试恰是最需要它的时刻（S46）。
    for attempt in 0..3u32 {
        match exec_once(id, &job, &local, &ops, total, &ev, &cancelled, &mut offset).await {
            // Cancelled 事件已由 exec_once 发出，不再追加 Done。
            // 注意此处**不提交**：临时件原样保留，下次带 resume 即从断点续跑。
            Ok(Attempt::Cancelled) => return,
            Ok(Attempt::Completed) => {
                // 提交前闸门（审计2 #10 / #15）。它排在 `commit` **之前**，因为 rename 一旦
                // 发生，用户原有的文件就没了——校验只能在那之前说话，不然它只是给一个已经
                // 造成的损失贴标签。任何**确凿**的不符都在这里终结本件传输：临时件原样保留，
                // 最终目标一个字节都没被碰过。
                if let Err(e) = precommit_gate(&job, &local, &ops, exec.as_ref(), &ident).await {
                    let _ = ev
                        .send(TransferEvent {
                            id,
                            bytes_done: offset,
                            bytes_total: total,
                            state: TransferState::Failed(format!("{e}")),
                        })
                        .await;
                    return;
                }
                if let Err(e) = commit(&job, &local, &ops).await {
                    // 提交失败同样保留临时件：数据已经完整落在 `.fspart` 上，只是没能改名，
                    // 报 Failed 让用户重来一次远比丢掉一整轮传输好。
                    let _ = ev
                        .send(TransferEvent {
                            id,
                            bytes_done: offset,
                            bytes_total: total,
                            state: TransferState::Failed(format!("{e}")),
                        })
                        .await;
                    return;
                }
                // 提交成功，临时件已被 rename 消耗掉，伴生身份记录随之作废。
                cleanup_identity(&job, &local, &ops).await;
                let _ = ev
                    .send(TransferEvent {
                        id,
                        bytes_done: total,
                        bytes_total: total,
                        state: TransferState::Done,
                    })
                    .await;
                return;
            }
            // bytes_done 报 `offset` 而非 0：游标跨重试保留，报 0 会让 UI 进度条
            // 在每次重试时假摔回起点，也与「下一次尝试从哪里续」相矛盾。
            Err(_) if attempt < 2 => {
                let _ = ev
                    .send(TransferEvent {
                        id,
                        bytes_done: offset,
                        bytes_total: total,
                        state: TransferState::Retrying {
                            attempt: attempt + 1,
                        },
                    })
                    .await;
                tokio::time::sleep(std::time::Duration::from_secs(1 << attempt)).await;
            }
            Err(e) => {
                let _ = ev
                    .send(TransferEvent {
                        id,
                        bytes_done: offset,
                        bytes_total: total,
                        state: TransferState::Failed(e.to_string()),
                    })
                    .await;
                return;
            }
        }
    }
}

/// 备好临时件并算出**真实**起点（审计 P0-2 / P1-5）。放在重试循环之外：非续传要把临时件
/// 清零，这动作若落进循环，第二次尝试就会把第一次已传好的前缀抹掉，重试直接退化成重头再来。
async fn prepare_part(
    job: &TransferJob,
    local: &Path,
    ops: &Arc<dyn SftpOps>,
    total: u64,
    ident: &PartIdentity,
) -> Result<u64, crate::Error> {
    match job.direction {
        Direction::Up => {
            let part = remote_part(&job.remote);
            // 只有「文件不存在」才归 0，其余错误一律上抛（审计 P1-6）。
            let existing = match ops.stat_size(&part).await {
                Ok(n) => Some(n),
                Err(e) if is_not_found(&e) => None,
                Err(e) => return Err(e),
            };
            if resumable_at(job, existing, total, ident, || {
                read_remote_identity(ops, remote_meta(&job.remote))
            })
            .await
            {
                return Ok(existing.unwrap_or(0));
            }
            // 从 0 重来。身份记录**先**落地、再动临时件：这样「临时件里有字节」与
            // 「旁边有一份说明这些字节出处的记录」之间不存在只成立后半句的中间态。
            // 反过来写（先清零再记）则会留下一个窗口：进程恰在此刻被杀，下一次看到的是
            // 一个空临时件配一份**旧**记录——空临时件的偏移是 0，续传等于没续，无害；
            // 但顺序本身不该依赖「恰好无害」这种论证。
            write_remote_identity(ops, &remote_meta(&job.remote), ident).await?;
            if existing.is_some() {
                ops.truncate(&part, 0).await?;
            } else {
                // 零长度写 = SSH_FXP_OPEN(WRITE|CREATE) + 写 0 字节，作用是把临时件**物化**出来。
                //
                // 少了这一步，空源文件（0 字节）会走出一条静默毁数据的路径：`exec_once` 首轮
                // 就读到 EOF，一次 `write_at` 都不会发，`<remote>.fspart` 从头到尾不存在；
                // `commit` 的 rename 于是必然失败，进而走进「删旧目标再试一次」分支，
                // 把用户远端**已有的同名文件删掉**，再报一句 Failed。用户传一个空文件，
                // 代价是原文件没了。
                ops.write_at(&part, 0, &[]).await?;
            }
            Ok(0)
        }
        Direction::Down => {
            let part = local_part(local);
            // 末段软链防护必须在**这里**再落一次（审计 P2）。`run_transfer` 里的
            // `resolve_within` 只查过 `<dest>`，而 `<dest>` 在整个下载期间一个字节都收不到——
            // 字节全落在 `<dest>.fspart` 上，`<dest>` 只在 `commit` 里被 rename 触碰一次
            // （rename 不跟随软链）。远端服务器控制下载文件名即控制临时件名，沙箱里预置一个
            // 同名软链就能把字节引到沙箱外。详见 `sandbox::reject_symlink_leaf`。
            crate::sandbox::reject_symlink_leaf(&part)?;
            // 注意 `metadata` 是**跟随**软链的：上面那道拒绝不只是「多一层保险」，
            // 它同时保证了下面这个长度真的是临时件自己的长度，而不是软链目标的长度——
            // 否则续传会拿着别人的文件长度当断点，从那个偏移往外面的文件里接着写。
            let existing = match tokio::fs::metadata(&part).await {
                Ok(m) => Some(m.len()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(e) => return Err(e.into()),
            };
            if resumable_at(job, existing, total, ident, || {
                read_local_identity(local_meta(local))
            })
            .await
            {
                if let Some(n) = existing {
                    return Ok(n);
                }
            }
            // 从 0 重来。身份记录先落地，理由同上传分支。
            write_local_identity(&local_meta(local), ident).await?;
            // 再 `create_new` 建一个全新的空临时件（`create_private_file` 内含 remove + 建）。
            // 全程不碰 `<dest>` ——原实现直接用 `truncate(false)` 打开最终目标，远端 50 MB
            // 覆盖本地 100 MB 只会盖住前 50 MB，旧文件的后 50 MB 原样挂在尾巴上（审计 P0-2 下载侧）。
            //
            // 这里不用 `create(true).truncate(true)`：那条路**会跟随软链**，于是防护的成败
            // 就系于「检查」与「打开」之间那道 TOCTOU 缝。而 `create_new` 是 `O_CREAT|O_EXCL`，
            // POSIX 明文规定路径是软链时直接失败（**悬空软链也算**），Windows 的 CREATE_NEW 同理——
            // 这条路径上「跟随软链」于是在结构上就不可能发生，不再依赖时序。
            // 配套的 `remove_file` 删的是链接本身（不跟随），清掉的正是要防的那颗雷；
            // 删完到建好之间若有人再抢种一个，`create_new` 报 AlreadyExists，传输失败——
            // fail-closed，而不是跟着链接走出去。
            //
            // 建出来即 0600（审计2 #16）：见 `create_private_file`。
            let f = create_private_file(&part).await?;
            // 同步 drop，理由见 `finish_local_file`（Windows 共享位）。
            drop(f.into_std().await);
            Ok(0)
        }
    }
}

/// 能否从 `existing` 这个偏移续跑（审计2 #9）。
///
/// 四道关，缺一不可，全部 fail-closed：
/// ① 用户确实要求续传；② 临时件确实在场；③ 它不比源文件长——`n > total` 意味着这段前缀
/// 来自一个更大的文件，从它末尾续跑会直接跳过本次源文件的后半段；④ **伴生身份记录逐字段
/// 吻合**。
///
/// 第 ④ 关是这次修复的全部要害，前三关合起来也只能把「长度碰巧对得上的陌生残件」放行。
/// 注意 `n == total` 也在允许之列（而不是像某些实现那样一律作废）：身份对得上时，
/// 一个满长度的临时件就是**已传完但没来得及提交**的那一件，续传它等于直接进提交，
/// 这正是断点续传最该省下的那一整轮。判据是身份，不是长度，所以这一格不再危险。
///
/// 取值器写成闭包而非直接传 `Option<PartIdentity>`：身份记录的读取要发一次网络往返
/// （上传方向），而前三关任一不过就没必要读——不能为了代码整齐给每件非续传传输白加一次 I/O。
/// 比对用的是**整份记录**（`PartIdentity: PartialEq` 的派生实现），不是挑几个字段手写比较：
/// 后者在加字段时极易漏掉一项，而漏掉的那一项恰恰就是判据被悄悄削弱的地方。
async fn resumable_at<F, Fut>(
    job: &TransferJob,
    existing: Option<u64>,
    total: u64,
    want: &PartIdentity,
    read_ident: F,
) -> bool
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Option<PartIdentity>>,
{
    if !job.resume {
        return false;
    }
    let Some(n) = existing else { return false };
    if n > total {
        return false;
    }
    read_ident().await.as_ref() == Some(want)
}

/// 提交前闸门（审计2 #10 + #15）：在 rename 之前把「传坏了」挡住。
///
/// # 为什么必须在提交之前
///
/// 原先的顺序是 `commit` → app 层 `run_verify` → 发 `transfer_verified`。也就是说校验跑的
/// 时候，用户原有的文件**已经被替换掉了**：`Mismatch` 这个结局不阻止任何事，它只是给一个
/// 已经造成的损失贴一张红标签，而原文件无从找回（临时件此刻也已被 rename 消耗）。更糟的是
/// 前端把「已完成」的行渲染成终态，既没有重试按钮也没有取消按钮——用户看到一行红字，
/// 能做的只有自己想办法。校验若不能否决提交，它就不是校验，是事后讣告。
///
/// # 四层证据，由廉价到昂贵
///
/// ① **临时件仍然归本次传输所有**（#12）。重读伴生身份记录，必须仍与本次的 `ident` 逐字段
///    相等。这一关防的是**进程外**的并发写：`TARGET_LOCKS` 是进程内 static，它拦不住第二个
///    实例，更拦不住另一台机器上的第二个客户端。而伴生记录恰好是这条路上唯一的公共状态——
///    另一个写者只要开工，就必然在 `prepare_part` 里把它改写成自己那一份（源不同 → 记录不同），
///    于是「记录变了」就是「有别人正在写同一个临时件」的确凿证据。
///    记录**消失**（对方提交成功后 `cleanup_identity` 删掉了它）同样判不符：那说明手里这份
///    临时件早已被对方 rename 走，我们正在写的是一个重新创建出来的同名文件。
///    读不到也判不符——这一关的语义是「拿得出所有权证明」，拿不出就不许提交；代价只是一次
///    重试（临时件与最终目标都原样保留，重试时整段续传直接命中），而放行的代价是坏文件。
///
/// ② **源没被就地改写**（#15）。重新量一次源的 size/mtime 与开工时的 `ident` 比对。
///    - 变短 → 硬失败：源被截断或整份重写，手里这份前缀已经不是任何一版内容的开头。
///    - 变长 → **放行**。这是有意的：下载一个还在滚动的日志是正当用法，而追加写不改动
///      前 `total` 字节，手里这份就是一个完全正确的快照。配套地，`exec_once` 两侧的读
///      长度都按 `total - offset` 夹取（见那里），所以增长不会让文件超长。
///    - 长度不变但 mtime 变了 → 硬失败：这是「就地改写」的典型指纹，长度这一维看不见它，
///      而它恰恰会让手里这份变成新旧字节的混合物。两侧 mtime 都取得到时才判——取不到
///      （服务端不回该属性）只是少一层证据，不是证据不利。
///
/// ③ **临时件长度 == total**。近乎免费，且能抓住写入被吞掉的尾巴。
///
/// ④ **内容哈希**。只在 `ident.hash` 存在时跑（= 用户开着传后校验，默认开）。
///    下载在本地算；上传要一条 exec 通道去远端算 `<remote>.fspart` 的 sha256。
///
/// # 什么算「确凿不符」
///
/// 只有**拿到了证据且证据不符**才终结传输。取不到证据（没有 exec 通道、服务端没有
/// `sha256sum`、哈希输出解析不出）一律降级到 ①+②+③，绝不当成失败——否则一台没装 coreutils
/// 的服务器会让所有上传全部失败。这与 `verify::run_verify` 的降级口径一致：**降级不被隐藏**
/// 才是保证，app 层随后仍会照常报出 `SizeOnlyMatch`/`Unverified` 让用户看见。
///
/// 第 ① 关**不**适用这条降级口径，两者要问的问题根本不同：③④ 问「这份内容对不对」，取不到
/// 答案时还有别的层兜着；① 问「这份临时件是不是还是我的」，而它是这个问题的**唯一**答案来源
/// ——降级等于永远回答「是」，那这一关就不存在了。何况两边的失败代价也不对称：① 误判的代价
/// 是一次重试（两个文件都原样在），漏判的代价是把另一个写者搅进来的字节提交成成品。
///
/// # 一处有意保留的成本
///
/// app 层的传后校验没有被删掉，它仍是 UI 那四种结局的唯一来源，于是同一份内容会被哈希
/// **两次**（下载是两次本地读，上传是两次远端 `sha256sum`）。这是明知的冗余：把结局从引擎
/// 透传到 UI 需要动 `verify_plans` 的登记/结算握手，而那正是审计 P2「`Settled` 孤儿」出过
/// 事的地方，不值得在一次数据完整性修复里顺手重构。成本是时间，收益是「坏文件永远进不了
/// 最终路径」——这笔账在正确性一侧。消除冗余记在性能门禁那一项（审计2 #42）里。
async fn precommit_gate(
    job: &TransferJob,
    local: &Path,
    ops: &Arc<dyn SftpOps>,
    exec: Option<&Arc<dyn ExecChannel>>,
    ident: &PartIdentity,
) -> Result<(), crate::Error> {
    let total = ident.size;

    // ① 临时件仍归本次传输所有（审计2 #12）
    let still = match job.direction {
        Direction::Up => read_remote_identity(ops, remote_meta(&job.remote)).await,
        Direction::Down => read_local_identity(local_meta(local)).await,
    };
    if still.as_ref() != Some(ident) {
        return Err(crate::Error::Transfer(format!(
            "提交前复核失败：临时件的伴生身份记录已不再属于本次传输（被改写、已消失或读不到）\
             ——极可能有另一个进程或另一次传输正在写同一个临时件（{}）。最终目标未被触碰。",
            match job.direction {
                Direction::Up => remote_part(&job.remote),
                Direction::Down => local_part(local).display().to_string(),
            }
        )));
    }

    // ② 源未被就地改写
    let now = source_identity(job, local, ops).await.map_err(|e| {
        crate::Error::Transfer(format!(
            "提交前复核源文件失败「{e}」；最终目标未被触碰，数据保留在临时件"
        ))
    })?;
    if now.size < total {
        return Err(crate::Error::Transfer(format!(
            "提交前复核失败：源文件在传输途中变短（{} → {} 字节，{}）——\
             手里这份前缀已不是任何一版内容的开头。最终目标未被触碰。",
            total, now.size, ident.src
        )));
    }
    if now.size == total {
        if let (Some(before), Some(after)) = (ident.mtime, now.mtime) {
            if before != after {
                return Err(crate::Error::Transfer(format!(
                    "提交前复核失败：源文件在传输途中被就地改写（长度未变，修改时间 {before} → {after}，{}）——\
                     手里这份可能是新旧内容的混合物。最终目标未被触碰。",
                    ident.src
                )));
            }
        }
    }

    // ③ 临时件长度
    let part_len = match job.direction {
        Direction::Up => ops.stat_size(&remote_part(&job.remote)).await?,
        Direction::Down => tokio::fs::metadata(local_part(local)).await?.len(),
    };
    if part_len != total {
        return Err(crate::Error::Transfer(format!(
            "提交前复核失败：临时件长度 {part_len} ≠ 应传 {total} 字节。最终目标未被触碰。"
        )));
    }

    // ④ 内容哈希
    let Some(expect) = ident.hash.as_deref() else {
        return Ok(());
    };
    let got: Option<String> = match job.direction {
        Direction::Down => Some(crate::verify::file_sha256(&local_part(local)).await?.0),
        Direction::Up => {
            let Some(exec) = exec else { return Ok(()) };
            let cmd = format!(
                "sha256sum -- {}",
                crate::verify::shell_quote(&remote_part(&job.remote))
            );
            // 通道失败/非零退出/输出解析不出 → 无证据 → 降级，不是失败（见上方文档注）。
            match exec.exec_once(&cmd).await {
                Ok(o) if o.code == Some(0) => {
                    crate::verify::parse_sha256sum(&o.stdout).map(|h| h.0)
                }
                Ok(_) | Err(_) => None,
            }
        }
    };
    match got {
        Some(h) if h != expect => Err(crate::Error::Transfer(format!(
            "提交前复核失败：内容哈希不符（期望 {expect}，实得 {h}）——传输过程中数据已损坏。\
             最终目标未被触碰，损坏的数据留在临时件里等待覆盖或删除。"
        ))),
        _ => Ok(()),
    }
}

/// 原子提交：把临时件改名成最终目标（审计 P0-2）。只有传输**整体成功且通过提交前闸门**后
/// 才会走到这里，因此最终目标要么是完整的旧内容、要么是完整的新内容，不存在中间态。
async fn commit(
    job: &TransferJob,
    local: &Path,
    ops: &Arc<dyn SftpOps>,
) -> Result<(), crate::Error> {
    match job.direction {
        Direction::Up => {
            let part = remote_part(&job.remote);
            // 先落盘、再改名——「写临时件 → fsync → rename」的原子替换次序（与 vault 的
            // 原子写同一个道理）。`write_at` 为了吞吐不再逐块 fsync（见其实现注释），
            // 于是整个文件的持久性就系于这一次：少了它，提交后服务器断电可能留下一个
            // 名字是新的、内容却残缺的文件，而用户的旧文件已经被改名顶替掉了。
            // 落盘失败即不提交：临时件原样留着，续传与重试都还有路可走。
            ops.sync(&part).await.map_err(|e| {
                crate::Error::Transfer(format!(
                    "提交前落盘失败（{part}）：{e}；未改名，最终目标未被触碰"
                ))
            })?;
            let first = match ops.rename(&part, &job.remote).await {
                Ok(()) => return Ok(()),
                Err(e) => e,
            };
            // SSH_FXP_RENAME（SFTP v3）没有覆盖语义，多数服务端在目标已存在时直接拒绝，
            // 故失败后需要给新文件腾地方再试一次。
            //
            // 腾地方的动作是**改名**，不是删除（审计2 #8）。原实现在这里发 `remove(remote)`：
            // 一个不可逆动作，成败与否用户的原文件都已经没了。而它建立在一个根本无法成立的
            // 前提上——「首次失败的原因是目标已存在」。SFTP v3 的 `StatusCode` 里没有
            // FILE_ALREADY_EXISTS（v4+ 才有），OpenSSH 对这种情况回的是通用的
            // SSH_FX_FAILURE，与权限拒绝、配额超限、只读文件系统、目标是目录、服务端内部
            // 错误共用同一个码。也就是说这个前提**在协议层面就判不出来**（详见 `is_not_found`）。
            // 于是原实现真正的语义是：只要 rename 失败，无论什么原因，就删掉用户的文件再赌一把。
            // 磁盘配额超了 → 删掉原文件 → 重试仍失败 → 用户失去原文件，只拿到一句报错。
            //
            // 结论不是「把原因判准」，而是**让正确性不依赖原因**：既然任何一步都可能失败，
            // 那每一步就都必须可回退。改名腾位正是可回退的那个版本——失败就挪回来。
            //
            // 动手前仍先确认临时件在场：临时件都没了就更没有理由去动最终目标。
            if let Err(e) = ops.stat_size(&part).await {
                return Err(crate::Error::Transfer(format!(
                    "提交失败：rename({part} → {}) 报「{first}」，且临时件不可读「{e}」；\
                     最终目标未被触碰",
                    job.remote
                )));
            }
            // 再确认最终目标确实在场。不在场时「目标已存在」必然不是失败原因（此路不通），
            // 让位动作也就毫无意义——而 `rename(remote → bak)` 在目标不存在时还会顺带
            // 制造一个误导性的二次错误，把真正的病因 `first` 埋在噪声里。
            if let Err(e) = ops.stat_size(&job.remote).await {
                return Err(crate::Error::Transfer(format!(
                    "提交失败：rename({part} → {}) 报「{first}」，且最终目标不可读「{e}」\
                     ——失败原因不是「目标已存在」，故未做任何让位动作；\
                     数据完整保留在临时件 {part}",
                    job.remote
                )));
            }
            let bak = format!("{}{}", job.remote, BAK_SUFFIX);
            if let Err(e) = ops.rename(&job.remote, &bak).await {
                return Err(crate::Error::Transfer(format!(
                    "提交失败：rename({part} → {}) 报「{first}」，为其让位改名到 {bak} 又报「{e}」；\
                     最终目标未被触碰，数据完整保留在临时件 {part}",
                    job.remote
                )));
            }
            match ops.rename(&part, &job.remote).await {
                Ok(()) => {
                    // 删备份是尽力而为，**失败不改变提交结论**：新文件已经在位、内容完整，
                    // 此时报 Failed 会让 UI 谎称一件成功的传输失败，用户于是去重传一个本已
                    // 正确的文件。残留的 `.fsbak` 是一份旧内容的副本，不是数据丢失。
                    let _ = ops.remove(&bak).await;
                    Ok(())
                }
                Err(second) => match ops.rename(&bak, &job.remote).await {
                    Ok(()) => Err(crate::Error::Transfer(format!(
                        "提交失败：rename({part} → {}) 首次报「{first}」，让位后重试仍报「{second}」；\
                         原文件已从 {bak} 原样挪回，数据完整保留在临时件 {part}",
                        job.remote
                    ))),
                    Err(restore) => Err(crate::Error::Transfer(format!(
                        "提交失败且未能复原：rename({part} → {}) 首次报「{first}」，\
                         让位后重试报「{second}」，把原文件从 {bak} 挪回又报「{restore}」。\
                         两份数据都完好：原文件在 {bak}，新数据在 {part}——请手动改名，切勿删除。",
                        job.remote
                    ))),
                },
            }
        }
        Direction::Down => {
            let part = local_part(local);
            // std/tokio 的 rename 在 Windows 上走 MoveFileEx + MOVEFILE_REPLACE_EXISTING，
            // 目标已存在也能覆盖；同目录同卷，因此是原子替换而非拷贝。
            tokio::fs::rename(&part, local).await.map_err(|e| {
                crate::Error::Transfer(format!(
                    "提交失败：rename({} → {}) 报「{e}」；数据完整保留在临时件",
                    part.display(),
                    local.display()
                ))
            })
        }
    }
}

/// 收尾本地句柄。下载侧必须 flush + sync_all：`tokio::fs::File` 带用户态写缓冲，
/// 不 flush 就 rename 会把没落盘的尾巴丢掉；不 sync 则断电后临时件长度与内容可能不一致，
/// 续传会从一个「看起来有数据、实际是空洞」的偏移起跑。
///
/// 显式 `into_std()` 后同步 drop，而不是直接丢 `tokio::fs::File`：后者的 Drop 是把关闭动作
/// 扔进阻塞线程池的**异步**行为，Windows 上会与紧随其后的 rename 抢共享位（ERROR_SHARING_VIOLATION）。
async fn finish_local_file(
    mut f: tokio::fs::File,
    direction: Direction,
) -> Result<(), crate::Error> {
    use tokio::io::AsyncWriteExt;
    if direction == Direction::Down {
        f.flush().await?;
        f.sync_all().await?;
    }
    drop(f.into_std().await);
    Ok(())
}

/// 单次尝试的结束方式。
///
/// 用返回值而不是「事后重读取消标志」来区分这两种收尾，是因为后者天然有竞态：`exec_once`
/// 正常读到 EOF 返回 `Ok`（此时**一条终态事件都没发**）之后、`run_transfer` 读标志之前，
/// 若取消恰在这一瞬置位，就会走进「已取消，事件已由 exec_once 发出」的分支直接 return——
/// 实际上谁都没发。UI 上那一行永远停在「传输中」，关闭确认永远拦着不让退；而 `cancels`
/// 表项已被摘除、`active_len()` 归零，关停自检还会报「一切干净」。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Attempt {
    /// 读到 EOF、数据已全部就位，等待 `commit`。
    Completed,
    /// 命中取消：`Cancelled` 终态事件已在 `exec_once` 内发出，调用方**不得**再补发终态。
    Cancelled,
}

/// 单次尝试。`local` 已由 `run_transfer` 完成沙箱解析；`offset` 是跨重试共享的已完成游标，
/// 本函数**原地推进**它，故失败后重入不会退回起点。
///
/// 读写两端的「目标」一律是临时件：上传写 `<remote>.fspart`、下载写 `<dest>.fspart`，
/// 最终目标只在 `commit` 里被 rename 触碰一次。
///
/// 参数确实多于 clippy 默认阈值：拆成 Ctx 结构体只是把同一批值换个地方传，
/// 反而多一层间接，不如就地放行。
#[allow(clippy::too_many_arguments)]
async fn exec_once(
    id: TransferId,
    job: &TransferJob,
    local: &Path,
    ops: &Arc<dyn SftpOps>,
    total: u64,
    ev: &mpsc::Sender<TransferEvent>,
    cancelled: &AtomicBool,
    offset: &mut u64,
) -> Result<Attempt, crate::Error> {
    use tokio::io::AsyncSeekExt;
    // 本地句柄在循环**外**开一次并 seek 一次。原实现每 256 KiB 就重开 + 重 seek：
    // 1 GiB 文件 = 4096 次 CreateFileW，Windows 上每次都要过一遍 Defender 过滤驱动（S45）。
    // 且一律走 `tokio::fs`：`std::fs` 的同步 read/write 会阻塞 tokio 工作线程，并发传输
    // 占满工作线程时连 SSH 会话自身的 I/O 任务都会挨饿（keepalive 超时 → 传输中途断线）；
    // 本 crate 其余处（verify.rs）早已是 tokio::fs，此处曾是唯一例外（S44）。
    let mut file = match job.direction {
        Direction::Up => tokio::fs::File::open(local).await?,
        Direction::Down => {
            let part = local_part(local);
            // 每次重入都重查一遍末段软链（审计 P2）：`prepare_part` 只在开工时查过一次，
            // 而失败重试之间隔着 1 s / 2 s 退避，那正是「趁虚而入种一个同名软链」的窗口。
            // 一次 `symlink_metadata` 的开销相对于整轮重试可以忽略。
            crate::sandbox::reject_symlink_leaf(&part)?;
            // `create(false)`：临时件必由 `prepare_part` 建好（续传分支只在它确实存在时才返回，
            // 否则当场创建），这里只该打开一个**已存在**的文件，不该有创建语义。
            // 收益只有一点，但正是这里要的那一点：上面那道检查与这句 open 之间终究隔着一道
            // TOCTOU 缝，若恰在此刻被换成一条**悬空**软链，`create(true)` 会顺着它在沙箱外
            // 凭空造出目标文件，而 `create(false)` 只会得到一个 ENOENT。代价为零，
            // 于是取零代价的那一侧。
            //
            // （不宣称它能防空洞：临时件被外部删掉时，紧随其后的 `on_disk < *offset` 夹取
            // 已经把游标拉回 0，本就不会留洞。那条收益是别处给的，不记在这句账上。）
            tokio::fs::OpenOptions::new()
                .create(false)
                .truncate(false) // 续传：绝不能截断已落盘的前缀（清零只在 prepare_part 做一次）
                .write(true)
                .open(&part)
                .await?
        }
    };
    // 下载重入的自愈：`tokio::fs::File` 带用户态写缓冲，而错误路径上句柄是被直接 drop 的
    // （`?` 提前返回，来不及 flush），末尾若干字节的写入错误会被吞掉——可 `offset` 早已按
    // 「write_all 返回 Ok」推进过了。重入时照着这个虚高的游标 seek，中间那段就成了一个
    // **空洞**（读出来是 0）：长度对、内容错，正是 P1-5 要根除的那一类。故每次重入都拿
    // 临时件的真实长度给游标封顶，多传一遍总好过悄悄留个洞。
    if job.direction == Direction::Down {
        let on_disk = tokio::fs::metadata(local_part(local)).await?.len();
        if on_disk < *offset {
            *offset = on_disk;
        }
    }
    file.seek(std::io::SeekFrom::Start(*offset)).await?;
    match job.direction {
        Direction::Up => upload_loop(id, job, local, ops, total, ev, cancelled, offset, file).await,
        Direction::Down => download_loop(id, job, ops, total, ev, cancelled, offset, file).await,
    }
}

/// 取消的收尾（两个方向共用）：本地句柄收好，发 Cancelled 终态。
///
/// 临时件**保留**：取消不等于放弃，用户下次带 resume 即从这里续跑；而最终目标
/// 全程未被触碰，所以「取消」对用户已有的文件零影响。失败路径同理。
async fn cancel_out(
    id: TransferId,
    direction: Direction,
    file: tokio::fs::File,
    ev: &mpsc::Sender<TransferEvent>,
    done: u64,
    total: u64,
) -> Result<Attempt, crate::Error> {
    finish_local_file(file, direction).await?;
    let _ = ev
        .send(TransferEvent {
            id,
            bytes_done: done,
            bytes_total: total,
            state: TransferState::Cancelled,
        })
        .await;
    Ok(Attempt::Cancelled)
}

/// 上传：一个写入器（同一远端句柄上最多 `sftp::WRITE_PIPELINE` 个写请求在途，1.0.1）。
///
/// # 两个游标
///
/// - `queued`：已经交给写入器的字节（进度条显示它，也决定下一块从本地哪里读）；
/// - `*offset`：**已确认**写到服务端的字节下界（写入器的 `confirmed`）。失败重试与续传
///   只认这一个：拿 `queued` 当断点，失败后就会从服务端其实没收到的位置续写，留下空洞。
///
/// 失败时远端可能还落下了一截「已发出、未确认」的数据，这截数据本身是连续的（同一句柄、
/// 顺序写、服务端按序处理），但为了让远端大小严格等于断点，仍尽力把临时件截回 `*offset`。
/// 截断失败（多半是连接已断）无妨：同一件传输的重试会从 `*offset` 覆盖写过去，跨进程的
/// 续传以远端大小为断点，而那仍是一个连续前缀。
#[allow(clippy::too_many_arguments)]
async fn upload_loop(
    id: TransferId,
    job: &TransferJob,
    local: &Path,
    ops: &Arc<dyn SftpOps>,
    total: u64,
    ev: &mpsc::Sender<TransferEvent>,
    cancelled: &AtomicBool,
    offset: &mut u64,
    mut file: tokio::fs::File,
) -> Result<Attempt, crate::Error> {
    use tokio::io::AsyncReadExt;
    let remote_target = remote_part(&job.remote);
    let mut writer: Option<Box<dyn crate::sftp::RemoteWriter + '_>> = None;
    let mut queued = *offset;
    loop {
        // 用户单件取消检查（per-id AtomicBool，TransferManager::cancel / cancel_all 置位）
        if cancelled.load(Ordering::SeqCst) {
            if let Some(w) = &writer {
                *offset = w.confirmed();
            }
            drop(writer);
            return cancel_out(id, job.direction, file, ev, *offset, total).await;
        }
        // 本轮还该读多少（审计2 #15）。**按 `total - queued` 夹取**，而不是每次都读满 CHUNK。
        //
        // 不夹取时，源文件在传输途中变长会让循环一路跟着新数据往下跑，直到源停止增长才停；
        // 得到的文件比开工时量到的 `total` 长，进度条冲过 100%，事后校验必然 Mismatch——
        // 而一个还在被追加的日志/数据库正是最常见的传输对象，这不是边角情况。夹取之后，
        // 传的永远是「开工那一刻的前 total 字节」这样一个定义清晰的快照：长度确定、
        // 与开工时算好的期望哈希对得上、可续传、可复现。
        //
        // 夹到 0 即「该传的都传完了」，这也是唯一的正常完成出口。
        let want = total.saturating_sub(queued).min(CHUNK as u64) as usize;
        if want == 0 {
            if let Some(w) = writer.take() {
                let floor = w.confirmed();
                if let Err(e) = w.finish().await {
                    *offset = floor;
                    if floor < queued {
                        let _ = ops.truncate(&remote_target, floor).await;
                    }
                    finish_local_file(file, job.direction).await?;
                    return Err(e);
                }
            }
            *offset = queued; // 写入器已等齐全部确认
            finish_local_file(file, job.direction).await?;
            return Ok(Attempt::Completed);
        }
        // 填满整块再发：`read` 允许短读，逐次短读会把一次整块的 SFTP 写打散成多次小写，
        // 白白多花往返。`filled == 0` 即 EOF。
        let mut buf = vec![0u8; want];
        let mut filled = 0usize;
        while filled < want {
            let n = file.read(&mut buf[filled..]).await?;
            if n == 0 {
                break;
            }
            filled += n;
        }
        if filled == 0 {
            if let Some(w) = &writer {
                *offset = w.confirmed();
            }
            drop(writer);
            finish_local_file(file, job.direction).await?;
            // 与下载侧对称（审计 P1-6）：还没读满开工时量到的长度就到了 EOF，说明源文件
            // 在传输途中被人改短了。此时提交上去的是个残缺文件，且长度信息已经对不上，
            // 续传也修不回来——宁可报错让用户重来。
            return Err(crate::Error::Transfer(format!(
                "本地源文件在传输途中变短：已读 {queued}/{total} 字节（{}）",
                local.display()
            )));
        }
        buf.truncate(filled);
        if writer.is_none() {
            writer = Some(ops.open_writer(&remote_target, queued).await?);
        }
        let w = writer.as_mut().expect("刚刚打开");
        if let Err(e) = w.write(&buf).await {
            *offset = w.confirmed();
            drop(writer);
            if *offset < queued {
                let _ = ops.truncate(&remote_target, *offset).await;
            }
            finish_local_file(file, job.direction).await?;
            return Err(e);
        }
        queued += filled as u64;
        *offset = w.confirmed();
        let _ = ev
            .send(TransferEvent {
                id,
                bytes_done: queued,
                bytes_total: total,
                state: TransferState::Running,
            })
            .await;
    }
}

/// 下载窗口上限：同时在途的读请求数（1.0.1）。
pub const READ_WINDOW_MAX: usize = 8;
/// 一个读请求在这个时间内完成，窗口加一。
const READ_FAST: std::time::Duration = std::time::Duration::from_secs(1);
/// 一个读请求用了这么久才完成，窗口减半。
const READ_SLOW: std::time::Duration = std::time::Duration::from_secs(5);

/// 下载：窗口内并发发出读请求，**按偏移顺序**落盘（1.0.1）。
///
/// # 为什么窗口是自适应的
///
/// 排在窗口后面的请求要等前面的数据都传完才会被应答，而每次读都受
/// `timeouts::DATA_TIMEOUT`（120 s）约束。固定 8 个在途的话，链路低于约 17 KB/s
/// 第八个请求就会超时；而窗口为 1 时下限是一块 255 KiB / 120 s ≈ 2.1 KB/s——卫星链路、
/// 2G 回落正落在这两者之间。所以从 1 起步：请求很快回来（≤ 1 s）就加一，慢了（≥ 5 s）
/// 就减半。快而远的链路很快涨到上限、把往返时间藏起来；慢链路停在 1–2，逐块顺序读。
/// （1.0.0 连 26 KB/s 以下都传不了：russh-sftp 默认每请求 10 s，见 `sftp::REQUEST_TIMEOUT_SECS`。）
///
/// # 读句柄复用
///
/// 每个在途的读占用一个读取器（[`SftpOps::open_reader`]），读完归还到空闲池，下一次读
/// 直接复用：每块只剩一次 READ 往返，而逐块 `read_range` 要先 OPEN 再 READ。读取器在
/// 请求自己的 future 里按需打开，窗口初次填满时不必串行等 8 次 OPEN；池子大小因此不超过
/// 窗口达到过的最大值。读失败的读取器随结果一起丢弃，不再归还。
///
/// # 断点语义不变
///
/// 本地文件由我们按序写，`*offset` 始终是已落盘的连续前缀；某个请求失败时，排在它后面的
/// 在途请求连同结果一起丢弃。
#[allow(clippy::too_many_arguments)]
async fn download_loop(
    id: TransferId,
    job: &TransferJob,
    ops: &Arc<dyn SftpOps>,
    total: u64,
    ev: &mpsc::Sender<TransferEvent>,
    cancelled: &AtomicBool,
    offset: &mut u64,
    mut file: tokio::fs::File,
) -> Result<Attempt, crate::Error> {
    use futures::stream::{FuturesOrdered, StreamExt};
    use tokio::io::AsyncWriteExt;
    // 第三项是这次读的耗时：在请求自己的 future 里量（从首次被轮询到应答），与
    // `TimedSftp` 超时守卫计时的区间一致；出队时再量会把队头阻塞和本地落盘也算进去。
    // 第四项是这次读用的读取器，读成功时归还空闲池。
    type Reader<'r> = Box<dyn crate::sftp::RemoteReader + 'r>;
    type Read<'r> = (
        u64,
        usize,
        std::time::Duration,
        Option<Reader<'r>>,
        Result<Vec<u8>, crate::Error>,
    );
    let ops: &dyn SftpOps = ops.as_ref();
    let path = job.remote.as_str();
    let mut inflight: FuturesOrdered<futures::future::BoxFuture<'_, Read<'_>>> =
        FuturesOrdered::new();
    let mut idle: Vec<Reader<'_>> = Vec::new();
    let mut window = 1usize;
    let mut next = *offset;
    loop {
        if cancelled.load(Ordering::SeqCst) {
            drop(inflight);
            return cancel_out(id, job.direction, file, ev, *offset, total).await;
        }
        // 补满窗口。每块按 `total - next` 夹取（审计2 #15，理由见上传侧同名注释）。
        while inflight.len() < window && next < total {
            let want = (total - next).min(CHUNK as u64) as usize;
            let (reader, at) = (idle.pop(), next);
            inflight.push_back(Box::pin(async move {
                // tokio 的 Instant：测试用暂停时钟模拟「慢读」时计时跟着走（std 的不会）
                let started = tokio::time::Instant::now();
                let mut reader = match reader {
                    Some(r) => r,
                    None => match ops.open_reader(path).await {
                        Ok(r) => r,
                        Err(e) => return (at, want, started.elapsed(), None, Err(e)),
                    },
                };
                let r = reader.read_at(at, want).await;
                (at, want, started.elapsed(), Some(reader), r)
            }));
            next += want as u64;
        }
        let Some((at, want, took, reader, r)) = inflight.next().await else {
            // 窗口空、且 `next == total`：该收的都收齐了
            finish_local_file(file, job.direction).await?;
            return Ok(Attempt::Completed);
        };
        debug_assert_eq!(at, *offset, "按序落盘：出队的块必须正好接在已落盘前缀之后");
        let buf = r?;
        idle.extend(reader);
        // 收到多少先落多少（空块时是空操作），断点因此始终等于已落盘的前缀。
        file.write_all(&buf).await?;
        *offset += buf.len() as u64;
        if buf.len() < want {
            // 短块（含空块）= 远端在开工量到的长度之前就到了 EOF（`read_range` 只在 EOF 时
            // 少给）：服务端半途返回空包、文件被别人截短、连接半断。无条件当 EOF 的话，残缺
            // 文件会被判 Done 并提交上去，用户毫不知情（审计 P1-6）。
            //
            // 流水线下还有第二层理由：窗口里排在后面的请求是按整块偏移发出的，此时已与落盘
            // 位置错开，**不得**再用——源文件若又长回来，它们的数据会落到错误的位置上。
            return Err(crate::Error::Transfer(format!(
                "远端提前返回空块：已收 {offset}/{total} 字节（{}）——疑似服务端截断或连接半断",
                job.remote
            )));
        }
        if took <= READ_FAST {
            window = (window + 1).min(READ_WINDOW_MAX);
        } else if took >= READ_SLOW {
            window = (window / 2).max(1);
        }
        let _ = ev
            .send(TransferEvent {
                id,
                bytes_done: *offset,
                bytes_total: total,
                state: TransferState::Running,
            })
            .await;
    }
}

#[cfg(test)]
mod chunk_tests {
    /// 分块对齐 OpenSSH `limits@openssh.com` 通告的单次读写上限（261 120 字节）。
    /// 大于它就被切成两个请求、下行每块多一次往返；这件事只在真服务端的吞吐上看得见。
    #[test]
    fn chunk_matches_the_openssh_sftp_read_write_limit() {
        assert_eq!(super::CHUNK, 256 * 1024 - 1024);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 同一个远端文件的各种写法必须落到**同一把锁**上（审计 P1 的另一半）。
    ///
    /// 端点修对了只解决「哪台主机」，路径这一维照样能把一个文件拆成多把锁：前端的远端路径是
    /// 「当前目录 + 条目名」拼出来的，`cwd = "/"` 时就会拼出 `//tmp/a.bin`。两把锁写一个
    /// `.fspart`，两件都报 Done，用户拿到的是两份内容交错拼起来的垃圾。
    #[test]
    fn normalize_remote_folds_every_spelling_of_one_path() {
        for spelling in [
            "/tmp/a.bin",
            "//tmp/a.bin",
            "/tmp//a.bin",
            "/tmp/./a.bin",
            "/tmp/x/../a.bin",
            "/./tmp/a.bin",
            "/tmp/a.bin/", // SFTP 服务端对文件路径的结尾斜杠通常宽容
        ] {
            assert_eq!(
                normalize_remote(spelling),
                "/tmp/a.bin",
                "{spelling} 与 /tmp/a.bin 是同一个文件，必须组出同一把锁的键"
            );
        }
    }

    /// 归一化只许**合并**，不许**拆分**，更不许把两个不同文件并成一个。
    #[test]
    fn normalize_remote_keeps_distinct_paths_distinct() {
        let distinct = [
            "/tmp/a.bin",
            "/tmp/b.bin",
            "/tmp/sub/a.bin",
            "/a.bin",
            // 相对路径无从消解成绝对路径（要一次服务端 realpath），故必须与绝对路径分开——
            // 合并它们才是真正危险的方向：会把 `/a.bin` 与某个 cwd 下的 `a.bin` 判成一个。
            "tmp/a.bin",
            "../a.bin",
        ];
        for (i, x) in distinct.iter().enumerate() {
            for y in &distinct[i + 1..] {
                assert_ne!(
                    normalize_remote(x),
                    normalize_remote(y),
                    "{x} 与 {y} 不是同一个文件，归一化不得把它们并成一把锁"
                );
            }
        }
    }

    /// 根与「当前目录」这两个退化输入不能塌成空串——空键会把「根目录」与「任意相对路径」
    /// 混在一起，也让报错信息里出现一个什么都没写的目标名。
    #[test]
    fn normalize_remote_handles_degenerate_paths() {
        assert_eq!(normalize_remote("/"), "/");
        assert_eq!(normalize_remote("//"), "/");
        assert_eq!(normalize_remote("/.."), "/", "POSIX 上根的父目录仍是根");
        assert_eq!(normalize_remote("/tmp/.."), "/");
        assert_eq!(normalize_remote(""), ".");
        assert_eq!(normalize_remote("."), ".");
        assert_eq!(normalize_remote("a/.."), ".");
        assert_eq!(
            normalize_remote("../../a"),
            "../../a",
            "相对路径的前导 .. 没有基准可消解，必须原样保留"
        );
    }

    // -----------------------------------------------------------------------
    // 审计2 #13：主机与路径别名绕过同目标锁
    // -----------------------------------------------------------------------

    #[test]
    fn split_parent_leaf_covers_every_shape_of_remote_path() {
        assert_eq!(split_parent_leaf("/tmp/a.bin"), Some(("/tmp", "a.bin")));
        assert_eq!(
            split_parent_leaf("/a.bin"),
            Some(("/", "a.bin")),
            "根下的文件，父目录是 `/` 而不是空串——空串会被拼成相对路径"
        );
        assert_eq!(
            split_parent_leaf("a.bin"),
            Some((".", "a.bin")),
            "无斜杠的相对路径，父目录是 SFTP 会话起始目录，即 `.`"
        );
        assert_eq!(split_parent_leaf("sub/a.bin"), Some(("sub", "a.bin")));
        assert_eq!(split_parent_leaf("../a.bin"), Some(("..", "a.bin")));
        // 没有叶子可谈的三种退化输入：它们都是目录而非传输目标
        assert_eq!(split_parent_leaf("/"), None);
        assert_eq!(split_parent_leaf("."), None);
        assert_eq!(split_parent_leaf(".."), None);
    }

    /// 大小写不敏感卷上的两种拼法必须共用一把锁；敏感卷上不得被误折。
    #[test]
    fn local_lock_keys_fold_case_only_when_asked() {
        let p = Path::new("/box/Report.PDF");
        let folded = local_lock_keys(p, true);
        let exact = local_lock_keys(p, false);
        assert_eq!(exact.len(), 1, "大小写敏感卷上只该有精确键一维");
        assert_eq!(folded.len(), 2, "不敏感卷上必须额外挂一维折叠键");
        // 交集判据：另一种拼法必须撞进同一把锁
        let other = local_lock_keys(Path::new("/box/report.pdf"), true);
        assert!(
            folded.iter().any(|k| other.contains(k)),
            "Report.PDF 与 report.pdf 在不敏感卷上是同一个文件，键集必须相交：\
             {folded:?} vs {other:?}"
        );
        assert!(
            !exact
                .iter()
                .any(|k| local_lock_keys(Path::new("/box/report.pdf"), false).contains(k)),
            "敏感卷上它们是两个文件，不得相交"
        );
    }

    /// 全小写路径不该凭空多出一维重复键（`try_lock_targets` 会去重，但键集本身也不该注水）。
    #[test]
    fn local_lock_keys_do_not_duplicate_an_already_lowercase_path() {
        assert_eq!(local_lock_keys(Path::new("/box/report.pdf"), true).len(), 1);
    }

    /// 抢键必须**升序**走，否则两件传输会互相踩死：A 持 k1 求 k2、B 持 k2 求 k1，
    /// 双方同时失败，同一个目标一件都传不动，而报错都是「目标忙」——用户对着一个
    /// 没人在传的目标反复重试，永远是忙。证明见 `try_lock_targets` 文档。
    ///
    /// 顺序本身不可直接观测（函数是同步的，外部插不进去），但**失败点**可以：两把键都被
    /// 外部占住时，报出来的那把就是它先摸的那把。给一个逆序的入参即可判别。
    #[test]
    fn try_lock_targets_walks_keys_in_sorted_order() {
        let _z = try_lock_target("sortcheck:zzz").expect("空表必然抢得到");
        let _a = try_lock_target("sortcheck:aaa").expect("空表必然抢得到");
        let busy = try_lock_targets(&["sortcheck:zzz".into(), "sortcheck:aaa".into()])
            .expect_err("两把键都被占，必须失败");
        assert_eq!(
            busy, "sortcheck:aaa",
            "入参是逆序的，报出 zzz 即说明按入参顺序抢——那正是会互相踩死的写法"
        );
    }

    /// 抢不全就一把都不留：半持有状态会让下一件传输撞上一把没有主人的锁，永远说忙。
    #[test]
    fn try_lock_targets_releases_everything_when_one_key_is_busy() {
        let held = try_lock_target("allornothing:held").expect("空表必然抢得到");
        let busy = try_lock_targets(&["allornothing:free".into(), "allornothing:held".into()])
            .expect_err("其中一把被占，整组必须失败");
        assert_eq!(busy, "allornothing:held");
        assert!(
            try_lock_target("allornothing:free").is_some(),
            "失败路径必须把已抢到的 free 放回去"
        );
        drop(held);
    }

    /// 同一把键在键集里出现两次（别名折叠回主键就是这个局面，而且是最常见的一种）
    /// 不得让一件孤零零的传输自己撞死自己。
    #[test]
    fn try_lock_targets_dedups_so_a_lone_transfer_never_blocks_itself() {
        let g = try_lock_targets(&["dedup:same".into(), "dedup:same".into()])
            .expect("重复键必须去重，否则第二次必然撞上自己刚拿的那把");
        assert_eq!(g.len(), 1, "去重后只该持有一把");
    }

    /// 键集 = 端点维 × 路径维的笛卡尔积；规范路径来自服务端 REALPATH，只问父目录。
    #[tokio::test]
    async fn remote_lock_keys_multiply_endpoints_by_paths() {
        let ops: Arc<dyn SftpOps> = Arc::new(StubCanon::answering("/link", Some("/real")));
        let job = keys_job("/link/a.bin", "h:22", &["1.2.3.4:22"]);
        let keys = remote_lock_keys(&job, &ops).await;
        assert_eq!(
            keys,
            vec![
                "remote:h:22:/link/a.bin",
                "remote:h:22:/real/a.bin",
                "remote:1.2.3.4:22:/link/a.bin",
                "remote:1.2.3.4:22:/real/a.bin",
            ],
            "两个端点 × 两条路径 = 四把键；词法名必须保留，否则与「解析失败的那一件」完全错开"
        );
    }

    /// REALPATH 失败 → 只剩词法名，传输照常。降级不是中止。
    #[tokio::test]
    async fn remote_lock_keys_degrade_to_lexical_when_realpath_fails() {
        let ops: Arc<dyn SftpOps> = Arc::new(StubCanon::answering("/nope", None));
        let job = keys_job("/nope/a.bin", "h:22", &[]);
        assert_eq!(
            remote_lock_keys(&job, &ops).await,
            vec!["remote:h:22:/nope/a.bin"]
        );
    }

    /// 服务端回一个空串时不得拼成 `/a.bin`——那会凭空把目标指到根目录上，
    /// 把两个毫不相干的文件锁成一个。
    #[tokio::test]
    async fn remote_lock_keys_reject_an_empty_realpath_answer() {
        let ops: Arc<dyn SftpOps> = Arc::new(StubCanon::answering("/d", Some("")));
        let job = keys_job("/d/a.bin", "h:22", &[]);
        assert_eq!(
            remote_lock_keys(&job, &ops).await,
            vec!["remote:h:22:/d/a.bin"]
        );
    }

    /// 叶子是 `..`、路径是 `/` 这类没有叶子的输入不得去问 REALPATH，也不得多出键。
    #[tokio::test]
    async fn remote_lock_keys_skip_realpath_for_pathless_targets() {
        let stub = Arc::new(StubCanon::answering("/", Some("/somewhere-else")));
        let ops: Arc<dyn SftpOps> = stub.clone();
        let job = keys_job("/", "h:22", &[]);
        assert_eq!(remote_lock_keys(&job, &ops).await, vec!["remote:h:22:/"]);
        assert_eq!(stub.asked(), 0, "没有叶子可拼时不该白发一次 REALPATH");
    }

    fn keys_job(remote: &str, endpoint: &str, aliases: &[&str]) -> TransferJob {
        TransferJob {
            direction: Direction::Up,
            local: PathBuf::from("/unused"),
            remote: remote.into(),
            resume: false,
            sandbox_root: None,
            target_endpoint: endpoint.into(),
            endpoint_aliases: aliases.iter().map(|s| (*s).to_string()).collect(),
            verify: None,
        }
    }

    /// 只回答 REALPATH 的桩：其余 11 个方法一律 `unreachable!`——`remote_lock_keys`
    /// 若哪天顺手发了别的请求，这里会当场炸而不是悄悄多一次往返。
    struct StubCanon {
        path: String,
        answer: Option<String>,
        asked: std::sync::atomic::AtomicUsize,
    }

    impl StubCanon {
        fn answering(path: &str, answer: Option<&str>) -> Self {
            Self {
                path: path.into(),
                answer: answer.map(String::from),
                asked: std::sync::atomic::AtomicUsize::new(0),
            }
        }
        fn asked(&self) -> usize {
            self.asked.load(std::sync::atomic::Ordering::Relaxed)
        }
    }

    #[async_trait::async_trait]
    impl SftpOps for StubCanon {
        async fn canonicalize(&self, path: &str) -> Result<String, crate::Error> {
            self.asked
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            assert_eq!(path, self.path, "只该问父目录，不该问整条路径");
            self.answer
                .clone()
                .ok_or_else(|| crate::Error::Sftp("realpath 不可用".into()))
        }
        async fn list(&self, _: &str) -> Result<crate::sftp::ListResult, crate::Error> {
            unreachable!("算锁键不该列目录")
        }
        async fn stat_size(&self, _: &str) -> Result<u64, crate::Error> {
            unreachable!("算锁键不该 stat")
        }
        async fn stat_meta(&self, _: &str) -> Result<crate::sftp::FileMeta, crate::Error> {
            unreachable!("算锁键不该 stat")
        }
        async fn read_range(&self, _: &str, _: u64, _: usize) -> Result<Vec<u8>, crate::Error> {
            unreachable!("算锁键不该读数据")
        }
        async fn write_at(&self, _: &str, _: u64, _: &[u8]) -> Result<(), crate::Error> {
            unreachable!("算锁键不该写数据")
        }
        async fn truncate(&self, _: &str, _: u64) -> Result<(), crate::Error> {
            unreachable!("算锁键不该截断")
        }
        async fn sync(&self, _: &str) -> Result<(), crate::Error> {
            unreachable!("算锁键不该落盘")
        }
        async fn mkdir(&self, _: &str) -> Result<(), crate::Error> {
            unreachable!("算锁键不该建目录")
        }
        async fn remove(&self, _: &str) -> Result<(), crate::Error> {
            unreachable!("算锁键不该删文件")
        }
        async fn remove_dir(&self, _: &str) -> Result<(), crate::Error> {
            unreachable!("算锁键不该删目录")
        }
        async fn rename(&self, _: &str, _: &str) -> Result<(), crate::Error> {
            unreachable!("算锁键不该改名")
        }
        async fn lstat(&self, _: &str) -> Result<crate::sftp::FileMeta, crate::Error> {
            unreachable!("算锁键不该 lstat")
        }
        async fn read_link(&self, _: &str) -> Result<String, crate::Error> {
            unreachable!("算锁键不该读软链")
        }
        async fn symlink(&self, _: &str, _: &str) -> Result<(), crate::Error> {
            unreachable!("算锁键不该建软链")
        }
    }
}
