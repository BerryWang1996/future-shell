//! RDP 的 IPC 门面（阶段 1）。
//!
//! 与 SSH 命令层的分工相同：这里只做参数校验、状态查取与事件转发，
//! 会话本体在 [`crate::rdp`]。
//!
//! 四条命令对应会话生命周期的四拍：
//! · `rdp_connect`：拉起 helper + TCP + TOFU 裁决，到 Connected 才返回
//!   （失败带人话）。会话 id 由这里生成（与 SSH 会话同款 uuid）。
//! · `rdp_cert_verdict`：Ask 之后前端弹框的答案回传。
//! · `rdp_input`：键鼠事件（RdpPane 打包成 fs_rdpproto::InputEvent）。
//! · `rdp_close`：优雅退出（Shutdown 帧 → helper 优雅关机 → 子进程收尾）。
//!
//! **口令从 Vault 现取现用**：与 SSH 同款纪律——不缓存、不落盘、IPC 期间
//! 也不进日志；rdp_connect 的 password 参数只在本函数作用域存活，
//! 随 ConnectParams 进 helper（用完即被 zeroize 的义务写在协议文档里）。

use crate::rdp::{self, RdpGlobal, RdpOutcome};
use crate::state::AppState;
use std::sync::Arc;
use tauri::{Manager as _, State};

/// 连接结果（前端据此建标签或弹错）。
#[derive(serde::Serialize)]
pub struct RdpConnectResult {
    pub ok: bool,
    pub session_id: String,
    pub width: u16,
    pub height: u16,
    pub message: String,
}

// 帧直送通道（4d raw IPC）作为参数传进来：前端在调用**之前**创建并注册好回调
//（见 lib/rdp-frames.ts）。非 Option 是刻意的：`Channel` 走 tauri 的 CommandArg
// 而不是 serde，`Option<Channel>` 反而不被宏接受；「没有通道」的形态（MCP 一侧）
// 不经这个 IPC 壳，见 [`rdp_connect_core`]。
#[tauri::command]
pub async fn rdp_connect(
    profile_id: String,
    frame_channel: tauri::ipc::Channel<tauri::ipc::InvokeResponseBody>,
    state: State<'_, Arc<AppState>>,
    app: tauri::AppHandle,
) -> Result<RdpConnectResult, String> {
    rdp_connect_core(profile_id, Some(frame_channel), state, app).await
}

/// `rdp_connect` 的本体。`frame_channel = None` = 走事件路径（MCP / 无前端的调用方）。
async fn rdp_connect_core(
    profile_id: String,
    frame_channel: Option<tauri::ipc::Channel<tauri::ipc::InvokeResponseBody>>,
    state: State<'_, Arc<AppState>>,
    app: tauri::AppHandle,
) -> Result<RdpConnectResult, String> {
    // Profile 现查：协议/主机/端口/用户名以**连接时刻**的库为准（与 SSH 同款）
    let profile = fs_connmgr::ProfileRepo::new(state.db.pool())
        .get(uuid::Uuid::parse_str(&profile_id).map_err(|e| format!("profile id 不合法：{e}"))?)
        .await
        .map_err(|e| format!("查不到连接配置：{e}"))?;
    if profile.protocol != fs_connmgr::Protocol::Rdp {
        return Err("该配置不是 RDP 协议（protocol != rdp）".into());
    }
    // 会话 id 先生成：口令询问的事件要带着它（前端据此把应答归位），
    // 而询问发生在连接之前。
    let session_id_for_prompt = uuid::Uuid::new_v4().to_string();

    // ── 口令来源：与 SSH 完全同构的三级回落 ──────────────────────────────
    //
    // **不设 password IPC 参数**：给前端一个「按 profile id 换明文口令」的通道
    // 等于给任何能 invoke 的代码开一条秘密外泄面——SSH 从来没有这条通道，
    // RDP 不做第一个。
    //
    // 但「没绑 Vault 记录就直接报错」是错的（2026-08-28 用户反馈）：那让
    // **Vault 变成用 RDP 的前提**，而 SSH 从来不是这样——SSH 没绑凭据时会弹
    // 框问口令，用户可勾「记住」再存进 Vault。Vault 该是「想用才用的应用锁」，
    // 不是「不建就没法连」的关卡。
    //
    // 三级：① 绑了记录且 Vault 已解锁 → 现取；② 绑了但没解锁 → 弹框问
    //（用户可能这次不想解锁整个保险库，只想连这一台）；③ 没绑 → 弹框问。
    // 后两者走的是**与 SSH 同一条** `auth:prompt` 事件 + `auth_respond` 应答，
    // 前端复用 AuthPromptDialog（含「记住（存入 Vault）」那个勾）。
    let from_vault = profile.auth.vault_record.and_then(|record| {
        crate::commands::vault_cmd::touch_vault_activity();
        let guard = state.vault.try_lock().ok()?;
        let store = guard.as_ref()?;
        let (kind, bytes) = store.get_typed(record).ok()?;
        if kind != fs_vault::SecretKind::Password {
            // 绑错了类型（比如绑成私钥）：不静默当没绑——那样用户会以为
            // 「记住」没生效。落到问口令那条路，但留一行痕迹。
            tracing::warn!(record, "RDP 连接绑定的 vault 记录不是口令类型，改为询问");
            return None;
        }
        String::from_utf8(bytes.to_vec()).ok()
    });

    let password = match from_vault {
        Some(p) => p,
        None => {
            let events = crate::events::GuiEvents {
                app: app.clone(),
                session_id: session_id_for_prompt.clone(),
                pending: state.pending.clone(),
                target: crate::events::target_label(&profile),
                profile_id: profile.id.to_string(),
            };
            // 直接调 SessionEvents 的口令询问：RDP 与 SSH 共用同一条链路
            // （同一个事件名、同一个应答命令、同一个前端对话框），复用而不是
            // 仿造——仿造迟早在超时/取消语义上走样。
            // 提示语分两种（2026-08-31 与 SSH 同口径）：档案**绑了**记录但保险库
            // 锁着取不到 → 说清是锁着（否则「未配置」那句会让用户以为自己存的
            // 凭据丢了）；真的没绑 → 原「未配置」。
            use fs_sshengine::events::SessionEvents as _;
            let vault_locked = profile.auth.vault_record.is_some() && {
                crate::commands::vault_cmd::touch_vault_activity();
                let guard = state.vault.try_lock().ok();
                guard.as_ref().is_none_or(|g| g.as_ref().is_none())
            };
            let answer = if vault_locked {
                events.password_prompt_reason(
                    "保险库未解锁，本次取不到已保存的口令——可输入本次使用的口令，或取消后先解锁保险库再连",
                )
            } else {
                events.password_prompt()
            };
            if answer.is_empty() {
                // 空应答 = 用户取消或超时。**不当成空口令去撞服务器**：
                // RDP 每次失败都计入账户锁定策略（Windows 默认 5 次锁 30 分钟）。
                return Ok(RdpConnectResult {
                    ok: false,
                    session_id: session_id_for_prompt,
                    width: 0,
                    height: 0,
                    message: "已取消（未输入口令）".into(),
                });
            }
            answer
        }
    };

    let session_id = session_id_for_prompt;
    match rdp::connect(
        app.clone(),
        (*state).clone(),
        profile,
        session_id.clone(),
        password,
        frame_channel,
    )
    .await
    {
        RdpOutcome::Session(s) => {
            let (w, h) = *s.size.lock().unwrap();
            let global = app.state::<RdpGlobal>();
            global
                .0
                .sessions
                .lock()
                .unwrap()
                .insert(session_id.clone(), s);
            Ok(RdpConnectResult {
                ok: true,
                session_id,
                width: w,
                height: h,
                message: String::new(),
            })
        }
        RdpOutcome::Failed { message } => Ok(RdpConnectResult {
            ok: false,
            session_id,
            width: 0,
            height: 0,
            message,
        }),
    }
}

#[tauri::command]
pub async fn rdp_cert_verdict(
    session_id: String,
    accept: bool,
    app: tauri::AppHandle,
) -> Result<bool, String> {
    let global = app.state::<RdpGlobal>();
    Ok(rdp::deliver_verdict(&global, &session_id, accept))
}

#[tauri::command]
pub async fn rdp_input(
    session_id: String,
    event: fs_rdpproto::InputEvent,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let global = app.state::<RdpGlobal>();
    let table = global.0.sessions.lock().unwrap();
    match table.get(&session_id) {
        Some(s) => {
            s.send_input(event);
            Ok(())
        }
        None => Err("没有这个 RDP 会话（可能已关闭）".into()),
    }
}

#[tauri::command]
pub async fn rdp_close(session_id: String, app: tauri::AppHandle) -> Result<(), String> {
    let global = app.state::<RdpGlobal>();
    let table = global.0.sessions.lock().unwrap();
    match table.get(&session_id) {
        Some(s) => {
            s.shutdown();
            Ok(())
        }
        None => Ok(()), // 幂等：关一个已关的会话不是错误
    }
}

/// 断线重连（阶段 2）：**重建式**，不是续上原连接。
///
/// 语义边界写在 `crate::rdp` 的重连段注释里（IronRDP 0.10 未接通自动重连
/// cookie，故每次重连都要重新认证；桌面内容能否恢复取决于**服务器**的会话
/// 保留策略，不是我们续上了什么）。
///
/// 由用户显式触发而不是自动重试：RDP 每次重连都过一次认证，自动重试等于
/// 拿用户口令反复撞服务器——Windows 默认 5 次失败锁 30 分钟。
///
/// 旧会话先从表里摘掉再连：新会话有新的 session_id（前端标签随之换 id），
/// 留着旧的只会让「哪个是活的」变成一个需要读代码才知道的问题。
// 重连必须**重新建 channel**（参数由此传入）：旧 channel 的回调闭包绑着旧会话的
// 绘制状态，新会话有新 id，旧 channel 上到达的帧没有任何画布认领（sink 表按
// session_id 键）。前端在调这里之前先 dropFrameChannel(旧) 再建新的。
#[tauri::command]
pub async fn rdp_reconnect(
    session_id: String,
    frame_channel: tauri::ipc::Channel<tauri::ipc::InvokeResponseBody>,
    state: State<'_, Arc<AppState>>,
    app: tauri::AppHandle,
) -> Result<RdpConnectResult, String> {
    let profile_id = {
        let global = app.state::<RdpGlobal>();
        let mut table = global.0.sessions.lock().unwrap();
        let old = table
            .remove(&session_id)
            .ok_or("没有这个 RDP 会话（可能已被关闭）")?;
        old.shutdown(); // 幂等：断了的会话再关一次无害
        old.profile_id.clone()
    };
    rdp_connect_core(profile_id, Some(frame_channel), state, app).await
}

// ─── 共享目录（RDPDR 批）──────────────────────────────────────────────
// 挂载的前置（确认框 + 只读选择）在前端：这里收到的是**用户已经确认过的**
// 挂载请求。审计行由 ShareTable::mount 落（挂载那一行在一切之前）。

/// 挂载一个共享目录给远端。`device` 是共享槽位号（前端按 1..=4 分配，
/// 同一会话最多四个盘——RDPDR 设备表本身没有上限，限的是用户管理的心智）。
#[tauri::command]
pub async fn rdp_share_mount(
    session_id: String,
    device: u8,
    dir: String,
    readonly: bool,
    app: tauri::AppHandle,
) -> Result<RdpShareStatus, String> {
    let global = app.state::<RdpGlobal>();
    let session = {
        let table = global.0.sessions.lock().unwrap();
        table.get(&session_id).cloned()
    };
    let Some(session) = session else {
        return Err("没有这个 RDP 会话（可能已被关闭）".into());
    };
    // 本地表先立（canonicalize + 审计都在里面），成功后才让 helper 宣告。
    session
        .shares
        .mount(device, &format!("共享{device}"), &dir, readonly)
        .await?;
    session.mount_share(device, &format!("共享{device}"), readonly);
    Ok(RdpShareStatus {
        device,
        mounted: true,
        readonly,
    })
}

/// 卸载。
#[tauri::command]
pub async fn rdp_share_unmount(
    session_id: String,
    device: u8,
    app: tauri::AppHandle,
) -> Result<RdpShareStatus, String> {
    let global = app.state::<RdpGlobal>();
    let session = {
        let table = global.0.sessions.lock().unwrap();
        table.get(&session_id).cloned()
    };
    let Some(session) = session else {
        return Err("没有这个 RDP 会话（可能已被关闭）".into());
    };
    let _ = session.shares.unmount(device).await; // 幂等：卸不存在的盘在 ShareTable 里就是 Ok
    session.unmount_share(device);
    Ok(RdpShareStatus {
        device,
        mounted: false,
        readonly: false,
    })
}

/// 当前挂载状态（画布重挂/前端刷新用）。
#[tauri::command]
pub async fn rdp_share_status(
    session_id: String,
    app: tauri::AppHandle,
) -> Result<Vec<RdpShareStatus>, String> {
    let global = app.state::<RdpGlobal>();
    let session = {
        let table = global.0.sessions.lock().unwrap();
        table.get(&session_id).cloned()
    };
    let Some(session) = session else {
        return Ok(Vec::new());
    };
    // is_mounted/readonly_of 是 ShareTable 的读面；这里逐槽位拼状态。
    let mut out = Vec::new();
    for device in 1..=4u8 {
        if session.shares.is_mounted(device) {
            out.push(RdpShareStatus {
                device,
                mounted: true,
                readonly: session.shares.readonly_of(device).unwrap_or(false),
            });
        }
    }
    Ok(out)
}

#[cfg(test)]
mod share_tests {
    /// 本文件的生产段（口径同 services_cmd.rs）。
    fn production_src() -> &'static str {
        const RAW: &str = include_str!("rdp_cmd.rs");
        let cut = RAW
            .find(
                "
#[cfg(test)]",
            )
            .expect("本文件必须有 #[cfg(test)] 段");
        &RAW[..cut]
    }

    /// 挂载必须先过 `ShareTable::mount`（本地表 + canonicalize + 审计 + 设备号查重
    /// 全在那条路上），成功后才让 helper 宣告。D6 变异（直接 mount_share）首版
    /// 幸存：行为测试全绿，而本地表没立——远端文件操作会全部落在「未知设备」
    /// 上被拒，且挂载本身一行审计都没有。
    #[test]
    fn mount_goes_through_the_share_table_before_announcing() {
        let src = production_src();
        let table_at = src
            .find(
                ".shares
        .mount(",
            )
            .expect("挂载没走 ShareTable::mount");
        let announce_at = src.find("session.mount_share(").expect("没有宣告");
        assert!(
            table_at < announce_at,
            "必须先 ShareTable::mount（闸与审计）再 mount_share（宣告）——反了等于先斩后奏"
        );
    }

    /// 卸载同理：本地表先撤（句柄全关 + 审计），再让 helper 收回宣告。
    #[test]
    fn unmount_tears_down_the_table_too() {
        let src = production_src();
        assert!(
            src.contains(".shares.unmount(device)"),
            "卸载没走 ShareTable（句柄与审计没人收）"
        );
    }
}

#[derive(Debug, serde::Serialize)]
pub struct RdpShareStatus {
    pub device: u8,
    pub mounted: bool,
    pub readonly: bool,
}

/// 本机剪贴板 → 远端的通告（阶段 2，CLIPRDR）。
///
/// **只做这一个方向**：反方向（远端 → 本机）是事件驱动的——远端一复制就
/// 通告，主程序收到即取并写进系统剪贴板，不需要前端参与。
///
/// 触发点是画布获得焦点：那是「用户开始在远端干活」的时刻，也是 mstsc 的
/// 对齐时机。
///
/// # 为什么本机内容由**后端**读（2026-08-27 真机改）
///
/// 原实现让前端 `navigator.clipboard.readText()` 读出来再传进来，理由是
/// 「尊重 webview 的授权模型」。真机上那条路的代价是：**WebView2 每次都弹
/// 一个「此页面想读取剪贴板」的授权框**——用户在一个远程桌面里看到浏览器
/// 的权限弹窗，既出戏又挡屏幕，而且每次画布获得焦点都来一遍。
///
/// 那条理由在桌面应用里本来也不成立：本进程**已经**在用 arboard 读写系统
/// 剪贴板（反方向就是这么落地的），任何本机程序都能读剪贴板，绕一圈 webview
/// 授权并不减少任何暴露面，只是多一个弹窗。
#[tauri::command]
pub async fn rdp_clipboard_sync(session_id: String, app: tauri::AppHandle) -> Result<(), String> {
    let global = app.state::<RdpGlobal>();
    let table = global.0.sessions.lock().unwrap();
    let s = table
        .get(&session_id)
        .ok_or("没有这个 RDP 会话（可能已关闭）")?;
    // 读失败（剪贴板为空、被别的进程独占、非文本内容）不是错误：本方向
    // 静默跳过即可，反方向不受影响。
    if let Ok(text) = crate::rdp::read_system_clipboard() {
        if !text.is_empty() {
            s.clipboard_offer(text);
        }
    }
    // **不在这里拉远端**：远端一复制，helper 就会通告，主程序侧的会话循环
    // 收到通告即取并写进系统剪贴板（见 rdp.rs 的 ClipboardOffer 分支）。
    // 这里再拉一次只会重复一次往返，且拿到的必然是同一份内容。
    Ok(())
}

/// 前端画完一帧的回执（帧级背压）。
///
/// # 为什么帧流要有回执
///
/// 产帧速度由**远端**定（看视频时每秒几十帧全屏），画帧速度由**本机
/// webview** 定。没有回执，两者之间就是一条无界队列：前端画不过来时帧在
/// 事件桥里无限堆积，主线程被 JSON/base64 解析占满——用户实测的现象是
/// 「动态画面一多就整个界面卡住，连关闭连接都点不动」（2026-08-27）。
///
/// 回执把在途帧钉在 2 帧以内。积压期间**不丢内容**：helper 的 DirtyRects
/// 把这段时间的更新合并成一块包围盒后再发——像素的语义就是后写覆盖前写。
///
/// 会话已关时静默成功：这是一条尽力而为的回执，让前端在会话收尾期
/// 多喊一声也不至于弹错误框。
#[tauri::command]
pub async fn rdp_frame_ack(session_id: String, app: tauri::AppHandle) -> Result<(), String> {
    let global = app.state::<RdpGlobal>();
    let table = global.0.sessions.lock().unwrap();
    if let Some(s) = table.get(&session_id) {
        s.frame_ack();
    }
    Ok(())
}

/// 请求整屏重绘（前端自愈用）。
///
/// 帧是**增量**的：漏画一块，那块就永久错到远端自己重绘为止。前端在两种
/// 情况下必须喊这一嗓子——画帧抛异常之后（回执照发，但像素丢了）、
/// 画布上下文重建之后（新画布是空白的）。
#[tauri::command]
pub async fn rdp_request_full_frame(
    session_id: String,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let global = app.state::<RdpGlobal>();
    let table = global.0.sessions.lock().unwrap();
    if let Some(s) = table.get(&session_id) {
        s.request_full_frame();
    }
    Ok(())
}

/// 动态分辨率（阶段 2）：前端画布可用尺寸变化时上报，helper 经
/// DISPLAYCONTROL 通道请求远端改分辨率。
///
/// **无回执**：MS-RDPEDISP 的 MonitorLayout 是单向通知，服务器接受与否只体现在
/// 它随后是否送来新尺寸的画面（Deactivate-All → 重新激活）。这里返回 Ok 只表示
/// 「请求已发出」，不表示「远端已改」——把它写成 Result<bool> 会诱使前端
/// 拿它当「改成功了」用。
#[tauri::command]
pub async fn rdp_resize(
    session_id: String,
    width: u16,
    height: u16,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let global = app.state::<RdpGlobal>();
    let table = global.0.sessions.lock().unwrap();
    match table.get(&session_id) {
        Some(s) => {
            s.resize(width, height);
            Ok(())
        }
        None => Err("没有这个 RDP 会话（可能已关闭）".into()),
    }
}

#[cfg(test)]
mod tests {
    /// 只取**实现部分**（`#[cfg(test)]` 之前）：整份文件读进来的话，
    /// 守卫自己那句字符串字面量会命中自己——判据于是永远为红，
    /// 且第一反应会是「实现坏了」，而不是「判据写错了」。
    fn impl_src() -> &'static str {
        const SRC: &str = include_str!("rdp_cmd.rs");
        SRC.split("#[cfg(test)]").next().expect("split 至少一段")
    }

    /// **Vault 不得是用 RDP 的前提**（2026-08-28 用户反馈）。
    ///
    /// 原实现在没绑 vault_record 时直接 `ok_or(...)?` 报错，于是「想连 RDP
    /// 就得先建保险库」——而 SSH 从来不是这样（没绑凭据会弹框问口令，
    /// 用户可勾「记住」再存）。Vault 的定位是**用户想用才用的应用锁**，
    /// 不是准入关卡。
    ///
    /// 这条守卫钉两件事：① 那句强制报错不能回来；② 询问链路必须是**复用
    /// SSH 那条**（同一个 `auth:prompt` 事件、同一个 `auth_respond` 应答、
    /// 同一个前端对话框）——仿造一套迟早在超时/取消语义上走样。
    #[test]
    fn rdp_does_not_require_a_vault_record_to_connect() {
        assert!(
            !impl_src().contains("该连接未绑定 Vault 口令记录"),
            "RDP 又开始强制要求 Vault 记录了——那让保险库成为用 RDP 的前提，\
             而 SSH 没有这个要求（见本文件口令来源段的记述）"
        );
        assert!(
            impl_src().contains("password_prompt()"),
            "没绑凭据时必须复用 SSH 的口令询问链路（SessionEvents::password_prompt），\
             不是自己造一套"
        );
    }

    /// vault 锁着但档案绑了记录时，询问的提示语必须**点明是锁着**（2026-08-31，
    /// 与 SSH 同口径）。固定那句「该连接未配置口令」在那个场景是错的——配置了，
    /// 只是锁着取不到，用户会以为自己存的凭据丢了。真机场景：保险库默认关闭后，
    /// 这是从「绑了凭据的老档案」到弹框的主路。
    #[test]
    fn rdp_prompt_distinguishes_locked_vault_from_unconfigured() {
        let src = impl_src();
        assert!(
            src.contains("password_prompt_reason("),
            "RDP 的口令询问必须区分「保险库锁着」与「未配置」两种缘由——             检查 rdp_connect 的 vault_locked 分支是否被移除"
        );
    }

    /// 用户取消（空应答）**不得当成空口令去撞服务器**。
    ///
    /// RDP 每次认证失败都计入 Windows 的账户锁定策略（默认 5 次锁 30 分钟）：
    /// 把「用户按了取消」翻译成「用空口令试一次」，等于替用户消耗锁定额度。
    #[test]
    fn cancelled_prompt_does_not_attempt_an_empty_password() {
        // 锚取「询问之后的收口」而不是某一句具体调用（2026-08-31 起询问分两种
        // 缘由，锚在调用形态上会让守卫跟着实现改写而假红）——锚在 answer 产生处。
        let block = impl_src()
            .split("let answer = ")
            .nth(1)
            .expect("找不到口令询问处——本守卫与实现分家了");
        let head: String = block.chars().take(600).collect();
        assert!(
            head.contains("is_empty()") && head.contains("return Ok("),
            "空应答（取消/超时）必须直接返回而不是继续连接：\
             RDP 每次失败都计入账户锁定策略"
        );
    }
}
