mod common;
use common::{setup_mock_keyring, tempdir as mk_tempdir};
use fs_vault::{SecretKind, Store};
use zeroize::Zeroizing;

fn tempdir() -> std::path::PathBuf {
    mk_tempdir("test")
}

#[test]
fn create_persist_reopen_roundtrip() {
    setup_mock_keyring();
    let dir = tempdir();
    let mut store = Store::open_or_create(&dir, None).unwrap();
    let id = store
        .put(
            SecretKind::Password,
            "prod-web".into(),
            Zeroizing::new(b"s3cret".to_vec()),
        )
        .unwrap();
    drop(store);

    let store2 = Store::open_or_create(&dir, None).unwrap(); // 从 mock keyring 取回同一 master key
    assert_eq!(&*store2.get(id).unwrap(), b"s3cret");
    assert_eq!(store2.list().len(), 1);
}

/// 审计2 #20：`get_typed` 返回的用途必须是**这条记录自己声明的**，且与明文同源。
///
/// 三条记录一起放进同一个库再逐条取，是为了让「kind 与 id 错位」这类实现（比如返回
/// 第一条的 kind、或返回一个写死的默认值）无处藏身——只放一条的话，写死成 Password
/// 也能绿。
#[test]
fn get_typed_reports_the_declared_kind() {
    setup_mock_keyring();
    let dir = tempdir();
    let mut store = Store::open_or_create(&dir, None).unwrap();
    let mut ids: Vec<(u64, SecretKind, &[u8])> = Vec::new();
    for k in SecretKind::ALL {
        let body: &[u8] = match k {
            SecretKind::Password => b"pw",
            SecretKind::PrivateKey => b"pem",
            SecretKind::ApiKey => b"api",
        };
        let id = store
            .put(k, format!("{k:?}"), Zeroizing::new(body.to_vec()))
            .unwrap();
        ids.push((id, k, body));
    }

    let check = |s: &Store| {
        for (id, kind, body) in &ids {
            let (got_kind, got) = s.get_typed(*id).unwrap();
            assert_eq!(got_kind, *kind, "记录 {id} 的用途被报成了 {got_kind:?}");
            assert_eq!(&*got, *body, "记录 {id} 的明文与用途不同源");
        }
    };
    check(&store);
    // 再跨一次落盘往返：kind 是写进 vault.json 的，取回来的必须还是同一个
    drop(store);
    check(&Store::open_or_create(&dir, None).unwrap());
}

/// `aad_tag` ↔ `from_aad_tag` 是一对互逆映射，且**认不出的串一律 None**（审计2 #20）。
///
/// 后半条才是重点：IPC 入口原先写的是 `_ => ApiKey`，于是一个拼错的类别串会被静默
/// 变成一个具体用途。这里逐条钉住那些「像但不是」的写法都必须落空。
#[test]
fn kind_tags_round_trip_and_never_fall_back() {
    for k in SecretKind::ALL {
        assert_eq!(SecretKind::from_aad_tag(k.aad_tag()), Some(k));
    }
    for bad in [
        "",
        "Password",
        "PASSWORD",
        "privatekey",
        "private-key",
        "apikey",
        "api key",
        "ssh_key",
        "password ",
        " password",
    ] {
        assert_eq!(
            SecretKind::from_aad_tag(bad),
            None,
            "{bad:?} 不是任何一个已知类别，绝不能被认成某个用途"
        );
    }
}

#[test]
fn passphrase_fallback_unlock() {
    setup_mock_keyring();
    let dir = tempdir();
    let mut store = Store::open_or_create(&dir, Some("app-pass")).unwrap();
    let id = store
        .put(
            SecretKind::ApiKey,
            "claude".into(),
            Zeroizing::new(b"k".to_vec()),
        )
        .unwrap();
    drop(store);

    let store2 = Store::unlock_with_passphrase(&dir, "app-pass").unwrap();
    assert_eq!(&*store2.get(id).unwrap(), b"k");
    assert!(Store::unlock_with_passphrase(&dir, "bad").is_err());
}

#[test]
fn delete_then_get_is_notfound() {
    setup_mock_keyring();
    let dir = tempdir();
    let mut store = Store::open_or_create(&dir, None).unwrap();
    let id = store
        .put(
            SecretKind::PrivateKey,
            "k".into(),
            Zeroizing::new(b"x".to_vec()),
        )
        .unwrap();
    store.delete(id).unwrap();
    assert!(matches!(store.get(id), Err(fs_vault::Error::NotFound(_))));
}

/// S21（med）：删除必须**落盘**，而不只是从内存里摘掉。
///
/// 上一个测试只看内存视图：`delete` 里的 `save()` 一旦被删掉（重构、改错、误合并），
/// 它照样全绿 —— 而用户看到的是「删了的口令下次启动又回来了」，且那份密文自始至终
/// 躺在磁盘上，与「我把它删了」这个心智模型直接矛盾（这正是凭据管理里最不能出的错）。
/// 故本测试跨越进程边界：重新打开必须查无此条，且文件里连 label 残渣都不许留。
#[test]
fn delete_is_persisted_to_disk() {
    setup_mock_keyring();
    let dir = tempdir();
    let mut store = Store::open_or_create(&dir, None).unwrap();
    let keep = store
        .put(
            SecretKind::Password,
            "keep-me".into(),
            Zeroizing::new(b"KEEP".to_vec()),
        )
        .unwrap();
    let gone = store
        .put(
            SecretKind::Password,
            "delete-me-forever".into(),
            Zeroizing::new(b"MUST-NOT-SURVIVE".to_vec()),
        )
        .unwrap();
    store.delete(gone).unwrap();
    // 重复删除 = 查无此条（幂等地报错，不是静默成功）
    assert!(matches!(
        store.delete(gone),
        Err(fs_vault::Error::NotFound(_))
    ));
    drop(store);

    // ① 重新打开：删掉的不得复活，留下的必须完好
    let store2 = Store::open_or_create(&dir, None).unwrap();
    assert!(
        matches!(store2.get(gone), Err(fs_vault::Error::NotFound(_))),
        "删除未落盘：重启后被删记录复活（S21）"
    );
    assert_eq!(store2.list().len(), 1, "删除未落盘：库里仍是两条（S21）");
    assert_eq!(&*store2.get(keep).unwrap(), b"KEEP", "删除误伤了其他记录");

    // ② 磁盘上连痕迹都不许留：label 是明文字段，残留即证明那条记录还在文件里
    let raw = std::fs::read(dir.join("vault.json")).unwrap();
    let text = String::from_utf8_lossy(&raw);
    assert!(
        !text.contains("delete-me-forever"),
        "vault.json 里仍留着被删记录的明文 label ⇒ 密文也还在（S21）"
    );
    assert!(text.contains("keep-me"), "取材失效：留存记录也不在文件里");
}

/// R112：keyring-only 库（建库时未设应用密码 → 无 passphrase PHC）必须能在二次启动时静默解锁；
/// 且 `unlock_with_keyring` 在库文件缺失时只能 Locked，绝不建库/铸新钥（那是 open_or_create 的语义）。
#[test]
fn keyring_only_vault_reopens_without_passphrase() {
    setup_mock_keyring();
    let dir = tempdir();
    let mut store = Store::open_or_create(&dir, None).unwrap(); // 首启建库：仅 keyring，无应用密码
    let id = store
        .put(
            SecretKind::Password,
            "only-keyring".into(),
            Zeroizing::new(b"pw".to_vec()),
        )
        .unwrap();
    drop(store);

    // ① 二次启动：走 keyring 静默解锁，不需要口令
    let store2 = Store::unlock_with_keyring(&dir).unwrap();
    assert_eq!(&*store2.get(id).unwrap(), b"pw");

    // ② 口令路径对该库必然 Locked——正是 R112 现场：前端若只给 unlock 口令弹窗即永久打不开
    assert!(matches!(
        Store::unlock_with_passphrase(&dir, "anything"),
        Err(fs_vault::Error::Locked)
    ));

    // ③ 无库文件 → Locked，且**不得建库**（open_or_create 在此会建库并铸新钥，正是必须避开的语义）
    let empty = tempdir();
    assert!(matches!(
        Store::unlock_with_keyring(&empty),
        Err(fs_vault::Error::Locked)
    ));
    assert!(!empty.join("vault.json").exists(), "解锁失败不得建库");
}

/// S16（med）：mock 凭据库的装载必须是**每进程一次**，重入不得把主密钥换掉。
///
/// 这是历史实现（每个测试函数各 `set_default_store` 一次）的确定性复现：
/// 第二次装载会换上一个空库，`open_or_create` 因 `NoEntry` 铸出**新**主密钥，
/// 于是同一目录里先前写入的密文再也解不开 —— 一条与被测代码毫无关系的 `Integrity`。
/// 在 `cargo test`（同进程多线程）下，这就是随机发生在任意两个测试之间的事故。
#[test]
fn mock_keyring_install_is_idempotent_across_reentry() {
    setup_mock_keyring();
    let dir = tempdir();
    let mut store = Store::open_or_create(&dir, None).unwrap();
    let id = store
        .put(
            SecretKind::Password,
            "survives-reentry".into(),
            Zeroizing::new(b"STILL-HERE".to_vec()),
        )
        .unwrap();
    drop(store);

    // 模拟同进程内另一个测试也调用了装载入口
    setup_mock_keyring();

    let store2 = Store::open_or_create(&dir, None).unwrap();
    assert_eq!(
        &*store2.get(id).unwrap(),
        b"STILL-HERE",
        "重复装载 mock 凭据库导致主密钥被换掉：既有密文全部解不开（S16）"
    );
}

/// M1 出口「Vault：…锁定后 secret 不可读」的**磁盘那一半**。
///
/// 「锁定」在本产品里是把内存里的 `Store` 丢掉（`app` 侧 `*state.vault = None`），
/// 于是锁定之后世上还剩的东西只有 `vault.json` 一个文件。这条出口标准要成立，
/// 前提就是那个文件里没有任何一段秘密的明文——否则「锁定」只是把 UI 关掉，
/// 谁 `cat` 一下都能读。
///
/// 此前**无人证**：`delete_is_persisted_to_disk` 断言过明文 **label** 的存在与消失
/// （那是刻意明文存放的元数据），却从没有一条用例检查过 **secret 本身**是否也在里面。
/// 两者只差一个字段名，而错的那种写法（把明文和密文并排写进 JSON）一样能让
/// 上面所有取放用例全绿。
#[test]
fn a_locked_vault_leaves_no_plaintext_secret_on_disk() {
    setup_mock_keyring();
    let dir = tempdir();
    let mut store = Store::open_or_create(&dir, None).unwrap();

    // 三种 kind 各放一条：AAD/kind 分支若各有各的落盘路径，只测一种会漏掉另两种。
    // 明文取值刻意选成不可能被 base64/JSON 转义拆散的纯 ASCII 长串——短串（如 "ok"）
    // 会在密文的 base64 里偶然出现，让断言产生假红。
    let secrets: [(SecretKind, &str, &[u8]); 3] = [
        (
            SecretKind::Password,
            "prod-web",
            b"PLAINTEXT-PASSWORD-MUST-NOT-APPEAR",
        ),
        (
            SecretKind::PrivateKey,
            "prod-key",
            b"PLAINTEXT-PRIVATEKEY-MUST-NOT-APPEAR",
        ),
        (
            SecretKind::ApiKey,
            "prod-api",
            b"PLAINTEXT-APIKEY-MUST-NOT-APPEAR",
        ),
    ];
    let mut ids = Vec::new();
    for (kind, label, plain) in secrets {
        ids.push(
            store
                .put(kind, label.into(), Zeroizing::new(plain.to_vec()))
                .unwrap(),
        );
    }
    drop(store); // ← 这就是「锁定」：内存里的 Store 没了，只剩磁盘

    let raw = std::fs::read(dir.join("vault.json")).unwrap();
    let text = String::from_utf8_lossy(&raw);

    // 取材自检：文件里确实有这三条记录（否则下面的「找不到明文」是因为压根没写进去）
    for (_, label, _) in secrets {
        assert!(
            text.contains(label),
            "取材失效：label {label} 都不在 vault.json 里，本用例什么也没检验"
        );
    }

    for (kind, label, plain) in secrets {
        let needle = std::str::from_utf8(plain).unwrap();
        assert!(
            !text.contains(needle),
            "vault.json 里能直接读到 {kind:?} 记录「{label}」的明文 ⇒ 「锁定后 secret 不可读」不成立：\
             锁定只是关掉了 UI，任何人 cat 一下就能拿走"
        );
        // 字节层再查一遍：明文若以非 UTF-8 边界落盘，from_utf8_lossy 会把它替换掉从而漏检
        assert!(
            !raw.windows(plain.len()).any(|w| w == plain),
            "vault.json 的原始字节里含 {kind:?} 记录「{label}」的明文"
        );
    }

    // 反向对照：拿着钥匙重新解锁，三条都必须原样取得回来——
    // 否则「读不到明文」可以靠「根本没存」达成。
    let store2 = Store::open_or_create(&dir, None).unwrap();
    for (i, (_, label, plain)) in secrets.iter().enumerate() {
        assert_eq!(
            &*store2.get(ids[i]).unwrap(),
            *plain,
            "解锁后取不回「{label}」——上面的「无明文」成了空头承诺"
        );
    }
}
