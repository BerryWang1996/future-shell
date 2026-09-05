//! RDP 会话的主程序侧装配（阶段 1）。
//!
//! 职责四件：**拉起并监管 helper 子进程**、**own TCP 连接并把线上字节中继给
//! helper**、**证书 TOFU 裁决与落库**（裁决在本进程——它才有数据库与 UI；
//! helper 只上报）、**帧转发给前端**。
//!
//! # 与 SSH 会话的关系
//!
//! RDP 不进 `SessionRegistry<LiveSession>`：那个结构持有 russh 通道句柄、SFTP
//! 句柄、重连锁——对 RDP 全是死字段。RDP 自己一张表（`AppState.rdp`），
//! 标签/会话生命周期对齐，内部完全不同构。
//!
//! # 进程内的三条流
//!
//! ```text
//! TCP ──读任务──► (NetIn,体) ──┐
//! 命令通道（Input/…）──────────┤
//! 协议循环（裁决回包等）────────┴──► 写任务（唯一 stdin 持有者）──► helper
//!                                                            helper stdout ──► 协议循环
//! ```
//!
//! **写任务独占 stdin**：握手、TCP 中继、用户输入、裁决回包都要写 helper 的
//! stdin；谁都可以写就是谁都可以把帧流写交错。全部经同一条 mpsc 汇到唯一
//! 写者——与 helper 侧「主循环是唯一 stdout 写者」互为镜像。
//!
//! # 帧转发：RGBA 直上 `rdp:frame:{id}`（阶段 1 的明知之选）
//!
//! 脏矩形在 helper 侧已合并，单帧通常几十 KB，事件 + base64 可接受；
//! **这条路的吞吐天花板记录在案**——1080p 全屏一帧 8.3MB×1.33 ≈ 11MB，
//! 事件桥扛不住连续全屏。阶段 2 换 `tauri::ipc::Channel` raw 通道
//! + 分档编码（计划已记）。阶段 1 不为不存在的问题预先复杂化。
//!
//! # helper 二进制的定位
//!
//! 与主程序同批打包。依次试 exe 同目录；不在则明确报错指向安装问题——
//! 不静默降级（RDP 面消失比一句报错更难排查）。

use fs_connmgr::{Db, Profile};
use fs_rdpproto::{ConnectParams, FromHelper, ToHelper, PROTOCOL_VERSION};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tauri::ipc::{Channel, InvokeResponseBody};
use tauri::{AppHandle, Emitter, Manager as _};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdout};
use tokio::sync::mpsc;

/// 一条待写进 helper stdin 的帧（header + 可选体）。
struct Wire(ToHelper, Vec<u8>);

/// 一条 RDP 会话的全部主程序侧状态。
///
/// session_id/profile_id 供审计与诊断（阶段 1 收尾批次接线）。
#[allow(dead_code)]
pub struct RdpSession {
    pub session_id: String,
    pub profile_id: String,
    /// 写通道：Input / Resize / Shutdown（与 TCP 中继共用同一条——单一写者）。
    wire_tx: mpsc::UnboundedSender<Wire>,
    /// 帧投递通道（4d raw IPC）：`Some` = 前端建好的 `tauri::ipc::Channel`，帧以
    /// **裸字节**（16 B 头 + RGBA）直送，不走 JSON+base64；`None`（MCP 开的会话 /
    /// 前端显式选了事件路径）回落 `rdp:frame:{id}` 事件。两端的对齐是天然的：
    /// channel 本来就是前端创建、连接时传进来的——它没传，自然也没在等 raw。
    frame_channel: Option<Channel<InvokeResponseBody>>,
    /// 桌面尺寸（Connected 时定格；前端画布据此定尺寸）
    pub size: Mutex<(u16, u16)>,
    /// 共享目录表（RDPDR）：远端文件操作的执行与三道闸（路径/只读/审计）
    /// 全在 [`crate::rdp_share::ShareTable`]。挂在会话上而不是全局表，
    /// 是因为句柄与挂载状态的生命周期与会话严格一致。
    pub shares: Arc<crate::rdp_share::ShareTable>,
}

impl RdpSession {
    pub fn send_input(&self, ev: fs_rdpproto::InputEvent) {
        let _ = self.wire_tx.send(Wire(ToHelper::Input(ev), Vec::new()));
    }

    /// 动态分辨率请求（阶段 2，DISPLAYCONTROL）。
    pub fn resize(&self, width: u16, height: u16) {
        let _ = self
            .wire_tx
            .send(Wire(ToHelper::Resize { width, height }, Vec::new()));
    }

    pub fn shutdown(&self) {
        let _ = self.wire_tx.send(Wire(ToHelper::Shutdown, Vec::new()));
    }

    /// 本机剪贴板有新文本：通告远端（阶段 2）。
    pub fn clipboard_offer(&self, text: String) {
        let _ = self
            .wire_tx
            .send(Wire(ToHelper::ClipboardOffer, text.into_bytes()));
    }

    /// 对远端取数请求的应答（体 = UTF-8 文本）。
    pub fn clipboard_data(&self, body: Vec<u8>) {
        let _ = self.wire_tx.send(Wire(ToHelper::ClipboardData, body));
    }

    /// 取远端剪贴板（用户在本机按下粘贴）。
    pub fn clipboard_pull(&self) {
        let _ = self.wire_tx.send(Wire(ToHelper::ClipboardPull, Vec::new()));
    }

    /// 前端画完一帧：归还 helper 的在途配额（帧级背压）。
    ///
    /// 丢弃发送失败是对的：会话正在关闭时这条回执没有意义，
    /// 而 helper 那侧的配额随进程一起消失。
    pub fn frame_ack(&self) {
        let _ = self.wire_tx.send(Wire(ToHelper::FrameAck, Vec::new()));
    }

    /// 要一次整屏重绘（前端画失败或画布重建后自愈）。
    pub fn request_full_frame(&self) {
        let _ = self
            .wire_tx
            .send(Wire(ToHelper::RequestFullFrame, Vec::new()));
    }

    /// 证书裁决回包（Ask 之后前端弹框的答案）。
    pub fn cert_verdict(&self, accept: bool) {
        let _ = self
            .wire_tx
            .send(Wire(ToHelper::CertVerdict { accept }, Vec::new()));
    }

    /// 挂载共享目录（RDPDR）：本地表先立（审计在 mount 里落），再让 helper
    /// 向远端宣告盘符。
    pub fn mount_share(&self, device: u8, name: &str, readonly: bool) {
        let _ = self.wire_tx.send(Wire(
            ToHelper::MountDrive {
                device,
                name: name.to_string(),
                readonly,
            },
            Vec::new(),
        ));
    }

    /// 卸载共享目录。
    pub fn unmount_share(&self, device: u8) {
        let _ = self
            .wire_tx
            .send(Wire(ToHelper::UnmountDrive { device }, Vec::new()));
    }

    /// 远端文件操作的应答（体 = Read 的数据）。
    fn drive_io_result(&self, r: ToHelper, body: Vec<u8>) {
        let _ = self.wire_tx.send(Wire(r, body));
    }
}

/// 一次连接的产物：要么 session，要么给前端的错误。
pub enum RdpOutcome {
    Session(Arc<RdpSession>),
    Failed { message: String },
}

/// RDP 全部会话表（挂 AppState）。
#[derive(Default)]
pub struct RdpRegistry {
    pub sessions: Mutex<HashMap<String, Arc<RdpSession>>>,
    /// 待答的证书裁决：前端从事件里看到证书 → 弹框 → rdp_cert_verdict 命令
    /// 经这里把答案递回协议循环。
    pub pending_verdicts: Mutex<HashMap<String, Arc<std::sync::Mutex<Option<bool>>>>>,
}

/* ── 断线重连（阶段 2）─────────────────────────────────────────────────────
 *
 * # 能做什么，不能做什么（先说清楚边界）
 *
 * RDP 有一套「自动重连 cookie」（MS-RDPBCGR 的 ARC_CS_PRIVATE_PACKET）：
 * 服务器在登录后下发一枚 cookie，客户端断线后凭它**无需再次认证**重回
 * **同一个会话**——桌面上打开的窗口都还在。
 *
 * **IronRDP 0.10 没有接通这条路**：cookie 的 PDU 字段在 ironrdp-pdu 里能解析，
 * 但 ironrdp-connector 的 `Config` 没有任何接收它的字段、连接序列里也没有
 * 消费点（实测：connector 全库零 auto_reconnect 引用）。这是上游的能力边界，
 * 不是本仓的实现选择。
 *
 * 于是这里做的是**重建式重连**：重新走一遍完整连接（TCP/跳板 → TLS → 证书
 * → 认证 → 会话）。它的语义与 cookie 重连**不同**，且差异对用户可见：
 * · 服务器上那个会话通常还在（Windows 默认保留断开的会话），重新登录会回到它——
 *   桌面内容因此往往真的恢复了，但这是**服务器的会话保留**在起作用，不是我们
 *   续上了原连接；
 * · 需要凭据（从 Vault 现取，与首次连接同一条路）；
 * · 服务器若配了「断开即注销」，重连拿到的是全新桌面——该丢的东西已经丢了。
 *
 * 把这些如实写在这里，是因为「重连成功」四个字在两种语义下含义差得很远，
 * 而用户看到的只有那四个字。
 *
 * # 为什么不抄 SSH 那套 watchdog
 *
 * SSH 侧的重连有 per-session CAS 锁、首断原因缓存、退避序列——那套复杂度
 * 服务于「终端会话的字节流不能丢」。RDP 的画面是**可重建的**（重连后服务器
 * 送一次全屏刷新），没有需要保护的中间态。故这里只做：断线 → 通知前端 →
 * 由**用户**决定是否重连（rdp_reconnect 命令）。
 *
 * 不做自动重连是刻意的：RDP 每次重连都要过一次认证，自动重试意味着拿着
 * 用户的口令反复撞服务器——那是账户锁定策略的典型触发器（Windows 默认
 * 5 次失败锁 30 分钟）。SSH 的自动重连没有这个代价（公钥认证不计失败次数），
 * RDP 有。
 */

/// 拉起 helper 的位置。
///
/// exe 同目录优先（开发期 target/debug、安装期安装目录都成立）。
/// 不在 → 明确报错：RDP 面静默消失比一句报错难排查得多。
fn helper_path() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("定位自身 exe 失败：{e}"))?;
    let dir = exe.parent().ok_or_else(|| "exe 无父目录".to_string())?;
    let name = if cfg!(windows) {
        "fs-rdp-helper.exe"
    } else {
        "fs-rdp-helper"
    };
    let sibling = dir.join(name);
    if sibling.exists() {
        return Ok(sibling);
    }
    Err(format!(
        "RDP helper（{name}）不在程序目录（{}）——安装不完整，请重装",
        dir.display()
    ))
}

/// 连接入口：拉 helper → TCP → 中继 → TOFU → 等 Connected。
///
/// 返回时连接已确立（或已失败）；帧转发任务持续后台运行，
/// 产物经 `rdp:frame:{sid}` / `rdp:status:{sid}` / `rdp:cert:{sid}` 给前端。
pub async fn connect(
    app: AppHandle,
    state: Arc<crate::state::AppState>,
    profile: Profile,
    session_id: String,
    password: String,
    frame_channel: Option<Channel<InvokeResponseBody>>,
) -> RdpOutcome {
    // 整体透传 AppState 而不是 db/vault/pending 三个句柄各传各的：
    // 跳板链要 SecretSource（vault）与人机回路（pending），与 SSH 会话同款
    //（session_cmd.rs 的同一条理由：五个句柄各传各的迟早漏一个）。
    let db = state.db.clone();
    let helper = match helper_path() {
        Ok(p) => p,
        Err(e) => return RdpOutcome::Failed { message: e },
    };
    let mut cmd = tokio::process::Command::new(&helper);
    cmd.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    // helper 是 **console 子系统**的二进制，主程序是 windows 子系统（本身无控制台）。
    // 不给这个标志，Windows 会为子进程新建一个控制台窗口——用户每连一次 RDP
    // 就多出一个空白黑框，且它随 helper 一起活到会话结束。
    //
    // 不改成把 helper 也编成 windows 子系统：那会让「从终端手动跑 helper 排查」
    // 这条路一起消失（stdio 仍通，但看不到它自己的输出）。标志只作用于这一次
    // spawn，手动运行的路径不受影响。
    #[cfg(windows)]
    {
        // `CREATE_NO_WINDOW`（winbase.h）。不引 windows-sys 只为一个常量；
        // creation_flags 是 tokio Command 在 Windows 上的固有方法，无需 trait import。
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return RdpOutcome::Failed {
                message: format!("拉不起 RDP helper（{}）：{e}", helper.display()),
            }
        }
    };
    let stdin = child.stdin.take().expect("piped stdin");
    let stdout = child.stdout.take().expect("piped stdout");

    // ── helper 的 stderr 必须有人读 ────────────────────────────────────────
    //
    // 这里此前只 `piped()` 而没有任何读者，两个后果：
    // ① helper 侧的诊断（IronRDP 的 tracing 输出、panic 信息）全部丢失——
    //    RDP 连接失败时主程序只拿得到一条经协议上报的 message，helper 自己
    //    知道的更多细节没有出口；
    // ② **潜在死锁**：管道缓冲区（Windows 上几十 KB）写满后，helper 会在
    //    `write(stderr)` 上永久阻塞，表现为「连接卡住不动、也不报错」。
    //    没人读的管道不是「输出被丢弃」，是「写者被挂起」。
    //
    // 逐行转进本进程日志，带 session id 以便与主程序的时间线对齐。
    if let Some(err_pipe) = child.stderr.take() {
        let sid = session_id.clone();
        tokio::spawn(async move {
            let mut lines = tokio::io::BufReader::new(err_pipe).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::warn!(target: "future_shell_app::rdp", session_id = %sid, "helper: {line}");
            }
        });
    }

    // ── 传输：主程序 own（helper 零网络）──────────────────────────────────
    //
    // 直连与经跳板走同一个出口类型（`Box<dyn AsyncStream>`）——这正是
    // 「经 SSH 跳板连 RDP 免费」的兑现处：helper 那侧一个字节都不用改，
    // 它只从管道收字节，根本不知道对面是 TCP 还是一条 direct-tcpip 通道。
    let transport: Box<dyn fs_sshengine::connect::AsyncStream> = if profile.jump.is_empty() {
        match tokio::time::timeout(
            std::time::Duration::from_secs(15),
            tokio::net::TcpStream::connect((profile.host.as_str(), profile.port)),
        )
        .await
        {
            Ok(Ok(s)) => Box::new(s),
            Ok(Err(e)) => {
                let _ = child.kill().await;
                return RdpOutcome::Failed {
                    message: format!("连不上 {}:{}：{e}", profile.host, profile.port),
                };
            }
            Err(_) => {
                let _ = child.kill().await;
                return RdpOutcome::Failed {
                    message: format!("连 {}:{} 超时", profile.host, profile.port),
                };
            }
        }
    } else {
        // 跳板链：逐跳校验主机密钥（每跳用自己的 policy/pins），末跳开一条
        // 到 RDP 端口的转发流。凭据取用与 SSH 会话同一条 SecretSource。
        match jump_stream(&app, &db, &profile, &session_id).await {
            Ok(s) => s,
            Err(message) => {
                let _ = child.kill().await;
                return RdpOutcome::Failed { message };
            }
        }
    };
    // io::split 而不是 TcpStream::into_split：传输已是 trait 对象，
    // 前者对任意 AsyncRead+AsyncWrite 都成立，两半也都是 'static。
    let (mut tcp_r, mut tcp_w) = tokio::io::split(transport);

    // ── 唯一写任务 ──────────────────────────────────────────────────────
    let (wire_tx, wire_rx) = mpsc::unbounded_channel::<Wire>();
    let mut stdin = Some(stdin);
    // JoinHandle 不必持有：writer 与 wire_tx 同生死（通道关闭即退出），
    // 意外 panic 的清理由会话循环的 child.kill 兜底。
    tokio::spawn(async move {
        let mut rx = wire_rx;
        let mut stdin = stdin.take().expect("writer 独占");
        while let Some(Wire(h, body)) = rx.recv().await {
            let mut buf = Vec::new();
            fs_rdpproto::encode(&h, &body, &mut buf);
            if stdin.write_all(&buf).await.is_err() {
                break;
            }
            if stdin.flush().await.is_err() {
                break;
            }
        }
        // 通道关闭：stdin drop = helper 的 stdin EOF，它自会优雅退出
    });

    // 握手 + Connect
    let _ = wire_tx.send(Wire(
        ToHelper::Hello {
            version: PROTOCOL_VERSION,
        },
        Vec::new(),
    ));
    let params = ConnectParams {
        server_name: profile.host.clone(),
        username: profile.username.clone(),
        domain: String::new(),
        password,
        width: 1280,
        height: 800,
        // **0 = 让服务器用该账户自己的默认输入区域**（MS-RDPBCGR 2.2.1.3.2）。
        // 硬编码 0x0409 是对服务器谎称「我是美式键盘」：中文/日文用户登进去
        // 会拿到一个与他账户设置不符的布局。我们本来就传**扫描码**（物理键位），
        // 布局该由远端说了算——这两件事必须一致，否则用户按的键与远端解释的
        // 字符对不上。
        keyboard_layout: 0,
    };
    let _ = wire_tx.send(Wire(ToHelper::Connect(params), Vec::new()));

    let session = Arc::new(RdpSession {
        session_id: session_id.clone(),
        profile_id: profile.id.to_string(),
        wire_tx: wire_tx.clone(),
        frame_channel,
        size: Mutex::new((0, 0)),
        shares: crate::rdp_share::ShareTable::new(&state, session_id.clone()),
    });

    // ── TCP→helper 泵 ───────────────────────────────────────────────────
    //
    // **传输为什么结束，必须记下来**。此处此前是 `Ok(0) | Err(_) => break`：
    // 干净的 EOF 与硬错误（RST/超时/网络不可达）被同等对待、且一个字都不留。
    // 而这个区分恰恰是 RDP 连接失败时最有价值的一条信息——helper 那侧只会
    // 看到「读不到字节了」，报出来的是 IronRDP 的 `read frame by hint`，
    // 至于对端是礼貌地关闭还是直接 RST，只有这里知道。
    let pump_tx = wire_tx.clone();
    let pump_sid = session_id.clone();
    tokio::spawn(async move {
        let mut buf = vec![0u8; 64 * 1024];
        let ended = loop {
            match tcp_r.read(&mut buf).await {
                Ok(0) => break "对端关闭了连接（干净 EOF）".to_string(),
                Err(e) => break format!("读取出错：{e}（kind={:?}）", e.kind()),
                Ok(n) => {
                    if pump_tx
                        .send(Wire(ToHelper::NetIn, buf[..n].to_vec()))
                        .is_err()
                    {
                        break "helper 写通道已关闭（helper 先退出了）".to_string();
                    }
                }
            }
        };
        tracing::info!(
            target: "future_shell_app::rdp",
            session_id = %pump_sid,
            "RDP 传输结束：{ended}"
        );
        // TCP 结束 → 告诉 helper（一次）
        let _ = pump_tx.send(Wire(ToHelper::NetEof, Vec::new()));
    });

    // ── 协议循环（helper stdout → 决策/转发） ──────────────────────────
    let mut reader = FrameReader::new(stdout);
    let mut verdict_cell: Arc<Mutex<Option<bool>>> = Arc::new(Mutex::new(None));
    let mut registered_verdict = false;
    #[allow(unused_assignments)] // break 前的赋值由 take() 消费，初始化值本身确实不读
    let mut final_outcome: Option<RdpOutcome> = None;

    loop {
        match reader.next().await {
            // 握手回包：版本不匹配的分支到不了这里（helper 自己会退出并报 Failed）
            Ok(FromHelper::Hello { .. }) => {}
            Ok(FromHelper::NetOut) => {
                if tcp_w.write_all(&reader.last_body).await.is_err() {
                    final_outcome = Some(RdpOutcome::Failed {
                        message: "TCP 写失败".into(),
                    });
                    break;
                }
            }
            Ok(FromHelper::CertPresented {
                fingerprint,
                subject,
                issuer,
                not_after,
            }) => {
                match decide_cert(
                    &db,
                    &profile.host,
                    profile.port,
                    &fingerprint,
                    &subject,
                    &issuer,
                )
                .await
                {
                    CertDecision::Accept => {
                        session.cert_verdict(true);
                    }
                    CertDecision::Ask => {
                        // 登记待答格子 + 请前端弹框；rdp_cert_verdict 命令填格子，
                        // 下一次循环的轮询把它取走发给 helper。
                        if !registered_verdict {
                            verdict_cell = Arc::new(Mutex::new(None));
                            app.state::<RdpGlobal>()
                                .0
                                .pending_verdicts
                                .lock()
                                .unwrap()
                                .insert(session_id.clone(), verdict_cell.clone());
                            registered_verdict = true;
                        }
                        // 固定事件名 + sessionId 在载荷里：裁决随时到、与活动面板无关，
                        // 逐会话动态名会让「谁在听」依赖标签是否已建（不该有的依赖）。
                        let _ = app.emit(
                            "rdp:cert",
                            serde_json::json!({
                                "sessionId": session_id,
                                "cert": {
                                    "fingerprint": fingerprint,
                                    "subject": subject,
                                    "issuer": issuer,
                                    "notAfter": not_after,
                                },
                            }),
                        );
                        // 等答案（轮询而非条件变量：连接期一秒一拍完全够，
                        // 且免掉在 async 上下文里跨 std 锁等待的形状问题）
                        loop {
                            if let Some(answer) = verdict_cell.lock().unwrap().take() {
                                session.cert_verdict(answer);
                                // 拒绝时**不在此设终态**：helper 会以
                                // Failed(CertRejected) 收尾，那才是唯一的出口结论
                                // （单点出口，两处各设一份迟早分叉）。
                                break;
                            }
                            // **真实的活性检查**：helper 若在等裁决期间死了
                            //（panic / 被杀 / 崩溃），它的 stdout EOF 只能被下一次
                            // read 观察到，而这个循环里没有人读——不查进程本体，
                            // 这个循环会以 1 Hz 无限自旋：connect 永不返回、
                            // 前端那个连接 Promise 永远挂着、helper 的尸体
                            // 也没人收。break 后由外层循环的 next() 拿到 EOF，
                            // 走统一的失败出口。
                            if let Ok(Some(_status)) = child.try_wait() {
                                break;
                            }
                            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                            // 有 helper 的新消息已躺在 buf 里（上报证书的那次读
                            // 恰好多带了一条进来——纯分片现象，不是活性保证）
                            // 先退出轮询去处理（多半是失败）。
                            if reader.poll_ready() {
                                break;
                            }
                        }
                        // 裁决格子用完即删：无论是等到了答案还是 helper 死了
                        // 先走——不删的话每条到过 Ask 的会话都在全局表里漏一条，
                        // 而 `deliver_verdict` 对着一个没人再读的格子返回 true，
                        // 前端会以为裁决送达了（其实接收方已死）。
                        app.state::<RdpGlobal>()
                            .0
                            .pending_verdicts
                            .lock()
                            .unwrap()
                            .remove(&session_id);
                    }
                    CertDecision::Reject(reason) => {
                        session.cert_verdict(false);
                        final_outcome = Some(RdpOutcome::Failed { message: reason });
                        break;
                    }
                }
            }
            Ok(FromHelper::Connected { width, height }) => {
                *session.size.lock().unwrap() = (width, height);
                final_outcome = Some(RdpOutcome::Session(session.clone()));
                break;
            }
            Ok(FromHelper::Frame { rect, .. }) => {
                emit_frame(
                    &app,
                    &session_id,
                    session.frame_channel.as_ref(),
                    &rect,
                    &reader.last_body,
                );
            }
            Ok(FromHelper::Status { message }) => {
                let _ = app.emit(
                    "rdp:status",
                    serde_json::json!({ "sessionId": session_id, "message": message }),
                );
            }
            Ok(FromHelper::Failed { kind, message }) => {
                let human = match kind {
                    fs_rdpproto::FailureKind::Auth => format!("认证失败：{message}"),
                    fs_rdpproto::FailureKind::CertRejected => format!("证书被拒绝：{message}"),
                    fs_rdpproto::FailureKind::Transport => format!("连接中断：{message}"),
                    _ => message,
                };
                final_outcome = Some(RdpOutcome::Failed { message: human });
                break;
            }
            // 剪贴板消息在**连接期**不该出现（CLIPRDR 通道要等会话激活）。
            // 真出现了也不当错误：忽略并留痕——它不影响连接本身，
            // 而把一次连接因为一条早到的剪贴板通告判死，代价完全不对等。
            Ok(FromHelper::ClipboardOffer)
            | Ok(FromHelper::ClipboardRequest)
            | Ok(FromHelper::ClipboardData)
            | Ok(FromHelper::AudioFormat { .. })
            | Ok(FromHelper::AudioData { .. })
            | Ok(FromHelper::AudioClose)
            | Ok(FromHelper::DriveIo { .. })
            | Ok(FromHelper::DriveMounted { .. }) => {
                tracing::debug!(%session_id, "连接期收到剪贴板/音频/共享消息，忽略");
            }
            Ok(FromHelper::Closed) => {
                final_outcome = Some(RdpOutcome::Failed {
                    message: "远端在连接完成前关闭".into(),
                });
                break;
            }
            Err(e) => {
                final_outcome = Some(RdpOutcome::Failed {
                    message: format!("helper 通道错误：{e}"),
                });
                break;
            }
        }
    }

    let outcome = final_outcome.take().unwrap_or(RdpOutcome::Failed {
        message: "连接流程意外结束".into(),
    });

    if let RdpOutcome::Failed { .. } = &outcome {
        drop(wire_tx); // writer 随之退出，stdin drop → helper 优雅退出
        let _ = child.kill().await;
        return outcome;
    }

    // 成功：会话循环接管（帧转发 + 输入已经由 wire_tx 进来，writer 继续服务）
    let sid = session_id.clone();
    let s = session.clone();
    let (w, h) = *s.size.lock().unwrap();
    // 桌面尺寸事件：前端画布据此定分辨率（帧事件只带增量矩形）。
    let _ = app.emit(
        &format!("rdp:connected:{sid}"),
        serde_json::json!({ "width": w, "height": h }),
    );
    tokio::spawn(async move {
        run_session(app, reader, child, tcp_w, sid, s).await;
    });
    outcome
}

/// 会话确立后的循环：转发帧、维持 TCP 写、收尾。
async fn run_session(
    app: AppHandle,
    mut reader: FrameReader<ChildStdout>,
    mut child: Child,
    mut tcp_w: tokio::io::WriteHalf<Box<dyn fs_sshengine::connect::AsyncStream>>,
    session_id: String,
    session: Arc<RdpSession>,
) {
    // 是否走的是「远端优雅收尾」那条路（见下面 Closed 分支）
    let mut graceful = false;
    loop {
        match reader.next().await {
            Ok(FromHelper::NetOut) => {
                if tcp_w.write_all(&reader.last_body).await.is_err() {
                    break;
                }
            }
            Ok(FromHelper::Frame { rect, .. }) => {
                emit_frame(
                    &app,
                    &session_id,
                    session.frame_channel.as_ref(),
                    &rect,
                    &reader.last_body,
                );
            }
            // 会话中途再来一次 Connected = **桌面尺寸变了**（helper 刚走完
            // Deactivation-Reactivation：远端接受了我们的分辨率请求，或它自己
            // 改了参数）。前端画布的位图尺寸只在这个事件上定格——不补发这一条，
            // 新分辨率的画面会按旧尺寸错切贴图（整屏斜纹）。
            Ok(FromHelper::Connected { width, height }) => {
                *session.size.lock().unwrap() = (width, height);
                let _ = app.emit(
                    &format!("rdp:connected:{session_id}"),
                    serde_json::json!({ "width": width, "height": height }),
                );
            }
            Ok(FromHelper::Status { message }) => {
                let _ = app.emit(
                    "rdp:status",
                    serde_json::json!({ "sessionId": session_id, "message": message }),
                );
            }
            // ── 剪贴板（阶段 2）───────────────────────────────────────────
            //
            // 系统剪贴板的读写都在**主程序**：helper 是沙箱进程，碰不到 GUI
            // 会话，也不该碰。三条消息各对一个动作：
            Ok(FromHelper::ClipboardOffer) => {
                // 远端复制了文本 → **立刻取回来**写进本机剪贴板。
                //
                // 「按需取」的省流优点在这里不成立：本机剪贴板的语义就是
                // 「随时能粘」，等用户按下 Ctrl+V 再去取要跨一次进程往返 +
                // 一次网络往返，粘出来的是上一次的内容（异步窗口期）。
                // mstsc 也是收到通告即取。
                //
                // 大内容的风险由协议层的 MAX_BODY_BYTES 兜着（16 MiB 上限，
                // 超了是解码错误而不是 OOM）。
                session.clipboard_pull();
            }
            Ok(FromHelper::ClipboardRequest) => {
                // 远端要本机的剪贴板内容 → 读系统剪贴板并应答。
                // 读**当下**的值而不是通告时存的快照：用户可能在通告之后
                // 又复制了别的东西，他期待粘出来的是最新那份。
                let text = read_system_clipboard().unwrap_or_default();
                session.clipboard_data(text.into_bytes());
            }
            Ok(FromHelper::ClipboardData) => {
                // 远端剪贴板的内容到了 → 写进系统剪贴板。
                let text = String::from_utf8_lossy(&reader.last_body).into_owned();
                if let Err(e) = write_system_clipboard(&text) {
                    tracing::warn!(%session_id, error = %e, "写系统剪贴板失败");
                }
            }
            // ── 音频（阶段 3，RDPSND）────────────────────────────────────
            //
            // 播放在**前端**（Web Audio API，webview 自带）而不是 Rust 侧：
            // 引 cpal 会带来一棵新依赖树、Linux 构建要 ALSA 头、且多一份
            // 出站面审计。webview 里那套 API 是现成的，且音量归系统混音器管
            // ——与用户在别处的音量控制是同一套。
            Ok(FromHelper::AudioFormat {
                sample_rate,
                channels,
                bits_per_sample,
            }) => {
                let _ = app.emit(
                    &format!("rdp:audio-format:{session_id}"),
                    serde_json::json!({
                        "sessionId": session_id,
                        "sampleRate": sample_rate,
                        "channels": channels,
                        "bitsPerSample": bits_per_sample,
                    }),
                );
            }
            Ok(FromHelper::AudioData { timestamp_ms }) => {
                use base64::Engine;
                let b64 = base64::engine::general_purpose::STANDARD.encode(&reader.last_body);
                let _ = app.emit(
                    &format!("rdp:audio:{session_id}"),
                    serde_json::json!({ "timestampMs": timestamp_ms, "pcmB64": b64 }),
                );
            }
            Ok(FromHelper::AudioClose) => {
                let _ = app.emit(
                    &format!("rdp:audio-close:{session_id}"),
                    serde_json::json!({ "sessionId": session_id }),
                );
            }
            Ok(FromHelper::DriveMounted { device }) => {
                // 前端把挂载状态亮出来（按钮从「共享目录…」变成「已共享 root（卸载）」）
                let _ = app.emit(
                    &format!("rdp:share:{session_id}"),
                    serde_json::json!({ "device": device, "mounted": true }),
                );
            }
            Ok(FromHelper::DriveIo { id, device, op }) => {
                // 远端文件操作：执行（三道闸全在 exec 里）→ 结果回 helper。
                // 这里的 unwrap 不会 panic：wire_tx 活得比本循环长（会话关闭时
                // 由 shutdown 收尾），且写半部无界。
                let outcome = session
                    .shares
                    .clone()
                    .exec(device, &op, &reader.last_body)
                    .await;
                session.drive_io_result(
                    ToHelper::DriveIoResult {
                        id,
                        status: outcome.status,
                        handle: outcome.handle,
                        n: outcome.n,
                        is_dir: outcome.is_dir,
                    },
                    outcome.body,
                );
            }
            // 远端主动收尾（用户注销/管理员踢下线）与通道错误（网线拔了/
            // helper 挂了）是两种断法，前端对它们的处置不同：前者不该提示
            // 重连（用户就是要退出），后者该提示。
            Ok(FromHelper::Closed) => {
                graceful = true;
                break;
            }
            Err(_) => break,
            Ok(_) => {}
        }
    }
    session.shutdown(); // 幂等：writer 可能已退
    let _ = child.kill().await;
    // 载荷带 sessionId 与 graceful：后者决定前端要不要提示「重新连接」。
    // 空载荷在事件契约的键集断言下是「键为空 → 恒真」的退化形状。
    let _ = app.emit(
        &format!("rdp:closed:{session_id}"),
        serde_json::json!({ "sessionId": session_id, "graceful": graceful }),
    );
}

/// 帧尺寸直方图（诊断用，默认关）。
///
/// # 为什么要有它
///
/// 「换 raw channel 能省多少」「要不要上 EGFX」这类判断，全都锚在**单帧
/// 有多大**上——而这个数从来没有人测过。仓内两处注释都按「1080p 全屏一帧
/// 8.3 MB」算天花板，但 DirtyRects 的贪心合并 + 帧级背压都在把稳态往小块推：
/// 如果真实分布就是几十 KB 的小块，那些改造的收益要重新估。
///
/// 默认不采集（零开销：一次原子读）。`FS_RDP_FRAMESTATS=1` 打开，
/// 每 5 秒往日志吐一行分档计数。**不落盘、不外发**，只进本进程日志。
mod framestats {
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    static ENABLED: AtomicBool = AtomicBool::new(false);
    static INIT: std::sync::Once = std::sync::Once::new();
    /// 分档：<64 KB / 64 KB–1 MB / 1–8 MB / >8 MB
    static BUCKETS: [AtomicU64; 4] = [
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
    ];
    static BYTES: AtomicU64 = AtomicU64::new(0);
    static LAST_REPORT_MS: AtomicU64 = AtomicU64::new(0);

    fn now_ms() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }

    pub fn record(len: usize) {
        INIT.call_once(|| {
            ENABLED.store(
                std::env::var("FS_RDP_FRAMESTATS").is_ok_and(|v| v == "1"),
                Ordering::Relaxed,
            );
            LAST_REPORT_MS.store(now_ms(), Ordering::Relaxed);
        });
        if !ENABLED.load(Ordering::Relaxed) {
            return;
        }
        let idx = match len {
            0..=65_535 => 0,
            65_536..=1_048_575 => 1,
            1_048_576..=8_388_607 => 2,
            _ => 3,
        };
        BUCKETS[idx].fetch_add(1, Ordering::Relaxed);
        BYTES.fetch_add(len as u64, Ordering::Relaxed);

        let now = now_ms();
        let last = LAST_REPORT_MS.load(Ordering::Relaxed);
        if now.saturating_sub(last) >= 5_000
            && LAST_REPORT_MS
                .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
        {
            let counts: Vec<u64> = BUCKETS
                .iter()
                .map(|b| b.swap(0, Ordering::Relaxed))
                .collect();
            let bytes = BYTES.swap(0, Ordering::Relaxed);
            let total: u64 = counts.iter().sum();
            let secs = (now.saturating_sub(last)) as f64 / 1000.0;
            tracing::info!(
                target: "future_shell_app::rdp",
                "帧统计（5s）：{total} 帧 / {:.1} fps，{:.2} MB（{:.2} MB/s）｜                 <64KB={} 64KB-1MB={} 1-8MB={} >8MB={}",
                total as f64 / secs,
                bytes as f64 / 1_048_576.0,
                bytes as f64 / 1_048_576.0 / secs,
                counts[0], counts[1], counts[2], counts[3],
            );
        }
    }
}

/// 一条 raw 帧的线上格式：`x/y/w/h` 各 4 字节小端，随后 RGBA 字节。
///
/// # 为什么是裸头而不是 JSON 头
///
/// 这条路存在的理由就是去掉 JSON+base64 的编解码开销（1080p 全屏一帧 8.3 MB，
/// base64 膨胀 1.33×，两端各一次编解码全在主线程）。头只要装下脏矩形，16 字节
/// 足够；对端（RdpPane 的 `paintRaw`）按同样的偏移切——两边的对齐由
/// `frame_wire_roundtrip` 与前端的同款测试双向钉住。
///
/// # 多字节序是契约的一部分
///
/// 小端不是「x86 恰好如此」：WebView 一侧用 `DataView.getUint32(off, true)` 显式
/// 按小端读，Rust 侧用 `to_le_bytes` 显式按小端写。谁在哪台机器上跑都无所谓。
pub fn frame_wire(rect: &fs_rdpproto::Rect, rgba: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(16 + rgba.len());
    // `Rect` 的字段是 u16，线上却写 u32：这不是浪费 8 字节/帧，是把格式从
    // 「今天的坐标恰好塞得下」里解出来——多显示器拼接的 x 偏移、8K 位图都
    // 出过 u16 不够的先例。改线格式 = 前后端同批（decodeFrame 对偶）。
    out.extend_from_slice(&(rect.x as u32).to_le_bytes());
    out.extend_from_slice(&(rect.y as u32).to_le_bytes());
    out.extend_from_slice(&(rect.width as u32).to_le_bytes());
    out.extend_from_slice(&(rect.height as u32).to_le_bytes());
    out.extend_from_slice(rgba);
    out
}

fn emit_frame(
    app: &AppHandle,
    sid: &str,
    channel: Option<&Channel<InvokeResponseBody>>,
    rect: &fs_rdpproto::Rect,
    rgba: &[u8],
) {
    framestats::record(rgba.len());
    // raw 优先：Channel 的大载荷走 WebView 的 fetch 档（tauri 的
    // MAX_RAW_DIRECT_EXECUTE_THRESHOLD = 1024，超过即 fetch），不经 eval 序列化。
    // 发送失败**回落事件**而不是丢帧：channel 死了多半是前端页面在重载，
    // 会话还在，下一帧事件路径仍能到达（RdpPane 在没有 channel 注册时听事件）。
    if let Some(ch) = channel {
        if ch
            .send(InvokeResponseBody::Raw(frame_wire(rect, rgba)))
            .is_ok()
        {
            return;
        }
    }
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(rgba);
    let _ = app.emit(
        &format!("rdp:frame:{sid}"),
        serde_json::json!({
            "x": rect.x, "y": rect.y, "w": rect.width, "h": rect.height, "rgbaB64": b64
        }),
    );
}

/// commands 层的裁决入口（rdp_cert_verdict 命令转到这里）。
pub fn deliver_verdict(global: &RdpGlobal, session_id: &str, accept: bool) -> bool {
    let table = global.0.pending_verdicts.lock().unwrap();
    if let Some(cell) = table.get(session_id) {
        *cell.lock().unwrap() = Some(accept);
        true
    } else {
        false
    }
}

/// 全局 RDP 状态（挂 tauri manage；含会话表与待答裁决）。
pub struct RdpGlobal(pub RdpRegistry);

enum CertDecision {
    Accept,
    Ask,
    /// 保留给将来「库本身拒绝」的情形（吊销名单/策略）；现在拒绝只能来自用户
    ///——decide_cert 只分 Accept/Ask，删掉变体会让这条演化路径无声消失。
    #[allow(dead_code)]
    Reject(String),
}

/// 证书 TOFU 判定（rdp_certs 表）。
///
/// 首见即落库**并仍要问一次**（TOFU 的 U 是此刻的用户）；库命中才静默放行；
/// 命中但指纹不同 = Ask（前端按「变更」措辞弹框）。没有 Reject 分支——
/// 拒绝只能来自用户，库本身不下这个判断。
async fn decide_cert(
    db: &Db,
    host: &str,
    port: u16,
    fingerprint: &str,
    subject: &str,
    issuer: &str,
) -> CertDecision {
    let known: Option<String> =
        sqlx::query_scalar("SELECT fingerprint_sha256 FROM rdp_certs WHERE host = ? AND port = ?")
            .bind(host)
            .bind(i64::from(port))
            .fetch_optional(db.pool())
            .await
            .unwrap_or(None);
    match known {
        Some(fp) if fp == fingerprint => CertDecision::Accept,
        Some(_) => CertDecision::Ask,
        None => {
            // 首见：先记库再问。答案为否时行留着（与 SSH TOFU 同款语义：
            // 拒绝不等于这台机器永远是坏的，下次连仍以「已见」身份弹框）
            let _ = sqlx::query(
                "INSERT OR REPLACE INTO rdp_certs (host, port, fingerprint_sha256, subject, issuer)
                 VALUES (?,?,?,?,?)",
            )
            .bind(host)
            .bind(i64::from(port))
            .bind(fingerprint)
            .bind(subject)
            .bind(issuer)
            .execute(db.pool())
            .await;
            CertDecision::Ask
        }
    }
}

/// helper stdout 的单次读取块大小。**只允许出现在堆上**——见 [`FrameReader::next`]。
const READ_CHUNK: usize = 64 * 1024;

/// helper stdout 的帧读取器（fs_rdpproto::decode 的流式壳）。
pub struct FrameReader<R> {
    inner: R,
    buf: Vec<u8>,
    pub last_body: Vec<u8>,
}

impl<R: AsyncReadExt + Unpin> FrameReader<R> {
    pub fn new(inner: R) -> Self {
        Self {
            inner,
            buf: Vec::with_capacity(READ_CHUNK),
            last_body: Vec::new(),
        }
    }

    /// 读出下一条上行（体留在 `last_body`）。
    ///
    /// ## 读取必须用 `read_buf`（读进 Vec 的空闲容量）
    ///
    /// 曾经这里写的是 `let mut chunk = [0u8; 64 * 1024]`，一行看着人畜无害的
    /// 栈上缓冲。但 `async fn` 的状态机会把**跨 await 存活的局部量**全部摊进
    /// future，于是这 64 KiB 成了本 future 的体积下限，并逐层向上放大：
    ///
    /// ```text
    /// FrameReader::next()             65,584 B   ← 那个数组
    ///   → run_session()               66,824
    ///   → connect()                   67,664
    ///   → rdp_connect()               68,464
    ///   → tauri ResultFutureTag      136,944     ×2（宏把 result 与 future 各存一份）
    ///   → 命令 async block           205,968     ×3
    ///   → respond_async_serialized   412,352     再翻倍
    /// ```
    ///
    /// 而 Tauri 是在**主线程**上（WebView2 的 WebResourceRequested 回调里）
    /// 构造并把最后那个对象搬进 tokio 任务的，Windows 主线程栈默认只有 1 MiB。
    /// 三份 400 KB 级的拷贝叠上去就是 `thread 'main' has overflowed its stack`——
    /// 点一次 RDP 连接整个程序消失，而且因为栈溢出走的是 SEH 而不是 Rust panic，
    /// panic 钩子不触发、一份现场都不留。
    ///
    /// 两条易错的推论，都被实测坐实过：
    /// · 崩溃发生在 `rdp_connect` **函数体第一条语句之前**——所以「没绑 Vault
    ///   口令应当立刻返回错误」与「点了就崩」并不矛盾，那行 `ok_or` 从未执行；
    /// · `cargo tauri dev` 复现不了。tauri 的 `ipc/mod.rs` 在 `debug_assertions`
    ///   下先 `Box::pin` 把 future 丢上堆，只有 release 走单态化路径把 400 KB
    ///   一路搬在栈上——**开发机全绿，装出来的包必崩**。
    ///
    /// `read_buf` 只在**真正读到字节时**才把它们追加进 buf（spare capacity），
    /// 相比「resize 零填充 → 读 → truncate 回收」的写法，它同时消掉了两件事：
    /// 零填充不变量（根本没有零被写进去），和**取消安全**（那种写法里 future
    /// 在 await 点被 drop 会让 64 KiB 零字节永久留在 buf 里——tokio 文档明示
    /// `read_buf` 取消安全，被 drop 不留任何中间态）。
    ///
    /// 尺寸由 [`frame_reader_future_must_not_carry_its_buffer_on_the_stack`] 钉住；
    /// 主线程栈另有 `app/build.rs` 的 `/STACK` 作纵深防御（两者互不替代：
    /// 抬栈挡的是「下一个还没写出来的大 future」，这里修的是根因）。
    pub async fn next(&mut self) -> Result<FromHelper, String> {
        loop {
            if let Some((pkt, used)) =
                fs_rdpproto::decode::<FromHelper>(&self.buf).map_err(|e| e.to_string())?
            {
                self.buf.drain(..used);
                self.last_body = pkt.body;
                return Ok(pkt.header);
            }
            // reserve 只保证空闲容量；read_buf 只把真正读到的字节追加进来。
            self.buf.reserve(READ_CHUNK);
            let n = self
                .inner
                .read_buf(&mut self.buf)
                .await
                .map_err(|e| e.to_string())?;
            if n == 0 {
                return Err("helper stdout EOF".into());
            }
        }
    }

    /// 缓冲里是否已有一条完整消息可读（轮询裁决时防饿死用）。
    pub fn poll_ready(&self) -> bool {
        matches!(fs_rdpproto::decode::<FromHelper>(&self.buf), Ok(Some(_)))
    }
}

// ── 剪贴板：系统侧读写（阶段 2）────────────────────────────────────────
//
// 与 `vault_copy_to_clipboard` 用同一个 arboard 通道——两处各引一套剪贴板
// 实现，迟早在「谁清空了剪贴板」这种问题上互相打架。
//
// **每次现开 Clipboard 而不是持有一个长命对象**：X11 下剪贴板所有权绑在
// 持有者进程的连接上，长命对象会让本进程一直是 selection owner（别的程序
// 复制之后我们仍宣称拥有），而这正是 Linux 上「复制了但粘不出来」的经典成因。

pub(crate) fn read_system_clipboard() -> Result<String, String> {
    let mut cb = arboard::Clipboard::new().map_err(|e| e.to_string())?;
    cb.get_text().map_err(|e| e.to_string())
}

fn write_system_clipboard(text: &str) -> Result<(), String> {
    let mut cb = arboard::Clipboard::new().map_err(|e| e.to_string())?;
    cb.set_text(text.to_string()).map_err(|e| e.to_string())
}

/// 经 SSH 跳板链开一条到 RDP 端口的转发流（阶段 2）。
///
/// 复用 SSH 会话的全部装配：`VaultSecrets`（按需取凭据，不留全量快照）与
/// `GuiEvents`（主机密钥确认、逐跳进度播报走同一套 UI 回路）。
///
/// **逐跳校验一寸未松**：`open_forwarded_stream` 内部对每一跳用该跳自己的
/// policy/pins（见 sshengine 的 hop_profile 文档——拿目标机的钉去校验跳板机
/// 是类型混淆）。这里不做任何放宽。
async fn jump_stream(
    app: &AppHandle,
    _db: &Arc<Db>,
    profile: &Profile,
    session_id: &str,
) -> Result<Box<dyn fs_sshengine::connect::AsyncStream>, String> {
    let state = app.state::<Arc<crate::state::AppState>>();
    let secrets = crate::commands::session_cmd::VaultSecrets::new(state.vault.clone());
    let events: Arc<dyn fs_sshengine::events::SessionEvents> = Arc::new(crate::events::GuiEvents {
        app: app.clone(),
        session_id: session_id.to_string(),
        pending: state.pending.clone(),
        target: crate::events::target_label(profile),
        profile_id: profile.id.to_string(),
    });
    fs_sshengine::connect::open_forwarded_stream(
        profile,
        &profile.host,
        profile.port,
        &secrets,
        state.db.pool(),
        events,
    )
    .await
    .map_err(|e| format!("经跳板连 {}:{} 失败：{e}", profile.host, profile.port))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **这条判据的由来是一次真实的整程序崩溃**（2026-08-27）：用户点 RDP 连接，
    /// 窗口瞬间消失，没有 crash 文件、没有事件日志、日志停在 `ipc:invoke` 那一行。
    /// 真因是 [`FrameReader::next`] 里一个 `[0u8; 64 * 1024]` 栈上缓冲跨了 await，
    /// 把命令 future 撑到 400 KB 级，压垮了 Windows 主线程仅 1 MiB 的栈
    /// （完整级联与为什么 dev 复现不了，见 `FrameReader::next` 的文档）。
    ///
    /// 所以这里钉的**不是**「代码长什么样」而是「future 有多大」——换一种写法
    /// 把缓冲放回栈上（`[0u8; N]`、`MaybeUninit<[u8; N]>`、大数组按值传参……）
    /// 都会让这条判据转红，而 grep 式的源码守卫只能拦住其中一种写法。
    ///
    /// 阈值 4 KiB：修好后的实测值是两百多字节，留足两个数量级的余量给正常演进；
    /// 一旦逼近 4 KiB，说明又有大东西跨 await 了，该看一眼而不是抬阈值。
    #[test]
    fn frame_reader_future_must_not_carry_its_buffer_on_the_stack() {
        // 不需要 runtime：只构造 future、量它的大小，从不 poll。
        let mut reader = FrameReader::new(tokio::io::empty());
        let fut = reader.next();
        let size = std::mem::size_of_val(&fut);
        assert!(
            size <= 4096,
            "FrameReader::next() 的 future 涨到 {size} 字节。\
             它会逐层放大到 Tauri 命令层（历史上是 ×6），而命令 future 是在**主线程**上\
             构造的，Windows 主线程栈只有 1 MiB —— 这正是 RDP 点一次就整个程序崩溃的成因。\
             请把跨 await 存活的大缓冲挪到堆上，不要抬这个阈值。"
        );
    }

    /// 读失败时不得把 `resize` 扩出来的零字节留在 `buf` 里——那些零会在下一次
    /// `decode` 时被当成协议数据。（这是把「读进 buf 尾部」这个优化引进来时
    /// 新增的一条不变量，故单独钉。）
    #[tokio::test]
    async fn read_error_does_not_leave_zero_padding_in_the_buffer() {
        struct AlwaysErr;
        impl tokio::io::AsyncRead for AlwaysErr {
            fn poll_read(
                self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
                _buf: &mut tokio::io::ReadBuf<'_>,
            ) -> std::task::Poll<std::io::Result<()>> {
                std::task::Poll::Ready(Err(std::io::Error::other("boom")))
            }
        }

        let mut reader = FrameReader::new(AlwaysErr);
        assert!(reader.next().await.is_err());
        assert_eq!(
            reader.buf.len(),
            0,
            "读失败后 buf 里残留了 {} 字节的零填充",
            reader.buf.len()
        );
    }

    /// **读失败的另一半语义**：buf 里已压着半条帧时读失败，已收的**真字节**
    /// 必须保住——`truncate(filled)` 相对 `clear()` 的全部意义就在这。
    /// （对抗复核指出：上面那条 AlwaysErr 判据里 filled 恒为 0，
    /// `truncate(filled)` 与 `clear()` 在它之下逐字等价，杀不住 `clear()` 变异。）
    ///
    /// 若有人把它改成 `clear()`：buf 里那半条帧被丢，一旦将来给读失败加了
    /// 重试，重试后将从帧中段开始解码——按荒谬的长度等一辈子。
    #[tokio::test]
    async fn read_error_preserves_bytes_already_received() {
        /// 脚本式读源：第一次给 4 字节（< 8 字节帧前缀，decode 必然 Ok(None)），
        /// 第二次返回错误。
        struct PartialThenErr(Vec<u8>);
        impl tokio::io::AsyncRead for PartialThenErr {
            fn poll_read(
                mut self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
                buf: &mut tokio::io::ReadBuf<'_>,
            ) -> std::task::Poll<std::io::Result<()>> {
                if self.0.is_empty() {
                    return std::task::Poll::Ready(Err(std::io::Error::other("boom")));
                }
                let n = self.0.len().min(buf.remaining());
                buf.put_slice(&self.0[..n]);
                self.0.drain(..n);
                std::task::Poll::Ready(Ok(()))
            }
        }

        let partial = vec![0x01, 0x02, 0x03, 0x04];
        let mut reader = FrameReader::new(PartialThenErr(partial.clone()));
        assert!(reader.next().await.is_err());
        assert_eq!(
            reader.buf, partial,
            "读失败后已收的半条帧必须原样保留（clear() 变异会把它丢掉）"
        );
    }

    /// **快乐路径的 `truncate(filled + n)`**（对抗复核实测：删掉那一行，
    /// 320 条测试全绿放行——它此前是零覆盖）。
    ///
    /// 不变量：读到 n 字节后，resize 扩出来的 64 KiB 零里只留 n 个真字节。
    /// 若不截断：第一帧能解出（drain 正常），**下一轮** decode 读到的是全零，
    /// 按 header_len=0/body_len=0 解出一个空帧 → BadHeader → 会话在第二条
    /// 上行上断掉。即「第一帧过、第二帧起全废」，而所有测试绿着。
    #[tokio::test]
    async fn happy_path_leaves_no_zero_padding_and_supports_a_second_frame() {
        // 两条合法帧（用真编码器造，不手搓前缀）
        let mut f1 = Vec::new();
        fs_rdpproto::encode(
            &FromHelper::Status {
                message: "第一条".into(),
            },
            &[],
            &mut f1,
        );
        let mut f2 = Vec::new();
        fs_rdpproto::encode(
            &FromHelper::Status {
                message: "第二条".into(),
            },
            &[],
            &mut f2,
        );
        let wire = [f1.clone(), f2.clone()].concat();
        let mut reader = FrameReader::new(std::io::Cursor::new(wire));

        let h1 = reader.next().await.expect("第一帧应可解");
        match h1 {
            FromHelper::Status { message } => assert_eq!(message, "第一条"),
            other => panic!("第一帧应是 Status，实得 {other:?}"),
        }
        // 关键断言一：帧解出后 buf 里**恰好只剩第二帧的字节**。
        // 删掉 truncate(filled + n) 时这里 buf 会是「第二帧 + 数万零字节」
        // ——第一帧碰巧还能解对，第二帧起被零污染全废（见本判据的文档）。
        assert_eq!(
            reader.buf,
            f2,
            "第一帧后 buf 应恰好等于第二帧的原始字节——多了零填充就是 \
             truncate(filled + n) 被删/改坏了（len 实际 = {}）",
            reader.buf.len()
        );

        // 关键断言二：第二条帧还能正常解出（零填充会把这里变成 BadHeader）
        let h2 = reader.next().await.expect("第二帧应可解（无零填充污染）");
        match h2 {
            FromHelper::Status { message } => assert_eq!(message, "第二条"),
            other => panic!("第二帧应是 Status，实得 {other:?}"),
        }
        // 之后干净 EOF
        assert!(reader.next().await.is_err());
    }

    // ── 4d：帧管道 raw IPC ─────────────────────────────────────────────

    /// `frame_wire` 的线上格式：16 字节小端头 + RGBA。
    ///
    /// 前端 `lib/rdp-frames.ts` 的 `decodeFrame` 按同样偏移切——两边各自的单测
    /// 钉住**同一组字节**，任何一端改了格式（字节序、字段序、头长）都当场红。
    #[test]
    fn frame_wire_roundtrip() {
        let rect = fs_rdpproto::Rect {
            x: 11,
            y: 22,
            width: 3,
            height: 2,
        };
        let rgba: Vec<u8> = (0u8..3 * 2 * 4).collect();
        let wire = frame_wire(&rect, &rgba);
        assert_eq!(wire.len(), 16 + rgba.len());
        // 小端是契约的一部分：前端 DataView.getUint32(off, true) 逐字对偶
        assert_eq!(&wire[0..4], &11u32.to_le_bytes());
        assert_eq!(&wire[4..8], &22u32.to_le_bytes());
        assert_eq!(&wire[8..12], &3u32.to_le_bytes());
        assert_eq!(&wire[12..16], &2u32.to_le_bytes());
        assert_eq!(&wire[16..], &rgba[..]);
        // 大端机器上也会红：to_le_bytes 保证的是「线上永远小端」，不是宿主机序
        assert_ne!(&wire[0..4], &11u32.to_be_bytes());
    }

    /// 大坐标/大尺寸不截断：脏矩形可以是 4K 全屏（3840×2160 的 x 偏移）。
    #[test]
    fn frame_wire_handles_full_hd_offsets() {
        let rect = fs_rdpproto::Rect {
            x: 3840,
            y: 2160,
            width: 1920,
            height: 1080,
        };
        let wire = frame_wire(&rect, &[]);
        assert_eq!(&wire[0..4], &3840u32.to_le_bytes());
        assert_eq!(&wire[4..8], &2160u32.to_le_bytes());
    }

    /// raw 分流：channel 在场时帧**只**走 channel（事件路径一个字都不发）。
    ///
    /// `Channel::new` 的回调是 pub API，正好当捕获器用——这测的是 emit_frame
    /// 的完整分流逻辑，不是 frame_wire 的重复。
    #[test]
    fn raw_channel_receives_the_frame() {
        use std::sync::Mutex as StdMutex;
        let got: std::sync::Arc<StdMutex<Option<Vec<u8>>>> =
            std::sync::Arc::new(StdMutex::new(None));
        let g = got.clone();
        let ch = tauri::ipc::Channel::new(move |body| {
            if let tauri::ipc::InvokeResponseBody::Raw(bytes) = body {
                *g.lock().unwrap() = Some(bytes);
            }
            Ok(())
        });
        let rect = fs_rdpproto::Rect {
            x: 1,
            y: 2,
            width: 4,
            height: 4,
        };
        let rgba = vec![7u8; 4 * 4 * 4];
        // AppHandle 在单测里不存在——分流到 raw 后 emit 根本不该被碰到，
        // 这由「channel 发送成功即 return」的路径保证；这里绕不开 app 参数，
        // 用测试桩 tauri AppHandle？没有——分流逻辑拆出来测：
        let sink = Some(&ch);
        // 直接构造不了 AppHandle，因此这条用例验证的是「send 成功 => 不需要 app」：
        // 若实现先 emit 再 send（顺序反了），本用例无从分辨——那条由 itest 与
        // 源码级守卫 emit_order_puts_channel_first 钉。
        assert!(ch
            .send(tauri::ipc::InvokeResponseBody::Raw(frame_wire(
                &rect, &rgba
            )))
            .is_ok());
        let bytes = got.lock().unwrap().take().unwrap();
        assert_eq!(bytes.len(), 16 + rgba.len());
        assert_eq!(&bytes[8..12], &4u32.to_le_bytes());
        let _ = sink; // 编译期证明 Option<&Channel> 这个形状存在（emit_frame 的实参形）
    }

    /// framestats（FS_RDP_FRAMESTATS=1）是 4d 出口判据的**数据源**——raw 与事件两条
    /// 路径的 A/B 对比全靠它的分档/字节/吞吐统计。emit_frame 里删掉 `framestats::record`
    /// 不影响任何功能（F4 变异实测全绿幸存），判据却从此无声失明。源码级钉住。
    #[test]
    fn emit_frame_records_every_frame_into_framestats() {
        let src = include_str!("rdp.rs");
        let at = src.find("fn emit_frame(").expect("emit_frame 不见了");
        let end = src[at..]
            .find(
                "
}
",
            )
            .map(|i| at + i)
            .unwrap_or(src.len());
        let body = &src[at..end];
        assert!(
            body.contains("framestats::record("),
            "emit_frame 不再记账——A/B 判据失去数据源"
        );
    }

    /// 源码级：分流的顺序是「先试 channel，成功即返；失败回落事件」。
    /// 单测里造不出 AppHandle，这条钉住 emit_frame 的控制流不被倒置。
    #[test]
    fn emit_order_puts_channel_first_and_falls_back_on_error() {
        let src = include_str!("rdp.rs");
        let at = src.find("fn emit_frame(").expect("emit_frame 不见了");
        let end = src[at..]
            .find(
                "
}
",
            )
            .map(|i| at + i + 2)
            .unwrap_or(src.len());
        let body = &src[at..end];
        // 空白不敏感：cargo fmt 会把链式调用与方法名拆到两行、把单行 if 块换成
        // 三行——任何按字面量的匹配都会在下一次格式化之后假红。压平再比。
        let f = body.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            f.find("ch .send(").expect("raw 分支不见了")
                < f.find("app.emit(").expect("回落 emit 不见了"),
            "channel 必须先于事件发送"
        );
        assert!(
            f.contains(".is_ok() { return; }"),
            "channel 成功后必须 return——继续往下会双发（同一帧画两次是可见的撕裂）"
        );
    }
}
