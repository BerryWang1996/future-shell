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
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fs_ai::exec::Observation;
use fs_mcpbridge::authz::Authorization;
use fs_mcpbridge::confirm::ConfirmTicket;
use fs_mcpbridge::ports::{
    CommandPort, ConfirmReply, McpAuditPort, McpCallRecord, McpConfirmPort, PortResult,
    ProfileSummary, SessionDirectory, SessionSummary, SftpPort, TerminalPort,
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

struct Management(Arc<Dir>);
impl fs_mcpbridge::managed::ManagementPort for Management {
    fn generation(&self, _: &str) -> Option<u64> {
        Some(1)
    }
    fn run<'a>(
        &'a self,
        tool: &'a fs_mcpbridge::managed::ManagedTool,
        _: Option<u64>,
    ) -> Fut<'a, Result<serde_json::Value, fs_mcpbridge::managed::ManagementError>> {
        Box::pin(async move {
            if let fs_mcpbridge::managed::ManagedTool::Close { session_id } = tool {
                self.0.sessions.lock().unwrap().remove(session_id);
            }
            Ok(serde_json::json!({}))
        })
    }
}
struct Dir {
    sessions: Mutex<std::collections::BTreeSet<String>>,
    next: AtomicUsize,
}
impl Dir {
    fn new(initial: bool) -> Self {
        let sessions = if initial {
            ["s-1".to_string()].into_iter().collect()
        } else {
            Default::default()
        };
        Self {
            sessions: Mutex::new(sessions),
            next: AtomicUsize::new(2),
        }
    }
}
impl SessionDirectory for Dir {
    fn list_profiles(&self) -> Fut<'_, PortResult<Vec<ProfileSummary>>> {
        Box::pin(async {
            Ok(vec![ProfileSummary {
                profile_id: "p-1".into(),
                name: "测试 hunter2".into(),
            }])
        })
    }

    fn list(&self) -> Fut<'_, Vec<SessionSummary>> {
        Box::pin(async move {
            self.sessions
                .lock()
                .unwrap()
                .iter()
                .map(|id| SessionSummary {
                    id: id.clone(),
                    name: "生产机".into(),
                    host: "example.internal".into(),
                    connected: true,
                })
                .collect()
        })
    }
    fn is_alive(&self, session_id: &str) -> bool {
        self.sessions.lock().unwrap().contains(session_id)
    }
    fn mcp_allowed(&self, profile_id: &str) -> Fut<'_, Option<bool>> {
        let allowed = match profile_id {
            "missing" => None,
            "unallowed" => Some(false),
            _ => Some(true),
        };
        Box::pin(async move { allowed })
    }
    fn session_allowed(&self, _session_id: &str) -> Fut<'_, bool> {
        Box::pin(async { true })
    }
    fn open(&self, _profile_id: &str) -> Fut<'_, PortResult<String>> {
        Box::pin(async move {
            let id = format!("s-{}", self.next.fetch_add(1, Ordering::SeqCst));
            self.sessions.lock().unwrap().insert(id.clone());
            Ok(id)
        })
    }
}

struct Term;
impl TerminalPort for Term {
    fn read_text(
        &self,
        _session_id: &str,
        _source: ReadSource,
        _max_lines: Option<usize>,
        _wait_ms: u64,
        _generation: Option<u64>,
    ) -> Fut<'_, PortResult<fs_mcpbridge::ports::TerminalSnapshot>> {
        // 故意回吐一个已知机密：脱敏必须发生在字节**离开本进程之前**。
        // 端口自己不脱敏（ports.rs 明写脱敏统一在 server.rs 做，
        // 那样只有一个脱敏点可以被守卫钉住）——所以这里裸着回，
        // 由协议对面收到的东西来判断那个唯一的脱敏点在不在。
        Box::pin(async {
            Ok(fs_mcpbridge::ports::TerminalSnapshot {
                text: "$ echo hunter2\nhunter2\n".to_string(),
                timed_out: false,
                connection_state: "connected".into(),
            })
        })
    }
    fn send(
        &self,
        _session_id: &str,
        _payload: &str,
        _generation: Option<u64>,
    ) -> Fut<'_, PortResult<()>> {
        Box::pin(async { Ok(()) })
    }
}

struct Cmd;
impl CommandPort for Cmd {
    fn run(
        &self,
        _session_id: &str,
        command: &str,
        _generation: Option<u64>,
    ) -> Fut<'_, PortResult<Observation>> {
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
    fn list(&self, _s: &str, _p: &str, _generation: Option<u64>) -> Fut<'_, PortResult<String>> {
        Box::pin(async { Ok("a.txt\n".to_string()) })
    }
    fn read(
        &self,
        _s: &str,
        _p: &str,
        _m: Option<usize>,
        _generation: Option<u64>,
    ) -> Fut<'_, PortResult<String>> {
        Box::pin(async { Ok("body".to_string()) })
    }
    fn write(
        &self,
        _s: &str,
        _p: &str,
        _c: &str,
        _generation: Option<u64>,
    ) -> Fut<'_, PortResult<()>> {
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
                skip_confirmation: false,
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
    paired_with_sessions(tools, answer, true).await
}

async fn paired_with_sessions(
    tools: &[&str],
    answer: Option<bool>,
    initial: bool,
) -> RunningService<RoleClient, ()> {
    let directory = Arc::new(Dir::new(initial));
    let server = McpServer::new(
        Arc::new(Ctx),
        Arc::new(Management(directory.clone())),
        Authorization::new(tools.iter().copied()),
        directory,
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
        "profiles.list",
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
async fn only_authorized_clients_can_discover_redacted_profile_summaries() {
    let peer = paired(&["sessions.list"], Some(true)).await;
    assert!(call(&peer, "profiles.list", serde_json::json!({})).await.0);

    let peer = paired(&["profiles.list"], Some(false)).await;
    let tools = peer.list_all_tools().await.unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "profiles.list");
    let (is_err, structured, text) = call(&peer, "profiles.list", serde_json::json!({})).await;
    assert!(!is_err);
    let value = structured.unwrap();
    let summary = value["profiles"][0].as_object().unwrap();
    assert_eq!(summary.len(), 2);
    assert_eq!(summary["profile_id"], "p-1");
    assert!(summary["name"].as_str().unwrap().contains("测试"));
    assert!(!value.to_string().contains("hunter2"));
    assert!(!text.contains("hunter2"));
}

#[tokio::test]
async fn terminal_enter_is_discoverable_and_uses_the_existing_risk_gate() {
    let peer = paired(&["terminal.send"], Some(true)).await;
    let tools = peer.list_all_tools().await.unwrap();
    let schema = &tools[0].input_schema;
    assert_eq!(schema["properties"]["press_enter"]["type"], "boolean");
    assert_eq!(schema["properties"]["press_enter"]["default"], false);

    let args = serde_json::json!({"session_id":"s-1","payload":"p","press_enter":true});
    let (is_err, _, text) = call(&peer, "terminal.send", args.clone()).await;
    assert!(!is_err);
    assert_eq!(text, "已发送 2 字节");

    let peer = paired(&["terminal.send"], Some(false)).await;
    let (is_err, structured, _) = call(&peer, "terminal.send", args).await;
    assert!(is_err);
    assert_eq!(structured.unwrap()["tier"], "dangerous");
}

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
    let structured = structured.expect("terminal.read supplies structured status");
    assert_eq!(structured["session_id"], "s-1");
    assert_eq!(structured["source"], "screen");
    assert_eq!(structured["empty"], false);
    assert_eq!(structured["timed_out"], false);
    assert_eq!(structured["connection_state"], "connected");
    assert_eq!(structured["text"], text);
    assert!(text.starts_with("$ echo "));
    let all = format!("{text}{structured}");
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
    let instructions = info.instructions.as_deref().expect("握手应说明操作边界");
    for expected in [
        "sessions.list 为空",
        "无需预先手动打开服务器",
        "同一配置每次成功打开",
        "不盲目重发",
        "原始连接端点",
        "无 PTY",
        "sessions.details",
        "all/scrollback",
        "不要自动重复执行",
    ] {
        assert!(instructions.contains(expected), "缺少协议说明: {expected}");
    }
}

#[tokio::test]
async fn empty_registry_allows_two_parallel_opens_and_independent_close() {
    let peer = paired_with_sessions(
        &[
            "profiles.list",
            "sessions.list",
            "sessions.open",
            "sessions.close",
            "terminal.read",
        ],
        Some(true),
        false,
    )
    .await;
    let (err, value, _) = call(&peer, "sessions.list", serde_json::json!({})).await;
    assert!(!err);
    assert_eq!(value.unwrap(), serde_json::json!([]));
    let (err, value, _) = call(&peer, "profiles.list", serde_json::json!({})).await;
    assert!(!err);
    let profile = value.unwrap()["profiles"][0]["profile_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let (a, b) = tokio::join!(
        call(
            &peer,
            "sessions.open",
            serde_json::json!({"profile_id":profile})
        ),
        call(
            &peer,
            "sessions.open",
            serde_json::json!({"profile_id":profile})
        ),
    );
    assert!(!a.0 && !b.0);
    let a = a.1.unwrap()["session_id"].as_str().unwrap().to_owned();
    let b = b.1.unwrap()["session_id"].as_str().unwrap().to_owned();
    assert_ne!(a, b);
    let (_, list, _) = call(&peer, "sessions.list", serde_json::json!({})).await;
    let list = list.unwrap();
    assert_eq!(list.as_array().unwrap().len(), 2);
    assert!(list.as_array().unwrap().iter().any(|s| s["id"] == a));
    assert!(list.as_array().unwrap().iter().any(|s| s["id"] == b));
    assert!(
        !call(&peer, "sessions.close", serde_json::json!({"session_id":a}))
            .await
            .0
    );
    let (_, list, _) = call(&peer, "sessions.list", serde_json::json!({})).await;
    let list = list.unwrap();
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["id"], b);
    assert!(
        !call(
            &peer,
            "terminal.read",
            serde_json::json!({"session_id":b,"source":"screen"})
        )
        .await
        .0
    );
}

#[tokio::test]
async fn session_guidance_is_discoverable_and_refusals_keep_generic_codes() {
    let peer = paired_with_sessions(
        &[
            "profiles.list",
            "sessions.list",
            "sessions.open",
            "sessions.details",
            "terminal.read",
        ],
        Some(true),
        false,
    )
    .await;
    let listed = peer.list_all_tools().await.unwrap();
    let description = |name: &str| {
        listed
            .iter()
            .find(|t| t.name == name)
            .unwrap()
            .description
            .as_deref()
            .unwrap()
            .to_string()
    };
    assert!(description("sessions.list").contains("空列表不代表没有可连接配置"));
    let open = description("sessions.open");
    for phrase in [
        "profiles.list",
        "无需先在 FutureShell 手动打开",
        "独立会话",
        "不要盲目重发",
    ] {
        assert!(open.contains(phrase), "missing guidance: {phrase}");
    }
    let missing_session = call(
        &peer,
        "terminal.read",
        serde_json::json!({"session_id":"missing","source":"screen"}),
    )
    .await;
    assert!(missing_session.0);
    let err = missing_session.1.unwrap();
    assert_eq!(err["code"], "FS_POLICY_FORBIDDEN");
    assert!(err["message"].as_str().unwrap().contains("profiles.list"));
    assert!(!err["message"].as_str().unwrap().contains("已断开"));
    let missing_managed = call(
        &peer,
        "sessions.details",
        serde_json::json!({"session_id":"missing"}),
    )
    .await;
    assert!(missing_managed.0);
    let managed_error = missing_managed.1.unwrap();
    assert_eq!(managed_error["code"], "FS_MCP_TARGET_UNAVAILABLE");
    assert!(managed_error["message"]
        .as_str()
        .unwrap()
        .contains("profiles.list"));
    let missing = call(
        &peer,
        "sessions.open",
        serde_json::json!({"profile_id":"missing"}),
    )
    .await;
    let denied = call(
        &peer,
        "sessions.open",
        serde_json::json!({"profile_id":"unallowed"}),
    )
    .await;
    assert!(missing.0 && denied.0);
    assert_eq!(missing.1, denied.1);
    let err = missing.1.unwrap();
    assert_eq!(err["code"], "FS_POLICY_FORBIDDEN");
    assert!(err["message"].as_str().unwrap().contains("profiles.list"));
}

struct ProbeDir {
    inner: Dir,
    open_barrier: Option<Arc<tokio::sync::Barrier>>,
    barrier_calls: AtomicUsize,
}
impl SessionDirectory for ProbeDir {
    fn list_profiles(&self) -> Fut<'_, PortResult<Vec<ProfileSummary>>> {
        self.inner.list_profiles()
    }
    fn list(&self) -> Fut<'_, Vec<SessionSummary>> {
        self.inner.list()
    }
    fn is_alive(&self, id: &str) -> bool {
        self.inner.is_alive(id)
    }
    fn mcp_allowed(&self, id: &str) -> Fut<'_, Option<bool>> {
        self.inner.mcp_allowed(id)
    }
    fn session_allowed(&self, id: &str) -> Fut<'_, bool> {
        self.inner.session_allowed(id)
    }
    fn open(&self, id: &str) -> Fut<'_, PortResult<String>> {
        let id = id.to_string();
        Box::pin(async move {
            if let Some(barrier) = &self.open_barrier {
                if self.barrier_calls.fetch_add(1, Ordering::SeqCst) < 2 {
                    barrier.wait().await;
                }
            }
            if id == "failed-open" {
                return Err("synthetic open failure 中文 hunter2".into());
            }
            self.inner.open(&id).await
        })
    }
}
struct ProbeCommand {
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}
impl CommandPort for ProbeCommand {
    fn run(&self, _: &str, _: &str, _: Option<u64>) -> Fut<'_, PortResult<Observation>> {
        Box::pin(async move {
            self.entered.notify_one();
            self.release.notified().await;
            Ok(Observation {
                stdout: "done".into(),
                stderr: String::new(),
                exit_code: Some(0),
                truncated: false,
                timed_out: false,
            })
        })
    }
}
#[derive(Default)]
struct ProbeConfirm {
    strong: Mutex<Vec<bool>>,
    reject: std::sync::atomic::AtomicBool,
}
impl McpConfirmPort for ProbeConfirm {
    fn confirm(&self, ticket: ConfirmTicket) -> Fut<'_, ConfirmReply> {
        self.strong.lock().unwrap().push(ticket.strong);
        let answer = !self.reject.load(Ordering::SeqCst);
        Box::pin(async move {
            ConfirmReply {
                answer: Some(answer),
                elapsed: Duration::from_secs(1),
                context_alive: true,
            }
        })
    }
}
#[derive(Default)]
struct ProbeAudit {
    fail_settled: std::sync::atomic::AtomicBool,
}
impl McpAuditPort for ProbeAudit {
    fn append(&self, r: McpCallRecord) -> Fut<'_, PortResult<()>> {
        let fail = r.tool_name == "sessions.open"
            && r.phase == fs_mcpbridge::ports::RecordPhase::Settled
            && self.fail_settled.swap(false, Ordering::SeqCst);
        Box::pin(async move {
            if fail {
                Err("synthetic settled failure 中文 hunter2".into())
            } else {
                Ok(())
            }
        })
    }
}
async fn concurrency_peer(
    dir: Arc<ProbeDir>,
    command: Arc<ProbeCommand>,
    conf: Arc<ProbeConfirm>,
    audit: Arc<ProbeAudit>,
) -> RunningService<RoleClient, ()> {
    let server = McpServer::new(
        Arc::new(Ctx),
        Arc::new(Management(Arc::new(Dir::new(true)))),
        Authorization::new(["sessions.list", "sessions.open", "command.run"]),
        dir,
        Arc::new(Term),
        command,
        Arc::new(Sftp),
        conf,
        audit,
    );
    let (server_io, client_io) = tokio::io::duplex(64 * 1024);
    let (sr, sw) = tokio::io::split(server_io);
    let (cr, cw) = tokio::io::split(client_io);
    tokio::spawn(async move {
        let _ = server.serve((sr, sw)).await.unwrap().waiting().await;
    });
    serve_client((), (cr, cw)).await.unwrap()
}
fn probe_dir(barrier: bool) -> Arc<ProbeDir> {
    Arc::new(ProbeDir {
        inner: Dir::new(true),
        open_barrier: barrier.then(|| Arc::new(tokio::sync::Barrier::new(2))),
        barrier_calls: AtomicUsize::new(0),
    })
}
fn probe_command() -> Arc<ProbeCommand> {
    Arc::new(ProbeCommand {
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    })
}

#[tokio::test]
async fn same_peer_read_is_not_blocked_by_running_command() {
    let command = probe_command();
    let peer = concurrency_peer(
        probe_dir(false),
        command.clone(),
        Arc::new(ProbeConfirm::default()),
        Arc::new(ProbeAudit::default()),
    )
    .await;
    let command_call = call(
        &peer,
        "command.run",
        serde_json::json!({"session_id":"s-1","command":"sleep 10"}),
    );
    let read_while_blocked = async {
        command.entered.notified().await;
        let read = tokio::time::timeout(
            Duration::from_secs(1),
            call(&peer, "sessions.list", serde_json::json!({})),
        )
        .await;
        command.release.notify_one();
        assert!(!read.expect("read blocked behind command").0);
    };
    let (command_result, _) = tokio::join!(command_call, read_while_blocked);
    assert!(!command_result.0);
}

#[tokio::test]
async fn same_peer_first_opens_run_concurrently_and_merge_seen_profiles() {
    let confirm = Arc::new(ProbeConfirm::default());
    let peer = concurrency_peer(
        probe_dir(true),
        probe_command(),
        confirm.clone(),
        Arc::new(ProbeAudit::default()),
    )
    .await;
    let both = tokio::time::timeout(Duration::from_secs(2), async {
        tokio::join!(
            call(
                &peer,
                "sessions.open",
                serde_json::json!({"profile_id":"p1"})
            ),
            call(
                &peer,
                "sessions.open",
                serde_json::json!({"profile_id":"p2"})
            )
        )
    })
    .await
    .expect("both opens must enter barrier concurrently");
    assert!(!both.0 .0 && !both.1 .0);
    assert_ne!(
        both.0 .1.unwrap()["session_id"],
        both.1 .1.unwrap()["session_id"]
    );
    for id in ["p1", "p2"] {
        assert!(
            !call(&peer, "sessions.open", serde_json::json!({"profile_id":id}))
                .await
                .0
        );
    }
    assert_eq!(
        *confirm.strong.lock().unwrap(),
        vec![true, true, false, false]
    );
}

#[tokio::test]
async fn concurrent_first_opens_for_same_profile_both_confirm_and_create_sessions() {
    let confirm = Arc::new(ProbeConfirm::default());
    let peer = concurrency_peer(
        probe_dir(true),
        probe_command(),
        confirm.clone(),
        Arc::new(ProbeAudit::default()),
    )
    .await;
    let (a, b) = tokio::time::timeout(Duration::from_secs(2), async {
        tokio::join!(
            call(
                &peer,
                "sessions.open",
                serde_json::json!({"profile_id":"p1"})
            ),
            call(
                &peer,
                "sessions.open",
                serde_json::json!({"profile_id":"p1"})
            )
        )
    })
    .await
    .expect("both same-profile opens must enter barrier");
    assert!(!a.0 && !b.0);
    assert_ne!(a.1.unwrap()["session_id"], b.1.unwrap()["session_id"]);
    assert!(
        !call(
            &peer,
            "sessions.open",
            serde_json::json!({"profile_id":"p1"})
        )
        .await
        .0
    );
    assert_eq!(*confirm.strong.lock().unwrap(), vec![true, true, false]);
}

#[tokio::test]
async fn failed_and_rejected_opens_do_not_enter_seen_ledger() {
    let confirm = Arc::new(ProbeConfirm::default());
    let peer = concurrency_peer(
        probe_dir(false),
        probe_command(),
        confirm.clone(),
        Arc::new(ProbeAudit::default()),
    )
    .await;
    for _ in 0..2 {
        let (is_error, structured, text) = call(
            &peer,
            "sessions.open",
            serde_json::json!({"profile_id":"failed-open"}),
        )
        .await;
        assert!(is_error);
        assert_eq!(structured.unwrap()["code"], "FS_MCP_OPERATION_FAILED");
        assert!(text.contains("synthetic open failure"));
    }
    confirm.reject.store(true, Ordering::SeqCst);
    assert!(
        call(
            &peer,
            "sessions.open",
            serde_json::json!({"profile_id":"rejected"})
        )
        .await
        .0
    );
    confirm.reject.store(false, Ordering::SeqCst);
    assert!(
        !call(
            &peer,
            "sessions.open",
            serde_json::json!({"profile_id":"rejected"})
        )
        .await
        .0
    );
    assert_eq!(
        *confirm.strong.lock().unwrap(),
        vec![true, true, true, true]
    );
}

#[tokio::test]
async fn successful_open_is_remembered_when_settled_audit_fails() {
    let confirm = Arc::new(ProbeConfirm::default());
    let audit = Arc::new(ProbeAudit::default());
    audit.fail_settled.store(true, Ordering::SeqCst);
    let dir = probe_dir(false);
    let peer = concurrency_peer(dir.clone(), probe_command(), confirm.clone(), audit).await;
    let first = call(
        &peer,
        "sessions.open",
        serde_json::json!({"profile_id":"audit-failure"}),
    )
    .await;
    assert!(first.0);
    let error = first.1.unwrap();
    assert_eq!(error["code"], "FS_MCP_AUDIT_FAILED");
    assert_eq!(error["details"]["stage"], "settled");
    assert_eq!(error["details"]["operation_attempted"], true);
    assert!(!error.to_string().contains("hunter2"));
    assert!(error["message"].as_str().unwrap().contains("中文"));
    assert!(error["message"].as_str().unwrap().contains("redacted"));
    assert!(error["details"]["operation_result"]["session_id"]
        .as_str()
        .unwrap()
        .starts_with("s-"));
    assert_eq!(
        dir.inner.sessions.lock().unwrap().len(),
        2,
        "first open actually succeeded despite audit failure"
    );
    assert!(
        !call(
            &peer,
            "sessions.open",
            serde_json::json!({"profile_id":"audit-failure"})
        )
        .await
        .0
    );
    assert_eq!(*confirm.strong.lock().unwrap(), vec![true, false]);
}

#[tokio::test]
async fn port_failure_is_a_tool_level_error_over_rmcp() {
    let dir = probe_dir(false);
    let peer = concurrency_peer(
        dir,
        probe_command(),
        Arc::new(ProbeConfirm::default()),
        Arc::new(ProbeAudit::default()),
    )
    .await;
    let (is_error, structured, _) = call(
        &peer,
        "sessions.open",
        serde_json::json!({"profile_id":"failed-open"}),
    )
    .await;
    assert!(is_error);
    let error = structured.unwrap();
    assert_eq!(error["code"], "FS_MCP_OPERATION_FAILED");
    assert!(error["message"]
        .as_str()
        .unwrap()
        .contains("synthetic open failure"));
    assert!(error["message"].as_str().unwrap().contains("中文"));
    assert!(error["message"].as_str().unwrap().contains("redacted"));
    assert!(!error.to_string().contains("hunter2"));
}
