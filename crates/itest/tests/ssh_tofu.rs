use fs_connmgr::{Db, HostKeyPolicy};
use fs_itest::sshd::SshdContainer;
use fs_sshengine::connect::Connector;
use fs_sshengine::events::{HostKeyChoice, Prompt, SessionEvents};
use fs_sshengine::hostkey::{Decision, PresentedKey, StoredKey, TrustStore};
use russh::client;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// 脚本化假事件：answer 决定主机密钥裁决；asked/statuses 留痕供断言
#[derive(Default)]
struct Scripted {
    answer: HostKeyChoice,
    asked: Mutex<Vec<String>>,
    statuses: Mutex<Vec<String>>,
}
impl SessionEvents for Scripted {
    fn host_key_decision(
        &self,
        host: &str,
        _port: u16,
        p: &PresentedKey,
        _h: &Decision,
    ) -> HostKeyChoice {
        self.asked
            .lock()
            .unwrap()
            .push(format!("{host}:{}", p.fingerprint_sha256));
        self.answer
    }
    fn kbd_interactive(&self, _name: &str, _instruction: &str, prompts: &[Prompt]) -> Vec<String> {
        prompts.iter().map(|_| String::new()).collect()
    }
    fn password_prompt(&self) -> String {
        String::new()
    }
    fn status(&self, msg: &str) {
        self.statuses.lock().unwrap().push(msg.to_string());
    }
}

/// 每例独立临时库（TempDir 随测试结束回收）
async fn open_db(tag: &str) -> (tempfile::TempDir, Db) {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join(format!("fs-{tag}.db")))
        .await
        .unwrap();
    (dir, db)
}

/// check_server_key 在 KEX 期触发：握手成败即裁决结果，无需走完认证
async fn handshake(
    addr: std::net::SocketAddr,
    connector: Connector,
) -> Result<client::Handle<Connector>, <Connector as client::Handler>::Error> {
    client::connect(Arc::new(client::Config::default()), addr, connector).await
}

// (a) 首连接受并记录 → 落库 source=tofu；二次握手命中信任库不弹框
#[tokio::test(flavor = "multi_thread")]
async fn tofu_first_contact_accept_and_record_persists() {
    if std::env::var("FS_ITEST").is_err() {
        eprintln!("skip: set FS_ITEST=1");
        return;
    }
    let sshd = SshdContainer::start("tofu1").await.unwrap();
    let addr = sshd.addr().await;
    let host = addr.ip().to_string();
    let (_dir, db) = open_db("tofu1").await;
    let events = Arc::new(Scripted {
        answer: HostKeyChoice::AcceptAndRecord,
        ..Default::default()
    });
    let connector = Connector::new(
        host.clone(),
        addr.port(),
        HostKeyPolicy::Tofu,
        vec![],
        events.clone(),
        db.pool().clone(),
    );

    handshake(addr, connector)
        .await
        .expect("AcceptAndRecord 首连必须握手成功");

    let trust = TrustStore::new(db.pool());
    let rows = trust.lookup(&host, addr.port()).await.unwrap();
    assert_eq!(rows.len(), 1, "AcceptAndRecord 必须落库一条");
    assert_eq!(rows[0].source, "tofu", "TOFU 落库 source 必须为 tofu");
    assert_eq!(events.asked.lock().unwrap().len(), 1, "首连应询问一次");

    // 换新 Scripted 二次握手：命中信任库 → 不弹框直接放行
    let events2 = Arc::new(Scripted {
        answer: HostKeyChoice::AcceptAndRecord,
        ..Default::default()
    });
    let connector2 = Connector::new(
        host.clone(),
        addr.port(),
        HostKeyPolicy::Tofu,
        vec![],
        events2.clone(),
        db.pool().clone(),
    );
    handshake(addr, connector2)
        .await
        .expect("已记录密钥必须命中信任库直接放行");
    assert!(
        events2.asked.lock().unwrap().is_empty(),
        "命中信任库不得再次弹框"
    );
}

// (b) 仅本次接受 → 放行但不落库；断开重连对该主机重走 TOFU 再次询问
#[tokio::test(flavor = "multi_thread")]
async fn tofu_accept_once_stays_in_memory_and_reprompts() {
    if std::env::var("FS_ITEST").is_err() {
        eprintln!("skip: set FS_ITEST=1");
        return;
    }
    let sshd = SshdContainer::start("tofu2").await.unwrap();
    let addr = sshd.addr().await;
    let host = addr.ip().to_string();
    let (_dir, db) = open_db("tofu2").await;
    let events = Arc::new(Scripted {
        answer: HostKeyChoice::AcceptOnce,
        ..Default::default()
    });
    let connector = Connector::new(
        host.clone(),
        addr.port(),
        HostKeyPolicy::Tofu,
        vec![],
        events.clone(),
        db.pool().clone(),
    );

    handshake(addr, connector)
        .await
        .expect("AcceptOnce 必须放行本次连接");
    let trust = TrustStore::new(db.pool());
    assert!(
        trust.lookup(&host, addr.port()).await.unwrap().is_empty(),
        "AcceptOnce 不得写 host_keys"
    );

    // 断开后新建连接：无持久信任 → 重走 TOFU 再次询问
    let events2 = Arc::new(Scripted {
        answer: HostKeyChoice::AcceptOnce,
        ..Default::default()
    });
    let connector2 = Connector::new(
        host.clone(),
        addr.port(),
        HostKeyPolicy::Tofu,
        vec![],
        events2.clone(),
        db.pool().clone(),
    );
    handshake(addr, connector2)
        .await
        .expect("重连经 TOFU 再次询问后必须放行");
    assert_eq!(
        events2.asked.lock().unwrap().len(),
        1,
        "仅本次接受后重连必须重新询问"
    );
    assert!(
        trust.lookup(&host, addr.port()).await.unwrap().is_empty(),
        "重连 AcceptOnce 后仍不得落库"
    );
}

// (c) strict 拒绝未知密钥：握手失败、不弹框、不落库（即便脚本答案是接受）
#[tokio::test(flavor = "multi_thread")]
async fn strict_refuses_unknown_without_prompt() {
    if std::env::var("FS_ITEST").is_err() {
        eprintln!("skip: set FS_ITEST=1");
        return;
    }
    let sshd = SshdContainer::start("tofu3").await.unwrap();
    let addr = sshd.addr().await;
    let host = addr.ip().to_string();
    let (_dir, db) = open_db("tofu3").await;
    let events = Arc::new(Scripted {
        answer: HostKeyChoice::AcceptAndRecord,
        ..Default::default()
    });
    let connector = Connector::new(
        host.clone(),
        addr.port(),
        HostKeyPolicy::Strict,
        vec![],
        events.clone(),
        db.pool().clone(),
    );

    // 用 is_err() 而非 expect_err()：后者要求 Ok 侧实现 Debug，而 russh 的
    // client::Handle<H> 未实现 Debug（E0277）。断言语义等价，失败文案照给。
    assert!(
        handshake(addr, connector).await.is_err(),
        "strict 模式必须拒绝未知密钥"
    );
    assert!(
        events.asked.lock().unwrap().is_empty(),
        "strict 拒绝未知密钥不得弹框询问"
    );
    // 实现期核实（计划 Step 5b 注记）：拒绝路径必须在 statuses 里留下可诊断文案，
    // 否则用户只看到一个语焉不详的握手错误，无从知道是 strict 策略挡下的。
    //
    // 判据钉的是**面向用户的两件事实**，不是某个英文单词：拒绝理由要说清「是策略挡的」
    // 与「挡的是未知主机」。原判据写的是 `contains("strict")`，而实现给的文案是中文的
    // 「「严格」策略拒绝未知主机密钥……」——这条测试因此**长期是红的**，只是容器 itest
    // 走 FS_ITEST 门控、日常关卡不跑，没人看见（2026-08-22 盘点时实跑才发现）。
    // 不改回英文：文案是给用户看的，中文是对的那一侧；改的是判据。
    let statuses = events.statuses.lock().unwrap().clone();
    let joined = statuses.join(" | ");
    assert!(
        joined.contains("严格") || joined.contains("strict"),
        "strict 拒绝须经 status 回调点明是**策略**挡下的；实收：{statuses:?}"
    );
    assert!(
        joined.contains("未知主机密钥") || joined.contains("没有这台主机"),
        "拒绝理由须点明挡的是「未知主机」，否则用户不知道该去导入 known_hosts；实收：{statuses:?}"
    );
    assert!(
        TrustStore::new(db.pool())
            .lookup(&host, addr.port())
            .await
            .unwrap()
            .is_empty(),
        "strict 拒绝后不得落库"
    );
}

// (d) 密钥变更：拒绝 → 握手失败且记录仍为 1；显式接受并记录 → 握手成功且替换旧行（记录仍为 1，R13 替换语义）
#[tokio::test(flavor = "multi_thread")]
async fn changed_key_refused_then_explicit_accept_replaces_old_key() {
    if std::env::var("FS_ITEST").is_err() {
        eprintln!("skip: set FS_ITEST=1");
        return;
    }
    let sshd = SshdContainer::start("tofu4").await.unwrap();
    let addr = sshd.addr().await;
    let host = addr.ip().to_string();
    let (_dir, db) = open_db("tofu4").await;
    let trust = TrustStore::new(db.pool());

    // 预置一把与容器宿主密钥不匹配的旧记录 → 构成 Changed 场景
    let k0 = b"K0-not-matching-container-host-key";
    trust
        .record(
            &host,
            addr.port(),
            &StoredKey {
                key_type: "ssh-ed25519".into(),
                key_blob: String::from_utf8_lossy(k0).into_owned(),
                fingerprint_sha256: fs_sshengine::hostkey::fingerprint_sha256(k0),
                source: "tofu".into(),
            },
            "tofu",
        )
        .await
        .unwrap();

    // 用户拒绝变更密钥 → 握手失败、记录仍为 1
    let events = Arc::new(Scripted {
        answer: HostKeyChoice::Refuse,
        ..Default::default()
    });
    let connector = Connector::new(
        host.clone(),
        addr.port(),
        HostKeyPolicy::Tofu,
        vec![],
        events.clone(),
        db.pool().clone(),
    );
    // 同上：Handle<H> 无 Debug，改用 is_err()
    assert!(
        handshake(addr, connector).await.is_err(),
        "拒绝变更密钥必须终止握手"
    );
    assert_eq!(
        events.asked.lock().unwrap().len(),
        1,
        "密钥变更必须弹框询问一次"
    );
    assert_eq!(
        trust.lookup(&host, addr.port()).await.unwrap().len(),
        1,
        "拒绝后不得新增记录"
    );

    // 用户显式接受并记录 → 握手成功、替换旧行（R13：信任库恒每 host:port 至多一行有效密钥）
    let events2 = Arc::new(Scripted {
        answer: HostKeyChoice::AcceptAndRecord,
        ..Default::default()
    });
    let connector2 = Connector::new(
        host.clone(),
        addr.port(),
        HostKeyPolicy::Tofu,
        vec![],
        events2.clone(),
        db.pool().clone(),
    );
    handshake(addr, connector2)
        .await
        .expect("显式接受变更密钥后握手必须成功");
    let rows = trust.lookup(&host, addr.port()).await.unwrap();
    assert_eq!(
        rows.len(),
        1,
        "显式接受后旧键必须退库，只剩一行有效密钥（R13 替换语义）"
    );
    assert_ne!(
        rows[0].key_blob,
        String::from_utf8_lossy(k0).into_owned(),
        "存行须为容器宿主密钥，而非预置的不匹配旧键"
    );
}

/// 路线图 L56「对已导入主机首连不弹 TOFU」之 itest 载体（ssh_tofu.rs 第五例）：
/// 先导入容器宿主密钥入信任库（source=imported），再以 Tofu 策略对同一容器握手——命中导入行直接校验、TOFU 回调零调用
#[tokio::test(flavor = "multi_thread")]
async fn imported_host_first_connect_verifies_without_tofu_prompt() {
    if std::env::var("FS_ITEST").is_err() {
        eprintln!("skip: set FS_ITEST=1");
        return;
    }
    let sshd = SshdContainer::start("tofu5").await.unwrap();
    let addr = sshd.addr().await;
    let host = addr.ip().to_string();
    let (_dir, db) = open_db("tofu5").await;

    // 复制 Task 9 ssh_connect.rs 同名 helper 写法：accept-all 基线会话仅用于读取容器宿主公钥（产品路径禁止 accept-all）
    struct AcceptAllKeys;
    impl russh::client::Handler for AcceptAllKeys {
        type Error = russh::Error;
        async fn check_server_key(
            &mut self,
            _server_key: &russh::keys::PublicKey,
        ) -> Result<bool, Self::Error> {
            Ok(true)
        }
    }
    async fn connect_password(
        addr: std::net::SocketAddr,
        user: &str,
        pass: &str,
    ) -> russh::client::Handle<AcceptAllKeys> {
        let config = client::Config::default();
        let mut session = client::connect(Arc::new(config), addr, AcceptAllKeys)
            .await
            .unwrap();
        let res = session.authenticate_password(user, pass).await.unwrap();
        assert!(res.success(), "password auth failed: {res:?}");
        session
    }

    let session = connect_password(addr, &sshd.username, &sshd.password).await;
    let mut channel = session.channel_open_session().await.unwrap();
    channel
        .exec(false, "cat /etc/ssh/ssh_host_ed25519_key.pub")
        .await
        .unwrap();

    // 同 ssh_connect.rs：每次 wait() 套 timeout_at，exit-status 在循环外断言（S27/S28）
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let mut out = Vec::new();
    let mut status = None;
    loop {
        match tokio::time::timeout_at(deadline, channel.wait()).await {
            Ok(Some(russh::ChannelMsg::Data { data })) => out.extend_from_slice(&data),
            Ok(Some(russh::ChannelMsg::ExitStatus { exit_status })) => {
                status = Some(exit_status);
                break;
            }
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(_) => panic!(
                "timeout reading host key file; got so far: {:?}",
                String::from_utf8_lossy(&out)
            ),
        }
    }
    let pub_line = String::from_utf8_lossy(&out).trim().to_string();
    assert_eq!(status, Some(0), "cat host key failed; output={pub_line}");
    assert!(
        pub_line.starts_with("ssh-ed25519 "),
        "host key file content: {pub_line}"
    );
    let known_hosts = format!("[{host}]:{} {pub_line}", addr.port());

    let ts = TrustStore::new(db.pool());
    let s = ts.import_known_hosts(&known_hosts).await.unwrap();
    assert_eq!(
        s,
        fs_sshengine::hostkey::ImportSummary {
            imported: 1,
            ..Default::default()
        },
        "{s:?}"
    );
    let rows = ts.lookup(&host, addr.port()).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].source, "imported");

    // 新 Scripted 记录器 + Tofu 策略握手同一容器：known_hosts 行按 [host]:port 落库，握手 host/port 同取以命中该行
    // answer 故意置 Refuse——若误弹 TOFU 并被拒则握手必失败；握手成功 + asked 为空双证「TOFU 回调零调用」
    let events = Arc::new(Scripted {
        answer: HostKeyChoice::Refuse,
        ..Default::default()
    });
    let connector = Connector::new(
        host.clone(),
        addr.port(),
        HostKeyPolicy::Tofu,
        vec![],
        events.clone(),
        db.pool().clone(),
    );
    handshake(addr, connector)
        .await
        .expect("已导入主机首连必须命中导入行直接校验放行");
    assert!(
        events.asked.lock().unwrap().is_empty(),
        "已导入主机首连不得弹 TOFU（TOFU 回调零调用）"
    );
    let rows2 = ts.lookup(&host, addr.port()).await.unwrap();
    assert_eq!(rows2.len(), 1, "直接校验不得新增信任库记录");
    assert_eq!(rows2[0].source, "imported", "存行来源仍为 imported");
}
