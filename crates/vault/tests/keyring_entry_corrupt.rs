//! OS 凭据库里的主密钥条目被外部改坏时，必须报错而不是 panic。
//!
//! 这是 S4 的同族缺口。S4（`tests/robustness.rs::corrupt_master_length_must_error_not_panic`）
//! 守的是 vault.json 里 `master_sealed` 解出长度不对的那条路；本文件守的是**另一条**：
//! 主密钥的第一来源是 OS 凭据库，而那份条目同样是外部可写的
//!（Windows 凭据管理器、`secret-tool`、Keychain 都能就地改一条通用凭据的值）。
//!
//! 两条路的后果也相同：`copy_from_slice` 对长度不符会 panic，panic 跨 IPC 会中止整个
//! core 进程 —— 把「一条坏凭据」放大成「整个应用起不来」，且用户看不到任何可行动的信息。
//!
//! 这个缺口是靠变异测试发现的：把 `decode_master_key` 的长度守卫拿掉，
//! `cargo test -p fs_vault` 全绿（37 条无一变红）—— S4 那条测试走的是 passphrase 路径的
//! 另一处守卫，从来没碰过 keyring 这一侧。
//!
//! 除「不 panic」外还钉住一条同样要紧的语义：条目**存在但坏掉**时，`open_or_create`
//! 必须失败，绝不能当成「没有条目」去铸一把新的 —— 那会让既有库里的密文当场变成死信。
//!
//! 单独成文件的原因：本测试要就地改坏那份进程内唯一的凭据条目，
//! 同一二进制里若还有别的测试，会被它们并发看见（`tests/common` 的 mock 库是进程全局的）。

mod common;

use base64::Engine;
use common::{setup_mock_keyring, tempdir as mk_tempdir};
use fs_vault::{Error, Store};
use std::collections::HashMap;

/// 把凭据库里那条主密钥条目的值换掉。
///
/// 刻意不硬编码 `(SERVICE, USER)`：那是 store 的私有实现细节，抄一份到测试里迟早漂移
///（口径同 `tests/common/mod.rs` 的说明）。改用「库里现有的唯一那条」来定位 ——
/// 它正是产品代码自己铸出来的那条，定位方式与被测对象同源。
fn overwrite_the_master_key_entry(value: &str) {
    let store = keyring_core::get_default_store().expect("mock 凭据库未装入");
    let found = store
        .search(&HashMap::from([]))
        .expect("mock 库支持 search；不支持说明装置换过了");
    assert_eq!(
        found.len(),
        1,
        "凭据库里应当只有产品代码铸的那一条主密钥；实际 {} 条，定位方式需要重写",
        found.len()
    );
    found[0].set_password(value).expect("改写条目失败");
}

#[test]
fn a_corrupt_keyring_entry_must_error_not_panic_and_never_remint() {
    setup_mock_keyring();
    let dir = mk_tempdir("keyring-corrupt");
    // 先按正常路径建一个库：此后 dir 里的密文由**当前**这把主密钥封着。
    drop(Store::open_or_create(&dir, None).expect("首启建库失败"));

    // ① 值仍是合法 base64，但解出来不是 32 字节 —— AEAD 拦不住它，只有长度守卫拦得住。
    //    没有守卫时这里是 `copy_from_slice` 的 panic，不是 Err。
    let too_short = base64::engine::general_purpose::STANDARD.encode(b"not-32-bytes");
    overwrite_the_master_key_entry(&too_short);

    match Store::unlock_with_keyring(&dir) {
        Err(Error::Crypto(m)) => assert!(m.contains("32"), "错误须点明长度期望，实际：{m}"),
        Err(e) => panic!("应为 Error::Crypto（长度不符），实际：{e}"),
        Ok(_) => panic!("畸形 keyring 条目竟解锁成功"),
    }

    // ② 同一条坏条目下，「打开或新建」也必须失败。
    //    这一条比「不 panic」更要紧：若把「坏条目」误当「无条目」，`open_or_create` 会铸一把
    //    新主密钥并写回凭据库，dir 里既有的密文从此永久不可解 —— 一次静默的数据毁灭。
    let fresh = mk_tempdir("keyring-corrupt-fresh");
    match Store::open_or_create(&fresh, None) {
        Err(Error::Crypto(m)) => assert!(m.contains("32"), "错误须点明长度期望，实际：{m}"),
        Err(e) => panic!("应为 Error::Crypto（长度不符），实际：{e}"),
        Ok(_) => panic!("坏条目被当成「没有条目」，已另铸主密钥 —— 既有密文全部报废"),
    }
    let entry_after = {
        let store = keyring_core::get_default_store().unwrap();
        store.search(&HashMap::from([])).unwrap()[0]
            .get_password()
            .unwrap()
    };
    assert_eq!(
        entry_after, too_short,
        "失败路径不得改动凭据库条目：改了就说明它已经另铸了一把"
    );

    // ③ 值根本不是 base64 —— 走的是解码分支，同样必须是 Err 而不是 panic。
    overwrite_the_master_key_entry("这不是 base64");
    match Store::unlock_with_keyring(&dir) {
        Err(Error::Crypto(_)) => {}
        Err(e) => panic!("应为 Error::Crypto（base64 解码失败），实际：{e}"),
        Ok(_) => panic!("非 base64 的 keyring 条目竟解锁成功"),
    }
}
