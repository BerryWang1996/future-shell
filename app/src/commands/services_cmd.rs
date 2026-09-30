//! 系统面工具箱的 IPC（M7.1）：服务管理 + 补丁只读盘点。
//!
//! 解析全在 `fs_sshengine::{services, packages}`（纯函数、可单测）；这一层只做三件事：
//! 取 exec 通道、把命令送出去、把「命令整体失败」与「内容为空」分开报。
//!
//! # 出口标准④：每个改变系统状态的动作写审计
//!
//! 只有 [`session_service_action`] 会改远端状态，故只有它写审计——而且是在**拿到退出码之后**
//! 才写，记的是「这次到底成没成」。与 M7.2 确认闸写的那条是**两件事**：那条记「用户批准了」，
//! 这条记「批准之后真的执行了，结果是这样」。少任何一条，事后追查都缺一半。
//!
//! # 出口标准③：全程零新增出站面
//!
//! 本文件不引入任何 HTTP 客户端，命令一律经既有的 SSH exec 通道。
//! `frontend/src/lib/egress-contract.test.ts` 因此不需要任何改动。

use crate::state::AppState;
use fs_connmgr::audit_repo::{Actor, AuditRepo, NewAuditEntry, Verdict};
use fs_sshengine::packages::{self, PackageScan};
use fs_sshengine::services::{self, ServiceAction, ServiceListResult};
use std::sync::Arc;
use tauri::State;

/// 跑一条只读命令并把 `(code, stdout, stderr)` 拿回来。
async fn run(
    state: &AppState,
    session_id: &str,
    cmd: &str,
) -> Result<(Option<i32>, String, String), String> {
    let exec = state.exec_adapter_for(session_id).await?;
    let o = exec.exec_once(cmd).await.map_err(|e| e.to_string())?;
    Ok((o.code, o.stdout, o.stderr))
}

/// 「命令整体没跑起来」与「跑了但没内容」是两回事：前者要把 stderr 带出去，
/// 后者是正常的空结果。判据同 `monitor_cmd`：退出码非零**且**一点 stdout 都没有。
fn fail_if_dead(code: Option<i32>, stdout: &str, stderr: &str, what: &str) -> Result<(), String> {
    if code != Some(0) && stdout.trim().is_empty() {
        return Err(format!(
            "{what}执行失败（exit {}）：{}",
            code.map(|c| c.to_string())
                .unwrap_or_else(|| "未观测到".into()),
            stderr.trim()
        ));
    }
    Ok(())
}

/// 服务列表（只读）。返回值区分「没有 systemd」与「有 systemd 但列表为空」。
#[tauri::command]
pub async fn session_services(
    session_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<ServiceListResult, String> {
    let (code, stdout, stderr) =
        run(&state, &session_id, services::list_services_command()).await?;
    fail_if_dead(code, &stdout, &stderr, "服务列表命令")?;
    Ok(services::parse_service_list(&stdout))
}

/// 单个单元的状态原文（只读）。命令自带 `|| true`，故不判退出码。
#[tauri::command]
pub async fn session_service_status(
    session_id: String,
    unit: String,
    state: State<'_, Arc<AppState>>,
) -> Result<String, String> {
    let cmd = services::status_command(&unit).map_err(|e| e.to_string())?;
    let (_, stdout, _) = run(&state, &session_id, &cmd).await?;
    Ok(stdout)
}

/// 单个单元的最近日志（只读）。
#[tauri::command]
pub async fn session_service_journal(
    session_id: String,
    unit: String,
    lines: u32,
    state: State<'_, Arc<AppState>>,
) -> Result<String, String> {
    let cmd = services::journal_command(&unit, lines).map_err(|e| e.to_string())?;
    let (_, stdout, _) = run(&state, &session_id, &cmd).await?;
    Ok(stdout)
}

/// 一次服务动作的结果。
#[derive(Debug, serde::Serialize)]
pub struct ServiceActionResult {
    pub ok: bool,
    /// 成功时是空串或 systemctl 的少量输出；失败时是**能行动**的一句话
    /// （`classify_action_failure`）或原始 stderr。
    pub message: String,
}

/// 启 / 停 / 重启一个服务。**这是本模块唯一会改远端状态的命令。**
///
/// 前端调它之前必须先过 M7.2 的确认闸（`lib/confirm-gate.ts`）。这里**不**再弹一次确认——
/// 两层确认会让用户点两遍，而第二遍他已经不看了；闸在前端是因为「命令全文摊给用户看」
/// 这件事只有前端做得到。
#[tauri::command]
pub async fn session_service_action(
    session_id: String,
    unit: String,
    action: ServiceAction,
    state: State<'_, Arc<AppState>>,
) -> Result<ServiceActionResult, String> {
    let cmd = services::action_command(action, &unit).map_err(|e| e.to_string())?;
    let (code, stdout, stderr) = run(&state, &session_id, &cmd).await?;
    let ok = code == Some(0);
    let message = if ok {
        stdout.trim().to_string()
    } else {
        services::classify_action_failure(code, &stderr).unwrap_or_else(|| {
            let s = stderr.trim();
            if s.is_empty() {
                format!(
                    "命令失败（exit {}），远端没有给出原因",
                    code.map(|c| c.to_string())
                        .unwrap_or_else(|| "未观测到".into())
                )
            } else {
                s.to_string()
            }
        })
    };

    // 出口标准④：改变系统状态的动作写审计。**拿到退出码之后**才写，记的是真实结果。
    // 写失败只 warn（口径同 audit_command_sent）：把一次已经执行完的动作报成失败，
    // 用户会再点一次——那才是真的多重启了一遍。
    // 命令文本照样过脱敏。单元名已过白名单校验、命令本身也不含秘密，但
    // `every_audit_writer_routes_through_redact` 那条门禁是**无条件**的——它守的正是
    // 「这一条我看过、不含秘密」这种逐处自辩：自辩一旦被接受，下一个写入方就会照抄这句话，
    // 而那一次可能真的带着口令。审计行永远留在库里并随取证包离开本机。
    let known = crate::commands::ai_cmd::known_secrets(&state).await;
    let display: fs_ai::agent::record::RedactedAction =
        fs_ai::agent::record::RedactedAction::new(&cmd, &known);
    let entry = NewAuditEntry {
        actor: Actor::User,
        action: format!("[服务] {}", display.as_str()),
        target_session: Some(session_id.clone()),
        risk_level: fs_policy::classify(&cmd).tier.as_str().to_string(),
        verdict: if ok {
            Verdict::Approved
        } else {
            Verdict::Rejected
        },
        exit_code: code.map(i64::from),
        output_digest: None,
        created_at: crate::commands::ai_cmd::now_rfc3339(),
    };
    if let Err(e) = AuditRepo::new(state.db.pool()).append(&entry).await {
        tracing::warn!(error = %e, "服务动作的审计行写入失败（动作本身已执行）");
    }
    Ok(ServiceActionResult { ok, message })
}

/// 补丁盘点（**只读**，不执行升级——理由见 `fs_sshengine::packages` 的模块头注）。
#[tauri::command]
pub async fn session_packages(
    session_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<PackageScan, String> {
    let (code, stdout, stderr) = run(&state, &session_id, packages::scan_command()).await?;
    fail_if_dead(code, &stdout, &stderr, "补丁盘点命令")?;
    Ok(packages::parse_scan(&stdout))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 本文件的**生产段**（在 `#[cfg(test)]` 处截断）。
    ///
    /// 下面两条守卫扫的是「本文件里有没有出现某某字符串」，而 `include_str!` 会把测试模块
    /// 自己也读进来——禁用词表就写在测试里，不截断的话这两条**恒红**（首版正是如此）。
    /// 同一处坑在 `vault_cmd.rs` 的 `production_src()` 上踩过一次，口径照抄。
    fn production_src() -> &'static str {
        const RAW: &str = include_str!("services_cmd.rs");
        let cut = RAW
            .find("\n#[cfg(test)]")
            .expect("本文件必须有 #[cfg(test)] 段");
        &RAW[..cut]
    }

    #[test]
    fn dead_command_is_reported_but_empty_output_is_not() {
        // 退出码非零且零 stdout = 命令没跑起来，要把 stderr 带出去
        let e = fail_if_dead(
            Some(127),
            "",
            "bash: systemctl: command not found",
            "服务列表命令",
        )
        .unwrap_err();
        assert!(e.contains("command not found"), "{e}");
        assert!(e.contains("exit 127"), "{e}");
        // 有 stdout 就不算「没跑起来」——内容为空由解析器落成空态
        assert!(fail_if_dead(Some(1), "SYSTEMD=0\n", "warn", "x").is_ok());
        assert!(fail_if_dead(Some(0), "", "", "x").is_ok());
    }

    /// 出口标准③：本文件不得引入任何出站面。
    #[test]
    fn no_outbound_surface_in_this_file() {
        let src = production_src();
        for forbidden in ["reqwest", "http://", "https://", "TcpStream", "ureq"] {
            assert!(
                !src.contains(forbidden),
                "系统面工具箱不得新增出站面，出现了 {forbidden:?}"
            );
        }
    }

    /// 只有服务动作那一条写审计——只读命令写审计会把审计链灌满噪声，
    /// 而噪声里的一条真事件等于没记。
    #[test]
    fn only_the_state_changing_command_writes_audit() {
        let src = production_src();
        let n = src.matches("NewAuditEntry {").count();
        assert_eq!(n, 1, "审计条目数不是 1：只读命令不该写审计，改状态的必须写");
        // 且它在 session_service_action 里
        let at = src.find("pub async fn session_service_action").unwrap();
        let entry_at = src.find("NewAuditEntry {").unwrap();
        assert!(entry_at > at, "审计不在 session_service_action 里");
    }
}
