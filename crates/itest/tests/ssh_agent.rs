// 本文件**不引容器**（R56）：`connect_agent_client()` 读的是**宿主** SSH_AUTH_SOCK，容器与断言无因果
// 关系——原 `SshdContainer::start("agent")` 从未被使用（名实分离），同批删除，`SshdContainer` 导入随之移除。
// 容器内起真 `ssh-agent` 的对照留到需要「真实签名往返」的那一天（那才是假 agent 补不上的部分）。
//
// ## 为什么需要一个假 agent（2026-08-25）
//
// 交叉审计指出：本文件的成功分支（`Ok ⇒ request_identities`）**从未在任何环境被真正执行过**——
// `SSH_AUTH_SOCK` 依赖宿主环境，CI 托管 runner 默认不设，于是这条测试在 CI 上恒走 `Err` 分支，
// 「连接成功」那一半的可执行证据是零。这正是一种**可平凡通过**：绿只证明了「无 agent 时报错文案
// 点名三管道」，与「agent 可用时真的可用」无关。
//
// 补法是在测试里**自己起一个说 SSH agent 协议的 Unix socket**，把 `SSH_AUTH_SOCK` 指过去，
// 逼出成功分支。它不替代真 `ssh-agent`（不做签名），但它把「构造 → 连接 → 问身份」这条链从
// 「从没跑过」变成「每次 CI 都跑」——而那条链恰恰是 S37 幽灵句柄事故所在。

/// `SSH_AUTH_SOCK` 是**进程级**环境变量。本文件里两条测试都会读写它（一条读、
/// 一条设），`cargo test` 默认并行，若不同步，一条设了值另一条就可能读到假象。
/// 用一把进程内的锁把「读环境 + 连接」整段串行化。
///
/// 用 `tokio::sync::Mutex` 而非 `std::sync::Mutex`：守卫必须**跨过** `connect` 的
/// await（否则锁在 await 处让出，另一条测试可能在这期间改掉 SSH_AUTH_SOCK），
/// 而 std 锁跨 await 正是 clippy `await_holding_lock` 要拦的形状。
static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[tokio::test(flavor = "multi_thread")]
async fn agent_unavailable_error_names_transports() {
    // 三平台同一套断言（S37 后不再按 cfg 跳过 Windows）：`connect_agent_client()` 的契约被收紧为
    // 「返回 Ok 即代表可用」，两条平台无关的判据由此成立——Ok ⇒ 真能问出身份列表，Err ⇒ 文案点名三条传输。
    if std::env::var("FS_ITEST").is_err() {
        eprintln!("skip: set FS_ITEST=1");
        return;
    }
    let _guard = ENV_LOCK.lock().await;
    // 本条要的是「**没有** agent」那一支。先把假测试可能留下的值清掉，保证读到的是真实环境。
    std::env::remove_var("SSH_AUTH_SOCK");
    let had_sock = std::env::var("SSH_AUTH_SOCK").is_ok();
    let got = fs_sshengine::agent::connect_agent_client().await;

    // unix 上 SSH_AUTH_SOCK 已设置即意味着有活着的 agent：此分支不得静默跳过
    if cfg!(unix) && had_sock {
        assert!(
            got.is_ok(),
            "SSH_AUTH_SOCK 已设置时须连上 agent: {:?}",
            got.as_ref().err()
        );
    }

    match got {
        Ok(mut c) => {
            // S37 本体：构造成功 ≠ 可用。旧实现在 Windows 上恒返回一个「幽灵 Pageant 句柄」
            // （Pageant 没在跑也 Ok），首次 I/O 才炸成 "early eof"；那时命名管道回退已被跳过，
            // 三管道指引也永远送不出去。这条断言正是用来钉死「Ok 必须真的可用」。
            let ids = c.request_identities().await;
            assert!(
                ids.is_ok(),
                "connect_agent_client 返回 Ok 就必须真的可用，实得: {:?}",
                ids.err()
            );
        }
        Err(e) => {
            // 无 agent：应得到带指引的错误而非 panic，且文案须点名三条传输
            let msg = format!("{e:?}");
            for needle in ["SSH_AUTH_SOCK", "Pageant", "openssh-ssh-agent"] {
                assert!(msg.contains(needle), "诊断须点名 {needle}，实得: {msg}");
            }
        }
    }
}

/// SSH agent 协议里「列出身份」的请求与应答。
///
/// 请求：`len(1) | 11`（SSH_AGENTC_REQUEST_IDENTITIES）。
/// 应答：`len | 12(SSH_AGENT_IDENTITIES_ANSWER) | count | 每把钥匙: bloblen | blob | commentlen | comment`。
///
/// 这里只实现「应答一次 REQUEST_IDENTITIES」——假 agent 需要的全部，多一个字节都不写。
/// 它**不做签名**（SIGN 请求直接回失败），所以它验证的是「连接 + 枚举」，不是「真能登录」；
/// 后者需要真 `ssh-agent`，见文件头。
#[cfg(unix)]
mod fake_agent {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::UnixListener;

    /// 构造一把 ed25519 公钥的 blob。不校验点是否在曲线上——协议解析只要求
    /// 「类型串 + 恰好 32 字节」，`request_identities` 不做密码学验证。
    fn ed25519_blob() -> Vec<u8> {
        let mut blob = Vec::new();
        blob.extend_from_slice(&11u32.to_be_bytes());
        blob.extend_from_slice(b"ssh-ed25519");
        blob.extend_from_slice(&32u32.to_be_bytes());
        blob.extend_from_slice(&[0x42u8; 32]); // 任意 32 字节
        blob
    }

    /// 一条「报 count 把钥匙」的 IDENTITIES_ANSWER 报文。
    fn identities_answer(count: u32) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.push(12); // SSH_AGENT_IDENTITIES_ANSWER
        payload.extend_from_slice(&count.to_be_bytes());
        for _ in 0..count {
            let blob = ed25519_blob();
            payload.extend_from_slice(&(blob.len() as u32).to_be_bytes());
            payload.extend_from_slice(&blob);
            let comment = b"itest-fake";
            payload.extend_from_slice(&(comment.len() as u32).to_be_bytes());
            payload.extend_from_slice(comment);
        }
        let mut msg = Vec::new();
        msg.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        msg.extend_from_slice(&payload);
        msg
    }

    /// 起一个只应答一次身份查询的假 agent，返回它监听的 socket 路径。
    ///
    /// 后台任务在收到第一个 REQUEST_IDENTITIES 后作答并退出；连接数与应答次数
    /// 通过返回的 `answered` 计数回传，供测试断言「真的被问过」。
    pub async fn spawn(
        dir: &std::path::Path,
    ) -> (
        std::path::PathBuf,
        std::sync::Arc<std::sync::atomic::AtomicUsize>,
    ) {
        let sock = dir.join("agent.sock");
        let listener = UnixListener::bind(&sock).expect("绑定假 agent socket");
        let answered = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let answered_clone = answered.clone();

        tokio::spawn(async move {
            loop {
                let (mut stream, _) = match listener.accept().await {
                    Ok(s) => s,
                    Err(_) => break,
                };
                let answered = answered_clone.clone();
                tokio::spawn(async move {
                    // 同一条连接上会来**不止一次**请求：`connect_agent_client` 的
                    // 存活探测（live_dynamic）自己就发一次 request_identities，之后
                    // 调用方还会再发。故在这里循环应答到对端关闭，而不是答一次就走——
                    // 答一次就 shutdown 会让第二次查询吃 BrokenPipe。
                    loop {
                        // 读请求头：4 字节长度 + 1 字节类型。
                        let mut hdr = [0u8; 5];
                        if stream.read_exact(&mut hdr).await.is_err() {
                            break; // 对端关闭
                        }
                        let req_type = hdr[4];
                        if req_type == 11 {
                            // REQUEST_IDENTITIES → 回一把钥匙
                            if stream.write_all(&identities_answer(1)).await.is_err() {
                                break;
                            }
                            answered.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        } else {
                            // 其它请求（含 SIGN）：回 FAILURE(5)，表明假 agent 不越权
                            let mut fail = Vec::new();
                            fail.extend_from_slice(&1u32.to_be_bytes());
                            fail.push(5);
                            if stream.write_all(&fail).await.is_err() {
                                break;
                            }
                        }
                    }
                });
            }
        });
        (sock, answered)
    }
}

/// 成功路径：`SSH_AUTH_SOCK` 指向一个活着的（这里是假的）agent 时，
/// `connect_agent_client()` 必须 `Ok`，且 `request_identities()` 必须真的问出钥匙。
///
/// **这条是「SSH_AUTH_SOCK agent 可用」那半句的可执行证据。** 它只在 unix 上编译——
/// Windows 的成功路径走 Pageant / 命名管道，仍需真机（见 `agent_unavailable_*` 的人工残留注记）。
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_live_agent_on_ssh_auth_sock_yields_its_keys() {
    if std::env::var("FS_ITEST").is_err() {
        eprintln!("skip: set FS_ITEST=1");
        return;
    }
    let _guard = ENV_LOCK.lock().await;

    let dir = std::env::temp_dir().join(format!("fs-itest-agent-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (sock, answered) = fake_agent::spawn(&dir).await;

    // 把 SSH_AUTH_SOCK 指到假 agent 上；退出时恢复，免得污染其它测试。
    let prev = std::env::var("SSH_AUTH_SOCK").ok();
    std::env::set_var("SSH_AUTH_SOCK", &sock);

    let got = fs_sshengine::agent::connect_agent_client().await;

    // 无论成败先恢复环境
    match prev {
        Some(v) => std::env::set_var("SSH_AUTH_SOCK", v),
        None => std::env::remove_var("SSH_AUTH_SOCK"),
    }
    let _ = std::fs::remove_dir_all(&dir);

    let mut client = got.expect("指向活着的假 agent 时须连接成功");
    let ids = client.request_identities().await.expect("须真的问出钥匙");
    assert!(
        !ids.is_empty(),
        "假 agent 报了一把钥匙，request_identities 不得返回空"
    );
    // 反向证明：这次「可用」是真的打了一次协议往返，不是构造成功即过。
    assert!(
        answered.load(std::sync::atomic::Ordering::SeqCst) >= 1,
        "假 agent 必须真的被问过身份查询——否则本条又退化成『构造成功』"
    );
}

// ═════════════ 真 ssh-agent 的真实签名往返（M1 出口四路里的第四路）═════════════
//
// 本文件开头那条注释写着：「容器内起真 `ssh-agent` 的对照留到需要『真实签名往返』
// 的那一天（那才是假 agent 补不上的部分）」。这一节就是那一天。
//
// 假 agent 只回身份列表、不签名。它把「构造 → 连接 → 问身份」这条链跑起来了，
// 但 agent 认证真正的那一步——**把待签数据交给 agent、拿回签名、送给服务器**——
// 它一次也没做过。那一步失败的样子是：身份列出来了、认证请求发出去了、
// 服务器拒绝了，而客户端报「agent 里的身份都不被接受」。与「服务器真的不认这把钥匙」
// 在断言上不可区分。
//
// 这里用**真的 `ssh-agent`** 与**真的 sshd 容器**：钥匙是容器自己 ssh-keygen 生成、
// 公钥已在 authorized_keys 里，所以「服务器不认」这个可能性被排除掉了。剩下唯一
// 能让它失败的就是签名那一步。

/// 起一个真 `ssh-agent`，把 `key_pem` 加进去，返回 (socket 路径, agent 进程)。
///
/// **不吞任何错误**：`ssh-agent` / `ssh-add` 不在 PATH 上、socket 没出现、
/// `ssh-add` 非零退出——每一种都 panic 并说明是哪一种。悄悄跳过等于让这条
/// 测试变成一个恒绿的装饰。
#[cfg(unix)]
fn start_real_agent(
    dir: &std::path::Path,
    key_pem: &str,
) -> (std::path::PathBuf, std::process::Child) {
    use std::os::unix::fs::PermissionsExt;

    let keyfile = dir.join("id_ed25519");
    std::fs::write(&keyfile, key_pem).expect("写私钥");
    // sshd 与 ssh-add 都对私钥权限有硬检查：过宽会被拒，且报错含糊。
    std::fs::set_permissions(&keyfile, std::fs::Permissions::from_mode(0o600)).expect("chmod 600");

    let sock = dir.join("agent.sock");
    // `-D`：前台运行（否则 ssh-agent 自己 fork 到后台，我们拿到的 Child 立刻退出，
    // 于是没有任何句柄可以在测试结束时把它杀掉——那才是真正会留下孤儿进程的写法）。
    let child = std::process::Command::new("ssh-agent")
        .args(["-a", &sock.display().to_string(), "-D"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("起不了 ssh-agent（PATH 上没有？precheck 镜像需 openssh-client）");

    // 等 socket 出现。轮询而不是 sleep 一个拍脑袋的时长。
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !sock.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "ssh-agent 起来了但 socket {} 一直没出现",
            sock.display()
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    let out = std::process::Command::new("ssh-add")
        .arg(&keyfile)
        .env("SSH_AUTH_SOCK", &sock)
        .output()
        .expect("起不了 ssh-add（PATH 上没有？）");
    assert!(
        out.status.success(),
        "ssh-add 失败：{}",
        String::from_utf8_lossy(&out.stderr)
    );
    (sock, child)
}

/// **agent 认证成功**一路，且签名是真的。
///
/// 判据四层，逐层更严：
/// ① agent 里确实有那把钥匙（`request_identities` 非空）——没有的话后面全是空转；
/// ② 连上了；
/// ③ 通道可用（跑出预期输出）；
/// ④ status 播报里出现 agent——用户要能看出用的是哪一路。
///
/// 服务器**只通告 publickey**（`restrict_to_publickey_only`）：否则即便 agent 路径
/// 坏了，也可能悄悄回落口令认证而测试照旧绿。而 profile 的 `vault_record` 置空、
/// 凭据源给空口令，那条回落路径本身也被堵死——两道防线，因为这一路最容易假绿。
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_real_agent_signature_authenticates_against_real_sshd() {
    use fs_connmgr::{AuthRef, Db, HostKeyPolicy, Profile};
    use fs_sshengine::events::{HostKeyChoice, Prompt, SessionEvents};
    use fs_sshengine::hostkey::{Decision, PresentedKey};
    use fs_sshengine::secrets::SecretSource;
    use std::sync::{Arc, Mutex};

    if std::env::var("FS_ITEST").is_err() {
        eprintln!("skip: set FS_ITEST=1");
        return;
    }
    let _guard = ENV_LOCK.lock().await;

    #[derive(Default)]
    struct Ev {
        statuses: Mutex<Vec<String>>,
    }
    impl SessionEvents for Ev {
        fn host_key_decision(
            &self,
            _h: &str,
            _p: u16,
            _k: &PresentedKey,
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
        fn status(&self, m: &str) {
            self.statuses.lock().unwrap().push(m.to_string());
        }
    }
    struct NoSecret;
    impl SecretSource for NoSecret {
        fn secret(&self, _r: u64) -> Result<fs_sshengine::secrets::Secret, fs_sshengine::Error> {
            Ok(fs_sshengine::secrets::Secret {
                kind: fs_sshengine::secrets::SecretKind::Password,
                bytes: zeroize::Zeroizing::new(Vec::new()),
            })
        }
    }

    let sshd = fs_itest::sshd::SshdContainer::start("agentauth")
        .await
        .unwrap();
    let key_pem = sshd.install_authorized_key().await.unwrap();
    sshd.restrict_to_publickey_only().await.unwrap();

    let dir = tempfile::tempdir().unwrap();
    let (sock, mut agent_proc) = start_real_agent(dir.path(), &key_pem);
    // 无论断言怎么失败，agent 进程都要被杀掉。用一个守卫而不是在末尾 kill——
    // panic 会跳过末尾那行，于是每一次红都留下一个 ssh-agent 进程。
    struct Kill<'a>(&'a mut std::process::Child);
    impl Drop for Kill<'_> {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let _kill = Kill(&mut agent_proc);

    std::env::set_var("SSH_AUTH_SOCK", &sock);

    // ① agent 里确实有钥匙
    let mut client = fs_sshengine::agent::connect_agent_client()
        .await
        .expect("真 agent 该连得上");
    let ids = client.request_identities().await.expect("该列得出身份");
    assert!(!ids.is_empty(), "ssh-add 加过了，agent 里却没有身份");

    // ② 连上
    let db = Db::open(&dir.path().join("fs-agent.db")).await.unwrap();
    let events = Arc::new(Ev::default());
    let addr = sshd.addr().await;
    let p = Profile {
        id: uuid::Uuid::nil(),
        name: "agent".into(),
        group_path: None,
        host: addr.ip().to_string(),
        port: addr.port(),
        username: sshd.username.clone(),
        protocol: fs_connmgr::Protocol::Ssh,
        auth: AuthRef {
            // 没有任何 vault 凭据：口令回落这条路被堵死。
            vault_record: None,
            allow_agent: true,
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
    let handle = fs_sshengine::connect::connect(&p, &NoSecret, db.pool(), events.clone())
        .await
        .unwrap_or_else(|e| {
            panic!(
                "agent 认证应当成功：{e}；statuses={:?}",
                events.statuses.lock().unwrap()
            )
        });

    // ③ 通道可用——协议层点头不等于会话能用
    let mut ch = handle.channel_open_session().await.unwrap();
    ch.exec(false, "printf 'agent-ok'").await.unwrap();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    let mut out = Vec::new();
    let mut status = None;
    loop {
        match tokio::time::timeout_at(deadline, ch.wait()).await {
            Ok(Some(russh::ChannelMsg::Data { data })) => out.extend_from_slice(&data),
            // 退出状态可能先于剩余输出到达（sshd 在子进程退出时即发），记下后继续收，
            // 直到通道关闭。在这里 break 过，CI 上偶发拿到空输出（2026-09-30）。
            Ok(Some(russh::ChannelMsg::ExitStatus { exit_status })) => status = Some(exit_status),
            Ok(Some(russh::ChannelMsg::Close)) => break,
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(_) => panic!(
                "读 agent 会话输出超时；已收到 {:?}",
                String::from_utf8_lossy(&out)
            ),
        }
    }
    assert_eq!(
        status,
        Some(0),
        "命令未成功；输出 {:?}",
        String::from_utf8_lossy(&out)
    );
    assert_eq!(String::from_utf8_lossy(&out).trim(), "agent-ok");

    // ④ 播报
    let statuses = events.statuses.lock().unwrap().clone();
    assert!(
        statuses.iter().any(|s| s.contains("agent")),
        "认证方法应经 status 播报；实收 {statuses:?}"
    );

    std::env::remove_var("SSH_AUTH_SOCK");
}
