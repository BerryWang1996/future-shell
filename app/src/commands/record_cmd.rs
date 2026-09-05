//! 会话录屏命令（M4a：asciicast v2 录制与回放）。
//!
//! 录制器在**建会话时**就常驻挂载（`PipeOpts.record`，idle 零开销）——手动启停只翻
//! 内部 active 标志，不必在运行中的管道上动结构。文件落在数据目录 recordings/ 下。
//!
//! **隐私口径与 sessionlog 同款**：录屏含输出明文（含命令与回显，颜色转义原样），
//! UI 的启动确认里必须说明。读取（`recording_read`）只接受 recordings 目录内的
//! 路径——入参来自前端，等于给了「读任意文件并解析回显」的能力，闸一次前缀比较
//! 的成本，挡住拿回放器当任意文件读取器。

use crate::state::AppState;
use std::sync::{Arc, Mutex};
use tauri::State;

/// 录屏文件的统一扩展名。
const CAST_EXT: &str = "cast";

/// 每会话的录制器表（建会话时插入 idle 实例，随会话拆除一并移除）。
pub type RecorderMap =
    Arc<Mutex<std::collections::HashMap<String, Arc<Mutex<fs_terminal::record::Recorder>>>>>;

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// recordings 目录（惰性创建）。
fn recordings_dir(state: &AppState) -> std::path::PathBuf {
    recordings_dir_of(state)
}

/// 同上，供**打开目录**那条入口复用（`opener_cmd::reveal_recordings_dir`）。
///
/// 单源化不是洁癖：会话日志那边就是「写的地方与打开的地方各拼一份」走散过，
/// 结果是入口打开一个空目录、文件躺在别处，而且不报错。录屏这边一开始就收成
/// 一个函数，两处引同一份。
pub(crate) fn recordings_dir_of(state: &AppState) -> std::path::PathBuf {
    let dir = state.data_dir.join("recordings");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// 开始录制。返回文件路径（UI 提示用户录到哪了）。
#[tauri::command]
pub async fn recording_start(
    session_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<String, String> {
    let recorder = state
        .recorders
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&session_id)
        .cloned()
        .ok_or("会话不存在或已关闭")?;
    let (cols, rows) = {
        let Some(session) = state.registry.get(&session_id) else {
            return Err("会话不存在或已关闭".into());
        };
        (session.pipe.grid_cols(), session.pipe.grid_rows())
    };
    // 已在录则拒绝：静默换文件会丢掉前一段（旧文件句柄被覆盖、不再收事件，
    // 用户以为还在录第一段）
    {
        let mut r = recorder.lock().unwrap_or_else(|p| p.into_inner());
        if r.is_active() {
            return Err("该会话已在录制中".into());
        }
        let dir = recordings_dir(state.inner());
        let host = state
            .registry
            .get(&session_id)
            .map(|s| {
                s.target_endpoint
                    .rsplit_once(':')
                    .map(|(h, _)| h)
                    .unwrap_or("session")
                    .to_string()
            })
            .unwrap_or_else(|| "session".into());
        // 文件名时间戳用 UTC（utc_parts 与 sessionlog 同源；文件名排序即时间序，
        // 差几小时不影响这一点）
        let p = crate::state::utc_parts(std::time::SystemTime::now());
        let stamp = format!(
            "{:04}{:02}{:02}-{:02}{:02}{:02}",
            p.0, p.1, p.2, p.3, p.4, p.5
        );
        let host = fs_terminal::sessionlog::sanitize_file_name(&host);
        let path = dir.join(format!("{host}-{stamp}.{CAST_EXT}"));
        r.start_into(path.clone(), cols, rows, now_ms())
            .map_err(|e| format!("开录失败：{e}"))?;
        let shown = path.to_string_lossy().to_string();
        tracing::info!(%session_id, path = %shown, "会话录屏开始");
        Ok(shown)
    }
}

/// 停止录制（未在录则如实报错，不装成功）。
#[tauri::command]
pub async fn recording_stop(
    session_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    let recorder = state
        .recorders
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&session_id)
        .cloned()
        .ok_or("会话不存在或已关闭")?;
    let mut r = recorder.lock().unwrap_or_else(|p| p.into_inner());
    if !r.is_active() {
        return Err("该会话未在录制".into());
    }
    r.stop(now_ms());
    tracing::info!(%session_id, "会话录屏结束");
    Ok(())
}

/// 当前录制状态（标签页上有没有红点、菜单项显示哪一项）。
#[derive(serde::Serialize)]
pub struct RecordingStatus {
    pub recording: bool,
    /// 未录制时为空
    pub path: String,
    /// 已写字节（录制中的大小指示）
    pub bytes: u64,
}

#[tauri::command]
pub async fn recording_status(
    session_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<RecordingStatus, String> {
    let recorder = state
        .recorders
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&session_id)
        .cloned();
    let status = match recorder {
        None => RecordingStatus {
            recording: false,
            path: String::new(),
            bytes: 0,
        },
        Some(r) => {
            let r = r.lock().unwrap_or_else(|p| p.into_inner());
            RecordingStatus {
                recording: r.is_active(),
                path: if r.is_active() {
                    r.path().to_string_lossy().to_string()
                } else {
                    String::new()
                },
                bytes: r.written_bytes(),
            }
        }
    };
    Ok(status)
}

/// 列出 recordings 目录（回放面板的数据源）。
#[derive(serde::Serialize)]
pub struct CastFile {
    pub name: String,
    /// 完整路径（回放时原样传回）
    pub path: String,
    pub bytes: u64,
    /// 修改时刻（unix 秒）
    pub modified: i64,
}

#[tauri::command]
pub async fn recordings_list(state: State<'_, Arc<AppState>>) -> Result<Vec<CastFile>, String> {
    let dir = recordings_dir(state.inner());
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some(CAST_EXT) {
            continue;
        }
        let meta = entry.metadata().map_err(|e| e.to_string())?;
        out.push(CastFile {
            name: path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("?")
                .to_string(),
            path: path.to_string_lossy().to_string(),
            bytes: meta.len(),
            modified: meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0),
        });
    }
    // 新的在前
    out.sort_by_key(|f| std::cmp::Reverse(f.modified));
    Ok(out)
}

/// 读事件列表（头 + 事件一起给；回放面板一次拿全）。**只接受 recordings 目录内的路径。**
#[derive(serde::Serialize)]
pub struct CastContent {
    pub header: fs_terminal::record::CastHeader,
    pub events: Vec<fs_terminal::record::CastEvent>,
}

#[tauri::command]
pub async fn recording_read_events(
    path: String,
    state: State<'_, Arc<AppState>>,
) -> Result<CastContent, String> {
    let (header, events) = read_cast_checked(state.inner(), &path)?;
    Ok(CastContent { header, events })
}

/// 路径闸 + 解析。错误带出「为什么打不开/哪里坏了」。
fn read_cast_checked(
    state: &AppState,
    path: &str,
) -> Result<
    (
        fs_terminal::record::CastHeader,
        Vec<fs_terminal::record::CastEvent>,
    ),
    String,
> {
    let dir = recordings_dir(state);
    let target = std::path::Path::new(path);
    // canonicalize 吸收 ..、符号链接与等价写法；目录本身也 canonicalize，两端一致才可比
    let dir_c = std::fs::canonicalize(&dir).map_err(|e| format!("recordings 目录不可用：{e}"))?;
    let target_c = std::fs::canonicalize(target).map_err(|_| format!("录屏文件不存在：{path}"))?;
    if !target_c.starts_with(&dir_c) {
        return Err("只允许读取 recordings 目录内的录屏文件".into());
    }
    let content = std::fs::read_to_string(&target_c).map_err(|e| format!("读取失败：{e}"))?;
    fs_terminal::record::parse_cast(&content)
}
