//! 「为什么停」只有一份真相。
//!
//! `RunReport`、UI 终态事件、RunEnd 审计行三方消费同一个值，不可能各说各话。
//! 每个变体可被测试直接构造、独立断言（实现规格 §4）。

use crate::agent::budget::TokensSpent;
use fs_policy::DenyReason;

#[derive(Debug)]
pub enum StopReason {
    /// 模型调了 finish（或纯文本无工具调用的回合）：收敛唯一正路。
    Finished { summary: String },
    /// 步数（模型回合）耗尽。`taken == limit` 恰好成立（先查后记：停下的那一下不记账）。
    StepsExhausted { taken: u32, limit: u32 },
    /// 连续无人参与动作触顶。第 limit+1 条**不执行**，审计留 RequestOnly 行。
    ConsecutiveAutoExhausted { streak: u32, limit: u32 },
    /// token 触顶（强预检或 settle 后闩上）。三本账都在，汇报不假装知道精确数。
    TokensExhausted { spent: TokensSpent, limit: u64 },
    /// 决策表 Deny：档位不允许该分级（拒绝即停，出口 1 为准）。
    PolicyDenied {
        reason: DenyReason,
        message: String,
        /// `fs_policy::rules::RULE_*` 稳定 id，给机器。
        rules: Vec<&'static str>,
        /// `Reason.detail` 中文原句 + `DenyReason::message()`，给人。
        details: Vec<String>,
    },
    /// 用户在确认对话框点了拒绝。
    UserRejected,
    /// 确认 60s（`CONFIRM_TIMEOUT`，含 `==` 边界）无人应答。
    ConfirmTimedOut,
    /// 确认票据的存活上下文消亡（会话掉线），resolve → Abandoned。
    SessionAbandoned,
    /// ask_user 60s 无人回答。
    AskUnanswered,
    /// 急停。`killed_in_flight` = 被掐断命令的部分 Observation
    /// （`timed_out: true`、`exit_code: None`——不猜值）。
    ///
    /// 「是人按的停」由本变体携带，与「命令自己超时」不混：两者在 `Observation`
    /// 里长得一样（都是 timed_out=true），区别只在这里。
    UserAbort {
        killed_in_flight: Option<crate::exec::Observation>,
    },
    /// 重试耗尽后 provider 仍失败。只放 `user_message()`——
    /// detail 可能含密钥/屏幕碎片，不进报告。
    ProviderFatal { user_message: String, attempts: u8 },
    /// 审计链写入失败：自主步不允许无记录地继续（宁停不留下链上空洞）。
    ///
    /// 刻意与 `ai_cmd::audit_ai` 的 warn-only 不同：一次性建议失败可以放过，
    /// **自主步无记录地继续是链上缺口**。代价是磁盘满/库锁会让 run 频繁中止——
    /// 这是选 custody 弃可用性的显式交换。
    AuditWriteFailed { detail: String },
}

impl StopReason {
    /// UI/RunEnd 行共用的一句话。带预算数字——「为什么停」不带上数字，
    /// 用户的第一反应是「坏了」而不是「到量了」。
    pub fn summary_line(&self) -> String {
        match self {
            Self::Finished { summary } => format!("完成：{summary}"),
            Self::StepsExhausted { taken, limit } => {
                format!("步数预算耗尽（{taken}/{limit} 个模型回合）")
            }
            Self::ConsecutiveAutoExhausted { streak, limit } => format!(
                "连续自动命令数触顶（{streak}/{limit}，连续 {limit} 条无人参与的动作后停止）"
            ),
            Self::TokensExhausted { spent, limit } => format!(
                "token 预算触顶（已知花 {} + {} 回合无账（按上界共 {}），上限 {limit}）",
                spent.known(),
                spent.unaccounted_turns(),
                spent.conservative_total()
            ),
            Self::PolicyDenied { message, .. } => format!("策略闸门拒绝：{message}"),
            Self::UserRejected => "你在确认框里拒绝了这条命令，任务停止".to_string(),
            Self::ConfirmTimedOut => "确认等待 60 秒无人应答，任务停止".to_string(),
            Self::SessionAbandoned => "会话已断开，未决的确认被放弃，任务停止".to_string(),
            Self::AskUnanswered => "提问 60 秒无人回答，任务停止".to_string(),
            Self::UserAbort { .. } => "你按了急停，任务停止".to_string(),
            Self::ProviderFatal { user_message, .. } => {
                format!("模型调用失败：{user_message}")
            }
            Self::AuditWriteFailed { .. } => {
                "审计记录写入失败，任务停止（自主步不允许无记录地继续）".to_string()
            }
        }
    }
}

/// 启动期拒绝（run 尚未存在，不进 StopReason）。
///
/// 「未加载」与「已禁用」是两件事（铁律 5 的启动期版）：前者是时序问题
/// （设置还没读到），后者是用户决定。UI 必须分开展示——把前者报成后者，
/// 用户会以为自己被禁用了。
#[derive(Debug)]
pub enum AgentStartError {
    /// 全局 `ai.mode` 还没读到。
    ModeNotLoaded,
    /// effective = Disabled：拒绝启动（不浪费一次已付费的模型回合）。
    ModeDisabled,
    /// `ProviderState::Unconfigured`——无假数据，显式臂（出口第 7 项的精神）。
    NoProvider,
}

impl AgentStartError {
    pub fn message(&self) -> &'static str {
        match self {
            Self::ModeNotLoaded => "AI 档位还没加载完，请稍候重试",
            Self::ModeDisabled => "AI 执行总开关是关着的（设置 → AI）",
            Self::NoProvider => "还没配置模型源（AI 助手面板 → 配置）",
        }
    }
}
