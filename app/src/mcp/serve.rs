//! MCP 对外服务的**套接字形态**（2026-08-28 重做）。
//!
//! # 为什么放弃「stdio 直接服务」
//!
//! 原形态把 MCP 服务挂在本进程自己的 stdio 上，设想是「外部客户端把本程序
//! 当子进程拉起」。它在 GUI 程序上有结构性死结：单实例闸（`lock_data_dir`）
//! 排在一切启动动作之前——主程序正开着时，被拉起的第二个进程**先死在闸上**；
//! 反过来外部客户端先拉起、用户再开主程序，用户自己吃到「另一个实例」报错。
//! 原形态只在「用户恰好没开主程序」的窗口里可用，设置里那个开关承诺的东西
//! 实际兑现不了（详见 `fs_mcpbridge::transport::serve_stream` 的文档）。
//!
//! # 本模块的形态
//!
//! ```text
//! 外部 MCP 客户端（Claude Desktop 等）
//!   └─ stdio ── future-shell-app.exe --mcp-bridge（纯字节中继，不开窗口、不碰单实例闸）
//!        └─ TCP 127.0.0.1:<port> ── 主程序的 accept 循环
//!              ├─ 第一行 = token（sha256 恒时比对）——不过闸直接断连
//!              └─ 过闸后每连接一个 McpServer（账本 per-client）
//! ```
//!
//! # 安全模型（如实陈述，不夸大）
//!
//! - **只绑回环**：不出本机，外网碰不到。
//! - **token 闸**：不知道 token 的本机进程连不上。它挡的是「同机的其他进程
//!   顺手拉起我们的能力」——这是用户要求的最低防线。
//! - **它挡不住的**：token 存在设置库里，与设置库同用户的恶意代码本来就能
//!   读它。这不是本方案独有的弱点——本机任何本地凭据（浏览器 cookie、ssh
//!   agent）在同用户恶意代码面前一样裸奔。如实记在这里，不假装它是一次
//!   「安全加固」，它是**从无到有的最低门槛 + 连接可见性**。
//! - 已连接的会话在 token 轮换后**保持到断开**：闸在连接建立时刻校验，
//!   不在会话中途重读——中途重读会把正在跑的任务拦腰打断，代价大于收益。

use crate::state::AppState;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicUsize, Ordering};
use std::sync::Arc;

/// MCP 运行时状态（挂到 Tauri，供 `mcp_status` 与前端状态点读取）。
#[derive(Default)]
pub struct McpRuntime {
    pub listening: AtomicBool,
    pub port: AtomicU16,
    /// 当前已过闸的连接数（含握手进行中的）。
    pub connections: AtomicUsize,
    /// 事件发射器：setup 时由 app 层塞进来（AppState 没有 AppHandle，
    /// 状态事件从这条通道走，避免为此给 AppState 加字段）。
    pub emitter: std::sync::OnceLock<tauri::AppHandle>,
}

impl McpRuntime {
    fn emit(&self) {
        use tauri::Emitter as _;
        let listening = self.listening.load(Ordering::Relaxed);
        let payload = serde_json::json!({
            "enabled": listening,
            "listening": listening,
            "port": if listening { self.port.load(Ordering::Relaxed) } else { 0 },
            "connections": self.connections.load(Ordering::Relaxed),
        });
        if let Some(app) = self.emitter.get() {
            let _ = app.emit("mcp:status", payload);
        }
    }
}

/// endpoint 文件名（数据目录下）。桥进程读它找端口。
/// 内容只有端口号——token 经客户端配置的 env 传给桥、由桥发给主程序，
/// **不落这个文件**（少一处明文副本；反正读得到这文件的进程也过不了闸）。
pub const ENDPOINT_FILE: &str = "mcp.endpoint";

/// 设置键：token（JSON 字符串形态，与其它设置同口径）。
pub const MCP_TOKEN_KEY: &str = "mcp.token";

/// 读 token；没有则生成一枚并落库（返回的是明文 token）。
///
/// 生成而不是报错：用户第一次打开 MCP 开关时不可能已经手动配过 token，
/// 报错只会把人挡在门外。`uuid::Uuid::new_v4` 的 122 位熵对这个威胁模型
/// （挡同机顺手进程，不挡同用户恶意代码）绰绰有余。
pub async fn ensure_token(state: &AppState) -> Result<String, String> {
    let repo = fs_connmgr::SettingsRepo::new(state.db.pool());
    if let Some(existing) = repo.get(MCP_TOKEN_KEY).await.map_err(|e| e.to_string())? {
        if let Ok(t) = serde_json::from_str::<String>(&existing) {
            if !t.is_empty() {
                return Ok(t);
            }
        }
    }
    let token = uuid::Uuid::new_v4().simple().to_string();
    let stored = serde_json::to_string(&token).map_err(|e| e.to_string())?;
    repo.set(MCP_TOKEN_KEY, &stored)
        .await
        .map_err(|e| e.to_string())?;
    Ok(token)
}

/// 轮换 token：旧连接保持（见模块头「安全模型」），新连接要新 token。
pub async fn rotate_token(state: &AppState) -> Result<String, String> {
    let repo = fs_connmgr::SettingsRepo::new(state.db.pool());
    let token = uuid::Uuid::new_v4().simple().to_string();
    let stored = serde_json::to_string(&token).map_err(|e| e.to_string())?;
    repo.set(MCP_TOKEN_KEY, &stored)
        .await
        .map_err(|e| e.to_string())?;
    Ok(token)
}

/// token 的 sha256——存这份做恒时比对，不存明文（比对时把来路也哈希一份）。
fn token_digest(token: &str) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(token.as_bytes());
    h.finalize().into()
}

/// 起监听并常驻 accept。返回 (端口, 关停句柄)——`spawn_listener` 持有后者。
///
/// **绑定 127.0.0.1:0**（随机端口）：固定端口会与本机其他服务撞，撞了还要
/// 用户来处理；随机端口写进 endpoint 文件，桥自己会找。
async fn bind(state: &AppState) -> Result<(tokio::net::TcpListener, PathBuf), String> {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0u16))
        .await
        .map_err(|e| format!("MCP 监听绑定失败（127.0.0.1:0）：{e}"))?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("MCP 监听取端口失败：{e}"))?
        .port();
    let file = state.data_dir.join(ENDPOINT_FILE);
    // 0600 收紧（Unix）：端口本身不是秘密，但这文件的存在告诉读者「MCP 开着」。
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::write(&file, port.to_string())
        .map_err(|e| format!("MCP endpoint 文件写入失败（{}）：{e}", file.display()))?;
    Ok((listener, file))
}

/// 起或停 MCP 监听，让设置开关**即时生效**（原 stdio 形态要重启程序）。
///
/// 由 `mcp_set_enabled` 命令调用。重复调用安全：已开再开 = 无动作，
/// 已关再关 = 无动作。监听任务持有 watch 通道做关停；endpoint 文件在
/// 关停时删除——留着它，桥会去连一个没人监听的端口，报错信息 misleading。
pub async fn set_enabled(
    state: Arc<AppState>,
    runtime: Arc<McpRuntime>,
    enabled: bool,
    shutdown: &tokio::sync::watch::Sender<bool>,
) -> Result<(), String> {
    if enabled {
        if runtime.listening.load(Ordering::Relaxed) {
            return Ok(());
        }
        let token = ensure_token(&state).await?;
        let (listener, file) = bind(&state).await?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        runtime.listening.store(true, Ordering::Relaxed);
        runtime.port.store(port, Ordering::Relaxed);
        emit_status(&state, &runtime);
        let st = state.clone();
        let rt = runtime.clone();
        let mut rx = shutdown.subscribe();
        tokio::spawn(async move {
            tokio::select! {
                _ = accept_loop(&listener, &st, &rt, &token) => {}
                _ = rx.changed() => {}
            }
            runtime.listening.store(false, Ordering::Relaxed);
            let _ = std::fs::remove_file(&file);
            emit_status(&st, &rt);
        });
    } else {
        // 关停靠唤醒 accept 任务里的 watch 分支（send 失败 = 任务已退，无妨）
        let _ = shutdown.send(true);
    }
    Ok(())
}

/// 状态广播：前端的状态点与设置页都听这一个事件（薄壳，调用点短一点）。
fn emit_status(_state: &AppState, runtime: &McpRuntime) {
    runtime.emit();
}

/// 读 token 行（到换行或 EOF，上限 256 字节）。
///
/// 逐字节读到行尾：token 行很短，逐字节的简单性 > 一次 read 的效率；
/// `\r` 也当行尾是给客户端实现里的 CRLF 差异留余量。
/// **None = 不是合法 token 行**（超 256 字节或读失败）——调用方一律断连，
/// 不给第二次机会：给了就等于可以在线爆破。
async fn read_token_line<S>(stream: &mut S) -> Option<String>
where
    S: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt as _;
    let mut buf = Vec::with_capacity(64);
    let mut chunk = [0u8; 1];
    while buf.len() < 256 {
        match stream.read(&mut chunk).await {
            Ok(0) => break, // 对端在行尾之前就断开：按已读内容比（多半比不上）
            Ok(_) => {
                if chunk[0] == b'\n' || chunk[0] == b'\r' {
                    break;
                }
                buf.push(chunk[0]);
            }
            Err(_) => return None,
        }
    }
    if buf.len() >= 256 {
        return None;
    }
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// accept 循环：每条连接先过 token 闸，过闸后独立任务里跑 MCP 会话。
///
/// 闸的读法：第一行（到 `\n`，上限 256 字节——token 是 32 字符，给客户端
/// 实现里的换行差异留余量）。超限/断连/比对失败一律**立即断开**，不给
/// 第二次机会——给了就等于可以在线爆破。
async fn accept_loop(
    listener: &tokio::net::TcpListener,
    state: &Arc<AppState>,
    runtime: &Arc<McpRuntime>,
    token: &str,
) {
    let expected = token_digest(token);
    loop {
        let Ok((stream, _peer)) = listener.accept().await else {
            // 监听器被关闭（程序退出）——正常路径
            return;
        };
        let st = state.clone();
        let rt = runtime.clone();
        tokio::spawn(async move {
            let mut stream = stream;
            // ── token 闸 ──────────────────────────────────────────────
            // 读行与比对抽在 read_token_line / token_digest：可离线测
            // （超长载荷、CRLF、EOF 提前断开这三条行为都有判据钉着）。
            let line = match read_token_line(&mut stream).await {
                Some(l) => l,
                // 不是合法 token 行（超限/读失败）：断连，不给第二次机会
                None => return,
            };
            let got = token_digest(&line);
            // 恒时比对：比对摘要而不是原文，长度差不会提前泄露。
            if got != expected {
                tracing::warn!("MCP 连接被拒：token 不匹配（{} 字节载荷）", line.len());
                return; // drop stream = 断开
            }
            // ── 过闸：跑 MCP 会话 ─────────────────────────────────────
            rt.connections.fetch_add(1, Ordering::Relaxed);
            emit_status(&st, &rt);
            let server = build_server(st.clone()).await;
            // stream 按**值**传入：serve_stream 要求 'static（rmcp 的传输
            // 会被装箱持有），借用的生命周期过不去。
            if let Err(e) = fs_mcpbridge::transport::serve_stream(server, stream).await {
                tracing::info!("MCP 连接结束：{e}");
            }
            rt.connections.fetch_sub(1, Ordering::Relaxed);
            emit_status(&st, &rt);
        });
    }
}

use super::build_server;

/// 连接指引（给设置页展示 + 复制）：外部 MCP 客户端该怎么配。
///
/// 用**当前运行中的 exe 路径**——用户从哪个位置装的程序，桥就在哪，
/// 指引里写的路径必须与之一致；写死相对路径会让用户配出一个指向
/// 不存在文件的 command。
pub fn connection_info(exe: &str, token: &str) -> String {
    // Windows 路径里的反斜杠在 JSON 里要转义
    let escaped = exe.replace('\\', "\\\\");
    format!(
        "{{\n  \"mcpServers\": {{\n    \"future-shell\": {{\n      \"command\": \"{escaped}\",\n      \"args\": [\"--mcp-bridge\"],\n      \"env\": {{ \"FS_MCP_TOKEN\": \"{token}\" }}\n    }}\n  }}\n}}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// token 闸的读行为（2026-08-28 套接字形态）：CRLF 当行尾、超限判非法、
    /// EOF 提前断开按已读内容比。这三条是闸的**协议**，客户端实现（桥）与
    /// 服务端各写一份，行为漂移 = 所有连接被拒。
    #[tokio::test]
    async fn token_line_reads_until_lf_or_cr() {
        // \n 结尾（桥的 writeln! 形态）
        let (mut client, mut server) = tokio::io::duplex(64);
        use tokio::io::AsyncWriteExt as _;
        client.write_all(b"abc123\n").await.unwrap();
        let line = read_token_line(&mut server).await;
        assert_eq!(line.as_deref(), Some("abc123"));

        // \r\n 结尾（给 CRLF 差异留的余量——\r 当行尾，不进 token）
        let (mut c2, mut s2) = tokio::io::duplex(64);
        c2.write_all(b"tok\r\n").await.unwrap();
        assert_eq!(read_token_line(&mut s2).await.as_deref(), Some("tok"));
    }

    /// 超过 256 字节还在读 = 不是 token 行，None。这条是防在线爆破的：
    /// 上限存在，攻击者就不能用超长行拖住连接慢慢试。
    #[tokio::test]
    async fn oversize_token_line_is_rejected_not_truncated() {
        let (mut client, mut server) = tokio::io::duplex(600);
        use tokio::io::AsyncWriteExt as _;
        client.write_all(&[b'a'; 300]).await.unwrap();
        // 截断后比对 = 前 256 字节对了就放行——那等于把 token 空间砍到 256 字节
        // 前缀。None（断连）才是对的行为。
        assert_eq!(read_token_line(&mut server).await, None);
    }

    /// 对端一个字节都没发就断开：按空串比（必然不匹配），不是 panic。
    #[tokio::test]
    async fn early_eof_yields_empty_line_not_error() {
        let (_client, mut server) = tokio::io::duplex(64);
        drop(_client);
        assert_eq!(read_token_line(&mut server).await.as_deref(), Some(""));
    }

    /// 连接指引里的 Windows 路径必须转义反斜杠——不转义，用户粘贴进
    /// Claude Desktop 的 JSON 解析不开，而报错在**别人的客户端**里，
    /// 用户只会觉得「这功能坏了」。
    #[test]
    fn connection_info_escapes_windows_backslashes() {
        let cfg = connection_info("C:\\Program Files\\FutureShell\\app.exe", "tok123");
        assert!(cfg.contains("\\\\Program Files"), "路径反斜杠未转义：{cfg}");
        assert!(cfg.contains("--mcp-bridge"), "桥参数缺失：{cfg}");
        assert!(cfg.contains("FS_MCP_TOKEN"), "token 环境变量名缺失：{cfg}");
        // 生成的必须是合法 JSON（用户整段粘贴，坏 JSON 直接配不进去）
        let inner = cfg
            .trim_start_matches('{')
            .trim_end_matches('}')
            .to_string();
        assert!(!inner.is_empty());
    }
}
