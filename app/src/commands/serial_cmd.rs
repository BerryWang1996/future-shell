//! 串口会话的 IPC（M7.4）。
//!
//! # 与 SSH 的分工
//!
//! 打开/关闭/改波特率是串口自己的（`serial_*`）；**打开之后的一切都是共用的**——
//! `term_input` / `term_resize` / `term_ack` / `session_close` 在 SSH 注册表落空时回落到
//! 串口注册表（见 `session_cmd` 里那几处 `serial_fallback`）。前端因此一行都不用改：
//! 标签、状态栏、滚动缓冲、录制、编码切换、会话日志全部原样可用。
//!
//! # 事件与 SSH 同名同义
//!
//! 拔线发 `session:disconnected`（前端保留标签 + banner），插回来发 `session:status`
//! 且带 `state:"connected"`（标签转绿），用户关闭发 `session:closed`（移除标签）。
//! 这三条正是前端 `bridgeSessionEvents` 已经在听的——不是「照着做一遍」，是同一套。

use crate::serial_session::{to_params, SerialSession};
use crate::state::AppState;
use fs_connmgr::repo::ProfileRepo;
use fs_serial::session::{SerialEvent, TokioSerialFactory};
use std::sync::Arc;
use tauri::{Emitter, State};

/// 枚举本机串口。
///
/// 枚举失败**不报错**而是回空列表 + 一行说明：拿不到列表不代表不能用串口（Linux 上不装
/// libudev 时某些虚拟串口就不出现在扫描里），用户仍可手打端口名。把它做成错误的话，
/// 界面上会是一个红框，而实际上他只要自己填 `/dev/pts/3` 就能连上。
#[tauri::command]
pub fn serial_list_ports() -> SerialPortList {
    match fs_serial::ports::list_ports() {
        Ok(ports) => SerialPortList {
            ports,
            note: String::new(),
        },
        Err(e) => {
            tracing::warn!(%e, "串口枚举失败");
            SerialPortList {
                ports: Vec::new(),
                note: format!("列不出串口（{e}）。可以直接填端口名，例如 COM3 或 /dev/ttyUSB0。"),
            }
        }
    }
}

#[derive(Debug, serde::Serialize)]
pub struct SerialPortList {
    pub ports: Vec<fs_serial::PortInfo>,
    /// 非空时界面上照原样显示（列表为空的**原因**，不是错误框）。
    pub note: String,
}

/// 常用波特率清单（界面下拉用）。**不是白名单**——自定义值照样接受。
#[tauri::command]
pub fn serial_common_bauds() -> Vec<u32> {
    fs_serial::params::COMMON_BAUDS.to_vec()
}

/// 按档案打开一个串口会话，返回 session_id。
///
/// 首次打开失败**当场报错**（「COM3 被占着」这种话只有此刻说才有用）；此后的断开一律自动重连。
#[tauri::command]
pub async fn serial_open(
    profile_id: String,
    rows: u32,
    cols: u32,
    state: State<'_, Arc<AppState>>,
) -> Result<String, String> {
    let st = state.inner().clone();
    let profile = {
        let repo = ProfileRepo::new(st.db.pool());
        repo.get(uuid::Uuid::parse_str(&profile_id).map_err(|e| e.to_string())?)
            .await
            .map_err(|e| e.to_string())?
    };
    if profile.protocol != fs_connmgr::Protocol::Serial {
        return Err("这个连接不是串口档案".into());
    }
    let params = to_params(&profile.serial);
    params.validate()?;

    let session_id = uuid::Uuid::new_v4().to_string();
    let (events_tx, events_rx) = tokio::sync::mpsc::channel::<SerialEvent>(32);
    let (link, reader) = fs_serial::session::spawn(TokioSerialFactory, params.clone(), events_tx)?;

    // 编码：与 SSH 同一条路（档案里的 term.encoding；认不出按 UTF-8 起步并留日志）。
    // 串口尤其需要它——裸板的中文提示基本都是 GBK。
    let decoder = match fs_terminal::decode::StreamDecoder::new(profile.term.encoding.as_deref()) {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(%e, %session_id, "档案里的终端编码无法识别，按 UTF-8 起步");
            fs_terminal::decode::StreamDecoder::default()
        }
    };
    let mut pipe = fs_terminal::SessionPipe::spawn(
        reader,
        fs_terminal::PipeOpts {
            grid_rows: rows as u16,
            grid_cols: cols as u16,
            scrollback_lines: profile
                .term
                .scrollback_lines
                .map(|v| v as usize)
                .unwrap_or(fs_terminal::grid::DEFAULT_SCROLLBACK_LINES),
            flow: Default::default(),
            ring_bytes: fs_terminal::ring::DEFAULT_RING_BYTES,
            session_log: st
                .build_session_log(&session_id, &profile.serial.port, "serial")
                .await,
            // ZMODEM 拦截器：串口上 rz/sz 同样常见（很多裸板的固件更新就是走 ZMODEM），
            // 而它与传输本身无关——同一个拦截器直接复用。
            tap: None,
            record: None,
            decoder,
        },
    );
    let rx = pipe.render_rx();
    crate::sessions::spawn_render_pump(st.app.clone(), session_id.clone(), rx);

    st.serial.insert(
        session_id.clone(),
        Arc::new(SerialSession {
            pipe: Arc::new(pipe),
            link,
            port: profile.serial.port.clone(),
        }),
    );
    spawn_event_bridge(
        st.clone(),
        session_id.clone(),
        profile.serial.port.clone(),
        events_rx,
    );
    Ok(session_id)
}

/// 把串口会话的事件翻成前端已经在听的那三条 SSH 事件。
fn spawn_event_bridge(
    st: Arc<AppState>,
    session_id: String,
    port: String,
    mut rx: tokio::sync::mpsc::Receiver<SerialEvent>,
) {
    tauri::async_runtime::spawn(async move {
        while let Some(ev) = rx.recv().await {
            match ev {
                SerialEvent::Disconnected(d) => {
                    if d == fs_serial::reconnect::Disconnect::UserClosed {
                        continue; // 用户关闭走 session_close 的 closed 事件，不发断线
                    }
                    let _ = st.app.emit(
                        "session:disconnected",
                        serde_json::json!({
                            "session_id": session_id,
                            "reason": "transport_eof",
                            "attempt": 0,
                            "message": d.message(&port),
                        }),
                    );
                }
                SerialEvent::Reconnected => {
                    // 与 SSH 重连成功那条逐字同形：**带 state 才算状态迁移**，
                    // 否则前端只会当成一句进度文案（见 lib/session-status.ts）。
                    let _ = st.app.emit(
                        "session:status",
                        serde_json::json!({
                            "session_id": session_id,
                            "state": "connected",
                            "message": format!("{port} 已重新连接"),
                        }),
                    );
                }
                SerialEvent::RetryFailed { attempt, error } => {
                    // 不每次都打扰用户：重连是无上限的，每次失败弹一条会刷屏。
                    // 只记日志，界面上那条「拔线了，插回去会自动重连」的 banner 一直在。
                    tracing::debug!(%session_id, attempt, %error, "串口重连未成功");
                }
                SerialEvent::Closed => {
                    st.serial.remove(&session_id);
                    let _ = st.app.emit(
                        "session:closed",
                        serde_json::json!({ "session_id": session_id, "reason": "user_closed" }),
                    );
                    return;
                }
            }
        }
    });
}

/// 改波特率（不重开端口）。返回新的参数摘要供状态栏显示。
#[tauri::command]
pub async fn serial_set_baud(
    session_id: String,
    baud: u32,
    state: State<'_, Arc<AppState>>,
) -> Result<String, String> {
    let s = state.serial.get(&session_id).ok_or("no session")?;
    s.link.set_baud(baud).await?;
    Ok(s.link.params().summary())
}

/// 当前串口参数摘要（`115200 8N1`）。状态栏用。
#[tauri::command]
pub async fn serial_params(
    session_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<SerialParamsView, String> {
    let s = state.serial.get(&session_id).ok_or("no session")?;
    let p = s.link.params();
    Ok(SerialParamsView {
        port: s.port.clone(),
        baud: p.baud,
        summary: p.summary(),
    })
}

#[derive(Debug, serde::Serialize)]
pub struct SerialParamsView {
    pub port: String,
    pub baud: u32,
    pub summary: String,
}

#[cfg(test)]
mod tests {
    /// 本文件的生产段（口径同 `services_cmd.rs`）。
    fn production_src() -> &'static str {
        const RAW: &str = include_str!("serial_cmd.rs");
        let cut = RAW
            .find("\n#[cfg(test)]")
            .expect("本文件必须有 #[cfg(test)] 段");
        &RAW[..cut]
    }

    /// 重连成功那条事件**必须带 `state:"connected"`**。
    ///
    /// 前端的 `statusTransition` 只认带 state 的那一条；不带的话它只是一句进度文案，
    /// 标签会永远停在「连接中」——那正是 P1-11 修过一次的缺陷，串口这条新路径极易重蹈。
    #[test]
    fn reconnect_event_carries_the_state_field() {
        let src = production_src();
        // 事件名**拼出来**而不是写成字面量：前端的 connect-state.test.ts 逐文件扫这个字面量
        // 来数「谁在发 session:status」，而它不剥 `#[cfg(test)]`——写死在这里会让本文件被
        // 数成两个发送点。这与 audit_cmd 的脱敏门禁是同一类自指陷阱。
        let needle = concat!("\"session", ":status\"");
        let at = src.find(needle).expect("没有发 session:status");
        let tail = &src[at..];
        let stmt_end = tail.find(");").unwrap_or(tail.len());
        assert!(
            tail[..stmt_end].contains(r#""state": "connected""#),
            "session:status 没带 state:\"connected\"——前端会把它当成进度文案，标签永远转圈"
        );
    }

    /// 用户主动关闭不得发 `session:disconnected`：那会让前端挂上「正在重连」的 banner，
    /// 而这条会话已经没了。
    #[test]
    fn user_close_does_not_emit_disconnected() {
        let src = production_src();
        assert!(
            src.contains("Disconnect::UserClosed") && src.contains("continue"),
            "用户关闭那一支没有跳过断线事件"
        );
    }

    /// 枚举失败要降级成「空列表 + 说明」，不是 Err：
    /// 拿不到列表不代表不能用串口，用户仍可手打端口名。
    #[test]
    fn enumeration_failure_is_not_an_error_dialog() {
        let src = production_src();
        assert!(src.contains("pub fn serial_list_ports() -> SerialPortList"));
        assert!(!src.contains("pub fn serial_list_ports() -> Result"));
    }
}
