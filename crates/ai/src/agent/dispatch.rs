//! `dispatch()`：**唯一执行入口**——授权→gate→预算→确认→执行→审计双行，
//! 全部内联 await（审计先行由控制流保证：下一次模型调用结构上发生在落库之后）。
//!
//! 独立成文件是给 grep 自守网一个唯一锚点：fs_policy 的咨询（经 gate.rs）、
//! 预算的消耗（admit_auto）、人触点（human_touched 的两个调用点都在这里）、
//! 通道的开关（经 ExecPort）——「单闸门单契约」的物理落点。

use crate::agent::budget::BudgetLedger;
use crate::agent::confirm::{conclude, ConfirmAnswer, ConfirmTicket};
use crate::agent::gate::{
    classify_for_gate, gate_check, ExternalToolTiers, GateOutcome, Initiator,
};
use crate::agent::history::ChatMessage;
use crate::agent::ports::RunClock;
use crate::agent::record::{RedactedAction, StepOutcome, StepRecord, StepVerdict};
use crate::agent::run::{clone_reason, Ports};
use crate::agent::stop::StopReason;
use crate::agent::tool::{parse_tool, Tool, ToolParseError};
use crate::ToolCall;
use std::time::Duration;

/// dispatch 的返回：`Ok(())` = 继续下一条；`Err(stop)` = 停（审计行已落）。
#[allow(clippy::too_many_arguments)]
pub(crate) async fn dispatch_step(
    call: ToolCall,
    turn: u32,
    index: u32,
    initiator: Initiator,
    conv: &mut crate::agent::history::Conversation,
    ledger: &mut BudgetLedger,
    ports: &Ports,
    clock: &RunClock,
    mode: fs_policy::AiMode,
    ext: &ExternalToolTiers,
    known_secrets: &[String],
) -> Result<(), StopReason> {
    // ── 模型错误 lane：parse 失败不进 gate，回喂重试 ─────────────────
    let tool = match parse_tool(&call.name, &call.arguments_json) {
        Ok(t) => t,
        Err(e) => {
            // 步数已在该回合 admit_turn 扣过，错误重试天然有界。
            // is_error 让模型知道这次调用没产生有效结果。
            conv.messages.push(ChatMessage::ToolResult {
                call_id: call.id,
                content: e.feed_message(),
                is_error: true,
            });
            return Ok(());
        }
    };

    // ──【G】① 全仓唯一分类点 → ② 全仓唯一 decide 调用点 ─────────────
    let gated = classify_for_gate(tool, initiator, ext);
    let tier = gated.verdict.tier;
    let rule = gated.verdict.primary().map(|r| r.rule);
    //【G】② 全仓唯一 decide 调用点（gate_check 内部）。这里先取结论，
    // StepRecord 的 decision 字段从它推导——不再调第二次。
    let verdict_outcome = gate_check(&gated, mode);
    let step_decision = decision_of(&verdict_outcome);
    let base = |outcome: Option<StepOutcome>| StepRecord {
        index,
        turn,
        tool: manifest_name(&gated.tool),
        display: display_of(&gated.tool, known_secrets),
        tier,
        rule,
        decision: step_decision,
        outcome,
    };

    match verdict_outcome {
        GateOutcome::Deny {
            reason,
            rules,
            details,
        } => {
            // 拒即停（出口 1 为准）。**不回喂模型**——把拒绝喂回去等于邀请它
            // 换一种危险姿势再试。
            let rec = StepRecord {
                outcome: Some(StepOutcome {
                    audit_verdict: StepVerdict::Denied,
                    observation: None,
                    digest: None,
                }),
                ..base(None)
            };
            audit(ports, rec).await?;
            Err(StopReason::PolicyDenied {
                message: reason.message().to_string(),
                reason,
                rules,
                details,
            })
        }
        GateOutcome::Auto => match &gated.tool {
            Tool::Finish { summary } => {
                // 控制流：不计数、不开通道，但仍过了 gate_check（ReadOnly）
                Err(StopReason::Finished {
                    summary: summary.clone(),
                })
            }
            Tool::AskUser { question } => {
                //【A】ConfirmPort 实现内部 select! abort 与会话存亡
                ask_and_feed(question, conv, ledger, ports, base).await
            }
            _ => {
                // ──【A】streak 预检 + 派发即 +1 ────────────────────────
                if let Err(r) = ledger.admit_auto() {
                    // 触顶：该命令**不执行**。审计补一行 RequestOnly——
                    // 「请求在、执行无」是七值里现成的诚实答案，比留空强。
                    let rec = StepRecord {
                        outcome: Some(StepOutcome {
                            audit_verdict: StepVerdict::RequestOnly,
                            observation: None,
                            digest: None,
                        }),
                        ..base(None)
                    };
                    audit(ports, rec).await?;
                    return Err(r);
                }
                // 双行制第 1 行（crash 窗口收窄：执行中崩溃也留得下「发起过」）
                let pre = StepRecord {
                    outcome: Some(StepOutcome {
                        audit_verdict: StepVerdict::RequestOnly,
                        observation: None,
                        digest: None,
                    }),
                    ..base(None)
                };
                audit(ports, pre).await?;
                // ──【R】ExecPort 内部：lease → clock.reset() → run_command
                //（唯一返回点必 close；超时臂先 kill 后 close——全在 exec.rs 钉死）
                let obs = exec_tool(ports, &gated.tool, clock).await;
                // 急停拨快了时钟的话，obs 是被杀命令的部分输出（timed_out=true）。
                // 先落结果行（它确实开跑过），停因交给下面。
                let rec = StepRecord {
                    outcome: Some(StepOutcome {
                        audit_verdict: StepVerdict::AutoRun,
                        observation: Some(obs.clone()),
                        digest: None, // 由写入侧（app 层）算：fs_ai 不依赖 connmgr 的 sha256
                    }),
                    ..base(None)
                };
                audit(ports, rec).await?;
                if ports.abort.requested() {
                    return Err(StopReason::UserAbort {
                        killed_in_flight: Some(obs),
                    });
                }
                feed_tool_result(&call.id, &obs, conv, known_secrets);
                Ok(())
            }
        },
        GateOutcome::Confirm { strong } => {
            // ── 确认回路：票据入队，阻塞等待（60s = CONFIRM_TIMEOUT）────
            let ticket = ConfirmTicket {
                id: next_ticket_id(),
                display: display_of(&gated.tool, known_secrets),
                tier,
                strong,
                rules: gated.verdict.reasons.iter().map(|r| r.rule).collect(),
                details: gated
                    .verdict
                    .reasons
                    .iter()
                    .map(|r| r.detail.clone())
                    .collect(),
                deadline: fs_policy::CONFIRM_TIMEOUT,
            };
            //【A】端口内部 select! abort；急停时收 ContextGone
            let ans = ports.confirm.confirm(ticket).await;
            match conclude(ans, Duration::from_secs(0)) {
                fs_policy::ConfirmOutcome::Approved => {
                    // ← streak 重置点 ①：人看过这条 write/dangerous 并点了准
                    ledger.human_touched();
                    let pre = StepRecord {
                        outcome: Some(StepOutcome {
                            audit_verdict: StepVerdict::RequestOnly,
                            observation: None,
                            digest: None,
                        }),
                        ..base(None)
                    };
                    audit(ports, pre).await?;
                    let obs = exec_tool(ports, &gated.tool, clock).await;
                    // approved 真的意味着已执行（与 ai_cmd 那条「建议即
                    // Approved」的不实映射相反——Agent 侧结构性消失）
                    let rec = StepRecord {
                        outcome: Some(StepOutcome {
                            audit_verdict: StepVerdict::Approved,
                            observation: Some(obs.clone()),
                            digest: None,
                        }),
                        ..base(None)
                    };
                    audit(ports, rec).await?;
                    if ports.abort.requested() {
                        return Err(StopReason::UserAbort {
                            killed_in_flight: Some(obs),
                        });
                    }
                    feed_tool_result(&call.id, &obs, conv, known_secrets);
                    Ok(())
                }
                fs_policy::ConfirmOutcome::Rejected => {
                    let rec = StepRecord {
                        outcome: Some(StepOutcome {
                            audit_verdict: StepVerdict::Rejected,
                            observation: None,
                            digest: None,
                        }),
                        ..base(None)
                    };
                    audit(ports, rec).await?;
                    Err(StopReason::UserRejected)
                }
                fs_policy::ConfirmOutcome::TimedOut => {
                    let rec = StepRecord {
                        outcome: Some(StepOutcome {
                            audit_verdict: StepVerdict::TimedOut,
                            observation: None,
                            digest: None,
                        }),
                        ..base(None)
                    };
                    audit(ports, rec).await?;
                    Err(StopReason::ConfirmTimedOut)
                }
                fs_policy::ConfirmOutcome::Abandoned => {
                    let rec = StepRecord {
                        outcome: Some(StepOutcome {
                            audit_verdict: StepVerdict::Abandoned,
                            observation: None,
                            digest: None,
                        }),
                        ..base(None)
                    };
                    audit(ports, rec).await?;
                    // 急停的 ContextGone 报 UserAbort；会话掉线报 SessionAbandoned
                    // ——「是人按的停」与「窗口没了」导向不同的后续。
                    Err(if ports.abort.requested() {
                        StopReason::UserAbort {
                            killed_in_flight: None,
                        }
                    } else {
                        StopReason::SessionAbandoned
                    })
                }
            }
        }
    }
}

/// 按工具类型分派到对应的端口方法。
///
/// **不是所有工具都走 `run`**：读文件/列目录/上传下载各有自己的端口方法
/// （实现侧是 sftp 子系统而不是 exec 通道）。第一版把它们全塞进 `run`，
/// 于是四个端口方法从没被调用过——而 fake 的 run 对它们回一个占位观察值，
/// 测试照样绿。是「每条命令都要关通道」那条断言把它逼出来的。
async fn exec_tool(ports: &Ports, tool: &Tool, clock: &RunClock) -> crate::exec::Observation {
    match tool {
        Tool::RunCommand { .. } => ports.exec.run(tool, clock).await,
        Tool::ReadRemoteFile { path, max_bytes } => {
            ports
                .exec
                .read_remote_file(
                    path,
                    max_bytes.unwrap_or(crate::exec::DEFAULT_MAX_OUTPUT_BYTES),
                )
                .await
        }
        Tool::ListRemoteDir { path } => ports.exec.list_remote_dir(path).await,
        Tool::SftpGet { remote, local_dest } => ports.exec.sftp_get(remote, local_dest).await,
        Tool::SftpPut {
            local_src,
            remote_dest,
        } => ports.exec.sftp_put(local_src, remote_dest).await,
        // 外部 MCP 工具（M3 出口 6）：转给挂载的外部 server。分级已在进门时由
        // `floor_at_write` 定好，这里只执行已放行的调用。
        Tool::ExternalMcp {
            server_id,
            tool,
            arguments_json,
        } => {
            ports
                .exec
                .external_mcp(server_id, tool, arguments_json)
                .await
        }
        // 控制流工具不会走到这里（Finish/AskUser 在上面的臂里处理）——但 match 要穷举。
        Tool::AskUser { .. } | Tool::Finish { .. } => crate::exec::Observation::default(),
    }
}

/// ask_user 的执行与回喂（streak 重置点 ② 在「被回答」臂）。
#[allow(clippy::too_many_arguments)]
async fn ask_and_feed(
    question: &str,
    conv: &mut crate::agent::history::Conversation,
    ledger: &mut BudgetLedger,
    ports: &Ports,
    base: impl Fn(Option<StepOutcome>) -> StepRecord,
) -> Result<(), StopReason> {
    // ask_user 发出本身留一行（request_only）
    let rec = StepRecord {
        outcome: Some(StepOutcome {
            audit_verdict: StepVerdict::RequestOnly,
            observation: None,
            digest: None,
        }),
        ..base(None)
    };
    audit(ports, rec).await?;
    match ports.confirm.ask_user(question).await {
        crate::agent::confirm::AskAnswer::Answered(text) => {
            // ← streak 重置点 ②：人回答了问题
            ledger.human_touched();
            conv.messages.push(ChatMessage::UserAnswer(text));
            Ok(())
        }
        crate::agent::confirm::AskAnswer::TimedOut => Err(StopReason::AskUnanswered),
        crate::agent::confirm::AskAnswer::ContextGone => Err(if ports.abort.requested() {
            StopReason::UserAbort {
                killed_in_flight: None,
            }
        } else {
            StopReason::SessionAbandoned
        }),
    }
}

/// 审计写入：失败即停（自主步无记录地继续是链上缺口）。
async fn audit(ports: &Ports, rec: StepRecord) -> Result<(), StopReason> {
    ports
        .audit
        .append(rec)
        .await
        .map_err(|e| StopReason::AuditWriteFailed { detail: e })
}

/// 执行观察值回喂模型：JSON 串过一遍 known_secrets 脱敏
/// （模型可能把屏幕上看过的口令抄进输出，而输出会被回喂/展示）。
fn feed_tool_result(
    call_id: &str,
    obs: &crate::exec::Observation,
    conv: &mut crate::agent::history::Conversation,
    known_secrets: &[String],
) {
    let json = serde_json::to_string(obs).unwrap_or_else(|_| "{}".to_string());
    let refs: Vec<&str> = known_secrets.iter().map(|s| s.as_str()).collect();
    let red = crate::redact::redact(&json, &refs);
    conv.messages.push(ChatMessage::ToolResult {
        call_id: call_id.to_string(),
        content: red.text,
        is_error: false,
    });
}

/// 一条工具的展示摘要（确认对话框与时间线用；构造即脱敏）。
fn display_of(tool: &Tool, known: &[String]) -> RedactedAction {
    let s = match tool {
        Tool::RunCommand { command, .. } => command.clone(),
        Tool::ReadRemoteFile { path, .. } => format!("read {path}"),
        Tool::ListRemoteDir { path } => format!("ls {path}"),
        Tool::SftpGet { remote, local_dest } => format!("get {remote} → {local_dest}"),
        Tool::SftpPut {
            local_src,
            remote_dest,
        } => format!("put {local_src} → {remote_dest}"),
        Tool::AskUser { question } => format!("问：{question}"),
        Tool::Finish { summary } => format!("完成：{summary}"),
        Tool::ExternalMcp {
            server_id, tool, ..
        } => format!("mcp[{server_id}] {tool}"),
    };
    RedactedAction::new(&s, known)
}

/// 工具的线上名（内置来自 manifest，外部 MCP 是运行期拼的 mcp__server__tool）。
fn manifest_name(tool: &Tool) -> String {
    tool.wire_name()
}

/// StepRecord 的 decision 字段：**从已有的 GateOutcome 推导**，不再调一次 decide。
///
/// 第一版这里写的是 `gate::decide(tier, mode)`——「只是为了填个展示字段」。
/// 自守测试当场红：那是 decide 的第二个生产调用点，而两个调用点就是两个裁决源，
/// 迟早在某次重构里给出不同答案（而不一致的方向一定是宽的那个赢）。
fn decision_of(outcome: &GateOutcome) -> fs_policy::Decision {
    match outcome {
        GateOutcome::Auto => fs_policy::Decision::AutoRun,
        GateOutcome::Confirm { strong: false } => fs_policy::Decision::Confirm,
        GateOutcome::Confirm { strong: true } => fs_policy::Decision::StrongConfirm,
        GateOutcome::Deny { reason, .. } => fs_policy::Decision::Deny(*reason),
    }
}

/// 票据 id：进程内单调。真源在 app 层（确认队列），这里给离线路径一个
/// 不冲突的起点；app 层实现自己的 id 时会用它自己的计数器。
fn next_ticket_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

// dispatch 是 crate 内唯一执行入口；ToolParseError 的 feed_message 消费点也在这。
#[allow(dead_code)]
fn _assert_feed_message_used(e: &ToolParseError) -> String {
    e.feed_message()
}

// clone_reason 复用自 run.rs（StopReason 两份臂表的同步由穷举保证）。
#[allow(dead_code)]
fn _reuse_clone(r: &StopReason) -> StopReason {
    clone_reason(r)
}

// ConfirmAnswer 的 ContextGone 分支消费在此文件（mod.rs 自守不扫这里，但
// 编译器保证穷举）。
#[allow(dead_code)]
fn _exhaustive(a: &ConfirmAnswer) -> u8 {
    match a {
        ConfirmAnswer::Answered(_) => 1,
        ConfirmAnswer::TimedOut => 2,
        ConfirmAnswer::ContextGone => 3,
    }
}
