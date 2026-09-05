//! MCP 确认契约（总设计 §4.5）。
//!
//! 规格原文：「⚠ 工具裁决为 `write`/`dangerous` 时，调用**阻塞等待** GUI
//! 确认队列……默认超时 120 s（可配）。……**stdio 传输下此契约是唯一的人机
//! 回路，不得静默放行。**」
//!
//! # 「不得静默放行」在这里的类型形式
//!
//! [`Conclusion`] 只有两支，且 `Proceed` 只能由 [`conclude`] 在
//! `ConfirmOutcome::Approved` 那一臂产出——没有 `Default`、没有 `pub` 构造函数、
//! 没有「待定」变体。想让一次未经回答的调用走到执行，得先给这个枚举加一支，
//! 而那是一次 review 里看得见的改动。
//!
//! # 60 秒与 120 秒是同一个谓词的两个上限
//!
//! `fs_policy::gate` 里的 `CONFIRM_TIMEOUT` 是 Agent 面的 60s；MCP 面是 120s。
//! 两边共用 `is_expired_with`（那个函数的文档已经点名了这件事）。差别有理由：
//! Agent 的确认框弹在用户正看着的窗口里，MCP 的调用方在另一个进程里，用户
//! 多半得先被通知、再切窗口过来。

use std::time::Duration;

use fs_ai::agent::record::RedactedAction;
use fs_policy::{gate, ConfirmOutcome, Decision, Tier};

use crate::gate::McpGated;
use crate::reject::ToolError;

/// 规格：「默认超时 120 s（可配）」。
pub const MCP_CONFIRM_TIMEOUT_SECS: u64 = 120;

/// 同上，`Duration` 形式。
pub const MCP_CONFIRM_TIMEOUT: Duration = Duration::from_secs(MCP_CONFIRM_TIMEOUT_SECS);

/// 等待是否已经超出 MCP 的确认预算。
///
/// 转调 `fs_policy::gate::is_expired_with`，**不自己写比较**：边界闭在哪一侧
/// （`>=` 还是 `>`）是个契约，两处各写一遍就会有一处写错，而这种错只在恰好
/// 卡在边界的那一次显形。
pub fn mcp_confirm_expired(elapsed: Duration) -> bool {
    gate::is_expired_with(elapsed, MCP_CONFIRM_TIMEOUT)
}

/// 一张送进 GUI 确认队列的票。
///
/// 人看到的两个字段都是 [`RedactedAction`]——**脱敏由类型保证，不由纪律保证**。
/// 这与 Agent 面同源：那边 `StepRecord.display` 用的是同一个 newtype，
/// app 层有一条 `every_audit_writer_routes_through_redact` 守着。
#[derive(Debug, Clone)]
pub struct ConfirmTicket {
    /// 线上工具名。
    pub tool_name: &'static str,
    /// 客户端标签。**展示用，非安全边界**（§4.5 信任边界）。
    pub caller: String,
    pub tier: Tier,
    /// 是否强确认（长按或二次输入）。由 [`Decision::StrongConfirm`] 决定，
    /// 不由调用方、也不由工具名决定。
    pub strong: bool,
    /// 「要做什么」的一行话。
    pub display: RedactedAction,
    /// 「为什么要问你」的逐条理由。
    pub details: Vec<RedactedAction>,
    /// 作用于哪个会话（`sessions.*` 没有）。
    pub session_id: Option<String>,
}

impl ConfirmTicket {
    /// 从裁决构造。**只有需要人的裁决才有票**。
    ///
    /// 返回 `Option` 而不是无条件造一张票，是为了让「给一个 `AutoRun` 发确认框」
    /// 与「给一个 `Deny` 发确认框」在类型上不可能——后者尤其要紧：那会弹出一个
    /// 批准了也照样失败的对话框，是在教用户对话框可以随便点。
    pub fn from_decision(
        d: Decision,
        g: &McpGated,
        raw_display: &str,
        session_id: Option<String>,
        known_secrets: &[String],
    ) -> Option<Self> {
        let strong = match d {
            Decision::Confirm => false,
            Decision::StrongConfirm => true,
            Decision::AutoRun | Decision::Deny(_) => return None,
        };
        Some(Self {
            tool_name: g.tool_name,
            caller: g.caller.clone(),
            tier: g.verdict.tier,
            strong,
            display: RedactedAction::new(raw_display, known_secrets),
            details: g
                .verdict
                .reasons
                .iter()
                .map(|r| RedactedAction::new(&r.detail, known_secrets))
                .collect(),
            session_id,
        })
    }
}

/// 确认的结论。**没有「待定」变体**——同 `fs_ai::agent::confirm::ConfirmAnswer`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Conclusion {
    /// 唯一的放行。
    Proceed,
    /// 一切非放行都随身带着要发回给调用方的结构化错误。
    ///
    /// 带着走而不是「返回 false 再由上层拼错误」：后者会让「拼哪个码」
    /// 变成一次上层的判断，而那正是 `ABORTED` 被写成 `TIMEOUT` 的方式。
    Refused(ToolError),
}

impl Conclusion {
    pub fn proceeds(&self) -> bool {
        matches!(self, Self::Proceed)
    }
}

/// 把一次确认的实际经过收敛成结论。**本 crate 唯一的 `gate::resolve` 调用点**
/// （由 `lib.rs` 的自守测试钉住）。
///
/// - `answer`：`None` = 没等到回答；
/// - `elapsed`：等了多久（只进文案，不参与判定——判定在 `resolve` 里）；
/// - `context_alive`：目标会话/窗口还在不在。
///
/// 四个出口对四个码，一一对应，不合并：
/// `Approved`→放行、`Rejected`→DENIED、`TimedOut`→TIMEOUT、`Abandoned`→**ABORTED**。
/// 最后那条是规格明令的（不得映射为 TIMEOUT，见 [`crate::reject`] 模块头）。
pub fn conclude(
    answer: Option<bool>,
    elapsed: Duration,
    context_alive: bool,
    tier: Tier,
) -> Conclusion {
    match gate::resolve(answer, elapsed, context_alive) {
        ConfirmOutcome::Approved => Conclusion::Proceed,
        ConfirmOutcome::Rejected => Conclusion::Refused(ToolError::denied_by_user(tier)),
        ConfirmOutcome::TimedOut => {
            Conclusion::Refused(ToolError::timed_out(tier, MCP_CONFIRM_TIMEOUT_SECS))
        }
        ConfirmOutcome::Abandoned => Conclusion::Refused(ToolError::aborted(tier)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gate::classify_for_mcp;
    use crate::tool::McpTool;

    fn gated(cmd: &str) -> McpGated {
        classify_for_mcp(
            &McpTool::CommandRun {
                session_id: "s".into(),
                command: cmd.into(),
            },
            "claude-desktop",
        )
    }

    /// 规格给的就是 120 秒。改它要连着改规格。
    #[test]
    fn the_mcp_budget_is_two_minutes_and_differs_from_the_agent_one() {
        assert_eq!(MCP_CONFIRM_TIMEOUT_SECS, 120);
        assert_eq!(MCP_CONFIRM_TIMEOUT, Duration::from_secs(120));
        assert_ne!(
            MCP_CONFIRM_TIMEOUT,
            fs_policy::CONFIRM_TIMEOUT,
            "两个面用的是同一个谓词、两个上限；相等说明有人把其中一个改没了"
        );
        assert!(MCP_CONFIRM_TIMEOUT > fs_policy::CONFIRM_TIMEOUT);
    }

    /// 边界闭在哪一侧是契约：恰好 120 秒算过期。
    #[test]
    fn the_deadline_is_closed_on_the_expired_side() {
        assert!(!mcp_confirm_expired(Duration::from_secs(119)));
        assert!(mcp_confirm_expired(MCP_CONFIRM_TIMEOUT));
        assert!(mcp_confirm_expired(Duration::from_secs(121)));
        // 与 Agent 的 60s 上限确实是两个上限：119 秒在 Agent 面早就过期了。
        assert!(fs_policy::gate::is_expired(Duration::from_secs(119)));
    }

    /// 四个出口对四个码，一一对应。**这条是本文件的核心。**
    #[test]
    fn every_outcome_maps_to_its_own_code_and_only_approval_proceeds() {
        let t = Tier::Dangerous;
        let d = Duration::from_secs(5);

        assert_eq!(conclude(Some(true), d, true, t), Conclusion::Proceed);

        let cases = [
            (Some(false), true, "FS_POLICY_DENIED", "用户答否"),
            (None, true, "FS_POLICY_TIMEOUT", "没等到回答"),
            (None, false, "FS_POLICY_ABORTED", "上下文消亡"),
            (Some(true), false, "FS_POLICY_ABORTED", "批准了但会话没了"),
        ];
        for (ans, alive, want, name) in cases {
            match conclude(ans, d, alive, t) {
                Conclusion::Proceed => panic!("{name} 不该放行"),
                Conclusion::Refused(e) => assert_eq!(e.code, want, "{name}"),
            }
        }
        // 做空防护 + 四个码各出现过一次（含放行那条共五个出口）。
        assert_eq!(cases.len(), 4);
    }

    /// 上下文消亡**优先于一切，包括已经拿到的批准**。
    ///
    /// 这条单拎出来是因为它反直觉：用户明明点了「同意」。但那一刻之后会话
    /// 就没了，此时执行意味着往一条不存在的连接上发命令，或者更糟——往一条
    /// 重连后**指向别处**的连接上发。
    #[test]
    fn a_dead_context_outranks_an_approval_that_already_arrived() {
        let c = conclude(Some(true), Duration::ZERO, false, Tier::Write);
        assert!(!c.proceeds());
        match c {
            Conclusion::Refused(e) => {
                assert_eq!(e.code, "FS_POLICY_ABORTED");
                assert_ne!(e.code, "FS_POLICY_DENIED", "不是用户拒绝，别这么记");
            }
            _ => unreachable!(),
        }
    }

    /// 超时文案里要出现的是 **120**，不是 fs_policy 的 60。
    #[test]
    fn the_timeout_message_quotes_the_mcp_budget_not_the_agent_one() {
        match conclude(None, Duration::from_secs(200), true, Tier::Write) {
            Conclusion::Refused(e) => {
                assert!(e.message.contains("120"), "文案：{}", e.message);
                assert!(
                    !e.message.contains("60"),
                    "串了 Agent 面的上限：{}",
                    e.message
                );
            }
            _ => unreachable!(),
        }
    }

    /// **不需要人的裁决没有票。** 尤其是 `Deny`：给它发确认框，等于弹出一个
    /// 批准了也照样失败的对话框，那是在教用户对话框可以随便点。
    #[test]
    fn only_a_decision_that_needs_a_human_produces_a_ticket() {
        let g = gated("ls");
        let mk = |d| ConfirmTicket::from_decision(d, &g, "x", None, &[]);
        assert!(mk(Decision::AutoRun).is_none());
        assert!(mk(Decision::Deny(fs_policy::DenyReason::GloballyDisabled)).is_none());
        assert!(mk(Decision::Deny(
            fs_policy::DenyReason::ModeAllowsReadOnlyOnly
        ))
        .is_none());
        // 反向对照：需要人的两个裁决都要有票，否则把这函数写成恒 None 也全绿。
        assert!(mk(Decision::Confirm).is_some());
        assert!(mk(Decision::StrongConfirm).is_some());
    }

    /// 强确认只由裁决决定。
    ///
    /// 同一个危险级 `gated` 在两种裁决下分别得到 weak/strong 票——说明
    /// `strong` 没有偷看 tier 自己下结论。真让它偷看的话，会话级覆盖
    /// （`ai_policy`）想把某个危险操作降成单次确认就没地方生效了。
    #[test]
    fn strong_comes_from_the_decision_and_nothing_else() {
        let g = gated("rm -rf /");
        let weak = ConfirmTicket::from_decision(Decision::Confirm, &g, "x", None, &[]).unwrap();
        let strong =
            ConfirmTicket::from_decision(Decision::StrongConfirm, &g, "x", None, &[]).unwrap();
        assert!(!weak.strong);
        assert!(strong.strong);
        assert_eq!(weak.tier, strong.tier);
        assert_eq!(weak.tier, Tier::Dangerous);
    }

    /// 票上给人看的字段**全部**经过脱敏，而不是「主要那个」经过。
    ///
    /// details 同样要脱：它们由 fs_policy 从同一段命令文本生成，命令里的密码
    /// 会原样出现在理由里。只脱 display 的话，密码从第二个字段照样上屏。
    #[test]
    fn every_human_visible_field_on_the_ticket_is_redacted() {
        let secret = "hunter2SuperSecretValue";
        let cmd = format!("mysql -p{secret} -e 'drop database x'");
        let g = classify_for_mcp(
            &McpTool::CommandRun {
                session_id: "s".into(),
                command: cmd.clone(),
            },
            "c",
        );
        let t = ConfirmTicket::from_decision(
            Decision::StrongConfirm,
            &g,
            &cmd,
            Some("s".into()),
            &[secret.to_string()],
        )
        .unwrap();
        assert!(!t.display.as_str().contains(secret), "display 泄漏了");
        for d in &t.details {
            assert!(
                !d.as_str().contains(secret),
                "details 泄漏了：{}",
                d.as_str()
            );
        }
        // 做空防护：这条命令确实产生了理由，否则上面的循环一次都不跑。
        assert!(!t.details.is_empty(), "这条命令应当有分类理由");
        // 且脱敏真的发生了（而不是因为命令里根本没这个串）。
        assert!(cmd.contains(secret));
        assert!(
            t.display.as_str().contains("redacted"),
            "没看到打码痕迹：{}",
            t.display.as_str()
        );
    }

    /// 票如实带走这次调用的身份——审计与 UI 都要它们。
    #[test]
    fn the_ticket_carries_the_identity_of_the_call() {
        let g = gated("rm -rf /");
        let t = ConfirmTicket::from_decision(
            Decision::StrongConfirm,
            &g,
            "command.run: rm -rf /",
            Some("sess-1".into()),
            &[],
        )
        .unwrap();
        assert_eq!(t.tool_name, "command.run");
        assert_eq!(t.caller, "claude-desktop");
        assert_eq!(t.session_id.as_deref(), Some("sess-1"));
        assert_eq!(t.tier, Tier::Dangerous);
    }
}
