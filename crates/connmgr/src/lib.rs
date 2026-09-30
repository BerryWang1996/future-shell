//! fs_connmgr — 连接配置/分组/信任库存储（spec §3.1）。
pub mod audit_checkpoint;
pub mod audit_repo;
pub mod civil;
pub mod cron;
pub mod db;
pub mod error;
pub mod forensics;
pub mod history_repo;
pub mod host;
pub mod import_foreign;
pub mod model;
pub mod repo;
pub mod schedule;
pub mod schedule_repo;
pub mod settings_repo;
pub use db::Db;
pub use error::Error;
// 与 `Db` 同级导出：主机名规范化是**存储键的定义**，信任库的每一个入口（sshengine 的
// `TrustStore`、db.rs 的升级改写）都必须走同一份实现，路径长一点都会诱使下一个人重写一遍。
pub use host::canonical_host;
pub use model::*;
// `BadRow` 与 `ProfileRepo` 同级导出：坏行诊断是 `list_with_diagnostics` 返回值的一半，
// 下游（app 层 conn_cmd）要按它组装给前端的提示，没理由逼调用方写 `repo::BadRow` 这种
// 「知道它藏在哪个模块」的长路径。
pub use history_repo::{HistoryEntry, HistoryRepo};
pub use repo::{BadRow, ProfileRepo};
pub use settings_repo::SettingsRepo;
