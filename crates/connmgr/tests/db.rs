mod common;
use fs_connmgr::Db;

#[tokio::test]
async fn open_fresh_db_migrates_and_sets_user_version() {
    let dir = tmp();
    let db = Db::open(&dir.join("fs.db")).await.unwrap();
    let ver: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(ver, Db::MAX_SCHEMA);
}

#[tokio::test]
async fn refuses_newer_schema() {
    let dir = tmp();
    let path = dir.join("fs.db");
    {
        let db = Db::open(&path).await.unwrap();
        // PRAGMA 不吃占位符（sqlx 0.9 AssertSqlSafe 的既有坑），插编译期常量
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "PRAGMA user_version = {}",
            Db::MAX_SCHEMA + 1,
        ))) // 比 MAX_SCHEMA 高一档 → 必须拒绝（不写死数字：MAX 会随迁移走）
        .execute(db.pool())
        .await
        .unwrap();
    }
    let err = Db::open(&path).await.unwrap_err();
    assert!(
        matches!(err, fs_connmgr::Error::SchemaTooNew { .. }),
        "{err:?}"
    );
}

#[tokio::test]
async fn reopen_is_idempotent() {
    let dir = tmp();
    let path = dir.join("fs.db");
    Db::open(&path).await.unwrap();
    Db::open(&path).await.unwrap();
}

/// S15（med）：断言必须覆盖**全部**迁移产物。历史版本只白名单了 `('settings','audit')`——
/// 0001 建的 profiles/host_keys/session_journal/meta 若哪天在迁移里写漏或改名，本测试照样全绿，
/// 直到运行期第一条 SQL 撞上「no such table」才暴露。这里改为逐张点名，一张不许少。
#[tokio::test]
async fn migrations_create_all_tables_and_audit_triggers() {
    let dir = tmp();
    let db = Db::open(&dir.join("fs.db")).await.unwrap();
    let names: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type='table'
           AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\'
           AND name <> '_sqlx_migrations' ORDER BY name",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    assert_eq!(
        names,
        vec![
            "audit".to_string(),
            // M3 出口 7 ③ 审计链周期性快照（0007）：增量校验的锚点
            "audit_checkpoint".to_string(),
            // M4a 历史命令库（0005）：本地命令历史 + 屏幕启发式提取的候选命令
            "command_history".to_string(),
            "host_keys".to_string(),
            "meta".to_string(),
            "profiles".to_string(),
            // RDP 证书 TOFU（0008，阶段 1）：与 host_keys 同款语义
            "rdp_certs".to_string(),
            "revoked_host_keys".to_string(),
            // M4a 计划任务（0006）：任务定义 + 执行记录（跑过没跑过、成没成、为什么没跑）
            "scheduled_runs".to_string(),
            "scheduled_tasks".to_string(),
            "session_journal".to_string(),
            "settings".to_string(),
            // 共享凭据登记（0009）：默认每个连接一份自己的凭据，
            // 只有被显式「设为共享」的记录才进这张表、才对其他连接可见
            "shared_credentials".to_string(),
        ],
        "迁移产物与预期表集合不符"
    );

    // append-only 由触发器承载：触发器没建出来，下面的 audit 用例全部会「静默通过」
    let triggers: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type='trigger' AND tbl_name='audit' ORDER BY name",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    assert_eq!(
        triggers,
        vec!["audit_no_delete".to_string(), "audit_no_update".to_string()]
    );

    // 快照表同样 append-only，理由同上一段：快照的语义是「我在时刻 T 查过」，
    // 允许 UPDATE 就等于允许把一次失败的校验事后改成成功的。
    // 这里与 audit 分开点名而不是并成一个查询：两张表的触发器各自可红。
    let cp_triggers: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type='trigger'
           AND tbl_name='audit_checkpoint' ORDER BY name",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    assert_eq!(
        cp_triggers,
        vec![
            "audit_checkpoint_no_delete".to_string(),
            "audit_checkpoint_no_update".to_string()
        ]
    );
}

#[tokio::test]
async fn audit_table_is_append_only() {
    let dir = tmp();
    let db = Db::open(&dir.join("fs.db")).await.unwrap();
    sqlx::query(
        "INSERT INTO audit (actor, action, target_session, risk_level, verdict, exit_code, output_digest, created_at, prev_hash, hash)
         VALUES ('alice', 'shell.exec', 'sess-1', 'high', 'approved', 0, 'sha256:digest', datetime('now'), NULL, 'h1')")
        .execute(db.pool()).await.unwrap();
    let upd = sqlx::query("UPDATE audit SET verdict = 'denied' WHERE actor = 'alice'")
        .execute(db.pool())
        .await;
    assert!(
        upd.is_err(),
        "audit 表必须拒绝 UPDATE（触发器 RAISE ABORT）"
    );
    let del = sqlx::query("DELETE FROM audit WHERE actor = 'alice'")
        .execute(db.pool())
        .await;
    assert!(
        del.is_err(),
        "audit 表必须拒绝 DELETE（触发器 RAISE ABORT）"
    );
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(n, 1);
}

/// S6（med）：`INSERT OR REPLACE` 是 append-only 的后门。
///
/// SQLite 的 REPLACE 语义：为满足唯一约束而删除既有行时，**只有在 `recursive_triggers` 打开时**
/// 才会触发该表的 DELETE 触发器。默认关闭 ⇒ 一条 `INSERT OR REPLACE INTO audit(id,…)` 就能
/// 原地改写任意一条审计记录，两个 RAISE(ABORT) 触发器全程沉默 —— 审计链（prev_hash/hash）
/// 被就地伪造且不留痕（总设计 §2.1「审计不可篡改」）。故 Db::open 必须开启该 PRAGMA。
#[tokio::test]
async fn audit_rejects_insert_or_replace_rewrite() {
    let dir = tmp();
    let db = Db::open(&dir.join("fs.db")).await.unwrap();
    sqlx::query(
        "INSERT INTO audit (id, actor, action, target_session, risk_level, verdict, exit_code, output_digest, created_at, prev_hash, hash)
         VALUES (1, 'alice', 'shell.exec', 'sess-1', 'high', 'approved', 0, 'sha256:real', datetime('now'), NULL, 'h1')")
        .execute(db.pool()).await.unwrap();

    // 攻击者视角：把「approved 的高危命令」就地改写成「denied 的低危命令」，id 不变
    let rewritten = sqlx::query(
        "INSERT OR REPLACE INTO audit (id, actor, action, target_session, risk_level, verdict, exit_code, output_digest, created_at, prev_hash, hash)
         VALUES (1, 'alice', 'noop', 'sess-1', 'low', 'denied', 0, 'sha256:forged', datetime('now'), NULL, 'h1')")
        .execute(db.pool()).await;
    assert!(
        rewritten.is_err(),
        "INSERT OR REPLACE 绕过了 append-only 触发器：审计记录可被就地伪造（S6）"
    );

    let (digest, verdict): (String, String) =
        sqlx::query_as("SELECT output_digest, verdict FROM audit WHERE id = 1")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(digest, "sha256:real", "原始审计记录必须原封不动");
    assert_eq!(verdict, "approved");
}

#[tokio::test]
async fn meta_min_app_version_is_seeded() {
    let dir = tmp();
    let db = Db::open(&dir.join("fs.db")).await.unwrap();
    let v: String = sqlx::query_scalar("SELECT value FROM meta WHERE key = 'min_app_version'")
        .fetch_one(db.pool())
        .await
        .unwrap();
    // 这里是 **0.1.0 而不是 APP_VERSION**，且是有意的（M4a.1 T92）：
    // `min_app_version` 由 migrations/0001_init.sql 写死，改它就要改那个 migration 的内容，
    // 而 sqlx 会校验已应用 migration 的 checksum——所有既有库当场开不了。
    // 语义上也对：这个字段是「**库**要求 app 至少多新」，0.1.0 起的 app 都能读懂当前 schema，
    // 与 app 自己升到 0.4.0 无关。真要抬高它，得新增一个 0007 migration 去 UPDATE 这一行，
    // 并与 updater 的降级/回滚语义一起定——本阶段不做。
    assert_eq!(v, "0.1.0");
}

#[tokio::test]
async fn refuses_db_requiring_newer_app() {
    let dir = tmp();
    let path = dir.join("fs.db");
    {
        let db = Db::open(&path).await.unwrap();
        // 模拟更新版本 app 写过的库：把库内声明的最低兼容版本抬高到本 app 之上
        sqlx::query("UPDATE meta SET value = '999.0.0' WHERE key = 'min_app_version'")
            .execute(db.pool())
            .await
            .unwrap();
    }
    let err = Db::open(&path).await.unwrap_err();
    assert!(
        matches!(err, fs_connmgr::Error::AppTooOld { .. }),
        "{err:?}"
    );
}

#[tokio::test]
async fn pre_migration_backup_is_pruned_to_five() {
    let dir = tmp();
    let path = dir.join("fs.db");
    Db::open(&path).await.unwrap();
    let backups = dir.join("backups");
    std::fs::create_dir_all(&backups).unwrap();
    for i in 1..=7u32 {
        std::fs::write(backups.join(format!("fs-v1-000000{i}.db")), b"x").unwrap();
    }
    // 构造「真要跑迁移」场景：回退 user_version 到 2（< MAX_SCHEMA）再打开 → 触发一次备份 + 裁剪
    {
        let db = Db::open(&path).await.unwrap();
        sqlx::query("PRAGMA user_version = 2")
            .execute(db.pool())
            .await
            .unwrap();
    }
    Db::open(&path).await.unwrap();
    let entries: Vec<_> = std::fs::read_dir(&backups)
        .unwrap()
        .map(|e| e.unwrap())
        .collect();
    let n = entries.len();
    assert!(n <= 5, "备份应裁剪至 ≤5 份，实际 {n}");
    // 仅断言 n<=5 会「空过」：VACUUM INTO 失败时只记 warn，7 份预置文件照样被裁到 5。
    // 故显式断言本轮确实产出了 fs-v2-*.db 快照且非空（spec §3.4.3 迁移前备份）。
    let fresh = entries
        .iter()
        .find(|e| e.file_name().to_string_lossy().starts_with("fs-v2-"))
        .expect("迁移前应产出 fs-v2-*.db 备份快照");
    assert!(fresh.metadata().unwrap().len() > 0, "备份快照不得为空文件");
}

/// S11（med）：库路径**不得**先拼进 `sqlite:` URL 再解析。
///
/// sqlx 0.9 的 `SqliteConnectOptions::from_str` 实测做两件破坏路径的事（sqlx-sqlite
/// `options/parse.rs`）：① `url.splitn(2, '?')` —— 首个 `?` 之后全被当查询串丢弃；
/// ② `percent_decode_str(database)` —— 文件名里的合法百分号转义会被解码。
/// 于是目录名里一个 `%41` 就把 `…100%41value/` 变成 `…100Avalue/`：要么静默把库建到
/// 另一个路径（用户眼中「配置全没了」，磁盘上其实躺着两个库），要么因该目录不存在而直接打不开。
///
/// 两条都可在 Windows 上复现（`%41`），`?` 一条只在类 Unix 可复现（Windows 文件名禁用 `?`）。
#[tokio::test]
async fn open_handles_url_special_chars_in_path() {
    // `%41` 是合法百分号转义（解码为 'A'），专治「先 URL 解析再当路径用」；
    // `?` 在 Windows 上是非法文件名字符，故只在类 Unix 上追加。
    #[cfg(windows)]
    let odd = "fs db #1 100%41value";
    #[cfg(not(windows))]
    let odd = "fs db #1 100%41value ?q=1";

    let dir = tmp().join(odd);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("fs.db");

    let db = Db::open(&path).await.unwrap();
    sqlx::query("INSERT INTO meta (key, value) VALUES ('canary', 'alive')")
        .execute(db.pool())
        .await
        .unwrap();
    drop(db);

    assert!(path.exists(), "库必须落在给定路径上，而非 URL 解析后的别处");
    let db = Db::open(&path).await.unwrap();
    let v: String = sqlx::query_scalar("SELECT value FROM meta WHERE key = 'canary'")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(v, "alive", "重开时打开的必须是同一个库文件（S11）");
}

/// S13（med）+ S6：连接级 PRAGMA 必须**真的**生效在池里取到的每一条连接上。
///
/// S13 单靠行为测很难非空验证——要复现「后到者立刻 SQLITE_BUSY」得跨进程抢写锁，
/// 而验证退避成功又得真等上十几秒。故直接读回 PRAGMA：这是该配置唯一的可观测面，
/// 少了它，多窗口同时首启的后到者会在迁移持锁期间直接报错退出，而不是退避重试。
#[tokio::test]
async fn connection_pragmas_are_in_effect() {
    let dir = tmp();
    let db = Db::open(&dir.join("fs.db")).await.unwrap();

    let busy: i64 = sqlx::query_scalar("PRAGMA busy_timeout")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(busy, 15_000, "busy_timeout 应为 15s（毫秒计）（S13）");

    let recursive: i64 = sqlx::query_scalar("PRAGMA recursive_triggers")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(
        recursive, 1,
        "recursive_triggers 必须开启，否则 REPLACE 绕过 audit 的 append-only 触发器（S6）"
    );

    let fk: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(fk, 1);
}

/// S22（high）回归：临时目录助手绝不得把**已存在**的目录交给测试。
///
/// 复现方式与真机触发路径同构：按旧命名规则（`fs-connmgr-{pid}-{n}`）预先铺满当前 pid 的
/// 前 128 个槽位、各放一份毒文件 —— 这正是「上一轮残留 + Windows 回收 pid」在本机造成的现场
/// （实测 `%TEMP%` 有 175 个 `fs-connmgr-*-0`，5 个装着 `min_app_version='999.0.0'` 的坏库）。
/// 旧实现的 `create_dir_all` 会把这些目录原样交回（计数器无论从几起步都落在已铺范围内），
/// 测试于是跑在毒数据上；新实现名字含纳秒、且用 `create_dir` 撞名重试，交回的目录必为空。
///
/// 铺 128 个而非 1 个：`cargo test` 单进程多线程跑时计数器为全binary 共享，起始值不可预知，
/// 128 覆盖本二进制的全部助手调用次数，令本测试与执行器无关地稳定复现。
#[test]
fn tmpdir_never_hands_back_an_existing_directory() {
    let base = std::env::temp_dir();
    let seeded: Vec<_> = (0..128u64)
        .map(|n| base.join(format!("fs-connmgr-{}-{}", std::process::id(), n)))
        .collect();
    for p in &seeded {
        std::fs::create_dir_all(p).unwrap();
        std::fs::write(p.join("poison.txt"), b"leftover from a previous run").unwrap();
    }
    for _ in 0..8 {
        let d = tmp();
        assert_eq!(
            std::fs::read_dir(&d).unwrap().count(),
            0,
            "助手交回了一个已存在且非空的目录：{}（S22）",
            d.display()
        );
    }
    for p in &seeded {
        let _ = std::fs::remove_dir_all(p);
    }
}

/// P1-18：迁移前备份失败必须**阻断**迁移。
///
/// 备份是迁移唯一的回滚保障。历史实现在 `VACUUM INTO` 失败时只记一条 warn 就照跑不误 ——
/// 于是 `migrate!().run()` 在「其实没有备份」的前提下改写 schema，一旦某条迁移出问题，
/// 用户的库既回不去也没有快照可还原；那条 warn 只是心理安慰。
///
/// 确定性地把备份做失败：SQLite 的 `VACUUM INTO` 要求目标文件不存在（或为空文件），
/// 而备份名是 `fs-v{ver}-{unix秒}.db` —— 预先把当前秒前后几秒的候选名全占上非空文件，
/// 无论 open 落在哪一秒，`VACUUM INTO` 都必然失败。
#[tokio::test]
async fn backup_failure_aborts_migration() {
    let dir = tmp();
    let path = dir.join("fs.db");
    // 造出「已有数据、且真要跑迁移」的库：回退 user_version 到 2（< MAX_SCHEMA）
    {
        let db = Db::open(&path).await.unwrap();
        sqlx::query("PRAGMA user_version = 2")
            .execute(db.pool())
            .await
            .unwrap();
    }

    let backups = dir.join("backups");
    std::fs::create_dir_all(&backups).unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    // 覆盖前后各几秒，容忍 open 与本行之间的任意耗时
    for s in (now.saturating_sub(2))..=(now + 8) {
        std::fs::write(backups.join(format!("fs-v2-{s}.db")), b"occupied").unwrap();
    }

    let err = Db::open(&path).await.unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("备份"),
        "错误必须指明是备份失败（用户据此去查磁盘/权限），实际：{msg}"
    );

    // 关键断言：迁移**没有发生**，库还是原来的样子。少了这条，上面的报错也可能是
    // 「迁移照跑完了，只是最后顺手报了个错」——那正是本条要修的缺陷。
    let ver: i64 = {
        let pool = sqlx::SqlitePool::connect_with(
            sqlx::sqlite::SqliteConnectOptions::new().filename(&path),
        )
        .await
        .unwrap();
        let v = sqlx::query_scalar("PRAGMA user_version")
            .fetch_one(&pool)
            .await
            .unwrap();
        pool.close().await;
        v
    };
    assert_eq!(ver, 2, "备份失败后 schema 必须原封不动（P1-18）");
}

/// P1-18 的另一半：**首次建库没有可备份的东西，不算失败**，不得因此挡住新用户。
/// 若把「无可备份」与「备份失败」混为一谈，全新安装的用户会在第一次启动时直接被拒之门外。
#[tokio::test]
async fn fresh_db_needs_no_backup() {
    let dir = tmp();
    // 备份目录预先占成一个**文件**：真要做备份就必然失败（create_dir_all 撞上同名文件）。
    // 首次建库能顺利打开，就证明这条路径根本没被走到，而不是「碰巧成功了」。
    std::fs::write(dir.join("backups"), b"not a directory").unwrap();

    let db = Db::open(&dir.join("fs.db")).await.unwrap();
    let ver: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(ver, Db::MAX_SCHEMA, "首次建库应正常迁移到最新版本");
}

// ── 审计2 #21：既有信任行的 host 列规范化（`Db::open` 每次都跑的自愈步骤）──────────

/// 往 `host_keys` 里塞一行**未经规范化**的记录，模拟旧版本写下的库。
async fn seed_key(pool: &sqlx::SqlitePool, host: &str, blob: &str, source: &str, last_seen: &str) {
    sqlx::query(
        r#"INSERT INTO host_keys (host, port, key_type, key_blob, fingerprint_sha256, first_seen, last_seen, source)
           VALUES (?1, 22, 'ssh-ed25519', ?2, ?3, '2020-01-01 00:00:00', ?4, ?5)"#,
    )
    .bind(host)
    .bind(blob)
    .bind(format!("SHA256:{blob}"))
    .bind(last_seen)
    .bind(source)
    .execute(pool)
    .await
    .unwrap();
}

async fn hosts_in_db(pool: &sqlx::SqlitePool) -> Vec<(String, String)> {
    sqlx::query_as("SELECT host, key_blob FROM host_keys ORDER BY host")
        .fetch_all(pool)
        .await
        .unwrap()
}

/// 旧库里同一台主机的四种等价拼法，重开一次必须收敛成**一个**键。
///
/// 不做这一步的后果是静默 fail-open：用户在 `Example.COM` 上确认过的信任，换成 `example.com`
/// 连接时一行都查不到 → 又弹一次「首次连接，是否信任」。危害不在多点一次鼠标，而在于它
/// 打破了「这个框只在第一次出现」这条用户唯一赖以判断的经验规则。
#[tokio::test]
async fn open_folds_equivalent_host_spellings_into_one_key() {
    let dir = tmp();
    let path = dir.join("fs.db");
    {
        let db = Db::open(&path).await.unwrap();
        for h in ["Example.COM", "example.com.", "EXAMPLE.com", "example.com"] {
            // 同一把密钥（blob 相同）的四条不同拼法：PK 是 (host, port, key_blob)，故能共存
            seed_key(db.pool(), h, "blob-same", "imported", "2020-01-02 00:00:00").await;
        }
        assert_eq!(hosts_in_db(db.pool()).await.len(), 4, "预置应有 4 行");
    }
    let db = Db::open(&path).await.unwrap();
    let rows = hosts_in_db(db.pool()).await;
    assert_eq!(
        rows,
        vec![("example.com".to_string(), "blob-same".to_string())],
        "四种等价拼法必须收敛成唯一的规范键"
    );
}

/// 折叠制造出的碰撞必须当场收敛到一行，且**择一有确定的优先级**：
/// 用户亲手 TOFU 确认过的 > 导入的；同来源则 `last_seen` 新者胜。
///
/// 落选的那把并没有因此变得可用——它下次出示时 `known` 非空且 blob 不匹配，`decide` 判
/// `Changed` 弹红框，是 fail-closed。反过来若在这里保留两行，就破坏了「每 (host, port)
/// 至多一行」（R13/R30）：攻击者的密钥能与真密钥并列长存，而那正是该不变式要挡的事。
#[tokio::test]
async fn colliding_rows_collapse_to_one_and_tofu_wins() {
    let dir = tmp();
    let path = dir.join("fs.db");
    {
        let db = Db::open(&path).await.unwrap();
        // 导入来的、时间更新 —— 单看 last_seen 它该赢；但来源优先级更高，必须让位给 tofu
        seed_key(
            db.pool(),
            "Host.example",
            "blob-imported",
            "imported",
            "2030-01-01 00:00:00",
        )
        .await;
        seed_key(
            db.pool(),
            "host.example",
            "blob-tofu",
            "tofu",
            "2020-01-01 00:00:00",
        )
        .await;
    }
    let db = Db::open(&path).await.unwrap();
    assert_eq!(
        hosts_in_db(db.pool()).await,
        vec![("host.example".to_string(), "blob-tofu".to_string())],
        "碰撞须收敛成一行，且用户亲手确认过的那把优先于导入的"
    );
    // 元数据不得被重置：这几列是用户判断「我什么时候信任的这台主机」的全部依据。
    let (first_seen, source): (String, String) =
        sqlx::query_as("SELECT first_seen, source FROM host_keys")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(first_seen, "2020-01-01 00:00:00", "first_seen 须原样保留");
    assert_eq!(source, "tofu", "source 须原样保留");
}

/// 同来源碰撞时按 `last_seen` 新者胜——这条单独钉，否则「来源优先」一条规则就能让
/// 上一条测试全绿，而排序的后半段（时间、blob 字典序兜底）从未被执行过。
#[tokio::test]
async fn same_source_collision_keeps_the_most_recently_seen() {
    let dir = tmp();
    let path = dir.join("fs.db");
    {
        let db = Db::open(&path).await.unwrap();
        seed_key(
            db.pool(),
            "Dup.example",
            "blob-old",
            "imported",
            "2020-01-01 00:00:00",
        )
        .await;
        seed_key(
            db.pool(),
            "dup.example",
            "blob-new",
            "imported",
            "2026-06-01 00:00:00",
        )
        .await;
    }
    let db = Db::open(&path).await.unwrap();
    assert_eq!(
        hosts_in_db(db.pool()).await,
        vec![("dup.example".to_string(), "blob-new".to_string())]
    );
}

/// 幂等：库本就规范时，重开**一个字节都不写**。
///
/// 这条自愈步骤每次 `Db::open` 都跑，不幂等就是每次启动改写一次用户的信任库。
///
/// 光看 `first_seen`/`last_seen` 是**证不出**这一点的：重写分支照样原样搬这几列（那是刻意的），
/// 所以列值相同既可能是「没写」，也可能是「删了再插、值一样」。真正能分辨两者的是 `rowid`——
/// DELETE + INSERT 必然换一个新的。少了这一条，把 `clean` 早返整段删掉都不会有测试变红，
/// 而那正是「每次启动重写一遍用户信任库」这个缺陷的样子。
///
/// 但 `rowid` 这个观测量**必须先钉住才成立**：重写分支是整表 `DELETE` 再逐行 `INSERT`，
/// 表里只有这一行时删完表就空了，下一个自动 rowid 又是 1——删了再插回来，rowid 原样复现，
/// 断言一声不吭。所以这里把 rowid 挪到一个重插不可能产生的值（自动分配只会从 1 起步），
/// 让「写过」变成真的看得见。V34/M4 就是踩在这上面：早返删掉，测试仍是绿的。
#[tokio::test]
async fn canonical_db_is_left_untouched_on_reopen() {
    const PINNED_ROWID: i64 = 424_242;
    let dir = tmp();
    let path = dir.join("fs.db");
    let rowid0: i64 = {
        let db = Db::open(&path).await.unwrap();
        seed_key(
            db.pool(),
            "already.canonical",
            "blob-x",
            "tofu",
            "2021-03-04 05:06:07",
        )
        .await;
        sqlx::query("UPDATE host_keys SET rowid = ?1")
            .bind(PINNED_ROWID)
            .execute(db.pool())
            .await
            .unwrap();
        let rowid: i64 = sqlx::query_scalar("SELECT rowid FROM host_keys")
            .fetch_one(db.pool())
            .await
            .unwrap();
        // 钉不住就等于下面那条断言是摆设，宁可在这里就炸
        assert_eq!(rowid, PINNED_ROWID, "rowid 没能钉到重插不可复现的值");
        rowid
    };
    for _ in 0..2 {
        let db = Db::open(&path).await.unwrap();
        let row: (String, String, String, String) =
            sqlx::query_as("SELECT host, key_blob, first_seen, last_seen FROM host_keys")
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(
            row,
            (
                "already.canonical".to_string(),
                "blob-x".to_string(),
                "2020-01-01 00:00:00".to_string(),
                "2021-03-04 05:06:07".to_string()
            ),
            "已规范的库重开必须原封不动"
        );
        let rowid: i64 = sqlx::query_scalar("SELECT rowid FROM host_keys")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(
            rowid, rowid0,
            "rowid 变了 = 这一行被删了再插一遍：库本就规范时不该有任何写入"
        );
    }
}

/// 来源与 `last_seen` 全都打平时，择一仍须**确定**。
///
/// 这一格看着像洁癖，实则是「同一份库，两次启动可能留下不同的密钥」——用户在 A 机器上
/// 看到的信任行和 B 机器上不一样，而两边的库文件逐字节相同。没有 `key_blob` 兜底时，
/// `sort_by` 是稳定排序，结果就退化成 SQLite 恰好按什么顺序把行吐出来。
///
/// 这里刻意让「先插入的那行」和「blob 字典序靠后的那行」是**不同**的两行：只有兜底判据
/// 真的生效，赢的才会是 `blob-bbb`；否则稳定排序会原样保留 SELECT 的行序，留下 `blob-aaa`。
#[tokio::test]
async fn tie_on_source_and_time_is_broken_deterministically() {
    let dir = tmp();
    let path = dir.join("fs.db");
    {
        let db = Db::open(&path).await.unwrap();
        for h in ["Tie.example", "tie.example"] {
            // 先插 aaa 后插 bbb；两行来源相同、last_seen 相同，只剩 blob 可比
            let blob = if h.starts_with('T') {
                "blob-aaa"
            } else {
                "blob-bbb"
            };
            seed_key(db.pool(), h, blob, "imported", "2024-05-06 07:08:09").await;
        }
    }
    let db = Db::open(&path).await.unwrap();
    assert_eq!(
        hosts_in_db(db.pool()).await,
        vec![("tie.example".to_string(), "blob-bbb".to_string())],
        "全平时须按 key_blob 字典序确定地择一，而不是听凭 SQLite 的行序"
    );
}

/// 审计2 #34：synchronous=FULL 是**拍过板**的耐久性决策，不是 builder 默认值碰巧。
///
/// 把 PRAGMA 的返回值钉住（FULL=2，Normal=1）：哪天有人图吞吐把它改回 Normal，
/// 这里立刻翻红，逼着下一任把「丢多少确认过的提交可以接受」重新写一遍理由。
#[tokio::test]
async fn synchronous_is_explicitly_full_not_normal() {
    let dir = tmp();
    let db = Db::open(&dir.join("fs.db")).await.unwrap();
    let v: i64 = sqlx::query_scalar("PRAGMA synchronous")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(
        v, 2,
        "synchronous 应为 FULL(=2)，实际 {v}：耐久性决策被静默回退了"
    );
}

/// 审计2 #31：新建的库文件必须 0600，且 -wal/-shm 旁文件继承同一权限位。
///
/// 权限检查必须赶在连接还活着的时候做：最后一次连接关闭时 SQLite 会做检查点并
/// **删除** -wal/-shm 旁文件，`drop(db)` 之后再查就只剩主文件，本测试的 WAL 一半
/// 会退化成「查一个不存在的文件」。
#[cfg(unix)]
#[tokio::test]
async fn a_fresh_db_and_its_sidecar_files_are_0600() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tmp();
    let path = dir.join("fs.db");
    let db = Db::open(&path).await.unwrap();
    // 让 WAL 旁文件确实落盘（WAL 模式下第一次写入才建出 -wal）
    sqlx::query("UPDATE meta SET value = value WHERE key = 'min_app_version'")
        .execute(db.pool())
        .await
        .unwrap();
    let wal = dir.join("fs.db-wal");
    assert!(
        wal.exists(),
        "取材失效：WAL 模式下写入后 -wal 旁文件应当存在——它不在，权限断言无从谈起"
    );
    for p in [&path, &wal] {
        let mode = std::fs::metadata(p)
            .unwrap_or_else(|e| panic!("读 {} 权限失败：{e}", p.display()))
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "{} 未按 0600 建出：{mode:o}", p.display());
    }
}

/// 审计2 #31：上一版本（umask 0644）建出来的库，重开时必须**就地收紧**——
/// 「只伺候新建路径」正是最容易写出来的半截修复。
#[cfg(unix)]
#[tokio::test]
async fn a_pre_existing_loose_db_is_tightened_on_open() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tmp();
    let path = dir.join("fs.db");
    std::fs::write(&path, b"").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    let db = Db::open(&path).await.unwrap();
    drop(db);
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "既有的 0644 库未被收紧：{mode:o}");
}

/// 审计2 #31：迁移前备份快照与库同密级——目录 0700、快照 0600。
#[cfg(unix)]
#[tokio::test]
async fn migration_backup_dir_and_snapshot_are_tightened() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tmp();
    let path = dir.join("fs.db");
    {
        let db = Db::open(&path).await.unwrap();
        // 回退 user_version 到 2，重开即触发一次迁移前备份
        sqlx::query("PRAGMA user_version = 2")
            .execute(db.pool())
            .await
            .unwrap();
    }
    Db::open(&path).await.unwrap();
    let backups = dir.join("backups");
    let dir_mode = std::fs::metadata(&backups).unwrap().permissions().mode() & 0o777;
    assert_eq!(dir_mode, 0o700, "备份目录未收紧到 0700：{dir_mode:o}");
    let snapshot = std::fs::read_dir(&backups)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("fs-v2-"))
        })
        .expect("取材失效：本应产出 fs-v2-*.db 快照");
    let file_mode = std::fs::metadata(&snapshot).unwrap().permissions().mode() & 0o777;
    assert_eq!(
        file_mode,
        0o600,
        "备份快照未收紧到 0600：{file_mode:o}（{}）",
        snapshot.display()
    );
}

/// 审计2 #31 的**顺序**守卫：权限收紧必须先于 SQLite connect——SQLite 的 -wal/-shm
/// 旁文件继承库文件本身的权限位（SQLite 文档），先 connect 再收紧，旁文件已经按
/// umask 落盘了，管住主文件也晚了。行为测试在 Windows 上观测不到任何权限位，
/// 这条只能按源码位置钉（与 app 侧单实例闸门守卫同一路数）。
#[test]
fn restrict_db_file_tightens_to_0600_and_runs_before_connect() {
    let src =
        std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/db.rs"))
            .unwrap();
    let call = src
        .find("restrict_db_file(path)?;")
        .expect("open() 里没有调用 restrict_db_file——审计2 #31 的收紧入口被删了");
    let connect = src
        .find("SqliteConnectOptions::new()")
        .expect("找不到 SqliteConnectOptions builder（改了排布请同步本测试）");
    assert!(
        call < connect,
        "权限收紧排在 SQLite connect 之后：-wal/-shm 已按 umask 落盘，管住主文件也晚了"
    );
    let start = src
        .find("fn restrict_db_file(")
        .expect("找不到 restrict_db_file 定义");
    let end = src[start..]
        .find("fn backup_failed(")
        .expect("找不到 restrict_db_file 之后的下一个自由函数");
    let body = &src[start..start + end];
    assert!(
        body.contains("Permissions::from_mode(0o600)"),
        "restrict_db_file 里没有 0600 收紧——Unix 上同机其他账户可读库文件"
    );
}

/// 审计2 #31：迁移前备份快照与库同密级。同样是 `#[cfg(unix)]` 块，Windows 上
/// 行为不可观测，按源码钉住「备份目录 0700、库文件与快照 0600 各一处」。
#[test]
fn migration_backup_perms_are_tightened_in_source() {
    let src =
        std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/db.rs"))
            .unwrap();
    assert_eq!(
        src.matches("Permissions::from_mode(0o700)").count(),
        1,
        "备份目录的 0700 收紧块应为 1 处（审计2 #31）"
    );
    assert_eq!(
        src.matches("Permissions::from_mode(0o600)").count(),
        2,
        "0600 收紧块应为 2 处：库文件（restrict_db_file）与迁移备份快照（审计2 #31）"
    );
}

fn tmp() -> std::path::PathBuf {
    common::tmpdir("connmgr")
}
