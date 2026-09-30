use fs_itest::sshd::SshdContainer;
use russh::client;
use russh::ChannelMsg;
use std::sync::Arc;
use std::time::Duration;

/// 集成测试基线用的 accept-all 校验器；产品路径见 Task 10（禁止 accept-all）。
///
/// `seen_host_key` 记录握手中服务器**实际出示**的宿主密钥（OpenSSH 单行文本）。
/// known_hosts 用例原先只读容器里的 `*.pub` 文件就断言「这就是 sshd 出示的密钥」——
/// 那是假设不是取证，accept-all 让握手侧对密钥不着一字（S29）。有了这份记录才能真的比对。
#[derive(Clone, Default)]
struct AcceptAllKeys {
    seen_host_key: Arc<std::sync::Mutex<Option<String>>>,
}

impl russh::client::Handler for AcceptAllKeys {
    type Error = russh::Error;
    async fn check_server_key(
        &mut self,
        server_key: &russh::keys::PublicKey,
    ) -> Result<bool, Self::Error> {
        *self.seen_host_key.lock().unwrap() = server_key.to_openssh().ok();
        Ok(true)
    }
}

async fn connect_password(
    addr: std::net::SocketAddr,
    user: &str,
    pass: &str,
    handler: AcceptAllKeys,
) -> russh::client::Handle<AcceptAllKeys> {
    let config = client::Config::default();
    let mut session = client::connect(Arc::new(config), addr, handler)
        .await
        .unwrap();
    let res = session.authenticate_password(user, pass).await.unwrap();
    assert!(res.success(), "password auth failed: {res:?}");
    session
}

/// 读 exec 通道直至 exit-status 或对端关闭，返回 (trim 后的 stdout, exit_status)。
///
/// 每次 `wait()` 都套 `timeout_at`：`ChannelMsg` 流本身不带超时，服务器若既不发数据也不关
/// 通道，`while let Some(..) = channel.wait().await` 会**无限期挂起**；nextest 默认只对慢测试
/// 打印 SLOW 而不终止它，于是表现为 CI 任务级超时而不是一条测试失败（S27）。
///
/// exit_status 单独返回而不在循环里就地断言：就地断言只有收到 `ExitStatus` 才会执行，
/// 服务器若直接关通道，循环从 `None` 退出、断言被**静默跳过**，测试照样绿（S28）。
async fn collect_exec(
    channel: &mut russh::Channel<client::Msg>,
    within: Duration,
) -> (String, Option<u32>) {
    let deadline = tokio::time::Instant::now() + within;
    let mut out = Vec::new();
    let mut status = None;
    loop {
        match tokio::time::timeout_at(deadline, channel.wait()).await {
            Ok(Some(ChannelMsg::Data { data })) => out.extend_from_slice(&data),
            Ok(Some(ChannelMsg::ExitStatus { exit_status })) => {
                status = Some(exit_status);
                break;
            }
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(_) => panic!(
                "timeout collecting exec output; got so far: {:?}",
                String::from_utf8_lossy(&out)
            ),
        }
    }
    (String::from_utf8_lossy(&out).trim().to_string(), status)
}

#[tokio::test(flavor = "multi_thread")]
async fn password_auth_and_exec() {
    if std::env::var("FS_ITEST").is_err() {
        eprintln!("skip: set FS_ITEST=1");
        return;
    }
    let sshd = SshdContainer::start("exec").await.unwrap();
    let session = connect_password(
        sshd.addr().await,
        &sshd.username,
        &sshd.password,
        AcceptAllKeys::default(),
    )
    .await;

    let mut channel = session.channel_open_session().await.unwrap();
    channel.exec(false, "echo hello-fs").await.unwrap();
    let (out, status) = collect_exec(&mut channel, Duration::from_secs(30)).await;
    assert_eq!(
        status,
        Some(0),
        "exit status missing or nonzero; stdout={out}"
    );
    assert_eq!(out, "hello-fs");
}

#[tokio::test(flavor = "multi_thread")]
async fn pty_shell_interactive() {
    if std::env::var("FS_ITEST").is_err() {
        eprintln!("skip: set FS_ITEST=1");
        return;
    }
    let sshd = SshdContainer::start("pty").await.unwrap();
    let session = connect_password(
        sshd.addr().await,
        &sshd.username,
        &sshd.password,
        AcceptAllKeys::default(),
    )
    .await;

    let mut channel = session.channel_open_session().await.unwrap();
    channel
        .request_pty(false, "xterm-256color", 80, 24, 0, 0, &[])
        .await
        .unwrap();
    channel.request_shell(false).await.unwrap();
    channel.data(&b"echo MARK-$((20+5))\n"[..]).await.unwrap();

    // 原写法 `while now < deadline { if let Some(Data) = channel.wait().await {..} }` 有两个洞（S27）：
    // ① deadline 只在两次 await 之间检查，`wait()` 自身无超时 —— 服务器沉默即永久挂起；
    // ② `wait()` 返回 `None`（通道已关）时 `if let` 不匹配，循环立刻重来、`None` 立刻再返回，
    //    于是满 CPU 空转到 deadline，最后报一句看不出根因的 "pty output: "。
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let mut seen = String::new();
    loop {
        match tokio::time::timeout_at(deadline, channel.wait()).await {
            Ok(Some(ChannelMsg::Data { data })) => {
                seen.push_str(&String::from_utf8_lossy(&data));
                if seen.contains("MARK-25") {
                    break;
                }
            }
            Ok(Some(_)) => {}
            Ok(None) => panic!("channel closed before MARK-25; pty output: {seen}"),
            Err(_) => panic!("timeout waiting for MARK-25; pty output: {seen}"),
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn wrong_password_reports_remaining_methods() {
    if std::env::var("FS_ITEST").is_err() {
        eprintln!("skip: set FS_ITEST=1");
        return;
    }
    let sshd = SshdContainer::start("badpw").await.unwrap();
    let config = client::Config::default();
    let mut session = client::connect(
        Arc::new(config),
        sshd.addr().await,
        AcceptAllKeys::default(),
    )
    .await
    .unwrap();
    let res = session
        .authenticate_password(&sshd.username, "wrong")
        .await
        .unwrap();
    match res {
        client::AuthResult::Success => panic!("wrong password must not authenticate"),
        client::AuthResult::Failure {
            remaining_methods, ..
        } => {
            // AuthResult 是枚举：Success | Failure { remaining_methods: MethodSet, partial_success }
            assert!(
                !remaining_methods.is_empty(),
                "server should announce remaining methods"
            );
            // 只断言「非空」太弱：Task 8 的 `Method::from_wire` 认的是 RFC 4252 线上名字串表
            // （"password" / "publickey" / "keyboard-interactive"），而它的单元测试是拿同一批
            // 字面量喂进去的 —— 自证。这里把真实 OpenSSH 的通告过一遍那张表，字符串对不上就红，
            // 于是 Task 8 状态机的输入口径由假设变成取证（S30）。
            let announced: Vec<&'static str> = remaining_methods.iter().map(|m| m.into()).collect();
            let mapped: Vec<fs_sshengine::auth::Method> = announced
                .iter()
                .filter_map(|n| fs_sshengine::auth::Method::from_wire(n))
                .collect();
            for expect in [
                fs_sshengine::auth::Method::PublicKey,
                fs_sshengine::auth::Method::Password,
                fs_sshengine::auth::Method::KbdInteractive,
            ] {
                assert!(
                    mapped.contains(&expect),
                    "server announced {announced:?} -> mapped {mapped:?}, missing {expect:?}"
                );
            }
        }
    }
}

/// known_hosts 导入通路的真实服务器验证（Round 5 路线图复审 HIGH#3 回灌、M1 出口「导入 known_hosts」）：
/// 经已认证会话 exec 读取容器宿主密钥公钥文件，**并与握手中服务器实际出示的密钥比对**，
/// 再拼成 known_hosts 行走 Task 7 `import_known_hosts`，断言落库、去重与指纹（二进制 blob 的 SHA256）自洽。
#[tokio::test(flavor = "multi_thread")]
async fn known_hosts_import_matches_server_host_key() {
    use base64::Engine as _;
    if std::env::var("FS_ITEST").is_err() {
        eprintln!("skip: set FS_ITEST=1");
        return;
    }
    let sshd = SshdContainer::start("kh").await.unwrap();
    let addr = sshd.addr().await;
    let handler = AcceptAllKeys::default();
    let session = connect_password(addr, &sshd.username, &sshd.password, handler.clone()).await;

    let mut channel = session.channel_open_session().await.unwrap();
    channel
        .exec(false, "cat /etc/ssh/ssh_host_ed25519_key.pub")
        .await
        .unwrap();
    let (pub_line, status) = collect_exec(&mut channel, Duration::from_secs(30)).await;
    assert_eq!(status, Some(0), "cat host key failed; output={pub_line}");
    assert!(
        pub_line.starts_with("ssh-ed25519 "),
        "host key file content: {pub_line}"
    );
    let b64 = pub_line.split_whitespace().nth(1).unwrap();

    // 文件里的公钥必须就是握手时出示的那把，否则后面所有断言都只是在自说自话（S29）。
    // to_openssh() 无 comment 段，故两边都取第 2 个字段（base64 本体）比。
    let handshake = handler
        .seen_host_key
        .lock()
        .unwrap()
        .clone()
        .expect("check_server_key must have run");
    assert_eq!(
        handshake.split_whitespace().nth(1),
        Some(b64),
        "file key != handshake key; handshake={handshake}, file={pub_line}"
    );

    let host = addr.ip().to_string();
    let known_hosts = format!("[{host}]:{} {pub_line}", addr.port());

    let dir = tempfile::tempdir().unwrap();
    let db = fs_connmgr::Db::open(&dir.path().join("fs.db"))
        .await
        .unwrap();
    let ts = fs_sshengine::hostkey::TrustStore::new(db.pool());
    let s = ts.import_known_hosts(&known_hosts).await.unwrap();
    assert_eq!(
        s,
        fs_sshengine::hostkey::ImportSummary {
            imported: 1,
            ..Default::default()
        },
        "{s:?}"
    );

    let got = ts.lookup(&host, addr.port()).await.unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].key_blob, b64);
    assert_eq!(got[0].source, "imported");
    let bin = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .unwrap();
    assert_eq!(
        got[0].fingerprint_sha256,
        fs_sshengine::hostkey::fingerprint_sha256(&bin)
    );

    // 幂等复导入：同 key 只计 skipped
    let s2 = ts.import_known_hosts(&known_hosts).await.unwrap();
    assert_eq!(s2.imported, 0);
}
