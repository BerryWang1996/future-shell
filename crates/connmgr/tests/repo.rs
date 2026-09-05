mod common;
use fs_connmgr::model::limits;
use fs_connmgr::{
    AiPolicy, AuthRef, AutoExec, Db, Error, HostKeyPin, HostKeyPolicy, JumpHop, Profile,
    ProfileRepo, Protocol, SftpDefaults, TermSettings, TermThemeOverride,
};
use uuid::Uuid;

/// S5（med）：样本必须**每个字段都偏离默认值，且同类型字段两两不同**。
/// 历史样本大半是 `Default::default()`，于是 upsert 的 14 个 bind 与 SELECT 的 14 列
/// 即便整体错位一格，往返读回来仍是「默认 == 默认」，测试全绿而数据早已串行。
/// 唯一能钉死这条不变量的写法是：写进去的每一格都独一无二，读出来整体相等。
fn sample() -> Profile {
    Profile {
        id: Uuid::new_v4(),
        name: "prod-web-01".into(),
        group_path: Some("工作/生产".into()),
        host: "10.0.0.1".into(),
        port: 2222,
        username: "deploy".into(),
        protocol: Protocol::Ssh,
        auth: AuthRef {
            vault_record: Some(7),
            // S5 同理：口令记录与凭据记录必须取**不同**的值，否则 auth_blob 里两栏一旦
            // 写反/漏绑，往返读回来仍然相等，测试全绿而加密私钥的口令早已错位。
            passphrase_vault_record: Some(71),
            allow_agent: true,
            allow_kbd_interactive: true,
            kbd_auto_answer_single: true,
        },
        jump: vec![JumpHop {
            host: "bastion.internal".into(),
            port: 2022,
            username: "jump-user".into(),
            auth: AuthRef {
                vault_record: Some(8),
                passphrase_vault_record: Some(81),
                allow_agent: true,
                allow_kbd_interactive: false,
                kbd_auto_answer_single: false,
            },
            // P1-9：逐跳的策略/钉与 Profile 级取**不同**的值（Strict vs FingerprintPinned、
            // 两把不同的钉），这样 jump_blob 若错误地沿用了 Profile 级字段，往返断言会当场变红。
            host_key_policy: Some(HostKeyPolicy::Strict),
            host_key_pins: vec![HostKeyPin {
                key_blob: "AAAAC3NzaC1lZDI1NTE5AAAAIEJhc3Rpb24=".into(),
                fingerprint_sha256: "SHA256:YmFzdGlvbi1waW4tZXhhbXBsZQAAAA".into(),
            }],
        }],
        host_key_policy: HostKeyPolicy::FingerprintPinned,
        host_key_pins: vec![HostKeyPin {
            key_blob: "AAAAC3NzaC1lZDI1NTE5AAAAIExhbXBsZQ==".into(),
            fingerprint_sha256: "SHA256:0ZG9sZXhhbXBsZWZpbmdlcnByaW50AAAA".into(),
        }],
        env: [
            ("LANG".to_string(), "zh_CN.UTF-8".to_string()),
            ("TZ".to_string(), "Asia/Shanghai".to_string()),
        ]
        .into_iter()
        .collect(),
        term: TermSettings {
            term: Some("xterm-256color".into()),
            encoding: Some("GBK".into()),
            scrollback_lines: Some(54321),
            theme_override: Some(TermThemeOverride {
                scheme: Some("solarized-dark".into()),
                font_size: Some(15),
            }),
        },
        sftp: SftpDefaults {
            local_dir: Some("D:/downloads".into()),
            remote_dir: Some("/srv/app".into()),
            download_sandbox: Some("D:/sandbox".into()),
        },
        ai_policy: AiPolicy {
            auto_execute: AutoExec::WithConfirm,
            mcp_allowed: true,
        },
        serial: Default::default(),
    }
}

async fn db() -> (tempfile_guard::Dir, Db) {
    let dir = tempfile_guard::Dir::new();
    let db = Db::open(&dir.path.join("fs.db")).await.unwrap();
    (dir, db)
}

mod tempfile_guard {
    pub struct Dir {
        pub path: std::path::PathBuf,
    }
    impl Dir {
        pub fn new() -> Self {
            Dir {
                path: crate::common::tmpdir("repo"),
            }
        }
    }
}

#[tokio::test]
async fn crud_roundtrip() {
    let (_d, db) = db().await;
    let repo = ProfileRepo::new(db.pool());
    let mut p = sample();
    repo.upsert(&p).await.unwrap();

    // S5：整体相等 —— 14 个字段一个不落，列序错位/漏绑/类型截断都会在这里当场变红
    assert_eq!(repo.get(p.id).await.unwrap(), p, "写入与读出必须逐字段相等");
    assert_eq!(repo.list().await.unwrap(), vec![p.clone()]);

    // 覆盖路径（ON CONFLICT DO UPDATE）同样要逐字段成立，且不得新增行
    p.name = "renamed".into();
    p.port = 2200;
    p.env.insert("EXTRA".into(), "1".into());
    p.ai_policy.auto_execute = AutoExec::ReadOnly;
    repo.upsert(&p).await.unwrap();
    assert_eq!(repo.list().await.unwrap().len(), 1);
    assert_eq!(repo.get(p.id).await.unwrap(), p, "覆盖写同样须逐字段落回");

    repo.delete(p.id).await.unwrap();
    // 删掉后是「查无此条」，不是「行坏了」——两者绝不能混（S18 的反面）
    assert!(matches!(repo.get(p.id).await, Err(Error::NotFound(_))));
}

#[tokio::test]
async fn export_import_roundtrip() {
    let (_d1, db1) = db().await;
    let repo1 = ProfileRepo::new(db1.pool());
    let p = sample();
    repo1.upsert(&p).await.unwrap();
    let json = repo1.export_json().await.unwrap();
    assert!(!json.contains("password"), "导出不得含明文秘密");

    let (_d2, db2) = db().await;
    let repo2 = ProfileRepo::new(db2.pool());
    let n = repo2.import_json(&json).await.unwrap();
    assert_eq!(n, 1);
    let got = repo2.list().await.unwrap().remove(0);
    // 除去被有意剥离的信任/凭据字段，其余必须原样过来（否则导入等于悄悄改配置）
    assert_eq!(got.name, p.name);
    assert_eq!(got.host, p.host);
    assert_eq!(got.port, p.port);
    assert_eq!(got.username, p.username);
    assert_eq!(got.env, p.env);
    assert_eq!(got.term, p.term);
    // `sftp` 不能整体相等：`download_sandbox` 是被有意剥离的本机写边界授权（见
    // `import_strips_download_sandbox`）。这里逐字段断言，是为了让「保留的」与「剥离的」
    // 各自被钉住——写成整体相等就只剩一条断言，剥离行为一旦被误删，红的会是这一条，
    // 而它的失败信息只会说「sftp 不相等」，说不出丢的是授权字段还是偏好字段。
    assert_eq!(
        got.sftp.local_dir, p.sftp.local_dir,
        "起始本地目录是偏好，须保留"
    );
    assert_eq!(
        got.sftp.remote_dir, p.sftp.remote_dir,
        "起始远端目录是偏好，须保留"
    );
    assert_eq!(
        got.sftp.download_sandbox, None,
        "下载沙箱根是本机写边界授权，导入面必须剥离"
    );
    assert_eq!(got.ai_policy, p.ai_policy);
    assert_ne!(got.id, p.id, "导入须换新 id，避免与本机既有条目撞号");
}

/// R113：导入面必须剥离 vault_record——否则人工构造的 profiles.json 能把任意 vault 记录 id
/// 指给攻击者自己的会话，登录时经 SecretSource 取出别人的口令/私钥送往攻击者主机。
#[tokio::test]
async fn import_strips_vault_record() {
    let (_d, db) = db().await;
    let repo = ProfileRepo::new(db.pool());
    let mut p = sample();
    p.auth.vault_record = Some(4242); // 攻击者构造：指向受害者的密码记录
                                      // 跳板必须是合法的一跳（审计2 #37 起导入面逐条过 `Profile::validate`，空 host 会被拒）；
                                      // 本测试关心的是引用剥离，不是校验，别让退化夹具挡住被测行为。
    p.jump = vec![JumpHop {
        host: "bastion.internal".into(),
        port: 2022,
        username: "jump-user".into(),
        auth: AuthRef {
            vault_record: Some(4243),
            ..Default::default()
        },
        ..Default::default()
    }];
    let json = serde_json::to_string(&vec![p]).unwrap();

    let n = repo.import_json(&json).await.unwrap();
    assert_eq!(n, 1);
    let got = repo.list().await.unwrap().remove(0);
    assert_eq!(got.auth.vault_record, None, "导入不得保留 vault 记录引用");
    assert_eq!(got.jump[0].auth.vault_record, None, "跳板链每跳同剥");
}

/// S9（med）：主机密钥策略与指纹钉是**信任断言**，不能由导入文件说了算。
/// 带 `fingerprint_pinned` + 攻击者指纹的 profiles.json 一旦照单全收，
/// 该会话首连不再弹 TOFU 确认 —— 中间人当场生效且用户全程无感。
#[tokio::test]
async fn import_resets_host_key_trust() {
    let (_d, db) = db().await;
    let repo = ProfileRepo::new(db.pool());
    let mut p = sample();
    p.host_key_policy = HostKeyPolicy::FingerprintPinned;
    p.host_key_pins = vec![HostKeyPin {
        key_blob: "QVRUQUNLRVIta2V5".into(), // 攻击者自己的主机公钥
        fingerprint_sha256: "SHA256:attacker".into(),
    }];
    let json = serde_json::to_string(&vec![p]).unwrap();

    assert_eq!(repo.import_json(&json).await.unwrap(), 1);
    let got = repo.list().await.unwrap().remove(0);
    assert_eq!(
        got.host_key_policy,
        HostKeyPolicy::default(),
        "导入必须回落默认 TOFU：信任只能在本机亲眼见过后建立（S9）"
    );
    assert!(
        got.host_key_pins.is_empty(),
        "导入的指纹钉必须清空，否则等于预先信任了攻击者的主机密钥（S9）"
    );
}

/// 审计 P1（`import-keeps-download-sandbox`）：`sftp.download_sandbox` 是 spec §3.3 下载沙箱的
/// **最高优先级**输入（`app/state.rs::download_sandbox_for` 第 ① 级，压过全局设置与默认目录），
/// 也就是「远端服务器送来的字节允许落在本机哪棵目录树下」这一授权的唯一来源。
///
/// 攻击链只有三步，且每一步都不触发任何告警：
///   ① 受害者导入一份 profiles.json，其中 `download_sandbox` 指向 `~/.ssh`（或启动项、
///      Web 根目录、`.git/hooks`……）；
///   ② 受害者连上那台（攻击者控制的）主机，下载一个看着人畜无害的文件；
///   ③ 服务端把文件名报成 `authorized_keys` —— 字节精确落进 `~/.ssh/authorized_keys`。
///
/// 第 ③ 步不会被沙箱防线拦住：它压根没越界。`resolve_within` 会尽职地确认目标就在沙箱内，
/// 因为**沙箱本身已被这份文件挪到了 `~/.ssh`**。围墙没被翻过去，是围墙被搬了家 ——
/// 这正是它与 R113/S9 同源的地方：授权判断只有本机有资格作，不能随文件旅行。
///
/// 与那两条一样用**手写 JSON** 而非序列化 `Profile`：攻击者交来的就是一份文本文件，
/// 从文本进场才复现了真实威胁模型。
///
/// 同时钉住**不该剥的**：`local_dir` / `remote_dir` 必须原样保留。这条边界划在
/// 「是否构成本机授权」而非「是否属于 sftp 段」上——它们目前在 Rust 侧无任何消费者
/// （仅供 ProfileDialog 显示为下次打开面板的起始目录），既不授权写入也不断言身份。
/// 少了这半条断言，日后有人「顺手把 sftp 整段清空」也照样是绿的，不变量随之从
/// 一条可判定的规则退化成一句含混的直觉。
#[tokio::test]
async fn import_strips_download_sandbox() {
    let (_d, db) = db().await;
    let repo = ProfileRepo::new(db.pool());

    let json = r#"[{
      "id": "99999999-8888-7777-6666-555555555555",
      "name": "innocent-looking",
      "group_path": null,
      "host": "files.attacker.example",
      "port": 22,
      "username": "victim",
      "auth": {},
      "jump": [],
      "host_key_policy": "tofu",
      "host_key_pins": [],
      "env": {},
      "term": {},
      "sftp": {
        "local_dir": "D:/downloads",
        "remote_dir": "/srv/app",
        "download_sandbox": "C:/Users/victim/.ssh"
      },
      "ai_policy": {},
      "updated_at": "2026-01-01T00:00:00Z"
    }]"#;

    assert_eq!(repo.import_json(json).await.unwrap(), 1);
    let got = repo.list().await.unwrap().remove(0);
    assert_eq!(
        got.sftp.download_sandbox, None,
        "导入必须剥离下载沙箱根：写边界授权只能由本机作出，否则一份 JSON 即可把\
         远端可控的字节引到 ~/.ssh、启动项或 Web 根目录，且全程不越界、无告警"
    );
    // 剥离必须精确：偏好字段不受牵连（否则导入等于悄悄改配置）
    assert_eq!(
        got.sftp.local_dir.as_deref(),
        Some("D:/downloads"),
        "起始本地目录是纯偏好，不构成任何本机授权，须原样保留"
    );
    assert_eq!(
        got.sftp.remote_dir.as_deref(),
        Some("/srv/app"),
        "起始远端目录同为纯偏好，须原样保留"
    );
}

/// R2-1（high）：**逐跳**的信任断言与口令记录引用同样必须在导入面归零。
///
/// 这条是 R113/S9 那两条防护的安全对偶——同一种攻击，换了个字段落点：
///   · `jump[i].host_key_pins` + `jump[i].host_key_policy = "fingerprint_pinned"`：
///     跳板机是整条链上**第一个**拿到你流量的节点。钉被文件预置后，跳板链首连不再弹 TOFU，
///     界面上还写着「已按钉扎策略校验」，而校验依据正是攻击者自己提供的身份断言 ——
///     比根本不校验更糟，因为用户会据此放心。
///   · `passphrase_vault_record`：与 `vault_record` 同为「可指向本机 vault 里任意记录」的
///     整数引用，导入一份指向别人记录的 JSON 就是跨记录读取。
///
/// 故意用**手写 JSON** 而非序列化 `Profile`：攻击者交来的就是一份文本文件，
/// 从文本进场才真正复现了威胁模型（也顺带证明这些键就算不经我方类型也照样被剥）。
#[tokio::test]
async fn import_strips_per_hop_trust_and_passphrase_refs() {
    let (_d, db) = db().await;
    let repo = ProfileRepo::new(db.pool());

    let json = r#"[{
      "id": "11111111-2222-3333-4444-555555555555",
      "name": "attacker-crafted",
      "group_path": null,
      "host": "target.example.com",
      "port": 22,
      "username": "victim",
      "auth": { "vault_record": 4242, "passphrase_vault_record": 4243 },
      "jump": [{
        "host": "bastion.attacker.example",
        "port": 2022,
        "username": "victim",
        "auth": { "vault_record": 4244, "passphrase_vault_record": 4245 },
        "host_key_policy": "fingerprint_pinned",
        "host_key_pins": [{
          "key_blob": "QVRUQUNLRVIta2V5",
          "fingerprint_sha256": "SHA256:attacker-hop"
        }]
      }]
    }]"#;

    assert_eq!(repo.import_json(json).await.unwrap(), 1);
    let got = repo.list().await.unwrap().remove(0);

    // 引用类（两级）：一个都不许活着进库
    assert_eq!(got.auth.vault_record, None);
    assert_eq!(
        got.auth.passphrase_vault_record, None,
        "口令记录引用与 vault_record 同源越权，必须一并剥离"
    );
    assert_eq!(got.jump[0].auth.vault_record, None);
    assert_eq!(
        got.jump[0].auth.passphrase_vault_record, None,
        "跳板跳的口令记录引用同样必须剥离"
    );

    // 断言类（逐跳）：策略回落「未配置」→ 连接层走 TOFU；钉必须清空
    assert_eq!(
        got.jump[0].host_key_policy, None,
        "逐跳策略必须回落未配置，否则跳板链首连不再弹 TOFU（R2-1）"
    );
    assert!(
        got.jump[0].host_key_pins.is_empty(),
        "导入的逐跳指纹钉必须清空，否则等于预先信任了攻击者的跳板主机密钥（R2-1）"
    );
}

/// S17（med）：导入是**全成功或全不落**。逐行裸写在中途失败时会留下一份「导入了一半」
/// 的连接列表，用户既看不出缺了什么，也无从判断该重导还是该补录。
#[tokio::test]
async fn import_is_all_or_nothing() {
    let (_d, db) = db().await;
    // 构造一个确定性的中途失败：第二条写入时被触发器中止
    sqlx::query(
        "CREATE TRIGGER boom BEFORE INSERT ON profiles WHEN NEW.name = 'BOOM'
         BEGIN SELECT RAISE(ABORT, 'boom'); END",
    )
    .execute(db.pool())
    .await
    .unwrap();

    let repo = ProfileRepo::new(db.pool());
    let mut ok1 = sample();
    ok1.name = "first".into();
    let mut boom = sample();
    boom.name = "BOOM".into();
    let mut ok2 = sample();
    ok2.name = "third".into();
    let json = serde_json::to_string(&vec![ok1, boom, ok2]).unwrap();

    assert!(repo.import_json(&json).await.is_err(), "中途失败须整体报错");
    assert!(
        repo.list().await.unwrap().is_empty(),
        "失败的导入必须整体回滚，一条都不许留下（S17）"
    );

    // 去掉障碍后同一份数据应当全数落地——证明上面的空并非「本就写不进去」
    sqlx::query("DROP TRIGGER boom")
        .execute(db.pool())
        .await
        .unwrap();
    let mut a = sample();
    a.name = "first".into();
    let mut b = sample();
    b.name = "second".into();
    let json = serde_json::to_string(&vec![a, b]).unwrap();
    assert_eq!(repo.import_json(&json).await.unwrap(), 2);
    assert_eq!(repo.list().await.unwrap().len(), 2);
}

/// 直接落一行原始数据，模拟「库被外部工具改写」。写路径永远产不出这些值，
/// 但 sqlite 文件是用户可及的普通文件，读路径必须自己把关。
async fn insert_raw(pool: &sqlx::SqlitePool, id: &str, name: &str, port: i64, auth_blob: &str) {
    sqlx::query(
        "INSERT INTO profiles (id, name, host, port, username, auth_blob)
         VALUES (?1, ?2, 'h', ?3, 'u', ?4)",
    )
    .bind(id)
    .bind(name)
    .bind(port)
    .bind(auth_blob)
    .execute(pool)
    .await
    .unwrap();
}

/// S12（med）：`port as u16` 会静默截断 —— 库里的 65538 读出来是 2，程序照连不误，
/// 只是连到了一个用户从未指定的端口上。越界必须是错误，不是「取低 16 位」。
#[tokio::test]
async fn out_of_range_port_is_corrupt_not_truncated() {
    let (_d, db) = db().await;
    let id = Uuid::new_v4();
    insert_raw(db.pool(), &id.to_string(), "bad-port", 65538, "{}").await;

    let repo = ProfileRepo::new(db.pool());
    match repo.get(id).await {
        Err(Error::CorruptRow { reason, .. }) => {
            assert!(reason.contains("65538"), "错误须点明越界值，实际：{reason}")
        }
        Err(e) => panic!("应为 CorruptRow，实际：{e}"),
        Ok(p) => panic!("越界端口被静默截断为 {}（S12）", p.port),
    }
}

/// S18（med）：一行坏数据不得让整张连接列表凭空消失（`list()` 跳过并记 warn），
/// 而点进那一条时必须如实报「行坏了」，不是误导性的「查无此条」。
#[tokio::test]
async fn corrupt_row_is_skipped_by_list_not_fatal() {
    let (_d, db) = db().await;
    let repo = ProfileRepo::new(db.pool());
    let good = sample();
    repo.upsert(&good).await.unwrap();

    insert_raw(db.pool(), "not-a-uuid", "z-broken-uuid", 22, "{}").await;
    let bad_blob_id = Uuid::new_v4();
    insert_raw(
        db.pool(),
        &bad_blob_id.to_string(),
        "z-broken-blob",
        22,
        "{ this is not json",
    )
    .await;

    let listed = repo.list().await.unwrap();
    assert_eq!(
        listed,
        vec![good.clone()],
        "坏行应被跳过，好行照常呈现（S18）"
    );

    match repo.get(bad_blob_id).await {
        Err(Error::CorruptRow { id, reason }) => {
            assert_eq!(id, bad_blob_id.to_string());
            assert!(
                reason.contains("auth_blob"),
                "错误须点明坏在哪一列：{reason}"
            );
        }
        Err(e) => panic!("应为 CorruptRow，实际：{e}"),
        Ok(_) => panic!("非法 JSON 的 auth_blob 竟解析成功"),
    }
}

/// P2：坏行被跳过是对的，**坏行不可见**才是缺陷。
///
/// 只跳过 + 记日志，对着 GUI 的用户就只看到「我那条配置凭空消失了」——既不知道该恢复
/// 备份还是该修数据，也无从判断是不是自己误删。故 `list_with_diagnostics` 必须把坏行
/// 连同 id 与原因一并交出来；同时 `list` 的旧签名与旧行为一格不许变，免得既有调用方受累。
#[tokio::test]
async fn list_with_diagnostics_surfaces_bad_rows() {
    let (_d, db) = db().await;
    let repo = ProfileRepo::new(db.pool());
    let good = sample();
    repo.upsert(&good).await.unwrap();

    // 两类坏法各来一条：id 本身不是 uuid（诊断只能拿到原始文本），以及 blob 非法 JSON
    insert_raw(db.pool(), "not-a-uuid", "z-broken-uuid", 22, "{}").await;
    let bad_blob_id = Uuid::new_v4();
    insert_raw(
        db.pool(),
        &bad_blob_id.to_string(),
        "z-broken-blob",
        22,
        "{ this is not json",
    )
    .await;

    let (listed, bad) = repo.list_with_diagnostics().await.unwrap();
    assert_eq!(
        listed,
        vec![good.clone()],
        "好行必须照常呈现，一条坏行不得毁掉整个列表"
    );
    assert_eq!(bad.len(), 2, "两条坏行都必须被报出来，实际：{bad:?}");

    // id 非法的那条：诊断里的 id 必须是库里的**原始文本**，否则指不出是哪一行
    let by_id = bad
        .iter()
        .find(|b| b.id == "not-a-uuid")
        .expect("坏 uuid 行未被报出");
    assert!(
        by_id.error.contains("uuid"),
        "诊断须点明坏在哪：{}",
        by_id.error
    );

    let by_blob = bad
        .iter()
        .find(|b| b.id == bad_blob_id.to_string())
        .expect("坏 JSON 行未被报出");
    assert!(
        by_blob.error.contains("auth_blob"),
        "诊断须点明坏在哪一列：{}",
        by_blob.error
    );

    // 旧签名的行为不变：仍只回好行、仍不报错
    assert_eq!(repo.list().await.unwrap(), vec![good]);
}

// ── 审计2 #37：导入规模闸与逐条校验 ─────────────────────────────────────────────

/// 字节上限在**解析之前**生效。交一份既超大又不是合法 JSON 的内容，报的必须是上限错
/// （Validation），而不是 serde 的解析错——顺序一倒（先解析后查尺寸），8 MiB 上限就成了
/// 摆设：内存峰值已经打出来了，闸只是把已经发生的代价包装成一个报错。
#[tokio::test]
async fn import_rejects_oversized_input_before_parse() {
    let (_d, db) = db().await;
    let repo = ProfileRepo::new(db.pool());
    // "[ " + 超限空白：尺寸首先越界，且绝不是合法 JSON
    let content = format!("[{}", " ".repeat(limits::IMPORT_MAX_BYTES));
    match repo.import_json(&content).await {
        Err(Error::Validation(m)) => assert!(m.contains("字节上限"), "应报上限错，实际：{m}"),
        other => panic!("应为 Validation（尺寸闸，先于解析），实际：{other:?}"),
    }
    assert!(repo.list().await.unwrap().is_empty());
}

/// 条数闸在落库之前生效：超限时一条都不许落（S17 全成功或全不落）。
#[tokio::test]
async fn import_rejects_too_many_records_atomically() {
    let (_d, db) = db().await;
    let repo = ProfileRepo::new(db.pool());
    let json = serde_json::to_string(&vec![sample(); limits::IMPORT_MAX_RECORDS + 1]).unwrap();
    match repo.import_json(&json).await {
        Err(Error::Validation(m)) => assert!(m.contains("记录数"), "应报条数上限，实际：{m}"),
        other => panic!("应为 Validation（条数闸），实际：{other:?}"),
    }
    assert!(
        repo.list().await.unwrap().is_empty(),
        "超限导入不得落任何记录"
    );
}

/// 逐条校验：导入入口与保存入口共用同一套 `Profile::validate` 规则（审计2 #37），
/// 坏行带**行号**报出；且一条坏整份拒——第一条好行也不得落地。
#[tokio::test]
async fn import_validates_each_record_and_rejects_wholesale() {
    let (_d, db) = db().await;
    let repo = ProfileRepo::new(db.pool());
    let mut ok = sample();
    ok.name = "good".into();
    let mut bad = sample();
    bad.name = "x".repeat(limits::NAME_MAX + 1);
    let json = serde_json::to_string(&vec![ok, bad]).unwrap();
    match repo.import_json(&json).await {
        Err(Error::Validation(m)) => {
            assert!(m.contains("第 2 条"), "须带行号指出坏在哪一条，实际：{m}")
        }
        other => panic!("应为 Validation（逐条校验），实际：{other:?}"),
    }
    assert!(repo.list().await.unwrap().is_empty(), "一条坏整份拒（S17）");
}
