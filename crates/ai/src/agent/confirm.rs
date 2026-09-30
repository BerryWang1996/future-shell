//! 确认回路：票据、无 pending 变体的答案类型、以及全仓唯一的 `gate::resolve`
//! 调用点 [`conclude`]。

use crate::agent::record::RedactedAction;
use fs_policy::gate::{self, ConfirmOutcome};
use fs_policy::{Tier, CONFIRM_TIMEOUT};
use std::time::Duration;

#[derive(Debug)]
pub struct ConfirmTicket {
    /// 面板队列键，单调递增（app 层的确认队列按它定位应答）。
    pub id: u64,
    /// 面板/通知展示（构造即脱敏）。
    pub display: RedactedAction,
    pub tier: Tier,
    /// `StrongConfirm` ⇒ 长按/二次输入（§4.3 的手势成本）。
    pub strong: bool,
    /// `RULE_*` id 给面板。
    pub rules: Vec<&'static str>,
    /// `Reason.detail` 中文原句，直进对话框。
    pub details: Vec<String>,
    /// Agent 面 = [`CONFIRM_TIMEOUT`]（60s）。MCP 面 120s 由 MCP 自己的端口实现定
    /// （`is_expired_with` 参数化），不共用这个字段。
    pub deadline: Duration,
}

/// 端口返回面：**没有「还在等」变体**。
///
/// future 未完成就是未完成——调用方结构上不可能把「未决」误读成任何结论。
/// 这是 gate.rs 那个 `resolve(None, 未到点, true) == TimedOut` 坑的类型级封死
/// （行为级封死见 [`conclude`] 的 TimedOut 臂：只有到点才可构造）。
pub enum ConfirmAnswer {
    Answered(bool),
    TimedOut,
    /// 存活上下文消亡（会话掉线 / 窗口销毁 / 急停）。
    ContextGone,
}

pub enum AskAnswer {
    Answered(String),
    TimedOut,
    ContextGone,
}

/// `fs_policy::gate::resolve` 的全仓唯一调用点；仅终态事件可达。
///
/// 三臂各对应一个**已发生**的终态事件：
/// - `Answered(b)`——用户真的点了；
/// - `TimedOut`——**到点才可构造**（debug_assert 钉住）：把「还没人答」喂给
///   `resolve(None, 未到点, true)` 会得到假 TimedOut（resolve 的 None 兜底臂），
///   这正是它要防的误用；
/// - `ContextGone`——`session_alive=false` ⇒ Abandoned，与超时不混。
pub fn conclude(ans: ConfirmAnswer, elapsed: Duration) -> ConfirmOutcome {
    match ans {
        ConfirmAnswer::Answered(b) => gate::resolve(Some(b), elapsed, true),
        ConfirmAnswer::TimedOut => {
            debug_assert!(
                gate::is_expired_with(elapsed, CONFIRM_TIMEOUT),
                "TimedOut 只有到点才可构造；还没人答不是结论"
            );
            gate::resolve(None, elapsed, true) // → TimedOut
        }
        ConfirmAnswer::ContextGone => gate::resolve(None, elapsed, false), // → Abandoned
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// D1：四臂 vs resolve 的优先级。
    #[test]
    fn conclude_matches_the_resolve_precedence() {
        let s = Duration::from_secs(1);
        assert_eq!(
            conclude(ConfirmAnswer::Answered(true), s),
            ConfirmOutcome::Approved
        );
        assert_eq!(
            conclude(ConfirmAnswer::Answered(false), s),
            ConfirmOutcome::Rejected
        );
        // 到点（含 == 边界，与 gate.rs 既有口径一致）
        assert_eq!(
            conclude(ConfirmAnswer::TimedOut, CONFIRM_TIMEOUT),
            ConfirmOutcome::TimedOut
        );
        assert_eq!(
            conclude(ConfirmAnswer::ContextGone, s),
            ConfirmOutcome::Abandoned
        );
        // ContextGone 优先于「已到点」：会话都没了，超时不超时已无意义（resolve 的臂序）
        assert_eq!(
            conclude(ConfirmAnswer::ContextGone, CONFIRM_TIMEOUT * 2),
            ConfirmOutcome::Abandoned
        );
    }

    /// D3：`is_expired_with` 边界——`==` 为真（委托改造后既有 gate 测试也继续过）。
    #[test]
    fn is_expired_with_is_closed_at_the_boundary() {
        assert!(fs_policy::gate::is_expired_with(
            CONFIRM_TIMEOUT,
            CONFIRM_TIMEOUT
        ));
        assert!(!fs_policy::gate::is_expired_with(
            CONFIRM_TIMEOUT - Duration::from_millis(1),
            CONFIRM_TIMEOUT
        ));
        // MCP 面 120s 用同一个谓词
        let mcp = Duration::from_secs(120);
        assert!(fs_policy::gate::is_expired_with(mcp, mcp));
        assert!(!fs_policy::gate::is_expired_with(CONFIRM_TIMEOUT, mcp));
    }

    /// 类型级封死的 Compile-fail 面：ConfirmAnswer 没有「还在等」变体——
    /// 这条用穷举 match 证明（新增变体 ⇒ 本函数非穷举 ⇒ 编译红）。
    #[test]
    fn confirm_answer_has_no_pending_variant() {
        // 穷举三臂。若有人加第四个变体（比如 Pending），这里的 match 不再穷举，
        // 编译失败——「未决」被强行读成结论的那个口子从类型上焊死。
        fn exhaustive(a: &ConfirmAnswer) -> u8 {
            match a {
                ConfirmAnswer::Answered(_) => 1,
                ConfirmAnswer::TimedOut => 2,
                ConfirmAnswer::ContextGone => 3,
            }
        }
        assert_eq!(exhaustive(&ConfirmAnswer::Answered(true)), 1);
        assert_eq!(exhaustive(&ConfirmAnswer::TimedOut), 2);
        assert_eq!(exhaustive(&ConfirmAnswer::ContextGone), 3);
    }
}
