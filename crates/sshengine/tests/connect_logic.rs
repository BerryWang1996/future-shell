use fs_connmgr::{Db, HostKeyPolicy};
use fs_sshengine::connect::Connector;
use fs_sshengine::events::{HostKeyChoice, Prompt, SessionEvents};
use fs_sshengine::hostkey::{Decision, PresentedKey, TrustStore};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct RecordingEvents {
    asked: Mutex<Vec<String>>,
    answer: HostKeyChoice,
}
impl SessionEvents for RecordingEvents {
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
        prompts.iter().map(|_| "otp-123".to_string()).collect()
    }
    fn password_prompt(&self) -> String {
        String::new()
    }
    fn status(&self, _msg: &str) {}
}

fn key(blob: &str) -> PresentedKey {
    PresentedKey {
        key_type: "ssh-ed25519".into(),
        key_blob: blob.into(),
        fingerprint_sha256: fs_sshengine::hostkey::fingerprint_sha256(blob.as_bytes()),
    }
}

#[tokio::test]
async fn tofu_flow_records_after_user_accept() {
    let dir = tmpdir();
    let db = Db::open(&dir.join("fs.db")).await.unwrap();
    let trust = TrustStore::new(db.pool());
    let events = RecordingEvents {
        answer: HostKeyChoice::AcceptAndRecord,
        ..Default::default()
    };

    let accepted = Connector::resolve_host_key(
        &trust,
        &events,
        "h",
        22,
        HostKeyPolicy::Tofu,
        &[],
        &key("K1"),
    )
    .await
    .unwrap();
    assert!(accepted);
    assert_eq!(trust.lookup("h", 22).await.unwrap().len(), 1);
    assert_eq!(events.asked.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn strict_default_refuses_unknown() {
    let dir = tmpdir();
    let db = Db::open(&dir.join("fs.db")).await.unwrap();
    let trust = TrustStore::new(db.pool());
    let events = RecordingEvents {
        answer: HostKeyChoice::AcceptAndRecord,
        ..Default::default()
    };

    let accepted = Connector::resolve_host_key(
        &trust,
        &events,
        "h",
        22,
        HostKeyPolicy::Strict,
        &[],
        &key("K1"),
    )
    .await
    .unwrap();
    assert!(
        !accepted,
        "strict 模式必须默认拒绝未知密钥，不得 accept-all"
    );
    assert!(trust.lookup("h", 22).await.unwrap().is_empty());
}

#[tokio::test]
async fn changed_key_refused_without_explicit_accept() {
    let dir = tmpdir();
    let db = Db::open(&dir.join("fs.db")).await.unwrap();
    let trust = TrustStore::new(db.pool());
    let k1 = key("K1");
    trust
        .record(
            "h",
            22,
            &fs_sshengine::hostkey::StoredKey {
                key_type: k1.key_type.clone(),
                key_blob: k1.key_blob.clone(),
                fingerprint_sha256: k1.fingerprint_sha256.clone(),
                source: "tofu".into(),
            },
            "tofu",
        )
        .await
        .unwrap();

    // 用户选择拒绝 → 不记录新密钥
    let events = RecordingEvents {
        answer: HostKeyChoice::Refuse,
        ..Default::default()
    };
    let accepted = Connector::resolve_host_key(
        &trust,
        &events,
        "h",
        22,
        HostKeyPolicy::Tofu,
        &[],
        &key("K2"),
    )
    .await
    .unwrap();
    assert!(!accepted);
    assert_eq!(trust.lookup("h", 22).await.unwrap().len(), 1);

    // 用户显式接受 → replace 替换（R13）：旧键退库、新键入库，信任库恰剩一行（审计在 app 层补）
    let events2 = RecordingEvents {
        answer: HostKeyChoice::AcceptAndRecord,
        ..Default::default()
    };
    let accepted2 = Connector::resolve_host_key(
        &trust,
        &events2,
        "h",
        22,
        HostKeyPolicy::Tofu,
        &[],
        &key("K2"),
    )
    .await
    .unwrap();
    assert!(accepted2);
    let rows = trust.lookup("h", 22).await.unwrap();
    assert_eq!(
        rows.len(),
        1,
        "显式接受后旧键必须退库，每 host:port 恒至多一行有效密钥"
    );
    assert_eq!(rows[0].key_blob, "K2");

    // 已泄露旧键再次出示 → Changed（硬失败 + 弹框），而非静默 Accept——消除 fail-open MITM
    let d = fs_sshengine::hostkey::decide(HostKeyPolicy::Tofu, &[], &rows, &[], &key("K1"));
    assert!(
        matches!(d, Decision::Changed { .. }),
        "旧键已退库，再出示必须判 Changed"
    );
}

#[tokio::test]
async fn check_server_key_tofu_accept_persists_to_trust_store() {
    // 生产路径回归：AcceptAndRecord 必须经 check_server_key 入口真正落库，而非仅测试专用路径
    use russh::client::Handler;
    use russh::keys::PublicKeyBase64;

    let dir = tmpdir();
    let db = Db::open(&dir.join("fs.db")).await.unwrap();
    // 与 Task 7 fingerprint 测试同一把已知 ed25519 公钥
    let key = russh::keys::parse_public_key_base64(
        "AAAAC3NzaC1lZDI1NTE5AAAAIKVhi6rF2tuD2Oew3S77VEabPAN7FNgrrWKdlVymi5bC",
    )
    .unwrap();
    let events = Arc::new(RecordingEvents {
        answer: HostKeyChoice::AcceptAndRecord,
        ..Default::default()
    });
    let mut connector = Connector::new(
        "h".into(),
        22,
        HostKeyPolicy::Tofu,
        vec![],
        events.clone(),
        db.pool().clone(),
    );

    let accepted = connector.check_server_key(&key).await.unwrap();
    assert!(accepted, "用户在 TOFU 提示选择接受后必须放行");
    assert_eq!(events.asked.lock().unwrap().len(), 1, "首次接触应询问一次");

    // 断言落库：经真实 TrustStore 读到记录
    let trust = TrustStore::new(db.pool());
    let stored = trust.lookup("h", 22).await.unwrap();
    assert_eq!(stored.len(), 1, "接受并记录必须写入 host_keys");
    assert_eq!(stored[0].key_blob, key.public_key_base64());
    assert_eq!(stored[0].key_type, "ssh-ed25519");
    assert_eq!(
        stored[0].fingerprint_sha256,
        "SHA256:WWGldbXAkapvaUjocuvVt0JLXky5hF80UshUCFxCoLE"
    );

    // 同一密钥二次握手：命中信任库 → 直接 Accept，不再询问
    assert!(connector.check_server_key(&key).await.unwrap());
    assert_eq!(
        events.asked.lock().unwrap().len(),
        1,
        "已知密钥不得重复询问"
    );
}

#[tokio::test]
async fn accept_once_accepts_without_recording() {
    // AcceptOnce「仅本次」：放行但不写 host_keys；无持久信任必再询问；Changed 场景不覆盖不新增
    let dir = tmpdir();
    let db = Db::open(&dir.join("fs.db")).await.unwrap();
    let trust = TrustStore::new(db.pool());
    let events = RecordingEvents {
        answer: HostKeyChoice::AcceptOnce,
        ..Default::default()
    };

    // ① 空库 + AcceptOnce → 放行、不落库、询问一次
    let accepted = Connector::resolve_host_key(
        &trust,
        &events,
        "h",
        22,
        HostKeyPolicy::Tofu,
        &[],
        &key("K1"),
    )
    .await
    .unwrap();
    assert!(accepted, "AcceptOnce 必须放行本次连接");
    assert!(
        trust.lookup("h", 22).await.unwrap().is_empty(),
        "AcceptOnce 不得写 host_keys"
    );
    assert_eq!(events.asked.lock().unwrap().len(), 1);

    // ② 同 key 再次 resolve → 无持久信任必再询问
    let accepted2 = Connector::resolve_host_key(
        &trust,
        &events,
        "h",
        22,
        HostKeyPolicy::Tofu,
        &[],
        &key("K1"),
    )
    .await
    .unwrap();
    assert!(accepted2);
    assert_eq!(
        events.asked.lock().unwrap().len(),
        2,
        "AcceptOnce 无持久信任，再次连接必须重新询问"
    );

    // ③ Changed 场景：已存 K1，呈现 K2 + AcceptOnce → 放行但不覆盖不新增
    let k1 = key("K1");
    trust
        .record(
            "h",
            22,
            &fs_sshengine::hostkey::StoredKey {
                key_type: k1.key_type.clone(),
                key_blob: k1.key_blob.clone(),
                fingerprint_sha256: k1.fingerprint_sha256.clone(),
                source: "tofu".into(),
            },
            "tofu",
        )
        .await
        .unwrap();
    let accepted3 = Connector::resolve_host_key(
        &trust,
        &events,
        "h",
        22,
        HostKeyPolicy::Tofu,
        &[],
        &key("K2"),
    )
    .await
    .unwrap();
    assert!(accepted3, "Changed + AcceptOnce 必须放行本次连接");
    let rows = trust.lookup("h", 22).await.unwrap();
    assert_eq!(rows.len(), 1, "AcceptOnce 不得覆盖或新增 host_keys 记录");
    assert_eq!(rows[0].key_blob, "K1", "原记录必须原样保留");
}

/// S59（med）：S22 的跨进程撞名修复**漏掉了本文件**——此处仍是旧式「pid + 进程内计数器」并
/// 叠加 `create_dir_all`。后果在 Task 13 门禁期以**假红**现身：`check_server_key_tofu_accept_persists_to_trust_store`
/// 在全量并行 nextest 里偶发单红（位置 59/127、0.5 s 处；隔离运行与整轮重跑皆绿），
/// 机理即 S22 已实测记录的那套：Windows 回收 pid 后，本测试进程继承了上一轮**同一测试**
/// 的残留目录 `fs-conn-{pid}-0`（`%TEMP%` 实测积有数千份 `fs-*` 残留），其中 host_keys
/// 已存有本次将出示的公钥 → 首触命中信任库不弹框 → `asked.len()==0 ≠ 1`。
/// 处置与 `crates/connmgr/tests/common/mod.rs` 的 S22 分析逐字对偶：名字加纳秒、
/// 用 `create_dir`（已存在即报错）撞名重试，唯一性交给文件系统裁决而非「pid 应该不会重」。
fn tmpdir() -> std::path::PathBuf {
    static C: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let base = std::env::temp_dir();
    loop {
        let p = base.join(format!(
            "fs-conn-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("系统时钟早于 UNIX 纪元")
                .as_nanos(),
            C.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ));
        match std::fs::create_dir(&p) {
            Ok(()) => return p,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => panic!("创建测试临时目录失败 {}: {e}", p.display()),
        }
    }
}

/// S59 回归：临时目录助手绝不得把**已存在**的目录交给测试。按旧命名规则（`fs-conn-{pid}-{n}`）
/// 预铺当前 pid 的前 128 个槽位、各放一份毒文件——正是「上一轮残留 + Windows 回收 pid」
/// 在本机造成的现场。旧实现 `create_dir_all` 会原样交回（计数器起始值落在已铺范围内），
/// 新实现名字含纳秒、撞名重试，交回的目录必为空。铺 128 个令本测试与执行器无关地稳定复现
/// （`cargo test` 单进程多线程时计数器为全 binary 共享，起始值不可预知）。
#[test]
fn tmpdir_never_hands_back_an_existing_directory() {
    let base = std::env::temp_dir();
    let seeded: Vec<_> = (0..128u64)
        .map(|n| base.join(format!("fs-conn-{}-{}", std::process::id(), n)))
        .collect();
    for p in &seeded {
        std::fs::create_dir_all(p).unwrap();
        std::fs::write(p.join("poison.txt"), b"leftover from a previous run").unwrap();
    }
    for _ in 0..8 {
        let d = tmpdir();
        assert_eq!(
            std::fs::read_dir(&d).unwrap().count(),
            0,
            "助手交回了一个已存在且非空的目录：{}（S59）",
            d.display()
        );
    }
    for p in &seeded {
        let _ = std::fs::remove_dir_all(p);
    }
}
