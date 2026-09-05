//! Agent 主循环：计划（一个模型回合）→ 执行/观察（一次一条，串行）→ 收敛。
//!
//! 实现规格 §5 的伪代码落成真代码。标记沿用：【G】= fs_policy 被咨询处；
//! 【A】= abort 检查处；【R】= 通道回收处——收拢点全在 gate.rs / dispatch.rs /
//! ExecPort，本文件只做编排，**不直接碰**任何策略或通道。
//!
//! 唯一需要 fake 栈的文件（FakeModelPort / CountingExecPort / ScriptedConfirmPort /
//! InMemAuditPort / FlagAbort）——其余 agent 文件都零 fake。

use crate::agent::budget::{BudgetLedger, MAX_PROVIDER_ATTEMPTS};
use crate::agent::dispatch::dispatch_step;
use crate::agent::gate::{effective_mode, ExternalToolTiers, Initiator};
use crate::agent::history::{Conversation, AGENT_SYSTEM_PROMPT};
use crate::agent::ports::{
    AbortSignal, AuditPort, ChannelRegistry, ConfirmPort, ExecPort, ModelPort, RunClock,
};
use crate::agent::record::{RedactedAction, StepRecord, StepVerdict};
use crate::agent::stop::StopReason;
use crate::agent::tool::TOOL_MANIFEST;
use crate::provider::ProviderConfig;
use std::sync::Arc;

/// 一次 run 的最终汇报：停因、预算快照、全部步记录。
///
/// UI 终态事件、RunEnd 审计行、测试断言三方消费同一个对象。
pub struct RunReport {
    pub stop: StopReason,
    pub budget: crate::agent::budget::BudgetState,
    pub steps: Vec<StepRecord>,
}

/// 端口束：注入面一把抓。测试给 fake，app 给真实现。
pub struct Ports {
    pub model: Arc<dyn ModelPort>,
    pub exec: Arc<dyn ExecPort>,
    pub confirm: Arc<dyn ConfirmPort>,
    pub audit: Arc<dyn AuditPort>,
    pub registry: Arc<ChannelRegistry>,
    pub abort: Arc<dyn AbortSignal>,
}

/// 主循环。调用方（app 层 agent_start）已做完启动期检查
/// （provider 有效、档位已加载且非 Disabled）并落了 RunBegin 行。
#[allow(clippy::too_many_arguments)]
pub async fn run_loop(
    // 借用而非取走：**对话就是这次 run 的 transcript**，调用方（面板/测试）
    // 跑完还要看它。取走的话「被拒的工具结果有没有进对话」这件事在外部
    // 不可观测——而那正是「拒绝不回喂」这条安全不变式的判据。
    conv: &mut Conversation,
    cfg: ProviderConfig,
    ledger: &mut BudgetLedger,
    ports: &Ports,
    clock: &RunClock,
    mode: fs_policy::AiMode,
    ext: &ExternalToolTiers,
    known_secrets: &[String],
    // 已挂载的外部 MCP 工具（M3 出口 6）。空 = 没挂任何 server ⇒ 模型看不到
    // 任何外部工具，也就不会去调——「用户挂了什么才有什么」。
    external: &[crate::agent::history::ExternalToolSchema],
) -> RunReport {
    let mut steps: Vec<StepRecord> = Vec::new();
    let stop = drive(
        conv,
        &cfg,
        ledger,
        ports,
        clock,
        mode,
        ext,
        known_secrets,
        external,
        &mut steps,
    )
    .await;
    let budget = ledger.snapshot();

    // 收尾：此刻顺序执行下**无在飞命令**（急停路径已在相位内 await 完被杀命令的
    // Observation）；通道计数的运行时层断言（出口 2 原文）在这里做一次终检。
    debug_assert!(
        ports.registry.count() == 0,
        "run 结束时通道计数应为 0，实测 {}",
        ports.registry.count()
    );

    // RunEnd 行：动作文本带停因与预算三件套（「为什么停」不带上数字，
    // 用户的第一反应是「坏了」而不是「到量了」——StopReason::summary_line 的口径）。
    let end_rec = StepRecord {
        index: steps.len() as u32,
        turn: budget.steps_taken,
        tool: "run".to_string(),
        display: RedactedAction::new(
            &format!(
                "[run] end: {} | steps {}/{} streak {} tokens known {} unaccounted {} charged {}",
                stop.summary_line(),
                budget.steps_taken,
                budget.tokens.conservative_total(),
                budget.streak,
                budget.tokens.known(),
                budget.tokens.unaccounted_turns(),
                budget.tokens.conservative_total(),
            ),
            known_secrets,
        ),
        tier: fs_policy::Tier::ReadOnly,
        rule: None,
        decision: fs_policy::Decision::AutoRun,
        // RunEnd 是簿记行：verdict = request_only（规格 §8 的夹逼行口径）。
        // 落成「无 outcome」会让它在审计表里变成一个没有裁决的行——
        // 七值里本来就有诚实的那个答案。
        outcome: Some(crate::agent::record::StepOutcome {
            audit_verdict: StepVerdict::RequestOnly,
            observation: None,
            digest: None,
        }),
    };
    // RunEnd 写失败不改变停因（run 已经停了，闩里已有事实）；
    // 但要把它算作一次审计失败向上冒泡吗？——不：停因是「事实的记录」，
    // RunEnd 是「记录的补全」，两者优先级不同。静默记不下来比谎报停因糟，
    // 但谎报停因比漏一行 RunEnd 糟得多。这里用 warn 级的忽略（与 ai_cmd 同口径），
    // 完整行不进 steps（时间线不显示内部簿记行）。
    if ports.audit.append(end_rec).await.is_err() {
        // 已停之 run 的收尾行写不进去：没有更好的动作了。
    }
    let _ = MAX_PROVIDER_ATTEMPTS; // 供文档引用；重试在 ModelPort 实现内
    RunReport {
        stop,
        budget,
        steps,
    }
}

/// 内层驱动，返回停因。所有 stop 路径汇合到这里。
#[allow(clippy::too_many_arguments)]
async fn drive(
    conv: &mut Conversation,
    cfg: &ProviderConfig,
    ledger: &mut BudgetLedger,
    ports: &Ports,
    clock: &RunClock,
    mode: fs_policy::AiMode,
    ext: &ExternalToolTiers,
    known_secrets: &[String],
    external: &[crate::agent::history::ExternalToolSchema],
    steps: &mut [StepRecord],
) -> StopReason {
    // 档位在此刻冻结进循环（实现规格 §6：与预算一起，运行中不变）。
    let _ = effective_mode(mode, None);
    loop {
        // ── PLAN：一个模型回合 ────────────────────────────────────────
        let req = conv.build(TOOL_MANIFEST, external);
        //【A】闩→急停→步数→token 强预检（固定检查序，budget.rs）
        if let Err(r) = ledger.admit_turn(req.max_tokens) {
            return r;
        }
        //【A】ModelPort 实现内部每次尝试 select! abort；急停赢则 Err 冒泡
        let resp = match ports.model.turn(cfg, &req).await {
            Ok(r) => r,
            Err(e) => {
                // 重试耗尽（重试在端口实现里，≤MAX_PROVIDER_ATTEMPTS）。
                // 每次失败尝试都已由端口落了保守上界（请求确实发出去了）。
                ledger.settle_turn(None, ports.model.last_request_bound());
                return StopReason::ProviderFatal {
                    user_message: user_message_of(&e),
                    attempts: MAX_PROVIDER_ATTEMPTS,
                };
            }
        };
        // 落账：usage 或保守上界；触顶即闩（本回剩余工具调用一个不执行）
        ledger.settle_turn(resp.usage, ports.model.last_request_bound());
        if let Some(r) = ledger.stopped() {
            return clone_reason(r);
        }

        if resp.tool_calls.is_empty() {
            // 空工具调用 = 纯文本最终答复。**不读 stop_reason**（原始 provider 串，
            // 未归一化，三家不同义）——「没有工具调用」本身就是收敛信号。
            return StopReason::Finished { summary: resp.text };
        }
        conv.messages
            .push(crate::agent::history::ChatMessage::Assistant {
                text: resp.text,
                tool_calls: resp.tool_calls.clone(),
            });

        // ── EXECUTE/OBSERVE：一次一条，串行 ──────────────────────────
        for call in resp.tool_calls {
            let turn_no = ledger.snapshot().steps_taken;
            // dispatch 是唯一执行入口（单闸门单契约的物理落点）：
            // 授权→gate→预算→确认→执行→审计双行，内联 await（审计先行）。
            let outcome = dispatch_step(
                call,
                turn_no,
                steps.len() as u32,
                Initiator::Agent,
                conv,
                ledger,
                ports,
                clock,
                mode,
                ext,
                known_secrets,
            )
            .await;
            match outcome {
                // 继续：处理下一条调用
                Ok(()) => {}
                // 停：dispatch 已经落完它该落的审计行
                Err(r) => return r,
            }
            // settle 闩上的话，本回余下调用全部跳过（budget.rs 的强读法）
            if let Some(r) = ledger.stopped() {
                return clone_reason(r);
            }
        }
        // ── CONVERGE：回到循环顶，下一回合带全部 ToolResult ───────────
    }
}

/// StopReason 深拷贝（budget.rs 的 clone_stop 是私有的；两份臂表由
/// stop.rs 的变体穷举保证同步——新增变体时两处编译器都会点名）。
pub(crate) fn clone_reason(r: &StopReason) -> StopReason {
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

/// ProviderError 的用户面消息。只放 user_message 一类的内容——detail 可能含
/// 密钥/屏幕碎片，不进报告（StopReason::ProviderFatal 的变体注释）。
fn user_message_of(e: &crate::ProviderError) -> String {
    // ProviderError 没有 user_message()（那是在 app 层装配的 UX 决定）；
    // 这里用 Display，与 transport 错误「刻意只带 e 的显示串，不带 spec」同口径。
    format!("{e}")
}

/// 帮 app 层构造初始对话（系统提示词 + 用户任务）。
/// RunBegin 行由调用方落——run_loop 看不到 run id，簿记行属于装配层。
pub fn initial_conversation(task: &str) -> Conversation {
    Conversation {
        system: AGENT_SYSTEM_PROMPT.to_string(),
        messages: vec![crate::agent::history::ChatMessage::User(task.to_string())],
    }
}

/// 供测试与 app 层断言「RunEnd 行的 Verdict」——七值里的 request_only。
pub const RUN_END_VERDICT: StepVerdict = StepVerdict::RequestOnly;
