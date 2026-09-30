use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("crypto failure: {0}")]
    Crypto(String),
    #[error("integrity check failed (AAD mismatch or tampered ciphertext)")]
    Integrity,
    #[error("vault locked")]
    Locked,
    /// OS 凭据库里没有这份库的主密钥条目（`keyring::Error::NoEntry`）。
    ///
    /// 单列而不并入 `Locked`（路线图 4c，2026-09-02）：`Locked` 同时还表示「库文件不在」
    /// 「口令路径拿不到 PHC」等，app 层只能笼统报「密码错误或 keyring/文件异常」。而本变体对
    /// **没设应用口令**的库意味着世上再无第二份主密钥——那是「从备份恢复」级别的事实，必须与
    /// 「再试一次口令」区分开。Display 前缀 `keyring entry missing` 是前端 VaultDialog 的判据子串
    /// （跨 IPC 只剩字符串），改动同批；tests/keyring_missing.rs 钉着它。
    #[error("keyring entry missing: this vault's master key is not in the OS credential store")]
    KeyringMissing,
    #[error("record not found: {0}")]
    NotFound(u64),
    #[error("keyring error: {0}")]
    Keyring(String),
    #[error("storage error: {0}")]
    Storage(String),
}
