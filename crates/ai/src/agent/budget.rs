//! 三条预算与停机闩。纯函数对象（Budget + 累计态 → Admission），零 fake 全测。
//!
//! # 固定检查序
//!
//! `admit_*` 的检查顺序是**定义**而非实现偶然：闩 → 急停 → 步数/连续 → token。
//! 多重触顶时「报哪个」由这个顺序决定——把它写成可重排的 if 链，「报哪个」
//! 就变成哪个分支先跑到的偶然。测试 [`fixed_check_order_is_a_definition`] 钉死。

use crate::agent::ports::AbortSignal;
use crate::agent::stop::StopReason;
use std::sync::Arc;

pub struct Budgets {
    /// 模型回合上限（回合粒度，总量，只增）。
    pub max_steps: u32,
    /// 连续无人参与动作上限（段粒度，人触点清零）。
    pub max_consecutive_auto: u32,
    /// token 总量上限（保守判定，见 [`TokensSpent`]）。
    pub max_tokens: u64,
}

impl Budgets {
    pub const DEFAULT_MAX_STEPS: u32 = 24;
    pub const DEFAULT_MAX_CONSECUTIVE_AUTO: u32 = 8;
    pub const DEFAULT_MAX_TOKENS: u64 = 200_000;

    pub fn defaults() -> Self {
        Self {
            max_steps: Self::DEFAULT_MAX_STEPS,
            max_consecutive_auto: Self::DEFAULT_MAX_CONSECUTIVE_AUTO,
            max_tokens: Self::DEFAULT_MAX_TOKENS,
        }
    }
}

/// `MAX_PROVIDER_ATTEMPTS` 是第四个独立预算（网络重试）：不占步数、不重置连跑
/// （没执行任何东西，也没人在环），但每次失败尝试**落保守上界**——请求确实发出去了。
pub const MAX_PROVIDER_ATTEMPTS: u8 = 3;

/// token 三本账：「确切花了多少」与「花了但不知道多少」分开记。
///
/// **绝不把后者记成 0**——那是把「不知道」当「知道是零」。某些网关剥掉
/// usage 字段；若无账回合免费，预算就在最需要它的环境里（不透明的网关后面）
/// 恰好失效。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TokensSpent {
    /// 各回合 Usage 的 input+output 之和（有账部分）。
    known: u64,
    /// `usage == None`（或请求失败）的回合数——是笔数，不是 0。
    unaccounted_turns: u32,
    /// 预算判定唯一入口 = known + 每个无账回合的可证上界。
    ///
    /// 上界 = 该回请求体字节数 + 该回 max_tokens：每 token ≥1 字节 ⇒ 字节数是
    /// 输入 token 的上界；max_tokens 是三家共同的输出上界。可证的高估，不是猜。
    /// 只增，永不向下修正——账目单调。
    conservative_charged: u64,
}

impl TokensSpent {
    pub fn known(&self) -> u64 {
        self.known
    }
    pub fn unaccounted_turns(&self) -> u32 {
        self.unaccounted_turns
    }
    /// 预算判定的唯一入口。
    pub fn conservative_total(&self) -> u64 {
        self.conservative_charged
    }

    fn settle(&mut self, usage: Option<crate::Usage>, bound: u64) {
        match usage {
            Some(u) => {
                self.known += u.input_tokens as u64 + u.output_tokens as u64;
                // 有精确账的回合：charged += 精确值。bound 是无账回合才用的上界。
                self.conservative_charged += u.input_tokens as u64 + u.output_tokens as u64;
            }
            None => {
                self.unaccounted_turns += 1;
                self.conservative_charged += bound;
            }
        }
    }
}

pub struct BudgetLedger {
    /// 构造后不可变（无 setter）——中途调上限需要定义「重置什么」语义，
    /// M3 不做（调上限 = 开新任务）。
    limits: Budgets,
    steps_taken: u32,
    streak: u32,
    tokens: TokensSpent,
    /// 观测面：全程人触点数（UI 报告「人参与了几次」用）。
    human_touchpoints: u32,
    abort: Arc<dyn AbortSignal>,
    /// 闩。一旦 `Some`，`admit_*` 永远原样返回——没有解除方法。
    ///
    /// 「停了就是停了」是预算的语义之一：续跑 = 洗预算，产品层明文禁止
    /// UI 写自动重启循环。
    stopped: Option<StopReason>,
}

impl BudgetLedger {
    pub fn new(limits: Budgets, abort: Arc<dyn AbortSignal>) -> Self {
        Self {
            limits,
            steps_taken: 0,
            streak: 0,
            tokens: TokensSpent::default(),
            human_touchpoints: 0,
            abort,
            stopped: None,
        }
    }

    /// 发起模型回合前的预检。固定顺序：**闩 → 急停 → 步数 → token 强预检**。
    ///
    /// 先查后记：停下的那一下不记账，报告里 `taken == limit` 恰好成立——
    /// 「预算让你跑了 N 步」的 N 不含被拒的那次。
    pub fn admit_turn(&mut self, request_max_tokens: u32) -> Result<(), StopReason> {
        if let Some(r) = &self.stopped {
            return Err(clone_stop(r));
        }
        if self.abort.requested() {
            let r = StopReason::UserAbort {
                killed_in_flight: None,
            };
            self.stopped = Some(clone_stop(&r));
            return Err(r);
        }
        if self.steps_taken >= self.limits.max_steps {
            let r = StopReason::StepsExhausted {
                taken: self.steps_taken,
                limit: self.limits.max_steps,
            };
            self.stopped = Some(clone_stop(&r));
            return Err(r);
        }
        // 强预检：剩余额度（饱和减）容不下本回 request_max_tokens（唯一事前可知的
        // 部分）就不发请求。§ max_tokens 恒 > 0（构造时校验）。
        let remaining = self
            .limits
            .max_tokens
            .saturating_sub(self.tokens.conservative_total());
        if remaining < request_max_tokens as u64 {
            let r = StopReason::TokensExhausted {
                spent: self.tokens,
                limit: self.limits.max_tokens,
            };
            self.stopped = Some(clone_stop(&r));
            return Err(r);
        }
        self.steps_taken += 1;
        Ok(())
    }

    /// 派发自动动作前的预检：**闩 → 急停 → 连续**。streak 在派发时 +1
    /// （不看结局——失败的只读循环同样消耗「无人值守」语义）。
    pub fn admit_auto(&mut self) -> Result<(), StopReason> {
        if let Some(r) = &self.stopped {
            return Err(clone_stop(r));
        }
        if self.abort.requested() {
            let r = StopReason::UserAbort {
                killed_in_flight: None,
            };
            self.stopped = Some(clone_stop(&r));
            return Err(r);
        }
        if self.streak >= self.limits.max_consecutive_auto {
            let r = StopReason::ConsecutiveAutoExhausted {
                streak: self.streak,
                limit: self.limits.max_consecutive_auto,
            };
            self.stopped = Some(clone_stop(&r));
            return Err(r);
        }
        // 派发即计数。挪到「执行成功之后」是最诱人的写法，也是最错的：
        // `cat /nonexistent` 的失败循环会永不触顶，而这个预算要截断的
        // 恰恰是无人值守的失控形态，失败与否都是失控。
        self.streak += 1;
        Ok(())
    }

    /// 一次模型请求落账。每个被派发的请求要么记 usage、要么记上界 bound
    /// （= 请求体字节数 + 该回 max_tokens）。
    ///
    /// 落账后若 `conservative_total >= max_tokens` 立即闩 `TokensExhausted`：
    /// 本回剩余工具调用一个都不执行（「触顶即停」的强读法——弱读法是
    /// 「跑完这一回再说」，那会让超支精确地多一整回）。
    pub fn settle_turn(&mut self, usage: Option<crate::Usage>, bound: u64) {
        self.tokens.settle(usage, bound);
        if self.tokens.conservative_total() >= self.limits.max_tokens && self.stopped.is_none() {
            // 不覆盖已有停因：先到的停因是事实，settle 只是又撞上同一堵墙。
            self.stopped = Some(StopReason::TokensExhausted {
                spent: self.tokens,
                limit: self.limits.max_tokens,
            });
        }
    }

    /// streak 唯一清零点：`pub(crate)`，全仓恰两个调用点（dispatch 的 Approved 臂、
    /// ask_user 被回答处）。mod.rs 的 grep 守卫钉死调用点数量。
    ///
    /// 为什么只有人触点清零：攻击者在提示注入下能制造任意多个模型侧事件、
    /// 零个人侧事件。「只有人给出的、模型无法伪造的事件才清零」让上限在
    /// 注入下依然成立；把清零钥匙交给被审者等于没有上限。
    // 批次③（dispatch.rs）落地前无生产调用点；届时删除此 allow。
    // grep 守卫（mod.rs）在 dispatch 落地后钉「恰两个调用点」。
    #[allow(dead_code)]
    pub(crate) fn human_touched(&mut self) {
        self.streak = 0;
        self.human_touchpoints += 1;
    }

    /// 外部停机（PolicyDenied / UserRejected / …）。
    pub fn stop(&mut self, r: StopReason) {
        if self.stopped.is_none() {
            self.stopped = Some(r);
        }
    }

    pub fn stopped(&self) -> Option<&StopReason> {
        self.stopped.as_ref()
    }

    pub fn snapshot(&self) -> BudgetState {
        BudgetState {
            steps_taken: self.steps_taken,
            streak: self.streak,
            tokens: self.tokens,
            human_touchpoints: self.human_touchpoints,
            abort_requested: self.abort.requested(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BudgetState {
    pub steps_taken: u32,
    pub streak: u32,
    pub tokens: TokensSpent,
    pub human_touchpoints: u32,
    pub abort_requested: bool,
}

/// StopReason 深拷贝（TokenExhausted 携带 Copy 的 TokensSpent，其余变体逐臂拷）。
/// 只在 admit 的「先闩再返回」两处用到——闩里存一份、返回值给调用方一份，
/// 两份内容必须一致。
fn clone_stop(r: &StopReason) -> StopReason {
    match r {
        StopReason::Finished { summary } => StopReason::Finished {
            summary: summary.clone(),
        },
        StopReason::StepsExhausted { taken, limit } => StopReason::StepsExhausted {
            taken: *taken,
            limit: *limit,
        },
        StopReason::ConsecutiveAutoExhausted { streak, limit } => {
            StopReason::ConsecutiveAutoExhausted {
                streak: *streak,
                limit: *limit,
            }
        }
        StopReason::TokensExhausted { spent, limit } => StopReason::TokensExhausted {
            spent: *spent,
            limit: *limit,
        },
        StopReason::PolicyDenied {
            reason,
            message,
            rules,
            details,
        } => StopReason::PolicyDenied {
            reason: *reason,
            message: message.clone(),
            rules: rules.clone(),
            details: details.clone(),
        },
        StopReason::UserRejected => StopReason::UserRejected,
        StopReason::ConfirmTimedOut => StopReason::ConfirmTimedOut,
        StopReason::SessionAbandoned => StopReason::SessionAbandoned,
        StopReason::AskUnanswered => StopReason::AskUnanswered,
        StopReason::UserAbort { killed_in_flight } => StopReason::UserAbort {
            killed_in_flight: killed_in_flight.clone(),
        },
        StopReason::ProviderFatal {
            user_message,
            attempts,
        } => StopReason::ProviderFatal {
            user_message: user_message.clone(),
            attempts: *attempts,
        },
        StopReason::AuditWriteFailed { detail } => StopReason::AuditWriteFailed {
            detail: detail.clone(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    use std::sync::atomic::Ordering;

    /// 旗标 fake：离线测试的急停令牌。真实实现（app 层）是 AtomicBool + Notify。
    struct FlagAbort(AtomicBool);
    impl FlagAbort {
        fn new() -> Arc<Self> {
            Arc::new(Self(AtomicBool::new(false)))
        }
        fn set(&self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    impl AbortSignal for FlagAbort {
        fn requested(&self) -> bool {
            self.0.load(Ordering::SeqCst)
        }
        fn fired<'a>(
            &'a self,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
            // 离线测试不进 select!；requested() 的瞬时快照就够 admit 用。
            Box::pin(std::future::pending())
        }
    }

    fn ledger(max_steps: u32, max_auto: u32, max_tokens: u64) -> (BudgetLedger, Arc<FlagAbort>) {
        let f = FlagAbort::new();
        (
            BudgetLedger::new(
                Budgets {
                    max_steps,
                    max_consecutive_auto: max_auto,
                    max_tokens,
                },
                f.clone(),
            ),
            f,
        )
    }

    /* ── A. 步数 ─────────────────────────────────────────────── */

    /// A1：步数触顶，`taken == limit` 恰好成立（先查后记）。
    #[test]
    fn steps_exhausted_reports_taken_equals_limit() {
        let (mut l, _) = ledger(2, 8, 1_000_000);
        assert!(l.admit_turn(100).is_ok()); // taken 0→1
        assert!(l.admit_turn(100).is_ok()); // taken 1→2
        let Err(StopReason::StepsExhausted { taken, limit }) = l.admit_turn(100) else {
            panic!("第三次该拒");
        };
        assert_eq!((taken, limit), (2, 2), "先查后记：停下的那一下不记账");
        // 闩住后再 admit 仍拒（同一个停因）
        assert!(matches!(
            l.admit_turn(100),
            Err(StopReason::StepsExhausted { .. })
        ));
    }

    /* ── 连续 ────────────────────────────────────────────────── */

    /// A2：streak 派发即计数——不看执行结局。
    #[test]
    fn streak_counts_at_dispatch_not_at_success() {
        let (mut l, _) = ledger(24, 2, 1_000_000);
        assert!(l.admit_auto().is_ok()); // streak 1
        assert!(l.admit_auto().is_ok()); // streak 2
        assert!(matches!(
            l.admit_auto(),
            Err(StopReason::ConsecutiveAutoExhausted {
                streak: 2,
                limit: 2
            })
        ));
    }

    /// A4/A5：两个重置点（pub(crate) 在测试里直接调用，同 crate）。
    ///
    /// 刻意**不先触顶**：触顶即闩死，触点救不了闩——那是
    /// a_latched_stop_is_not_unlatched_by_a_touchpoint 钉死的行为。本条要验的是
    /// 「未触顶时，触点把段配额还回来」。
    #[test]
    fn human_touchpoints_reset_the_streak() {
        let (mut l, _) = ledger(24, 3, 1_000_000);
        l.admit_auto().unwrap(); // streak 1
        l.human_touched(); // 触点①：streak → 0
                           // 用 snapshot 观测清零——不在中间触顶（触顶即闩死，触点救不了闩），
                           // 这是本测试第二版学到的：想验证「段配额还回来了」，看计数器，
                           // 不要靠「顶一下看它疼不疼」。
        assert_eq!(l.snapshot().streak, 0, "触点后 streak 应回零");
        assert!(l.admit_auto().is_ok()); // streak 1
        assert!(l.admit_auto().is_ok()); // streak 2
        assert_eq!(l.snapshot().streak, 2);
        l.human_touched(); // 触点②（ask_user 被回答）：再次清零
        assert_eq!(l.snapshot().streak, 0);
        assert_eq!(l.snapshot().human_touchpoints, 2);
        // 收尾触一次顶（此后无断言）：没触点的话 cap=3 会在下一次 admit 拒
        assert!(l.admit_auto().is_ok()); // streak 1（触点后的新段）
    }

    /// A7：被闩住的触顶不会因为 human_touched 而解除——「停了就是停了」。
    #[test]
    fn a_latched_stop_is_not_unlatched_by_a_touchpoint() {
        let (mut l, _) = ledger(24, 1, 1_000_000);
        l.admit_auto().unwrap();
        let Err(_) = l.admit_auto() else { panic!() };
        l.human_touched(); // 触点清 streak，但闩不解除
        assert!(
            matches!(
                l.stopped(),
                Some(StopReason::ConsecutiveAutoExhausted { .. })
            ),
            "闩住了就停了；续跑=开新任务"
        );
    }

    /* ── token 三本账 ────────────────────────────────────────── */

    /// A8：精确记账。
    #[test]
    fn known_usage_settles_exactly() {
        let (mut l, _) = ledger(24, 8, 1_000_000);
        l.settle_turn(
            Some(crate::Usage {
                input_tokens: 100,
                output_tokens: 50,
            }),
            0,
        );
        l.settle_turn(
            Some(crate::Usage {
                input_tokens: 100,
                output_tokens: 50,
            }),
            0,
        );
        let t = l.snapshot().tokens;
        assert_eq!(t.known(), 300);
        assert_eq!(t.unaccounted_turns(), 0);
        assert_eq!(t.conservative_total(), 300);
    }

    /// A9：None-usage 记可证上界，绝不记 0。
    #[test]
    fn unaccounted_turns_charge_the_provable_bound() {
        let (mut l, _) = ledger(24, 8, 1_000_000);
        l.settle_turn(None, 10_000);
        let t = l.snapshot().tokens;
        assert_eq!(t.known(), 0, "无账不进 known");
        assert_eq!(t.unaccounted_turns(), 1, "是笔数");
        assert_eq!(t.conservative_total(), 10_000, "记上界，不是 0");
    }

    /// A10：强预检——剩余额度容不下本回请求就不发。
    #[test]
    fn the_strong_precheck_refuses_before_sending() {
        let (mut l, _) = ledger(24, 8, 3_000);
        l.settle_turn(
            Some(crate::Usage {
                input_tokens: 2_900,
                output_tokens: 0,
            }),
            0,
        );
        // 剩 100，容不下 4096 的回合
        assert!(matches!(
            l.admit_turn(4_096),
            Err(StopReason::TokensExhausted { .. })
        ));
    }

    /// A11：settle 后闩——本回剩余工具调用一个不执行。
    #[test]
    fn settling_past_the_limit_latches_immediately() {
        let (mut l, _) = ledger(24, 8, 5_000);
        assert!(l.admit_turn(4_096).is_ok());
        // 这回 usage 把账推过线
        l.settle_turn(
            Some(crate::Usage {
                input_tokens: 4_999,
                output_tokens: 100,
            }),
            0,
        );
        assert!(
            matches!(l.stopped(), Some(StopReason::TokensExhausted { .. })),
            "settle 后立即闩，不是跑完这回再说"
        );
        // 闩住了 ⇒ 连 auto 也进不去（「剩余工具调用一个不执行」）
        assert!(l.admit_auto().is_err());
    }

    /* ── 固定检查序 ──────────────────────────────────────────── */

    /// A12：多重触顶时「报哪个」是定义。步数+token 同触 ⇒ StepsExhausted；
    /// 急停+步数同触 ⇒ UserAbort。
    #[test]
    fn fixed_check_order_is_a_definition() {
        // 步数与 token 双触顶：报步数（顺序：闩→急停→步数→token）。
        // 关键：双触必须发生在**同一次 admit** 里且事前无闩——settle 若已把
        // token 推过线会先闩上，那之后闩永远先报（闩是第一位），测的就不是
        // 步数与 token 的先后了。
        let (mut l, _) = ledger(1, 8, 100);
        assert!(l.admit_turn(50).is_ok()); // taken=1（步数到顶）
        l.settle_turn(
            Some(crate::Usage {
                input_tokens: 40,
                output_tokens: 0,
            }),
            0,
        ); // 40<100：不闩
           // 剩余 60 容不下 50 的请求（token 也会拒），但步数检查在前
        assert!(matches!(
            l.admit_turn(50),
            Err(StopReason::StepsExhausted { .. })
        ));

        // 急停与步数双触：报急停（急停在步数之前）
        let (mut l, f) = ledger(1, 8, 1_000_000);
        assert!(l.admit_turn(50).is_ok());
        f.set();
        assert!(matches!(
            l.admit_turn(50),
            Err(StopReason::UserAbort { .. })
        ));
    }

    /// A13：常量钉住（改一个常量 ⇒ 恰这里红，逼人回答「为什么改」）。
    #[test]
    fn budget_constants_are_pinned() {
        assert_eq!(Budgets::DEFAULT_MAX_STEPS, 24);
        assert_eq!(Budgets::DEFAULT_MAX_CONSECUTIVE_AUTO, 8);
        assert_eq!(Budgets::DEFAULT_MAX_TOKENS, 200_000);
        assert_eq!(MAX_PROVIDER_ATTEMPTS, 3);
    }

    /// admit 的急停检查是**瞬时重查**，不闩「未请求」：early 时刻没按停不代表
    /// 后续时刻没按。旗标中途置位后下一次 admit 必须看见。
    #[test]
    fn abort_is_rechecked_not_latched_as_clear() {
        let (mut l, f) = ledger(24, 8, 1_000_000);
        assert!(l.admit_turn(100).is_ok()); // 此刻未请求
        f.set(); // 中途按停
        assert!(matches!(l.admit_auto(), Err(StopReason::UserAbort { .. })));
    }
}
