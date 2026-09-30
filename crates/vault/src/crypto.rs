use crate::Error;
use aes_gcm::aead::{Aead, Payload};
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::{Algorithm, Argon2, Params, Version};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sealed {
    pub nonce: [u8; 12],
    /// base64 密文
    pub ciphertext: String,
}

/// AAD 域前缀，**含版本号**。日后若再调整绑定口径，改这里即可让新旧密文互不通用
/// （而不是新旧口径悄悄互认，那正是绑定形同虚设的样子）。
const AAD_DOMAIN: &[u8] = b"future-shell/vault/aad/v2";

/// 与密文一同被认证的元数据（AEAD 的 AAD）。
///
/// S8（med）：AAD 历史上只绑 `record_id + version`，`kind` 与 `label` 毫无保护 ——
/// 而这两个字段恰恰**决定这段密文将被怎么用**，且与密文并排明文躺在 vault.json 里：
///   · `kind` 决定认证路径：把一条 `private_key` 记录改成 `password`，登录时私钥全文
///     会被当口令**明文发往远端主机**（口令认证本就是把明文交给对端）；
///   · `label` 是用户在 ProfileDialog 里挑凭据的**唯一依据**：把生产库口令的 label 改成
///     「测试机口令」，用户就会把生产口令绑到攻击者的测试主机上，连一次即拱手送出。
/// AEAD 只保护它认证过的东西 —— 不绑进 AAD，改这两个字段不留任何痕迹。绑上之后，
/// 任何一处改动都让 `open` 返回 `Integrity`，即「这条记录被动过」。
///
/// **契约**：`label` 已进 AAD，故日后新增改名接口时必须**用新 label 重新 seal**，
/// 不能只改元数据 —— 否则改完就再也解不开了。
///
/// 用结构体而非平铺参数：`kind` 与 `label` 同为 `&str`，平铺时相邻两参调换编译器不会吭声，
/// 而调换后 seal/open 两边同错、依旧自洽，是一条测试极难发现的静默降级（S5 同类）。
#[derive(Debug, Clone, Copy)]
pub struct Aad<'a> {
    pub record_id: u64,
    pub version: u64,
    /// 记录类别的稳定标签（见 `SecretKind::aad_tag`）。**永不改名**：改名 = 既有密文全部作废。
    pub kind: &'a str,
    pub label: &'a str,
}

impl Aad<'static> {
    /// master key 自封装专用 AAD：`kind` 走独立域 `master-key`，与任何记录都不同域，
    /// 于是记录密文与 `master_sealed` 永远不可互换（换了就 `Integrity`）。
    pub fn master(version: u64) -> Self {
        Aad {
            record_id: 0,
            version,
            kind: "master-key",
            label: "",
        }
    }
}

impl Aad<'_> {
    /// 序列化为无歧义字节串：域前缀 + 定长整数 + **长度前缀**的变长段。
    /// 长度前缀不可省 —— 直接拼接时 `("ab", "c")` 与 `("a", "bc")` 得到同一串 AAD，
    /// 攻击者便能在两条记录间凑出「AAD 相同」的 kind/label 组合，绑定当场失效。
    fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(AAD_DOMAIN.len() + 32 + self.label.len());
        buf.extend_from_slice(AAD_DOMAIN);
        buf.extend_from_slice(&self.record_id.to_be_bytes());
        buf.extend_from_slice(&self.version.to_be_bytes());
        for part in [self.kind, self.label] {
            buf.extend_from_slice(&(part.len() as u64).to_be_bytes());
            buf.extend_from_slice(part.as_bytes());
        }
        buf
    }
}

pub fn seal(key: &[u8; 32], aad: Aad<'_>, plaintext: &[u8]) -> Result<Sealed, Error> {
    let cipher = Aes256Gcm::new(key.into());
    let mut nonce = [0u8; 12];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut nonce);
    // aes-gcm 0.11 起 Array::from_slice 已弃用（-D warnings 下会挂），改走 From<[u8; N]>
    let ct = cipher
        .encrypt(
            &Nonce::from(nonce),
            Payload {
                msg: plaintext,
                aad: &aad.encode(),
            },
        )
        .map_err(|e| Error::Crypto(e.to_string()))?;
    use base64::Engine;
    Ok(Sealed {
        nonce,
        ciphertext: base64::engine::general_purpose::STANDARD.encode(ct),
    })
}

pub fn open(key: &[u8; 32], aad: Aad<'_>, sealed: &Sealed) -> Result<Zeroizing<Vec<u8>>, Error> {
    use base64::Engine;
    let ct = base64::engine::general_purpose::STANDARD
        .decode(&sealed.ciphertext)
        .map_err(|e| Error::Crypto(e.to_string()))?;
    let cipher = Aes256Gcm::new(key.into());
    let pt = cipher
        .decrypt(
            &Nonce::from(sealed.nonce),
            Payload {
                msg: &ct,
                aad: &aad.encode(),
            },
        )
        .map_err(|_| Error::Integrity)?;
    Ok(Zeroizing::new(pt))
}

/// KEK 派生盐长度。盐无需保密（明文存于 vault.json），只需**唯一且与校验盐无关**。
pub const KDF_SALT_LEN: usize = 16;

/// 生成 KEK 派生专用随机盐。每次 `arm_passphrase` 重新生成。
pub fn generate_kdf_salt() -> [u8; KDF_SALT_LEN] {
    let mut salt = [0u8; KDF_SALT_LEN];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut salt);
    salt
}

/// 由口令 + **独立随机盐** 派生 32 字节 KEK（密钥加密密钥），用于封装 master key。
///
/// R117（critical）安全不变式：本函数所用的盐必须与 `derive_key_from_passphrase` 生成的
/// 校验用 PHC 内嵌盐**相互独立**。二者若同盐同参，Argon2 输出逐字节相同 —— 那意味着
/// 明文落盘的 PHC 末段 hash 字段**就是** KEK 本身，攻击者仅凭读取 vault.json
/// 即可解开 master_sealed 并解密全部凭据（无需口令、无需 OS keyring）。
/// 故：KEK 走本函数（盐 = `kdf_salt`），口令校验走 `verify_passphrase`（盐 = PHC 自带），
/// 两条路径的盐由 `Store::arm_passphrase` 分别独立生成，永不复用。
pub fn derive_kek(passphrase: &str, kdf_salt: &[u8]) -> Result<Zeroizing<[u8; 32]>, Error> {
    if kdf_salt.len() < KDF_SALT_LEN {
        return Err(Error::Crypto(format!(
            "kdf salt too short: {} < {KDF_SALT_LEN}",
            kdf_salt.len()
        )));
    }
    let mut out = Zeroizing::new([0u8; 32]);
    Argon2::new(Algorithm::Argon2id, Version::V0x13, Params::default())
        .hash_password_into(passphrase.as_bytes(), kdf_salt, &mut *out)
        .map_err(|e| Error::Crypto(e.to_string()))?;
    Ok(out)
}

/// Argon2id 口令**校验串**派生；返回 PHC 字符串（含随机盐与参数）供存储，校验走 verify_passphrase。
///
/// 注意：返回值只是校验凭据，**不得**（直接或经其内嵌盐重算）当作加密密钥使用 —— 见 `derive_kek` 的 R117 注记。
pub fn derive_key_from_passphrase(passphrase: &str, _salt_hint: &str) -> Result<String, Error> {
    let salt = SaltString::generate(&mut rand::rngs::OsRng);
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, Params::default());
    let out = argon2
        .hash_password(passphrase.as_bytes(), &salt)
        .map_err(|e| Error::Crypto(e.to_string()))?;
    Ok(out.serialize().to_string())
}

pub fn verify_passphrase(passphrase: &str, phc: &str) -> Result<(), Error> {
    let parsed = PasswordHash::new(phc).map_err(|e| Error::Crypto(e.to_string()))?;
    Argon2::default()
        .verify_password(passphrase.as_bytes(), &parsed)
        .map_err(|_| Error::Integrity)
}
