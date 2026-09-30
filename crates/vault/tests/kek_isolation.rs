//! R117 回归（critical）：`vault.json` 明文内容不得泄露保护 master key 的 KEK。
//!
//! 历史缺陷现场：`arm_passphrase` 用 `derive_key_from_passphrase` 生成 PHC 校验串落盘，
//! 又用 `derive_raw` 从**该 PHC 自身的盐** + 同一套 Argon2 参数重算 32 字节密钥当 KEK 去封
//! `master_sealed`。同口令 + 同盐 + 同参 ⇒ Argon2 输出逐字节相同 —— 即 PHC 末段的 hash 字段
//! **就是** KEK 本身，而它与被它加密的 `master_sealed` 并排明文躺在同一个 JSON 里。
//! 攻击者只要能读到 `vault.json`（备份、同步盘、误传、他人账户下的取证镜像），
//! 无需口令、无需 OS keyring，即可解出 master key 并解密全部凭据（总设计 §3.2 彻底失效）。
//!
//! 本测试以「只拿到 vault.json 的攻击者」为模型：文件里任何一个能解出 32 字节的字段，
//! 都必须打不开 `master_sealed`。

mod common;
use base64::Engine;
use common::{setup_mock_keyring, tempdir as mk_tempdir};
use fs_vault::{crypto, SecretKind, Store};
use zeroize::Zeroizing;

fn tempdir() -> std::path::PathBuf {
    mk_tempdir("kek")
}

/// 把 JSON 里所有「可解成 32 字节」的串收进候选集：整串按标准/无填充 base64 解，
/// 含 `$` 的（PHC）再按 `$` 切段逐段解 —— PHC 的盐段与 hash 段都会落进来。
fn collect_candidates(v: &serde_json::Value, out: &mut Vec<Vec<u8>>) {
    match v {
        serde_json::Value::String(s) => {
            let mut try_decode = |t: &str| {
                // 两个引擎同为 GeneralPurpose 具体类型：base64::Engine 有泛型方法、不是 dyn 兼容 trait
                for eng in [
                    &base64::engine::general_purpose::STANDARD,
                    &base64::engine::general_purpose::STANDARD_NO_PAD,
                ] {
                    if let Ok(b) = eng.decode(t) {
                        if b.len() == 32 {
                            out.push(b);
                        }
                    }
                }
            };
            try_decode(s);
            if s.contains('$') {
                for seg in s.split('$') {
                    try_decode(seg);
                }
            }
        }
        serde_json::Value::Array(a) => a.iter().for_each(|x| collect_candidates(x, out)),
        serde_json::Value::Object(m) => m.values().for_each(|x| collect_candidates(x, out)),
        _ => {}
    }
}

#[test]
fn vault_file_alone_must_not_yield_the_master_key() {
    setup_mock_keyring();
    let dir = tempdir();
    let mut store = Store::open_or_create(&dir, Some("app-pass")).unwrap();
    let id = store
        .put(
            SecretKind::Password,
            "prod-web".into(),
            Zeroizing::new(b"TOP-SECRET-PASSWORD".to_vec()),
        )
        .unwrap();
    drop(store);

    let raw = std::fs::read(dir.join("vault.json")).unwrap();
    let v: serde_json::Value = serde_json::from_slice(&raw).unwrap();
    let (ver, sealed): (u64, crypto::Sealed) =
        serde_json::from_value(v["master_sealed"].clone()).expect("master_sealed 必须在场");

    // ① 精确复现历史攻击：PHC 末段（Argon2 输出）当 KEK 试开 master_sealed。
    //    这一段恒存在且恒为 32 字节，故本测试不会因取材落空而假绿。
    let phc = v["passphrase_phc"]
        .as_str()
        .expect("passphrase_phc 必须在场");
    let tail = phc.rsplit('$').next().unwrap();
    let hash = base64::engine::general_purpose::STANDARD_NO_PAD
        .decode(tail)
        .expect("PHC 末段须为 B64 编码的 Argon2 输出");
    assert_eq!(hash.len(), 32, "Argon2 默认输出长度须为 32 字节");
    let mut kek = [0u8; 32];
    kek.copy_from_slice(&hash);
    assert!(
        crypto::open(&kek, crypto::Aad::master(ver), &sealed).is_err(),
        "PHC 末段可直接解开 master_sealed —— 校验串与 KEK 未做域分离，读到 vault.json 即可解密全部秘密（R117）"
    );

    // ② 泛化扫荡：文件里任何可解成 32 字节的字段都不得是 KEK（挡同类缺陷再引入）
    let mut candidates = Vec::new();
    collect_candidates(&v, &mut candidates);
    assert!(
        !candidates.is_empty(),
        "候选集为空说明取材逻辑失效，本测试会假绿"
    );
    for c in &candidates {
        let mut key = [0u8; 32];
        key.copy_from_slice(c);
        assert!(
            crypto::open(&key, crypto::Aad::master(ver), &sealed).is_err(),
            "vault.json 中存在可直接解开 master_sealed 的明文字段（R117）"
        );
    }

    // ③ 正路仍须通：有口令才解得开，且秘密完好
    let ok = Store::unlock_with_passphrase(&dir, "app-pass").unwrap();
    assert_eq!(&*ok.get(id).unwrap(), b"TOP-SECRET-PASSWORD");
}

/// KEK 派生盐必须与校验用 PHC 的内嵌盐相互独立 —— 同盐同参即退化回 R117。
#[test]
fn kek_salt_is_independent_of_verifier_salt() {
    setup_mock_keyring();
    let dir = tempdir();
    let store = Store::open_or_create(&dir, Some("app-pass")).unwrap();
    drop(store);

    let raw = std::fs::read(dir.join("vault.json")).unwrap();
    let v: serde_json::Value = serde_json::from_slice(&raw).unwrap();
    let phc = v["passphrase_phc"]
        .as_str()
        .expect("passphrase_phc 必须在场");
    let verifier_salt = phc.split('$').nth(4).expect("PHC 盐段");
    let kdf_salt = v["kdf_salt"]
        .as_str()
        .expect("kdf_salt 必须在场：KEK 须由独立盐派生，而非复用 PHC 的盐");

    let kdf_salt_bin = base64::engine::general_purpose::STANDARD
        .decode(kdf_salt)
        .expect("kdf_salt 须为标准 base64");
    assert!(kdf_salt_bin.len() >= 16, "KDF 盐至少 16 字节");
    let verifier_salt_bin = base64::engine::general_purpose::STANDARD_NO_PAD
        .decode(verifier_salt)
        .expect("PHC 盐段须为 B64");
    assert_ne!(
        kdf_salt_bin, verifier_salt_bin,
        "KEK 盐与校验盐相同 ⇒ 两次 Argon2 输出逐字节相同 ⇒ PHC 末段即 KEK（R117）"
    );
}
