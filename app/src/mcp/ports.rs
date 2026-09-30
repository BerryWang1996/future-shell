//! 六个 MCP 端口在 app 层的真实现。
//!
//! 这里的每一格都只是「翻译」：把 `fs_mcpbridge` 的端口调用翻成本应用既有的
//! 设施（注册表 / 网格 / russh / 审计库）。**不引入任何新的裁决逻辑**——
//! 分级、授权、确认收敛全在 `fs_mcpbridge`，已测试钉死。
//!
//! 唯一有裁量权的一处是 [`map_verdict`]（`McpVerdict` → 审计枚举），它用一张
//! 逐格写死的 `match` 表达，且测试断言七格全覆盖、无 `_` 兜底——新增变体时
//! 编译器会逼着回答「这一格落到审计的哪一档」。

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Instant;

use fs_ai::agent::ports::{AbortSignal, RunClock};
use fs_ai::agent::tool::Tool;
use fs_ai::exec::{ExecMode, Observation};
use fs_connmgr::audit_repo::{Actor, NewAuditEntry, Verdict as AuditVerdict};
use fs_connmgr::ProfileRepo;
use fs_mcpbridge::confirm::{ConfirmTicket, MCP_CONFIRM_TIMEOUT};
use fs_mcpbridge::ports::{
    CommandPort, ConfirmReply, McpAuditPort, McpCallRecord, McpConfirmPort, McpVerdict, PortResult,
    SessionDirectory, SessionSummary, SftpPort, TerminalPort,
};
use fs_mcpbridge::server::CallContext;
use fs_mcpbridge::tool::ReadSource;
use fs_mcpbridge::transport::McpContextSource;
use tauri::Emitter;
use tokio::sync::oneshot;

use fs_sshengine::sftp::SftpOps;

use crate::agent::exec_port::RusshExecPort;
use crate::state::AppState;

type Fut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

// ── 上下文源 ────────────────────────────────────────────────────────────────

/// 每次调用现读档位与机密。见 [`McpContextSource`] 的文档——不缓存。
pub struct McpAppContext {
    pub state: Arc<AppState>,
    pub caller: String,
}

impl McpContextSource for McpAppContext {
    fn current<'a>(&'a self) -> Fut<'a, CallContext> {
        Box::pin(async move {
            // 与 AI 面板同一份读法：同一个键、同一个逆函数、同一个缺省档。
            let mode = crate::commands::ai_cmd::read_mode(&self.state).await;
            let known_secrets = crate::commands::ai_cmd::known_secrets(&self.state).await;
            CallContext {
                caller: self.caller.clone(),
                mode,
                known_secrets,
            }
        })
    }
}

// ── 会话目录 ────────────────────────────────────────────────────────────────

pub struct McpSessions {
    pub state: Arc<AppState>,
}

/// 注册表里那条会话记着的 profile_id → 连接名与主机。
///
/// 从**注册表反查**而不是信任调用方给的 id（同 `ai_cmd::profile_facts_of` 的
/// 理由）：MCP 调用方给什么都可能，而「这个会话是谁」只能以本端事实为准。
async fn profile_name_host(state: &AppState, profile_id: &str) -> (String, String) {
    let Ok(uuid) = uuid::Uuid::parse_str(profile_id) else {
        return (String::new(), String::new());
    };
    match ProfileRepo::new(state.db.pool()).get(uuid).await {
        Ok(p) => (p.name, p.host),
        Err(_) => (String::new(), String::new()),
    }
}

impl SessionDirectory for McpSessions {
    fn list(&self) -> Fut<'_, Vec<SessionSummary>> {
        Box::pin(async move {
            // 持表锁内只抄一份 (id, profile_id)，查库在锁外——查库是 await，
            // 持锁跨 await 会把所有会话操作冻住。
            let pairs: Vec<(String, String)> = {
                let guard = self.state.registry.sessions.lock().unwrap();
                guard
                    .iter()
                    .map(|(id, e)| (id.clone(), e.session.profile_id.clone()))
                    .collect()
            };
            let mut out = Vec::with_capacity(pairs.len());
            for (id, profile_id) in pairs {
                let (name, host) = profile_name_host(&self.state, &profile_id).await;
                out.push(SessionSummary {
                    id,
                    name,
                    host,
                    connected: true, // 在注册表里即已连接；断线的会被摘出注册表
                });
            }
            out
        })
    }

    fn is_alive(&self, session_id: &str) -> bool {
        self.state.registry.get(session_id).is_some()
    }

    fn mcp_allowed(&self, profile_id: &str) -> Fut<'_, Option<bool>> {
        let profile_id = profile_id.to_string();
        Box::pin(async move {
            let Ok(uuid) = uuid::Uuid::parse_str(&profile_id) else {
                return None; // 解析不出 uuid ⇒ 等同查无此 profile
            };
            match ProfileRepo::new(self.state.db.pool()).get(uuid).await {
                Ok(p) => Some(p.ai_policy.mcp_allowed),
                Err(_) => None,
            }
        })
    }

    fn open(&self, profile_id: &str) -> Fut<'_, PortResult<String>> {
        let profile_id = profile_id.to_string();
        Box::pin(async move {
            // 与前端「连接」走**同一条**装配序列（connect + establish + journal）。
            // PortResult 是 mcpbridge crate 定的 Result<T, String>，改不动；结构化失败
            // 在这条线上只留 summary（Display 原文）——MCP 客户端拿到的与此前一字不差。
            crate::commands::session_cmd::open_session_core(self.state.clone(), profile_id)
                .await
                .map_err(|f| f.summary)
        })
    }
}

// ── 终端 ────────────────────────────────────────────────────────────────────

pub struct McpTerminal {
    pub state: Arc<AppState>,
}

impl TerminalPort for McpTerminal {
    fn read_text(
        &self,
        session_id: &str,
        source: ReadSource,
        max_lines: Option<usize>,
    ) -> Fut<'_, PortResult<String>> {
        let session_id = session_id.to_string();
        Box::pin(async move {
            let Some(session) = self.state.registry.get(&session_id) else {
                return Err("会话不存在或已关闭".into());
            };
            // screen = 当前可见屏；scrollback/all = 回看+可见屏的末尾 N 行
            //（网格侧没有「纯回看不含当前屏」的取法，见 fs_terminal::pipe 文档）。
            Ok(match source {
                ReadSource::Screen => session.pipe.grid_text(),
                ReadSource::Scrollback | ReadSource::All => {
                    session.pipe.scrollback_text(max_lines.unwrap_or(200))
                }
            })
        })
    }

    fn send(&self, session_id: &str, payload: &str) -> Fut<'_, PortResult<()>> {
        let session_id = session_id.to_string();
        let payload = payload.as_bytes().to_vec();
        Box::pin(async move {
            let Some(session) = self.state.registry.get(&session_id) else {
                return Err("会话不存在或已关闭".into());
            };
            // 与 term_input 同一条写半部（Mutex 串行化保字节顺序）。
            let w = session.write.lock().await;
            w.data_bytes(payload)
                .await
                .map_err(|e| format!("写入失败：{e}"))
        })
    }
}

// ── 命令执行 ────────────────────────────────────────────────────────────────

/// MCP 侧的急停信号：stdio 传输没有「急停按钮」这一说，传输断开由
/// `serve_stdio` 的 `waiting()` 收尾。故这里给一个**永不触发**的信号——
/// 超时仍由 `run_command` 自己的 60 秒契约兜底（那条契约不依赖急停）。
struct NoAbort;

impl AbortSignal for NoAbort {
    fn requested(&self) -> bool {
        false
    }
    fn fired<'a>(&'a self) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        Box::pin(std::future::pending())
    }
}

pub struct McpCommand {
    pub state: Arc<AppState>,
}

impl McpCommand {
    async fn exec(&self, session_id: &str, tool: Tool) -> PortResult<Observation> {
        let Some(session) = self.state.registry.get(session_id) else {
            return Err("会话不存在或已关闭".into());
        };
        // 复用 Agent 的 RusshExecPort：同一条 `run_command` 契约、同一套
        // kill-before-close 与心跳读。ChannelRegistry 在这里只起「通道计数」的
        // 诊断作用，每次调用给一个新的即可。
        let port = RusshExecPort::new(
            session,
            Arc::new(fs_ai::agent::ports::ChannelRegistry::default()),
            self.state.db.clone(),
        );
        let clock = RunClock::new(Arc::new(NoAbort));
        use fs_ai::agent::ports::ExecPort;
        Ok(port.run(&tool, &clock).await)
    }
}

impl CommandPort for McpCommand {
    fn run(&self, session_id: &str, command: &str) -> Fut<'_, PortResult<Observation>> {
        let session_id = session_id.to_string();
        let command = command.to_string();
        Box::pin(async move {
            self.exec(
                &session_id,
                Tool::RunCommand {
                    command,
                    mode: ExecMode::Exec,
                },
            )
            .await
        })
    }
}

// ── SFTP ────────────────────────────────────────────────────────────────────

pub struct McpSftp {
    pub state: Arc<AppState>,
}

/// `Observation` → 文本端口的返回值：非零退出码即失败。
///
/// read/list 走的是 `RusshExecPort` 的 `head -c` / `ls -la`（上限在**远端**
/// 生效），失败原因在 stderr——原样带回，让调用方知道为什么没读到。
fn obs_to_text(obs: Observation) -> PortResult<String> {
    match obs.exit_code {
        Some(0) | None => Ok(obs.stdout),
        Some(_) => {
            let mut msg = obs.stderr;
            if msg.is_empty() {
                msg = format!("命令退出码 {:?}", obs.exit_code);
            }
            Err(msg)
        }
    }
}

impl McpSftp {
    /// 取该会话的 [`RusshExecPort`]。read/list 走它的**专用方法**
    ///（`read_remote_file`/`list_remote_dir`）——通用 `run` 只认 `RunCommand`，
    /// 其余工具喂进去会拿回一个空 `Observation`（exec_port 的 `_ =>` 兜底臂）。
    async fn exec_port(&self, session_id: &str) -> PortResult<RusshExecPort> {
        let Some(session) = self.state.registry.get(session_id) else {
            return Err("会话不存在或已关闭".into());
        };
        Ok(RusshExecPort::new(
            session,
            Arc::new(fs_ai::agent::ports::ChannelRegistry::default()),
            self.state.db.clone(),
        ))
    }
}

impl SftpPort for McpSftp {
    fn list(&self, session_id: &str, path: &str) -> Fut<'_, PortResult<String>> {
        let session_id = session_id.to_string();
        let path = path.to_string();
        Box::pin(async move {
            use fs_ai::agent::ports::ExecPort;
            let port = self.exec_port(&session_id).await?;
            obs_to_text(port.list_remote_dir(&path).await)
        })
    }

    fn read(
        &self,
        session_id: &str,
        path: &str,
        max_bytes: Option<usize>,
    ) -> Fut<'_, PortResult<String>> {
        let session_id = session_id.to_string();
        let path = path.to_string();
        Box::pin(async move {
            use fs_ai::agent::ports::ExecPort;
            let port = self.exec_port(&session_id).await?;
            let obs = port
                .read_remote_file(
                    &path,
                    max_bytes.unwrap_or(fs_ai::exec::DEFAULT_MAX_OUTPUT_BYTES),
                )
                .await;
            obs_to_text(obs)
        })
    }

    fn write(&self, session_id: &str, path: &str, content: &str) -> Fut<'_, PortResult<()>> {
        let session_id = session_id.to_string();
        let path = path.to_string();
        let content = content.as_bytes().to_vec();
        Box::pin(async move {
            // 走 SFTP 子系统（与传输/文件面板同一条通道管理），不走 exec：
            // 内容经 `cat >` 管道要过一层远端 shell 转义，二进制内容会被破坏。
            let ops = self.state.sftp_ops_for(&session_id).await?;
            // write_at 带 CREATE 但**不截断**：先写到 0 偏移，再把文件截到内容长度，
            // 才是「替换」语义——否则新内容比旧文件短时，尾巴上会留着旧文件的残片。
            ops.write_at(&path, 0, &content)
                .await
                .map_err(|e| format!("写入失败：{e}"))?;
            ops.truncate(&path, content.len() as u64)
                .await
                .map_err(|e| format!("截断失败：{e}"))
        })
    }
}

// ── 确认 ────────────────────────────────────────────────────────────────────

pub struct McpConfirm {
    pub confirms: Arc<crate::mcp::McpConfirms>,
    pub app: tauri::AppHandle,
}

impl McpConfirmPort for McpConfirm {
    fn confirm(&self, ticket: ConfirmTicket) -> Fut<'_, ConfirmReply> {
        Box::pin(async move {
            let id = self.confirms.next_id();
            let (tx, rx) = oneshot::channel();
            self.confirms.insert(id, tx);
            // 入队 + 引导用户到 FutureShell（§4.5）。前端确认组件消费此事件。
            let payload = serde_json::json!({
                "request_id": id,
                "tool": ticket.tool_name,
                "caller": ticket.caller,
                "tier": ticket.tier.as_str(),
                "strong": ticket.strong,
                "display": ticket.display.as_str(),
                "details": ticket.details.iter().map(|d| d.as_str()).collect::<Vec<_>>(),
                "session_id": ticket.session_id,
            });
            let _ = self.app.emit("mcp:confirm", payload);

            let start = Instant::now();
            // 两臂：应答 / 到点。**120 秒是规格给的上限**（MCP_CONFIRM_TIMEOUT），
            // 超时臂自清待答项，迟到的 `mcp_confirm_answer` 将无处可答。
            let answer = tokio::select! {
                r = rx => r.ok(), // 发送端被丢弃（如窗口销毁）⇒ None
                _ = tokio::time::sleep(MCP_CONFIRM_TIMEOUT) => {
                    self.confirms.remove(id);
                    None
                }
            };
            // context_alive 恒真：应用进程本身活着。「会话在不在」由
            // `server::call` 在确认返回后**重新问一次** is_alive 兜底——
            // 那一问才是权威的，这里不越权代答。
            ConfirmReply {
                answer,
                elapsed: start.elapsed(),
                context_alive: true,
            }
        })
    }
}

// ── 审计 ────────────────────────────────────────────────────────────────────

/// `McpVerdict` → 审计落库枚举。**七格逐一写死，不留 `_`**：新增变体时
/// 编译器强制回答映射，而不是悄悄落进某个「差不多」的档。
pub fn map_verdict(v: McpVerdict) -> AuditVerdict {
    match v {
        McpVerdict::AutoRun => AuditVerdict::AutoRun,
        McpVerdict::Approved => AuditVerdict::Approved,
        McpVerdict::Rejected => AuditVerdict::Rejected,
        McpVerdict::TimedOut => AuditVerdict::TimedOut,
        McpVerdict::Abandoned => AuditVerdict::Abandoned,
        McpVerdict::Denied => AuditVerdict::Denied,
        McpVerdict::RequestOnly => AuditVerdict::RequestOnly,
    }
}

pub struct McpAudit {
    pub db: Arc<fs_connmgr::Db>,
}

impl McpAuditPort for McpAudit {
    fn append(&self, rec: McpCallRecord) -> Fut<'_, PortResult<()>> {
        Box::pin(async move {
            let entry = NewAuditEntry {
                actor: Actor::Mcp {
                    caller: rec.caller.clone(),
                },
                // display/output 在 fs_mcpbridge 的单一脱敏点已经脱过。这个显式
                // 类型绑定不是为了编译——是为了让「本行进审计的 action 来自
                // 脱敏保证类型」在**写入点**肉眼可见（`every_audit_writer_routes_through_redact`
                // 守卫认的模式③）。去掉它，守卫无从确认这一点。
                action: {
                    let display: &fs_ai::agent::record::RedactedAction = &rec.display;
                    format!("[{}] {}", rec.tool_name, display.as_str())
                },
                target_session: rec.session_id.clone(),
                risk_level: rec.tier.as_str().to_string(),
                verdict: map_verdict(rec.verdict),
                exit_code: rec.exit_code.map(|c| c as i64),
                output_digest: rec
                    .output
                    .as_ref()
                    .map(|o| fs_connmgr::audit_repo::digest_output(o.as_str().as_bytes())),
                created_at: crate::commands::ai_cmd::now_rfc3339(),
            };
            fs_connmgr::audit_repo::AuditRepo::new(self.db.pool())
                .append(&entry)
                .await
                .map(|_| ())
                .map_err(|e| e.to_string())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 映射表七格全覆盖、各不相撞。
    ///
    /// 用「七个变体 → 七个**互不相同**的审计档」来钉：任何一格映错或映重，
    /// 集合大小就不对。这比逐格 `assert_eq` 更能防「两格映到同一档」这种
    /// 逐格看也看不出问题的错。
    #[test]
    fn all_seven_mcp_verdicts_map_to_distinct_audit_verdicts() {
        let all = [
            McpVerdict::AutoRun,
            McpVerdict::Approved,
            McpVerdict::Rejected,
            McpVerdict::TimedOut,
            McpVerdict::Abandoned,
            McpVerdict::Denied,
            McpVerdict::RequestOnly,
        ];
        let mapped: Vec<AuditVerdict> = all.iter().map(|v| map_verdict(*v)).collect();
        let uniq: std::collections::BTreeSet<String> =
            mapped.iter().map(|v| v.as_str().to_string()).collect();
        assert_eq!(uniq.len(), 7, "七格必须映到七个不同的档：{mapped:?}");
        // 逐格点一条最要紧的：执行类不得映成非执行类。
        assert!(map_verdict(McpVerdict::Approved).executed());
        assert!(map_verdict(McpVerdict::AutoRun).executed());
        assert!(!map_verdict(McpVerdict::Rejected).executed());
        assert!(!map_verdict(McpVerdict::Denied).executed());
    }

    /// `obs_to_text`：非零退出码 ⇒ 失败，stderr 原样带回。
    #[test]
    fn a_nonzero_exit_code_is_a_failure_with_stderr_preserved() {
        let ok = obs_to_text(Observation {
            stdout: "body".into(),
            exit_code: Some(0),
            ..Default::default()
        });
        assert_eq!(ok, Ok("body".to_string()));

        let bad = obs_to_text(Observation {
            stdout: String::new(),
            stderr: "没有那个文件".into(),
            exit_code: Some(1),
            ..Default::default()
        });
        assert_eq!(bad, Err("没有那个文件".to_string()));

        // 无退出码（超时/读错）按成功路径处理——观察值本身已带 timed_out 标志，
        // 由调用方读结构化输出判断，不在这里二次定罪。
        let no_code = obs_to_text(Observation {
            stdout: "partial".into(),
            exit_code: None,
            ..Default::default()
        });
        assert_eq!(no_code, Ok("partial".to_string()));
    }
}
