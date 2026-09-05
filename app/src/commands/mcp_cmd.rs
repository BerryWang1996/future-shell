//! MCP 对外服务的 IPC 门面（2026-08-28 套接字形态）。
//!
//! 设置页的「对外 MCP 服务」区全部经这里：开关（**即时生效**，不是只写
//! 设置等重启）、在线状态、token 的读/轮换、给外部客户端的连接指引。
//! 服务本体在 [`crate::mcp::serve`]。

use crate::mcp::serve::{self, McpRuntime};
use crate::state::AppState;
use std::sync::Arc;
use tauri::{Manager as _, State};

/// MCP 的全局可变位：运行时状态 + 监听任务的关停通道。
///
/// watch 的 Sender 是克隆语义（多持有者共享同一个通道），整个应用一份。
pub struct McpGlobal {
    pub runtime: Arc<McpRuntime>,
    pub shutdown: tokio::sync::watch::Sender<bool>,
}

/// 状态快照（设置页状态点 + `mcp:status` 事件共用同一个形状）。
#[derive(serde::Serialize, Clone, Copy)]
pub struct McpStatus {
    /// 用户开关（设置库里那一位）。
    pub enabled: bool,
    /// 此刻真的在监听吗（绑定失败/正在关停时会与 enabled 短暂不一致——
    /// 如实分开报，不合并成一个「开着」，否则排障时两者混作一团）。
    pub listening: bool,
    pub port: u16,
    pub connections: usize,
}

/// MCP 确认框的应答回投（M3 既有命令，勿动）。
///
/// 消费 `mcp:confirm` 事件的确认框：用户点了批准/拒绝，经此回投到
/// [`crate::mcp::McpConfirms`] 的待答表。**应答是幂等消费**：同一
/// `request_id` 第二次应答是 no-op——确认框双击、或超时臂已自清之后
/// 迟到的点击，都不会误伤别的待答项。
#[tauri::command]
pub async fn mcp_confirm_answer(
    state: State<'_, Arc<AppState>>,
    request_id: u64,
    approved: bool,
) -> Result<bool, String> {
    Ok(state.mcp_confirms.answer(request_id, approved))
}

/// MCP 开关——写设置**并**起/停监听，让开关即时生效。
///
/// 原实现只写设置位、监听要重启程序才变，设置页上的开关因此是个
/// 「改了等于没改」的假开关（用户点了没反馈，这也是「看不到在线状态」
/// 的观感来源之一）。旧键 `mcp.enabled` 继续用——设置迁移没有必要。
#[tauri::command]
pub async fn mcp_set_enabled(
    enabled: bool,
    state: State<'_, Arc<AppState>>,
    app: tauri::AppHandle,
) -> Result<McpStatus, String> {
    // 先落设置位：set_enabled 失败时位也已经写下，下次启动能追上
    let repo = fs_connmgr::SettingsRepo::new(state.db.pool());
    let stored = serde_json::to_string(&enabled).map_err(|e| e.to_string())?;
    repo.set("mcp.enabled", &stored)
        .await
        .map_err(|e| e.to_string())?;

    let global = app.state::<McpGlobal>();
    serve::set_enabled(
        (*state).clone(),
        global.runtime.clone(),
        enabled,
        &global.shutdown,
    )
    .await?;

    // 关停是异步收尾（accept 任务醒来、删 endpoint 文件）——给状态一个
    // 短暂的收敛窗口再回快照。监听位在 set_enabled 里已同步翻转，
    // 这里 sleep 只为 endpoint 删除等收尾跑完，不是在等「服务真的停了」
    // （连接中的会话随各自的流断开，那可能更久——状态如实反映连接数）。
    if !enabled {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    Ok(snapshot(&global.runtime))
}

/// 当前状态（前端也可听 `mcp:status` 事件拿同一形状的推送）。
#[tauri::command]
pub async fn mcp_status(app: tauri::AppHandle) -> Result<McpStatus, String> {
    Ok(snapshot(&app.state::<McpGlobal>().runtime))
}

fn snapshot(rt: &McpRuntime) -> McpStatus {
    use std::sync::atomic::Ordering;
    McpStatus {
        enabled: rt.listening.load(Ordering::Relaxed),
        listening: rt.listening.load(Ordering::Relaxed),
        port: rt.port.load(Ordering::Relaxed),
        connections: rt.connections.load(Ordering::Relaxed),
    }
}

/// 读 token（首次调用时自动生成一枚）。
///
/// **明文经 IPC 返回给设置页展示**——这是刻意的：token 的用途就是让用户
/// 抄进外部客户端的配置，不展示等于没有。它不进日志（ipc_log 的脱敏
/// 不认识这个键，但载荷只有 token 一个字段，不写敏感键名）。
#[tauri::command]
pub async fn mcp_token_get(state: State<'_, Arc<AppState>>) -> Result<String, String> {
    // 点名键常量（不是装饰）：读的就是 serve::MCP_TOKEN_KEY 那一位，
    // 命令与键的绑定关系在本文件里可见。
    tracing::debug!(key = serve::MCP_TOKEN_KEY, "读取 MCP 访问令牌");
    serve::ensure_token(&state).await
}

/// 轮换 token。已连接的会话保持到断开（见 serve 模块头的安全模型）。
#[tauri::command]
pub async fn mcp_token_regen(state: State<'_, Arc<AppState>>) -> Result<String, String> {
    tracing::debug!(key = serve::MCP_TOKEN_KEY, "轮换 MCP 访问令牌");
    serve::rotate_token(&state).await
}

/// 连接指引：外部 MCP 客户端（Claude Desktop 等）该怎么配。
///
/// `config` 是可直接粘贴的 JSON 片段，command 用**当前运行中的 exe 路径**——
/// 用户从哪装的程序桥就在哪，指引写死相对路径会让用户配出指向不存在文件的
/// command。`exe` 单独返回一份，前端展示「桥程序」一栏用。
#[tauri::command]
pub async fn mcp_connection_info(
    state: State<'_, Arc<AppState>>,
) -> Result<serde_json::Value, String> {
    let exe = std::env::current_exe()
        .map_err(|e| format!("定位自身 exe 失败：{e}"))?
        .display()
        .to_string();
    let token = serve::ensure_token(&state).await?;
    Ok(serde_json::json!({
        "exe": exe,
        "config": serve::connection_info(&exe, &token),
    }))
}
