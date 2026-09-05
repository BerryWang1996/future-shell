//! fs_vault — 加密凭据保险箱（spec §3.2）。
pub mod crypto;
mod error;
pub use error::Error;
pub mod store;
pub use store::{
    file_needs_passphrase, lock_data_dir, release_process_locks, BackupEntry, RecordMeta,
    SecretKind, Store, BACKUP_DIR,
};
