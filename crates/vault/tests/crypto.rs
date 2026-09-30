use fs_vault::crypto::{self, Aad};
use fs_vault::Error;

/// 一条普通记录的 AAD（S8 后 AAD 含 kind/label，故测试也得据实构造）。
fn rec(record_id: u64, version: u64) -> Aad<'static> {
    Aad {
        record_id,
        version,
        kind: "password",
        label: "prod-web",
    }
}

#[test]
fn seal_open_roundtrip() {
    let key = [7u8; 32];
    let sealed = crypto::seal(&key, rec(1, 1), b"hunter2").unwrap();
    let pt = crypto::open(&key, rec(1, 1), &sealed).unwrap();
    assert_eq!(&*pt, b"hunter2");
}

#[test]
fn aad_record_id_bound() {
    let key = [7u8; 32];
    let sealed = crypto::seal(&key, rec(1, 1), b"x").unwrap();
    // 换 record_id 即 AAD 不匹配
    assert!(matches!(
        crypto::open(&key, rec(2, 1), &sealed),
        Err(Error::Integrity)
    ));
    // 回滚 version 即 AAD 不匹配
    assert!(matches!(
        crypto::open(&key, rec(1, 0), &sealed),
        Err(Error::Integrity)
    ));
}

/// S8（med）：kind 与 label 同样必须被 AAD 认证 —— 它们决定这段密文将被怎么用。
#[test]
fn aad_binds_kind_and_label() {
    let key = [7u8; 32];
    let sealed = crypto::seal(&key, rec(1, 1), b"x").unwrap();

    // ① 改 kind：private_key 记录被改成 password 后，其明文会被当口令发往远端主机
    let swapped_kind = Aad {
        kind: "private_key",
        ..rec(1, 1)
    };
    assert!(
        matches!(
            crypto::open(&key, swapped_kind, &sealed),
            Err(Error::Integrity)
        ),
        "kind 未进 AAD：篡改凭据类别不留痕，私钥可被当口令明文送出（S8）"
    );

    // ② 改 label：用户挑凭据只看 label，改了它就等于把秘密绑到了别的主机上
    let swapped_label = Aad {
        label: "test-box",
        ..rec(1, 1)
    };
    assert!(
        matches!(
            crypto::open(&key, swapped_label, &sealed),
            Err(Error::Integrity)
        ),
        "label 未进 AAD：改名不留痕，用户会把生产口令绑到攻击者主机（S8）"
    );

    // ③ 变长段必须**长度前缀**编码：拼接歧义会让 ("ab","c") 与 ("a","bc") 得到同一 AAD
    let ambiguous_a = Aad {
        kind: "ab",
        label: "c",
        ..rec(1, 1)
    };
    let ambiguous_b = Aad {
        kind: "a",
        label: "bc",
        ..rec(1, 1)
    };
    let s = crypto::seal(&key, ambiguous_a, b"x").unwrap();
    assert!(
        matches!(crypto::open(&key, ambiguous_b, &s), Err(Error::Integrity)),
        "AAD 变长段无长度前缀：kind/label 可在边界上平移，绑定形同虚设（S8）"
    );

    // ④ master key 的 AAD 与任何记录都不同域：两者的密文永不可互换
    let master = crypto::seal(&key, Aad::master(0), b"x").unwrap();
    assert!(matches!(
        crypto::open(&key, rec(0, 0), &master),
        Err(Error::Integrity)
    ));
}

#[test]
fn tampered_ciphertext_detected() {
    use base64::Engine;
    let key = [7u8; 32];
    let mut sealed = crypto::seal(&key, rec(1, 1), b"x").unwrap();
    // ciphertext 是 base64 字符串：解码 → 翻转首字节 → 重新编码写回
    let mut raw = base64::engine::general_purpose::STANDARD
        .decode(&sealed.ciphertext)
        .unwrap();
    raw[0] ^= 0xff;
    sealed.ciphertext = base64::engine::general_purpose::STANDARD.encode(&raw);
    assert!(matches!(
        crypto::open(&key, rec(1, 1), &sealed),
        Err(Error::Integrity)
    ));
}

#[test]
fn wrong_key_detected() {
    let sealed = crypto::seal(&[1u8; 32], rec(1, 1), b"x").unwrap();
    assert!(matches!(
        crypto::open(&[2u8; 32], rec(1, 1), &sealed),
        Err(Error::Integrity)
    ));
}

#[test]
fn passphrase_derive_verify() {
    let phc = crypto::derive_key_from_passphrase("correct horse", "c2FsdHlzYWx0").unwrap();
    assert!(
        phc.starts_with("$argon2id$"),
        "PHC 必须是 Argon2id 格式：{phc}"
    );
    // 正例：正确口令校验通过
    assert!(crypto::verify_passphrase("correct horse", &phc).is_ok());
    // 负例：错误口令 → Integrity
    assert!(matches!(
        crypto::verify_passphrase("wrong", &phc),
        Err(Error::Integrity)
    ));
}
