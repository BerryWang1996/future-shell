//! stdio 传输层：把 rmcp 的 `tools/list` / `tools/call` 桥到 [`crate::server`]。
//!
//! 这一层**不做任何裁决**——分级、授权、确认、审计全在 `server::call` 里，
//! 这里只负责三件事：
//!
//! 1. 把 [`Authorization::enumerable`] 过滤出的清单翻成 rmcp 的 `Tool`；
//! 2. 把 `tools/call` 的参数序列化成 `server::call` 要的 JSON 字符串；
//! 3. 把 [`crate::server::CallOutput`] 翻成 MCP 回包——**拒绝走
//!    `structured_error`（工具级，用户可见），不走 `Err(ErrorData)`（协议级，
//!    客户端渲染成不透明提示）**。这是 rmcp spike 定下的错误分路：走反了，
//!    用户读不到 `FS_POLICY_*` 拒绝原因，也就无从知道下一步该改档位还是改配置。
//!
//! ## 传输面没有地址
//!
//! `serve_stdio` 只接 `transport::io::stdio()`——stdin/stdout，不监听任何端口。
//! 这是总设计 §4.5「stdio 为 M3 阶段唯一传输（传输层无监听地址）」的字面实现。
//! 将来若要开 HTTP，是另一条需要显式决策与门禁放行（仅 127.0.0.1）的路。

use std::sync::Arc;

use rmcp::{
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
        InitializeResult, JsonObject, ListToolsResult, PaginatedRequestParams, ServerCapabilities,
        Tool,
    },
    service::{RequestContext, RoleServer},
    ErrorData, ServerHandler, ServiceExt,
};

use crate::authz::{Authorization, OpenLedger};
use crate::ports::{
    CommandPort, McpAuditPort, McpConfirmPort, SessionDirectory, SftpPort, TerminalPort,
};
use crate::server::{list_tools, CallContext, CallOutput, Ports};

/// 每次调用前给出**当下**的上下文（全局档位 / 已知机密 / 客户端标签）。
///
/// 抽成 trait 是因为这三样都随运行态变：档位可能被用户在设置里改，机密集随
/// Vault 解锁状态变。传输层不该自己缓存它们——缓存一份过期的档位，等于让一次
/// 已被用户收紧的授权继续放行。由 app 层实现，每次现读。
///
/// **异步**：读档位要走 SQLite、读机密要拿 Vault 锁，都是 await。做成手写
/// boxed future（与 `ports.rs` 同口径），保住 `Arc<dyn McpContextSource>` 的
/// dyn 兼容。
pub trait McpContextSource: Send + Sync {
    fn current<'a>(
        &'a self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = CallContext> + Send + 'a>>;
}

/// 对外 MCP Server（stdio）。持有全部端口的 `Arc`，每次调用现拼一个借用的
/// [`Ports`] 喂给 `server::call`——端口是长寿命的，`Ports<'_>` 是按次借用的。
pub struct McpServer {
    ctx_source: Arc<dyn McpContextSource>,
    authz: Authorization,
    /// `server::call` 要 `&mut OpenLedger`；stdio 上多个请求可能并发到达，故加锁。
    /// 用 `tokio::sync::Mutex` 而非 std 的：守卫要**跨过** `server::call` 的 await，
    /// std 锁守卫非 Send，会让整个 `call_tool` 的 future 不满足 `MaybeSendFuture`。
    ledger: tokio::sync::Mutex<OpenLedger>,
    sessions: Arc<dyn SessionDirectory>,
    terminal: Arc<dyn TerminalPort>,
    command: Arc<dyn CommandPort>,
    sftp: Arc<dyn SftpPort>,
    confirm: Arc<dyn McpConfirmPort>,
    audit: Arc<dyn McpAuditPort>,
}

#[allow(clippy::too_many_arguments)]
impl McpServer {
    pub fn new(
        ctx_source: Arc<dyn McpContextSource>,
        authz: Authorization,
        sessions: Arc<dyn SessionDirectory>,
        terminal: Arc<dyn TerminalPort>,
        command: Arc<dyn CommandPort>,
        sftp: Arc<dyn SftpPort>,
        confirm: Arc<dyn McpConfirmPort>,
        audit: Arc<dyn McpAuditPort>,
    ) -> Self {
        Self {
            ctx_source,
            authz,
            ledger: tokio::sync::Mutex::new(OpenLedger::new()),
            sessions,
            terminal,
            command,
            sftp,
            confirm,
            audit,
        }
    }
}

impl ServerHandler for McpServer {
    /// 只声明 tools 能力。**不覆写 `initialize`**——rmcp 默认实现会调这里并自动
    /// 协商协议版本；自己写反而容易漏协商。
    fn get_info(&self) -> InitializeResult {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(
                "FutureShell",
                env!("CARGO_PKG_VERSION"),
            ))
    }

    /// `tools/list`：**从授权层过滤后的清单**生成，不另写一份。未授权工具在这里
    /// 就不可枚举（M3 出口第 5 项）。
    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let mut tools = Vec::new();
        for m in list_tools(&self.authz) {
            let schema: JsonObject = serde_json::from_str(m.schema_json).map_err(|e| {
                ErrorData::internal_error(format!("工具 {} 的 schema 非法：{e}", m.name), None)
            })?;
            tools.push(Tool::new(m.name, m.description, schema));
        }
        Ok(ListToolsResult::with_all_items(tools))
    }

    /// `tools/call`：序列化参数 → `server::call` → 翻回包。
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        // rmcp 的 arguments 是 Option<Map>；`server::call` 要 JSON 字符串。
        // 缺省按空对象处理（无参工具客户端常不发 arguments）。
        let args_json = match &request.arguments {
            Some(args) => serde_json::to_string(args).map_err(|e| {
                ErrorData::invalid_params(format!("arguments 无法序列化：{e}"), None)
            })?,
            None => "{}".to_string(),
        };

        let ctx = self.ctx_source.current().await;
        // 锁只罩住这一次调用；拿到结果立刻放，别在持有锁期间做别的。
        let out = {
            let mut ledger = self.ledger.lock().await;
            let ports = Ports {
                sessions: &*self.sessions,
                terminal: &*self.terminal,
                command: &*self.command,
                sftp: &*self.sftp,
                confirm: &*self.confirm,
                audit: &*self.audit,
            };
            crate::server::call(
                &request.name,
                &args_json,
                &ctx,
                &self.authz,
                &mut ledger,
                &ports,
            )
            .await
        };
        Ok(to_response(out))
    }
}

/// 把 [`CallOutput`] 翻成 MCP 回包。**错误分路见模块头。**
///
/// 成功一支：带结构化补充（`command.run` 的 Observation）就用 `structured`
/// ——它把脱敏后的整个值同时放进 `content`（给模型读的 JSON 文本）与
/// `structured_content`（给程序读的），exit_code/truncated/timed_out 一个不丢。
/// 纯文本工具用 `success` 走显式脱敏文本。
fn to_response(out: CallOutput) -> CallToolResponse {
    match out {
        CallOutput::Ok { text, structured } => match structured {
            Some(value) => CallToolResult::structured(value).into(),
            None => CallToolResult::success(vec![ContentBlock::text(text)]).into(),
        },
        // 拒绝/超时/中止：ToolError 序列化出 {isError, code, tier, confirmation_required}，
        // 经 structured_error 交给调用方。绝不能换成 Err(ErrorData)——那是协议级错误，
        // 客户端渲染成不透明提示，用户读不到拒绝原因。
        CallOutput::Err(e) => {
            let value = serde_json::to_value(&e)
                .unwrap_or_else(|_| serde_json::json!({ "message": e.message }));
            CallToolResult::structured_error(value).into()
        }
    }
}

/// 在 stdio 上跑起来并常驻，直到连接关闭。
///
/// `serve` 会先阻塞等客户端的 `initialize` 握手；`waiting` 常驻到对端断开。
/// 返回的 `Result` 只有「握手失败 / 任务异常」才非 `Ok`——对端正常断开不是错误。
///
/// **调用方（app 层）决定何时调它**：MCP Server 默认关闭，只有用户在设置里打开
/// 且传输被显式启动，这里才会被调用。本函数自己不做开关判断。
pub async fn serve_stdio(server: McpServer) -> Result<(), String> {
    let service = server
        .serve(rmcp::transport::io::stdio())
        .await
        .map_err(|e| format!("MCP stdio 握手失败：{e}"))?;
    service
        .waiting()
        .await
        .map_err(|e| format!("MCP stdio 服务异常退出：{e}"))?;
    Ok(())
}

/// 在一条**已通过鉴权**的连接上跑 MCP 会话（localhost 套接字形态，2026-08-28）。
///
/// # 为什么 stdio 形态换成了「localhost 套接字 + 桥」
///
/// 原形态是「外部客户端把本程序当子进程拉起、在其 stdio 上对话」。它在
/// GUI 程序上有一个**结构性的死结**：本程序有单实例闸（`fs_vault::lock_data_dir`，
/// 排在一切启动动作之前），于是主程序正开着时，被拉起的第二个进程**先死在闸上**，
/// MCP 连接根本建立不起来；反过来，外部客户端先拉起、用户再双击主程序，
/// 用户自己吃到「另一个实例正在使用同一数据目录」。也就是说原形态只在
/// 「用户恰好没开主程序」这一窗口里可用——设置里那个开关承诺的东西实际兑现不了。
///
/// 新形态：主程序常驻监听 `127.0.0.1:<随机端口>`（只绑回环，不出本机）；
/// 外部客户端的配置里用 `future-shell-app.exe --mcp-bridge` 作为 command——
/// 桥进程不做任何初始化、不开窗口、不碰单实例闸，只把它的 stdio 与主程序的
/// 套接字做字节中继。这样主程序开着也能连，多个客户端各连各的。
///
/// # 鉴权在哪一侧
///
/// **不在本函数**。套接字的 token 闸在 app 层的 accept 循环里（要先于 MCP
/// 握手完成），通过后才调到这里。传输层不掺和安全决策——与 stdio 形态
/// 「调用方决定何时起服务」是同一条分工。
///
/// 每条连接各建一个 [`McpServer`]：`OpenLedger`（预算账本）是 per-client 的
/// 会话状态，多客户端共用一本账会把 A 的 token 预算记到 B 头上。
pub async fn serve_stream<S>(server: McpServer, stream: S) -> Result<(), String>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin + 'static,
{
    let service = server
        .serve(stream)
        .await
        .map_err(|e| format!("MCP 连接握手失败：{e}"))?;
    service
        .waiting()
        .await
        .map_err(|e| format!("MCP 连接服务异常退出：{e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reject::ToolError;
    use fs_policy::Tier;

    /// 拒绝必须落在**工具级错误**（`is_error=true` + 结构化码），不是协议级。
    ///
    /// 这条钉的是错误分路本身：若哪天有人把 `Err` 一支改成 `Err(ErrorData)`，
    /// 用户就读不到 `FS_POLICY_*` 拒绝原因了——而那是 §4.5 确认契约的全部落点。
    #[test]
    fn a_rejection_becomes_a_tool_level_structured_error_not_a_protocol_error() {
        let e = ToolError::denied_by_gate(Tier::Dangerous, "只读档");
        let resp = to_response(CallOutput::Err(e));
        match resp {
            CallToolResponse::Complete(result) => {
                assert_eq!(result.is_error, Some(true), "拒绝必须 isError=true");
                let sc = result.structured_content.expect("要带结构化码");
                assert_eq!(sc["code"], "FS_POLICY_DENIED");
                assert_eq!(sc["tier"], "dangerous");
                assert_eq!(sc["confirmation_required"], false);
            }
            other => panic!("拒绝该是 Complete(CallToolResult)，实得 {other:?}"),
        }
    }

    /// 成功带结构化（command.run）⇒ `structured` 同时给 content 与 structured_content。
    #[test]
    fn a_structured_success_carries_the_value_in_both_channels() {
        let value = serde_json::json!({"stdout":"ok","stderr":"","exit_code":0,"truncated":false,"timed_out":false});
        let resp = to_response(CallOutput::Ok {
            text: "ok".into(),
            structured: Some(value.clone()),
        });
        match resp {
            CallToolResponse::Complete(result) => {
                assert_eq!(result.is_error, Some(false));
                assert_eq!(result.structured_content, Some(value));
                // content 文本是值的 JSON 序列化——模型据此读到 exit_code 等全字段。
                assert!(!result.content.is_empty());
            }
            other => panic!("该是 Complete，实得 {other:?}"),
        }
    }

    /// 成功无结构化（纯文本工具）⇒ 走显式文本。
    #[test]
    fn a_plain_success_uses_the_explicit_text() {
        let resp = to_response(CallOutput::Ok {
            text: "file body".into(),
            structured: None,
        });
        match resp {
            CallToolResponse::Complete(result) => {
                assert_eq!(result.is_error, Some(false));
                assert!(result.structured_content.is_none());
            }
            other => panic!("该是 Complete，实得 {other:?}"),
        }
    }
}
