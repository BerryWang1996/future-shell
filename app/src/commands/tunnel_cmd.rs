//! SSH 隧道命令（M4a 隧道管理器）。
//!
//! 隧道**不持久化**：它是会话级设施，会话没了隧道也没了（tunnel.rs 模块头③）。
//! 想要「每次连上都自动建同样的隧道」属于档案级配置，是另一件事——不做即不假装
//! 做了，UI 上写明「隧道随会话关闭而停止」。

use crate::state::AppState;
use std::sync::Arc;
use tauri::State;

/// 生产实现的 direct-tcpip opener：从注册表按 session_id 取连接句柄开通道。
///
/// 每次 open 都重新查注册表而不是缓存句柄：会话可能已被关闭或**重连过**
/// （重连换了 russh Handle，缓存的旧句柄开出来的通道通向一条已死的连接）。
/// 查表的代价是一次哈希 + 一次锁，与开通道的网络往返相比可忽略。
struct RegistryOpener {
    state: Arc<AppState>,
    session_id: String,
}

impl fs_sshengine::tunnel::ChannelOpener for RegistryOpener {
    fn open(
        &self,
        host: String,
        port: u16,
        origin: std::net::SocketAddr,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<fs_sshengine::tunnel::TunnelStream, String>>
                + Send
                + '_,
        >,
    > {
        Box::pin(async move {
            let session = self
                .state
                .registry
                .get(&self.session_id)
                .ok_or("会话已关闭，隧道连接被拒绝")?;
            let handle = session.handle.lock().await;
            let channel = handle
                .channel_open_direct_tcpip(
                    host,
                    port as u32,
                    origin.ip().to_string(),
                    origin.port() as u32,
                )
                .await
                .map_err(|e| format!("开 direct-tcpip 通道失败：{e}"))?;
            // 锁在开通道之后即释放：对拷可能持续很久，握着会话锁不放会把
            // term_input/SFTP 一起堵死
            drop(handle);
            Ok(Box::new(channel.into_stream()) as fs_sshengine::tunnel::TunnelStream)
        })
    }
}

/// 启动一条本地端口转发（`-L local_port:remote_host:remote_port`）。
#[tauri::command]
pub async fn tunnel_start(
    session_id: String,
    spec: fs_sshengine::tunnel::TunnelSpec,
    state: State<'_, Arc<AppState>>,
) -> Result<TunnelInfo, String> {
    let app_state = state.inner().clone();
    // 会话必须在：对已关闭会话建隧道只会造出一个 accept 后立刻失败的端口
    if app_state.registry.get(&session_id).is_none() {
        return Err("会话不存在或已关闭".into());
    }
    // 同一会话内 id 唯一：重复 id 会让「停止」指向错误的隧道
    {
        let tunnels = app_state.tunnels.lock().await;
        if tunnels
            .get(&session_id)
            .is_some_and(|v| v.iter().any(|t| t.spec.id == spec.id))
        {
            return Err(format!("隧道 id 已存在：{}", spec.id));
        }
    }
    let opener = Arc::new(RegistryOpener {
        state: app_state.clone(),
        session_id: session_id.clone(),
    });
    let running = fs_sshengine::tunnel::start_local_forward(opener, spec).await?;
    let info = TunnelInfo::from(&running);
    app_state
        .tunnels
        .lock()
        .await
        .entry(session_id)
        .or_default()
        .push(running);
    Ok(info)
}

/// 停止一条隧道（按 id）。不存在即 Err——静默成功会让 UI 以为停掉了。
#[tauri::command]
pub async fn tunnel_stop(
    session_id: String,
    tunnel_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    let mut tunnels = state.tunnels.lock().await;
    let list = tunnels
        .get_mut(&session_id)
        .ok_or("该会话没有运行中的隧道")?;
    let idx = list
        .iter()
        .position(|t| t.spec.id == tunnel_id)
        .ok_or_else(|| format!("找不到隧道：{tunnel_id}"))?;
    let t = list.remove(idx);
    t.stop();
    Ok(())
}

/// 列出某会话的隧道（UI 列表数据源）。
#[tauri::command]
pub async fn tunnel_list(
    session_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<TunnelInfo>, String> {
    Ok(state
        .tunnels
        .lock()
        .await
        .get(&session_id)
        .map(|v| v.iter().map(TunnelInfo::from).collect())
        .unwrap_or_default())
}

/// 前端可见的隧道状态（`RunningTunnel` 本体含句柄，不能序列化给前端）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct TunnelInfo {
    pub id: String,
    pub local_port: u16,
    pub local_addr: String,
    pub remote_host: String,
    pub remote_port: u16,
    pub bind_all: bool,
    /// 累计已转发的连接数（UI 显示「已转发 N 条」，也是「隧道到底有没有被用过」的判据）
    pub accepted: u64,
}

impl From<&fs_sshengine::tunnel::RunningTunnel> for TunnelInfo {
    fn from(t: &fs_sshengine::tunnel::RunningTunnel) -> Self {
        Self {
            id: t.spec.id.clone(),
            local_port: t.spec.local_port,
            local_addr: t.local_addr.to_string(),
            remote_host: t.spec.remote_host.clone(),
            remote_port: t.spec.remote_port,
            bind_all: t.spec.bind_all,
            accepted: t.accepted_count(),
        }
    }
}
