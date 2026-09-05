use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("sqlite: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("migration: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
    #[error("schema too new: db user_version={found}, this app supports up to {max}")]
    SchemaTooNew { found: i64, max: i64 },
    #[error("app too old: db requires app >= {min}, this app is {cur}")]
    AppTooOld { min: String, cur: String },
    #[error("not found: {0}")]
    NotFound(String),
    /// 库中某行无法还原为领域对象（uuid 非法、port 越界、blob 非法 JSON……）。
    /// S18：这类「数据坏了」必须与「查无此条」区分——历史实现把 uuid 解析失败映射成
    /// `NotFound`，既误导排障，又让 `list()` 因一行坏数据而整体失败（全部连接凭空消失）。
    #[error("corrupt row {id}: {reason}")]
    CorruptRow { id: String, reason: String },
    /// P1-18：迁移前备份失败 —— 迁移已被**阻断**，库仍是原样。
    ///
    /// 单列变体而非借道 `Error::Io`：这条是调用方需要**分支处理**的（提示用户去看目标盘的
    /// 剩余空间/权限，而不是笼统的「IO 出错」），而 `Error::Io` 同时承载着 create_dir_all
    /// 等一堆不相干的失败，`matches!` 根本分不开；错误文案里必带 `dest`，因为用户十有八九
    /// 要去那个路径上排查。
    #[error("迁移前备份失败，已中止迁移：{reason}；目标 {dest}")]
    BackupFailed { dest: String, reason: String },
    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),
    /// 审计2 #37：输入校验/导入上限被触发——「内容能被解析，但产品不接受」。
    /// 与 `Serde`/`Io` 分开：前者是语法坏，后者是设备坏，这条是**规模或形状越界**，
    /// 调用方拿它当「把文案直出给用户改完重试」的信号用。
    #[error("{0}")]
    Validation(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}
