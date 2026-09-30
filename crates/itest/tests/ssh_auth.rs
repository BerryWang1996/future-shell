//! S32 回归：认证状态机必须**先问服务器通告、再挑方法**，绝不尝试未通告的方法（spec §2.1）。

use fs_connmgr::{AuthRef, Db, HostKeyPolicy, Profile};
use fs_itest::sshd::SshdContainer;
use fs_sshengine::events::{HostKeyChoice, Prompt, SessionEvents};
use fs_sshengine::hostkey::{Decision, PresentedKey};
use fs_sshengine::secrets::SecretSource;
use std::sync::{Arc, Mutex};
use zeroize::Zeroizing;

/// 主机密钥一律接受（本例的被测面是认证方法选择，不是 TOFU）；status 留痕供诊断
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
        _h: &Decision,
    ) -> HostKeyChoice {
        HostKeyChoice::AcceptOnce
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

/// 只有口令、没有私钥的凭据源
struct PasswordOnly(String);
impl SecretSource for PasswordOnly {
    fn secret(&self, _rec: u64) -> Result<fs_sshengine::secrets::Secret, fs_sshengine::Error> {
        Ok(fs_sshengine::secrets::Secret {
            kind: fs_sshengine::secrets::SecretKind::Password,
            bytes: Zeroizing::new(self.0.clone().into_bytes()),
        })
    }
}

fn profile(host: String, port: u16, username: String) -> Profile {
    Profile {
        id: uuid::Uuid::nil(),
        name: "s32".into(),
        group_path: None,
        host,
        port,
        username,
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
    }
}

/// 服务器只通告 publickey，而 profile 只有口令 → 必须**一次尝试都不发**就判失败。
///
/// 判据是 `Error::Auth.tried` 为空。修复前的实现把 remaining 初值硬编码成全四项，会先发一次
/// password 认证请求（把用户明文口令送给一台永不使用它的服务器，并白占一次 MaxAuthTries），
/// 此时 `tried == ["password"]`——两者相差恰好一次真实的网络认证尝试，断言因此是可证伪的。
#[tokio::test(flavor = "multi_thread")]
async fn never_attempts_unannounced_method() {
    if std::env::var("FS_ITEST").is_err() {
        eprintln!("skip: set FS_ITEST=1");
        return;
    }
    let sshd = SshdContainer::start("auth").await.unwrap();
    sshd.restrict_to_publickey_only().await.unwrap();
    let addr = sshd.addr().await;

    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("fs-auth.db")).await.unwrap();
    let events = Arc::new(AcceptKeys::default());
    let p = profile(addr.ip().to_string(), addr.port(), sshd.username.clone());
    let secrets = PasswordOnly(sshd.password.clone());

    // 用 match 而非 expect_err：Ok 侧是 russh 的 client::Handle<Connector>，它未实现 Debug（E0277）
    let err = match fs_sshengine::connect::connect(&p, &secrets, db.pool(), events.clone()).await {
        Ok(_) => panic!("服务器只通告 publickey 而本例只有口令，连接必须失败"),
        Err(e) => e,
    };

    match err {
        // notes 不参与本例判据（S38 新增栏位，只在本地故障时才有内容），故 `..` 忽略
        fs_sshengine::Error::Auth {
            tried, remaining, ..
        } => {
            assert!(
                tried.is_empty(),
                "认证状态机在拿到服务器通告前不得尝试任何方法；实际已尝试：{tried:?}（statuses={:?}）",
                events.statuses.lock().unwrap()
            );
            assert_eq!(
                remaining,
                vec!["publickey".to_string()],
                "服务器通告未被如实带出（容器收紧配置可能没生效）"
            );
        }
        other => panic!("期望 Error::Auth，实得 {other:?}"),
    }
}

/// 无任何凭据可取的来源：档案没存口令时，连接层必须走 `password_prompt` 向用户现问。
struct NoSecret;
impl SecretSource for NoSecret {
    fn secret(&self, _rec: u64) -> Result<fs_sshengine::secrets::Secret, fs_sshengine::Error> {
        Err(fs_sshengine::Error::Connect(
            "无凭据（测试不提供任何 vault 记录）".into(),
        ))
    }
}

/// 口令询问的应答者：记录「确实被问了」，并回以容器口令。
struct PromptPassword {
    password: String,
    prompted: std::sync::atomic::AtomicBool,
}
impl SessionEvents for PromptPassword {
    fn host_key_decision(
        &self,
        _h: &str,
        _p: u16,
        _k: &PresentedKey,
        _d: &Decision,
    ) -> HostKeyChoice {
        HostKeyChoice::AcceptOnce
    }
    fn kbd_interactive(&self, _n: &str, _i: &str, _p: &[Prompt]) -> Vec<String> {
        vec![]
    }
    fn password_prompt(&self) -> String {
        self.prompted
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self.password.clone()
    }
    fn status(&self, _m: &str) {}
}

/// 连接时输口令（Task 47）：档案没配口令（vault_record=None）+ 无任何凭据来源，
/// 服务器又通告 password → 连接层必须**弹一次口令询问**，拿用户输入的口令完成密码认证。
///
/// 可证伪点：`prompted` 恒为 false（删掉 connect.rs 里的询问分支）时连接会因零凭据失败，
/// 断言「连接成功」与「确实问过」双双转红——一条用例钉死整条询问链路。
#[tokio::test(flavor = "multi_thread")]
async fn passwordless_profile_prompts_and_connects() {
    if std::env::var("FS_ITEST").is_err() {
        eprintln!("skip: set FS_ITEST=1");
        return;
    }
    let sshd = SshdContainer::start("auth-prompt").await.unwrap();
    let addr = sshd.addr().await;

    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("fs-auth-prompt.db"))
        .await
        .unwrap();
    let events = Arc::new(PromptPassword {
        password: sshd.password.clone(),
        prompted: std::sync::atomic::AtomicBool::new(false),
    });
    // vault_record=None：档案层面没有口令；NoSecret 让「从 vault 取」也取不到。
    let mut p = profile(addr.ip().to_string(), addr.port(), sshd.username.clone());
    p.auth.vault_record = None;
    let secrets = NoSecret;

    fs_sshengine::connect::connect(&p, &secrets, db.pool(), events.clone())
        .await
        .expect("现场输口令应当足以完成认证；连接失败");

    assert!(
        events.prompted.load(std::sync::atomic::Ordering::SeqCst),
        "零凭据档案没有走到 password_prompt——连接时输口令的链路断了（但连接又成功了？）"
    );
}

/// 只有私钥、没有口令的凭据源（公钥认证一路）。
struct PrivateKeyOnly(String);
impl SecretSource for PrivateKeyOnly {
    fn secret(&self, _rec: u64) -> Result<fs_sshengine::secrets::Secret, fs_sshengine::Error> {
        Ok(fs_sshengine::secrets::Secret {
            kind: fs_sshengine::secrets::SecretKind::PrivateKey,
            bytes: Zeroizing::new(self.0.clone().into_bytes()),
        })
    }
}

/// **公钥认证成功**一路（M1 出口原文「密码/公钥/kbd-interactive/agent 四路认证 itest 全绿」）。
///
/// 2026-08-23 盘点发现：这一路此前**没有任何容器 itest**，而且不是「忘了写」——
/// `crates/itest/src/sshd.rs` 压根没有植入 `authorized_keys` 的能力，写不出来。
/// 既有的 `never_attempts_unannounced_method` 用 `restrict_to_publickey_only` 制造的是
/// **失败**场景（服务器只通告 publickey 而本例只有口令），它证明的是「不乱试方法」，
/// 与「公钥能连上」是两回事。
///
/// 判据取**服务端能看到的结果**：认证通过并在通道上跑出预期输出。只断言
/// `authenticate_publickey` 返回 success 不够——那只说明协议层点头了，不代表会话可用。
#[tokio::test(flavor = "multi_thread")]
async fn publickey_auth_succeeds_against_real_sshd() {
    if std::env::var("FS_ITEST").is_err() {
        eprintln!("skip: set FS_ITEST=1");
        return;
    }
    let sshd = SshdContainer::start("pubkey").await.unwrap();
    // 装公钥并取回私钥 PEM
    let key_pem = sshd.install_authorized_key().await.unwrap();
    assert!(
        key_pem.contains("BEGIN OPENSSH PRIVATE KEY"),
        "取回的应当是 OpenSSH 私钥 PEM，实得前 40 字：{:?}",
        key_pem.chars().take(40).collect::<String>()
    );
    // 收紧为**只通告 publickey**：否则即便公钥路径坏了，也可能悄悄回落口令认证而测试照旧绿
    sshd.restrict_to_publickey_only().await.unwrap();

    let addr = sshd.addr().await;
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("fs-pubkey.db")).await.unwrap();
    let events = Arc::new(AcceptKeys::default());
    let p = profile(addr.ip().to_string(), addr.port(), sshd.username.clone());
    let secrets = PrivateKeyOnly(key_pem);

    let handle = fs_sshengine::connect::connect(&p, &secrets, db.pool(), events.clone())
        .await
        .unwrap_or_else(|e| {
            panic!(
                "公钥认证应当成功：{e}；statuses={:?}",
                events.statuses.lock().unwrap()
            )
        });

    // 会话真的可用：开通道跑一条命令并比对输出
    let mut ch = handle.channel_open_session().await.unwrap();
    ch.exec(false, "printf 'pubkey-ok'").await.unwrap();
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
                "读公钥会话输出超时；已收到 {:?}",
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
    assert_eq!(
        String::from_utf8_lossy(&out).trim(),
        "pubkey-ok",
        "公钥会话上的命令输出不符——认证过了但通道不可用，同样不算这一路达成"
    );

    // 状态播报里应当出现 publickey：用户要能看出用的是哪种认证
    let statuses = events.statuses.lock().unwrap().clone();
    assert!(
        statuses.iter().any(|s| s.contains("publickey")),
        "认证方法应经 status 播报（用户要知道用的是公钥还是口令）；实收 {statuses:?}"
    );
}

/// 记下 kbd-interactive 的提示，并以真口令应答。
///
/// 与文件顶部的 `AcceptKeys` 分开写而不是加一个字段：那一个的 `kbd_interactive`
/// **恒回空串**，是「不作答」的语义，两条既有测试依赖它。把它改成会作答的，
/// 那两条会以看不出来的方式变了含义。
#[derive(Default)]
struct AnswerKbd {
    password: String,
    statuses: Mutex<Vec<String>>,
    /// 服务器实际发过来的提示词。判据要看它——空的提示列表意味着这一轮
    /// 根本没走到 InfoRequest，那时「认证成功」可能来自别的方法。
    prompts_seen: Mutex<Vec<String>>,
}
impl SessionEvents for AnswerKbd {
    fn host_key_decision(
        &self,
        _host: &str,
        _port: u16,
        _p: &PresentedKey,
        _h: &Decision,
    ) -> HostKeyChoice {
        HostKeyChoice::AcceptOnce
    }
    fn kbd_interactive(&self, _name: &str, _instruction: &str, prompts: &[Prompt]) -> Vec<String> {
        let mut seen = self.prompts_seen.lock().unwrap();
        for p in prompts {
            seen.push(p.text.clone());
        }
        prompts.iter().map(|_| self.password.clone()).collect()
    }
    fn password_prompt(&self) -> String {
        // **故意返回空串**：这一路若悄悄回落到口令认证，服务器会拒掉空口令，
        // 测试红。判据因此不会被「口令碰巧也能连上」蒙混过去。
        String::new()
    }
    fn status(&self, msg: &str) {
        self.statuses.lock().unwrap().push(msg.to_string());
    }
}

/// **keyboard-interactive 认证成功**一路（M1 出口原文四路里的第三路）。
///
/// 2026-08-26 盘点：这一路此前**没有任何容器 itest**。全仓那几处 `kbd_interactive`
/// 出现在单测与纯函数里（`auth::kbd_auto_answer` 的判定、事件回调的形状），
/// 没有一条打到真 sshd——而这一路的价值恰恰在于 InfoRequest 往返本身，
/// 那是纯函数测不到的部分。
///
/// 服务器用 `KbdSshd`：Debian + PAM，且**只通告 keyboard-interactive**
/// （publickey 与 password 都关掉）。只通告一种是判据的一半——三样俱全时
/// 口令认证成功也会让测试变绿，而那证明不了 kbd-interactive 链上的任何一行。
///
/// 判据三层，逐层更严：
/// ① 连上了；② 服务器**真的发过 InfoRequest**（prompts_seen 非空）；
/// ③ 通道可用（跑出预期输出）——认证过了但通道不可用同样不算这一路达成。
#[tokio::test(flavor = "multi_thread")]
async fn kbd_interactive_auth_succeeds_against_real_sshd() {
    if std::env::var("FS_ITEST").is_err() {
        eprintln!("skip: set FS_ITEST=1");
        return;
    }
    let sshd = fs_itest::kbd_sshd::KbdSshd::start("kbd").await.unwrap();
    let addr = sshd.addr().await;
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("fs-kbd.db")).await.unwrap();
    let events = Arc::new(AnswerKbd {
        password: sshd.password.clone(),
        ..Default::default()
    });

    let mut p = profile(addr.ip().to_string(), addr.port(), sshd.username.clone());
    p.auth.allow_kbd_interactive = true;
    // vault_record 置空：这一路不该有任何口令/私钥可用，逼它只能走 kbd-interactive。
    p.auth.vault_record = None;

    let handle =
        fs_sshengine::connect::connect(&p, &PasswordOnly(String::new()), db.pool(), events.clone())
            .await
            .unwrap_or_else(|e| {
                panic!(
                    "keyboard-interactive 认证应当成功：{e}；statuses={:?}",
                    events.statuses.lock().unwrap()
                )
            });

    // ② 服务器真的问过——没有这一条的话，一次「碰巧成功」的其它方法也会让测试绿。
    let prompts = events.prompts_seen.lock().unwrap().clone();
    assert!(
        !prompts.is_empty(),
        "服务器从未发过 InfoRequest：这一轮走的不是 keyboard-interactive"
    );

    // ③ 通道可用
    let mut ch = handle.channel_open_session().await.unwrap();
    ch.exec(false, "printf 'kbd-ok'").await.unwrap();
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
                "读 kbd 会话输出超时；已收到 {:?}",
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
    assert_eq!(
        String::from_utf8_lossy(&out).trim(),
        "kbd-ok",
        "kbd 会话上的命令输出不符——认证过了但通道不可用，同样不算这一路达成"
    );

    let statuses = events.statuses.lock().unwrap().clone();
    assert!(
        statuses.iter().any(|s| s.contains("keyboard-interactive")),
        "认证方法应经 status 播报；实收 {statuses:?}"
    );
}

/// 反向对照：同一台服务器上，**答错口令**必须失败。
///
/// 没有这一条的话，上一条的绿也可能来自「服务器根本不校验应答」——
/// 一台配错 PAM 的 sshd 正是那个样子（通告 kbd-interactive、对任何应答都点头）。
/// 这条测试把那种服务器与真服务器区分开，因此它守的是**上一条测试的前提**。
#[tokio::test(flavor = "multi_thread")]
async fn kbd_interactive_with_a_wrong_answer_is_rejected() {
    if std::env::var("FS_ITEST").is_err() {
        eprintln!("skip: set FS_ITEST=1");
        return;
    }
    let sshd = fs_itest::kbd_sshd::KbdSshd::start("kbdbad").await.unwrap();
    let addr = sshd.addr().await;
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("fs-kbdbad.db")).await.unwrap();
    let events = Arc::new(AnswerKbd {
        password: "definitely-not-the-password".into(),
        ..Default::default()
    });

    let mut p = profile(addr.ip().to_string(), addr.port(), sshd.username.clone());
    p.auth.allow_kbd_interactive = true;
    p.auth.vault_record = None;

    let err =
        fs_sshengine::connect::connect(&p, &PasswordOnly(String::new()), db.pool(), events.clone())
            .await
            .err()
            .expect("错误的应答必须被拒绝——服务器点头就说明它根本没校验");

    // 且失败原因是认证，不是别的（连不上、超时都会让上一条测试的前提落空）。
    let fs_sshengine::Error::Auth { tried, .. } = &err else {
        panic!("失败原因该是认证：{err:?}");
    };
    // **服务器只通告了 keyboard-interactive** ——这是上一条测试的前提，
    // 在这里一次钉死：`tried` 只可能含服务器通告过的方法（认证状态机的
    // 通告驱动性质，见本文件开头的 S32 回归）。混进 password / publickey
    // 就说明容器配置没生效，那时上一条测试的绿可能来自别的方法。
    assert_eq!(
        tried,
        &vec!["keyboard-interactive".to_string()],
        "服务器不该通告 keyboard-interactive 以外的方法——容器配置没生效"
    );
    // 服务器确实问过——问都没问就拒绝，说明它压根没通告 kbd-interactive。
    assert!(
        !events.prompts_seen.lock().unwrap().is_empty(),
        "服务器没发过 InfoRequest：这台服务器不是在做 keyboard-interactive"
    );
}
