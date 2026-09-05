use crate::state::AppState;
use std::collections::HashMap;
use std::sync::Arc;
use tauri::State;

/// 每会话的上一次 `/proc/stat` 采样（CPU% 差量计算的输入）。
///
/// CPU% 的语义是「这一轮到上一轮之间忙了百分之几」，必须跨调用记忆——单次采集
/// 里 /proc/stat 只有开机以来的累计值，没有前值就没有差量。存 app 层而不是
/// 前端：前端刷新/重挂会丢一轮（丢了也只是 CPU% 空一轮，无害），而**会话重连**
/// 后前值属于旧机器时刻（机器重启过则 jiffies 归零），算出的差量是垃圾——
/// 因此 `clear` 于会话关闭（registry 移除）时一并清掉，重连从空态重新起步。
///
/// 键 = session_id（不含 generation）：同一 id 的重连沿用旧前值一轮的窗口里，
/// `cpu_percent_between` 的钳位保证至多出一个失真一轮的百分比，下一轮自愈；
/// 为一轮失真把键复杂化成 (id, generation) 不值。
pub type CpuPrevMap = HashMap<String, fs_sshengine::monitor::CpuTimes>;

/// 系统监控快照（M4a）：对活动会话跑一次只读采集命令（见 `fs_sshengine::monitor`），
/// 解析成快照回给前端周期轮询。命令经 `exec_adapter_for`（已套 `TimedExec` 总超时，审计2 #11）。
#[tauri::command]
pub async fn session_monitor(
    session_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<fs_sshengine::monitor::MonitorSnapshot, String> {
    let exec = state.exec_adapter_for(&session_id).await?;
    let o = exec
        .exec_once(fs_sshengine::monitor::monitor_command())
        .await
        .map_err(|e| e.to_string())?;
    let (code, stdout, stderr) = (o.code, o.stdout, o.stderr);
    // 命令整体失败（exit 非零/未观测到退出码，且一点 stdout 都没有）：把 stderr 带出去，
    // 别伪装成空快照。单项缺失由解析器落空态，不在这里拦。
    if code != Some(0) && stdout.trim().is_empty() {
        return Err(format!(
            "采集命令执行失败（exit {}）：{}",
            code.map(|c| c.to_string())
                .unwrap_or_else(|| "未观测到".into()),
            stderr.trim()
        ));
    }
    let mut snap = fs_sshengine::monitor::parse_monitor_output(&stdout);
    // CPU% 差量（S310）：有前值就算、无论成败都更新前值——失败的轮次如果保留旧前值，
    // 下一轮的差量窗口会横跨故障期，得出一个毫无意义又看似可信的数。
    if let Some(cur) = snap.cpu_times {
        let mut prev = state.cpu_prev.lock().unwrap_or_else(|p| p.into_inner());
        let pct = prev
            .get(&session_id)
            .and_then(|p| fs_sshengine::monitor::cpu_percent_between(p, &cur));
        snap.cpu_percent = pct;
        prev.insert(session_id.clone(), cur);
    } else {
        // 无 /proc（macOS/BSD）：清前值，避免换机器/容器后用旧值差量。
        state
            .cpu_prev
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&session_id);
        // 远端若直接给了百分比（macOS 的 `top -l 2` 那一路）就用它。
        // **优先级不能反过来**：`/proc/stat` 的差量更准，且不像 `top -l 2` 要等一秒；
        // 有 cpu_times 的机器上永远走上面那一支。
        snap.cpu_percent = snap.cpu_percent_direct;
    }
    Ok(snap)
}

/// 远端进程列表（M4a 服务器进程管理）。
///
/// 与 `session_monitor` 同一条通道与同一套超时（`exec_adapter_for` 已套 `TimedExec`）。
/// 空结果**必须**与失败区分开：把执行失败显示成空进程表，用户读到的是「这台机器上
/// 没有进程」——一个永不可能为真的结论，却看起来像正常结果（见 `procs::classify_empty`）。
#[tauri::command]
pub async fn session_processes(
    session_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<fs_sshengine::procs::ProcessInfo>, String> {
    let exec = state.exec_adapter_for(&session_id).await?;
    let o = exec
        .exec_once(fs_sshengine::procs::process_list_command())
        .await
        .map_err(|e| e.to_string())?;
    if let Some(reason) = fs_sshengine::procs::classify_empty(o.code, &o.stdout, &o.stderr) {
        return Err(reason);
    }
    Ok(fs_sshengine::procs::parse_process_list(&o.stdout))
}

/// 终止远端进程（M4a）。
///
/// `pid` 是 `u32`、`signal` 过闭合白名单映射到编译期字面量——命令串里没有用户可控的
/// 自由文本（见 `procs::kill_command`）。确认对话框在前端；后端只拦「无法表达合理
/// 意图」的输入（PID 0 = 整个进程组）。
///
/// 成功判据是**退出码**而非 stdout：`kill` 成功时一个字都不打印，只看 stdout 会把
/// 「没权限」也当成成功。
#[tauri::command]
pub async fn session_kill_process(
    session_id: String,
    pid: u32,
    signal: String,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    let cmd = fs_sshengine::procs::kill_command(pid, &signal).map_err(|e| e.to_string())?;
    let exec = state.exec_adapter_for(&session_id).await?;
    let o = exec.exec_once(&cmd).await.map_err(|e| e.to_string())?;
    if o.code != Some(0) {
        // 把远端原话带出来：「Operation not permitted」与「No such process」对用户
        // 是两件完全不同的事，统一成「终止失败」等于把唯一的线索抹掉。
        let msg = o.stderr.trim();
        return Err(if msg.is_empty() {
            format!(
                "终止 PID {pid} 失败（exit {}），远端无错误输出",
                o.code
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "未观测到".into())
            )
        } else {
            format!("终止 PID {pid} 失败：{msg}")
        });
    }
    tracing::info!(%session_id, pid, signal = %signal, "已向远端进程发送信号");
    Ok(())
}

/// 主机可达性探测（M4a 主机状态灯轮询）：对 `host:port` 做一次 TCP 连接。
///
/// **只测 TCP 握手，不测 SSH 协议**：状态灯问的是「这台机器活着吗」，握手成功即绿——
/// Xshell 同款语义。不读 banner（读要等对端数据，慢且常被 fail2ban 拖）；拒绝（RST）
/// 与超时都算「不可达」——sshd 停了的机器对用户来说就是「红灯」。
///
/// 连接本身丢到 `spawn_blocking`：同步 `TcpStream::connect_timeout` 在异步上下文里
/// 会占住 worker 线程最长一个超时周期，轮询的是**全部未连接档案**（可能几十台），
/// 并发占用不可接受。
#[tauri::command]
pub async fn host_probe(host: String, port: u16) -> Result<bool, String> {
    // host 来自 profile 表；空值在此响亮失败——connect 对畸形地址的 Err 与
    // 「不可达」不可分辨，坏档案应该去修档案而不是亮红灯
    if host.trim().is_empty() {
        return Err("host 为空".to_string());
    }
    // 太长会把一轮轮询拖过间隔；太短在跨区网络上抖成圣诞灯
    let timeout = std::time::Duration::from_millis(1500);
    let ok = tauri::async_runtime::spawn_blocking(move || probe_tcp(&host, port, timeout))
        .await
        .map_err(|e| e.to_string())?;
    Ok(ok)
}

fn probe_tcp(host: &str, port: u16, timeout: std::time::Duration) -> bool {
    use std::net::ToSocketAddrs;
    // DNS 解析失败 = 不可达（域名过期/DNS 挂了，对状态灯语义成立）
    (host, port)
        .to_socket_addrs()
        .ok()
        .and_then(|mut it| it.next())
        .map(|addr| std::net::TcpStream::connect_timeout(&addr, timeout).is_ok())
        .unwrap_or(false)
}

/// 连接详情（M4a：状态栏状态灯点击弹层；UI 规格 §2.7）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct ConnectionDetail {
    pub host: String,
    pub port: u16,
    pub username: String,
    /// 最终成功的认证方法（`publickey` / `password` / …）；未知为空串
    pub auth_method: String,
    /// 主机密钥类型与 SHA256 指纹（OpenSSH 口径）
    pub key_type: String,
    pub fingerprint_sha256: String,
    /// 往返延时（毫秒）；测量失败为 null——**不填 0**：0 ms 会被读成「极快」，
    /// 而真相是「没测到」。
    pub latency_ms: Option<u64>,
}

/// 取活动会话的连接详情。
///
/// 延时用一次**空 exec**（`:` 内建命令，不产生输出、不改远端状态）测往返：
/// 它走的是真实的 SSH 加密通道与服务端调度，比 TCP ping 更贴近「用起来卡不卡」。
/// 代价是包含一次开通道的开销，故文案上写「往返」而不是「网络延时」——
/// 不把一个包含服务端处理的数字说成纯网络指标。
#[tauri::command]
pub async fn session_connection_detail(
    session_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<ConnectionDetail, String> {
    let session = state.registry.get(&session_id).ok_or("no session")?;
    let profile = {
        let repo = fs_connmgr::ProfileRepo::new(state.db.pool());
        repo.get(uuid::Uuid::parse_str(&session.profile_id).map_err(|e| e.to_string())?)
            .await
            .map_err(|e| e.to_string())?
    };
    let facts = fs_sshengine::connect::observed_facts(&profile.host, profile.port);
    // 延时测量失败不让整条命令失败：详情弹层的其余字段仍有价值（用户点开多半
    // 是为了核对指纹），为一个测不到的数字整屏报错是本末倒置。
    let latency_ms = match state.exec_adapter_for(&session_id).await {
        Ok(exec) => {
            let t0 = std::time::Instant::now();
            match exec.exec_once(":").await {
                Ok(_) => Some(t0.elapsed().as_millis() as u64),
                Err(e) => {
                    tracing::debug!(%e, "连接详情：延时测量失败");
                    None
                }
            }
        }
        Err(e) => {
            tracing::debug!(%e, "连接详情：取 exec 通道失败");
            None
        }
    };
    Ok(ConnectionDetail {
        host: profile.host,
        port: profile.port,
        username: profile.username,
        auth_method: facts.auth_method,
        key_type: facts.key_type,
        fingerprint_sha256: facts.fingerprint_sha256,
        latency_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-08-22 盘点补：`monitor_cmd` 此前**零测试**——`host_probe` 的空 host 判断、
    /// `probe_tcp` 的 DNS 失败与不可达路径全部裸奔，而状态灯的 itest 用的是自己重写的
    /// 一份 probe（`crates/itest/tests/monitor.rs`），生产函数一行都没被跑过。
    ///
    /// 这里补的是**能离线断言的那部分**：判空、DNS 解析失败、连接被拒。
    /// 真实 sshd 的停机/恢复仍由 itest 覆盖（那需要容器）。

    #[test]
    fn a_blank_host_fails_loudly_instead_of_showing_a_red_light() {
        // 空 host 来自坏档案：connect 对畸形地址的 Err 与「主机不可达」不可分辨，
        // 亮红灯会让用户去查一台好机器，而真正该做的是修档案。
        //
        // **必须真调 `host_probe`。** 这条曾经在测试体里现场重写一份判空逻辑
        //（`if bad.trim().is_empty() { Err } else { Ok }`）再断言自己的返回值——
        // 那是一条恒真断言：把生产函数里的判空整段删掉，它照样绿。交叉审计逮到的。
        for bad in ["", "   ", "\t"] {
            let err = tauri::async_runtime::block_on(host_probe(bad.to_string(), 22));
            assert!(err.is_err(), "空 host {bad:?} 必须响亮失败而不是判成不可达");
            // 错因要说清是「空」，否则与「连不上」在 UI 上又混成一件事
            assert!(
                err.unwrap_err().contains("空"),
                "错因要点明 host 为空，用户才知道去修档案而不是查网络"
            );
        }
        // 反向对照：非空 host 不会被判空分支拦下（它会往下走到真探测）。
        // 只有上面那半的话，把 host_probe 写成恒 Err 也能全绿。
        let ok = tauri::async_runtime::block_on(host_probe(
            "127.0.0.1".to_string(),
            1, // 几乎不会有人监听的端口：走到真探测并返回 Ok(false)
        ));
        assert!(ok.is_ok(), "非空 host 不该被判空分支拦下，实得 {ok:?}");
    }

    #[test]
    fn an_unresolvable_name_counts_as_unreachable() {
        // DNS 解析失败 = 不可达（域名过期/DNS 挂了，对状态灯语义成立），不是 panic 也不是「可达」。
        //
        // **不用 `.invalid` 顶级域做判据**：RFC 2606 只保证它不该被注册，不保证本机 DNS 会
        // 回 NXDOMAIN。本机实测就把 `no-such-host.invalid` 劫持解析到 198.18.0.191 且该地址
        // 22 端口有人应答——于是这条断言在这台机器上必然假红（第一版就栽在这里）。
        // 改用一个**语法上就不可能是主机名**的输入：空字节与非法字符必然让 to_socket_addrs 失败，
        // 与 DNS 配置无关，判据因此在任何机器上都成立。
        let t = std::time::Duration::from_millis(200);
        for bad in ["host with spaces\0", "..", ":::::"] {
            assert!(
                !probe_tcp(bad, 22, t),
                "{bad:?} 解析必然失败，必须判不可达而不是 panic 或报可达"
            );
        }
    }

    #[test]
    fn a_closed_port_on_localhost_counts_as_unreachable() {
        // 端口 9（discard）在本机通常无人监听：连接被拒必须判不可达，而不是被当成成功。
        // 这一条与上一条合起来盖住 probe_tcp 的两条失败路径（解析失败 / 握手失败）。
        let t = std::time::Duration::from_millis(300);
        assert!(
            !probe_tcp("127.0.0.1", 9, t),
            "本机 9 端口无人监听时必须判不可达"
        );
    }

    #[test]
    fn a_listening_port_counts_as_reachable() {
        // 反向对照：防「恒返回 false」的假绿。现开一个监听口，probe 必须报可达。
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind 失败");
        let port = listener.local_addr().unwrap().port();
        let t = std::time::Duration::from_millis(500);
        assert!(
            probe_tcp("127.0.0.1", port, t),
            "有人监听时必须判可达（否则 probe 恒 false 也能骗过前两条）"
        );
    }
}
