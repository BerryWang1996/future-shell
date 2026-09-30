//! 审计2 #27：恢复一份**来自另一台机器**的备份。
//!
//! 单独占一个测试二进制，因为它必须改写进程全局的 OS 凭据库条目（见
//! `tests/common/mod.rs` 的 S16 注记：同进程里换主密钥会让别的测试随机报 `Integrity`）。
//! 仓库里 `keyring_mint_race.rs` / `keyring_entry_corrupt.rs` 也是同样的理由各占一份。
//!
//! 这里要证的事只有一件，但它是整个「恢复」功能成不成立的分水岭：
//! **主密钥不在 vault.json 里，而在 OS 凭据库里。** 备份带着它自己那把主密钥
//! （封在 `master_sealed` 里，用应用密码解），本机凭据库里躺的却是本机那把。
//! 恢复若只换文件不换钥，下一次启动走静默 keyring 解锁读到的是**每一条都 Integrity**——
//! 一份看上去恢复成功、实则整个打不开的库。

mod common;
use common::{setup_mock_keyring, tempdir as mk_tempdir};
use fs_vault::{SecretKind, Store};
use zeroize::Zeroizing;

const SERVICE: &str = "future-shell";
const USER: &str = "master-key";
const USER_REPLACED: &str = "master-key-replaced";

fn tempdir() -> std::path::PathBuf {
    mk_tempdir("bkr")
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

fn keyring_get(user: &str) -> Option<String> {
    keyring::Entry::new(SERVICE, user).ok()?.get_password().ok()
}

fn keyring_set(user: &str, b64: &str) {
    keyring::Entry::new(SERVICE, user)
        .unwrap()
        .set_password(b64)
        .unwrap();
}

/// 把备份从 `src` 的备份目录搬到 `dst` 的备份目录（模拟用户拷文件过来）。
fn carry_backup(src: &std::path::Path, dst: &std::path::Path, name: &str) {
    let bdir = dst.join(fs_vault::BACKUP_DIR);
    std::fs::create_dir_all(&bdir).unwrap();
    std::fs::copy(src.join(fs_vault::BACKUP_DIR).join(name), bdir.join(name)).unwrap();
}

#[test]
fn restoring_a_foreign_backup_rewires_the_keyring_and_keeps_the_displaced_vault_openable() {
    setup_mock_keyring();

    // ── ① 「另一台机器」：用当前这把主密钥建库、存东西、导出备份 ──────────────
    let old_machine = tempdir();
    let mut away = Store::open_or_create(&old_machine, Some("pass-away")).unwrap();
    let id_away = put(&mut away, "from-the-old-laptop", b"AWAY-SECRET");
    let name = away.export_backup().unwrap();
    drop(away);
    let away_master = keyring_get(USER).expect("取材失效：凭据库里应当已有主密钥");

    // ── ② 「本机」：换一把不同的主密钥，再建一份自己的库 ──────────────────────
    // 这一步是本测试的取材：两份库的主密钥必须真的不同，否则第 ④ 步的断言恒真。
    use base64::Engine;
    let local_master = base64::engine::general_purpose::STANDARD.encode([0xA7u8; 32]);
    assert_ne!(
        local_master, away_master,
        "取材失效：两台「机器」的主密钥相同，本测试证不出任何东西"
    );
    keyring_set(USER, &local_master);

    let here = tempdir();
    let mut mine = Store::open_or_create(&here, Some("pass-here")).unwrap();
    let id_mine = put(&mut mine, "on-this-machine", b"LOCAL-SECRET");
    drop(mine);
    // 对照：换钥之前，本机的库走静默 keyring 解锁是好的
    assert_eq!(
        &*Store::unlock_with_keyring(&here)
            .unwrap()
            .get(id_mine)
            .unwrap(),
        b"LOCAL-SECRET"
    );

    // ── ③ 恢复：把老机器的备份拷过来顶替本机的库 ──────────────────────────────
    carry_backup(&old_machine, &here, &name);
    let displaced = Store::restore_backup(&here, &name, "pass-away")
        .unwrap()
        .expect("本机原有的库必须留档");

    // ── ④ 恢复之后，**静默 keyring 解锁必须直接可用** ────────────────────────
    // 不换 keyring 的实现在这里拿到的是一片 Integrity：文件是新的，钥还是旧的。
    let reopened = Store::unlock_with_keyring(&here)
        .expect("恢复后静默 keyring 解锁失败——主密钥没有跟着备份一起换过来");
    assert_eq!(
        &*reopened.get(id_away).unwrap(),
        b"AWAY-SECRET",
        "恢复后读到的不是备份里的凭据"
    );
    // 应用密码路径同样要通（两条路必须指向同一把主密钥）
    assert_eq!(
        &*Store::unlock_with_passphrase(&here, "pass-away")
            .unwrap()
            .get(id_away)
            .unwrap(),
        b"AWAY-SECRET"
    );
    assert_eq!(
        keyring_get(USER).as_deref(),
        Some(away_master.as_str()),
        "凭据库里的主密钥不是备份那一把"
    );

    // ── ⑤ 被顶替的那份库必须**仍然打得开** ───────────────────────────────────
    // 留档文件只是一堆密文，解它的钥恰好是刚被覆盖掉的那个条目。不把旧钥挪进
    // `master-key-replaced`，这份「后悔药」就是个打不开的文件——比不留更坏，
    // 因为它看上去像是安全网。
    assert!(
        displaced.exists(),
        "留档文件不存在：{}",
        displaced.display()
    );
    let stashed = keyring_get(USER_REPLACED).expect("被顶替的主密钥没有留存，留档库将永远打不开");
    assert_eq!(stashed, local_master, "留存的不是被顶替的那把主密钥");

    // 真的把它打开一次，而不是只看条目存不存在：
    // 挪个目录 + 把旧钥放回 master-key，就是用户「我后悔了」时要走的路。
    let rollback = tempdir();
    std::fs::copy(&displaced, rollback.join("vault.json")).unwrap();
    keyring_set(USER, &stashed);
    let recovered = Store::unlock_with_keyring(&rollback)
        .expect("留档库配上留存的主密钥仍然打不开——这条后悔药是假的");
    assert_eq!(
        &*recovered.get(id_mine).unwrap(),
        b"LOCAL-SECRET",
        "留档库里不是被顶替前的内容"
    );
    // 它自己的应用密码也该照旧管用
    assert_eq!(
        &*Store::unlock_with_passphrase(&rollback, "pass-here")
            .unwrap()
            .get(id_mine)
            .unwrap(),
        b"LOCAL-SECRET"
    );
}
