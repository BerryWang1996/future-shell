//! 计划任务的持久层（M4a）。判定逻辑在 [`crate::schedule`]，cron 解析在 [`crate::cron`]。

use crate::cron::Cron;
use crate::schedule::CatchUp;
use crate::Error;
use sqlx::SqlitePool;

/// 单条执行记录里 `detail` 的字符上限。
///
/// 远端 stderr 可以是几百兆（一个 `find /` 打满错误就够了）。记录的用途是「让人看出
/// 为什么失败」，首段就够；不截断的话一次失败能把库撑大到影响启动。
/// 按**字符**而不是字节截：按字节切会把一个中文字劈成半个，落库即是乱码。
pub const RUN_DETAIL_CHARS_MAX: usize = 2000;

/// 每个任务保留的执行记录条数上限。
///
/// 一条每分钟的任务一天就是 1440 条。留 200 条足够回答「最近怎么样」，
/// 而「三个月前那次为什么失败」不是这个面板要回答的问题。
pub const RUNS_PER_TASK_MAX: i64 = 200;

/// 任务名与命令的长度上限（与 `history_cmd` 的口径一致：存命令，不存粘贴进来的文件）。
pub const NAME_CHARS_MAX: usize = 200;
pub const COMMAND_CHARS_MAX: usize = 8 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, sqlx::FromRow)]
pub struct ScheduledTask {
    pub id: i64,
    pub name: String,
    pub cron: String,
    pub command: String,
    pub profile_id: String,
    pub enabled: bool,
    pub catchup: String,
    pub tz_offset_minutes: i64,
    pub tz_follows_dst: bool,
    pub checked_minute: i64,
    pub last_fire_minute: Option<i64>,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, sqlx::FromRow)]
pub struct ScheduledRun {
    pub id: i64,
    pub task_id: i64,
    pub fired_at: i64,
    pub outcome: String,
    pub catchup: bool,
    pub exit_code: Option<i64>,
    pub detail: String,
}

/// 新建/更新任务的入参。
///
/// 具名结构而不是一串位置参数：`name`/`cron`/`command`/`profile_id` 四个都是 `&str`，
/// 传错顺序编译器一声不响，而后果是一条 cron 写在 name 里、命令写在 cron 里的任务。
#[derive(Debug, Clone, Copy)]
pub struct TaskSpec<'a> {
    pub name: &'a str,
    pub cron: &'a str,
    pub command: &'a str,
    pub profile_id: &'a str,
    pub catchup: CatchUp,
    pub tz_offset_minutes: i32,
    pub tz_follows_dst: bool,
}

pub struct ScheduleRepo<'a> {
    pool: &'a SqlitePool,
}

impl<'a> ScheduleRepo<'a> {
    pub fn new(pool: &'a SqlitePool) -> Self {
        Self { pool }
    }

    /// 新建任务。返回新 id。
    ///
    /// **写入时就校验 cron**：一条 cron 不合法的任务会静默地永不触发，而用户以为它在跑。
    /// 这类「存下来了但永远不生效」是本仓反复修过的形状（写而不读的设置键），
    /// 所以在唯一的入口处拦掉。
    pub async fn create(&self, spec: TaskSpec<'_>, now: i64) -> Result<i64, Error> {
        let (name, command) = validate(spec)?;
        // 评估水位从创建时刻起步：任务不补跑它诞生之前的触发（见 schedule::Input）
        let row: (i64,) = sqlx::query_as(
            "INSERT INTO scheduled_tasks
               (name, cron, command, profile_id, enabled, catchup,
                tz_offset_minutes, tz_follows_dst, checked_minute, last_fire_minute, created_at)
             VALUES (?1, ?2, ?3, ?4, 1, ?5, ?6, ?7, ?8, NULL, ?9)
             RETURNING id",
        )
        .bind(name)
        .bind(spec.cron.trim())
        .bind(command)
        .bind(spec.profile_id)
        .bind(spec.catchup.as_str())
        .bind(spec.tz_offset_minutes)
        .bind(spec.tz_follows_dst)
        .bind(crate::civil::minute_index(now))
        .bind(now)
        .fetch_one(self.pool)
        .await?;
        Ok(row.0)
    }

    /// 改任务。cron 同样在此校验。
    ///
    /// 改动之后把**评估水位重置到当前分钟**：旧表达式的历史触发点对新表达式没有意义
    /// ——把「昨天 03:30 评估过」当作新 cron `*/5` 的水位，会让留痕/补跑窗口横跨一整天。
    /// `last_fire` 不动（它记录的是「上次真实触发」这个事实，与表达式无关）。
    pub async fn update(&self, id: i64, spec: TaskSpec<'_>, now: i64) -> Result<(), Error> {
        let (name, command) = validate(spec)?;
        let n = sqlx::query(
            "UPDATE scheduled_tasks SET
               name = ?2, cron = ?3, command = ?4, profile_id = ?5,
               catchup = ?6, tz_offset_minutes = ?7, tz_follows_dst = ?8,
               checked_minute = ?9
             WHERE id = ?1",
        )
        .bind(id)
        .bind(name)
        .bind(spec.cron.trim())
        .bind(command)
        .bind(spec.profile_id)
        .bind(spec.catchup.as_str())
        .bind(spec.tz_offset_minutes)
        .bind(spec.tz_follows_dst)
        .bind(crate::civil::minute_index(now))
        .execute(self.pool)
        .await?
        .rows_affected();
        if n == 0 {
            return Err(Error::NotFound(format!("计划任务 {id} 不存在")));
        }
        Ok(())
    }

    pub async fn list(&self) -> Result<Vec<ScheduledTask>, Error> {
        Ok(sqlx::query_as::<_, ScheduledTask>(
            "SELECT id, name, cron, command, profile_id, enabled, catchup,
                    tz_offset_minutes, tz_follows_dst, checked_minute, last_fire_minute, created_at
             FROM scheduled_tasks ORDER BY id",
        )
        .fetch_all(self.pool)
        .await?)
    }

    /// 启用/停用。
    ///
    /// **启用时把评估水位推到当前分钟**：停用意味着「这段时间别跑」，用户早上十点
    /// 重新启用一条凌晨三点的任务，几乎不可能是想让它立刻补跑凌晨那次。
    /// 这条决定放在这里而不是判定函数里——它是产品语义（「停用期间不算错过」），
    /// 而 `schedule::decide` 只该管时间算术。
    pub async fn set_enabled(&self, id: i64, enabled: bool, now: i64) -> Result<(), Error> {
        let now_minute = crate::civil::minute_index(now);
        let n = if enabled {
            sqlx::query("UPDATE scheduled_tasks SET enabled = 1, checked_minute = ?2 WHERE id = ?1")
                .bind(id)
                .bind(now_minute)
                .execute(self.pool)
                .await?
                .rows_affected()
        } else {
            sqlx::query("UPDATE scheduled_tasks SET enabled = 0 WHERE id = ?1")
                .bind(id)
                .execute(self.pool)
                .await?
                .rows_affected()
        };
        if n == 0 {
            return Err(Error::NotFound(format!("计划任务 {id} 不存在")));
        }
        Ok(())
    }

    /// 删任务，连带它的执行记录。
    ///
    /// 显式删两张表而不靠 `ON DELETE CASCADE`：SQLite 的外键约束默认**关闭**，
    /// 要靠每个连接 `PRAGMA foreign_keys=ON`，而本仓没有统一开启。写了 CASCADE
    /// 却没开 pragma 的结果是记录变成孤儿——看起来做了级联，实际没有。
    pub async fn delete(&self, id: i64) -> Result<(), Error> {
        sqlx::query("DELETE FROM scheduled_runs WHERE task_id = ?1")
            .bind(id)
            .execute(self.pool)
            .await?;
        sqlx::query("DELETE FROM scheduled_tasks WHERE id = ?1")
            .bind(id)
            .execute(self.pool)
            .await?;
        Ok(())
    }

    /// 触发落地：评估水位推到本分钟，`last_fire` 一并写。
    ///
    /// 两个更新在**同一条语句**里：崩溃只会发生在「整条都没落」或「整条都落了」之间，
    /// 不会出现水位推了而上次触发没记（或反过来）的中间态。
    ///
    /// 写库在执行**之前**（先记后跑）：命令可能跑十分钟，而 tick 每 2 秒一拍，
    /// 不先落的话这一分钟内会被反复判成该触发。代价如实记下——先落之后、进程被杀
    /// 之前的那个窗口里这次触发会**至多跑零次**（`last_fire` 显示触发过但没有执行
    /// 记录，是一个可被用户察觉的异常组合，不是静默丢失）。取「至多一次」而非
    /// 「至少一次」：重复执行一条非幂等的命令（删旧备份再重建）比漏跑一次更危险。
    pub async fn commit_fire(&self, id: i64, minute: i64) -> Result<(), Error> {
        sqlx::query(
            "UPDATE scheduled_tasks SET
               checked_minute = MAX(checked_minute, ?2),
               last_fire_minute = ?2
             WHERE id = ?1",
        )
        .bind(id)
        .bind(minute)
        .execute(self.pool)
        .await?;
        Ok(())
    }

    /// 推评估水位（MAX 语义：只升不降）。
    ///
    /// 时钟回拨期间调用方不会推（判定返回 Paused）；即便有代码失误带着更低的值来了，
    /// MAX 也保证水位不被拉低——拉低水位等于重放已处置过的触发点。
    pub async fn advance_checked(&self, id: i64, minute: i64) -> Result<(), Error> {
        sqlx::query(
            "UPDATE scheduled_tasks SET checked_minute = MAX(checked_minute, ?2) WHERE id = ?1",
        )
        .bind(id)
        .bind(minute)
        .execute(self.pool)
        .await?;
        Ok(())
    }

    /// Skip 策略的留痕：**单事务**里落一条 `skipped` 记录并推水位。
    ///
    /// 必须同事务：留痕落了水位没推 → 每 2 秒重复落一条；水位推了留痕没落 → 这次
    /// 「按策略跳过」永久无声。任一半边单独存在都比不分事务更坏。
    pub async fn record_skip_and_advance(
        &self,
        task_id: i64,
        fired_at: i64,
        detail: &str,
        minute: i64,
    ) -> Result<(), Error> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "INSERT INTO scheduled_runs (task_id, fired_at, outcome, catchup, exit_code, detail)
             VALUES (?1, ?2, 'skipped', 0, NULL, ?3)",
        )
        .bind(task_id)
        .bind(fired_at)
        .bind(truncate_chars(detail, RUN_DETAIL_CHARS_MAX))
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE scheduled_tasks SET checked_minute = MAX(checked_minute, ?2) WHERE id = ?1",
        )
        .bind(task_id)
        .bind(minute)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// 追加一条执行结果记录，并把该任务的记录裁到上限。
    pub async fn record_run(
        &self,
        task_id: i64,
        fired_at: i64,
        outcome: &str,
        catchup: bool,
        exit_code: Option<i32>,
        detail: &str,
    ) -> Result<(), Error> {
        sqlx::query(
            "INSERT INTO scheduled_runs (task_id, fired_at, outcome, catchup, exit_code, detail)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )
        .bind(task_id)
        .bind(fired_at)
        .bind(outcome)
        .bind(catchup)
        .bind(exit_code)
        .bind(truncate_chars(detail, RUN_DETAIL_CHARS_MAX))
        .execute(self.pool)
        .await?;
        // 只裁**该任务**的记录：按全表裁会让一条高频任务把别的任务的历史挤光
        sqlx::query(
            "DELETE FROM scheduled_runs WHERE task_id = ?1 AND id NOT IN (
               SELECT id FROM scheduled_runs WHERE task_id = ?1
               ORDER BY fired_at DESC, id DESC LIMIT ?2
             )",
        )
        .bind(task_id)
        .bind(RUNS_PER_TASK_MAX)
        .execute(self.pool)
        .await?;
        Ok(())
    }

    pub async fn recent_runs(&self, task_id: i64, limit: i64) -> Result<Vec<ScheduledRun>, Error> {
        Ok(sqlx::query_as::<_, ScheduledRun>(
            "SELECT id, task_id, fired_at, outcome, catchup, exit_code, detail
             FROM scheduled_runs WHERE task_id = ?1
             ORDER BY fired_at DESC, id DESC LIMIT ?2",
        )
        .bind(task_id)
        .bind(limit)
        .fetch_all(self.pool)
        .await?)
    }

    pub async fn count_runs(&self, task_id: i64) -> Result<i64, Error> {
        let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM scheduled_runs WHERE task_id = ?1")
            .bind(task_id)
            .fetch_one(self.pool)
            .await?;
        Ok(n)
    }
}

/// 校验并归一 spec，返回裁剪后的 (name, command)。
fn validate<'a>(spec: TaskSpec<'a>) -> Result<(&'a str, &'a str), Error> {
    let name = spec.name.trim();
    if name.is_empty() {
        return Err(Error::Validation("任务名不能为空".into()));
    }
    if name.chars().count() > NAME_CHARS_MAX {
        return Err(Error::Validation(format!(
            "任务名过长（上限 {NAME_CHARS_MAX} 字符）"
        )));
    }
    let command = spec.command.trim();
    if command.is_empty() {
        return Err(Error::Validation("命令不能为空".into()));
    }
    if command.chars().count() > COMMAND_CHARS_MAX {
        return Err(Error::Validation(format!(
            "命令过长（上限 {COMMAND_CHARS_MAX} 字符）"
        )));
    }
    if spec.profile_id.trim().is_empty() {
        return Err(Error::Validation("必须指定目标连接".into()));
    }
    // cron 在此校验：不合法的 cron 存下去就是一条静默永不触发的任务
    Cron::parse(spec.cron)?;
    // 偏移必须是真实存在的时区范围（UTC-12..=UTC+14）
    if !(-12 * 60..=14 * 60).contains(&spec.tz_offset_minutes) {
        return Err(Error::Validation(format!(
            "时区偏移越界：{} 分钟（允许 -720..=840）",
            spec.tz_offset_minutes
        )));
    }
    Ok((name, command))
}

/// 按**字符**截断（不按字节）。按字节切会把一个中文字劈成半个，落库即是乱码。
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn pool() -> SqlitePool {
        let p = SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::migrate!("./migrations").run(&p).await.unwrap();
        p
    }

    fn spec<'a>(name: &'a str, cron: &'a str, command: &'a str) -> TaskSpec<'a> {
        TaskSpec {
            name,
            cron,
            command,
            profile_id: "p1",
            catchup: CatchUp::Skip,
            tz_offset_minutes: 8 * 60,
            tz_follows_dst: false,
        }
    }

    #[tokio::test]
    async fn create_list_update_delete() {
        let p = pool().await;
        let r = ScheduleRepo::new(&p);
        // 创建于 now=600_000（分钟序号 10000）
        let id = r
            .create(spec("每夜备份", "30 3 * * *", "/opt/backup.sh"), 600_000)
            .await
            .unwrap();
        let all = r.list().await.unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].name, "每夜备份");
        assert_eq!(all[0].cron, "30 3 * * *");
        assert!(all[0].enabled, "新建的任务默认启用");
        assert_eq!(all[0].catchup, "skip");
        assert_eq!(all[0].tz_offset_minutes, 480);
        assert!(!all[0].tz_follows_dst, "默认固定偏移");
        assert_eq!(
            all[0].checked_minute, 10_000,
            "评估水位应从创建时刻起步（任务不补跑史前）"
        );
        assert_eq!(all[0].last_fire_minute, None, "从未触发");

        r.update(
            id,
            TaskSpec {
                catchup: CatchUp::Once,
                tz_follows_dst: true,
                ..spec("改个名", "0 4 * * 1", "/opt/weekly.sh")
            },
            900_000,
        )
        .await
        .unwrap();
        let one = &r.list().await.unwrap()[0];
        assert_eq!(
            (one.name.as_str(), one.cron.as_str()),
            ("改个名", "0 4 * * 1")
        );
        assert_eq!(one.catchup, "once");
        assert!(one.tz_follows_dst);
        assert_eq!(one.checked_minute, 15_000, "改表达式须重置水位到当前分钟");
        assert_eq!(
            one.last_fire_minute, None,
            "last_fire 记录的是事实，不随表达式重置"
        );

        r.delete(id).await.unwrap();
        assert!(r.list().await.unwrap().is_empty());
    }

    /// 改不存在的任务要报 NotFound，而不是静默成功。
    #[tokio::test]
    async fn update_missing_task_reports_not_found() {
        let p = pool().await;
        let r = ScheduleRepo::new(&p);
        let e = r
            .update(999, spec("n", "* * * * *", "c"), 0)
            .await
            .unwrap_err();
        assert!(matches!(e, Error::NotFound(_)), "实得 {e:?}");
        let e2 = r.set_enabled(999, true, 0).await.unwrap_err();
        assert!(matches!(e2, Error::NotFound(_)), "实得 {e2:?}");
    }

    /// **cron 不合法必须在写入时被拒**。
    ///
    /// 存下去就是一条静默永不触发的任务，而用户以为它在跑——本仓反复修过这个形状
    /// （「写而不读」的设置键）。
    #[tokio::test]
    async fn invalid_cron_is_rejected_at_write_time() {
        let p = pool().await;
        let r = ScheduleRepo::new(&p);
        for bad in ["", "* * * *", "60 * * * *", "@daily", "0 0 * * FRI"] {
            let e = r.create(spec("n", bad, "cmd"), 0).await.unwrap_err();
            assert!(
                matches!(e, Error::Validation(_)),
                "cron {bad:?} 应被拒，实得 {e:?}"
            );
        }
        assert!(r.list().await.unwrap().is_empty(), "被拒的任务不得入库");
    }

    /// 空名/空命令/空目标连接都拒。
    #[tokio::test]
    async fn empty_fields_are_rejected() {
        let p = pool().await;
        let r = ScheduleRepo::new(&p);
        assert!(
            r.create(spec("", "* * * * *", "c"), 0).await.is_err(),
            "空名应拒"
        );
        assert!(
            r.create(spec("  ", "* * * * *", "c"), 0).await.is_err(),
            "纯空白名应拒"
        );
        assert!(
            r.create(spec("n", "* * * * *", ""), 0).await.is_err(),
            "空命令应拒"
        );
        assert!(
            r.create(
                TaskSpec {
                    profile_id: "  ",
                    ..spec("n", "* * * * *", "c")
                },
                0
            )
            .await
            .is_err(),
            "空目标连接应拒"
        );
    }

    /// 时区偏移必须落在真实存在的范围内。
    #[tokio::test]
    async fn absurd_timezone_offsets_are_rejected() {
        let p = pool().await;
        let r = ScheduleRepo::new(&p);
        for bad in [-13 * 60, 15 * 60, 100_000, -100_000] {
            assert!(
                r.create(
                    TaskSpec {
                        tz_offset_minutes: bad,
                        ..spec("n", "* * * * *", "c")
                    },
                    0
                )
                .await
                .is_err(),
                "偏移 {bad} 分钟应被拒"
            );
        }
        // 边界内的极端时区照收（UTC+14 是 Kiritimati，真实存在）
        for ok in [-12 * 60, 14 * 60, 5 * 60 + 45] {
            assert!(
                r.create(
                    TaskSpec {
                        tz_offset_minutes: ok,
                        ..spec("n", "* * * * *", "c")
                    },
                    0
                )
                .await
                .is_ok(),
                "偏移 {ok} 分钟是真实时区，应接受"
            );
        }
    }

    /// 启用时把评估水位推到当前分钟——停用期间不算错过。
    ///
    /// 用户早上十点重新启用一条凌晨三点的任务，几乎不可能是想让它立刻补跑凌晨那次。
    #[tokio::test]
    async fn enabling_does_not_catch_up_the_disabled_period() {
        let p = pool().await;
        let r = ScheduleRepo::new(&p);
        let id = r.create(spec("t", "30 3 * * *", "c"), 0).await.unwrap();
        r.commit_fire(id, 100).await.unwrap();
        r.set_enabled(id, false, 0).await.unwrap();
        assert!(!r.list().await.unwrap()[0].enabled);
        assert_eq!(
            r.list().await.unwrap()[0].checked_minute,
            100,
            "停用本身不该动水位"
        );
        r.set_enabled(id, true, 99_999 * 60).await.unwrap();
        let t = &r.list().await.unwrap()[0];
        assert!(t.enabled);
        assert_eq!(
            t.checked_minute, 99_999,
            "启用时须把水位推到当前分钟，否则会补跑停用期间错过的那些"
        );
        // last_fire 保持为真实触发过的那一刻（展示不撒谎）
        assert_eq!(t.last_fire_minute, Some(100));
    }

    /// 水位只升不降（MAX 语义）——拉低水位等于重放已处置过的触发点。
    #[tokio::test]
    async fn checked_minute_never_goes_down() {
        let p = pool().await;
        let r = ScheduleRepo::new(&p);
        let id = r
            .create(spec("t", "* * * * *", "c"), 100 * 60)
            .await
            .unwrap();
        r.advance_checked(id, 500).await.unwrap();
        assert_eq!(r.list().await.unwrap()[0].checked_minute, 500);
        // 失误带了更低的值来：MAX 保住原水位
        r.advance_checked(id, 300).await.unwrap();
        assert_eq!(
            r.list().await.unwrap()[0].checked_minute,
            500,
            "水位被拉低就是重放，MAX 必须挡住"
        );
        r.advance_checked(id, 700).await.unwrap();
        assert_eq!(r.list().await.unwrap()[0].checked_minute, 700);
        // commit_fire 同样是 MAX 语义
        r.commit_fire(id, 600).await.unwrap();
        assert_eq!(
            r.list().await.unwrap()[0].checked_minute,
            700,
            "触发也不得拉低水位"
        );
    }

    /// Skip 留痕与推水位是**一个事务**：两个半边单独存在都比不分事务更坏。
    #[tokio::test]
    async fn record_skip_and_advance_is_atomic_and_leaves_one_row() {
        let p = pool().await;
        let r = ScheduleRepo::new(&p);
        let id = r.create(spec("t", "*/10 * * * *", "c"), 0).await.unwrap();
        r.record_skip_and_advance(id, 123_456, "已按策略跳过 3 次", 10_000)
            .await
            .unwrap();
        let t = &r.list().await.unwrap()[0];
        assert_eq!(t.checked_minute, 10_000, "留痕与推水位同生");
        let runs = r.recent_runs(id, 10).await.unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].outcome, "skipped");
        assert!(runs[0].detail.contains("跳过 3 次"));
        assert_eq!(runs[0].exit_code, None, "没跑起来时退出码是 NULL 而非 0");
    }

    /// 执行记录三态分开存，且能按时间倒序取回。
    #[tokio::test]
    async fn runs_are_recorded_with_distinct_outcomes() {
        let p = pool().await;
        let r = ScheduleRepo::new(&p);
        let id = r.create(spec("t", "* * * * *", "c"), 0).await.unwrap();
        r.record_run(id, 100, "ok", false, Some(0), "")
            .await
            .unwrap();
        r.record_run(id, 200, "failed", false, Some(1), "permission denied")
            .await
            .unwrap();
        r.record_run(id, 300, "skipped", true, None, "没有该连接的活动会话")
            .await
            .unwrap();

        let runs = r.recent_runs(id, 10).await.unwrap();
        assert_eq!(runs.len(), 3);
        assert_eq!(runs[0].fired_at, 300, "最近的排最前");
        assert_eq!(runs[0].outcome, "skipped");
        assert!(runs[0].catchup, "补跑标记须存下来");
        assert_eq!(
            runs[0].exit_code, None,
            "没跑起来时退出码是 None 而不是 0——0 是「成功」这个有意义的值"
        );
        assert_eq!(runs[1].outcome, "failed");
        assert_eq!(runs[1].exit_code, Some(1));
        assert!(
            runs[1].detail.contains("permission denied"),
            "失败原因须带出来"
        );
    }

    /// `detail` 按字符截断，不劈开中文。
    #[tokio::test]
    async fn detail_is_truncated_by_chars_not_bytes() {
        let p = pool().await;
        let r = ScheduleRepo::new(&p);
        let id = r.create(spec("t", "* * * * *", "c"), 0).await.unwrap();
        let long = "错".repeat(RUN_DETAIL_CHARS_MAX + 500);
        r.record_run(id, 1, "failed", false, Some(2), &long)
            .await
            .unwrap();
        let got = &r.recent_runs(id, 1).await.unwrap()[0].detail;
        assert_eq!(got.chars().count(), RUN_DETAIL_CHARS_MAX, "应截到字符上限");
        // 按字节截会切出半个字，`chars().count()` 之外还要保证内容没坏
        assert!(got.chars().all(|c| c == '错'), "截断后不得出现坏字符");
    }

    /// 记录数按**每个任务**裁到上限，且不影响其他任务。
    #[tokio::test]
    async fn runs_are_pruned_per_task() {
        let p = pool().await;
        let r = ScheduleRepo::new(&p);
        let a = r.create(spec("a", "* * * * *", "c"), 0).await.unwrap();
        let b = r.create(spec("b", "* * * * *", "c"), 0).await.unwrap();
        for i in 0..(RUNS_PER_TASK_MAX + 10) {
            r.record_run(a, i, "ok", false, Some(0), "").await.unwrap();
        }
        r.record_run(b, 1, "ok", false, Some(0), "").await.unwrap();
        assert_eq!(
            r.count_runs(a).await.unwrap(),
            RUNS_PER_TASK_MAX,
            "该任务裁到上限"
        );
        assert_eq!(
            r.count_runs(b).await.unwrap(),
            1,
            "另一个任务的记录不得被高频任务挤掉"
        );
        // 留下的是最近的那些
        let newest = r.recent_runs(a, 1).await.unwrap();
        assert_eq!(newest[0].fired_at, RUNS_PER_TASK_MAX + 9);
    }

    /// 删任务连带删记录——不留孤儿。
    ///
    /// 显式删两张表而不靠 CASCADE：SQLite 的外键默认关闭，写了 CASCADE 却没开
    /// pragma 的结果是「看起来做了级联，实际没有」。
    #[tokio::test]
    async fn deleting_a_task_removes_its_runs() {
        let p = pool().await;
        let r = ScheduleRepo::new(&p);
        let id = r.create(spec("t", "* * * * *", "c"), 0).await.unwrap();
        r.record_run(id, 1, "ok", false, Some(0), "").await.unwrap();
        assert_eq!(r.count_runs(id).await.unwrap(), 1);
        r.delete(id).await.unwrap();
        assert_eq!(r.count_runs(id).await.unwrap(), 0, "记录不得成为孤儿");
    }

    #[test]
    fn truncate_chars_handles_multibyte_and_short_input() {
        assert_eq!(truncate_chars("abc", 10), "abc");
        assert_eq!(truncate_chars("abcdef", 3), "abc");
        assert_eq!(truncate_chars("中文测试", 2), "中文");
        assert_eq!(truncate_chars("", 5), "");
    }
}
