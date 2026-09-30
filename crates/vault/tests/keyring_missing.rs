//! keyring 条目不在时，`unlock_with_keyring` 必须报 `KeyringMissing`，不能再笼统地报 `Locked`
//!（路线图 4c「keyring 条目缺失时解锁框要说真话」，2026-09-02）。
//!
//! 此前 NoEntry 与「库文件不在」共用 `Error::Locked`，app 层只能给出一句
//! 「解锁失败（密码错误或 keyring/文件异常）」——用户唯一能做的是再试一次空口令。
//! 而对**没设应用口令**的库，keyring 条目一旦不在（换机 / 重装 / 凭据管理器被清理），
//! 主密钥世上再无第二份，试一万次也开不了。这件事必须能被前端**区分出来**并直说，
//! 区分的前提是 core 侧先把它单列成一个变体。
//!
//! 同时钉住与 `keyring_entry_corrupt.rs` 同款的不变式：失败路径**不铸新钥、不回写文件**。
//! 「无条目」是 `open_or_create` 铸新钥的触发条件，本路径若误走那条分支，既有密文当场报废。
//!
//! 单独成文件的原因同 `keyring_entry_corrupt.rs`：要删掉进程内唯一那条凭据条目，
//! `tests/common` 的 mock 库是进程全局的，同一二进制里的其它测试会被并发殃及。

mod common;

use common::{setup_mock_keyring, tempdir as mk_tempdir};
use fs_vault::{Error, Store};
use std::collections::HashMap;

/// 删掉凭据库里那条主密钥条目。定位方式同 `keyring_entry_corrupt.rs`：
/// 不硬编码 `(SERVICE, USER)`（那是 store 的私有细节，抄一份迟早漂移），
/// 取「库里现有的唯一那条」——它正是产品代码自己铸出来的。
fn delete_the_master_key_entry() {
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
    found[0].delete_credential().expect("删除条目失败");
}

/// 凭据库里那条主密钥此刻读出来是什么：`Ok(值)` / `Err(NoEntry)`。
///
/// 不按 `search` 的条数判「删干净了没」：mock 库的 `delete_credential` 只把条目状态置成
/// NoEntry，条目壳仍留在 search 结果里（实测 1 条）。真实 OS 凭据库删了就是没了——两种语义下
/// 「读不到密码」都成立，所以以 `get_password` 为准；条目壳也没有时同样视为 NoEntry。
fn master_key_read() -> Result<String, keyring_core::Error> {
    let store = keyring_core::get_default_store().expect("mock 凭据库未装入");
    let found = store
        .search(&HashMap::from([]))
        .expect("mock 库支持 search");
    match found.first() {
        Some(entry) => entry.get_password(),
        None => Err(keyring_core::Error::NoEntry),
    }
}

#[test]
fn a_missing_keyring_entry_is_keyring_missing_not_locked_and_never_remints() {
    setup_mock_keyring();
    let dir = mk_tempdir("keyring-missing");
    // 正常路径建库：dir 里的密文由当前这把主密钥封着，且库**没设应用口令**——
    // 正是「条目一丢就永久不可恢复」的那类库。
    drop(Store::open_or_create(&dir, None).expect("首启建库失败"));
    let file_before = std::fs::read(dir.join("vault.json")).expect("库文件应已落盘");
    assert!(
        !fs_vault::file_needs_passphrase(&dir).expect("读库文件头失败"),
        "本用例的前提是库没设应用口令"
    );

    delete_the_master_key_entry();
    assert!(
        matches!(master_key_read(), Err(keyring_core::Error::NoEntry)),
        "删除后应读不到主密钥条目"
    );

    // ① 变体必须是 KeyringMissing——前端据此分岔「不可恢复」与「再试口令」两套文案。
    match Store::unlock_with_keyring(&dir) {
        Err(Error::KeyringMissing) => {}
        Err(e) => panic!("应为 Error::KeyringMissing，实际：{e}"),
        Ok(_) => panic!("keyring 条目已删竟解锁成功"),
    }

    // ② 失败路径不得铸新钥、不得回写：文件逐字节不变，凭据库里仍读不到主密钥。
    //    误走 `open_or_create` 那条「无条目即铸新钥并 save()」的分支，这两条中至少一条会变。
    assert_eq!(
        std::fs::read(dir.join("vault.json")).unwrap(),
        file_before,
        "失败路径改动了库文件：说明它已另铸主密钥并回写，既有密文全部报废"
    );
    assert!(
        matches!(master_key_read(), Err(keyring_core::Error::NoEntry)),
        "失败路径往凭据库里写了主密钥：已另铸一把"
    );

    // ③ 「无库文件」仍是 Locked：两种缺失不得混为一谈。app 靠 Locked 决定是否显示初始化向导，
    //    靠 KeyringMissing 决定是否直说「不可恢复」——合并任一侧都会让另一侧的文案说错话。
    let no_file = mk_tempdir("keyring-missing-nofile");
    match Store::unlock_with_keyring(&no_file) {
        Err(Error::Locked) => {}
        Err(e) => panic!("无库文件应为 Error::Locked，实际：{e}"),
        Ok(_) => panic!("无库文件竟解锁成功"),
    }

    // ④ Display 前缀是前端 VaultDialog 的判据子串（跨 IPC 只剩字符串），两侧同批改。
    assert!(
        Error::KeyringMissing
            .to_string()
            .starts_with("keyring entry missing"),
        "Display 文案变了：前端 VaultDialog 的判据子串要同批改，实际：{}",
        Error::KeyringMissing
    );
}
