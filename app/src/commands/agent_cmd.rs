//! Agent 的 IPC 面（实现规格 §2 的 `agent_cmd`）。
//!
//! 五个命令：启动 / 急停 / 应答确认 / 回答提问 / 查状态。
//!
//! # 启动是异步的，但拒绝是同步的
//!
//! `agent_start` **先**做启动期检查（provider 有效、档位非 Disabled、会话在），
//! 检查不过就同步返回错误——那时 run 还不存在，用户看到的是「为什么起不来」
//! 而不是「起来了又立刻停了」。检查过了才 spawn 后台 run 并立刻返回 run_id：
//! 一次 run 可能跑几分钟，IPC 不能挂着。
//!
//! # 启动期装配复用 `ai_cmd::prepare`
//!
//! provider 选取与档位读取只有一份实现——两份就是两套「没配置怎么办」的答案，
//! 而那两套迟早分叉（M2 的 read_mode 已经是「缺键 → 默认档、脏值 → Disabled」
//! 这样一个不对称回退，抄第二份必错）。

use crate::agent::agent_audit::DbAuditPort;
use crate::agent::confirm_port::UiConfirmPort;
use crate::agent::exec_port::RusshExecPort;
use crate::agent::model_port::HttpModelPort;
use crate::ai_mode::ai_mode_of;
use crate::state::AppState;
use fs_ai::agent::budget::{BudgetLedger, Budgets};
use fs_ai::agent::gate::{effective_mode, ExternalToolTiers};
use fs_ai::agent::ports::{ChannelRegistry, RunClock};
use fs_ai::agent::run::{initial_conversation, run_loop, Ports};
use serde::Serialize;
use std::sync::Arc;
use tauri::{Emitter, State};

/// `agent_start` 的返回。
#[derive(Debug, Clone, Serialize)]
pub struct StartedRun {
    pub run_id: String,
}

/// run 结束事件（`agent:stopped`）。
#[derive(Debug, Clone, Serialize)]
pub struct RunFinished {
    pub run_id: String,
    /// 停因的一句话（`StopReason::summary_line`——UI、报告、RunEnd 行同一份文本）。
    pub reason: String,
    pub steps_taken: u32,
    pub streak: u32,
    pub tokens_known: u64,
    pub tokens_unaccounted: u32,
    pub tokens_charged: u64,
    pub human_touchpoints: u32,
}

/// 启动一次 Agent run。
#[tauri::command]
pub async fn agent_start(
    state: State<'_, Arc<AppState>>,
    task: String,
    session_id: String,
) -> Result<StartedRun, String> {
    let task = task.trim().to_string();
    if task.is_empty() {
        return Err("任务描述不能为空".into());
    }
    // ── 启动期检查（复用 M2 的装配：provider + 档位 + 上下文）──────────
    // screen=None：Agent 的起始上下文不带屏幕内容——它自己会发只读命令去看。
    let (cfg, ctx, global_mode) =
        crate::commands::ai_cmd::prepare(&state, None, Some(&session_id)).await?;

    // 会话级覆盖：Off = 未表态 ⇒ 不覆盖（见 ai_mode.rs 的理由）
    let session_override = session_auto_exec(&state, &session_id)
        .await
        .and_then(ai_mode_of);
    let mode = effective_mode(global_mode, session_override);
    if mode == fs_policy::AiMode::Disabled {
        // prepare 已挡过全局 Disabled；这条挡的是「会话覆盖成 Disabled」。
        // 不浪费一次已付费的模型回合。
        return Err("这个会话的 AI 档位是关闭（连接属性 → AI 策略）".into());
    }
    let session = state
        .registry
        .get(&session_id)
        .ok_or_else(|| "会话不在（可能已断开）".to_string())?;

    // ── 装配 ────────────────────────────────────────────────────────
    let run_id = uuid::Uuid::new_v4().to_string();
    let abort = state.agents.register(&run_id);
    let registry = Arc::new(ChannelRegistry::default());
    let known = crate::commands::ai_cmd::known_secrets(&state).await;

    let app = state.app.clone();
    let emit_app = app.clone();
    let ports = Ports {
        model: Arc::new(HttpModelPort::new(abort.clone())),
        exec: Arc::new(RusshExecPort::new(
            session,
            registry.clone(),
            state.db.clone(),
        )),
        confirm: Arc::new(UiConfirmPort {
            supervisor: state.agents.clone(),
            abort: abort.clone(),
            run_id: run_id.clone(),
            sink: crate::agent::confirm_port::EventSink(Arc::new(move |name, payload| {
                let _ = emit_app.emit(name, payload);
            })),
        }),
        audit: Arc::new(DbAuditPort::new(
            state.db.pool().clone(),
            Some(session_id.clone()),
            known.clone(),
        )),
        registry,
        abort: abort.clone(),
    };
    let clock = RunClock::new(abort.clone());
    let supervisor = state.agents.clone();
    let pool = state.db.pool().clone();
    let rid = run_id.clone();

    // RunBegin 行（簿记行属装配层——run_loop 看不到 run id）。
    // 写不进去就**不启动**：一次没有起点的 run 在链上是无根的，而 Agent 的
    // 每一步都要求可审。与 AuditWriteFailed 的停机口径一致。
    let begin = fs_connmgr::audit_repo::NewAuditEntry {
        actor: fs_connmgr::audit_repo::Actor::Agent,
        action: format!(
            "[run {rid}] begin: {}",
            fs_ai::agent::record::RedactedAction::new(&task, &known).as_str()
        ),
        target_session: Some(session_id.clone()),
        risk_level: fs_policy::Tier::ReadOnly.as_str().to_string(),
        verdict: fs_connmgr::audit_repo::Verdict::RequestOnly,
        exit_code: None,
        output_digest: None,
        created_at: crate::commands::ai_cmd::now_rfc3339(),
    };
    if let Err(e) = fs_connmgr::audit_repo::AuditRepo::new(&pool)
        .append(&begin)
        .await
    {
        supervisor.unregister(&rid);
        return Err(format!("审计起始行写入失败，未启动：{e}"));
    }

    // 已挂载的外部 MCP 工具（M3 出口 6）。**在 spawn 之前现拉一次**：
    // 一来 state 是 Tauri 的借用、进不了 spawn；二来语义上也该在这儿——
    // 工具集与档位、预算一样，开跑那一刻定格，跑到一半不变。
    // 拉不起来的挂载只记 warn 并跳过（见 external_tools）。
    let external = crate::mcp::external_tools(&state).await;

    // ── 后台跑 ──────────────────────────────────────────────────────
    tokio::spawn(async move {
        // 起始对话 = 系统提示词 + 任务 + M2 已组装好的上下文（profile 元信息
        // 与连接期采集的 host facts）。上下文进第一条 user 消息——Agent 不为它
        // 单开一次 exec（HOST_FACTS_COMMAND 含 $() 替换，classify 会判 Write 以上，
        // with_confirm 下每次启动都要点一次确认，确认疲劳会侵蚀人触点的质量）。
        let mut conv = initial_conversation(&format!("{task}\n\n{}", ctx.text));
        let mut ledger = BudgetLedger::new(Budgets::defaults(), abort);
        let report = run_loop(
            &mut conv,
            cfg,
            &mut ledger,
            &ports,
            &clock,
            mode,
            &ExternalToolTiers::default(),
            &known,
            &external,
        )
        .await;
        let b = report.budget;
        let _ = app.emit(
            "agent:stopped",
            RunFinished {
                run_id: rid.clone(),
                reason: report.stop.summary_line(),
                steps_taken: b.steps_taken,
                streak: b.streak,
                tokens_known: b.tokens.known(),
                tokens_unaccounted: b.tokens.unaccounted_turns(),
                tokens_charged: b.tokens.conservative_total(),
                human_touchpoints: b.human_touchpoints,
            },
        );
        supervisor.unregister(&rid);
    });

    Ok(StartedRun { run_id })
}

/// 取会话所属 profile 的 AI 档位设置。查不到就是「未表态」。
async fn session_auto_exec(
    state: &AppState,
    session_id: &str,
) -> Option<fs_connmgr::model::AutoExec> {
    let session = state.registry.get(session_id)?;
    let profiles = fs_connmgr::ProfileRepo::new(state.db.pool())
        .list()
        .await
        .ok()?;
    profiles
        .iter()
        .find(|p| p.id.to_string() == session.profile_id)
        .map(|p| p.ai_policy.auto_execute)
}

/// 急停。返回 false = 那个 run 已经结束了（前端据此更新按钮态，而不是显示
/// 一个「停止中…」永远转下去）。
#[tauri::command]
pub async fn agent_abort(state: State<'_, Arc<AppState>>, run_id: String) -> Result<bool, String> {
    Ok(state.agents.abort(&run_id))
}

/// 应答一条确认。返回 false = 这条已不在等（超时/撤回/重复点击）。
#[tauri::command]
pub async fn agent_confirm_answer(
    state: State<'_, Arc<AppState>>,
    request_id: u64,
    approved: bool,
) -> Result<bool, String> {
    Ok(state.agents.answer_confirm(request_id, approved))
}

/// 回答一条提问。
#[tauri::command]
pub async fn agent_ask_reply(
    state: State<'_, Arc<AppState>>,
    request_id: u64,
    text: String,
) -> Result<bool, String> {
    Ok(state.agents.answer_ask(request_id, text))
}

/// 查一个 run 还在不在（前端刷新后恢复按钮态）。
#[tauri::command]
pub async fn agent_is_running(
    state: State<'_, Arc<AppState>>,
    run_id: String,
) -> Result<bool, String> {
    Ok(state.agents.is_running(&run_id))
}

#[cfg(test)]
mod tests {
    /// 接线守卫：五个命令都要注册进 invoke_handler。
    ///
    /// 与 audit_cmd 的同名守卫同一形态——「功能做完入口没接」在本仓发生过五次。
    #[test]
    fn agent_commands_are_registered() {
        let lib = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lib.rs"),
        )
        .expect("读不到 lib.rs");
        for cmd in [
            "agent_start",
            "agent_abort",
            "agent_confirm_answer",
            "agent_ask_reply",
            "agent_is_running",
        ] {
            assert!(
                lib.contains(&format!("commands::agent_cmd::{cmd}")),
                "{cmd} 没注册进 invoke_handler"
            );
        }
    }
}
