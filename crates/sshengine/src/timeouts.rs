//! 远程 I/O 的时间边界与超时后的退出语义（审计2 #11 / #25）。
//!
//! 在此之前，建连路径上的五段各有预算（`connect.rs` 的 P1-8），但**连接建立之后**的
//! 一切远程调用——每一次 SFTP 往返、每一次校验 exec、agent 存活探测——都没有任何上限。
//! 一台三次握手正常、SSH 握手正常、认证也通过，随后就再不回话的服务器（半断的 NAT 映射、
//! 打满 `MaxSessions` 的 sshd、被中间设备静默丢包的长连接）因此可以把一个 worker 永久占住。
//!
//! 这类挂死在本程序里格外难看，因为它发生在**持锁之后**：`AppState::sftp_ops_for` 与
//! `RusshExecAdapter::exec_once` 都是先 `handle.lock().await` 再发请求，一次无限期等待
//! 会把同一条会话上的所有其他操作一并冻住，表现为「整个标签页再也点不动」。
//!
//! ## 两条语义，缺一条都不算修好
//!
//! **① 每一次远程调用都有预算。** 装饰器套在 `SftpOps` / `ExecChannel` 这两个 trait 上，
//! 而不是逐个调用点去包——传输引擎里的远程调用有几十处，逐点包的做法只要漏一处就等于没做，
//! 而且没有任何东西能守住「后来新增的调用点也要包」。套在 trait 上则是：想发远程请求，
//! 就只能经过这一层。
//!
//! **② 超时之后这条通道就算死了。** 这一条比第一条更重要，也更容易被漏掉。
//! `tokio::time::timeout` 到点只是把 future **丢掉**，它不会撤销已经发到线上的请求：
//! 那条 `SSH_FXP_RENAME` 可能仍在服务端排队，可能一秒后就生效。此时若照常发下一条命令，
//! 我们就是在一个状态未知的通道上继续改用户的文件——审计2 #8 刚修完的那类数据损失，
//! 正是从「以为自己知道对端处于什么状态」开始的。所以第一次超时即把整个 `TimedSftp`
//! 判死，后续调用不再上线、就地返回 [`Error::Poisoned`]，由上层丢弃通道重建。
use crate::sftp::{FileMeta, ListResult, SftpOps};
use crate::verify::ExecChannel;
use crate::Error;
use async_trait::async_trait;
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// 单次控制面往返（stat / rename / remove / mkdir / readlink / symlink / truncate）上限。
///
/// 这些请求的响应体都是几十字节，耗时与文件大小无关，正常情况下就是一个 RTT。30 s 已经
/// 宽出两个数量级，留的是给负载极高的服务端和跨洲链路的余量，不是给「慢」留的——
/// 一次 stat 要跑满 30 s 的服务器，无论如何都该让用户知道。
pub const CONTROL_TIMEOUT: Duration = Duration::from_secs(30);

/// 单次数据面往返（read_range / write_at / list）上限。
///
/// 分块是 256 KiB，120 s 对应约 2 KiB/s 的下限——比任何还能称为「可用」的链路都低。
/// 之所以不压到跟控制面一个量级：卫星链路、2G 回落、共享带宽被打满的机器确实会慢到
/// 几 KiB/s，而在那种链路上传文件是本程序的正当用途，误杀它比多等两分钟糟得多。
///
/// `list` 归在数据面而非控制面：一个几万条目的目录，响应体可以到几 MB。
/// （目录条目数本身该有上限，那是审计2 #38 的事，与本预算不冲突。）
pub const DATA_TIMEOUT: Duration = Duration::from_secs(120);

/// 一次性 exec 的**总**上限（`sha256sum` 等）。
///
/// 这一条与上面两条的性质不同：命令本体可以合法地跑很久，而且跑的过程中通道上**一个字节
/// 都不会有**——`sha256sum` 读完整个文件才输出一行。所以这里既不能用「首字节」也不能用
/// 「空闲」做判据，只有总时长可用。
///
/// 10 分钟按 100 MB/s 覆盖约 60 GB。超出的情形不会让传输失败：提交前闸门与传后校验
/// 都把 exec 失败当作**证据缺失**而非不符（见 `transfer::precommit_gate` 第③关与
/// `verify::run_verify`），所以超时的后果是这一件退到降级校验，不是把一个好文件判死。
pub const EXEC_TOTAL_TIMEOUT: Duration = Duration::from_secs(600);

/// exec **整次调用**的兜底上限，含 `run_exec_channel` 看不见的那两段（开通道、发 exec 请求）。
///
/// 比 [`EXEC_TOTAL_TIMEOUT`] 多留一分钟，是为了让内层那道先响：内层到点会带着已收到的部分
/// 输出正常返回，走的是「证据缺失 → 降级」这条温和路径；外层到点则是把 future 丢掉，
/// 通道状态就此不明。两道边界若取同一个值，谁先响就成了调度器的运气。
pub const EXEC_HARD_TIMEOUT: Duration = Duration::from_secs(660);

/// agent 存活探测上限（审计2 #25）。
///
/// 探测走的是本机 IPC（unix socket / 命名管道 / Pageant 的 WM_COPYDATA），正常在毫秒级。
/// 会卡住的是这些情形：管道对端是个半死的进程、Pageant 所在的窗口线程正忙、
/// 或者 agent 背后接的是要用户按指纹的硬件 token。前两种该尽快放弃并回退到下一条传输，
/// 第三种不该走探测这条路（探测只发 `request_identities`，列身份不需要用户确认）。
/// 5 s 对本机 IPC 是很长的时间，同时又短到不会让「agent 没起来」这件事拖住整个连接。
pub const AGENT_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// 给一个 await 套上**点名阶段**的超时。
///
/// 返回 `Result<F::Output, Error>`：只负责「超时与否」，内层自己的 Result 原样交回调用方
/// 处置，避免把两类失败（超时 / 对端明确报错）在这里就压成同一个字符串。
///
/// `stage` 会原样进错误文案，所以它必须是**能指导排查**的一句话，而不是函数名。
pub async fn with_timeout<F: Future>(
    stage: &str,
    budget: Duration,
    fut: F,
) -> Result<F::Output, Error> {
    tokio::time::timeout(budget, fut)
        .await
        .map_err(|_| Error::Timeout {
            stage: stage.to_string(),
            secs: budget.as_secs(),
        })
}

/// 给 `SftpOps` 套上预算与「超时即判死」的装饰器。
///
/// 一个 `TimedSftp` 对应一条 SFTP 通道。判死标志因此是**每通道**的：一条通道超时不该
/// 影响同一台服务器上另一条会话的通道。
pub struct TimedSftp {
    inner: Arc<dyn SftpOps>,
    control: Duration,
    data: Duration,
    /// 一旦置位就不再复位。没有「自动恢复」这种设计：让一条曾经失去响应的通道自己
    /// 声明自己又好了，等于把「我们不知道对端状态」这个前提悄悄换掉。恢复只有一条路径——
    /// 上层丢弃本对象、重开通道，那条路径上会有一次真实的 channel open 作为证据。
    poisoned: AtomicBool,
}

impl TimedSftp {
    /// 生产构造：控制面 [`CONTROL_TIMEOUT`]、数据面 [`DATA_TIMEOUT`]。
    pub fn new(inner: Arc<dyn SftpOps>) -> Self {
        Self::with_budgets(inner, CONTROL_TIMEOUT, DATA_TIMEOUT)
    }

    /// 自定义预算。存在的理由是测试要用极小的预算跑真实的超时路径，
    /// 而不是把 `timeout` 换成一个测试专用的假分支——那样被测的就不是生产代码了。
    pub fn with_budgets(inner: Arc<dyn SftpOps>, control: Duration, data: Duration) -> Self {
        Self {
            inner,
            control,
            data,
            poisoned: AtomicBool::new(false),
        }
    }

    /// 这条通道是否已被超时判死。上层据此丢弃缓存的通道并重建（见 `AppState::sftp_ops_for`）。
    pub fn is_poisoned(&self) -> bool {
        self.poisoned.load(Ordering::Acquire)
    }

    /// 预算 + 判死的唯一入口。所有 `SftpOps` 方法都必须经过它——
    /// 绕过它的方法就是一个没有上限的远程调用，而这正是本次要根除的东西。
    ///
    /// `op` / `path` 分开传而不是预先 `format!`：成功路径上一个字符串都不用建，
    /// 而 `write_at` 是每 256 KiB 就来一次的热路径。
    async fn guard<T, F>(
        &self,
        op: &'static str,
        path: &str,
        budget: Duration,
        fut: F,
    ) -> Result<T, Error>
    where
        F: Future<Output = Result<T, Error>>,
    {
        if self.is_poisoned() {
            return Err(Error::Poisoned(format!(
                "SFTP 通道已因先前的一次超时被判死，{op} {path} 未发出；请重连该会话后重试"
            )));
        }
        match tokio::time::timeout(budget, fut).await {
            Ok(r) => r,
            Err(_) => {
                self.poisoned.store(true, Ordering::Release);
                Err(Error::Timeout {
                    stage: format!("SFTP {op} {path}"),
                    secs: budget.as_secs(),
                })
            }
        }
    }
}

#[async_trait]
impl SftpOps for TimedSftp {
    async fn list(&self, path: &str) -> Result<ListResult, Error> {
        self.guard("list", path, self.data, self.inner.list(path))
            .await
    }
    async fn stat_size(&self, path: &str) -> Result<u64, Error> {
        self.guard("stat_size", path, self.control, self.inner.stat_size(path))
            .await
    }
    async fn stat_meta(&self, path: &str) -> Result<FileMeta, Error> {
        self.guard("stat_meta", path, self.control, self.inner.stat_meta(path))
            .await
    }
    async fn read_range(&self, path: &str, offset: u64, len: usize) -> Result<Vec<u8>, Error> {
        self.guard(
            "read_range",
            path,
            self.data,
            self.inner.read_range(path, offset, len),
        )
        .await
    }
    async fn write_at(&self, path: &str, offset: u64, data: &[u8]) -> Result<(), Error> {
        self.guard(
            "write_at",
            path,
            self.data,
            self.inner.write_at(path, offset, data),
        )
        .await
    }
    async fn truncate(&self, path: &str, size: u64) -> Result<(), Error> {
        self.guard(
            "truncate",
            path,
            self.control,
            self.inner.truncate(path, size),
        )
        .await
    }
    async fn sync(&self, path: &str) -> Result<(), Error> {
        self.guard("sync", path, self.control, self.inner.sync(path))
            .await
    }
    async fn mkdir(&self, path: &str) -> Result<(), Error> {
        self.guard("mkdir", path, self.control, self.inner.mkdir(path))
            .await
    }
    async fn remove(&self, path: &str) -> Result<(), Error> {
        self.guard("remove", path, self.control, self.inner.remove(path))
            .await
    }
    async fn remove_dir(&self, path: &str) -> Result<(), Error> {
        self.guard(
            "remove_dir",
            path,
            self.control,
            self.inner.remove_dir(path),
        )
        .await
    }
    async fn rename(&self, from: &str, to: &str) -> Result<(), Error> {
        self.guard("rename", from, self.control, self.inner.rename(from, to))
            .await
    }
    async fn lstat(&self, path: &str) -> Result<FileMeta, Error> {
        self.guard("lstat", path, self.control, self.inner.lstat(path))
            .await
    }
    async fn read_link(&self, path: &str) -> Result<String, Error> {
        self.guard("read_link", path, self.control, self.inner.read_link(path))
            .await
    }
    async fn symlink(&self, target: &str, link_path: &str) -> Result<(), Error> {
        self.guard(
            "symlink",
            link_path,
            self.control,
            self.inner.symlink(target, link_path),
        )
        .await
    }
    async fn canonicalize(&self, path: &str) -> Result<String, Error> {
        self.guard(
            "canonicalize",
            path,
            self.control,
            self.inner.canonicalize(path),
        )
        .await
    }
}

/// 给 `ExecChannel` 套上兜底预算（[`EXEC_HARD_TIMEOUT`]）。
///
/// 正常情况下先响的是 `verify::run_exec_channel` 内层那道 [`EXEC_TOTAL_TIMEOUT`]，它能带着
/// 部分输出正常返回；本层管的是内层看不见的两段——开通道与发 exec 请求——在那两段上挂死时，
/// 内层的循环压根还没开始跑。
///
/// 这里**不**判死：exec 是一次性通道，每次调用自开自关，一次超时不影响下一次；
/// 而且 exec 的失败在两个消费方（提交前闸门、传后校验）都只是「少一层证据」，
/// 把整条会话判死会让一次慢哈希连累到与校验无关的操作。
pub struct TimedExec {
    inner: Arc<dyn ExecChannel>,
    budget: Duration,
}

impl TimedExec {
    pub fn new(inner: Arc<dyn ExecChannel>) -> Self {
        Self {
            inner,
            budget: EXEC_HARD_TIMEOUT,
        }
    }
    pub fn with_budget(inner: Arc<dyn ExecChannel>, budget: Duration) -> Self {
        Self { inner, budget }
    }
}

#[async_trait]
impl ExecChannel for TimedExec {
    async fn exec_once(&self, cmd: &str) -> Result<crate::verify::ExecOutput, Error> {
        // 命令原文**不**进错误文案：exec 的命令行里可能带远端路径以外的东西，
        // 而超时文案会进日志（审计1 P1-20：日志不得含命令文本）。阶段名足以定位。
        with_timeout(
            "远端一次性命令（exec）",
            self.budget,
            self.inner.exec_once(cmd),
        )
        .await?
    }
}
