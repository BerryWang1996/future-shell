//! S8（med）回归：密文必须与**决定它怎么被使用**的元数据不可分割。
//!
//! `vault.json` 里每条记录都是「明文元数据 + 密文」并排存放：`kind` 决定这段秘密走
//! 哪条认证路径，`label` 是用户在界面上挑凭据的唯一依据。历史实现的 AAD 只绑
//! `record_id + version`，于是这两个字段可被任意改写而 AEAD 毫无察觉：
//!
//!   · **kind 篡改**：把一条 `PrivateKey` 记录改成 `Password`，登录时私钥全文会被当作
//!     口令**明文发往远端主机**（口令认证的语义就是把明文交给对端）。攻击者只要能改一次
//!     文件，再诱导用户连一次自己的主机，就白拿一把私钥。
//!   · **label 对调**：把「生产库口令」与「测试机口令」的 label 互换，用户在
//!     ProfileDialog 里按 label 选择，就会把生产口令绑到攻击者的测试主机上。
//!
//! 威胁模型与 S2/S4/S19 一致：`vault.json` 是可被外部改写的数据文件（备份、同步盘、
//! 同机他人账户、取证镜像）。本文件断言：任何一处元数据改动都让 `get` 返回
//! `Integrity`（「这条被动过」），而不是照常吐出秘密。

mod common;
use common::{setup_mock_keyring, tempdir as mk_tempdir};
use fs_vault::{Error, SecretKind, Store};
use zeroize::Zeroizing;

fn tempdir() -> std::path::PathBuf {
    mk_tempdir("aad")
}

/// 建一个含两条记录的库，返回 (目录, 私钥记录 id, 口令记录 id)。
fn seeded() -> (std::path::PathBuf, u64, u64) {
    let dir = tempdir();
    let mut store = Store::open_or_create(&dir, None).unwrap();
    let key_id = store
        .put(
            SecretKind::PrivateKey,
            "prod-key".into(),
            Zeroizing::new(b"PRIVATE-KEY-MATERIAL".to_vec()),
        )
        .unwrap();
    let pw_id = store
        .put(
            SecretKind::Password,
            "test-box".into(),
            Zeroizing::new(b"test-pw".to_vec()),
        )
        .unwrap();
    (dir, key_id, pw_id)
}

fn read_json(dir: &std::path::Path) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(dir.join("vault.json")).unwrap()).unwrap()
}

fn write_json(dir: &std::path::Path, v: &serde_json::Value) {
    std::fs::write(dir.join("vault.json"), serde_json::to_vec(v).unwrap()).unwrap();
}

/// 非空对照：**只**把 JSON 原样解析后重新序列化写回，库必须照常打开、秘密照常取回。
///
/// 没有这一条，下面两个篡改测试就可能因为「重写 JSON 本身就把库弄坏了」而假绿 ——
/// 那样它们证明的是「serde 往返有损」，而不是「AAD 绑住了元数据」。
#[test]
fn untouched_rewrite_still_opens() {
    setup_mock_keyring();
    let (dir, key_id, pw_id) = seeded();
    let v = read_json(&dir);
    write_json(&dir, &v);

    let store = Store::open_or_create(&dir, None).unwrap();
    assert_eq!(&*store.get(key_id).unwrap(), b"PRIVATE-KEY-MATERIAL");
    assert_eq!(&*store.get(pw_id).unwrap(), b"test-pw");
}

/// 篡改 `kind`：私钥被改标成口令 —— 必须当场识破，绝不能把私钥当口令交出去。
#[test]
fn tampered_kind_is_detected() {
    setup_mock_keyring();
    let (dir, key_id, pw_id) = seeded();
    let mut v = read_json(&dir);
    assert_eq!(
        v["records"][key_id.to_string()]["kind"],
        "PrivateKey",
        "取材失效：记录的 kind 字段不在预期位置，本测试会假绿"
    );
    v["records"][key_id.to_string()]["kind"] = serde_json::json!("Password");
    write_json(&dir, &v);

    let store = Store::open_or_create(&dir, None).unwrap();
    assert!(
        matches!(store.get(key_id), Err(Error::Integrity)),
        "kind 未绑进 AAD：私钥被改标为口令后照常取出，登录时会被明文发往远端主机（S8）"
    );
    // 未被动过的记录不受牵连：报错必须精确到那一条
    assert_eq!(&*store.get(pw_id).unwrap(), b"test-pw");
}

/// 对调两条记录的 `label`：用户按 label 选凭据，换了名就是把秘密送去了别处。
#[test]
fn swapped_labels_are_detected() {
    setup_mock_keyring();
    let (dir, key_id, pw_id) = seeded();
    let mut v = read_json(&dir);
    let (a, b) = (key_id.to_string(), pw_id.to_string());
    assert_eq!(
        v["records"][&a]["label"], "prod-key",
        "取材失效：label 位置不符"
    );
    let (la, lb) = (
        v["records"][&a]["label"].clone(),
        v["records"][&b]["label"].clone(),
    );
    v["records"][&a]["label"] = lb;
    v["records"][&b]["label"] = la;
    write_json(&dir, &v);

    let store = Store::open_or_create(&dir, None).unwrap();
    for id in [key_id, pw_id] {
        assert!(
            matches!(store.get(id), Err(Error::Integrity)),
            "label 未绑进 AAD：凭据被改名后照常取出，用户会把生产口令绑到攻击者主机（S8）"
        );
    }
    // 元数据仍照常列出 —— 篡改在**取用**时被拦下，而不是让整个库打不开
    assert_eq!(store.list().len(), 2);
}
