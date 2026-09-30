//! MCP Server 经**真 rmcp 客户端**的端到端协议测试（M3 出口第 5 项）。
//!
//! # 这一组补的是什么洞
//!
//! `crates/mcpbridge` 此前有 102 个测试，覆盖得相当密——但它们全部止步于
//! **函数调用**：`server::call(...)` 直接被调、`to_response(...)` 直接被调。
//! 没有一条测试让一个真正的 MCP 客户端说过一句 JSON-RPC。
//!
//! 出口原文说的是「第三方 MCP 客户端（如 Claude Desktop 配置指向本程序）
//! 可 list/call 已授权工具」。「客户端可以」与「函数返回了正确的值」之间隔着
//! 一整层协议：initialize 协商、tools/list 的分页包装、tools/call 的参数
//! 序列化、错误走 result.isError 还是 JSON-RPC error。那一层此前一次也没跑过。
//!
//! 这里用 `tokio::io::duplex` 把 `McpServer` 与 `rmcp::serve_client` 对接：
//! **协议是真的**（同一套编解码、同一次握手、同一条 JSON-RPC 通道），
//! 省掉的只是操作系统的进程边界。进程边界那一半由
//! `crates/mcpbridge/src/client.rs` 的子进程路径与 app 层的 `serve_stdio` 承担。
//!
//! # 与 Claude Desktop 真机核验的关系
//!
//! 这组测试**不能**替代「拿 Claude Desktop 连一次」——真客户端可能有自己的
//! 协议版本口径、可能对 schema 有额外要求。它能替代的是「协议这一层根本没跑过」
//! 这个状态：现在它跑过了，且任何一处翻译写反都会在这里转红。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use fs_ai::exec::Observation;
use fs_mcpbridge::authz::Authorization;
use fs_mcpbridge::confirm::ConfirmTicket;
use fs_mcpbridge::ports::{
    CommandPort, ConfirmReply, McpAuditPort, McpCallRecord, McpConfirmPort, PortResult,
    SessionDirectory, SessionSummary, SftpPort, TerminalPort,
};
use fs_mcpbridge::server::CallContext;
use fs_mcpbridge::tool::ReadSource;
use fs_mcpbridge::transport::{McpContextSource, McpServer};
use fs_policy::AiMode;
use rmcp::model::CallToolRequestParams;
use rmcp::service::{RoleClient, RunningService};
use rmcp::{serve_client, ServiceExt};

type Fut<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

// ───────────────────────── 假端口 ─────────────────────────
//
// 只做到「能把协议跑起来」的最小程度。裁决逻辑本身由 server.rs 的 102 个
// 测试覆盖，这里要验的是协议那一层，故端口一律给确定的、好断言的返回。

struct Dir;
impl SessionDirectory for Dir {
    fn list(&self) -> Fut<'_, Vec<SessionSummary>> {
        Box::pin(async {
            vec![SessionSummary {
                id: "s-1".into(),
                name: "生产机".into(),
                host: "example.internal".into(),
                connected: true,
            }]
        })
    }
    fn is_alive(&self, session_id: &str) -> bool {
        session_id == "s-1"
    }
    fn mcp_allowed(&self, _profile_id: &str) -> Fut<'_, Option<bool>> {
        Box::pin(async { Some(true) })
    }
    fn open(&self, _profile_id: &str) -> Fut<'_, PortResult<String>> {
        Box::pin(async { Ok("s-2".to_string()) })
    }
}

struct Term;
impl TerminalPort for Term {
    fn read_text(
        &self,
        _session_id: &str,
        _source: ReadSource,
        _max_lines: Option<usize>,
    ) -> Fut<'_, PortResult<String>> {
        // 故意回吐一个已知机密：脱敏必须发生在字节**离开本进程之前**。
        // 端口自己不脱敏（ports.rs 明写脱敏统一在 server.rs 做，
        // 那样只有一个脱敏点可以被守卫钉住）——所以这里裸着回，
        // 由协议对面收到的东西来判断那个唯一的脱敏点在不在。
        Box::pin(async { Ok("$ echo hunter2\nhunter2\n".to_string()) })
    }
    fn send(&self, _session_id: &str, _payload: &str) -> Fut<'_, PortResult<()>> {
        Box::pin(async { Ok(()) })
    }
}

struct Cmd;
impl CommandPort for Cmd {
    fn run(&self, _session_id: &str, command: &str) -> Fut<'_, PortResult<Observation>> {
        let c = command.to_string();
        Box::pin(async move {
            Ok(Observation {
                stdout: format!("ran: {c}"),
                stderr: String::new(),
                exit_code: Some(0),
                truncated: false,
                timed_out: false,
            })
        })
    }
}

struct Sftp;
impl SftpPort for Sftp {
    fn list(&self, _s: &str, _p: &str) -> Fut<'_, PortResult<String>> {
        Box::pin(async { Ok("a.txt\n".to_string()) })
    }
    fn read(&self, _s: &str, _p: &str, _m: Option<usize>) -> Fut<'_, PortResult<String>> {
        Box::pin(async { Ok("body".to_string()) })
    }
    fn write(&self, _s: &str, _p: &str, _c: &str) -> Fut<'_, PortResult<()>> {
        Box::pin(async { Ok(()) })
    }
}

/// 确认端口：按构造时给定的答案回。`None` 模拟超时。
struct Conf {
    answer: Option<bool>,
}
impl McpConfirmPort for Conf {
    fn confirm(&self, _t: ConfirmTicket) -> Fut<'_, ConfirmReply> {
        let a = self.answer;
        Box::pin(async move {
            ConfirmReply {
                answer: a,
                elapsed: Duration::from_secs(if a.is_none() { 60 } else { 1 }),
                context_alive: true,
            }
        })
    }
}

/// 审计端口：只数行数。**审计写入失败会阻断执行**，故这里必须成功——
/// 否则每个用例都会撞上「Intent 写不进去 → 不执行」而不是撞上被测的那件事。
#[derive(Default)]
struct Audit {
    rows: AtomicUsize,
}
impl McpAuditPort for Audit {
    fn append(&self, _r: McpCallRecord) -> Fut<'_, PortResult<()>> {
        self.rows.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(()) })
    }
}

struct Ctx;
impl McpContextSource for Ctx {
    fn current(&self) -> Fut<'_, CallContext> {
        Box::pin(async {
            CallContext {
                caller: "test-client".into(),
                // WithConfirm 是默认档，也是唯一能把「确认四出口」跑到的档：
                // Disabled 连确认框都不弹，ReadOnlyOnly 直接拒写。
                mode: AiMode::WithConfirm,
                known_secrets: vec!["hunter2".into()],
            }
        })
    }
}

// ───────────────────────── 装配 ─────────────────────────

/// 起一对 duplex：一端跑 `McpServer`，另一端是**真的 rmcp 客户端**。
///
/// 必须持有返回的 `RunningService` 本体——传输与服务循环挂在它上面，
/// 提前析构连接就断了（client.rs 的测试里同一条注释，同一个坑）。
async fn paired(tools: &[&str], answer: Option<bool>) -> RunningService<RoleClient, ()> {
    let server = McpServer::new(
        Arc::new(Ctx),
        Authorization::new(tools.iter().copied()),
        Arc::new(Dir),
        Arc::new(Term),
        Arc::new(Cmd),
        Arc::new(Sftp),
        Arc::new(Conf { answer }),
        Arc::new(Audit::default()),
    );
    let (server_io, client_io) = tokio::io::duplex(64 * 1024);
    let (sr, sw) = tokio::io::split(server_io);
    let (cr, cw) = tokio::io::split(client_io);
    tokio::spawn(async move {
        let _ = server.serve((sr, sw)).await.unwrap().waiting().await;
    });
    serve_client((), (cr, cw)).await.unwrap()
}

/// 走协议发一次 `tools/call`，取回 `(is_error, structured_content)`。
async fn call(
    peer: &RunningService<RoleClient, ()>,
    name: &str,
    args: serde_json::Value,
) -> (bool, Option<serde_json::Value>, String) {
    let mut params = CallToolRequestParams::new(name.to_string());
    if let Some(obj) = args.as_object() {
        params = params.with_arguments(obj.clone());
    }
    // `expect` 而不是往上抛：**协议级出错就是被测契约本身破了**。
    // 拒绝必须落在 result.isError（工具级，用户读得到 FS_POLICY_* 码），
    // 走成 JSON-RPC error 的话客户端只会渲染一句不透明提示——
    // 那正是 rmcp spike 定下的错误分路里被明确排除的那一支。
    let result = peer
        .call_tool(params)
        .await
        .expect("协议级不该出错：拒绝要走工具级 isError，不是 JSON-RPC error");
    let text = result
        .content
        .iter()
        .filter_map(|c| c.as_text().map(|t| t.text.clone()))
        .collect::<Vec<_>>()
        .join("\n");
    (
        result.is_error.unwrap_or(false),
        result.structured_content,
        text,
    )
}

// ───────────────────────── tools/list ─────────────────────────

#[tokio::test]
async fn a_real_client_sees_exactly_the_authorized_tools() {
    let peer = paired(&["sessions.list", "terminal.read"], Some(true)).await;
    let listed = peer.list_all_tools().await.expect("tools/list 应当成功");
    let mut names: Vec<String> = listed.iter().map(|t| t.name.to_string()).collect();
    names.sort();
    assert_eq!(
        names,
        vec!["sessions.list".to_string(), "terminal.read".to_string()],
        "客户端看到的清单必须与授权集逐字相同"
    );
    // schema 要真的过得了 JSON Schema 的形（客户端据此构造参数）。
    for t in &listed {
        let schema = serde_json::to_value(&*t.input_schema).unwrap();
        assert_eq!(schema["type"], "object", "{} 的 schema 不是 object", t.name);
    }
}

#[tokio::test]
async fn an_unauthorized_tool_is_not_enumerable_over_the_wire() {
    // 出口原文：「未授权工具与 Vault 类工具不可枚举」。
    // 「不可枚举」比「调用时被拒」强一档——外部模型连它存在都不该知道。
    let peer = paired(&["sessions.list"], Some(true)).await;
    let listed = peer.list_all_tools().await.unwrap();
    let names: Vec<String> = listed.iter().map(|t| t.name.to_string()).collect();
    for forbidden in [
        "command.run",
        "terminal.send",
        "sftp.write",
        "sessions.open",
    ] {
        assert!(
            !names.contains(&forbidden.to_string()),
            "未授权的 {forbidden} 出现在了枚举里：{names:?}"
        );
    }
}

#[tokio::test]
async fn authorizing_nothing_enumerates_nothing() {
    // 默认关的那一档在协议上长什么样：清单为空，而不是「清单里都是灰的」。
    let peer = paired(&[], Some(true)).await;
    let listed = peer.list_all_tools().await.unwrap();
    assert!(
        listed.is_empty(),
        "没授权任何工具时清单该是空的：{listed:?}"
    );
}

#[tokio::test]
async fn a_vault_class_tool_cannot_be_authorized_into_existence() {
    // Vault 类不可枚举**不是靠枚举时过滤**兑现的（那依赖一个人记得写过滤），
    // 而是清单里根本没有这类条目。所以「授权一个 vault 工具」的结果只能是
    // 「这个名字不认识」，而不是「授权了但被过滤掉」——两者的区别在于：
    // 前者不可能因为漏写一处过滤而失效。
    let authz = Authorization::new(["vault.read", "vault.list", "sessions.list"]);
    assert_eq!(
        authz.unknown_names(),
        ["vault.read".to_string(), "vault.list".to_string()],
        "vault 类名字必须被报为不认识，而不是被静默接受"
    );
    let peer = paired(&["vault.read", "sessions.list"], Some(true)).await;
    let names: Vec<String> = peer
        .list_all_tools()
        .await
        .unwrap()
        .iter()
        .map(|t| t.name.to_string())
        .collect();
    assert_eq!(names, vec!["sessions.list".to_string()]);
}

// ───────────────────────── tools/call ─────────────────────────

#[tokio::test]
async fn calling_an_authorized_readonly_tool_round_trips() {
    let peer = paired(&["sessions.list"], Some(true)).await;
    let (is_err, structured, text) = call(&peer, "sessions.list", serde_json::json!({})).await;
    assert!(!is_err, "只读工具不该被拒：{text}");
    let body = structured.map(|v| v.to_string()).unwrap_or(text);
    assert!(body.contains("example.internal"), "结果没回来：{body}");
    // 会话摘要里**不该**有用户名与端口（ports.rs：那是一份现成的横向移动清单）。
    assert!(!body.contains("22"), "会话摘要泄露了端口：{body}");
}

#[tokio::test]
async fn known_secrets_are_redacted_before_the_bytes_leave_the_process() {
    // 端口裸着回了 "hunter2"（见 Term::read_text 的注释）。它要是原样穿过协议，
    // 就等于把 Vault 里的密钥送进了外部模型的上下文。
    let peer = paired(&["terminal.read"], Some(true)).await;
    let (is_err, structured, text) = call(
        &peer,
        "terminal.read",
        serde_json::json!({"session_id":"s-1","source":"screen"}),
    )
    .await;
    assert!(!is_err, "terminal.read 是只读，不该被拒：{text}");
    let all = format!(
        "{text}{}",
        structured.map(|v| v.to_string()).unwrap_or_default()
    );
    assert!(!all.contains("hunter2"), "已知机密经协议流出了：{all}");
    // 反向对照：不是整段被吞掉了——非机密部分要照常到达。
    assert!(all.contains("echo"), "脱敏把正文也吃掉了：{all}");
}

#[tokio::test]
async fn an_unauthorized_call_and_a_nonexistent_call_are_indistinguishable() {
    // 防枚举：两条路径的回答必须**逐字相同**。差一个字，外部模型就能靠
    // 「问一遍看回什么」把「有这个工具但你没权限」和「没这个工具」分开，
    // 而前者本身就是情报（它告诉对方这台机器上有哪些能力可以去争取）。
    let peer = paired(&["sessions.list"], Some(true)).await;
    let (e1, s1, t1) = call(&peer, "command.run", serde_json::json!({})).await;
    let (e2, s2, t2) = call(&peer, "no.such.tool.at.all", serde_json::json!({})).await;
    assert!(e1 && e2, "两者都该是错误");
    assert_eq!(t1, t2, "两条路径的文案必须逐字相同（否则是枚举预言机）");
    assert_eq!(s1, s2, "两条路径的结构化码也必须相同");
}

#[tokio::test]
async fn a_denial_is_a_tool_level_structured_error_not_a_json_rpc_error() {
    // 若哪天有人把拒绝改成 Err(ErrorData)，`call` 里的 expect 会先炸——
    // 那句 expect 的消息就是这条契约本身。这里再断言一次落点。
    let peer = paired(&["sessions.list"], Some(true)).await;
    let (is_err, structured, _) = call(&peer, "command.run", serde_json::json!({})).await;
    assert!(is_err, "拒绝必须 isError=true");
    let sc = structured.expect("拒绝必须带结构化码");
    assert!(
        sc["code"].as_str().unwrap_or_default().starts_with("FS_"),
        "拒绝码不是 FS_* 稳定码：{sc}"
    );
}

#[tokio::test]
async fn a_confirmation_timeout_becomes_a_structured_rejection_over_the_wire() {
    // 出口原文第三句：「确认超时→结构化拒绝」。
    // 超时与「用户答了否」必须是两个码——事后追查时，「没人在」和
    // 「有人看了并拒绝了」是完全不同的两件事。
    let peer = paired(&["command.run"], None).await;
    let (is_err, structured, text) = call(
        &peer,
        "command.run",
        serde_json::json!({"session_id":"s-1","command":"rm -rf /tmp/x"}),
    )
    .await;
    assert!(is_err, "超时必须是拒绝：{text}");
    let sc = structured.expect("超时也要带结构化码");
    let code = sc["code"].as_str().unwrap_or_default().to_string();
    assert!(
        code.contains("TIMEOUT") || code.contains("TIMED"),
        "超时码不对：{sc}"
    );

    // 反向对照：同一个调用在「用户答了否」下拿到的是另一个码。
    let peer2 = paired(&["command.run"], Some(false)).await;
    let (_, s2, _) = call(
        &peer2,
        "command.run",
        serde_json::json!({"session_id":"s-1","command":"rm -rf /tmp/x"}),
    )
    .await;
    let code2 = s2.expect("拒绝要带码")["code"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert_ne!(code, code2, "「没人在」与「有人拒了」不能是同一个码");
}

#[tokio::test]
async fn an_approved_dangerous_call_executes_and_returns_over_the_wire() {
    // 确认四出口里「批准」那一支的协议侧：批准之后结果要真的回来，
    // 且带 exit_code 等结构化字段（外部模型据此判断成没成）。
    let peer = paired(&["command.run"], Some(true)).await;
    let (is_err, structured, text) = call(
        &peer,
        "command.run",
        serde_json::json!({"session_id":"s-1","command":"rm -rf /tmp/x"}),
    )
    .await;
    assert!(!is_err, "批准后不该再被拒：{text}");
    let sc = structured.expect("command.run 要带结构化 Observation");
    assert_eq!(sc["exit_code"], 0);
    assert!(
        sc["stdout"].as_str().unwrap_or_default().contains("rm -rf"),
        "命令没到达端口：{sc}"
    );
}

#[tokio::test]
async fn the_handshake_advertises_tools_and_the_server_identity() {
    let peer = paired(&["sessions.list"], Some(true)).await;
    let info = peer.peer_info().expect("握手后该有对端信息");
    assert!(
        info.capabilities.tools.is_some(),
        "没声明 tools 能力，客户端不会去问 tools/list"
    );
    let ident = info.server_info.as_ref().expect("握手要报出服务端身份");
    assert_eq!(ident.name, "FutureShell");
}
