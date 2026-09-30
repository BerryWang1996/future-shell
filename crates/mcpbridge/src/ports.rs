//! 本 crate 与外界的全部接触面。
//!
//! 纯逻辑层（[`crate::gate`] / [`crate::authz`] / [`crate::confirm`] /
//! [`crate::payload`]）不认识 SSH、不认识数据库、不认识 GUI。它们只认识这里的
//! trait。好处不是「解耦」这种空话，而是**闸门可以在没有一台真实主机的情况下
//! 被完整地测出来**——`server.rs` 的行为测试全部跑在假实现上，包括那些
//! 「批准了但会话在这一瞬间没了」之类根本没法在真机上稳定复现的路径。
//!
//! 全部 trait 用手写 boxed future 而非 `async fn in trait`：后者不是 dyn 兼容的，
//! 而 `server.rs` 要拿着 `&dyn` 走。这与 `fs_ai::agent::ports` 同口径。

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use fs_ai::exec::Observation;

use crate::confirm::ConfirmTicket;
use crate::tool::ReadSource;

/// 端口的返回：要么拿到东西，要么一句给人看的失败原因。
///
/// 失败原因**不是** [`crate::reject::ToolError`]：那四个码表达的是「策略不让你做」，
/// 而端口失败表达的是「让你做了但没做成」（连接断了、路径不存在、权限不足）。
/// 把两者混为一谈会让审计里「被拦下」与「试过但失败」不可区分，
/// 而这两件事在事后追查时的意义完全相反。
pub type PortResult<T> = Result<T, String>;

type Fut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// 一个会话在 `sessions.list` 里的样子。
///
/// **没有任何凭据字段，也没有用户名。** 这不是「暂时没加」——`sessions.list`
/// 是本清单里唯一无条件只读、无条件可枚举的工具，它的返回值会原样进入外部
/// 模型的上下文。主机名已经是边界上的东西了，用户名与端口的组合再加上去就是
/// 一份现成的横向移动清单。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSummary {
    pub id: String,
    /// 用户给这个会话起的名字。
    pub name: String,
    /// 主机名（不含用户名与端口）。
    pub host: String,
    pub connected: bool,
}

/// 会话与连接配置的目录。
pub trait SessionDirectory: Send + Sync {
    fn list(&self) -> Fut<'_, Vec<SessionSummary>>;

    /// 这个会话此刻还在不在。
    ///
    /// **同步**是刻意的：它要在确认收敛的那一瞬间被问（见
    /// [`crate::confirm::conclude`] 的 `context_alive`），那个瞬间不该再有
    /// 一次可能被调度打断的 await——中间隔一次 await，问到的就是「刚才还在」。
    fn is_alive(&self, session_id: &str) -> bool;

    /// 这个 profile 允不允许被 MCP 打开（`fs_connmgr::AiPolicy::mcp_allowed`）。
    ///
    /// `None` = 查无此 profile。调用方（[`crate::authz::authorize_open`]）
    /// 对 `None` 与 `Some(false)` 给同一个回答。
    ///
    /// **异步**：真实现要查库（profile 在 SQLite 里）。同步签名会逼着实现
    /// 要么缓存一份会过期的许可表，要么在异步上下文里阻塞等库——前者是
    /// 「用户刚取消许可、MCP 仍能开」，后者是卡住事件循环，都不可接受。
    fn mcp_allowed(&self, profile_id: &str) -> Fut<'_, Option<bool>>;

    /// 打开一个新会话，成功则给出新会话 id。
    fn open(&self, profile_id: &str) -> Fut<'_, PortResult<String>>;
}

/// 终端网格的读写。
pub trait TerminalPort: Send + Sync {
    /// 读 headless 网格的**纯文本**（不含转义序列）。
    ///
    /// 脱敏**不在这一层做**——它在 `server.rs` 里统一做，那样只有一个
    /// 脱敏点可以被守卫钉住。端口若自己脱一遍，就会有人以为端口已经脱过了。
    fn read_text(
        &self,
        session_id: &str,
        source: ReadSource,
        max_lines: Option<usize>,
    ) -> Fut<'_, PortResult<String>>;

    /// 往终端写 payload（像用户敲进去一样）。
    fn send(&self, session_id: &str, payload: &str) -> Fut<'_, PortResult<()>>;
}

/// 命令执行。走 §4.4 `run_command` 契约（超时/截断/观察值），
/// **实现只有一份**（`fs_ai::exec::run_command`），由 app 层的端口转调。
pub trait CommandPort: Send + Sync {
    fn run(&self, session_id: &str, command: &str) -> Fut<'_, PortResult<Observation>>;
}

/// SFTP 三件事。
pub trait SftpPort: Send + Sync {
    fn list(&self, session_id: &str, path: &str) -> Fut<'_, PortResult<String>>;
    fn read(
        &self,
        session_id: &str,
        path: &str,
        max_bytes: Option<usize>,
    ) -> Fut<'_, PortResult<String>>;
    /// 内容内联。与 Agent 的 `sftp_put`（本地源 → 远端目的）不是同一个操作。
    fn write(&self, session_id: &str, path: &str, content: &str) -> Fut<'_, PortResult<()>>;
}

/// 用户在确认框上做了什么。
///
/// 三个字段一起交给 [`crate::confirm::conclude`]，**由它**决定结论——
/// 端口不下结论。端口若返回一个 `bool`，「没等到」与「答了否」就在类型上
/// 合并了，而它们对应两个不同的码。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmReply {
    /// `None` = 没等到回答（超时或被放弃）。
    pub answer: Option<bool>,
    /// 实际等了多久。进文案。
    pub elapsed: Duration,
    /// **收敛那一瞬间**目标上下文还在不在。
    pub context_alive: bool,
}

/// 把票送进 GUI 确认队列并阻塞等待。
pub trait McpConfirmPort: Send + Sync {
    /// 规格：「调用**阻塞等待** GUI 确认队列（入队 + 窗口闪烁/系统通知引导
    /// 用户到 FutureShell）」。实现负责在
    /// [`crate::confirm::MCP_CONFIRM_TIMEOUT`] 之后给出 `answer: None`。
    fn confirm(&self, ticket: ConfirmTicket) -> Fut<'_, ConfirmReply>;
}

/// 一次 MCP 调用的审计事实。
///
/// 字段与 `fs_connmgr::audit_repo::NewAuditEntry` 一一对得上，但**这里不引
/// connmgr**——审计落库是 app 层的事，本 crate 只负责说清「发生了什么」。
/// 由 app 层做那一步翻译，是为了让「digest 怎么算」「时间戳谁给」这类
/// 与存储绑死的决定留在存储那一层。
#[derive(Debug, Clone)]
pub struct McpCallRecord {
    /// 这一行是「打算做」还是「做完了」。见 [`RecordPhase`]。
    pub phase: RecordPhase,
    /// 线上工具名。
    pub tool_name: &'static str,
    /// 客户端标签。app 层据此拼 `mcp:{client}` 发起方。
    pub caller: String,
    /// 已脱敏的「做了什么」。类型保证，不是纪律保证。
    pub display: fs_ai::agent::record::RedactedAction,
    pub session_id: Option<String>,
    pub tier: fs_policy::Tier,
    pub verdict: McpVerdict,
    /// 命中的规则 id。
    pub rules: Vec<&'static str>,
    pub exit_code: Option<i32>,
    /// 输出。**app 层负责取摘要**（本 crate 没有 sha2，也不该有）。
    ///
    /// 类型是 [`RedactedAction`] 而不是 `String`，理由与 `display` 相同，
    /// 但这一处是**后补的**：第一版写成 `Option<String>`，于是端口失败那一支
    /// 的原文（里面有路径、命令、有时还有远端回吐的凭据）一路裸奔进审计行。
    /// 变异验证抓到的是「删掉一处脱敏测试照样绿」，顺着查才发现真洞在这里——
    /// 当时没有任何一条测试看过 `output`。
    ///
    /// 摘要取的是**脱敏后**的内容：审计链保证的是「我记的没被改过」，
    /// 而不是「我记了原文」；取证包导出时给人看的也该是脱过敏的。
    pub output: Option<fs_ai::agent::record::RedactedAction>,
}

/// 一条审计行处在这次调用的哪个阶段。
///
/// # 为什么要两阶段
///
/// 「先执行、审计失败就记个日志」等于在审计缺口期间放行了一次**无记录的执行**，
/// 而那正是事后最想查的那一次。所以要动手的调用写两行：
///
/// 1. [`Self::Intent`]——**执行之前**写。写不进去就不执行，调用直接失败。
///    这样「动作发生了却没有任何记录」在时序上不可能。
/// 2. [`Self::Settled`]——执行之后写，带 exit code 与输出。
///    这一行写不进去时动作已经发生了，调用方会收到一个明说「结果未记录」的
///    错误——这是两阶段唯一没能消掉的残余，但它至少是**可见**的：
///    链上有 Intent 没有 Settled，一眼就能看出来。
///
/// 不动手的调用（被拒/超时/放弃）只写一行 [`Self::Settled`]：没有动作，
/// 也就没有「执行了没记上」这个风险。
///
/// 先例：`app/src/commands/agent_cmd.rs` 的 RunBegin——写不进去就拒绝启动。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordPhase {
    /// 打算做，还没做。
    Intent,
    /// 已定局（做完了，或者根本没做成）。
    Settled,
}

impl RecordPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Intent => "intent",
            Self::Settled => "settled",
        }
    }
}

/// 一次调用的结局。
///
/// 七档，与 `fs_connmgr::audit_repo::Verdict` 一一对应——**刻意同构**：
/// 少一档就要在 app 层的映射里做一次「这个算哪一档」的判断，而那种判断
/// 只要做错一格，审计里就会出现一条读起来完全正常、意思却相反的行
/// （M3 批次③ 里真的出过一次：RunEnd 写成了 `none` 而不是 `request_only`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpVerdict {
    /// 闸门判自动放行，已执行。
    AutoRun,
    /// 用户批准，已执行。
    Approved,
    /// 用户拒绝。
    Rejected,
    /// 等确认超时。
    TimedOut,
    /// 上下文在等待期间消亡。
    Abandoned,
    /// 闸门或授权层判拒（没问过人）。
    Denied,
    /// 只是问了一下（`tools/list` 之类），没有执行动作。
    RequestOnly,
}

impl McpVerdict {
    /// 这一档意味着**动作真的发生了**。
    ///
    /// 事后追查时最要紧的一个问题就是「它到底跑了没有」，所以这个谓词
    /// 必须是枚举上的一个显式事实，而不是在某个查询里现拼的条件。
    pub fn executed(self) -> bool {
        matches!(self, Self::AutoRun | Self::Approved)
    }

    /// 稳定串。与 `fs_connmgr::audit_repo::Verdict::as_str` 逐字相同——
    /// app 层的映射有一条测试钉着两边不许分叉。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AutoRun => "auto_run",
            Self::Approved => "approved",
            Self::Rejected => "rejected",
            Self::TimedOut => "timed_out",
            Self::Abandoned => "abandoned",
            Self::Denied => "denied",
            Self::RequestOnly => "request_only",
        }
    }
}

/// 审计写入。
///
/// 返回 `Result` 而不是吞掉失败：审计写不进去时**调用必须停**。
/// 「先执行、审计失败就记个日志」等于在审计缺口期间放行了一次无记录的执行，
/// 而那正是事后最想查的那一次。
pub trait McpAuditPort: Send + Sync {
    fn append(&self, rec: McpCallRecord) -> Fut<'_, PortResult<()>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 七档与 connmgr 那边的稳定串逐字相同。
    ///
    /// 本 crate 不依赖 connmgr，所以这里写死对照表；app 层另有一条把两个
    /// 枚举真的对起来的映射测试。两处都要有：这里防串本身被改，
    /// 那里防映射连错格。
    #[test]
    fn the_seven_verdicts_use_the_stable_strings() {
        let all = [
            (McpVerdict::AutoRun, "auto_run", true),
            (McpVerdict::Approved, "approved", true),
            (McpVerdict::Rejected, "rejected", false),
            (McpVerdict::TimedOut, "timed_out", false),
            (McpVerdict::Abandoned, "abandoned", false),
            (McpVerdict::Denied, "denied", false),
            (McpVerdict::RequestOnly, "request_only", false),
        ];
        for (v, s, exec) in all {
            assert_eq!(v.as_str(), s);
            assert_eq!(v.executed(), exec, "{s} 的 executed 判断错了");
        }
        let uniq: std::collections::BTreeSet<_> = all.iter().map(|(v, _, _)| v.as_str()).collect();
        assert_eq!(uniq.len(), 7, "七个串必须互不相同");
        // 做空防护 + 反向：确实有档位是「跑了」的，也确实有档位不是。
        assert!(all.iter().any(|(_, _, e)| *e));
        assert!(all.iter().any(|(_, _, e)| !*e));
    }

    /// `SessionSummary` 里没有凭据、没有用户名、没有端口。
    ///
    /// 用源码扫描而不是靠人记得——`sessions.list` 的返回值会原样进入
    /// 外部模型的上下文，加一个 `username` 字段是那种「顺手补全一下」
    /// 的动作，不会有人专门为它写测试。
    #[test]
    fn a_session_summary_carries_nothing_that_helps_someone_move_laterally() {
        let src = include_str!("ports.rs");
        let start = src.find("pub struct SessionSummary").expect("找不到定义");
        let body = &src[start..start + src[start..].find('}').expect("找不到结尾")];
        for banned in [
            "username",
            "user:",
            "password",
            "port",
            "key",
            "token",
            "credential",
            "auth",
        ] {
            assert!(
                !body.to_lowercase().contains(banned),
                "SessionSummary 里出现了 {banned}"
            );
        }
        // 做空防护：确认扫的是真定义。
        assert!(body.contains("pub host"), "扫查坏了");
        assert!(body.contains("pub connected"));
    }
}
