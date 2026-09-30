//! 保险库健壮性回归：拒绝把「打开」变成「毁库」或「改密」。
//!
//! 覆盖 M0 对抗复审确认的四条缺陷：
//!   · S2（high×3）`open_or_create` 把一切 `fs::read` 失败吞成空库后无条件 `save()` 覆写；
//!   · S4（high）  `unlock_with_passphrase` 对解出的主密钥长度不设防，畸形文件直接 panic；
//!   · S19（med）  vault.json 无格式版本，旧程序打开新库会在回写时静默截断未知字段；
//!   · S20（low）  `open_or_create(dir, Some(pw))` 无条件重铸口令 —— 一次笔误即静默改密。
//!
//! 以及 P1-17（落盘耐久性与内存回滚）：save 失败后内存态必须退回调用前，
//! 且原子替换不得留下临时残骸 —— 见文件末尾一节。

mod common;
use base64::Engine;
use common::{setup_mock_keyring, tempdir as mk_tempdir};
use fs_vault::{crypto, Error, SecretKind, Store};
use std::path::Path;
use zeroize::Zeroizing;

fn tempdir() -> std::path::PathBuf {
    mk_tempdir("robust")
}

// ── 「文件在、但读不动」的可移植构造 ──────────────────────────────────────
// 这是 S2 唯一有意义的现场：文件不存在走的是合法新建路径，只有「存在却读失败」
// 才能暴露「吞掉错误 → 当空库 → 覆写」这条毁库链。故必须真刀真枪地夺掉读权限。

#[cfg(unix)]
fn make_unreadable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o000)).is_ok()
}

#[cfg(unix)]
fn restore_readable(p: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o600));
}

#[cfg(windows)]
fn make_unreadable(p: &Path) -> bool {
    let Ok(user) = std::env::var("USERNAME") else {
        return false;
    };
    std::process::Command::new("icacls")
        .arg(p)
        .arg("/deny")
        .arg(format!("{user}:(RD)"))
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[cfg(windows)]
fn restore_readable(p: &Path) {
    if let Ok(user) = std::env::var("USERNAME") {
        let _ = std::process::Command::new("icacls")
            .arg(p)
            .arg("/remove:d")
            .arg(user)
            .output();
    }
}

/// S2（high×3）：读不动既有库时必须报错，且**一个字节都不许改**。
#[test]
fn unreadable_vault_must_error_and_survive_untouched() {
    setup_mock_keyring();
    let dir = tempdir();
    let mut store = Store::open_or_create(&dir, Some("app-pass")).unwrap();
    let id = store
        .put(
            SecretKind::Password,
            "prod-web".into(),
            Zeroizing::new(b"IRREPLACEABLE".to_vec()),
        )
        .unwrap();
    drop(store);

    let path = dir.join("vault.json");
    let before = std::fs::read(&path).unwrap();
    assert!(!before.is_empty());

    // 权限模型因运行账户而异（root / 管理员 + SeBackupPrivilege 可无视 ACL）：
    // 构造不成就明确跳过，绝不改断言口径假装通过。
    if !make_unreadable(&path) {
        eprintln!("跳过 S2 回归：本环境无法构造不可读文件");
        return;
    }
    if std::fs::read(&path).is_ok() {
        restore_readable(&path);
        eprintln!("跳过 S2 回归：拒绝读权限未生效（疑似特权账户运行）");
        return;
    }

    let result = Store::open_or_create(&dir, Some("app-pass"));
    restore_readable(&path); // 先恢复，保证后续断言与清理不受影响

    assert!(
        result.is_err(),
        "读失败被吞成空库：随后的 save() 会把用户全部凭据静默覆写销毁（S2）"
    );
    assert_eq!(
        before,
        std::fs::read(&path).unwrap(),
        "vault.json 在一次**失败**的打开中被改写（S2）"
    );
    // 密文完好 ⇒ 秘密仍取得回来
    let ok = Store::unlock_with_passphrase(&dir, "app-pass").unwrap();
    assert_eq!(&*ok.get(id).unwrap(), b"IRREPLACEABLE");
}

/// S2 的另一半：文件**不存在**仍须照常建库（别把修复做成因噎废食）。
#[test]
fn missing_vault_still_creates_fresh_store() {
    setup_mock_keyring();
    let dir = tempdir();
    assert!(!dir.join("vault.json").exists());
    let store = Store::open_or_create(&dir, Some("app-pass")).unwrap();
    assert!(store.list().is_empty());
    assert!(dir.join("vault.json").exists(), "首启必须落盘建库");
}

/// S4（high）：master_sealed 解出非 32 字节时报错，绝不 panic。
///
/// vault.json 是可被外部改写的数据文件；panic 跨 IPC 会中止整个 core 进程，
/// 把「一份坏文件」放大成「整个应用起不来」。
#[test]
fn corrupt_master_length_must_error_not_panic() {
    setup_mock_keyring();
    let dir = tempdir();
    drop(Store::open_or_create(&dir, Some("app-pass")).unwrap());

    let path = dir.join("vault.json");
    let mut v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    // 用真 KEK 重新封一段**长度不对**的明文：AEAD 校验会通过，只有长度守卫拦得住它
    let salt = base64::engine::general_purpose::STANDARD
        .decode(v["kdf_salt"].as_str().unwrap())
        .unwrap();
    let kek = crypto::derive_kek("app-pass", &salt).unwrap();
    let bogus = crypto::seal(&kek, crypto::Aad::master(0), b"too-short").unwrap();
    v["master_sealed"] = serde_json::json!([0u64, serde_json::to_value(&bogus).unwrap()]);
    std::fs::write(&path, serde_json::to_vec(&v).unwrap()).unwrap();

    match Store::unlock_with_passphrase(&dir, "app-pass") {
        Err(Error::Crypto(m)) => assert!(m.contains("32"), "错误须点明长度期望，实际：{m}"),
        Err(e) => panic!("应为 Error::Crypto（长度不符），实际：{e}"),
        Ok(_) => panic!("畸形 master_sealed 竟解锁成功"),
    }
}

/// S19（med）：更高格式版本的库一律拒绝打开 —— 打开即意味着回写时截断新版字段。
#[test]
fn future_format_version_is_refused_without_rewrite() {
    setup_mock_keyring();
    let dir = tempdir();
    drop(Store::open_or_create(&dir, Some("app-pass")).unwrap());

    let path = dir.join("vault.json");
    let mut v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(v["format"], 2, "当前库须带 format 版本号");
    v["format"] = serde_json::json!(999);
    v["future_field_from_a_newer_build"] = serde_json::json!("must survive");
    let doctored = serde_json::to_vec(&v).unwrap();
    std::fs::write(&path, &doctored).unwrap();

    assert!(
        matches!(Store::open_or_create(&dir, None), Err(Error::Storage(_))),
        "更高 format 必须拒绝打开（S19）"
    );
    assert!(matches!(
        Store::unlock_with_passphrase(&dir, "app-pass"),
        Err(Error::Storage(_))
    ));
    assert!(matches!(
        Store::unlock_with_keyring(&dir),
        Err(Error::Storage(_))
    ));
    assert_eq!(
        doctored,
        std::fs::read(&path).unwrap(),
        "拒绝打开的同时必须原样保留文件，新版字段一个都不能丢（S19）"
    );
}

/// S20（low）：带口令打开既有库 = 校验，**不是**改密。笔误必须报错，而非把库口令改成笔误。
#[test]
fn open_with_wrong_passphrase_must_not_silently_rekey() {
    setup_mock_keyring();
    let dir = tempdir();
    let mut store = Store::open_or_create(&dir, Some("correct-horse")).unwrap();
    let id = store
        .put(
            SecretKind::ApiKey,
            "k".into(),
            Zeroizing::new(b"v".to_vec()),
        )
        .unwrap();
    drop(store);

    assert!(
        matches!(
            Store::open_or_create(&dir, Some("correct-hoarse")),
            Err(Error::Integrity)
        ),
        "口令不符仍放行 ⇒ 库口令被静默改成了笔误，用户下次用真口令必然打不开（S20）"
    );
    // 原口令依旧有效，记录完好
    let ok = Store::unlock_with_passphrase(&dir, "correct-horse").unwrap();
    assert_eq!(&*ok.get(id).unwrap(), b"v");
    // 正确口令打开则照常放行
    drop(Store::open_or_create(&dir, Some("correct-horse")).unwrap());
}

/// 改密走专用入口，且必须验现口令。
#[test]
fn change_passphrase_requires_current_and_rotates() {
    setup_mock_keyring();
    let dir = tempdir();
    let mut store = Store::open_or_create(&dir, Some("old-pass")).unwrap();
    let id = store
        .put(
            SecretKind::PrivateKey,
            "id_ed25519".into(),
            Zeroizing::new(b"PRIVKEY".to_vec()),
        )
        .unwrap();

    assert!(matches!(
        store.change_passphrase(Some("not-the-old-one"), "new-pass"),
        Err(Error::Integrity)
    ));
    store
        .change_passphrase(Some("old-pass"), "new-pass")
        .unwrap();
    drop(store);

    assert!(Store::unlock_with_passphrase(&dir, "old-pass").is_err());
    let ok = Store::unlock_with_passphrase(&dir, "new-pass").unwrap();
    assert_eq!(&*ok.get(id).unwrap(), b"PRIVKEY", "改密不得动到已有记录");
}

/// S14（med）配套：公开 API 里承载秘密的返回类型必须是 `Zeroizing`。
/// 若有人把 `get` 的返回改成裸 `Vec<u8>`，本文件**编译失败** —— 这是安全 Rust 里
/// 唯一可靠的零化断言方式（读已释放内存是 UB，无法运行期检查）。
#[test]
fn secret_bearing_apis_return_zeroizing_types() {
    fn must_self_zeroize<T: zeroize::Zeroize>(_: &Zeroizing<T>) {}

    setup_mock_keyring();
    let dir = tempdir();
    let mut store = Store::open_or_create(&dir, Some("app-pass")).unwrap();
    let id = store
        .put(
            SecretKind::Password,
            "p".into(),
            Zeroizing::new(b"s".to_vec()),
        )
        .unwrap();
    must_self_zeroize(&store.get(id).unwrap());
    must_self_zeroize(&crypto::derive_kek("app-pass", &[7u8; 16]).unwrap());
}

/// S8 配套（格式版本下限）：旧口径（AAD 只绑 id+version）写出的库必须在**打开**这一步
/// 就被明确拒绝，并告诉用户该怎么办 —— 而不是一路放行到解密时抛一个
/// 「完整性校验失败」，把「格式旧了」误报成「文件被人动过」。
#[test]
fn legacy_format_is_refused_with_actionable_error() {
    setup_mock_keyring();
    let dir = tempdir();
    drop(Store::open_or_create(&dir, Some("app-pass")).unwrap());

    let path = dir.join("vault.json");
    let mut v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    v["format"] = serde_json::json!(1); // S8 修复前的格式
    let doctored = serde_json::to_vec(&v).unwrap();
    std::fs::write(&path, &doctored).unwrap();

    for r in [
        Store::open_or_create(&dir, None).map(|_| ()),
        Store::unlock_with_passphrase(&dir, "app-pass").map(|_| ()),
        Store::unlock_with_keyring(&dir).map(|_| ()),
    ] {
        match r {
            Err(Error::Storage(m)) => assert!(
                m.contains("重新初始化"),
                "错误须给出可执行的下一步，实际：{m}"
            ),
            Err(e) => panic!("应为 Error::Storage（格式过旧），实际：{e}"),
            Ok(()) => panic!("旧格式库竟被放行打开"),
        }
    }
    assert_eq!(
        doctored,
        std::fs::read(&path).unwrap(),
        "拒绝打开的同时必须原样保留文件"
    );
}

// ── P1-17：save 失败后的内存回滚 ─────────────────────────────────────────────
//
// 注入手段是「夺掉数据目录的**建文件**权限」：`save()` 的第一步就是在库文件旁边建
// 临时文件，建不出来就是一次干净的 save 失败 —— 且此刻磁盘上的 vault.json 一个字节
// 都没动，正是「内存必须退回去与磁盘对齐」这条不变式最纯粹的现场。
// （不选「只读文件」：原子替换根本不写目标文件本身，把 vault.json 设成只读拦不住 rename。）

#[cfg(unix)]
fn deny_dir_writes(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o500)).is_ok()
}

#[cfg(unix)]
fn restore_dir_writes(p: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o700));
}

#[cfg(windows)]
fn deny_dir_writes(p: &Path) -> bool {
    let Ok(user) = std::env::var("USERNAME") else {
        return false;
    };
    // WD = 建文件，AD = 建子目录。deny 优先于任何 allow，故对普通账户立即生效。
    std::process::Command::new("icacls")
        .arg(p)
        .arg("/deny")
        .arg(format!("{user}:(WD,AD)"))
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[cfg(windows)]
fn restore_dir_writes(p: &Path) {
    if let Ok(user) = std::env::var("USERNAME") {
        let _ = std::process::Command::new("icacls")
            .arg(p)
            .arg("/remove:d")
            .arg(user)
            .output();
    }
}

/// 夺权后**实测**一次：管理员 / root 无视 ACL，构造不成就得跳过而不是改断言口径
///（与本文件 S2 用例同一套纪律）。
fn writes_really_denied(dir: &Path) -> bool {
    let probe = dir.join("p1_17_probe.tmp");
    match std::fs::File::create(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            false
        }
        Err(_) => true,
    }
}

/// P1-17（core）：`delete` 的 save 失败后，`get` 必须仍返回**旧值**。
///
/// 这是整条修复的判定点。历史实现先 `records.remove` 再 `save()`，save 一失败：
/// 内存里那条已经没了（后续 `get` 报 NotFound、界面上凭据消失），磁盘上却原封不动 ——
/// 重启后这条「已删掉」的凭据原样复活。用户以为撤销掉的授权其实一直有效，
/// 而全程没有任何一处报错能提示他去复查。
#[test]
fn failed_save_on_delete_must_roll_back_and_keep_secret_readable() {
    setup_mock_keyring();
    let dir = tempdir();
    let mut store = Store::open_or_create(&dir, None).unwrap();
    let id = store
        .put(
            SecretKind::Password,
            "prod-db".into(),
            Zeroizing::new(b"KEEP-ME".to_vec()),
        )
        .unwrap();
    let disk_before = std::fs::read(dir.join("vault.json")).unwrap();

    if !deny_dir_writes(&dir) || !writes_really_denied(&dir) {
        restore_dir_writes(&dir);
        eprintln!("跳过 P1-17 回归：本环境无法夺掉目录写权限（疑似特权账户运行）");
        return;
    }
    let err = store.delete(id);
    restore_dir_writes(&dir); // 先恢复，保证后续断言与临时目录清理不受影响

    assert!(
        matches!(err, Err(Error::Storage(_))),
        "落盘失败必须如实报错，实际：{err:?}"
    );
    assert_eq!(
        &*store
            .get(id)
            .expect("save 失败却把记录从内存里抹了：内存与磁盘就此分叉（P1-17）"),
        b"KEEP-ME"
    );
    assert_eq!(store.list().len(), 1, "list 也必须回到删除前");
    assert_eq!(
        disk_before,
        std::fs::read(dir.join("vault.json")).unwrap(),
        "一次失败的 delete 不该动到磁盘"
    );
}

/// P1-17：`put` 的 save 失败后，新记录不得留在内存里，`next_id` 也必须一并退回。
///
/// next_id 不回滚看着无害，实则会在库里凿出永久性的 id 空洞：一次网络盘抖动
/// 就让 id 单调跳号，而 id 是 AAD 的一部分、也是 profile 里 `vault_record` 的外键 ——
/// 「内存里的下一个 id」与「磁盘上的下一个 id」必须始终是同一个数。
#[test]
fn failed_save_on_put_must_roll_back_record_and_next_id() {
    setup_mock_keyring();
    let dir = tempdir();
    let mut store = Store::open_or_create(&dir, None).unwrap();
    let first = store
        .put(
            SecretKind::ApiKey,
            "claude".into(),
            Zeroizing::new(b"k1".to_vec()),
        )
        .unwrap();

    if !deny_dir_writes(&dir) || !writes_really_denied(&dir) {
        restore_dir_writes(&dir);
        eprintln!("跳过 P1-17 回归：本环境无法夺掉目录写权限（疑似特权账户运行）");
        return;
    }
    let err = store.put(
        SecretKind::Password,
        "ghost".into(),
        Zeroizing::new(b"never-persisted".to_vec()),
    );
    restore_dir_writes(&dir);

    assert!(
        matches!(err, Err(Error::Storage(_))),
        "落盘失败必须如实报错，实际：{err:?}"
    );
    assert_eq!(
        store.list().len(),
        1,
        "save 失败的记录仍挂在内存里：界面显示已保存，重启即凭空消失（P1-17）"
    );

    // 恢复权限后再存一条：它必须拿到那个被回滚掉的 id，证明 next_id 确实退回去了
    let second = store
        .put(
            SecretKind::Password,
            "real".into(),
            Zeroizing::new(b"k2".to_vec()),
        )
        .unwrap();
    assert_eq!(
        second,
        first + 1,
        "next_id 未回滚，id 被那次失败的写永久吃掉"
    );

    drop(store);
    let reopened = Store::open_or_create(&dir, None).unwrap();
    assert_eq!(reopened.list().len(), 2, "磁盘上不该有那条 ghost 记录");
    assert_eq!(&*reopened.get(second).unwrap(), b"k2");
}

/// P1-17：原子替换不得留下临时残骸，且临时名必须**唯一**（不再是固定的 vault.json.tmp）。
///
/// 固定名在两个实例并发保存时会互相把对方写了一半的内容 rename 上去，得到一个
/// JSON 都解析不了的库；残骸本身也是一份完整密文库（含 PHC 与盐），留在盘上等于
/// 白送一份可离线爆破的副本。
#[test]
fn atomic_save_leaves_no_temp_debris() {
    setup_mock_keyring();
    let dir = tempdir();
    let mut store = Store::open_or_create(&dir, None).unwrap();
    for i in 0..5 {
        store
            .put(
                SecretKind::Password,
                format!("p{i}"),
                Zeroizing::new(b"v".to_vec()),
            )
            .unwrap();
    }
    let leftovers: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".tmp"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "保存后仍有临时残骸（每份都是完整密文库）：{leftovers:?}"
    );
    assert!(
        !dir.join("vault.json.tmp").exists(),
        "仍在用固定临时名：并发保存会互相覆盖半成品（P1-17）"
    );
}

/// P1-17：跨进程锁不得把**本进程**挡在门外。
///
/// 闸门按数据目录共享、且与进程等长；若实现成「每个 Store 各抢一次、drop 才放」，
/// app 侧 `vault_init`（旧 Store 还活着时先建新 Store）与「解锁后再打开」这类
/// 完全正常的用法会当场拿不到锁 —— 修一个并发问题换来一个必然复现的死锁，得不偿失。
#[test]
fn in_process_reopen_shares_the_same_lock() {
    setup_mock_keyring();
    let dir = tempdir();
    let mut first = Store::open_or_create(&dir, Some("app-pass")).unwrap();
    let id = first
        .put(
            SecretKind::Password,
            "p".into(),
            Zeroizing::new(b"v".to_vec()),
        )
        .unwrap();
    assert!(
        dir.join("vault.lock").exists(),
        "跨进程锁文件必须落在数据目录里"
    );

    // 三条打开路径都要能在 first 仍存活时成功
    let second = Store::open_or_create(&dir, Some("app-pass")).unwrap();
    let third = Store::unlock_with_passphrase(&dir, "app-pass").unwrap();
    let fourth = Store::unlock_with_keyring(&dir).unwrap();
    for s in [&second, &third, &fourth] {
        assert_eq!(&*s.get(id).unwrap(), b"v");
    }
    // 后开的实例写同一个库须成功（save 闸门按文件共享，不是各锁各的）
    let mut second = second;
    let q = second
        .put(
            SecretKind::ApiKey,
            "q".into(),
            Zeroizing::new(b"w".to_vec()),
        )
        .unwrap();

    // 这里原本是 `first.delete(id).unwrap()`，注释写着「两个实例交替写同一个库仍须各自成功」。
    // 它确实成功了 —— 代价是把 `q` 连同 `next_id` 一起抹掉：`first` 的快照停在 `q` 出现之前，
    // 而 `save()` 是**整文件重写**。这个测试当时是绿的，磁盘上剩下的却是
    // `{"next_id": 1, "records": {}}`：两条凭据全没了，还倒退了计数器（下一条新记录会复用
    // 一个曾经发出过的 id，旧密文的 AAD 从此对不上）。断言写成「各自成功」，
    // 等于把这条静默毁数据的路径钉成了产品行为。
    //
    // 现在的口径：闸门保证不交错，世代号保证不覆盖。过期实例的写当场被拒（fail-closed），
    // 用户拿到一条能读懂的错误而不是一个悄悄变空的保险库。
    let err = first
        .delete(id)
        .expect_err("基于过期快照的写必须被拒绝，否则它会把另一实例刚存的记录整份覆盖掉");
    match err {
        Error::Storage(m) if m.contains("另一个实例") => {}
        other => panic!("拒绝理由不对：{other:?}"),
    }

    // 被拒之后：磁盘上两条记录都在，且 `first` 的内存态已回滚（delete 未生效）
    assert_eq!(
        &*first.get(id).unwrap(),
        b"v",
        "保存失败后内存必须退回调用前"
    );
    let disk = Store::unlock_with_keyring(&dir).unwrap();
    assert_eq!(&*disk.get(id).unwrap(), b"v", "先写入的记录不该被抹掉");
    assert_eq!(&*disk.get(q).unwrap(), b"w", "后写入的记录不该被抹掉");
}
