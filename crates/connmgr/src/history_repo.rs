//! 本地命令历史库（M4a：历史命令 UI 的持久层）。
//!
//! 两个来源，必须可区分（见 `0005_command_history.sql` 的 `source` 列注）：
//! `sent` = 本程序亲手发出的字节，可以原样重发；`grid` = 从终端回滚里启发式提取，
//! 近似值。把两者混成一堆的结果是用户重发一条带提示符残渣的「命令」。

use crate::Error;
use sqlx::SqlitePool;

/// 历史库条数上限。
///
/// 有上限不是为了省磁盘（几万条命令也就几 MB），是为了检索**有用**：无界增长的
/// 历史里翻出来的多是几个月前的一次性命令，而人要找的几乎总在最近几千条内。
/// 超限时按 `used_at` 最旧的先删。
pub const HISTORY_CAP: i64 = 5000;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, sqlx::FromRow)]
pub struct HistoryEntry {
    pub id: i64,
    pub command: String,
    pub host: String,
    pub profile_id: String,
    pub source: String,
    pub used_at: i64,
    pub use_count: i64,
}

pub struct HistoryRepo<'a> {
    pool: &'a SqlitePool,
}

impl<'a> HistoryRepo<'a> {
    pub fn new(pool: &'a SqlitePool) -> Self {
        Self { pool }
    }

    /// 记一条命令。同 (命令, 主机, 来源) 已存在则**只更新时间与次数**。
    ///
    /// `now` 由调用方注入（unix 秒）：本 crate 不引时间库，且注入让「同一秒内的
    /// 多次记录」在测试里可控。
    ///
    /// 空白命令直接忽略：回车空行、只按了 Ctrl-C 的行都会走到这里，存进去只是
    /// 给检索结果添噪。
    pub async fn record(
        &self,
        command: &str,
        host: &str,
        profile_id: &str,
        source: &str,
        now: i64,
    ) -> Result<(), Error> {
        let cmd = command.trim();
        if cmd.is_empty() {
            return Ok(());
        }
        sqlx::query(
            "INSERT INTO command_history (command, host, profile_id, source, used_at, use_count)
             VALUES (?1, ?2, ?3, ?4, ?5, 1)
             ON CONFLICT(command, host, source) DO UPDATE SET
               used_at = ?5,
               use_count = use_count + 1,
               -- profile_id 跟着最近一次更新：档案可能被重建（换了 uuid），
               -- 而「最近一次是从哪个档案发的」才是跳回连接时有用的那个。
               profile_id = ?3",
        )
        .bind(cmd)
        .bind(host)
        .bind(profile_id)
        .bind(source)
        .bind(now)
        .execute(self.pool)
        .await?;
        self.prune().await
    }

    /// 检索。`query` 为空则按最近使用倒序返回全部（截到 `limit`）。
    ///
    /// 匹配用 `LIKE %q%` 并**转义 LIKE 的通配符**：用户搜 `100%` 时不转义会变成
    /// 「以 100 开头的任意串」，把无关条目也捞出来；搜 `_` 更是命中一切。
    pub async fn search(
        &self,
        query: &str,
        host: Option<&str>,
        limit: i64,
    ) -> Result<Vec<HistoryEntry>, Error> {
        let q = query.trim();
        let pattern = format!("%{}%", escape_like(q));
        // ESCAPE '\' 必须显式声明，SQLite 默认没有转义字符
        let rows = if let Some(h) = host {
            sqlx::query_as::<_, HistoryEntry>(
                "SELECT id, command, host, profile_id, source, used_at, use_count
                 FROM command_history
                 WHERE command LIKE ?1 ESCAPE '\\' AND host = ?2
                 ORDER BY used_at DESC LIMIT ?3",
            )
            .bind(&pattern)
            .bind(h)
            .bind(limit)
            .fetch_all(self.pool)
            .await?
        } else {
            sqlx::query_as::<_, HistoryEntry>(
                "SELECT id, command, host, profile_id, source, used_at, use_count
                 FROM command_history
                 WHERE command LIKE ?1 ESCAPE '\\'
                 ORDER BY used_at DESC LIMIT ?2",
            )
            .bind(&pattern)
            .bind(limit)
            .fetch_all(self.pool)
            .await?
        };
        Ok(rows)
    }

    /// 删一条（用户在面板上剔掉误记的条目）。
    pub async fn delete(&self, id: i64) -> Result<(), Error> {
        sqlx::query("DELETE FROM command_history WHERE id = ?1")
            .bind(id)
            .execute(self.pool)
            .await?;
        Ok(())
    }

    /// 清空全部（隐私操作：历史里可能有带口令的命令行）。
    pub async fn clear(&self) -> Result<(), Error> {
        sqlx::query("DELETE FROM command_history")
            .execute(self.pool)
            .await?;
        Ok(())
    }

    pub async fn count(&self) -> Result<i64, Error> {
        let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM command_history")
            .fetch_one(self.pool)
            .await?;
        Ok(n)
    }

    /// 超出 [`HISTORY_CAP`] 时删掉最旧的若干条。
    async fn prune(&self) -> Result<(), Error> {
        sqlx::query(
            "DELETE FROM command_history WHERE id IN (
               SELECT id FROM command_history ORDER BY used_at ASC, id ASC
               LIMIT MAX(0, (SELECT COUNT(*) FROM command_history) - ?1)
             )",
        )
        .bind(HISTORY_CAP)
        .execute(self.pool)
        .await?;
        Ok(())
    }
}

/// 转义 `LIKE` 的通配符（`%`、`_`）与转义符本身。
///
/// 不转义的后果不是「搜不到」而是「搜出一堆无关的」：搜 `100%` 会变成「以 100
/// 开头的任意串」，搜 `_` 命中一切。后者尤其常见——运维命令里下划线满地都是。
pub fn escape_like(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c == '\\' || c == '%' || c == '_' {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn repo_pool() -> SqlitePool {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool
    }

    /// 同一条命令重复执行只更新时间与次数，不新增行。
    ///
    /// 不去重的历史库里绝大部分是重复项，检索结果会被同一条 `ls` 占满。
    #[tokio::test]
    async fn repeated_command_bumps_count_instead_of_adding_rows() {
        let pool = repo_pool().await;
        let r = HistoryRepo::new(&pool);
        r.record("ls -la", "web-01", "p1", "sent", 100)
            .await
            .unwrap();
        r.record("ls -la", "web-01", "p1", "sent", 200)
            .await
            .unwrap();
        r.record("ls -la", "web-01", "p1", "sent", 300)
            .await
            .unwrap();
        assert_eq!(r.count().await.unwrap(), 1);
        let hits = r.search("ls", None, 10).await.unwrap();
        assert_eq!(hits[0].use_count, 3);
        assert_eq!(hits[0].used_at, 300, "时间须跟到最近一次");
    }

    /// 不同主机上的同一条命令是两条独立历史——「在哪台机器上跑过」是有用信息。
    #[tokio::test]
    async fn same_command_on_different_hosts_are_separate() {
        let pool = repo_pool().await;
        let r = HistoryRepo::new(&pool);
        r.record("systemctl restart nginx", "web-01", "p1", "sent", 100)
            .await
            .unwrap();
        r.record("systemctl restart nginx", "web-02", "p2", "sent", 110)
            .await
            .unwrap();
        assert_eq!(r.count().await.unwrap(), 2);
        let only_01 = r.search("nginx", Some("web-01"), 10).await.unwrap();
        assert_eq!(only_01.len(), 1);
        assert_eq!(only_01[0].host, "web-01");
    }

    /// 主机未知时用 ''（不是 NULL）才能正常去重。
    ///
    /// SQLite 的唯一索引把 NULL 当彼此不同：用 NULL 的话「同一条命令在未知主机上
    /// 跑过 100 次」会变成 100 行，去重整体失效。
    #[tokio::test]
    async fn unknown_host_still_dedupes() {
        let pool = repo_pool().await;
        let r = HistoryRepo::new(&pool);
        for t in [1, 2, 3] {
            r.record("echo hi", "", "", "sent", t).await.unwrap();
        }
        assert_eq!(r.count().await.unwrap(), 1, "空主机也必须去重");
        assert_eq!(r.search("", None, 10).await.unwrap()[0].use_count, 3);
    }

    /// 两个来源分开存：`grid` 提取的近似值不能与亲手发出的字节混为一条。
    #[tokio::test]
    async fn sources_are_kept_distinct() {
        let pool = repo_pool().await;
        let r = HistoryRepo::new(&pool);
        r.record("df -h", "h", "p", "sent", 10).await.unwrap();
        r.record("df -h", "h", "p", "grid", 20).await.unwrap();
        assert_eq!(r.count().await.unwrap(), 2);
        let all = r.search("df", None, 10).await.unwrap();
        let sources: Vec<&str> = all.iter().map(|e| e.source.as_str()).collect();
        assert!(sources.contains(&"sent") && sources.contains(&"grid"));
    }

    /// 空白命令不入库（回车空行、只按了 Ctrl-C 的行）。
    #[tokio::test]
    async fn blank_commands_are_ignored() {
        let pool = repo_pool().await;
        let r = HistoryRepo::new(&pool);
        for c in ["", "   ", "\t", "\n"] {
            r.record(c, "h", "p", "sent", 1).await.unwrap();
        }
        assert_eq!(r.count().await.unwrap(), 0);
    }

    /// 命令首尾空白被裁掉后去重——否则 `ls ` 与 `ls` 是两条。
    #[tokio::test]
    async fn command_is_trimmed_before_dedupe() {
        let pool = repo_pool().await;
        let r = HistoryRepo::new(&pool);
        r.record("  ls  ", "h", "p", "sent", 1).await.unwrap();
        r.record("ls", "h", "p", "sent", 2).await.unwrap();
        assert_eq!(r.count().await.unwrap(), 1);
        assert_eq!(r.search("", None, 5).await.unwrap()[0].command, "ls");
    }

    /// 检索按最近使用倒序。
    #[tokio::test]
    async fn search_orders_by_recency() {
        let pool = repo_pool().await;
        let r = HistoryRepo::new(&pool);
        r.record("first", "h", "p", "sent", 10).await.unwrap();
        r.record("second", "h", "p", "sent", 20).await.unwrap();
        r.record("third", "h", "p", "sent", 30).await.unwrap();
        let cmds: Vec<String> = r
            .search("", None, 10)
            .await
            .unwrap()
            .into_iter()
            .map(|e| e.command)
            .collect();
        assert_eq!(cmds, vec!["third", "second", "first"]);
    }

    /// LIKE 通配符必须被转义，否则「搜出一堆无关的」。
    ///
    /// 搜 `_` 不转义会命中一切——而运维命令里下划线满地都是。
    #[tokio::test]
    async fn like_wildcards_are_escaped() {
        let pool = repo_pool().await;
        let r = HistoryRepo::new(&pool);
        r.record("df -h | grep 100%", "h", "p", "sent", 1)
            .await
            .unwrap();
        r.record("echo abc", "h", "p", "sent", 2).await.unwrap();
        r.record("cat my_file", "h", "p", "sent", 3).await.unwrap();
        r.record("cat myXfile", "h", "p", "sent", 4).await.unwrap();

        // `%` 必须当字面量：不转义时 "100%" → LIKE '%100%%' 会额外命中别的
        let hits = r.search("100%", None, 10).await.unwrap();
        assert_eq!(hits.len(), 1, "实得 {hits:?}");
        assert!(hits[0].command.contains("100%"));

        // `_` 必须当字面量：不转义时会把 myXfile 也捞出来
        let hits = r.search("my_file", None, 10).await.unwrap();
        assert_eq!(
            hits.iter().map(|e| e.command.as_str()).collect::<Vec<_>>(),
            vec!["cat my_file"],
            "下划线须当字面量，不得命中 myXfile"
        );
    }

    #[test]
    fn escape_like_covers_all_three() {
        assert_eq!(escape_like("100%"), "100\\%");
        assert_eq!(escape_like("a_b"), "a\\_b");
        assert_eq!(escape_like("c:\\path"), "c:\\\\path");
        assert_eq!(escape_like("plain"), "plain");
    }

    /// 超上限时删最旧的，且删的是**最旧**而不是随便几条。
    #[tokio::test]
    async fn prune_drops_the_oldest_beyond_cap() {
        let pool = repo_pool().await;
        let r = HistoryRepo::new(&pool);
        // 直接灌到超上限：每条命令不同以免被去重吃掉
        for i in 0..(HISTORY_CAP + 5) {
            r.record(&format!("cmd{i}"), "h", "p", "sent", i)
                .await
                .unwrap();
        }
        assert_eq!(r.count().await.unwrap(), HISTORY_CAP, "须裁到上限");
        // 最旧的 5 条（cmd0..cmd4）应已消失，最新的仍在
        assert!(
            r.search("cmd0", None, 5).await.unwrap().is_empty(),
            "最旧的应被删"
        );
        assert!(
            !r.search(&format!("cmd{}", HISTORY_CAP + 4), None, 5)
                .await
                .unwrap()
                .is_empty(),
            "最新的必须留着"
        );
    }

    /// 删除与清空。
    #[tokio::test]
    async fn delete_and_clear() {
        let pool = repo_pool().await;
        let r = HistoryRepo::new(&pool);
        r.record("a", "h", "p", "sent", 1).await.unwrap();
        r.record("b", "h", "p", "sent", 2).await.unwrap();
        let id = r.search("a", None, 1).await.unwrap()[0].id;
        r.delete(id).await.unwrap();
        assert_eq!(r.count().await.unwrap(), 1);
        r.clear().await.unwrap();
        assert_eq!(r.count().await.unwrap(), 0);
    }

    /// limit 生效（面板一次只渲染有限条）。
    #[tokio::test]
    async fn limit_is_honored() {
        let pool = repo_pool().await;
        let r = HistoryRepo::new(&pool);
        for i in 0..10 {
            r.record(&format!("c{i}"), "h", "p", "sent", i)
                .await
                .unwrap();
        }
        assert_eq!(r.search("", None, 3).await.unwrap().len(), 3);
    }
}
