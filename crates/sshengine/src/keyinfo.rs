//! 私钥的**可公开元信息**（M4b「密钥/代理管理器」出口）。
//!
//! # 这个模块的全部意义在于它的返回类型
//!
//! 出口原文要求「密钥列表浏览（**仅指纹/类型/注释**）」并且「单测断言私钥材料不出现在
//! UI 数据载荷」。保证这件事最可靠的办法不是在序列化时过滤，而是**让能带出材料的路径
//! 根本不存在**：[`KeyInfo`] 只有三个 `String`，没有任何字段装得下密钥材料。
//!
//! 于是「私钥泄漏到 UI」在这条路径上不是一个需要小心避免的错误，而是一件**写不出来**的事。
//! 过滤式的做法（返回完整结构再删字段）只要有人加一个字段就会破，而且破的时候没有信号。
//!
//! # 指纹算的是公钥
//!
//! SSH 指纹的定义就是 SHA256(公钥 blob)，与私钥无关——同一把密钥无论加不加密、
//! 什么格式存储，指纹都一样。这也是它能用来核对「服务器 authorized_keys 里那把是不是这把」
//! 的原因，而那正是用户打开密钥管理器时想干的事。

use crate::hostkey::fingerprint_sha256;
use russh::keys::PublicKeyBase64;

/// 一把密钥的可公开信息。**装不下密钥材料**——见模块头。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct KeyInfo {
    /// `SHA256:...`，与 `ssh-keygen -lf` 输出的第二列同格式。
    pub fingerprint: String,
    /// 算法名（`ssh-ed25519` / `ssh-rsa` / `ecdsa-sha2-nistp256` …）。
    pub algorithm: String,
    /// 密钥自带的注释（通常是 `user@host`）。没有就是空串。
    pub comment: String,
}

/// 读不出信息时的原因。**刻意不含底层错误详情**。
///
/// 底层解析错误可能包含 PEM 片段（russh 的某些错误会带上下文），而这个值最终会显示在
/// 界面上。密钥管理器要说的只有「读不读得出、为什么」，不需要把解析器的内部状态摊开。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum KeyInfoError {
    /// 密钥带口令，没给口令就读不出指纹。
    ///
    /// **这不是错误状态，是正常状态**：加密私钥本来就该是加密的。界面上应当显示
    /// 「已加密」而不是一个红色的失败——把正常状态画成失败会让用户去「修」一件没坏的事。
    Encrypted,
    /// 根本不是一份私钥（存错了类别、文件损坏）。
    NotAKey,
}

/// 从 PEM 文本读出可公开信息。
///
/// `passphrase` 给 `None` 时，加密私钥返回 [`KeyInfoError::Encrypted`] 而不是尝试破解。
/// 密钥管理器**不应该**为了显示一个指纹去问用户要口令——那会把「看一眼列表」
/// 变成「逐个解锁」，而列表本来就该是随手能开的。
pub fn describe_private_key(pem: &str, passphrase: Option<&str>) -> Result<KeyInfo, KeyInfoError> {
    let key = match russh::keys::decode_secret_key(pem, passphrase) {
        Ok(k) => k,
        Err(russh::keys::Error::KeyIsEncrypted) => return Err(KeyInfoError::Encrypted),
        Err(_) => {
            // 给了口令却失败：可能是口令错，也可能真的不是密钥。两者都归 NotAKey——
            // 区分它们要把底层错误带出来，而那正是上面说的「可能含 PEM 片段」。
            // 密钥管理器不是解锁界面，这里的分辨力够用了。
            return Err(KeyInfoError::NotAKey);
        }
    };
    let public = key.public_key();
    Ok(KeyInfo {
        fingerprint: fingerprint_sha256(&public.public_key_bytes()),
        algorithm: public.algorithm().to_string(),
        // russh 的 comment 挂在私钥上（OpenSSH 格式里公钥部分也带一份）。
        // 注释是用户自己写的，长度无上限——截断，否则一个几 KB 的"注释"会撑坏列表。
        comment: key.comment().to_string().chars().take(120).collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 现造一把 Ed25519 私钥。
    ///
    /// 不写死一份测试用 PEM：写死的密钥材料躺在仓库里，早晚会有人以为它是真的、
    /// 或者被扫描工具报成泄漏。现造的每次都不一样，且顺带证明了这条路径对
    /// **真实的 russh 私钥**有效——而那是一份手抄的 PEM 证明不了的。
    fn make_key() -> (russh::keys::PrivateKey, String) {
        let key =
            russh::keys::PrivateKey::random(&mut rand::rng(), russh::keys::Algorithm::Ed25519)
                .expect("生成测试密钥");
        let pem = key
            .to_openssh(russh::keys::ssh_key::LineEnding::LF)
            .expect("序列化测试密钥")
            .to_string();
        (key, pem)
    }

    #[test]
    fn a_plain_key_yields_fingerprint_and_algorithm() {
        let (_, pem) = make_key();
        let info = describe_private_key(&pem, None).expect("明文密钥应当读得出");
        assert!(
            info.fingerprint.starts_with("SHA256:"),
            "{}",
            info.fingerprint
        );
        assert_eq!(info.algorithm, "ssh-ed25519");
    }

    #[test]
    fn the_fingerprint_matches_the_public_key_not_the_private_one() {
        // SSH 指纹的定义是 SHA256(公钥 blob)。这条钉住它——按私钥算的话，
        // 同一把密钥加密前后会得到两个不同的指纹，而用户拿它去比对
        // 服务器上的 authorized_keys 会永远对不上。
        let (key, pem) = make_key();
        let expected = crate::hostkey::fingerprint_sha256(&key.public_key().public_key_bytes());
        assert_eq!(
            describe_private_key(&pem, None).unwrap().fingerprint,
            expected
        );
    }

    #[test]
    fn the_same_key_always_has_the_same_fingerprint() {
        let (_, pem) = make_key();
        let a = describe_private_key(&pem, None).unwrap();
        let b = describe_private_key(&pem, None).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn different_keys_have_different_fingerprints() {
        // 反向对照：上一条若因指纹恒为某个常量而通过，这一条会红。
        let (_, a) = make_key();
        let (_, b) = make_key();
        assert_ne!(
            describe_private_key(&a, None).unwrap().fingerprint,
            describe_private_key(&b, None).unwrap().fingerprint
        );
    }

    #[test]
    fn an_encrypted_key_reports_encrypted_rather_than_failing() {
        // 加密私钥读不出指纹是**正常状态**，不是错误。画成红色失败会让用户
        // 去「修」一件没坏的事；而密钥管理器为了显示一个指纹去问口令，
        // 会把「看一眼列表」变成「逐个解锁」。
        let (key, _) = make_key();
        let pem = key
            .encrypt(&mut rand::rng(), "口令")
            .expect("加密测试密钥")
            .to_openssh(russh::keys::ssh_key::LineEnding::LF)
            .expect("序列化")
            .to_string();
        assert_eq!(
            describe_private_key(&pem, None),
            Err(KeyInfoError::Encrypted)
        );
        // 给对口令就读得出——「加密」这个判定不是因为这条路径本身就跑不通
        let info = describe_private_key(&pem, Some("口令")).expect("给了口令应当读得出");
        assert_eq!(info.algorithm, "ssh-ed25519");
    }

    #[test]
    fn garbage_is_not_a_key() {
        for junk in [
            "",
            "hello",
            "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----",
        ] {
            assert_eq!(
                describe_private_key(junk, None),
                Err(KeyInfoError::NotAKey),
                "{junk:?}"
            );
        }
    }

    /// **出口点名的那一条**：私钥材料不得出现在给 UI 的数据里。
    ///
    /// 断言的是整个结构序列化后的 JSON，而不是逐字段检查——逐字段会漏掉将来新加的字段，
    /// 而「将来新加一个字段」正是这类泄漏最常见的来路。
    #[test]
    fn no_key_material_survives_into_the_payload() {
        let (key, pem) = make_key();
        let info = describe_private_key(&pem, None).unwrap();
        let json = serde_json::to_string(&info).unwrap();

        // ① PEM 的任何一段都不许在里面
        assert!(!json.contains("PRIVATE KEY"), "{json}");
        for line in pem
            .lines()
            .filter(|l| !l.starts_with("-----") && l.len() > 16)
        {
            assert!(!json.contains(line), "PEM 的一整行出现在载荷里：{line}");
        }
        // ② 私钥字节（原始形态）不许在里面
        let raw = key
            .to_bytes()
            .map(|b| base64::Engine::encode(&base64::engine::general_purpose::STANDARD, b))
            .unwrap_or_default();
        if raw.len() > 16 {
            assert!(!json.contains(&raw), "{json}");
        }
        // ③ 反向对照：确实**有**东西在里面（否则空结构会让上面几条恒真）
        assert!(json.contains("SHA256:"), "{json}");
        assert!(json.contains("ssh-ed25519"), "{json}");
    }

    /// 结构本身装不下材料——这是比「过滤掉」更硬的保证。
    #[test]
    fn the_struct_has_exactly_three_public_string_fields() {
        // 加了第四个字段时这条会红，逼着人回答「那个字段会不会带出材料」。
        // 过滤式的做法（返回完整结构再删字段）只要有人加字段就会破，且破时没有信号。
        const SRC: &str = include_str!("keyinfo.rs");
        // 从**左花括号之后**切起：`pub struct KeyInfo {` 那一行自己也以 `pub ` 开头，
        // 不跳过它会把结构名数成一个字段（首跑实测：4 而不是 3）。
        let head = SRC.find("pub struct KeyInfo {").expect("找不到 KeyInfo");
        let start = head + SRC[head..].find('{').expect("结构没有左花括号") + 1;
        let body = &SRC[start..start + SRC[start..].find('}').expect("结构没闭合")];
        let fields: Vec<&str> = body
            .lines()
            .filter(|l| l.trim_start().starts_with("pub "))
            .collect();
        assert_eq!(fields.len(), 3, "KeyInfo 的字段变了：{fields:?}");
        for f in &fields {
            assert!(f.contains("String"), "非 String 字段：{f}");
        }
    }

    #[test]
    fn an_overlong_comment_is_truncated() {
        // 注释是用户自己写的，长度无上限。一个几 KB 的"注释"会撑坏列表布局，
        // 而它同样要经 IPC 传一遍。
        let (key, _) = make_key();
        let long = "备".repeat(500);
        let mut key = key;
        key.set_comment(long.as_str());
        let pem = key
            .to_openssh(russh::keys::ssh_key::LineEnding::LF)
            .expect("序列化")
            .to_string();
        let info = describe_private_key(&pem, None).unwrap();
        assert_eq!(info.comment.chars().count(), 120);
    }
}
