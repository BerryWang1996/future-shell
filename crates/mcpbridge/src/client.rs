//! MCP **Client**（总设计 §4.5 / M3 出口 6）：把用户配置的外部 MCP 服务器
//! （stdio 子进程）挂载为 Agent 的额外工具。
//!
//! 这一层只做「连接 + 调用 + 翻译结果」。**分级不在这里**：外部工具的危险级由
//! `fs_ai::agent::gate::floor_at_write` 在 Agent 进门时就定好（Write 起步、
//! 工具级可提级、不可降级），本层只负责把已放行的调用真的发出去、把结果翻回
//! `Observation`。
//!
//! ## 传输
//!
//! 生产路径 = `TokioChildProcess`（拉起外部 server 子进程，stdio 对话）。
//! 测试路径 = `tokio::io::duplex`（进程内把 client 端与一个 rmcp server 端对接，
//! 不拉真进程就能端到端验证协议往返）。两条路都落在同一个泛型入口
//! [`call_tool_once`] 上，故协议行为被测得着。

use rmcp::{
    model::{CallToolRequestParams, CallToolResult, JsonObject},
    service::{serve_client, RoleClient},
    transport::TokioChildProcess,
};

/// 外部服务器配置（用户在设置里填的）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct McpMountConfig {
    /// 稳定 id：Agent 的 `Tool::ExternalMcp.server_id` 指它，工具级提级也按它。
    pub server_id: String,
    /// 启动命令（如 `"npx"`）。
    pub command: String,
    /// 命令参数（如 `["-y", "@modelcontextprotocol/server-filesystem", "/data"]`）。
    #[serde(default)]
    pub args: Vec<String>,
}

/// `CallToolResult` → `Observation`。
///
/// 工具级错误（`is_error=true`）**不是**传输失败：它是工具自己说「没做成」，
/// 翻成 `exit_code=1` + stderr 装原因，让 Agent 像对待一条失败命令那样对待它。
/// 结构化内容优先（程序读的），退化时用文本内容。
pub fn result_to_observation(res: CallToolResult) -> fs_ai::exec::Observation {
    let mut text = String::new();
    if let Some(structured) = &res.structured_content {
        text = structured.to_string();
    } else {
        for block in &res.content {
            if let Some(t) = block.as_text() {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(t.text.as_ref());
            }
        }
    }
    if res.is_error == Some(true) {
        fs_ai::exec::Observation {
            stdout: String::new(),
            stderr: text,
            exit_code: Some(1),
            truncated: false,
            timed_out: false,
        }
    } else {
        fs_ai::exec::Observation {
            stdout: text,
            stderr: String::new(),
            exit_code: Some(0),
            truncated: false,
            timed_out: false,
        }
    }
}

/// 在一个**已建立**的客户端 peer 上调一个工具。抽出来是为了让「连接」与
/// 「调用」可分开测：连接那一段要真进程，调用这一段用假/内存传输就能覆盖。
pub async fn call_on_peer(
    peer: &rmcp::service::Peer<RoleClient>,
    tool: &str,
    arguments_json: &str,
) -> Result<fs_ai::exec::Observation, String> {
    // name 要 `Cow<'static, str>`，借用串转不动——拷成自有串。
    let mut params = CallToolRequestParams::new(tool.to_string());
    if !arguments_json.trim().is_empty() && arguments_json.trim() != "null" {
        let args: JsonObject = serde_json::from_str(arguments_json)
            .map_err(|e| format!("arguments 不是合法 JSON 对象：{e}"))?;
        params = params.with_arguments(args);
    }
    let res = peer
        .call_tool(params)
        .await
        .map_err(|e| format!("外部 MCP 调用失败：{e}"))?;
    Ok(result_to_observation(res))
}

/// 拉起外部 server、问一次 `tools/list`、断开。返回 (tool_name, description, schema_json)。
///
/// Agent 每次开跑前调它，把结果翻成模型可见的工具（`fs_ai` 的
/// `ExternalToolSchema`）。**现问现用、不缓存**：用户刚卸载一个 server，
/// 下一次开跑模型就不该再看到它的工具。
///
/// 单个 server 拉不起来 ⇒ `Err`。调用方应当**跳过它、继续拉别的**——
/// 一个坏掉的挂载不该让整个 Agent 起不来。
pub async fn list_tools_once(
    cfg: &McpMountConfig,
) -> Result<Vec<(String, String, String)>, String> {
    let mut cmd = tokio::process::Command::new(&cfg.command);
    cmd.args(&cfg.args);
    let (child, _stderr) = TokioChildProcess::builder(cmd)
        .spawn()
        .map_err(|e| format!("无法启动外部 MCP server `{}`：{e}", cfg.command))?;
    let service = serve_client((), child)
        .await
        .map_err(|e| format!("外部 MCP 握手失败：{e}"))?;
    let listed = service
        .list_tools(None)
        .await
        .map_err(|e| format!("外部 MCP tools/list 失败：{e}"))?;
    let out = listed
        .tools
        .into_iter()
        .map(|t| {
            let schema = serde_json::to_string(&*t.input_schema)
                .unwrap_or_else(|_| r#"{"type":"object"}"#.to_string());
            (
                t.name.to_string(),
                t.description.map(|d| d.to_string()).unwrap_or_default(),
                schema,
            )
        })
        .collect();
    let _ = service.cancel().await;
    Ok(out)
}

/// 拉起外部 server 子进程、握手、调一个工具、返回结果。
///
/// 这是生产路径：每次调用现连现断（外部 server 的生命周期不由我们常驻管理，
/// 避免挂着一排没人管的子进程）。代价是每次调用多一次握手——对外部工具的
/// 低频调用可接受，换「不留孤儿子进程」的确定性。
pub async fn call_tool_once(
    cfg: &McpMountConfig,
    tool: &str,
    arguments_json: &str,
) -> Result<fs_ai::exec::Observation, String> {
    let mut cmd = tokio::process::Command::new(&cfg.command);
    cmd.args(&cfg.args);
    // builder 默认 stdin/stdout piped、stderr inherit；spawn 返回 (传输, 可选 stderr 句柄)。
    let (child, _stderr) = TokioChildProcess::builder(cmd)
        .spawn()
        .map_err(|e| format!("无法启动外部 MCP server `{}`：{e}", cfg.command))?;
    // TokioChildProcess 自身即 Transport<RoleClient>；`()` 是「不处理服务端主动
    // 请求」的最小客户端（impl ClientHandler for ()）。
    let service = serve_client((), child)
        .await
        .map_err(|e| format!("外部 MCP 握手失败：{e}"))?;
    let obs = call_on_peer(&service, tool, arguments_json).await;
    // 主动收尾：取消服务、关连接，子进程随 stdio 关闭而退出（不留孤儿）。
    let _ = service.cancel().await;
    obs
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `CallToolResult` 的翻译：成功 → stdout；工具级错误 → stderr + exit_code 1。
    #[test]
    fn a_tool_error_becomes_a_failed_observation_not_a_transport_error() {
        let ok = result_to_observation(CallToolResult::structured(serde_json::json!({"k":"v"})));
        assert_eq!(ok.exit_code, Some(0));
        assert!(ok.stdout.contains("\"k\""));
        assert!(ok.stderr.is_empty());

        let bad = result_to_observation(CallToolResult::error(vec![
            rmcp::model::ContentBlock::text("权限不足"),
        ]));
        assert_eq!(bad.exit_code, Some(1), "工具级错误要落成非零退出码");
        assert!(bad.stderr.contains("权限不足"));
        assert!(bad.stdout.is_empty());
    }

    /// `"null"` 解析不成 JSON 对象——这正是 [`call_on_peer`] 要先把它挡在
    /// `from_str` 之前（当作无参数）的原因。这条钉住那个前提。
    #[test]
    fn null_arguments_do_not_parse_into_an_object() {
        assert!(serde_json::from_str::<JsonObject>("{}").is_ok());
        assert!(
            serde_json::from_str::<JsonObject>("null").is_err(),
            "null 不是对象，call_on_peer 必须先挡掉再轮到 from_str"
        );
    }

    // ── 端到端：进程内把 client 与 server 用 duplex 对接 ─────────────────
    //
    // 不拉真子进程就能验证「客户端真的能把一次调用发出去、把结果翻回来」。
    // 生产路径（call_tool_once）只是把传输从 duplex 换成子进程，协议行为同一条。

    use rmcp::{
        model::{
            ContentBlock, InitializeResult, ListToolsResult, PaginatedRequestParams,
            ServerCapabilities, Tool as McpTool,
        },
        service::{RequestContext, RoleServer},
        ErrorData, ServerHandler, ServiceExt,
    };

    /// 最小 echo server：一个 `echo` 工具，把 arguments 原样结构化返回；
    /// 一个 `fail` 工具，回工具级错误。
    struct EchoServer;

    impl ServerHandler for EchoServer {
        fn get_info(&self) -> InitializeResult {
            InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
        }
        async fn list_tools(
            &self,
            _: Option<PaginatedRequestParams>,
            _: RequestContext<RoleServer>,
        ) -> Result<ListToolsResult, ErrorData> {
            let schema: JsonObject =
                serde_json::from_str(r#"{"type":"object","properties":{"msg":{"type":"string"}}}"#)
                    .unwrap();
            Ok(ListToolsResult::with_all_items(vec![
                McpTool::new("echo", "echo back", schema.clone()),
                McpTool::new("fail", "always fails", schema),
            ]))
        }
        async fn call_tool(
            &self,
            req: CallToolRequestParams,
            _: RequestContext<RoleServer>,
        ) -> Result<rmcp::model::CallToolResponse, ErrorData> {
            if req.name == "fail" {
                return Ok(CallToolResult::error(vec![ContentBlock::text("故意失败")]).into());
            }
            let value = req
                .arguments
                .map(serde_json::Value::Object)
                .unwrap_or(serde_json::Value::Null);
            Ok(CallToolResult::structured(value).into())
        }
    }

    /// 起一对 duplex：server 端跑 EchoServer，返回 client 端的 RunningService。
    /// **必须持有 RunningService 本体**（而不是克隆出的 Peer）——传输与服务循环
    /// 挂在它上面，提前析构连接就断了。call_on_peer 收 &Peer，RunningService Deref 可达。
    async fn paired_client() -> rmcp::service::RunningService<RoleClient, ()> {
        let (server_io, client_io) = tokio::io::duplex(64 * 1024);
        let (sr, sw) = tokio::io::split(server_io);
        let (cr, cw) = tokio::io::split(client_io);
        tokio::spawn(async move {
            let _ = EchoServer.serve((sr, sw)).await.unwrap().waiting().await;
        });
        serve_client((), (cr, cw)).await.unwrap()
    }

    /// 客户端经 duplex 调 `echo`，拿回结构化结果 → Observation.stdout。
    #[tokio::test]
    async fn a_client_call_round_trips_through_the_protocol() {
        let peer = paired_client().await;
        let obs = call_on_peer(&peer, "echo", r#"{"msg":"你好"}"#)
            .await
            .unwrap();
        assert_eq!(obs.exit_code, Some(0));
        assert!(
            obs.stdout.contains("你好"),
            "结果应带回参数：{}",
            obs.stdout
        );
        assert!(obs.stderr.is_empty());
    }

    /// 工具级错误（fail）→ Observation.exit_code 非零、原因进 stderr。
    /// **不是**传输错误——客户端要能区分「工具没做成」与「连不上」。
    #[tokio::test]
    async fn a_tool_level_failure_is_an_observation_not_a_transport_error() {
        let peer = paired_client().await;
        let obs = call_on_peer(&peer, "fail", "{}").await.unwrap();
        assert_eq!(obs.exit_code, Some(1));
        assert!(obs.stderr.contains("故意失败"));
        assert!(obs.stdout.is_empty());
    }

    /// 无参调用（arguments 空串）也走得通——零参工具常见。
    #[tokio::test]
    async fn a_zero_argument_call_works() {
        let peer = paired_client().await;
        let obs = call_on_peer(&peer, "echo", "").await.unwrap();
        assert_eq!(obs.exit_code, Some(0));
    }
}
