use crate::Error;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::SqlitePool;
use std::path::Path;
use std::time::Duration;

// `Debug` 为必需而非装饰：Task 5 测试以 `Db::open(..).unwrap_err()` 断言版本闸，
// `Result::<T, E>::unwrap_err` 要求 `T: Debug`，缺该 derive 即 E0277（本机实测取证）。
#[derive(Debug)]
pub struct Db {
    pool: SqlitePool,
}

impl Db {
    pub const MAX_SCHEMA: i64 = 9;
    /// 本 app 版本号；Db::open 用其与库内 meta.min_app_version 比较（spec §3.4.2）。
    pub const APP_VERSION: &'static str = env!("CARGO_PKG_VERSION");

    pub async fn open(path: &Path) -> Result<Self, Error> {
        // P1-18：必须在 `create_if_missing` 把文件变出来**之前**问一次「库本来在不在」。
        // 打开之后再问，答案恒为「在」，就再也分不清「首次建库、本就无可备份」与
        // 「已有用户数据、备份是迁移唯一的回滚保障」这两种情形了。
        let db_existed = path.exists();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // 审计2 #31：库文件先收紧权限再交给 SQLite。SQLite 用进程 umask 建文件（常见 0644），
        // 而库里是 profile（主机、端口、认证方式）与主机信任关系。必须**先于** connect：
        // SQLite 的 -wal/-shm 旁文件会继承库文件本身的权限位（SQLite 文档），所以管住
        // 主文件这一处就全管住了；文件已存在时（上一版本用 0644 建出来的正是最常见情形）
        // 照样收紧一次，而不是只伺候新建路径。
        restrict_db_file(path)?;
        // S11（med）：**绝不**把文件路径拼进 `sqlite:` URL 再 `from_str`。URL 解析会percent-解码、
        // 并在首个 `?` 处截断查询串——用户目录里一个 `?`、`#` 或 `%` 就让程序打开/新建到错误的
        // 路径上（Windows 上 `%USERNAME%` 这类字面量目录名并不罕见）。改走 builder，全程零解析。
        let opts = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            // 审计2 #34：Normal → FULL 是一次**明确接受**的耐久性决策。Normal 在 WAL 下允许
            // 断电后丢失最近若干已确认的提交（WAL 未同步回库），换的是写入吞吐；本库的写
            // 量是「增删一条 profile、记一行审计」，吞吐毫无意义，「保存成功」四字却必须
            // 是真的——每次提交多一次 fsync 的代价在这里可以忽略。原来的问题不在 Normal
            // 本身，而在它是 builder 的默认值：没人拍过板。现在拍板，并写在这里。
            .synchronous(SqliteSynchronous::Full)
            .foreign_keys(true)
            // S13（med）：多窗口/多进程同时首启时，迁移持写锁，后到者会立刻 SQLITE_BUSY 而不是等待。
            // SQLite 无跨进程建议锁可用，busy_timeout 是唯一的正解：让后到者退避重试而非报错退出。
            .busy_timeout(Duration::from_secs(15))
            // S6（med）：`INSERT OR REPLACE` 为满足约束而删行时，**仅在开启递归触发器时**才会触发
            // DELETE 触发器（SQLite 官方 REPLACE 语义）。不开则 audit 表的 append-only 触发器形同虚设：
            // 一条 `INSERT OR REPLACE INTO audit(id,…)` 就能原地改写任意审计行且不留痕（总设计 §2.1）。
            .pragma("recursive_triggers", "on");
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(opts)
            .await?;

        // 版本闸：迁移之前检查（便携 U 盘库可能被更新版本写高）
        let existing: i64 = sqlx::query_scalar("PRAGMA user_version")
            .fetch_one(&pool)
            .await?;
        if existing > Self::MAX_SCHEMA {
            return Err(Error::SchemaTooNew {
                found: existing,
                max: Self::MAX_SCHEMA,
            });
        }

        // 迁移前备份（spec §3.4.3）：已有数据且**真要跑迁移**的库才备份；VACUUM INTO 取一致快照（避开 WAL 旁文件拷贝问题，SQLite ≥3.27）；保留最近 5 份
        //
        // P1-18：这里的两个前置条件都是「无可备份」而非「备份失败」，跳过是正确的——
        //   · `db_existed == false`：本次 open 刚把空库变出来，没有任何用户数据可丢；
        //   · `existing == 0`：库是空的/未迁移过，同上。
        // 除此以外一旦进了这个分支，备份就是迁移的**唯一**回滚保障：磁盘满、备份目录只读、
        // 同名快照已存在……任一失败都意味着接下来那次 `migrate!().run()` 一旦改坏 schema
        // 就再也回不去。历史实现只记一条 warn 就照跑不误——那等于把「有备份」这个前提
        // 悄悄抽掉，留下的仅仅是一种心理安慰。故失败必须**阻断**迁移并如实报错，
        // 让用户先去解决磁盘/权限问题，库还原封不动地躺在那里。
        if db_existed && existing > 0 && existing < Self::MAX_SCHEMA {
            let backups = path.parent().unwrap_or(Path::new(".")).join("backups");
            std::fs::create_dir_all(&backups)?;
            // 审计2 #31：快照与库同密级（含全部 profile 与信任关系），目录收紧到 0700。
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&backups, std::fs::Permissions::from_mode(0o700))?;
            }
            let dest = backups.join(format!("fs-v{existing}-{}.db", unix_stamp()));
            sqlx::query("VACUUM INTO ?1")
                .bind(dest.display().to_string())
                .execute(&pool)
                .await
                .map_err(|e| backup_failed(&dest, format!("VACUUM INTO 失败：{e}")))?;
            // VACUUM INTO 返回 Ok 却没落下可用文件（外部工具竞删、网络盘写入被静默丢弃）
            // 同样是「没有备份」。备份的价值全在「事后真能拿它还原」，只信返回码不够。
            match std::fs::metadata(&dest) {
                Ok(m) if m.len() > 0 => {}
                Ok(_) => {
                    return Err(backup_failed(&dest, "备份快照为 0 字节".to_string()));
                }
                Err(e) => {
                    return Err(backup_failed(&dest, format!("备份快照不可读：{e}")));
                }
            }
            // 审计2 #31：VACUUM INTO 用进程 umask 建快照（常见 0644），同样收紧到 0600。
            // 失败不阻断迁移（内容已确认可用，权限不严只是收紧没成功），但不能静默——
            // 记一条 error 留痕。
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Err(e) =
                    std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o600))
                {
                    tracing::error!(dest = %dest.display(), %e, "备份快照权限收紧失败");
                }
            }
            // 裁剪必须排在「新快照已确认可用」之后：先删旧的再备份，一旦备份失败，
            // 手里连上一轮的快照都没了。裁剪本身失败不阻断迁移——它只影响磁盘占用。
            prune_backups(&backups, 5);
        }
        sqlx::migrate!("./migrations").run(&pool).await?;
        // PRAGMA 不支持 bind 占位符，插值量为编译期常量 MAX_SCHEMA，故 AssertSqlSafe 而非 .bind()（sqlx 0.9 SqlSafeStr）
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "PRAGMA user_version = {}",
            Self::MAX_SCHEMA
        )))
        .execute(&pool)
        .await?;

        // 审计2 #21：把信任库的 host 列折成规范键。**每次 open 都跑**，不与某一次迁移绑定。
        //
        // 只在「0003→0004 升级那一次」跑是不够的，也更难验证：这条改写是安全关键路径
        // （漏跑 = 全部既有信任行一次性查不到 = 全用户 fail-open 回 TOFU 弹框），而绑迁移
        // 意味着它只在一个**测试极难构造**的时点执行一次。做成幂等的自愈步骤后，
        // 普通单测就能直接钉住它。代价是每次启动一条 SELECT——host_keys 每台主机一行，
        // 且下面在「本就规范」时零写入。
        canonicalize_host_keys(&pool).await?;

        // 版本闸二（spec §3.4.2）：读库内声明的最低兼容 app 版本（meta 表随 0001 落地）；
        // 更新版本 app 写过的便携库可能声明高于本 app 的最低兼容版本 → 拒绝打开
        let min_app: Option<String> =
            sqlx::query_scalar("SELECT value FROM meta WHERE key = 'min_app_version'")
                .fetch_optional(&pool)
                .await?;
        if let Some(min) = min_app {
            if version_gt(&min, Self::APP_VERSION) {
                return Err(Error::AppTooOld {
                    min,
                    cur: Self::APP_VERSION.to_string(),
                });
            }
        }
        Ok(Self { pool })
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }
}

/// 把 `host_keys.host` 就地折成 [`crate::canonical_host`] 的输出（审计2 #21）。
///
/// 幂等：已经规范且无碰撞时**一个字节都不写**（直接返回），因此可以放在每次 `Db::open` 上。
///
/// ── 碰撞的处置 ────────────────────────────────────────────────────────────────
/// 折叠会把 `Example.COM` 与 `example.com` 变成同一个键，而 `host_keys` 钉着「每
/// (host, port) 至多一行」（R13/R30）。两行的 blob 若不同，就是「同一台主机存了两把
/// 互不相同的已信任密钥」——这正是该不变式存在的理由，必须当场收敛到一行。
///
/// 择一顺序：`source='tofu'`（用户亲手确认过的）优先于导入的 → `last_seen` 新者优先
/// → `key_blob` 字典序兜底（保证同一份数据每次得到同一个结果，而不是取决于 SQLite 的
/// 行序）。被丢掉的那把并没有因此变得可用：它下次出示时 `known` 非空且 blob 不匹配，
/// `decide` 判 `Changed` 弹红框——**fail-closed**，方向正确。
async fn canonicalize_host_keys(pool: &SqlitePool) -> Result<(), Error> {
    #[derive(sqlx::FromRow)]
    struct Row {
        host: String,
        port: i64,
        key_type: String,
        key_blob: String,
        fingerprint_sha256: String,
        first_seen: String,
        last_seen: String,
        source: String,
    }
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT host, port, key_type, key_blob, fingerprint_sha256, first_seen, last_seen, source
           FROM host_keys",
    )
    .fetch_all(pool)
    .await?;
    if rows.is_empty() {
        return Ok(());
    }
    let mut groups: std::collections::BTreeMap<(String, i64), Vec<Row>> = Default::default();
    for r in rows {
        groups
            .entry((crate::canonical_host(&r.host), r.port))
            .or_default()
            .push(r);
    }
    // 已是规范形且无碰撞 → 无事可做。两个条件缺一不可：全都规范但存在重复 (host, port)
    // 的库（手工改过库文件）同样需要收敛。
    let clean = groups
        .values()
        .all(|g| g.len() == 1 && crate::canonical_host(&g[0].host) == g[0].host);
    if clean {
        return Ok(());
    }

    let mut winners: Vec<((String, i64), Row)> = Vec::with_capacity(groups.len());
    for (key, mut g) in groups {
        let distinct: std::collections::BTreeSet<&str> =
            g.iter().map(|r| r.key_blob.as_str()).collect();
        if distinct.len() > 1 {
            tracing::warn!(
                host = %key.0,
                port = key.1,
                dropped = distinct.len() - 1,
                "主机名规范化后，同一台主机存在多把互不相同的已信任密钥，只保留其一；\
                 其余密钥再出示时会按「主机密钥已变更」硬提示"
            );
        }
        g.sort_by(|a, b| {
            (b.source == "tofu")
                .cmp(&(a.source == "tofu"))
                .then_with(|| b.last_seen.cmp(&a.last_seen))
                .then_with(|| b.key_blob.cmp(&a.key_blob))
        });
        winners.push((key, g.swap_remove(0)));
    }

    // 单事务：中途失败不得留下「删了一半」的信任库——那会让下次连接对一台早已确认过的
    // 主机弹 TOFU 框，与本次修复要消灭的正是同一种 fail-open。
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM host_keys")
        .execute(&mut *tx)
        .await?;
    for ((host, port), w) in winners {
        // first_seen / last_seen / source 原样搬过去：这三列是用户判断「我什么时候
        // 信任的这台主机」的全部依据，规范化不该重置它们。
        sqlx::query(
            r#"INSERT INTO host_keys
                 (host, port, key_type, key_blob, fingerprint_sha256, first_seen, last_seen, source)
               VALUES (?1,?2,?3,?4,?5,?6,?7,?8)"#,
        )
        .bind(&host)
        .bind(port)
        .bind(&w.key_type)
        .bind(&w.key_blob)
        .bind(&w.fingerprint_sha256)
        .bind(&w.first_seen)
        .bind(&w.last_seen)
        .bind(&w.source)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// 审计2 #31：库文件权限收紧到 0600（Unix；Windows 上权限位由 ACL 决定，无对应操作）。
///
/// 文件不存在时**先由我们**用 0600 建出来，不让 `create_if_missing` 拿进程 umask 碰
/// 运气（常见 0644，同机其他账户可读）；已存在也照样收紧一次——「上个版本建的 0644
/// 库」正是最可能出现的情形。`create_new` 撞上并发建文件（另一个实例、测试并行）
/// 不算错误：文件反正已经在那里，落进后面的 chmod 同一条路。
fn restrict_db_file(path: &Path) -> Result<(), Error> {
    if !path.exists() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            let created = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(path);
            match created {
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(Error::Io(e)),
            }
        }
        #[cfg(not(unix))]
        {
            let created = std::fs::File::create(path);
            match created {
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(Error::Io(e)),
            }
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(Error::Io)?;
    }
    Ok(())
}

/// P1-18：把「迁移前备份失败」组装成一条**用户看得懂、排障能用**的错误。
///
/// 已换成专用变体 `Error::BackupFailed`（上一轮因 `error.rs` 归属别的改动面而暂借
/// `Error::Io`）。差别不只是文案好看：借道 `Io` 时调用方无法把这条与 `create_dir_all`
/// 之类的普通 IO 失败区分开，只能去匹配错误字符串——那是会随文案微调而悄悄失效的判据。
/// `dest` 单列成字段而非只拼进消息，正是为了让上层能直接把路径塞进「打开所在文件夹」按钮。
fn backup_failed(dest: &Path, why: String) -> Error {
    let e = Error::BackupFailed {
        dest: dest.display().to_string(),
        reason: why,
    };
    tracing::error!(dest = %dest.display(), "{e}");
    e
}

/// 备份文件名后缀：无额外依赖，SystemTime 秒数即可。
fn unix_stamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 备份裁剪：文件名含时间戳，按文件名字典序降序保留最近 `keep` 份（spec §3.4.3「保留最近 5 份」）。
fn prune_backups(dir: &Path, keep: usize) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<_> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with("fs-v") && n.ends_with(".db"))
                .unwrap_or(false)
        })
        .collect();
    files.sort_unstable_by(|a, b| b.file_name().cmp(&a.file_name()));
    for old in files.into_iter().skip(keep) {
        let _ = std::fs::remove_file(old);
    }
}

/// 版本号比较：按点分段取数值（缺段/非数字段按 0），a > b 时返回 true。
fn version_gt(a: &str, b: &str) -> bool {
    fn parts(v: &str) -> [u64; 3] {
        let mut out = [0u64; 3];
        for (i, seg) in v.split('.').take(3).enumerate() {
            out[i] = seg.parse().unwrap_or(0);
        }
        out
    }
    parts(a) > parts(b)
}
