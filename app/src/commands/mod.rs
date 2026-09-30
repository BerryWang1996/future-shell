pub mod agent_cmd;
pub mod ai_cmd;
pub mod audit_cmd;
pub mod audit_exec_cmd;
pub mod auth_cmd;
pub mod clipboard_cmd;
pub mod conn_cmd;
pub mod credential_cmd;
pub mod editor_cmd;
pub mod fileops_cmd;
pub mod history_cmd;
pub mod import_cmd;
pub mod key_cmd;
pub mod mcp_cmd;
pub mod monitor_cmd;
pub mod opener_cmd;
pub mod rdp_cmd;
pub mod record_cmd;
pub mod schedule_cmd;
pub mod serial_cmd;
pub mod services_cmd;
pub mod session_cmd;
pub mod settings_cmd;
pub mod sftp_cmd;
pub mod tunnel_cmd;
pub mod update_cmd;
pub mod vault_cmd;
pub mod window_cmd;
pub mod zmodem_cmd;

#[tauri::command]
pub async fn ping() -> Result<String, String> {
    Ok("pong".into())
}

/// 同一条前端错误的最短重复上报间隔（S300）。
///
/// 前端异常最爱的形状是「每帧一次」——`term:data` 监听器里抛一次就是每 16 ms 一条。
/// 无节流会在几秒内把 8 MiB 日志上限撑爆、把出事前的现场卷进 `.old` 再被覆盖掉
/// （正是 Task 57 给 term_ack 降噪要解决的同一个问题）。按「消息文本」去重而非
/// 按次数限流：不同错误互不遮蔽，同一错误只留首末观感。
const FRONTEND_ERROR_DEDUP_WINDOW: std::time::Duration = std::time::Duration::from_secs(60);

/// 前端未捕获异常上报（`window.onerror` / `unhandledrejection` / 显式 catch）。
///
/// 存在理由：改造前整个前端**零**全局错误处理器，`ctrl?.write()` 的可选链短路、
/// `void invoke(...)` 吞掉的 rejection、`decodeB64` 的同步抛——三条都会静默丢帧，
/// 而丢帧在旧 ack 口径下等于永久冻结。2026-08-19 事故复盘时无法区分这三个出口，
/// 正是因为它们全都不留痕（S300）。
#[tauri::command]
pub async fn log_frontend_error(
    source: String,
    message: String,
    state: tauri::State<'_, std::sync::Arc<crate::state::AppState>>,
) -> Result<(), String> {
    let now = std::time::Instant::now();
    let mut seen = state
        .frontend_error_seen
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    // 顺带清理过期条目：无界 map 本身就是内存泄漏面（错误文本可含变量，基数无上限）。
    seen.retain(|_, at: &mut std::time::Instant| {
        now.duration_since(*at) < FRONTEND_ERROR_DEDUP_WINDOW
    });
    let key = format!("{source}\u{1}{message}");
    if seen.contains_key(&key) {
        return Ok(());
    }
    seen.insert(key, now);
    drop(seen);
    tracing::error!(source = %source, message = %message, "前端未捕获异常");
    Ok(())
}
