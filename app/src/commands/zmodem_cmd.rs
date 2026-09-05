//! ZMODEM 终端内传输命令（M4a：rz/sz）。
//!
//! 桥接与状态机在 [`crate::zmodem_bridge`]；这里只是 IPC 门面。
//!
//! **下载不需要指令**：终端里跑 `sz file` 时拦截器自己认出起始帧、自己落盘，
//! 前端只收进度事件。需要指令的只有上传——`rz` 起来之后要由人选一个本地文件。

use crate::state::AppState;
use std::sync::Arc;
use tauri::State;

/// 当前传输状态（前端进度条据此决定是否显示、能否点取消）。
#[derive(serde::Serialize)]
pub struct ZmodemStatus {
    /// 是否有传输在进行
    pub active: bool,
    /// 落盘根目录（让用户知道下载去哪了，也是「打开所在目录」的落点）
    pub sandbox: Option<String>,
    /// 自动检测是否开启
    pub auto: bool,
}

#[tauri::command]
pub async fn zmodem_status(
    session_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<ZmodemStatus, String> {
    let tap = state
        .zmodem
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&session_id)
        .cloned();
    Ok(match tap {
        // 会话没有拦截器（未开启或建会话时未装配）：如实报「不可用」，
        // 而不是报 active=false —— 前者该隐藏入口，后者该显示可用入口。
        None => ZmodemStatus {
            active: false,
            sandbox: None,
            auto: false,
        },
        Some(t) => ZmodemStatus {
            active: t.active(),
            sandbox: Some(t.sandbox_display()),
            auto: t.auto_enabled(),
        },
    })
}

/// 上传本地文件（对端须已在跑 `rz`）。多选：一次入队、同一会话逐个发。
#[tauri::command]
pub async fn zmodem_send(
    session_id: String,
    local_paths: Vec<String>,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    let tap = state
        .zmodem
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&session_id)
        .cloned()
        .ok_or("该会话未启用终端内传输")?;
    // 上传源过 SFTP 上传的同一把浏览根闸（sftp_cmd::ensure_within）。不另写一份：
    // 上传越界规则有两份实现时，分叉的那一侧就是能读走任意文件的那一侧。
    // 逐个解析、第一个越界的点名报错——一个都不开始传（见 begin_send 的注释）。
    let mut paths = Vec::with_capacity(local_paths.len());
    for raw in &local_paths {
        paths.push(crate::commands::sftp_cmd::resolve_upload_source(raw)?);
    }
    tap.begin_send(&paths)
}

/// 在系统文件管理器里定位刚收到的文件。
///
/// 只接受**本会话沙箱内**的路径。看似多余（路径本来就是后端自己发给前端的），
/// 但这条命令的入参来自前端，等于给了「让程序去 reveal 任意路径」的能力——
/// reveal 会打开资源管理器并选中目标，用它探测任意路径是否存在是可行的。
/// 闸在这里，成本一次字符串前缀比较。
#[tauri::command]
pub async fn zmodem_reveal(
    session_id: String,
    path: String,
    app: tauri::AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    let tap = state
        .zmodem
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&session_id)
        .cloned()
        .ok_or("该会话未启用终端内传输")?;
    let target = tap.resolve_received(&path)?;
    app.opener()
        .reveal_item_in_dir(target)
        .map_err(|e| e.to_string())
}

/// 归置一次收完的下载（两段式的第二段）。
///
/// 对端 `sz` 的协议超时是几十秒级，不能等用户选完目录再应答——所以协议侧先收进
/// spool（`.incoming/recv-*.part`），收完发 `needs-decision` 事件，由前端弹框，
/// 用户的选择经本命令落定。
///
/// - `on_conflict`：overwrite（覆盖）/ keep-both（保留两者，同名改 `(n)`）/ skip
///   （不要了，删 spool）。
/// - `remember`：`Some(true)` = 把 `dir` 写成固定下载目录；`Some(false)` = 以后每次
///   都问；`None` = 不动设置。
#[tauri::command]
pub async fn zmodem_finalize(
    session_id: String,
    dir: String,
    name: Option<String>,
    on_conflict: String,
    remember: Option<bool>,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    let app = state.app.clone();
    let tap = state
        .zmodem
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&session_id)
        .cloned()
        .ok_or("该会话未启用终端内传输")?;
    match tap.finalize_from_ipc(&dir, name.as_deref(), &on_conflict)? {
        Some((fname, path, bytes)) if on_conflict != "skip" => {
            if let Some(fixed) = remember {
                write_download_mode(&state, fixed.then_some(dir.clone())).await?;
            }
            use tauri::Emitter as _;
            let _ = app.emit(
                "zmodem:progress",
                serde_json::json!({
                    "sessionId": session_id,
                    "phase": "saved",
                    "name": fname,
                    "path": path.display().to_string(),
                    "done": bytes,
                    "total": Some(bytes),
                    "message": "",
                    "file_index": null,
                    "file_count": null,
                    "suggestedDir": null,
                    "spoolPath": null,
                    "conflict": null,
                }),
            );
            Ok(())
        }
        // skip：spool 已删、没有落地文件——不再发 saved（前端自己收尾提示）。
        Some(_) => {
            if let Some(fixed) = remember {
                write_download_mode(&state, fixed.then_some(dir.clone())).await?;
            }
            Ok(())
        }
        // 没有待归置的下载（前端重复确认）：幂等成功，不是错误。
        None => Ok(()),
    }
}

/// 写 `term.zmodem.download`。`Some(dir)` = fixed 模式，`None` = ask 模式。
///
/// 读-改-写而不是只写 download 一键：设置 JSON 是整份序列化的，只写一个键
/// 会把 `enabled`/`autoReceive` 抹掉。
async fn write_download_mode(
    state: &Arc<AppState>,
    fixed_dir: Option<String>,
) -> Result<(), String> {
    let repo = fs_connmgr::SettingsRepo::new(state.db.pool());
    let mut cfg = state.zmodem_config().await;
    cfg.download = fixed_dir.map(|dir| crate::state::ZmodemDownloadConfig {
        mode: crate::state::ZmodemDownloadMode::Fixed,
        dir,
    });
    if cfg.download.is_none() {
        cfg.download = Some(crate::state::ZmodemDownloadConfig {
            mode: crate::state::ZmodemDownloadMode::Ask,
            dir: String::new(),
        });
    }
    let raw = serde_json::to_string(&cfg).map_err(|e| e.to_string())?;
    repo.set("term.zmodem", &raw)
        .await
        .map_err(|e| e.to_string())
}

/// 取消当前传输（发 8×CAN 让对端也退出传输态）。
#[tauri::command]
pub async fn zmodem_cancel(
    session_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    let tap = state
        .zmodem
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&session_id)
        .cloned()
        .ok_or("该会话未启用终端内传输")?;
    tap.cancel()
}
