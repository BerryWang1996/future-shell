use crate::Error;
use std::fmt;
use zeroize::Zeroizing;

/// 秘密的**用途**（审计2 #20）。
///
/// 这一栏必须由存储层声明，不能由引擎从明文内容猜。原实现猜的判据是
/// `text.contains("PRIVATE KEY")`，两个方向都会错，而错的那一侧后果不对称：
///
/// - **私钥被当成口令**：PuTTY 的 `.ppk` 以 `PuTTY-User-Key-File-3:` 开头，正文里没有
///   `PRIVATE KEY` 这个词。于是整份私钥材料被当作口令**发给服务器**。通道是加密的，
///   但对端拿到的是用户的私钥——这是把凭据交给了连接的另一端，与「口令泄露」不同级。
/// - **口令被当成私钥**：一句包含 `PRIVATE KEY` 的口令（密码短语里出现这两个词并不离奇）
///   会被送去解 PEM，认证以一句「私钥解码失败」神秘告终。
///
/// 与 `fs_vault::SecretKind` 一一对应。core 不依赖 vault 的具体存储（见 [`SecretSource`]），
/// 故在此另立一份由 app 层映射；[`SecretKind::wire_tag`] 与 vault 侧 `SecretKind::aad_tag`
/// **逐字相同**，映射是否漂移由 app 层的对偶测试钉住。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretKind {
    Password,
    PrivateKey,
    /// SSH 认证**用不到**它。留着这个变体不是为了将来支持，而是为了让「vault 里存在这类记录」
    /// 这件事在类型上可表达——否则 app 层映射时只能把它塞进 Password 或 PrivateKey，
    /// 那正是本枚举要消灭的那种猜测。
    ApiKey,
}

impl SecretKind {
    /// 与 `fs_vault::SecretKind::aad_tag` 逐字相同的稳定标签。
    ///
    /// vault 那一侧的同名串已被 AEAD 的 AAD 认证、**永不改名**（改名 = 既有密文全部作废）；
    /// 这里跟着它走，好让两侧的对应关系有一个可比较的可观测量，而不是只存在于人的记忆里。
    pub fn wire_tag(self) -> &'static str {
        match self {
            SecretKind::Password => "password",
            SecretKind::PrivateKey => "private_key",
            SecretKind::ApiKey => "api_key",
        }
    }

    /// 解析 [`SecretKind::wire_tag`]。**认不出即 None**，绝不回落到某个默认值：
    /// 回落等于把「拼错的类别」悄悄变成一个具体用途，而用途正是这条边界要守住的东西。
    pub fn from_wire_tag(tag: &str) -> Option<SecretKind> {
        match tag {
            "password" => Some(SecretKind::Password),
            "private_key" => Some(SecretKind::PrivateKey),
            "api_key" => Some(SecretKind::ApiKey),
            _ => None,
        }
    }

    /// 面向用户的中文名，用于错误文案。
    pub fn label(self) -> &'static str {
        match self {
            SecretKind::Password => "口令",
            SecretKind::PrivateKey => "私钥",
            SecretKind::ApiKey => "API Key",
        }
    }
}

/// 一条取出的凭据：**用途 + 材料**。
pub struct Secret {
    pub kind: SecretKind,
    /// Zeroizing：随 drop 自动清零（总设计 §3.2）。
    pub bytes: Zeroizing<Vec<u8>>,
}

/// 手写 Debug 取代 derive：材料恒输出 `[REDACTED]`，与 `auth::CredentialSet` 同口径。
/// 长度也不出——它对排查没有用，却足以把短口令的搜索空间缩掉一大截。
impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Secret")
            .field("kind", &self.kind)
            .field("bytes", &"[REDACTED]")
            .finish()
    }
}

/// 由 app 层以 vault::Store 实现；core 不依赖 vault 的具体存储。
/// S289（Task 19 接线）：`Send + Sync` 超 trait——`connect()` 的 future 跨 await 持有
/// `&dyn SecretSource`，app 层经 `tauri::async_runtime::spawn`/`spawn_blocking` 驱动时
/// 该 future 必须 Send ⇒ `&dyn SecretSource: Send` ⇒ trait 须 Sync（Task 10 期只在
/// current-thread 测试运行时调用，未暴露此约束）。
pub trait SecretSource: Send + Sync {
    /// 取一条记录。**`kind` 必须来自存储层的元数据**，不得由实现方按内容推断——
    /// 那样只是把 [`SecretKind`] 文档里描述的猜测挪个地方，一个字都没修掉。
    fn secret(&self, vault_record: u64) -> Result<Secret, Error>;
}
