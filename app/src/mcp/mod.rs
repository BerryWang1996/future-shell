//! MCP Server 的 app 接线（M3 出口 5 的落地）。
//!
//! `fs_mcpbridge` 出策略核心与传输；本层把六个端口接到应用的**真实设施**上：
//! 会话注册表、终端网格、russh exec/sftp、审计库、确认队列。裁决逻辑一行都不在
//! 这里——全在 `fs_mcpbridge::server`（百余条测试钉死）。本层只做「翻译」，
//! 而翻译里唯一有裁量权的地方（`McpVerdict` → 审计枚举的映射）也有一张
//! 逐格写死的表加测试守着，见 [`ports::map_verdict`]。
//!
//! ## 默认关闭
//!
//! `serve_stdio` 只在 [`maybe_spawn`] 判定「设置里打开了 MCP」之后才被调用。
//! 本模块**不**在启动链路上无条件拉起任何传输——总设计 §4.5「默认关闭」。

mod ports;
pub mod serve;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use fs_connmgr::SettingsRepo;
use fs_mcpbridge::authz::Authorization;
use fs_mcpbridge::transport::McpServer;
use tokio::sync::oneshot;

use crate::state::AppState;

/// 设置键：MCP 总开关（`"true"` 才开，默认关）。
pub const MCP_ENABLED_KEY: &str = "mcp.enabled";
/// 设置键：授权给 MCP 客户端的工具名单（JSON 数组，元素为线上工具名）。
/// 空/缺失 ⇒ 一个都不授权——「默认关闭」落到工具面就是「默认无可枚举」。
pub const MCP_TOOLS_KEY: &str = "mcp.tools";
/// 设置键：调用方标签（进审计 `mcp:{client}`，展示用、非安全边界，§4.5）。
pub const MCP_CALLER_KEY: &str = "mcp.caller";
/// 设置键：外部 MCP server 挂载表（M3 出口 6），`[{server_id,command,args}]`。
pub const MCP_MOUNTS_KEY: &str = "mcp.mounts";

/// 按 `server_id` 从挂载表里取一个外部 server 的配置。
///
/// 返回 `Ok(None)` = 表里没有这个 id（调用方应报「找不到」而不是 panic）。
/// 配置解析失败只跳过坏条目、不整表作废——一条写错的挂载不该挡住别的。
pub async fn mount_config_for(
    db: &fs_connmgr::Db,
    server_id: &str,
) -> Result<Option<fs_mcpbridge::client::McpMountConfig>, String> {
    let raw = SettingsRepo::new(db.pool())
        .get(MCP_MOUNTS_KEY)
        .await
        .map_err(|e| e.to_string())?;
    let Some(raw) = raw else { return Ok(None) };
    let mounts: Vec<fs_mcpbridge::client::McpMountConfig> =
        serde_json::from_str(&raw).map_err(|e| format!("mcp.mounts 不是合法配置：{e}"))?;
    Ok(mounts.into_iter().find(|m| m.server_id == server_id))
}

/// 拉齐**全部**已挂载 server 的工具，翻成模型可见的形状（M3 出口 6）。
///
/// Agent 每次开跑前调一次。语义上的两条纪律：
/// - **现问现用，不缓存**：卸载一个 server 之后模型立刻看不到它的工具。
/// - **一个坏掉的挂载不阻断别的**：拉不起来只记 warn 并跳过。让整个 Agent
///   因为某个外部 server 装错了而起不来，是把一个可绕开的问题变成了硬故障。
pub async fn external_tools(state: &AppState) -> Vec<fs_ai::agent::history::ExternalToolSchema> {
    let raw = SettingsRepo::new(state.db.pool())
        .get(MCP_MOUNTS_KEY)
        .await
        .ok()
        .flatten();
    let Some(raw) = raw else { return Vec::new() };
    let mounts: Vec<fs_mcpbridge::client::McpMountConfig> =
        serde_json::from_str(&raw).unwrap_or_default();

    let mut out = Vec::new();
    for m in &mounts {
        match fs_mcpbridge::client::list_tools_once(m).await {
            Ok(tools) => {
                for (tool, description, input_schema_json) in tools {
                    out.push(fs_ai::agent::history::ExternalToolSchema {
                        server_id: m.server_id.clone(),
                        tool,
                        description,
                        input_schema_json,
                    });
                }
            }
            Err(e) => tracing::warn!(
                server_id = %m.server_id,
                error = %e,
                "外部 MCP server 的工具清单拉取失败，本次跳过它（其余挂载不受影响）"
            ),
        }
    }
    out
}

/// MCP 确认的待答表。与 Agent 的 [`crate::agent::confirm_port::AgentSupervisor`]
/// 分开：两边的票据形状、超时上限、应答语义都不同，塞进同一张表只会让
/// 「答错表」成为可能。
///
/// 只有 `answer` 能取走一枚待答项；超时臂在 [`McpConfirmPort`] 里自清。
#[derive(Default)]
pub struct McpConfirms {
    next_id: AtomicU64,
    pending: Mutex<HashMap<u64, oneshot::Sender<bool>>>,
}

impl McpConfirms {
    pub fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::SeqCst) + 1
    }
    pub fn insert(&self, id: u64, tx: oneshot::Sender<bool>) {
        self.pending.lock().unwrap().insert(id, tx);
    }
    /// 用户在 MCP 确认框上点了批准/拒绝。返回是否真的命中了一枚待答项。
    pub fn answer(&self, id: u64, approved: bool) -> bool {
        match self.pending.lock().unwrap().remove(&id) {
            Some(tx) => tx.send(approved).is_ok(),
            None => false,
        }
    }
    /// 超时/放弃时清掉，免得应答发到没人收的通道上。
    pub fn remove(&self, id: u64) {
        self.pending.lock().unwrap().remove(&id);
    }
}

/// 设置里读「开了没」。只有字面 `"true"` 算开——解析失败、缺键、其它值一律关。
/// 与 `security.clipboardClear` 同口径：**存疑落向关**，而不是落向开。
pub async fn is_enabled(state: &AppState) -> bool {
    SettingsRepo::new(state.db.pool())
        .get(MCP_ENABLED_KEY)
        .await
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str::<bool>(&s).ok())
        .unwrap_or(false)
}

/// 从设置拼出 [`Authorization`]。
///
/// 名单按 `fs_mcpbridge::tool::manifest_of` 过滤：**不存在的工具名进不了白名单**
///（`Authorization::new` 把它们归入 `unknown`），于是配置里写错名字最坏只是
/// 「没授权」，不是「授权了一个幽灵」。
async fn authorization_of(state: &AppState) -> Authorization {
    let names: Vec<String> = SettingsRepo::new(state.db.pool())
        .get(MCP_TOOLS_KEY)
        .await
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
        .unwrap_or_default();
    Authorization::new(names)
}

/// 客户端标签。缺省 `"mcp"`——它只进审计与确认框的「谁在问」一栏，
/// 不是安全边界（§4.5），所以缺一个值不危险，但**不能为空串**（空串会让
/// 审计里出现 `mcp:` 这种没有主体的发起方）。
async fn caller_of(state: &AppState) -> String {
    let label = SettingsRepo::new(state.db.pool())
        .get(MCP_CALLER_KEY)
        .await
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str::<String>(&s).ok())
        .unwrap_or_default();
    let label = label.trim().to_string();
    if label.is_empty() {
        "mcp".to_string()
    } else {
        label
    }
}

/// 从当下状态拼出可供 [`McpServer`] 使用的全部端口与上下文源。
///
/// 授权名单与调用方标签在这里**现读一次**：它们在一次 `serve` 的生命周期内不变
///（改设置要重启传输才生效，这与「默认关闭、显式启动」的模型一致）。
pub async fn build_server(state: Arc<AppState>) -> McpServer {
    let authz = authorization_of(&state).await;
    let caller = caller_of(&state).await;
    McpServer::new(
        Arc::new(ports::McpAppContext {
            state: state.clone(),
            caller,
        }),
        authz,
        Arc::new(ports::McpSessions {
            state: state.clone(),
        }),
        Arc::new(ports::McpTerminal {
            state: state.clone(),
        }),
        Arc::new(ports::McpCommand {
            state: state.clone(),
        }),
        Arc::new(ports::McpSftp {
            state: state.clone(),
        }),
        Arc::new(ports::McpConfirm {
            confirms: state.mcp_confirms.clone(),
            app: state.app.clone(),
        }),
        Arc::new(ports::McpAudit {
            db: state.db.clone(),
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `McpConfirms` 的应答语义：命中即消费、重复应答落空。
    ///
    /// 「重复应答落空」是重点：确认框若因双击发了两次 `answer`，第二次必须
    /// 是 no-op 而不是把另一个待答项顶掉。
    #[tokio::test]
    async fn an_answer_consumes_exactly_one_pending_item() {
        let c = McpConfirms::default();
        let id = c.next_id();
        let (tx, rx) = oneshot::channel();
        c.insert(id, tx);

        assert!(c.answer(id, true), "第一下应答须命中");
        assert!(rx.await.is_ok());
        assert!(!c.answer(id, false), "第二下应答不得再命中");
    }

    /// 超时清理后，迟到的应答不得命中。
    #[tokio::test]
    async fn a_removed_item_cannot_be_answered() {
        let c = McpConfirms::default();
        let id = c.next_id();
        let (tx, _rx) = oneshot::channel();
        c.insert(id, tx);
        c.remove(id);
        assert!(!c.answer(id, true));
    }
}
