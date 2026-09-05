//! 对外 MCP Server 的派发（总设计 §4.5）。
//!
//! # 检查顺序是个定义，不是实现巧合
//!
//! ```text
//! 授权 → 解析 → 会话在不在 → 分类 → 裁决 → 确认 → 审计(Intent) → 执行 → 审计(Settled)
//! ```
//!
//! 每一步为什么在它现在的位置：
//!
//! - **授权在最前，且按线上名字**：这样「这个工具不存在」与「它存在但你没
//!   被授权」走的是同一条出口、同一句文案。分开的话，两句不同的话就是一台
//!   枚举器——调用方逐个试名字，就能问出本机支持哪些工具而只是没给它。
//!   M3 出口第 5 项要的「未授权工具不可枚举」，光靠 `tools/list` 里不列是
//!   不够的，回包也不能说漏嘴。
//! - **解析在授权之后**：走到解析的都是有权使用的工具，于是 `BadArgs`
//!   泄露的参数形状只泄露给本来就该知道的人。同时未知名字仍然到不了参数
//!   解析——那条更早的约束被更强地满足了。
//! - **授权在闸门之前**：授权层判拒返回 `FS_POLICY_FORBIDDEN`，而那个码
//!   「不可经确认放行」。放到确认之后就会弹出一个**批准了也照样失败**的
//!   对话框，那是在教用户对话框可以随便点。
//! - **会话检查在分类之前**：对一个不存在的会话做分类是白做，而且
//!   `sessions.list` 之外的工具都吃 `session_id`，让它成为一个存在性探针
//!   不值当。
//! - **确认之后才是审计与执行**：见 [`crate::ports::RecordPhase`]。
//!
//! 这个顺序在 `the_checks_happen_in_exactly_this_order` 里被逐步钉住——
//! 不是靠读代码确认，而是靠一组「只在这一步该失败的地方失败」的用例。
//!
//! # 这个文件是唯一同时看得见闸门与端口的地方
//!
//! 纯逻辑层不认识端口，端口不认识闸门。两者只在这里相遇，于是「绕过闸门
//! 直接调端口」这件事只可能发生在这一个文件里——而这一个文件有守卫。

use fs_policy::{AiMode, Decision};

use crate::authz::{authorize_open, Authorization, OpenAuthz, OpenLedger};
use crate::confirm::{conclude, Conclusion, ConfirmTicket};
use crate::gate::{classify_for_mcp, decide_for_mcp, deny_details, rules_of, McpGated};
use crate::ports::{
    CommandPort, McpAuditPort, McpCallRecord, McpConfirmPort, McpVerdict, RecordPhase,
    SessionDirectory, SftpPort, TerminalPort,
};
use crate::reject::ToolError;
use crate::tool::{parse_tool, McpTool, ParseError};

/// 一次调用要用到的全部外界接触面。
pub struct Ports<'a> {
    pub sessions: &'a dyn SessionDirectory,
    pub terminal: &'a dyn TerminalPort,
    pub command: &'a dyn CommandPort,
    pub sftp: &'a dyn SftpPort,
    pub confirm: &'a dyn McpConfirmPort,
    pub audit: &'a dyn McpAuditPort,
}

/// 一次调用的上下文（每次调用都会变的那些东西）。
pub struct CallContext {
    /// 客户端标签。**展示用，非安全边界**（§4.5 信任边界）。
    pub caller: String,
    /// 全局 AI 档位（已经过会话级覆盖）。
    pub mode: AiMode,
    /// 已知机密，进脱敏。
    pub known_secrets: Vec<String>,
}

/// 一次调用的产出。
///
/// 成功那一支带的是**已脱敏**的文本——`terminal.read` 的返回值会原样进入
/// 外部模型的上下文，规格明写它要「经 §6.2 同一脱敏过滤器」。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallOutput {
    Ok {
        text: String,
        /// 结构化补充（`command.run` 的 `Observation` 等）。
        structured: Option<serde_json::Value>,
    },
    Err(ToolError),
}

impl CallOutput {
    fn err(e: ToolError) -> Self {
        Self::Err(e)
    }
    pub fn is_err(&self) -> bool {
        matches!(self, Self::Err(_))
    }
    pub fn error_code(&self) -> Option<&'static str> {
        match self {
            Self::Err(e) => Some(e.code),
            Self::Ok { .. } => None,
        }
    }
}

/// 解析错误按 MCP 的口径回话。
///
/// 两种解析错误都用 `FORBIDDEN`：它们都不是「策略不让你做」，而是
/// 「这个请求本身没法当成一次调用来看」。用 `DENIED` 会让调用方以为
/// 换个档位就能过。
fn parse_error_output(e: ParseError) -> CallOutput {
    CallOutput::err(ToolError::forbidden(e.to_string()))
}

/// 会话不存在或已断开。
fn no_such_session() -> CallOutput {
    CallOutput::err(ToolError::forbidden(
        "指定的会话不存在或已断开。请先用 sessions.list 看看有哪些可用的会话。",
    ))
}

/// 给人看的一行话：「工具名: 要做什么」。进确认框与审计。
fn display_of(t: &McpTool) -> String {
    let what = match t {
        McpTool::SessionsList => String::new(),
        McpTool::SessionsOpen { profile_id } => profile_id.clone(),
        McpTool::TerminalRead { source, .. } => format!("{source:?}"),
        McpTool::TerminalSend { payload, .. } | McpTool::TerminalSendRaw { payload, .. } => {
            payload.clone()
        }
        McpTool::CommandRun { command, .. } => command.clone(),
        McpTool::SftpList { path, .. } | McpTool::SftpRead { path, .. } => path.clone(),
        McpTool::SftpWrite { path, content, .. } => format!("{path}（{} 字节）", content.len()),
    };
    if what.is_empty() {
        t.name().to_string()
    } else {
        format!("{}: {what}", t.name())
    }
}

/// `tools/list`：**从清单过滤**，过滤器就是授权层。
///
/// 单独一个函数而不是让传输层自己拼，是为了让「未授权工具不可枚举」
/// 只有一个实现。
pub fn list_tools(authz: &Authorization) -> Vec<&'static crate::tool::ManifestEntry> {
    authz.enumerable()
}

/// `tools/call` 的唯一入口。**顺序见模块头。**
pub async fn call(
    name: &str,
    args_json: &str,
    ctx: &CallContext,
    authz: &Authorization,
    ledger: &mut OpenLedger,
    ports: &Ports<'_>,
) -> CallOutput {
    // ── ① 授权（**按线上名字，在解析之前**）─────────────────────────────
    //
    // 顺序曾经是「先解析后授权」，那是错的：解析失败的文案是「没有名为 X
    // 的工具」，授权失败的文案是「这个工具在当前配置下不可用」——两句话不同，
    // 于是调用方拿回包一比就知道哪些工具**存在但没授权**。那正是 M3 出口第 5 项
    // 「未授权工具不可枚举」要挡的事。（这个洞是被 server.rs 的
    // `a_forbidden_tool_is_indistinguishable_from_a_nonexistent_one` 抓出来的，
    // 而它第一版只比了 code 与 tier，没比文案，所以没抓住——现在比了。）
    //
    // 按名字先授权还顺带把「未知名字到不了参数解析」做得更彻底：现在连
    // 已知但未授权的工具，参数都不会被解析，也就不会有 BadArgs 泄露它的形状。
    //
    // 未授权的调用**不写审计**：写了就等于替调用方确认了这个工具存在，
    // 而审计是会被人读的。这一步的正确记录是「有个客户端在乱试」，
    // 那属于传输层的连接日志，不属于操作审计。
    if let Err(e) = authz.check(name) {
        return CallOutput::err(e);
    }

    // ── ② 解析 ────────────────────────────────────────────────────────
    //
    // 走到这里说明这个名字既存在于清单、又被授权了，所以 `BadArgs` 只会
    // 泄露给一个本来就有权使用它的调用方。
    let tool = match parse_tool(name, args_json) {
        Ok(t) => t,
        Err(e) => return parse_error_output(e),
    };
    debug_assert_eq!(tool.name(), name, "解析出来的工具名必须就是请求的那个");

    // ── ③ 会话在不在（分类之前）────────────────────────────────────────
    if let Some(sid) = tool.session_id() {
        if !ports.sessions.is_alive(sid) {
            return no_such_session();
        }
    }

    // `sessions.open` 的第二道门：profile 必须被标记允许 MCP 打开。
    // 这一步仍属授权层，所以也在闸门之前。
    let open_authz = match &tool {
        McpTool::SessionsOpen { profile_id } => {
            let allowed = ports.sessions.mcp_allowed(profile_id).await;
            match authorize_open(allowed, &ctx.caller, profile_id, ledger) {
                OpenAuthz::Forbidden(e) => return CallOutput::err(e),
                other => Some(other),
            }
        }
        _ => None,
    };

    // ── ④ 分类 ────────────────────────────────────────────────────────
    let gated = classify_for_mcp(&tool, &ctx.caller);

    // ── ⑤ 裁决 ────────────────────────────────────────────────────────
    let mut decision = decide_for_mcp(&gated, ctx.mode);

    // 首次 (client, profile) 提级到强确认。**只提不降**：`escalates_to_strong`
    // 为假时什么也不做，而不是把裁决降成 AutoRun。
    if open_authz.as_ref().is_some_and(|a| a.escalates_to_strong()) && decision == Decision::Confirm
    {
        decision = Decision::StrongConfirm;
    }

    if let Decision::Deny(reason) = decision {
        let e = ToolError::denied_by_gate(gated.tier(), deny_details(&gated, reason).join("；"));
        return settle(&tool, &gated, ctx, ports, McpVerdict::Denied, None, None, e).await;
    }

    dispatch_after_gate(tool, gated, decision, open_authz, ctx, ledger, ports).await
}

#[allow(clippy::too_many_arguments)]
async fn dispatch_after_gate(
    tool: McpTool,
    gated: McpGated,
    decision: Decision,
    open_authz: Option<OpenAuthz>,
    ctx: &CallContext,
    ledger: &mut OpenLedger,
    ports: &Ports<'_>,
) -> CallOutput {
    let raw_display = display_of(&tool);

    // ── ⑥ 确认 ────────────────────────────────────────────────────────
    let verdict = match ConfirmTicket::from_decision(
        decision,
        &gated,
        &raw_display,
        tool.session_id().map(|s| s.to_string()),
        &ctx.known_secrets,
    ) {
        // 不需要人：闸门放行。
        None => McpVerdict::AutoRun,
        Some(ticket) => {
            let reply = ports.confirm.confirm(ticket).await;
            // 收敛那一瞬间再问一次会话在不在——**不复用第③步的结果**。
            // 那一步到现在之间隔着一整段人类思考的时间，用旧读数就是
            // 「刚才还在」冒充「现在还在」。
            let alive = reply.context_alive
                && tool
                    .session_id()
                    .map(|s| ports.sessions.is_alive(s))
                    .unwrap_or(true);
            match conclude(reply.answer, reply.elapsed, alive, gated.tier()) {
                Conclusion::Proceed => McpVerdict::Approved,
                Conclusion::Refused(e) => {
                    let v = match e.code {
                        "FS_POLICY_TIMEOUT" => McpVerdict::TimedOut,
                        "FS_POLICY_ABORTED" => McpVerdict::Abandoned,
                        _ => McpVerdict::Rejected,
                    };
                    return settle(&tool, &gated, ctx, ports, v, None, None, e).await;
                }
            }
        }
    };

    // ── ⑦ 审计（Intent）：写不进去就不执行 ──────────────────────────────
    let intent = McpCallRecord {
        phase: RecordPhase::Intent,
        tool_name: gated.tool_name,
        caller: ctx.caller.clone(),
        display: fs_ai::agent::record::RedactedAction::new(&raw_display, &ctx.known_secrets),
        session_id: tool.session_id().map(|s| s.to_string()),
        tier: gated.tier(),
        verdict,
        rules: rules_of(&gated),
        exit_code: None,
        output: None,
    };
    if let Err(why) = ports.audit.append(intent).await {
        return CallOutput::err(ToolError::forbidden(format!(
            "审计写入失败，本次调用未执行：{why}"
        )));
    }

    // ── ⑧ 执行 ────────────────────────────────────────────────────────
    let ran = execute(&tool, ports).await;

    // 会话真的开起来了才记台账——在「用户批准」那一刻记，会让一次因为
    // 网络失败而没开成的尝试也算数，下次就不问了。
    if let (McpTool::SessionsOpen { profile_id }, Ok(_)) = (&tool, &ran) {
        if open_authz.is_some() {
            ledger.remember(&ctx.caller, profile_id);
        }
    }

    // 执行产出的一切——成功的输出、失败的原因——在这里**一次性**脱敏。
    //
    // 不在 `execute` 里脱：端口适配器不该知道什么是机密。也不让两个下游
    // （回包与审计行）各脱一次：那样总有一天只改了其中一处，而先被忘掉的
    // 永远是审计那一处（没人盯着它看）。这里脱一次，两边用**同一份**。
    let exit_code = ran.as_ref().ok().and_then(|e| e.exit_code);
    let raw = match &ran {
        Ok(e) => e.text.clone(),
        // 端口失败不是策略拒绝——用 FORBIDDEN 会让调用方以为改档位有用。
        Err(why) => format!("操作失败：{why}"),
    };
    let text = fs_ai::agent::record::RedactedAction::new(&raw, &ctx.known_secrets);

    // ── ⑨ 审计（Settled）──────────────────────────────────────────────
    let settled = McpCallRecord {
        phase: RecordPhase::Settled,
        tool_name: gated.tool_name,
        caller: ctx.caller.clone(),
        display: fs_ai::agent::record::RedactedAction::new(&raw_display, &ctx.known_secrets),
        session_id: tool.session_id().map(|s| s.to_string()),
        tier: gated.tier(),
        verdict,
        rules: rules_of(&gated),
        exit_code,
        output: Some(text.clone()),
    };
    if let Err(why) = ports.audit.append(settled).await {
        // 动作已经发生了。**如实说**——链上会留下一条有 Intent 没有 Settled
        // 的记录，那是可见的缺口，比一条假装一切正常的成功回包好。
        return CallOutput::err(ToolError::forbidden(format!(
            "操作已执行，但结果未能写入审计（{why}）。请到 FutureShell 检查审计链完整性。"
        )));
    }

    // 回包与审计行用的是**同一个** `text`，不可能一处脱了另一处没脱。
    // `structured` 也在**这一处**脱：它是端口给的原始值（`command.run` 的
    // Observation 里就带 stdout/stderr），若不脱就进结构化输出，等于给机密留了
    // 一条绕过唯一脱敏点的旁路。递归脱掉所有字符串叶子。
    let refs: Vec<&str> = ctx.known_secrets.iter().map(|s| s.as_str()).collect();
    CallOutput::Ok {
        text: text.as_str().to_string(),
        structured: ran
            .ok()
            .and_then(|e| e.structured)
            .map(|v| redact_value(&v, &refs)),
    }
}

/// 把一个结构化值里**所有字符串叶子**脱敏。
///
/// 为什么是递归到叶子而不是只脱已知字段：结构化输出的形状由各端口决定
/// （今天是 Observation，明天可能是别的），只脱「我知道的那几个字段」迟早漏掉
/// 新加的一个；对字符串叶子一律过一遍脱敏，才不依赖「我记得有哪些字段」。
/// 非字符串值（数字/布尔/null）原样保留——它们装不下机密文本。
fn redact_value(v: &serde_json::Value, secrets: &[&str]) -> serde_json::Value {
    use serde_json::Value;
    match v {
        Value::String(s) => Value::String(fs_ai::redact::redact(s, secrets).text),
        Value::Array(items) => {
            Value::Array(items.iter().map(|x| redact_value(x, secrets)).collect())
        }
        Value::Object(map) => {
            let mut out = serde_json::Map::with_capacity(map.len());
            for (k, val) in map {
                out.insert(k.clone(), redact_value(val, secrets));
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

struct Executed {
    text: String,
    structured: Option<serde_json::Value>,
    exit_code: Option<i32>,
}

/// 把工具路由到对应的端口。**每一支都真的转调一个不同的方法**——
/// 全部塞给 `command.run` 是 M3 批次③ 真出过的一个 bug（四个特化方法一次
/// 都没被调过），那次是靠调用计数断言逮住的。
/// 它**不脱敏**——端口适配器不该知道什么是机密。脱敏在唯一的那一处
/// （`dispatch_after_gate` 里执行返回之后），回包与审计行共用同一份。
async fn execute(tool: &McpTool, ports: &Ports<'_>) -> Result<Executed, String> {
    let plain = |text: String| Executed {
        text,
        structured: None,
        exit_code: None,
    };
    match tool {
        McpTool::SessionsList => {
            let list = ports.sessions.list().await;
            let v = serde_json::json!(list
                .iter()
                .map(|s| serde_json::json!({
                    "id": s.id, "name": s.name, "host": s.host, "connected": s.connected
                }))
                .collect::<Vec<_>>());
            Ok(Executed {
                text: format!("{} 个会话", list.len()),
                structured: Some(v),
                exit_code: None,
            })
        }
        McpTool::SessionsOpen { profile_id } => {
            let id = ports.sessions.open(profile_id).await?;
            Ok(Executed {
                text: format!("会话已打开：{id}"),
                structured: Some(serde_json::json!({ "session_id": id })),
                exit_code: None,
            })
        }
        McpTool::TerminalRead {
            session_id,
            source,
            max_lines,
        } => ports
            .terminal
            .read_text(session_id, *source, *max_lines)
            .await
            .map(plain),
        McpTool::TerminalSend {
            session_id,
            payload,
        }
        | McpTool::TerminalSendRaw {
            session_id,
            payload,
        } => ports
            .terminal
            .send(session_id, payload)
            .await
            .map(|()| plain(format!("已发送 {} 字节", payload.len()))),
        McpTool::CommandRun {
            session_id,
            command,
        } => {
            let obs = ports.command.run(session_id, command).await?;
            // `Observation` 的 snake_case serde 输出直接进 structured_content
            // （rmcp spike 结论）。文本一路给模型读，结构化一路给程序读。
            let structured = serde_json::to_value(&obs).ok();
            let mut text = obs.stdout.clone();
            if !obs.stderr.is_empty() {
                text.push_str("\n[stderr]\n");
                text.push_str(&obs.stderr);
            }
            if obs.timed_out {
                text.push_str("\n[超时——流式/长驻命令请改用后台模式后轮询]");
            }
            Ok(Executed {
                text,
                structured,
                exit_code: obs.exit_code,
            })
        }
        McpTool::SftpList { session_id, path } => {
            ports.sftp.list(session_id, path).await.map(plain)
        }
        McpTool::SftpRead {
            session_id,
            path,
            max_bytes,
        } => ports
            .sftp
            .read(session_id, path, *max_bytes)
            .await
            .map(plain),
        McpTool::SftpWrite {
            session_id,
            path,
            content,
        } => ports
            .sftp
            .write(session_id, path, content)
            .await
            .map(|()| plain(format!("已写入 {path}（{} 字节）", content.len()))),
    }
}

/// 不动手的收尾：写一行 `Settled` 审计，然后把结构化错误发回去。
///
/// 审计写失败时**仍然返回原本的策略错误**，只是文案里带上审计失败。
/// 理由：这条路径上没有动作发生，所以调用方最需要知道的仍然是「为什么不让做」。
#[allow(clippy::too_many_arguments)]
async fn settle(
    tool: &McpTool,
    gated: &McpGated,
    ctx: &CallContext,
    ports: &Ports<'_>,
    verdict: McpVerdict,
    exit_code: Option<i32>,
    output: Option<fs_ai::agent::record::RedactedAction>,
    err: ToolError,
) -> CallOutput {
    debug_assert!(!verdict.executed(), "settle 只用于没动手的收尾");
    let rec = McpCallRecord {
        phase: RecordPhase::Settled,
        tool_name: gated.tool_name,
        caller: ctx.caller.clone(),
        display: fs_ai::agent::record::RedactedAction::new(&display_of(tool), &ctx.known_secrets),
        session_id: tool.session_id().map(|s| s.to_string()),
        tier: gated.tier(),
        verdict,
        rules: rules_of(gated),
        exit_code,
        output,
    };
    if let Err(why) = ports.audit.append(rec).await {
        let mut e = err;
        e.message = format!("{}（另：这次拒绝未能写入审计：{why}）", e.message);
        return CallOutput::err(e);
    }
    CallOutput::err(err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::{ConfirmReply, PortResult, SessionSummary};
    use crate::tool::ReadSource;
    use fs_ai::exec::Observation;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
    use std::sync::Mutex;
    use std::time::Duration;

    type F<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

    /// 每个端口方法各自的调用计数。
    ///
    /// **分开计数不是为了好看**：M3 批次③ 出过一个 bug——四个特化端口方法
    /// 一次都没被调过，全走了通用那条，而所有「结果对不对」的断言照样绿。
    /// 只有「这个方法恰好被调了 1 次、那个恰好 0 次」逮得住。
    #[derive(Default)]
    struct Calls {
        list: AtomicUsize,
        open: AtomicUsize,
        alive_asked: AtomicUsize,
        read_text: AtomicUsize,
        send: AtomicUsize,
        run: AtomicUsize,
        sftp_list: AtomicUsize,
        sftp_read: AtomicUsize,
        sftp_write: AtomicUsize,
        confirm: AtomicUsize,
    }

    impl Calls {
        /// 除了列出来的这些，其它端口方法一次都没被调过。
        fn only(&self, expected: &[(&str, usize)]) {
            let all: Vec<(&str, usize)> = vec![
                ("list", self.list.load(SeqCst)),
                ("open", self.open.load(SeqCst)),
                ("read_text", self.read_text.load(SeqCst)),
                ("send", self.send.load(SeqCst)),
                ("run", self.run.load(SeqCst)),
                ("sftp_list", self.sftp_list.load(SeqCst)),
                ("sftp_read", self.sftp_read.load(SeqCst)),
                ("sftp_write", self.sftp_write.load(SeqCst)),
            ];
            for (name, got) in &all {
                let want = expected
                    .iter()
                    .find(|(n, _)| n == name)
                    .map(|(_, c)| *c)
                    .unwrap_or(0);
                assert_eq!(*got, want, "{name} 的调用次数不对");
            }
            // 做空防护：期望表里的名字必须都是真名字，写错名字会让期望静默失效。
            for (n, _) in expected {
                assert!(
                    all.iter().any(|(a, _)| a == n),
                    "期望表里有个不存在的端口名 {n}"
                );
            }
        }

        fn executed(&self) -> usize {
            self.list.load(SeqCst)
                + self.open.load(SeqCst)
                + self.read_text.load(SeqCst)
                + self.send.load(SeqCst)
                + self.run.load(SeqCst)
                + self.sftp_list.load(SeqCst)
                + self.sftp_read.load(SeqCst)
                + self.sftp_write.load(SeqCst)
        }
    }

    /// 会话目录假实现。
    ///
    /// `alive_until` 是 C5 那条测试的关键：它让「第一次问在、第二次问不在」
    /// 可表达——生产代码在确认返回后**重新问一次** `is_alive`，若它复用第③步
    /// 的旧读数，这个假实现就抓不到它。
    struct Dir {
        calls: std::sync::Arc<Calls>,
        alive_until: usize,
        allowed: Mutex<std::collections::BTreeMap<String, bool>>,
        open_result: Mutex<PortResult<String>>,
        sessions: Vec<SessionSummary>,
    }

    impl Default for Dir {
        fn default() -> Self {
            Self {
                calls: Default::default(),
                alive_until: usize::MAX,
                allowed: Mutex::new(Default::default()),
                open_result: Mutex::new(Ok("new-session".to_string())),
                sessions: vec![SessionSummary {
                    id: "s1".into(),
                    name: "生产机".into(),
                    host: "prod.example.com".into(),
                    connected: true,
                }],
            }
        }
    }

    impl SessionDirectory for Dir {
        fn list(&self) -> F<'_, Vec<SessionSummary>> {
            self.calls.list.fetch_add(1, SeqCst);
            let v = self.sessions.clone();
            Box::pin(async move { v })
        }
        fn is_alive(&self, _sid: &str) -> bool {
            let n = self.calls.alive_asked.fetch_add(1, SeqCst);
            n < self.alive_until
        }
        fn mcp_allowed(&self, pid: &str) -> F<'_, Option<bool>> {
            let v = self.allowed.lock().unwrap().get(pid).copied();
            Box::pin(async move { v })
        }
        fn open(&self, _pid: &str) -> F<'_, PortResult<String>> {
            self.calls.open.fetch_add(1, SeqCst);
            let r = self.open_result.lock().unwrap().clone();
            Box::pin(async move { r })
        }
    }

    /// 终端假实现。`text` 是 read_text 要吐的内容（F2 用它塞机密）。
    struct Term {
        calls: std::sync::Arc<Calls>,
        text: String,
    }

    impl TerminalPort for Term {
        fn read_text(
            &self,
            _s: &str,
            _src: ReadSource,
            _m: Option<usize>,
        ) -> F<'_, PortResult<String>> {
            self.calls.read_text.fetch_add(1, SeqCst);
            let t = self.text.clone();
            Box::pin(async move { Ok(t) })
        }
        fn send(&self, _s: &str, _p: &str) -> F<'_, PortResult<()>> {
            self.calls.send.fetch_add(1, SeqCst);
            Box::pin(async move { Ok(()) })
        }
    }

    /// 命令假实现。`obs` 是要吐的观察值（F1 用它塞机密）。
    struct Cmd {
        calls: std::sync::Arc<Calls>,
        obs: Observation,
    }

    impl CommandPort for Cmd {
        fn run(&self, _s: &str, _c: &str) -> F<'_, PortResult<Observation>> {
            self.calls.run.fetch_add(1, SeqCst);
            let o = self.obs.clone();
            Box::pin(async move { Ok(o) })
        }
    }

    struct Sftp {
        calls: std::sync::Arc<Calls>,
    }

    impl SftpPort for Sftp {
        fn list(&self, _s: &str, _p: &str) -> F<'_, PortResult<String>> {
            self.calls.sftp_list.fetch_add(1, SeqCst);
            Box::pin(async move { Ok("a\nb\n".to_string()) })
        }
        fn read(&self, _s: &str, _p: &str, _m: Option<usize>) -> F<'_, PortResult<String>> {
            self.calls.sftp_read.fetch_add(1, SeqCst);
            Box::pin(async move { Ok("file body".to_string()) })
        }
        fn write(&self, _s: &str, _p: &str, _c: &str) -> F<'_, PortResult<()>> {
            self.calls.sftp_write.fetch_add(1, SeqCst);
            Box::pin(async move { Ok(()) })
        }
    }

    /// 确认假实现：录下收到的票，按脚本作答。
    struct Conf {
        calls: std::sync::Arc<Calls>,
        reply: ConfirmReply,
        /// 收到过的票。测试据此断言 strong 提级、脱敏等。
        seen: Mutex<Vec<(String, bool, String)>>,
        /// 置真时被调用即 panic——用来证明「根本没走到确认这一步」。
        never: bool,
    }

    impl Conf {
        fn approving() -> Self {
            Self {
                calls: Default::default(),
                reply: ConfirmReply {
                    answer: Some(true),
                    elapsed: Duration::from_secs(3),
                    context_alive: true,
                },
                seen: Mutex::new(Vec::new()),
                never: false,
            }
        }
        fn with(answer: Option<bool>, alive: bool) -> Self {
            let mut c = Self::approving();
            c.reply.answer = answer;
            c.reply.context_alive = alive;
            c
        }
        fn forbidden() -> Self {
            let mut c = Self::approving();
            c.never = true;
            c
        }
        fn last_strong(&self) -> bool {
            self.seen.lock().unwrap().last().expect("没有收到任何票").1
        }
    }

    impl McpConfirmPort for Conf {
        fn confirm(&self, t: ConfirmTicket) -> F<'_, ConfirmReply> {
            assert!(!self.never, "这条路径不该走到确认——弹一个批准了也没用的框");
            self.calls.confirm.fetch_add(1, SeqCst);
            self.seen.lock().unwrap().push((
                t.tool_name.to_string(),
                t.strong,
                t.display.as_str().to_string(),
            ));
            let r = self.reply.clone();
            Box::pin(async move { r })
        }
    }

    /// 假审计里落下的一行。具名而不是四元组：`r.2` 与 `r.3` 读起来
    /// 分不清谁是 display 谁是 output，而这两者恰好都是脱敏断言要看的。
    struct Row {
        phase: RecordPhase,
        verdict: McpVerdict,
        display: String,
        output: Option<String>,
    }

    /// 审计假实现：录下每一行，可脚本化让第 N 次写入失败。
    #[derive(Default)]
    struct Audit {
        rows: Mutex<Vec<Row>>,
        /// 第几次调用开始失败（1 = 第一次就失败）。0 = 永不失败。
        fail_from: usize,
    }

    impl Audit {
        fn failing_at(n: usize) -> Self {
            Self {
                rows: Default::default(),
                fail_from: n,
            }
        }
        fn phases(&self) -> Vec<RecordPhase> {
            self.rows.lock().unwrap().iter().map(|r| r.phase).collect()
        }
        fn verdicts(&self) -> Vec<McpVerdict> {
            self.rows
                .lock()
                .unwrap()
                .iter()
                .map(|r| r.verdict)
                .collect()
        }
        fn displays(&self) -> Vec<String> {
            self.rows
                .lock()
                .unwrap()
                .iter()
                .map(|r| r.display.clone())
                .collect()
        }
        /// 落库的输出（含端口失败那一支的失败原因）。
        fn outputs(&self) -> Vec<String> {
            self.rows
                .lock()
                .unwrap()
                .iter()
                .filter_map(|r| r.output.clone())
                .collect()
        }
    }

    impl McpAuditPort for Audit {
        fn append(&self, rec: McpCallRecord) -> F<'_, PortResult<()>> {
            let mut rows = self.rows.lock().unwrap();
            let nth = rows.len() + 1;
            rows.push(Row {
                phase: rec.phase,
                verdict: rec.verdict,
                display: rec.display.as_str().to_string(),
                output: rec.output.as_ref().map(|o| o.as_str().to_string()),
            });
            let fail = self.fail_from != 0 && nth >= self.fail_from;
            drop(rows);
            Box::pin(async move {
                if fail {
                    Err("磁盘满".to_string())
                } else {
                    Ok(())
                }
            })
        }
    }

    /// 一整套假实现 + 装配。
    struct Rig {
        dir: Dir,
        term: Term,
        cmd: Cmd,
        sftp: Sftp,
        conf: Conf,
        audit: Audit,
        calls: std::sync::Arc<Calls>,
        ctx: CallContext,
        authz: Authorization,
        ledger: OpenLedger,
    }

    const SECRET: &str = "hunter2SuperSecretValue";

    impl Rig {
        fn new() -> Self {
            let calls: std::sync::Arc<Calls> = Default::default();
            let mut conf = Conf::approving();
            conf.calls = calls.clone();
            Self {
                dir: Dir {
                    calls: calls.clone(),
                    ..Default::default()
                },
                term: Term {
                    calls: calls.clone(),
                    text: "screen text".into(),
                },
                cmd: Cmd {
                    calls: calls.clone(),
                    obs: Observation {
                        stdout: "ok".into(),
                        ..Default::default()
                    },
                },
                sftp: Sftp {
                    calls: calls.clone(),
                },
                conf,
                audit: Audit::default(),
                calls,
                ctx: CallContext {
                    caller: "claude-desktop".into(),
                    mode: AiMode::WithConfirm,
                    known_secrets: vec![SECRET.to_string()],
                },
                authz: Authorization::new(crate::tool::MCP_MANIFEST.iter().map(|m| m.name)),
                ledger: OpenLedger::new(),
            }
        }

        async fn call(&mut self, name: &str, args: &str) -> CallOutput {
            let ports = Ports {
                sessions: &self.dir,
                terminal: &self.term,
                command: &self.cmd,
                sftp: &self.sftp,
                confirm: &self.conf,
                audit: &self.audit,
            };
            super::call(name, args, &self.ctx, &self.authz, &mut self.ledger, &ports).await
        }

        async fn run_cmd(&mut self, c: &str) -> CallOutput {
            let args = serde_json::json!({"session_id":"s1","command":c}).to_string();
            self.call("command.run", &args).await
        }
    }

    // ══ ① 顺序：解析 → 授权 → 会话 → 分类 → 裁决 ═══════════════════════

    /// A1 不存在的工具名 + 畸形参数 ⇒ 不许报 `BadArgs`。
    ///
    /// 报 BadArgs 等于告诉调用方「这个名字我认识、只是参数写错了」。
    #[tokio::test]
    async fn an_unknown_tool_name_never_reaches_argument_parsing() {
        let mut r = Rig::new();
        let out = r.call("vault.read", "{{{").await;
        assert_eq!(out.error_code(), Some("FS_POLICY_FORBIDDEN"));
        match &out {
            CallOutput::Err(e) => {
                assert!(!e.message.contains("参数"), "泄露了参数解析：{}", e.message);
                assert!(
                    !e.message.contains("vault"),
                    "回包里复述了名字：{}",
                    e.message
                );
            }
            _ => panic!("该失败"),
        }
        r.calls.only(&[]);
        assert!(r.audit.phases().is_empty(), "乱试不写操作审计");
    }

    /// A1b 授权过的工具，参数写错了**要**说清楚——否则调用方无从修。
    ///
    /// 这是 A1 的反向对照：只有 A1 的话，把所有错误都压成一句
    /// 「不可用」也能全绿，而那会让正常使用者完全无法排错。
    #[tokio::test]
    async fn an_authorized_tool_with_bad_arguments_gets_a_useful_complaint() {
        let mut r = Rig::new();
        let out = r.call("command.run", r#"{"session_id":"s1"}"#).await;
        assert_eq!(out.error_code(), Some("FS_POLICY_FORBIDDEN"));
        match &out {
            CallOutput::Err(e) => {
                assert!(e.message.contains("command.run"), "{}", e.message);
                assert!(e.message.contains("参数"), "{}", e.message);
            }
            _ => panic!("该失败"),
        }
        r.calls.only(&[]);
    }

    /// A2 工具存在但未授权 ⇒ FORBIDDEN，端口一次都没调，审计一行都没写。
    #[tokio::test]
    async fn an_unauthorized_tool_touches_neither_a_port_nor_the_audit_log() {
        let mut r = Rig::new();
        r.authz = Authorization::new(["sessions.list"]);
        let out = r.run_cmd("ls").await;
        assert_eq!(out.error_code(), Some("FS_POLICY_FORBIDDEN"));
        r.calls.only(&[]);
        assert!(r.audit.phases().is_empty());
        // 反向对照：授权之后同一次调用会真的动端口、也真的写审计。
        r.authz = Authorization::new(["command.run"]);
        let ok = r.run_cmd("ls").await;
        assert!(!ok.is_err(), "{ok:?}");
        r.calls.only(&[("run", 1)]);
        assert!(!r.audit.phases().is_empty());
    }

    /// A3 「未授权」与「不存在」**逐字**同一个回包——否则回包本身就是枚举器。
    ///
    /// 这条第一版只比了 code 与 tier，于是漏掉了真正的洞：两条路径的
    /// **文案**不同（「没有名为 X 的工具」vs「这个工具在当前配置下不可用」），
    /// 调用方逐个试名字就能问出本机支持哪些工具而只是没给它。比文案才抓得住。
    ///
    /// 也断言回包里**不出现被试的名字**：复述名字本身就是一种确认。
    #[tokio::test]
    async fn a_forbidden_tool_is_indistinguishable_from_a_nonexistent_one() {
        let mut r = Rig::new();
        r.authz = Authorization::new(["sessions.list"]);
        let args = r#"{"session_id":"s1","path":"/x"}"#;

        let existing_but_denied = r.call("sftp.write", args).await; // 清单里有
        let never_existed = r.call("sftp.delete", args).await; // 清单里没有
        let also_never = r.call("vault.read", args).await;

        let unwrap = |o: CallOutput| match o {
            CallOutput::Err(e) => e,
            _ => panic!("该失败"),
        };
        let a = unwrap(existing_but_denied);
        let b = unwrap(never_existed);
        let c = unwrap(also_never);

        assert_eq!(a.code, b.code);
        assert_eq!(a.tier, b.tier);
        assert_eq!(
            a.message, b.message,
            "文案不同就是一台枚举器：存在但未授权 vs 根本不存在"
        );
        assert_eq!(a.message, c.message);
        assert_eq!(a.confirmation_required, b.confirmation_required);

        for name in ["sftp.write", "sftp.delete", "vault", "delete"] {
            assert!(!a.message.contains(name), "回包复述了名字：{}", a.message);
            assert!(!b.message.contains(name), "回包复述了名字：{}", b.message);
        }

        // 反向对照：**授权了的**工具走的是完全不同的一条路（不再是 FORBIDDEN），
        // 否则把 `call` 写成「一律返回这句话」也能让上面全绿。
        r.authz = Authorization::new(["sftp.write"]);
        let allowed = r
            .call(
                "sftp.write",
                r#"{"session_id":"s1","path":"/tmp/a","content":"x"}"#,
            )
            .await;
        assert!(!allowed.is_err(), "{allowed:?}");
    }

    /// A4 会话不在 ⇒ 到不了分类与确认。
    ///
    /// 用一个「被调用就 panic」的确认端口来证明这一步没走到——断言
    /// 「返回值是 FORBIDDEN」是不够的，那个码在好几条路径上都会出现。
    #[tokio::test]
    async fn a_dead_session_stops_the_call_before_it_is_ever_classified() {
        let mut r = Rig::new();
        r.conf = Conf::forbidden();
        r.dir.alive_until = 0; // 第一次问就说不在
        let out = r.run_cmd("rm -rf /").await;
        assert_eq!(out.error_code(), Some("FS_POLICY_FORBIDDEN"));
        r.calls.only(&[]);
        assert_eq!(r.calls.confirm.load(SeqCst), 0);
        assert!(r.audit.phases().is_empty());
    }

    /// A5 授权在闸门之前：一个**未授权的危险工具**回的是 FORBIDDEN 而不是 DENIED。
    ///
    /// 反了的话，调用方会以为「换个档位就能过」，而实际上换档位也没用。
    #[tokio::test]
    async fn authorization_is_checked_before_the_gate_ever_sees_the_command() {
        let mut r = Rig::new();
        r.authz = Authorization::new(["sessions.list"]);
        r.ctx.mode = AiMode::WithConfirm;
        let out = r.run_cmd("rm -rf /").await;
        assert_eq!(
            out.error_code(),
            Some("FS_POLICY_FORBIDDEN"),
            "未授权要先于危险级判定"
        );
        // 反向对照：授权之后，同一条命令确实走到闸门并被判危（要强确认）。
        r.authz = Authorization::new(["command.run"]);
        let _ = r.run_cmd("rm -rf /").await;
        assert!(r.conf.last_strong(), "rm -rf / 该是强确认");
    }

    // ══ ② 闸门 ═══════════════════════════════════════════════════════

    /// B1 总开关关闭时，**连只读工具都拦**。
    #[tokio::test]
    async fn the_global_switch_stops_even_a_read_only_tool() {
        let mut r = Rig::new();
        r.ctx.mode = AiMode::Disabled;
        r.conf = Conf::forbidden();
        let out = r.call("sessions.list", "{}").await;
        assert_eq!(out.error_code(), Some("FS_POLICY_DENIED"));
        r.calls.only(&[]);
        assert_eq!(r.audit.phases(), vec![RecordPhase::Settled], "拒绝只写一行");
        assert_eq!(r.audit.verdicts(), vec![McpVerdict::Denied]);
        // 反向对照：换成 WithConfirm，同一次调用会真的执行。
        r.ctx.mode = AiMode::WithConfirm;
        let ok = r.call("sessions.list", "{}").await;
        assert!(!ok.is_err(), "{ok:?}");
        r.calls.only(&[("list", 1)]);
    }

    /// B2 只读档：只读命令放行，写操作被拒。
    #[tokio::test]
    async fn read_only_mode_lets_reads_through_and_denies_writes() {
        let mut r = Rig::new();
        r.ctx.mode = AiMode::ReadOnlyOnly;
        r.conf = Conf::forbidden();

        let ok = r.run_cmd("ls -la").await;
        assert!(!ok.is_err(), "只读命令该放行：{ok:?}");
        assert_eq!(r.calls.run.load(SeqCst), 1);

        let denied = r
            .call(
                "sftp.write",
                r#"{"session_id":"s1","path":"/tmp/a","content":"x"}"#,
            )
            .await;
        assert_eq!(denied.error_code(), Some("FS_POLICY_DENIED"));
        assert_eq!(r.calls.sftp_write.load(SeqCst), 0);
    }

    /// B3 自动放行**不弹框**。多余的确认在教用户闭眼点确认。
    #[tokio::test]
    async fn an_auto_run_never_bothers_the_user() {
        let mut r = Rig::new();
        r.conf = Conf::forbidden();
        let out = r.run_cmd("df -h").await;
        assert!(!out.is_err(), "{out:?}");
        assert_eq!(r.calls.confirm.load(SeqCst), 0);
        assert_eq!(
            r.audit.verdicts(),
            vec![McpVerdict::AutoRun, McpVerdict::AutoRun]
        );
    }

    /// B4 危险命令走**强**确认。
    #[tokio::test]
    async fn a_dangerous_command_asks_for_the_deliberate_kind_of_confirmation() {
        let mut r = Rig::new();
        let _ = r.run_cmd("rm -rf /").await;
        assert_eq!(r.calls.confirm.load(SeqCst), 1);
        assert!(r.conf.last_strong());
        // 反向对照：普通写操作只要单次确认。
        let mut r2 = Rig::new();
        let _ = r2
            .call(
                "sftp.write",
                r#"{"session_id":"s1","path":"/tmp/a","content":"x"}"#,
            )
            .await;
        assert!(!r2.conf.last_strong(), "普通写不该是强确认");
    }

    // ══ ③ 确认四出口 ══════════════════════════════════════════════════

    /// C1 批准 ⇒ 执行发生，审计**两行**且 Intent 在前。
    ///
    /// 两行的顺序是这条的重点：Settled 在 Intent 之前意味着「先记结果再记
    /// 打算」，那就等于没有两阶段。
    #[tokio::test]
    async fn an_approval_executes_and_leaves_an_intent_row_before_the_settled_one() {
        let mut r = Rig::new();
        let out = r.run_cmd("rm -rf /tmp/x").await;
        assert!(!out.is_err(), "{out:?}");
        assert_eq!(r.calls.run.load(SeqCst), 1);
        assert_eq!(
            r.audit.phases(),
            vec![RecordPhase::Intent, RecordPhase::Settled]
        );
        assert_eq!(
            r.audit.verdicts(),
            vec![McpVerdict::Approved, McpVerdict::Approved]
        );
    }

    /// C2 拒绝 ⇒ **不执行**，审计只有一行 Settled。
    #[tokio::test]
    async fn a_refusal_executes_nothing_and_leaves_exactly_one_row() {
        let mut r = Rig::new();
        r.conf = Conf::with(Some(false), true);
        let out = r.run_cmd("rm -rf /").await;
        assert_eq!(out.error_code(), Some("FS_POLICY_DENIED"));
        assert_eq!(r.calls.executed(), 0, "拒绝了却动了端口");
        assert_eq!(r.audit.phases(), vec![RecordPhase::Settled]);
        assert_eq!(r.audit.verdicts(), vec![McpVerdict::Rejected]);
        match out {
            CallOutput::Err(e) => assert!(e.confirmation_required, "问过了，答案是否"),
            _ => unreachable!(),
        }
    }

    /// C3 超时 ⇒ TIMEOUT，未执行。
    #[tokio::test]
    async fn a_timeout_executes_nothing_and_is_recorded_as_a_timeout() {
        let mut r = Rig::new();
        r.conf = Conf::with(None, true);
        let out = r.run_cmd("rm -rf /").await;
        assert_eq!(out.error_code(), Some("FS_POLICY_TIMEOUT"));
        assert_eq!(r.calls.executed(), 0);
        assert_eq!(r.audit.verdicts(), vec![McpVerdict::TimedOut]);
    }

    /// C4 上下文消亡 ⇒ **ABORTED，不是 TIMEOUT**（规格明令）。
    ///
    /// 两者都源自「没等到回答」，差别只在会话还在不在——正因为它们如此
    /// 相邻，才最容易被合并成一个码。
    #[tokio::test]
    async fn a_dead_context_is_aborted_and_never_reported_as_a_timeout() {
        let mut r = Rig::new();
        r.conf = Conf::with(None, false);
        let out = r.run_cmd("rm -rf /").await;
        assert_eq!(out.error_code(), Some("FS_POLICY_ABORTED"));
        assert_ne!(out.error_code(), Some("FS_POLICY_TIMEOUT"));
        assert_eq!(r.calls.executed(), 0);
        assert_eq!(r.audit.verdicts(), vec![McpVerdict::Abandoned]);
    }

    /// C5 用户点了同意，但**收敛那一刻**会话已经没了 ⇒ ABORTED，未执行。
    ///
    /// 这条钉的是「不复用旧读数」：生产代码在第③步问过一次 is_alive，
    /// 确认返回后必须**再问一次**。复用旧读数的话，这里会一路执行下去——
    /// 往一条不存在的连接、或者更糟，往一条重连后指向别处的连接上发命令。
    ///
    /// 假实现让 is_alive 第一次答「在」、之后答「不在」。
    #[tokio::test]
    async fn an_approval_is_void_if_the_session_died_while_the_user_was_thinking() {
        let mut r = Rig::new();
        r.dir.alive_until = 1; // 第 0 次问在，第 1 次起不在
        r.conf = Conf::with(Some(true), true); // 确认端口自己说上下文还在
        let out = r.run_cmd("rm -rf /").await;
        assert_eq!(
            out.error_code(),
            Some("FS_POLICY_ABORTED"),
            "确认返回后必须重新问一次会话在不在"
        );
        assert_eq!(r.calls.executed(), 0);
        assert!(
            r.calls.alive_asked.load(SeqCst) >= 2,
            "只问了 {} 次——说明复用了旧读数",
            r.calls.alive_asked.load(SeqCst)
        );
        // 反向对照：会话一直在的话，同样的批准会执行。
        let mut ok = Rig::new();
        let o = ok.run_cmd("rm -rf /").await;
        assert!(!o.is_err(), "{o:?}");
        assert_eq!(ok.calls.run.load(SeqCst), 1);
    }

    // ══ ④ 审计 ═══════════════════════════════════════════════════════

    /// D1 Intent 写不进去 ⇒ **不执行**。
    ///
    /// 这是两阶段审计的全部意义：让「动作发生了却没有任何记录」在时序上
    /// 不可能。
    #[tokio::test]
    async fn a_failed_intent_row_stops_the_call_before_anything_happens() {
        let mut r = Rig::new();
        r.audit = Audit::failing_at(1);
        let out = r.run_cmd("rm -rf /tmp/x").await;
        assert!(out.is_err());
        assert_eq!(r.calls.executed(), 0, "审计写不进去却执行了");
        match out {
            CallOutput::Err(e) => assert!(e.message.contains("未执行"), "{}", e.message),
            _ => unreachable!(),
        }
        // 反向对照：审计正常时同一次调用会执行。
        let mut ok = Rig::new();
        let o = ok.run_cmd("rm -rf /tmp/x").await;
        assert!(!o.is_err(), "{o:?}");
    }

    /// D2 Settled 写不进去 ⇒ 动作**已经发生了**，如实说。
    ///
    /// 这是两阶段没能消掉的残余。回包必须承认动作发生了——报一个笼统的
    /// 失败会让调用方以为什么都没做，然后重试一次。
    #[tokio::test]
    async fn a_failed_settled_row_admits_that_the_action_already_happened() {
        let mut r = Rig::new();
        r.audit = Audit::failing_at(2);
        let out = r.run_cmd("rm -rf /tmp/x").await;
        assert!(out.is_err());
        assert_eq!(r.calls.run.load(SeqCst), 1, "这一步动作确实发生了");
        match out {
            CallOutput::Err(e) => {
                assert!(e.message.contains("已执行"), "{}", e.message);
                assert!(!e.message.contains("未执行"), "{}", e.message);
            }
            _ => unreachable!(),
        }
        // 链上留下一条有 Intent 没有 Settled 的可见缺口。
        assert_eq!(r.audit.phases()[0], RecordPhase::Intent);
    }

    /// D3 审计行里的 display **每一行**都脱过敏。
    #[tokio::test]
    async fn every_audit_row_carries_a_redacted_display() {
        let mut r = Rig::new();
        let out = r.run_cmd(&format!("mysql -p{SECRET} -e drop")).await;
        assert!(!out.is_err(), "{out:?}");
        let d = r.audit.displays();
        assert_eq!(d.len(), 2, "该有两行");
        for line in &d {
            assert!(!line.contains(SECRET), "审计泄漏了机密：{line}");
            assert!(line.contains("redacted"), "没看到打码痕迹：{line}");
        }
    }

    /// D4 六种结局各自落到对的 verdict 上。
    ///
    /// 做成一张表逐个跑：连错一格会让审计里出现一条读起来完全正常、
    /// 意思却相反的行。
    #[tokio::test]
    async fn each_outcome_lands_on_its_own_verdict() {
        // (布置, 期望的最后一行 verdict)
        /// 一行布置：名字、怎么摆弄 Rig、期望落在哪个 verdict 上。
        struct Case(&'static str, Box<dyn Fn(&mut Rig)>, McpVerdict);
        let cases: Vec<Case> = vec![
            Case("自动放行", Box::new(|_: &mut Rig| {}), McpVerdict::AutoRun),
            Case(
                "批准",
                Box::new(|r: &mut Rig| r.conf = Conf::with(Some(true), true)),
                McpVerdict::Approved,
            ),
            Case(
                "拒绝",
                Box::new(|r: &mut Rig| r.conf = Conf::with(Some(false), true)),
                McpVerdict::Rejected,
            ),
            Case(
                "超时",
                Box::new(|r: &mut Rig| r.conf = Conf::with(None, true)),
                McpVerdict::TimedOut,
            ),
            Case(
                "消亡",
                Box::new(|r: &mut Rig| r.conf = Conf::with(None, false)),
                McpVerdict::Abandoned,
            ),
            Case(
                "闸门拒",
                Box::new(|r: &mut Rig| r.ctx.mode = AiMode::Disabled),
                McpVerdict::Denied,
            ),
        ];
        for Case(name, setup, want) in &cases {
            let mut r = Rig::new();
            setup(&mut r);
            // 「自动放行」用只读命令，其余用危险命令走确认。
            let cmd = if *want == McpVerdict::AutoRun {
                "ls"
            } else {
                "rm -rf /"
            };
            let _ = r.run_cmd(cmd).await;
            let v = r.audit.verdicts();
            assert!(!v.is_empty(), "{name}：一行审计都没写");
            assert_eq!(v.last(), Some(want), "{name} 落错了档位");
        }
        assert_eq!(cases.len(), 6, "做空防护");
        // 六个 verdict 互不相同，否则这张表证明不了任何事。
        let uniq: std::collections::BTreeSet<&str> =
            cases.iter().map(|Case(_, _, v)| v.as_str()).collect();
        assert_eq!(uniq.len(), 6);
    }

    // ══ ⑤ 路由 ═══════════════════════════════════════════════════════

    /// E1 九个工具各自转调**各自的**端口方法。
    ///
    /// 防的是「全都走了 command.run」——M3 批次③ 真出过这个 bug，四个特化
    /// 端口方法一次都没被调过，而所有「结果对不对」的断言照样绿。
    #[tokio::test]
    async fn every_tool_reaches_its_own_port_method_and_no_other() {
        let cases: Vec<(&str, String, &str)> = vec![
            ("sessions.list", "{}".into(), "list"),
            (
                "terminal.read",
                serde_json::json!({"session_id":"s1","source":"screen"}).to_string(),
                "read_text",
            ),
            (
                "terminal.send",
                serde_json::json!({"session_id":"s1","payload":"ls
"})
                .to_string(),
                "send",
            ),
            (
                "terminal.send_raw",
                serde_json::json!({"session_id":"s1","payload":"x"}).to_string(),
                "send",
            ),
            (
                "command.run",
                serde_json::json!({"session_id":"s1","command":"ls"}).to_string(),
                "run",
            ),
            (
                "sftp.list",
                serde_json::json!({"session_id":"s1","path":"/"}).to_string(),
                "sftp_list",
            ),
            (
                "sftp.read",
                serde_json::json!({"session_id":"s1","path":"/f"}).to_string(),
                "sftp_read",
            ),
            (
                "sftp.write",
                serde_json::json!({"session_id":"s1","path":"/tmp/f","content":"c"}).to_string(),
                "sftp_write",
            ),
        ];
        for (name, args, port) in &cases {
            let mut r = Rig::new();
            let out = r.call(name, args).await;
            assert!(!out.is_err(), "{name}: {out:?}");
            // 恰好这一个端口被调了一次，其余全是 0。
            r.calls.only(&[(port, 1)]);
        }
        // sessions.open 单列：它要先打勾。
        let mut r = Rig::new();
        r.dir.allowed.lock().unwrap().insert("p1".into(), true);
        let out = r.call("sessions.open", r#"{"profile_id":"p1"}"#).await;
        assert!(!out.is_err(), "{out:?}");
        r.calls.only(&[("open", 1)]);

        assert_eq!(
            cases.len() + 1,
            crate::tool::MCP_MANIFEST.len(),
            "九个工具一个不少"
        );
    }

    // ══ ⑥ 出站脱敏 ════════════════════════════════════════════════════

    /// F1/F2 出站文本一律脱敏——命令输出与终端文本都是。
    #[tokio::test]
    async fn secrets_never_leave_through_either_the_command_or_the_terminal() {
        let mut r = Rig::new();
        r.cmd.obs = Observation {
            stdout: format!("db_password={SECRET}"),
            stderr: format!("also {SECRET}"),
            ..Default::default()
        };
        r.term.text = format!("$ echo {SECRET}");

        let a = r.run_cmd("ls").await;
        match &a {
            CallOutput::Ok { text, structured } => {
                assert!(!text.contains(SECRET), "命令输出泄漏了：{text}");
                assert!(text.contains("redacted"));
                // **结构化那一路同样不许漏**：command.run 的 structured 是裸
                // Observation（stdout/stderr 原文）。它若带着机密进结构化输出，
                // 就是绕过唯一脱敏点的旁路。逐字段断言。
                let s = structured.as_ref().expect("command.run 该带结构化输出");
                let dump = s.to_string();
                assert!(!dump.contains(SECRET), "结构化输出泄漏了：{dump}");
                assert!(s["stdout"].as_str().is_some_and(|t| t.contains("redacted")));
                assert!(s["stderr"].as_str().is_some_and(|t| t.contains("redacted")));
            }
            _ => panic!("该成功：{a:?}"),
        }

        let b = r
            .call("terminal.read", r#"{"session_id":"s1","source":"all"}"#)
            .await;
        match &b {
            CallOutput::Ok { text, .. } => {
                assert!(!text.contains(SECRET), "终端文本泄漏了：{text}");
            }
            _ => panic!("该成功：{b:?}"),
        }
    }

    // ══ ⑦ sessions.open 的两道门 ══════════════════════════════════════

    /// G1 没打勾 ⇒ FORBIDDEN，且**没弹确认框**。
    #[tokio::test]
    async fn an_unflagged_profile_is_refused_without_ever_asking_the_user() {
        let mut r = Rig::new();
        r.conf = Conf::forbidden();
        r.dir.allowed.lock().unwrap().insert("p1".into(), false);
        let out = r.call("sessions.open", r#"{"profile_id":"p1"}"#).await;
        assert_eq!(out.error_code(), Some("FS_POLICY_FORBIDDEN"));
        assert_eq!(r.calls.open.load(SeqCst), 0);
        assert_eq!(r.calls.confirm.load(SeqCst), 0);
    }

    /// G2/G3 首次提级到强确认；打开成功后第二次回落到单次确认。
    #[tokio::test]
    async fn the_first_time_a_client_opens_a_profile_it_takes_a_deliberate_confirmation() {
        let mut r = Rig::new();
        r.dir.allowed.lock().unwrap().insert("p1".into(), true);

        let a = r.call("sessions.open", r#"{"profile_id":"p1"}"#).await;
        assert!(!a.is_err(), "{a:?}");
        assert!(r.conf.last_strong(), "首次该是强确认");

        let b = r.call("sessions.open", r#"{"profile_id":"p1"}"#).await;
        assert!(!b.is_err(), "{b:?}");
        assert!(!r.conf.last_strong(), "见过之后回落到单次确认");

        // 换个 profile：又是首次。
        r.dir.allowed.lock().unwrap().insert("p2".into(), true);
        let c = r.call("sessions.open", r#"{"profile_id":"p2"}"#).await;
        assert!(!c.is_err(), "{c:?}");
        assert!(r.conf.last_strong(), "没见过的 profile 该重新提级");
    }

    /// G4 打开**失败**时不记台账——下一次仍然算首次。
    ///
    /// 在「用户批准」那一刻记台账的话，一次因为网络失败而没开成的尝试
    /// 也会算数，下次就不问了。
    #[tokio::test]
    async fn a_failed_open_does_not_count_as_having_seen_this_pair() {
        let mut r = Rig::new();
        r.dir.allowed.lock().unwrap().insert("p1".into(), true);
        *r.dir.open_result.lock().unwrap() = Err("连不上".into());

        let a = r.call("sessions.open", r#"{"profile_id":"p1"}"#).await;
        assert!(r.conf.last_strong(), "首次该是强确认");
        // 端口失败走的是文本回包（不是策略拒绝），但里面要说清失败了。
        match &a {
            CallOutput::Ok { text, .. } => assert!(text.contains("失败"), "{text}"),
            CallOutput::Err(e) => panic!("端口失败不该报成策略拒绝：{e:?}"),
        }
        assert!(r.ledger.is_empty(), "没开成却记了台账");

        *r.dir.open_result.lock().unwrap() = Ok("s2".into());
        let b = r.call("sessions.open", r#"{"profile_id":"p1"}"#).await;
        assert!(!b.is_err(), "{b:?}");
        assert!(r.conf.last_strong(), "上次没开成，这次仍然算首次");
        assert_eq!(r.ledger.len(), 1, "这次开成了，该记上");
    }

    /// D5 审计行的 **output** 也脱过敏——成功与端口失败两条路都要。
    ///
    /// 这条是变异验证逼出来的：删掉一处脱敏时九条测试全绿，因为当时
    /// **没有任何一条测试看过 output**。而端口失败那一支的原文（路径、命令、
    /// 有时还有远端回吐的凭据）当时根本没经过脱敏就进了审计行。
    #[tokio::test]
    async fn the_audit_output_is_redacted_on_both_the_success_and_the_failure_path() {
        // 成功路径
        let mut r = Rig::new();
        r.cmd.obs = Observation {
            stdout: format!("db_password={SECRET}"),
            ..Default::default()
        };
        let a = r.run_cmd("ls").await;
        assert!(!a.is_err(), "{a:?}");
        let outs = r.audit.outputs();
        assert_eq!(outs.len(), 1, "只有 Settled 那一行带 output");
        assert!(!outs[0].contains(SECRET), "审计 output 泄漏了：{}", outs[0]);
        assert!(outs[0].contains("redacted"), "没看到打码痕迹：{}", outs[0]);

        // 端口失败路径：失败原因里带机密
        let mut r2 = Rig::new();
        r2.dir.allowed.lock().unwrap().insert("p1".into(), true);
        *r2.dir.open_result.lock().unwrap() = Err(format!("认证失败（用了 {SECRET}）"));
        let b = r2.call("sessions.open", r#"{"profile_id":"p1"}"#).await;
        let outs2 = r2.audit.outputs();
        assert_eq!(outs2.len(), 1);
        assert!(
            !outs2[0].contains(SECRET),
            "端口失败的原文没脱敏就进了审计：{}",
            outs2[0]
        );
        // 回包里也不许有。
        match &b {
            CallOutput::Ok { text, .. } => assert!(!text.contains(SECRET), "{text}"),
            CallOutput::Err(e) => assert!(!e.message.contains(SECRET), "{}", e.message),
        }
    }

    /// D6 回包与审计行用的是**同一份**脱敏结果。
    ///
    /// 两处各脱一次的话，总有一天只改了其中一处；而先被忘掉的永远是审计
    /// 那一处（没人盯着它看）。断言两者逐字相同，就让「只改一处」这件事
    /// 当场可见。
    #[tokio::test]
    async fn the_reply_and_the_audit_row_carry_the_very_same_text() {
        let mut r = Rig::new();
        r.cmd.obs = Observation {
            stdout: format!("token={SECRET} rest"),
            ..Default::default()
        };
        let out = r.run_cmd("ls").await;
        match out {
            CallOutput::Ok { text, .. } => {
                assert_eq!(r.audit.outputs(), vec![text], "回包与审计行的文本必须同源");
            }
            _ => panic!("该成功"),
        }
    }
}
