//! 审计2 #27 / 发布门槛第 5 条：保险库的备份与恢复。
//!
//! 这一组测试守的是同一件事——**「恢复」不许把用户现有的凭据弄丢**。
//! 恢复是本程序里唯一一个会顶替整份凭据库的操作，它的失败模式不是「报个错」，
//! 而是「用户点完确认，两份库都打不开了」。所以下面每一条都在问同一个问题：
//! 这一步失败时，用户手里还剩什么？

mod common;
use common::{setup_mock_keyring, tempdir as mk_tempdir};
use fs_vault::{SecretKind, Store};
use std::path::Path;
use zeroize::Zeroizing;

fn tempdir() -> std::path::PathBuf {
    mk_tempdir("backup")
}

fn put(store: &mut Store, label: &str, body: &[u8]) -> u64 {
    store
        .put(
            SecretKind::Password,
            label.to_string(),
            Zeroizing::new(body.to_vec()),
        )
        .unwrap()
}

fn read(path: &Path) -> Vec<u8> {
    std::fs::read(path).unwrap_or_else(|e| panic!("读 {} 失败：{e}", path.display()))
}

/// 目录里所有 `vault.json.replaced-*` 留档文件。
fn displaced_files(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut v: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("vault.json.replaced-"))
        })
        .collect();
    v.sort();
    v
}

// ── 导出 ────────────────────────────────────────────────────────────────────

/// 没设应用密码就**不许导出**，而且一个字节都不许落盘。
///
/// 这不是多此一举的洁癖：主密钥此刻只在本机 OS 凭据库里，拷走的 vault.json 在任何
/// 其他机器上都是一堆永远解不开的密文。导出这样一份文件，唯一的作用是让用户以为
/// 自己已经备份过了——等他真的需要它时（换机器、重装、凭据库被清），才发现手里
/// 拿的是块砖。给假的安全感比明说「还不能备份」危险得多。
#[test]
fn export_is_refused_while_the_vault_has_no_passphrase() {
    setup_mock_keyring();
    let dir = tempdir();
    let mut store = Store::open_or_create(&dir, None).unwrap(); // 仅 keyring，无应用密码
    put(&mut store, "only-keyring", b"pw");

    let err = store
        .export_backup()
        .expect_err("无应用密码时导出必须被拒绝——那样的备份换台机器就永远打不开");
    let msg = err.to_string();
    assert!(
        msg.contains("应用密码"),
        "错误信息必须点明「先设应用密码」这条出路，否则用户只会以为备份功能坏了：{msg}"
    );
    assert!(
        !dir.join(fs_vault::BACKUP_DIR).exists(),
        "被拒绝的导出连备份目录都不该建出来"
    );
    assert!(
        Store::list_backups(&dir).unwrap().is_empty(),
        "被拒绝的导出不得留下任何文件"
    );
}

/// 连点两次「导出」必须得到两份备份，绝不能第二次把第一次覆盖掉。
///
/// 秒级时间戳同一秒内会撞名，而「备份」恰恰是用户手里最后一根绳子——
/// 覆盖式命名意味着一次误操作（导出后才发现刚删错了东西）就把上一份好的抹掉了。
#[test]
fn a_second_export_never_overwrites_the_first() {
    setup_mock_keyring();
    let dir = tempdir();
    let mut store = Store::open_or_create(&dir, Some("pw")).unwrap();
    put(&mut store, "one", b"AAA");
    let first = store.export_backup().unwrap();
    put(&mut store, "two", b"BBB");
    let second = store.export_backup().unwrap();

    assert_ne!(first, second, "同一秒内的第二次导出撞名并覆盖了第一份");
    let names: Vec<String> = Store::list_backups(&dir)
        .unwrap()
        .into_iter()
        .map(|b| b.name)
        .collect();
    assert!(
        names.contains(&first) && names.contains(&second),
        "两份备份都必须在列表里，实际为 {names:?}"
    );
    // 第一份必须停在导出那一刻的内容上（不含后加的 two）
    let dir_b = dir.join(fs_vault::BACKUP_DIR);
    let a = String::from_utf8(read(&dir_b.join(&first))).unwrap();
    let b = String::from_utf8(read(&dir_b.join(&second))).unwrap();
    assert!(!a.contains("two"), "第一份备份被后来的改动污染了");
    assert!(b.contains("two"), "第二份备份没有包含导出时的最新记录");
}

/// 列表只报本程序自己导出的文件。
///
/// 备份目录在用户的数据目录下，他完全可能往里放别的东西（拖进去的笔记、
/// 别的程序的临时文件）。把它们混进「可恢复的备份」列表里，就是在邀请用户
/// 拿一个不相干的文件去顶替自己的凭据库。
#[test]
fn listing_ignores_files_that_are_not_our_backups() {
    setup_mock_keyring();
    let dir = tempdir();
    let mut store = Store::open_or_create(&dir, Some("pw")).unwrap();
    put(&mut store, "x", b"X");
    let real = store.export_backup().unwrap();

    let bdir = dir.join(fs_vault::BACKUP_DIR);
    for stray in [
        "notes.txt",
        "vault-backup-abc.json", // 时间戳段不是数字
        "vault-backup-1.txt",    // 后缀不对
        "vault.json",            // 名字像库本体，但不是备份
        ".hidden.json",
    ] {
        std::fs::write(bdir.join(stray), b"{}").unwrap();
    }
    // 目录也不得混入
    std::fs::create_dir(bdir.join("vault-backup-999.json.d")).unwrap();

    let names: Vec<String> = Store::list_backups(&dir)
        .unwrap()
        .into_iter()
        .map(|b| b.name)
        .collect();
    assert_eq!(names, vec![real], "列表混进了非备份文件：{names:?}");
}

/// 还没备份过 ≠ 出错。目录不存在时给空表，UI 才好显示「暂无备份」。
#[test]
fn listing_an_absent_backup_dir_is_empty_not_an_error() {
    setup_mock_keyring();
    let dir = tempdir();
    assert!(Store::list_backups(&dir).unwrap().is_empty());
}

// ── 恢复：先验通，再动盘 ────────────────────────────────────────────────────

/// 备份能把删掉的凭据带回来——这是整个功能存在的理由，先把它钉住。
#[test]
fn restore_brings_back_what_was_deleted() {
    setup_mock_keyring();
    let dir = tempdir();
    let mut store = Store::open_or_create(&dir, Some("pw")).unwrap();
    let id = put(&mut store, "prod-db", b"THE-PASSWORD");
    let name = store.export_backup().unwrap();

    store.delete(id).unwrap();
    assert!(matches!(store.get(id), Err(fs_vault::Error::NotFound(_))));
    drop(store);

    let displaced = Store::restore_backup(&dir, &name, "pw")
        .unwrap()
        .expect("被顶替的库必须留档，否则「恢复错了」就没有退路");
    assert!(
        displaced.exists(),
        "留档路径不存在：{}",
        displaced.display()
    );

    let back = Store::unlock_with_passphrase(&dir, "pw").unwrap();
    assert_eq!(
        &*back.get(id).unwrap(),
        b"THE-PASSWORD",
        "恢复后取不回备份时的凭据"
    );
}

/// 口令不对时，**现有的库必须一个字节都没被动过**。
///
/// 这是本组测试的核心。历史上这类实现的常见写法是「先换文件，再尝试打开」——
/// 于是口令记错一次，用户就同时失去了现有的库（已被顶替）和备份（打不开）。
/// 所以断言的不是「报了错」，而是磁盘上的字节完全相同、且没有产生留档文件
///（有留档就说明替换动作已经开始了）。
#[test]
fn a_wrong_passphrase_leaves_the_live_vault_byte_identical() {
    setup_mock_keyring();
    let dir = tempdir();
    let mut store = Store::open_or_create(&dir, Some("right")).unwrap();
    let id = put(&mut store, "live", b"LIVE");
    let name = store.export_backup().unwrap();
    // 备份之后再改一次，让「现有库」与「备份」内容确实不同
    let id2 = put(&mut store, "added-later", b"LATER");
    drop(store);

    let path = dir.join("vault.json");
    let before = read(&path);

    let err = Store::restore_backup(&dir, &name, "WRONG").expect_err("错口令必须拒绝恢复");
    assert!(
        !matches!(err, fs_vault::Error::NotFound(_)),
        "错口令不该被报成「查无此条」：{err}"
    );
    assert_eq!(read(&path), before, "错口令的恢复动了现有保险库的字节");
    assert!(
        displaced_files(&dir).is_empty(),
        "错口令的恢复产生了留档文件，说明替换已经开始动手了"
    );

    // 现有库仍完全可用：两条记录都在
    let still = Store::unlock_with_passphrase(&dir, "right").unwrap();
    assert_eq!(&*still.get(id).unwrap(), b"LIVE");
    assert_eq!(&*still.get(id2).unwrap(), b"LATER");
}

/// 备份文件损坏时同理：解析不过就不许动现有的库。
///
/// 与错口令分开测，是因为它们踩的是**两道不同的闸门**（`parse_vault` 与口令验证）。
/// 只测一条的话，另一条被挪到替换之后也照样全绿。
#[test]
fn a_corrupt_backup_leaves_the_live_vault_byte_identical() {
    setup_mock_keyring();
    let dir = tempdir();
    let mut store = Store::open_or_create(&dir, Some("pw")).unwrap();
    let id = put(&mut store, "live", b"LIVE");
    let name = store.export_backup().unwrap();
    drop(store);

    // 把备份内容换成「合法 JSON，但不是保险库」
    std::fs::write(
        dir.join(fs_vault::BACKUP_DIR).join(&name),
        br#"{"format":2,"next_id":1,"records":{}}"#,
    )
    .unwrap();
    // ↑ 这份没有 passphrase_phc / master_sealed：解析得过，但口令路径必然打不开
    let path = dir.join("vault.json");
    let before = read(&path);
    assert!(
        Store::restore_backup(&dir, &name, "pw").is_err(),
        "一份解不开的备份被当作可恢复接受了"
    );
    assert_eq!(read(&path), before, "无效备份动了现有保险库的字节");
    assert!(displaced_files(&dir).is_empty(), "无效备份产生了留档文件");

    let still = Store::unlock_with_passphrase(&dir, "pw").unwrap();
    assert_eq!(&*still.get(id).unwrap(), b"LIVE");

    // 语法都不成立的 JSON 走同一条路
    std::fs::write(
        dir.join(fs_vault::BACKUP_DIR).join(&name),
        b"not json at all",
    )
    .unwrap();
    assert!(Store::restore_backup(&dir, &name, "pw").is_err());
    assert_eq!(read(&path), before);
}

/// 文件名来自渲染进程，必须走白名单——否则「恢复备份」就成了「拿任意路径的文件
/// 当保险库读」，而这条命令跑在特权侧（app 进程）。
///
/// 逐个断言而不是只测一两个：路径穿越的写法在不同平台上长得不一样
///（Windows 认反斜杠与盘符、Unix 认斜杠），漏掉哪一种，哪一种就是入口。
#[test]
fn restore_rejects_anything_that_is_not_one_of_our_backup_names() {
    setup_mock_keyring();
    let dir = tempdir();
    let mut store = Store::open_or_create(&dir, Some("pw")).unwrap();
    put(&mut store, "live", b"LIVE");
    let real = store.export_backup().unwrap();
    drop(store);

    let path = dir.join("vault.json");
    let before = read(&path);

    for name in [
        "../vault.json",
        "..\\vault.json",
        "../../etc/passwd",
        "..\\..\\Users\\x\\.ssh\\id_rsa",
        "/etc/passwd",
        "C:\\Windows\\win.ini",
        "vault-backup-../1.json",
        "vault-backup-1.json/../../vault.json",
        "vault.json",
        "",
        ".",
        "..",
        "vault-backup-.json",   // 时间戳段为空
        "vault-backup-1.json ", // 末尾空格：Windows 上会被文件系统吃掉
        "VAULT-BACKUP-1.JSON",  // 大小写不同即不是我们导出的名字
    ] {
        let err = Store::restore_backup(&dir, name, "pw")
            .expect_err(&format!("非法备份名被接受了：{name:?}"));
        assert!(
            err.to_string().contains("备份文件名不合法"),
            "{name:?} 被拒绝的理由不是文件名校验，而是 {err}——说明它已经走到了读文件那一步"
        );
    }
    assert_eq!(read(&path), before, "非法备份名的恢复动了现有保险库");
    assert!(displaced_files(&dir).is_empty());

    // 对照：合法名字必须能过，否则上面全是假绿
    Store::restore_backup(&dir, &real, "pw").expect("合法备份名被误拒，本测试的其余断言均无意义");
}

/// 恢复之后，**在世的 `Store` 实例必须失效**（fail-closed）。
///
/// 这些实例手里攥着恢复前的内存快照。若它们还能保存，用户的下一次「存个口令」
/// 就会把刚刚恢复回来的整份库原样覆盖掉——恢复白做了，而且没有任何报错。
#[test]
fn live_handles_go_stale_after_a_restore() {
    setup_mock_keyring();
    let dir = tempdir();
    let mut store = Store::open_or_create(&dir, Some("pw")).unwrap();
    let id = put(&mut store, "keep", b"KEEP");
    let name = store.export_backup().unwrap();

    // 备份之后这个实例又改了东西；它的快照现在与磁盘不同
    let doomed = put(&mut store, "doomed", b"DOOMED");
    Store::restore_backup(&dir, &name, "pw").unwrap();

    let err = store
        .put(
            SecretKind::Password,
            "after-restore".into(),
            Zeroizing::new(b"X".to_vec()),
        )
        .expect_err("恢复之后旧实例仍能保存——它会把恢复回来的库整份覆盖掉");
    assert!(
        err.to_string().contains("已被"),
        "拒绝的理由必须说清是「库被改写了」，否则用户无从判断该怎么办：{err}"
    );

    let fresh = Store::unlock_with_passphrase(&dir, "pw").unwrap();
    assert_eq!(&*fresh.get(id).unwrap(), b"KEEP");
    assert!(
        matches!(fresh.get(doomed), Err(fs_vault::Error::NotFound(_))),
        "磁盘上的库不是备份的内容——旧实例的快照把它覆盖了"
    );
}

/// 恢复到一个**还没有库**的空目录（换机器后的主用场景）。
/// 此时没有东西可留档，也不该因为「找不到要顶替的文件」而失败。
#[test]
fn restore_into_a_fresh_data_dir_works_and_has_nothing_to_displace() {
    setup_mock_keyring();
    let src = tempdir();
    let mut store = Store::open_or_create(&src, Some("pw")).unwrap();
    let id = put(&mut store, "carried", b"CARRIED");
    let name = store.export_backup().unwrap();
    drop(store);

    let dst = tempdir();
    let bdir = dst.join(fs_vault::BACKUP_DIR);
    std::fs::create_dir_all(&bdir).unwrap();
    std::fs::copy(src.join(fs_vault::BACKUP_DIR).join(&name), bdir.join(&name)).unwrap();
    assert!(
        !dst.join("vault.json").exists(),
        "取材失效：目标目录本就该是空的"
    );

    assert_eq!(
        Store::restore_backup(&dst, &name, "pw").unwrap(),
        None,
        "空目录里什么都没被顶替，却报了一个留档路径——UI 会照着它让用户去找一个不存在的文件"
    );
    assert!(
        displaced_files(&dst).is_empty(),
        "空目录里没有库可顶替，不该凭空造出留档文件"
    );
    let s = Store::unlock_with_passphrase(&dst, "pw").unwrap();
    assert_eq!(&*s.get(id).unwrap(), b"CARRIED");
}

/// `has_passphrase` 必须如实反映这份库能不能离机恢复（UI 靠它决定要不要警告用户）。
#[test]
fn has_passphrase_tracks_the_only_off_machine_recovery_path() {
    setup_mock_keyring();
    let dir = tempdir();
    let mut store = Store::open_or_create(&dir, None).unwrap();
    assert!(!store.has_passphrase(), "仅 keyring 的库不该自称有应用密码");
    store.change_passphrase(None, "set-now").unwrap();
    assert!(store.has_passphrase(), "刚设过应用密码却报没有");
    // 设完之后导出就该放行了
    store.export_backup().unwrap();
    drop(store);
    assert!(
        Store::unlock_with_passphrase(&dir, "set-now")
            .unwrap()
            .has_passphrase(),
        "重新打开后应用密码的存在性丢失了"
    );
}
