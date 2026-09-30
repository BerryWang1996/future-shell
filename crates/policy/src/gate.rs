//! 裁决闸门：把「这条命令多危险」翻译成「要不要执行、要不要问」（总设计 §4.3「默认裁决」）。
//!
//! # 为什么分级与裁决要分成两层
//!
//! [`crate::Tier`] 回答的是关于**命令**的事实：它有多危险。这个答案与用户的设置无关，
//! 也不该随设置变——同一条 `rm -rf /` 在任何档位下都是 `Dangerous`。
//!
//! 本模块回答的是关于**这次执行**的策略：在当前档位与总开关状态下，该自动跑、该问一次、
//! 该强确认、还是该直接拒绝。
//!
//! 混在一起写会出现一类很难发现的错误：为了让某个档位「放行得多一点」而去调低分级，
//! 于是审计记录里那条命令的风险等级也跟着变低了——事后追查时看到的是一条
//! 「write 级操作」，而它其实是 `rm -rf /`。分两层之后，分级永远记录事实，
//! 放宽只发生在裁决层且有迹可循。

use crate::Tier;
use std::time::Duration;

/// 确认请求的存活时长（M2 出口原文：「拒绝/超时（60s）不落执行」）。
///
/// 超时**等同于拒绝**，不等同于批准。这个方向是不能反的：一个没人看的确认框
/// 在 60 秒后自己变成「同意」，等于把「需要人确认」这件事悄悄取消掉——
/// 而最需要确认的场景（用户离开了座位）恰恰就是没人看的那种。
pub const CONFIRM_TIMEOUT: Duration = Duration::from_secs(60);

/// AI 执行档位。
///
/// 顺序即严格程度：`Disabled` 最严，`WithConfirm` 最松。
/// [`Ord`] 的方向刻意让「更严」= 更小，这样「取更严者」就是 [`Ord::min`]。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AiMode {
    /// 全局总开关关闭（总设计 §4.3 末句「全局总开关可一键禁止一切 AI 执行」）。
    ///
    /// 这一档下**连确认框都不弹**。弹了就意味着「总开关关着，但只要点一下就能跑」,
    /// 那不是开关，是提示。
    Disabled,
    /// 只允许只读：`write` 与 `dangerous` 一律拒绝，且**不给确认机会**。
    ///
    /// 与 `WithConfirm` 的区别不是「问得更凶」而是「不问」。用户选这一档的意思是
    /// 「我不想在 AI 面前做决定」，那么弹一个可以点「同意」的框就违背了这个选择。
    ReadOnlyOnly,
    /// 默认档：只读自动放行、write 单次确认、dangerous 强确认。
    WithConfirm,
}

impl AiMode {
    /// 稳定串（settings 键值与审计行用；**永不改名**）。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::ReadOnlyOnly => "read_only",
            Self::WithConfirm => "with_confirm",
        }
    }

    pub fn from_str_exact(s: &str) -> Option<Self> {
        match s {
            "disabled" => Some(Self::Disabled),
            "read_only" => Some(Self::ReadOnlyOnly),
            "with_confirm" => Some(Self::WithConfirm),
            _ => None,
        }
    }
}

impl std::fmt::Display for AiMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 对这一次执行的裁决。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// 直接执行，不打扰用户。
    AutoRun,
    /// 单次确认（一个按钮）。
    Confirm,
    /// 强确认（长按或二次输入——§4.3「强确认」）。
    ///
    /// 与 `Confirm` 分开的意义全在**手势成本**上：单次确认可以被肌肉记忆点掉，
    /// 强确认不能。如果两者的交互一样，那么区分 write 与 dangerous 就没有任何作用。
    StrongConfirm,
    /// 拒绝，且不提供「同意」这个选项。
    Deny(DenyReason),
}

/// 拒绝的原因。做成枚举而不是字符串，是为了让 UI 能对不同原因给不同的下一步指引
/// （总开关关着 → 指向设置；档位限制 → 指向档位说明）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DenyReason {
    /// AI 执行总开关关闭。
    GloballyDisabled,
    /// 当前档位只允许只读操作。
    ModeAllowsReadOnlyOnly,
}

impl DenyReason {
    /// 给用户看的一句话，含下一步该去哪里。
    ///
    /// 「被拒绝」而不说清怎么改，用户的下一步就是关掉整个功能。
    pub fn message(self) -> &'static str {
        match self {
            Self::GloballyDisabled => {
                "AI 执行总开关处于关闭状态。要启用请到 设置 → AI 打开总开关。"
            }
            Self::ModeAllowsReadOnlyOnly => {
                "当前会话的 AI 档位是「仅只读」，写入类与危险类操作一律不执行、也不询问。\
                 要执行它请先把档位调整为「需确认」。"
            }
        }
    }
}

impl Decision {
    /// 这次裁决最终会不会真的去执行。
    ///
    /// 注意 `Confirm`/`StrongConfirm` 都返回 `false`——它们只是**可能**执行，
    /// 取决于用户答不答应。把它们当成「会执行」是一类典型的实现错误：
    /// 那样一来审计会在用户还没点之前就记下「已执行」。
    pub fn runs_immediately(self) -> bool {
        matches!(self, Self::AutoRun)
    }

    /// 这次裁决是否需要人来答一句。
    pub fn needs_user(self) -> bool {
        matches!(self, Self::Confirm | Self::StrongConfirm)
    }
}

/// 核心裁决（总设计 §4.3「默认裁决」）。
///
/// 纯函数，只看两个输入。刻意不接受「命令文本」——分级已经在 [`crate::classify`] 做完了，
/// 这里再看一眼文本就意味着有第二处判定逻辑，而两处判定迟早会不一致。
pub fn decide(tier: Tier, mode: AiMode) -> Decision {
    match mode {
        // 总开关优先于一切，包括只读。用户关掉它的意思是「AI 什么都别跑」，
        // 而不是「AI 可以跑安全的那些」。
        AiMode::Disabled => Decision::Deny(DenyReason::GloballyDisabled),
        AiMode::ReadOnlyOnly => match tier {
            Tier::ReadOnly => Decision::AutoRun,
            Tier::Write | Tier::Dangerous => Decision::Deny(DenyReason::ModeAllowsReadOnlyOnly),
        },
        AiMode::WithConfirm => match tier {
            Tier::ReadOnly => Decision::AutoRun,
            Tier::Write => Decision::Confirm,
            Tier::Dangerous => Decision::StrongConfirm,
        },
    }
}

/// 会话级档位覆盖（总设计 §4.3：「可按会话经 `ai_policy` 覆盖，**仅可向严调整**」）。
///
/// 返回两者中更严的那个。**这个函数不会放宽任何东西**——这是它存在的全部理由：
/// 如果会话级设置能把档位往松调，那么一次提示注入（让模型「建议」调整档位）
/// 就能把总开关绕过去。
pub fn apply_session_override(global: AiMode, session_request: AiMode) -> AiMode {
    // Ord 的方向是「更严 = 更小」，所以取更严者就是 min
    global.min(session_request)
}

/// 用户对一次确认请求的应答。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmOutcome {
    /// 明确同意。
    Approved,
    /// 明确拒绝。
    Rejected,
    /// [`CONFIRM_TIMEOUT`] 内没有应答。
    TimedOut,
    /// 会话在等待期间关闭了（标签被关、连接断了）。
    Abandoned,
}

impl ConfirmOutcome {
    /// 这个应答是否放行执行。
    ///
    /// **只有明确同意才算。** 超时、被放弃都算不放行——M2 出口原文
    /// 「拒绝/超时（60s）不落执行」。
    pub fn allows_execution(self) -> bool {
        matches!(self, Self::Approved)
    }

    /// 落审计时用的稳定串。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Approved => "approved",
            Self::Rejected => "rejected",
            Self::TimedOut => "timed_out",
            Self::Abandoned => "abandoned",
        }
    }
}

/// 一次确认请求是否已经超时。
///
/// 单独一个纯函数而不是内嵌 `Instant::now()`：调用方传入已经过去的时长，
/// 于是这段判定可以被测试。内嵌当前时间的实现只能靠 sleep 来测，
/// 而那样的测试要么慢要么不稳。
pub fn is_expired(elapsed: Duration) -> bool {
    is_expired_with(elapsed, CONFIRM_TIMEOUT)
}

/// [`is_expired`] 的参数化版（M3 Agent 引入）：等待上限由调用方给。
///
/// Agent 面的确认 60s 用 [`CONFIRM_TIMEOUT`]；MCP 面（§4.5）是 120s——同一个
/// 过期谓词、两个上限，不该为 120s 再写一份 `>=`（两份 `>=` 迟到早晚会有一份
/// 被单独改成 `>`）。
pub fn is_expired_with(elapsed: Duration, limit: Duration) -> bool {
    elapsed >= limit
}

/// 把「等待结果」收敛成最终应答。
///
/// `answer` 为 `None` 表示用户还没答；此时若已超时即 [`ConfirmOutcome::TimedOut`]。
pub fn resolve(answer: Option<bool>, elapsed: Duration, session_alive: bool) -> ConfirmOutcome {
    if !session_alive {
        return ConfirmOutcome::Abandoned;
    }
    match answer {
        Some(true) => ConfirmOutcome::Approved,
        Some(false) => ConfirmOutcome::Rejected,
        None if is_expired(elapsed) => ConfirmOutcome::TimedOut,
        // 还在等——调用方不该把这个当成结论，但函数必须有返回值。
        // 给 TimedOut 而不是 Approved：任何「还没定」的状态都不能导向执行。
        None => ConfirmOutcome::TimedOut,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_mode_follows_the_spec_table() {
        // §4.3「默认裁决」：read_only 自动放行；write 单次确认；dangerous 强确认
        assert_eq!(
            decide(Tier::ReadOnly, AiMode::WithConfirm),
            Decision::AutoRun
        );
        assert_eq!(decide(Tier::Write, AiMode::WithConfirm), Decision::Confirm);
        assert_eq!(
            decide(Tier::Dangerous, AiMode::WithConfirm),
            Decision::StrongConfirm
        );
    }

    #[test]
    fn read_only_mode_denies_rather_than_asks() {
        // M2 出口：「read_only 档位自动放行读命令、写命令仍拒绝」。
        // 关键是**拒绝**而不是「问得更凶」——用户选这一档的意思是不想做这个决定，
        // 弹一个能点同意的框就违背了这个选择。
        assert_eq!(
            decide(Tier::ReadOnly, AiMode::ReadOnlyOnly),
            Decision::AutoRun
        );
        assert_eq!(
            decide(Tier::Write, AiMode::ReadOnlyOnly),
            Decision::Deny(DenyReason::ModeAllowsReadOnlyOnly)
        );
        assert_eq!(
            decide(Tier::Dangerous, AiMode::ReadOnlyOnly),
            Decision::Deny(DenyReason::ModeAllowsReadOnlyOnly)
        );
        // 反向对照：这两条不得是「需要确认」
        for t in [Tier::Write, Tier::Dangerous] {
            assert!(!decide(t, AiMode::ReadOnlyOnly).needs_user());
        }
    }

    #[test]
    fn the_global_switch_outranks_everything_including_read_only() {
        // 总开关关着时连只读也不跑：用户的意思是「AI 什么都别跑」，
        // 不是「可以跑安全的那些」。
        for t in [Tier::ReadOnly, Tier::Write, Tier::Dangerous] {
            assert_eq!(
                decide(t, AiMode::Disabled),
                Decision::Deny(DenyReason::GloballyDisabled),
                "{t} 在总开关关闭时也必须拒绝"
            );
            assert!(!decide(t, AiMode::Disabled).runs_immediately());
            assert!(
                !decide(t, AiMode::Disabled).needs_user(),
                "关着还弹框，那不是开关"
            );
        }
    }

    #[test]
    fn only_auto_run_executes_immediately() {
        // Confirm/StrongConfirm 都还没执行。把它们当成「会执行」会让审计
        // 在用户点之前就记下「已执行」。
        assert!(Decision::AutoRun.runs_immediately());
        assert!(!Decision::Confirm.runs_immediately());
        assert!(!Decision::StrongConfirm.runs_immediately());
        assert!(!Decision::Deny(DenyReason::GloballyDisabled).runs_immediately());
    }

    #[test]
    fn session_override_can_only_tighten() {
        // 收紧：采纳
        assert_eq!(
            apply_session_override(AiMode::WithConfirm, AiMode::ReadOnlyOnly),
            AiMode::ReadOnlyOnly
        );
        assert_eq!(
            apply_session_override(AiMode::WithConfirm, AiMode::Disabled),
            AiMode::Disabled
        );
        // 放宽：忽略。这条是整个覆盖机制的前提——能放宽就意味着
        // 一次提示注入（让模型「建议」把档位调松）可以绕过总开关。
        assert_eq!(
            apply_session_override(AiMode::ReadOnlyOnly, AiMode::WithConfirm),
            AiMode::ReadOnlyOnly
        );
        assert_eq!(
            apply_session_override(AiMode::Disabled, AiMode::WithConfirm),
            AiMode::Disabled
        );
        assert_eq!(
            apply_session_override(AiMode::Disabled, AiMode::ReadOnlyOnly),
            AiMode::Disabled
        );
    }

    #[test]
    fn mode_ordering_is_strictness_order() {
        // apply_session_override 用 min 取更严者，所以这个方向必须成立
        assert!(AiMode::Disabled < AiMode::ReadOnlyOnly);
        assert!(AiMode::ReadOnlyOnly < AiMode::WithConfirm);
    }

    #[test]
    fn timeout_and_rejection_both_block_execution() {
        // M2 出口原文：「拒绝/超时（60s）不落执行」
        assert!(ConfirmOutcome::Approved.allows_execution());
        assert!(!ConfirmOutcome::Rejected.allows_execution());
        assert!(!ConfirmOutcome::TimedOut.allows_execution());
        assert!(!ConfirmOutcome::Abandoned.allows_execution());
    }

    #[test]
    fn the_timeout_is_sixty_seconds() {
        assert_eq!(CONFIRM_TIMEOUT, Duration::from_secs(60));
        assert!(!is_expired(Duration::from_secs(59)));
        // 边界含等号：正好 60s 即超时
        assert!(is_expired(Duration::from_secs(60)));
        assert!(is_expired(Duration::from_secs(61)));
    }

    #[test]
    fn an_unanswered_request_never_resolves_to_approval() {
        // 「还没答」和「同意」之间不能有任何路径。最需要确认的场景
        // （用户离开座位）恰恰就是没人答的那种。
        for elapsed in [
            Duration::ZERO,
            Duration::from_secs(1),
            Duration::from_secs(59),
            Duration::from_secs(60),
            Duration::from_secs(3600),
        ] {
            let o = resolve(None, elapsed, true);
            assert!(
                !o.allows_execution(),
                "elapsed={elapsed:?} 时未应答竟然放行了：{o:?}"
            );
        }
    }

    #[test]
    fn resolve_maps_the_four_outcomes() {
        assert_eq!(
            resolve(Some(true), Duration::ZERO, true),
            ConfirmOutcome::Approved
        );
        assert_eq!(
            resolve(Some(false), Duration::ZERO, true),
            ConfirmOutcome::Rejected
        );
        assert_eq!(
            resolve(None, CONFIRM_TIMEOUT, true),
            ConfirmOutcome::TimedOut
        );
        // 会话没了：即使用户「同意」过也不执行——他同意的是另一个会话里的事
        assert_eq!(
            resolve(Some(true), Duration::ZERO, false),
            ConfirmOutcome::Abandoned
        );
        assert!(!resolve(Some(true), Duration::ZERO, false).allows_execution());
    }

    #[test]
    fn stable_strings_round_trip_and_reject_variants() {
        for m in [AiMode::Disabled, AiMode::ReadOnlyOnly, AiMode::WithConfirm] {
            assert_eq!(AiMode::from_str_exact(m.as_str()), Some(m));
        }
        // 大小写与近似写法不得放宽——settings 里存错一个字就该报错而不是静默取默认
        for bad in ["Disabled", "readonly", "with-confirm", "", "on", "off"] {
            assert_eq!(AiMode::from_str_exact(bad), None, "{bad:?} 不该被接受");
        }
    }

    #[test]
    fn deny_messages_tell_the_user_where_to_go() {
        // 只说「被拒绝」不说怎么改，用户的下一步就是关掉整个功能
        for r in [
            DenyReason::GloballyDisabled,
            DenyReason::ModeAllowsReadOnlyOnly,
        ] {
            let m = r.message();
            assert!(m.contains("设置") || m.contains("档位"), "{m}");
        }
    }

    #[test]
    fn every_tier_mode_pair_has_a_decision() {
        // 穷举 3×3：任何一格漏掉都会在这里现形（match 的编译期穷尽性
        // 保证不了「语义上是否合理」，但保证得了「有返回值」）
        let mut auto = 0;
        let mut ask = 0;
        let mut deny = 0;
        for t in [Tier::ReadOnly, Tier::Write, Tier::Dangerous] {
            for m in [AiMode::Disabled, AiMode::ReadOnlyOnly, AiMode::WithConfirm] {
                match decide(t, m) {
                    Decision::AutoRun => auto += 1,
                    Decision::Confirm | Decision::StrongConfirm => ask += 1,
                    Decision::Deny(_) => deny += 1,
                }
            }
        }
        // 2 个自动（只读×两个非禁用档）、2 个询问（write/dangerous×默认档）、5 个拒绝
        assert_eq!((auto, ask, deny), (2, 2, 5), "3×3 裁决表的形状变了");
    }
}
