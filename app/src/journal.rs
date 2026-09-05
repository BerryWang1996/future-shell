//! 会话日志（journal）—— **非致命旁路**，不是会话状态的权威来源（审计 P1-12）。
//!
//! 唯一用途：进程异常退出（崩溃 / 断电 / 任务管理器结束）后，下次启动时提示「上次有 N 个会话
//! 没有正常关闭，是否恢复」。它服务的是一个**尽力而为**的便利功能，误差（多一行、少一行、
//! 时间戳偏一点）对用户完全可容忍。
//!
//! 权威状态源是 `SessionRegistry`：会话是否真的存在、是否真的关掉了，只以注册表为准。
//!
//! 由此得出本模块必须遵守的契约——**journal 写失败绝不能反过来否决真实的会话状态**：
//! - 原实现里 `session_open` 在会话已经装配成功、注册表已登记之后才 `mark_open`，一旦这句
//!   写库失败就把整个 IPC 判成失败。前端据此认为「连接没建起来」，不画标签、不做后续清理，
//!   可后端的 live session 还在跑——一个前端无从感知、也无从关闭的孤儿会话（连它的
//!   session_id 都没回给前端）。
//! - 对偶的错误在 `session_close`：先真移除了会话，再 `mark_closed`；写失败让 IPC 返回 Err，
//!   前端以为「关闭失败」而保留标签，实际连接早已断开，标签点什么都没反应。
//!
//! 故对外只暴露 `*_best_effort` 两个入口：失败一律 `tracing::error!` 落日志、**不改变调用方的
//! 返回值**。写失败的真实后果仅仅是「下次启动的恢复提示不准」，与之相比，让一次 SQLite 写错误
//! 去污染会话生命周期的正确性是完全不成比例的代价。
//! 底层的 `mark_open`/`mark_closed` 仍保留 `Result` 供测试与将来可能的严格路径使用。

use fs_connmgr::Db;

/// 启动恢复列表行：sessions_unclosed 经 Tauri command 直接返回 Vec<JournalRow>，须 Serialize；
/// Debug/Clone 便于前端模态状态复用与排障（P2-17）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct JournalRow {
    pub session_key: String,
    pub profile_id: Option<String>,
    pub updated_at: String,
}

pub async fn mark_open(
    db: &Db,
    session_key: &str,
    profile_id: Option<&str>,
) -> Result<(), fs_connmgr::Error> {
    sqlx::query(
        "INSERT INTO session_journal (session_key, profile_id, closed_cleanly, updated_at)
         VALUES (?1,?2,0,datetime('now'))
         ON CONFLICT(session_key) DO UPDATE SET closed_cleanly=0, updated_at=datetime('now')",
    )
    .bind(session_key)
    .bind(profile_id)
    .execute(db.pool())
    .await?;
    Ok(())
}

pub async fn mark_closed(db: &Db, session_key: &str) -> Result<(), fs_connmgr::Error> {
    sqlx::query(
        "UPDATE session_journal SET closed_cleanly=1, updated_at=datetime('now') WHERE session_key=?1",
    )
    .bind(session_key)
    .execute(db.pool())
    .await?;
    Ok(())
}

pub async fn unclosed(db: &Db) -> Result<Vec<JournalRow>, fs_connmgr::Error> {
    let rows = sqlx::query_as::<_, JournalRowInner>(
        "SELECT session_key, profile_id, updated_at FROM session_journal WHERE closed_cleanly=0",
    )
    .fetch_all(db.pool())
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| JournalRow {
            session_key: r.session_key,
            profile_id: r.profile_id,
            updated_at: r.updated_at,
        })
        .collect())
}

#[derive(sqlx::FromRow)]
struct JournalRowInner {
    session_key: String,
    profile_id: Option<String>,
    updated_at: String,
}

/// `mark_open` 的旁路封装（审计 P1-12）：写失败只记日志，**不上抛**。
/// 调用点在「会话已装配成功、注册表已登记」之后——此刻会话客观上已经存在，
/// 任何返回 Err 的做法都只会让前端与后端对「有没有这个会话」产生分歧。
pub async fn mark_open_best_effort(db: &Db, session_key: &str, profile_id: Option<&str>) {
    if let Err(e) = mark_open(db, session_key, profile_id).await {
        tracing::error!(
            session_key,
            error = %e,
            "会话 journal 记账（open）失败：会话本身正常，仅下次启动的「未正常关闭」提示可能漏报"
        );
    }
}

/// `mark_closed` 的旁路封装（审计 P1-12）：写失败只记日志，**不上抛**。
/// 漏记的后果是下次启动多提示一条已经关掉的会话——用户点「跳过」即可，
/// 远轻于让前端误以为关闭失败而留下一个点不动的死标签。
pub async fn mark_closed_best_effort(db: &Db, session_key: &str) {
    if let Err(e) = mark_closed(db, session_key).await {
        tracing::error!(
            session_key,
            error = %e,
            "会话 journal 记账（closed）失败：会话已真正关闭，仅下次启动可能误报「未正常关闭」"
        );
    }
}

/// Task 22 Step 2: 清空未关闭会话记录（用户选择"全部跳过"时调用）
pub async fn discard_all(db: &Db) -> Result<(), fs_connmgr::Error> {
    sqlx::query("DELETE FROM session_journal WHERE closed_cleanly=0")
        .execute(db.pool())
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    //! 「强杀后启动列出未关闭会话」的后半句的可执行证据（交叉审计 2026-08-25）。
    //!
    //! 此前本文件零测试：`mark_open`/`mark_closed`/`unclosed` 的行为只被
    //! `session-restore.test.ts` 在前端**假设**过。而这一层恰恰是「上次有哪些会话没
    //! 正常关闭」的唯一事实源——它若记错，恢复提示就整个错了。这里用真 SQLite
    //! 把记账语义逐条钉死。
    //!
    //! 不测 `*_best_effort` 的「吞错」分支：那需要一个**写进去会失败**的库，构造它
    //! 的手段（只读文件/坏池）本身比被测代码更脆，且吞错路径的正确性已由「失败只
    //! `tracing::error!`、不改返回值」的源码形状保证——见模块头契约。

    use super::*;

    async fn fresh_db() -> (tempfile::TempDir, Db) {
        let dir = tempfile::tempdir().expect("建临时目录");
        // Db::open 自带迁移，session_journal 表就位（见 connmgr db.rs 的
        // migrations_create_all_tables_and_audit_triggers）。
        let db = Db::open(&dir.path().join("fs.db")).await.expect("开库");
        (dir, db)
    }

    /// 开一个会话 ⇒ 它出现在未关闭列表里。这是整个功能的起点。
    #[tokio::test]
    async fn an_opened_session_shows_up_as_unclosed() {
        let (_dir, db) = fresh_db().await;
        mark_open(&db, "sess-A", Some("profile-1")).await.unwrap();

        let rows = unclosed(&db).await.unwrap();
        assert_eq!(rows.len(), 1, "开了一个就该列出一个：{rows:?}");
        assert_eq!(rows[0].session_key, "sess-A");
        assert_eq!(rows[0].profile_id.as_deref(), Some("profile-1"));
        assert!(!rows[0].updated_at.is_empty(), "时间戳不该为空");
    }

    /// 正常关闭 ⇒ 从未关闭列表里消失。**这是「正常退出」与「崩溃」的分界。**
    #[tokio::test]
    async fn a_closed_session_no_longer_counts_as_unclosed() {
        let (_dir, db) = fresh_db().await;
        mark_open(&db, "sess-A", None).await.unwrap();
        mark_closed(&db, "sess-A").await.unwrap();

        assert!(
            unclosed(&db).await.unwrap().is_empty(),
            "已正常关闭的会话不得再被列为未关闭"
        );
    }

    /// 崩溃恢复的语义核心：**关掉过又再打开**的会话，必须重新算未关闭。
    ///
    /// 这依赖 `mark_open` 的 upsert 把 `closed_cleanly` 置回 0。若哪天有人把
    /// mark_open 改成「已存在就不动」，这条会红——而那种改法恰恰会让「上次崩溃前
    /// 开着、这次重连」的会话从恢复列表里凭空消失。
    #[tokio::test]
    async fn reopening_a_closed_session_marks_it_unclosed_again() {
        let (_dir, db) = fresh_db().await;
        mark_open(&db, "sess-A", None).await.unwrap();
        mark_closed(&db, "sess-A").await.unwrap();
        mark_open(&db, "sess-A", None).await.unwrap();

        let rows = unclosed(&db).await.unwrap();
        assert_eq!(rows.len(), 1, "重开后应重新算未关闭：{rows:?}");
    }

    /// `mark_open` 是幂等 upsert：同一 session_key 开两次只有一行。
    ///
    /// 反向对照放在同一条里：两个**不同**的 key 各开一次是两行——否则把
    /// upsert 写成「无条件 INSERT」也能让「开两次是一行」碰巧成立。
    #[tokio::test]
    async fn opening_the_same_key_twice_yields_one_row_but_distinct_keys_yield_two() {
        let (_dir, db) = fresh_db().await;
        mark_open(&db, "sess-A", None).await.unwrap();
        mark_open(&db, "sess-A", None).await.unwrap();
        assert_eq!(
            unclosed(&db).await.unwrap().len(),
            1,
            "同 key 重开不得重复计数"
        );

        mark_open(&db, "sess-B", None).await.unwrap();
        assert_eq!(unclosed(&db).await.unwrap().len(), 2, "不同 key 应各占一行");
    }

    /// `discard_all`（用户选「全部跳过」）只清未关闭的，**不动已正常关闭的记录**。
    ///
    /// 已关闭的行留着无害（不会被 `unclosed` 选中），但这条断言防的是把
    /// `discard_all` 写成无条件 `DELETE FROM session_journal`——那样会把历史一并抹掉。
    #[tokio::test]
    async fn discard_all_clears_only_the_unclosed_rows() {
        let (_dir, db) = fresh_db().await;
        mark_open(&db, "open-one", None).await.unwrap();
        mark_open(&db, "closed-one", None).await.unwrap();
        mark_closed(&db, "closed-one").await.unwrap();

        discard_all(&db).await.unwrap();

        assert!(
            unclosed(&db).await.unwrap().is_empty(),
            "跳过后未关闭列表必须清空"
        );
        // 已关闭那行还在（closed_cleanly=1），只是 unclosed 选不到它。
        let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM session_journal")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(total, 1, "discard_all 不该连已关闭的记录一起删");
    }

    /// 空库 ⇒ 空列表，而不是报错。首次启动（从没开过会话）走的就是这条。
    #[tokio::test]
    async fn a_fresh_db_reports_no_unclosed_sessions() {
        let (_dir, db) = fresh_db().await;
        assert!(unclosed(&db).await.unwrap().is_empty());
        // discard_all 在空库上也得能跑（用户可能在没有任何未关闭会话时点「全部跳过」）。
        discard_all(&db).await.unwrap();
    }

    /// `profile_id` 可空：`None` 要能存能取，不得被当成空串或报错。
    #[tokio::test]
    async fn profile_id_round_trips_both_some_and_none() {
        let (_dir, db) = fresh_db().await;
        mark_open(&db, "with-profile", Some("p-9")).await.unwrap();
        mark_open(&db, "without-profile", None).await.unwrap();

        let mut rows = unclosed(&db).await.unwrap();
        rows.sort_by(|a, b| a.session_key.cmp(&b.session_key));
        assert_eq!(rows.len(), 2);
        let without = rows
            .iter()
            .find(|r| r.session_key == "without-profile")
            .unwrap();
        let with = rows
            .iter()
            .find(|r| r.session_key == "with-profile")
            .unwrap();
        assert_eq!(without.profile_id, None);
        assert_eq!(with.profile_id.as_deref(), Some("p-9"));
    }

    /// `mark_closed` 对一个**从没开过**的 key 是空操作，不得报错。
    ///
    /// 现实中这发生在「会话在 mark_open 记账前就断了」的竞态里——关闭路径不该
    /// 因为打开路径没记上而炸。
    #[tokio::test]
    async fn closing_a_never_opened_key_is_a_noop_not_an_error() {
        let (_dir, db) = fresh_db().await;
        mark_closed(&db, "never-opened").await.unwrap();
        assert!(unclosed(&db).await.unwrap().is_empty());
    }
}
