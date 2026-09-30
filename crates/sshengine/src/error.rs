use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("ssh: {0}")]
    Ssh(String),
    #[error("connect: {0}")]
    Connect(String),
    #[error("auth failed; tried: {tried:?}; server allows: {remaining:?}; notes: {notes:?}")]
    Auth {
        tried: Vec<String>,
        remaining: Vec<String>,
        /// 本地原因导致某方法**未能真正发起**时的诊断（S38：如 agent 三管道均不可用、
        /// agent 里一把可用身份都没有）。单列一栏而不并进 `remaining`：那一栏的语义是
        /// 「服务器还允许什么」，混入本地故障会把排查方向直接引偏到服务端。
        notes: Vec<String>,
    },
    #[error("host key: {0}")]
    HostKey(String),
    #[error("sftp: {0}")]
    Sftp(String),
    #[error("transfer: {0}")]
    Transfer(String),
    /// 远程操作在预算内没有回音（审计2 #11/#25）。
    ///
    /// 单列一个变体而不是塞进 `Sftp(String)`/`Ssh(String)`，是因为超时与「对端明确报错」
    /// 在**善后动作**上完全不同：后者说明连接是活的、这一次操作被拒绝，重试或换个路径即可；
    /// 前者说明我们已经不知道对端处于什么状态——那条请求可能还在服务端排队、也可能已经生效，
    /// 于是唯一安全的处置是放弃这条通道。调用方要能靠类型而不是靠 `msg.contains("超时")`
    /// 做出这个区分（[`Error::is_timeout`]），字符串匹配一改文案就悄悄失效。
    #[error("{stage} 超时（上限 {secs} 秒）")]
    Timeout { stage: String, secs: u64 },
    /// 该通道已被先前的一次超时判死，后续调用一律就地失败（见 `timeouts::TimedSftp`）。
    #[error("{0}")]
    Poisoned(String),
    /// 凭据来源（保险库）**锁着**，记录取不到（2026-08-31）。
    ///
    /// 单列一个变体而不是并进 `Auth.notes`，是因为两者的**处置**完全不同：
    /// notes 是终态诊断（用户只能去改配置再重连），而「锁着」是一个**可降级**
    /// 的状态——档案绑的记录这次取不到，但服务器若通告 password，弹框问一次
    /// 本次口令照样能连（与 RDP 侧 2026-08-28 的三级回落同口径）。靠字符串
    /// `notes.contains("vault 未解锁")` 做这个分支，改一句文案就悄悄失效
    /// （与 [`Error::Timeout`] 单列的同一条理由）。
    #[error("凭据库未解锁")]
    SecretSourceLocked,
    /// 审计2 #37：known_hosts 导入的规模闸被触发（文件超大/行数超限）。
    /// 与 `HostKey` 分开：那是运行时验证失败，这是导入文件本身不被接受——
    /// 调用方应把文案直出给用户（改小文件再导一次），而不是当成信任判断故障去排查。
    #[error("{0}")]
    Import(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("storage: {0}")]
    Storage(#[from] fs_connmgr::Error),
    // hostkey.rs 直连 sqlx（lookup/record/import 的裸 `?` 需要此 From；
    // From 不具传递性，Storage 变体无法替代）
    #[error("sqlite: {0}")]
    Sqlx(#[from] sqlx::Error),
}

impl Error {
    /// 「对端没在预算内回话」——含由此判死的后续调用。
    ///
    /// 两个变体合并成一个判据，是因为调用方关心的是同一件事：这条通道还能不能继续用。
    /// `Poisoned` 只是同一次超时在时间上的延续，不是另一类故障。
    pub fn is_timeout(&self) -> bool {
        matches!(self, Error::Timeout { .. } | Error::Poisoned(_))
    }
}
