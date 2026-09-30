//! 跳板链（ProxyJump）增删后**连接真的走了新路径**的容器集成测试
//! （M4a 出口标准：「两跳链增删后连接经新路径建立」）。
//!
//! 判据取自**服务端**：目标机会话里的 `$SSH_CONNECTION` 第一个字段是服务端看到的
//! 对端 IP。直连时它是 Docker 网关；经跳板时它是**那台跳板容器**的 bridge IP——
//! 因为 direct-tcpip 的 TCP 连接由跳板机自己发起。
//!
//! 为什么不能用我们自己的 `events.status()` 播报当判据：那串「跳板 2/2：经上一跳
//! 转发至 …」是**打算**走哪条路，由被测代码自己写出来。若 `connect()` 把 hop 列表
//! 算错、或把最后一跳的转发漏掉而直连目标，播报照旧完美，测试照旧全绿。
//! 只有服务端看到的对端 IP 才与实现的意图无关。
//!
//! 需 `FS_ITEST=1` + 可用 Docker（三个容器）。

use fs_connmgr::{AuthRef, Db, HostKeyPolicy, JumpHop, Profile};
use fs_itest::sshd::{SshdContainer, INTERNAL_SSH_PORT};
use fs_sshengine::events::{HostKeyChoice, Prompt, SessionEvents};
use fs_sshengine::hostkey::{Decision, PresentedKey};
use fs_sshengine::secrets::SecretSource;
use russh::ChannelMsg;
use std::sync::{Arc, Mutex};
use zeroize::Zeroizing;

#[derive(Default)]
struct AcceptKeys {
    statuses: Mutex<Vec<String>>,
}
impl SessionEvents for AcceptKeys {
    fn host_key_decision(
        &self,
        _host: &str,
        _port: u16,
        _p: &PresentedKey,
        _d: &Decision,
    ) -> HostKeyChoice {
        HostKeyChoice::AcceptOnce
    }
    fn kbd_interactive(&self, _n: &str, _i: &str, prompts: &[Prompt]) -> Vec<String> {
        prompts.iter().map(|_| String::new()).collect()
    }
    fn password_prompt(&self) -> String {
        String::new()
    }
    fn status(&self, msg: &str) {
        self.statuses.lock().unwrap().push(msg.to_string());
    }
}

/// 各跳与目标的口令可以不同，凭据源按 vault 记录号分发（跳板链的凭据是**逐跳**的）。
struct PerRecord(std::collections::HashMap<u64, String>);
impl SecretSource for PerRecord {
    fn secret(&self, rec: u64) -> Result<fs_sshengine::secrets::Secret, fs_sshengine::Error> {
        let pw = self
            .0
            .get(&rec)
            .ok_or_else(|| fs_sshengine::Error::Connect(format!("测试凭据源没有记录 {rec}")))?;
        Ok(fs_sshengine::secrets::Secret {
            kind: fs_sshengine::secrets::SecretKind::Password,
            bytes: Zeroizing::new(pw.clone().into_bytes()),
        })
    }
}

/// 一跳的地址**取决于它在链上的位置**，这不是测试的随意选择而是拓扑事实：
/// 第 1 跳由**宿主**发起 TCP，必须用宿主映射地址（Docker Desktop for Windows/macOS 下
/// 172.17/16 在宿主根本不可路由——实测 os error 10054「远程主机强迫关闭连接」）；
/// 第 2 跳起由**上一跳容器**发起，必须用容器 bridge IP + 容器内部端口（宿主映射端口
/// 在容器网络命名空间里不存在）。
fn hop_at(c: &SshdContainer, host: String, port: u16, rec: u64) -> JumpHop {
    JumpHop {
        host,
        port,
        username: c.username.clone(),
        auth: AuthRef {
            vault_record: Some(rec),
            ..Default::default()
        },
        host_key_policy: None, // 继承 profile 的 Tofu
        host_key_pins: vec![],
    }
}

/// 链首跳：从宿主直连，用宿主映射地址。
async fn first_hop(c: &SshdContainer, rec: u64) -> JumpHop {
    let a = c.addr().await;
    hop_at(c, a.ip().to_string(), a.port(), rec)
}

/// 链中/末跳：从上一跳容器内发起，用容器 bridge IP + 内部端口。
async fn inner_hop(c: &SshdContainer, rec: u64) -> JumpHop {
    let ip = c.container.get_bridge_ip_address().await.unwrap();
    hop_at(c, ip.to_string(), INTERNAL_SSH_PORT, rec)
}

/// 出口标准：同一个 profile 上把跳板链**从 0 改到 1、再改到 2、再删回 1**，
/// 每次连接后由目标机自己报出「这条 TCP 是谁连上来的」，四次判据两两不同且逐一对上。
#[tokio::test(flavor = "multi_thread")]
async fn editing_the_jump_chain_changes_the_actual_path() {
    if std::env::var("FS_ITEST").is_err() {
        eprintln!("skip: set FS_ITEST=1");
        return;
    }
    // 三台机器：两台跳板 + 一台目标。跳板需开 TCP 转发（镜像默认禁）。
    let b1 = SshdContainer::start("jpath-b1").await.unwrap();
    let b2 = SshdContainer::start("jpath-b2").await.unwrap();
    let tgt = SshdContainer::start("jpath-tgt").await.unwrap();
    b1.enable_tcp_forwarding().await.unwrap();
    b2.enable_tcp_forwarding().await.unwrap();

    let b1_ip = b1.container.get_bridge_ip_address().await.unwrap();
    let b2_ip = b2.container.get_bridge_ip_address().await.unwrap();
    let tgt_ip = tgt.container.get_bridge_ip_address().await.unwrap();
    let tgt_addr = tgt.addr().await; // 直连用宿主映射端口

    // 自检：三台在同一 bridge 网段上互通，否则后面的失败会指向错误的原因
    b1.run(&format!(
        "nc -z -w 5 {tgt_ip} {INTERNAL_SSH_PORT} && echo reachable"
    ))
    .await
    .unwrap_or_else(|e| panic!("跳板 1 到目标不可达（bridge 网络前置不成立）：{e}"));

    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("jump-path.db")).await.unwrap();
    let events = Arc::new(AcceptKeys::default());
    let secrets = PerRecord(
        [
            (1u64, tgt.password.clone()),
            (2, b1.password.clone()),
            (3, b2.password.clone()),
        ]
        .into_iter()
        .collect(),
    );

    let base = Profile {
        id: uuid::Uuid::nil(),
        name: "jump-path".into(),
        group_path: None,
        host: tgt_addr.ip().to_string(),
        port: tgt_addr.port(),
        username: tgt.username.clone(),
        protocol: fs_connmgr::Protocol::Ssh,
        auth: AuthRef {
            vault_record: Some(1),
            ..Default::default()
        },
        jump: vec![],
        host_key_policy: HostKeyPolicy::Tofu,
        host_key_pins: vec![],
        env: Default::default(),
        term: Default::default(),
        sftp: Default::default(),
        ai_policy: Default::default(),
        serial: Default::default(),
    };

    // ── ① 零跳：直连。对端是 Docker 网关，肯定不是任何一台跳板容器
    let direct = peer_ip_seen_by_target(&base, &secrets, &db, events.clone()).await;
    assert_ne!(
        direct, b1_ip,
        "零跳时目标看到的对端不该是跳板 1（说明前置状态就串了）"
    );
    assert_ne!(direct, b2_ip, "零跳时目标看到的对端不该是跳板 2");

    // ── ② 加一跳（b1）：目标必须看到 b1 的 IP，且 host/port 改为容器内地址
    //     （经跳板时 TCP 由跳板机在容器网络里发起，宿主映射端口在那里不存在）
    let mut one = base.clone();
    one.host = tgt_ip.to_string();
    one.port = INTERNAL_SSH_PORT;
    one.jump = vec![first_hop(&b1, 2).await];
    let via1 = peer_ip_seen_by_target(&one, &secrets, &db, events.clone()).await;
    assert_eq!(
        via1, b1_ip,
        "加一跳后目标看到的对端必须是跳板 1 的 bridge IP（实得 {via1}；\
         若等于零跳时的 {direct}，说明跳板被绕过、连接仍是直连）"
    );

    // ── ③ 再加一跳（b1 → b2 → 目标）：目标看到的必须变成**末跳** b2
    let mut two = one.clone();
    two.jump = vec![first_hop(&b1, 2).await, inner_hop(&b2, 3).await];
    let (via2, live) = peer_ip_and_live_handle(&two, &secrets, &db, events.clone()).await;
    assert_eq!(
        via2, b2_ip,
        "两跳链上目标看到的对端必须是**末跳** b2（实得 {via2}；等于 {b1_ip} 则说明第二跳没建、\
         链被截断在 b1）"
    );
    // 两跳链的中间跳也必须真的被用上：b1 上应能看到一条到 b2 的出向连接。
    //
    // 必须在**连接还活着时**看。断开之后再看是竞态：若 b2 那头先关，b1 这一侧走
    // CLOSE_WAIT → LAST_ACK 转眼就消失、不留 TIME_WAIT，grep 数到 0——1.0.0 发布试跑的
    // ubuntu 关卡就是这样红的，而同一份代码在 PR 的 CI 上是绿的。
    let b1_seen = b1
        .run(&format!(
            "(netstat -tn 2>/dev/null || ss -tn) | grep -c '{b2_ip}:{INTERNAL_SSH_PORT}' || true"
        ))
        .await
        .unwrap();
    drop(live);
    assert_ne!(
        b1_seen.trim(),
        "0",
        "两跳链跑完后跳板 1 上看不到任何到跳板 2 的连接——第二跳可能是客户端直连建的"
    );

    // ── ④ 删掉末跳，改回一跳：路径必须**退回** b1（增删对称，不是只加不减）
    let mut back = two.clone();
    back.jump.pop();
    let via_back = peer_ip_seen_by_target(&back, &secrets, &db, events.clone()).await;
    assert_eq!(
        via_back, b1_ip,
        "删掉末跳后路径必须退回跳板 1（实得 {via_back}；仍等于 {b2_ip} 则说明删除没生效）"
    );

    // 逐跳播报存在（诊断用，不作为路径判据——它由被测代码自己写出）
    let st = events.statuses.lock().unwrap().clone();
    assert!(
        st.iter().any(|s| s.contains("跳板 2/2")),
        "两跳链应播报到 2/2，实得 {st:?}"
    );
}

/// 跳板链**顺序**改变后路径随之改变：同一组两跳换个先后，末跳换人，目标看到的对端就换人。
///
/// 顺序是 UI 上「↑/↓ 移动」直接产出的编辑动作。若 `connect()` 拿 hop 列表时做了任何
/// 集合化处理（排序、去重、按 host 查表），前一个用例的「增/删」判据仍会全绿，
/// 而顺序错乱是安全相关的：链上第一个节点才是最先拿到你流量的那台机器。
#[tokio::test(flavor = "multi_thread")]
async fn reordering_hops_swaps_the_last_hop_seen_by_target() {
    if std::env::var("FS_ITEST").is_err() {
        eprintln!("skip: set FS_ITEST=1");
        return;
    }
    let b1 = SshdContainer::start("jord-b1").await.unwrap();
    let b2 = SshdContainer::start("jord-b2").await.unwrap();
    let tgt = SshdContainer::start("jord-tgt").await.unwrap();
    b1.enable_tcp_forwarding().await.unwrap();
    b2.enable_tcp_forwarding().await.unwrap();

    let b1_ip = b1.container.get_bridge_ip_address().await.unwrap();
    let b2_ip = b2.container.get_bridge_ip_address().await.unwrap();
    let tgt_ip = tgt.container.get_bridge_ip_address().await.unwrap();

    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("jump-order.db")).await.unwrap();
    let events = Arc::new(AcceptKeys::default());
    let secrets = PerRecord(
        [
            (1u64, tgt.password.clone()),
            (2, b1.password.clone()),
            (3, b2.password.clone()),
        ]
        .into_iter()
        .collect(),
    );
    let mut p = Profile {
        id: uuid::Uuid::nil(),
        name: "jump-order".into(),
        group_path: None,
        host: tgt_ip.to_string(),
        port: INTERNAL_SSH_PORT,
        username: tgt.username.clone(),
        protocol: fs_connmgr::Protocol::Ssh,
        auth: AuthRef {
            vault_record: Some(1),
            ..Default::default()
        },
        jump: vec![first_hop(&b1, 2).await, inner_hop(&b2, 3).await],
        host_key_policy: HostKeyPolicy::Tofu,
        host_key_pins: vec![],
        env: Default::default(),
        term: Default::default(),
        sftp: Default::default(),
        ai_policy: Default::default(),
        serial: Default::default(),
    };
    let a = peer_ip_seen_by_target(&p, &secrets, &db, events.clone()).await;
    assert_eq!(a, b2_ip, "b1→b2 链的末跳是 b2");

    // UI 的「↑/↓ 移动」：换序后 b2 成为首跳、b1 成为末跳。地址形态随位置一起换
    // （首跳从宿主发起、末跳从上一跳容器内发起），这是上面 first_hop/inner_hop 注释里的同一条拓扑事实。
    p.jump = vec![first_hop(&b2, 3).await, inner_hop(&b1, 2).await];
    let b = peer_ip_seen_by_target(&p, &secrets, &db, events.clone()).await;
    assert_eq!(
        b, b1_ip,
        "换序为 b2→b1 后末跳应变成 b1（实得 {b}；仍是 {b2_ip} 说明 hop 顺序未被尊重）"
    );
}

/// 用被测的 `connect()` 建连，然后在目标机上问它自己看到的对端 IP。
///
/// `$SSH_CONNECTION` 由 sshd 按这条 TCP 的实际对端填写（第一个字段 = 客户端 IP），
/// 与客户端的意图无关，故可作路径判据。
async fn peer_ip_seen_by_target(
    p: &Profile,
    secrets: &PerRecord,
    db: &Db,
    events: Arc<AcceptKeys>,
) -> std::net::IpAddr {
    peer_ip_and_live_handle(p, secrets, db, events).await.0
}

/// 同上，但把**仍然活着的**会话句柄一并交出：需要在「连接尚在」的窗口里观察服务端状态
/// （如跳板机上的出向连接）时用这个版本。
async fn peer_ip_and_live_handle(
    p: &Profile,
    secrets: &PerRecord,
    db: &Db,
    events: Arc<AcceptKeys>,
) -> (
    std::net::IpAddr,
    russh::client::Handle<fs_sshengine::connect::Connector>,
) {
    let handle = fs_sshengine::connect::connect(p, secrets, db.pool(), events.clone())
        .await
        .unwrap_or_else(|e| {
            panic!(
                "连接失败（jump={:?}）：{e}；statuses={:?}",
                p.jump.iter().map(|h| &h.host).collect::<Vec<_>>(),
                events.statuses.lock().unwrap()
            )
        });
    let mut ch = handle.channel_open_session().await.unwrap();
    ch.exec(false, "printf '%s' \"$SSH_CONNECTION\"")
        .await
        .unwrap();

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    let mut out = Vec::new();
    let mut status = None;
    loop {
        match tokio::time::timeout_at(deadline, ch.wait()).await {
            Ok(Some(ChannelMsg::Data { data })) => out.extend_from_slice(&data),
            // 退出状态可能先于剩余输出到达（sshd 在子进程退出时即发），记下后继续收，
            // 直到通道关闭。在这里 break 过，CI 上偶发拿到空输出（2026-09-30）。
            Ok(Some(ChannelMsg::ExitStatus { exit_status })) => status = Some(exit_status),
            Ok(Some(ChannelMsg::Close)) => break,
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(_) => panic!(
                "读 $SSH_CONNECTION 超时；已收到 {:?}",
                String::from_utf8_lossy(&out)
            ),
        }
    }
    let text = String::from_utf8_lossy(&out).trim().to_string();
    assert_eq!(
        status,
        Some(0),
        "取 $SSH_CONNECTION 的命令未成功；输出 {text:?}"
    );
    // 形如 "172.17.0.3 41234 172.17.0.5 2222"
    let first = text
        .split_whitespace()
        .next()
        .unwrap_or_else(|| panic!("$SSH_CONNECTION 为空——sshd 未设置该变量？实得 {text:?}"));
    let ip = first
        .parse()
        .unwrap_or_else(|e| panic!("$SSH_CONNECTION 首字段不是 IP（{first:?}）：{e}"));
    (ip, handle)
}
