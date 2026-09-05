//! 审计库对**真 SQLite** 的集成测试（总设计 §6.2 / M2 出口第 7 项）。
//!
//! 单测覆盖了哈希编码的性质；这里要验的是只有真库才能验的那些：
//! append-only 触发器真的拦得住、哈希链真的能发现篡改、并发写入不会让链分叉。
//!
//! 「哈希链能发现篡改」这条尤其只能在真库上验：它的整个价值在于
//! **绕过 SQL 层直接改数据之后仍会被发现**，而绕过 SQL 层这件事在内存里没法模拟。

mod common;
use common::tmpdir;
use fs_connmgr::audit_repo::{
    digest_output, Actor, AuditRepo, BreakReason, ChainStatus, NewAuditEntry, Verdict,
};
use fs_connmgr::Db;

fn tmp() -> std::path::PathBuf {
    tmpdir("audit")
}

fn entry(action: &str, verdict: Verdict) -> NewAuditEntry {
    NewAuditEntry {
        actor: Actor::Ai,
        action: action.into(),
        target_session: Some("s-1".into()),
        risk_level: "write".into(),
        verdict,
        exit_code: Some(0),
        output_digest: Some(digest_output(b"out")),
        created_at: "2026-08-23T10:00:00Z".into(),
    }
}

#[tokio::test]
async fn an_empty_chain_is_intact() {
    // 空库算完整。判成「断了」会让一个刚装好的程序一开机就报审计异常。
    let dir = tmp();
    let db = Db::open(&dir.join("fs.db")).await.unwrap();
    let repo = AuditRepo::new(db.pool());
    assert_eq!(
        repo.verify_chain().await.unwrap(),
        ChainStatus::Intact { rows: 0 }
    );
}

#[tokio::test]
async fn appended_rows_form_an_intact_chain() {
    let dir = tmp();
    let db = Db::open(&dir.join("fs.db")).await.unwrap();
    let repo = AuditRepo::new(db.pool());

    let a = repo.append(&entry("ls", Verdict::AutoRun)).await.unwrap();
    let b = repo
        .append(&entry("rm /tmp/x", Verdict::Approved))
        .await
        .unwrap();
    let c = repo
        .append(&entry("rm -rf /", Verdict::Rejected))
        .await
        .unwrap();

    // 第一行没有前驱；后面每行的 prev_hash 指向前一行的 hash
    assert_eq!(a.prev_hash, None);
    assert_eq!(b.prev_hash.as_deref(), Some(a.hash.as_str()));
    assert_eq!(c.prev_hash.as_deref(), Some(b.hash.as_str()));
    assert_eq!(
        repo.verify_chain().await.unwrap(),
        ChainStatus::Intact { rows: 3 }
    );
}

#[tokio::test]
async fn the_append_only_triggers_actually_fire() {
    // M1 建了触发器，但从没有测试证明它们拦得住。
    let dir = tmp();
    let db = Db::open(&dir.join("fs.db")).await.unwrap();
    let repo = AuditRepo::new(db.pool());
    let row = repo.append(&entry("ls", Verdict::AutoRun)).await.unwrap();

    let e = sqlx::query("UPDATE audit SET action = 'tampered' WHERE id = ?")
        .bind(row.id)
        .execute(db.pool())
        .await;
    assert!(e.is_err(), "UPDATE 竟然成功了——append-only 触发器没生效");

    let e = sqlx::query("DELETE FROM audit WHERE id = ?")
        .bind(row.id)
        .execute(db.pool())
        .await;
    assert!(e.is_err(), "DELETE 竟然成功了");

    // 触发器拦住之后，链仍然完整
    assert_eq!(
        repo.verify_chain().await.unwrap(),
        ChainStatus::Intact { rows: 1 }
    );
}

#[tokio::test]
async fn tampering_that_bypasses_sql_is_still_detected() {
    // 这是哈希链存在的全部理由。触发器管不着「关掉触发器再改」，
    // 而用户自己机器上的 SQLite 文件是可以这么改的。
    let dir = tmp();
    let path = dir.join("fs.db");
    let db = Db::open(&path).await.unwrap();
    let repo = AuditRepo::new(db.pool());
    repo.append(&entry("ls", Verdict::AutoRun)).await.unwrap();
    let target = repo
        .append(&entry("rm -rf /", Verdict::Rejected))
        .await
        .unwrap();
    repo.append(&entry("pwd", Verdict::AutoRun)).await.unwrap();

    // 模拟绕过 SQL 层：先删掉触发器，再改内容。
    // 「把一条被拒绝的危险命令改成一条无害的只读命令」正是最有动机的那种篡改。
    sqlx::query("DROP TRIGGER audit_no_update")
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE audit SET action = 'ls', risk_level = 'read_only', verdict = 'auto_run' WHERE id = ?")
        .bind(target.id)
        .execute(db.pool())
        .await
        .unwrap();

    match repo.verify_chain().await.unwrap() {
        ChainStatus::Broken { at_id, why } => {
            assert_eq!(at_id, target.id, "指出的行号不对");
            assert_eq!(why, BreakReason::ContentAltered);
        }
        s => panic!("篡改没被发现：{s:?}"),
    }
}

#[tokio::test]
async fn deleting_a_row_breaks_the_link_not_just_the_content() {
    // 只校验「本行内容与本行 hash 相符」挡不住整行被删——
    // 剩下的每一行自己都还自洽。必须同时校验 prev_hash 的衔接。
    let dir = tmp();
    let db = Db::open(&dir.join("fs.db")).await.unwrap();
    let repo = AuditRepo::new(db.pool());
    repo.append(&entry("a", Verdict::AutoRun)).await.unwrap();
    let middle = repo.append(&entry("b", Verdict::Approved)).await.unwrap();
    repo.append(&entry("c", Verdict::AutoRun)).await.unwrap();

    sqlx::query("DROP TRIGGER audit_no_delete")
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("DELETE FROM audit WHERE id = ?")
        .bind(middle.id)
        .execute(db.pool())
        .await
        .unwrap();

    match repo.verify_chain().await.unwrap() {
        ChainStatus::Broken { why, .. } => assert_eq!(why, BreakReason::LinkMismatch),
        s => panic!("删行没被发现：{s:?}"),
    }
}

#[tokio::test]
async fn concurrent_appends_do_not_fork_the_chain() {
    // 取上一行 hash 与插入必须原子。分开做的话两个并发写入会取到同一个 prev_hash，
    // 于是链上出现分叉——而分叉在事后看起来与「有一行被删掉」一模一样。
    let dir = tmp();
    let db = Db::open(&dir.join("fs.db")).await.unwrap();
    let pool = db.pool().clone();

    let mut handles = Vec::new();
    for i in 0..12 {
        let p = pool.clone();
        handles.push(tokio::spawn(async move {
            let repo = AuditRepo::new(&p);
            repo.append(&entry(&format!("cmd-{i}"), Verdict::AutoRun))
                .await
        }));
    }
    let mut ok = 0;
    for h in handles {
        if h.await.unwrap().is_ok() {
            ok += 1;
        }
    }
    assert_eq!(ok, 12, "有并发写入失败了");

    let repo = AuditRepo::new(&pool);
    assert_eq!(
        repo.verify_chain().await.unwrap(),
        ChainStatus::Intact { rows: 12 },
        "并发写入之后链断了"
    );
}

#[tokio::test]
async fn rejected_and_denied_actions_are_recorded_too() {
    // 只记「执行过什么」的审计回答不了「有没有什么被拦下来了」，
    // 而后者往往才是要查的：「上周 AI 有没有试图动过 /etc」。
    let dir = tmp();
    let db = Db::open(&dir.join("fs.db")).await.unwrap();
    let repo = AuditRepo::new(db.pool());
    for v in [
        Verdict::AutoRun,
        Verdict::Approved,
        Verdict::Rejected,
        Verdict::TimedOut,
        Verdict::Abandoned,
        Verdict::Denied,
        Verdict::RequestOnly,
    ] {
        repo.append(&entry("rm -rf /etc", v)).await.unwrap();
    }
    let rows = repo.recent(100).await.unwrap();
    assert_eq!(rows.len(), 7);
    let executed = rows.iter().filter(|r| {
        Verdict::parse(&r.verdict)
            .map(Verdict::executed)
            .unwrap_or(false)
    });
    assert_eq!(executed.count(), 2, "只有 auto_run 与 approved 算已执行");
    // 被拦下来的那五条也在库里
    assert_eq!(
        rows.iter().filter(|r| r.verdict == "denied").count(),
        1,
        "被策略挡住的那条没有落库"
    );
}

#[tokio::test]
async fn filtering_by_actor_answers_what_did_the_ai_do() {
    let dir = tmp();
    let db = Db::open(&dir.join("fs.db")).await.unwrap();
    let repo = AuditRepo::new(db.pool());

    let mut u = entry("ls", Verdict::AutoRun);
    u.actor = Actor::User;
    repo.append(&u).await.unwrap();
    repo.append(&entry("rm /tmp/x", Verdict::Approved))
        .await
        .unwrap();
    let mut m = entry("cat /etc/hosts", Verdict::AutoRun);
    m.actor = Actor::Mcp {
        caller: "vscode".into(),
    };
    repo.append(&m).await.unwrap();

    assert_eq!(repo.by_actor(&Actor::Ai, 100).await.unwrap().len(), 1);
    assert_eq!(repo.by_actor(&Actor::User, 100).await.unwrap().len(), 1);
    assert_eq!(
        repo.by_actor(
            &Actor::Mcp {
                caller: "vscode".into()
            },
            100
        )
        .await
        .unwrap()
        .len(),
        1
    );
    // 不同调用方要分得开——否则 MCP 的多调用方隔离在审计上就不成立
    assert_eq!(
        repo.by_actor(
            &Actor::Mcp {
                caller: "other".into()
            },
            100
        )
        .await
        .unwrap()
        .len(),
        0
    );
}

#[tokio::test]
async fn recent_returns_newest_first_and_respects_the_limit() {
    let dir = tmp();
    let db = Db::open(&dir.join("fs.db")).await.unwrap();
    let repo = AuditRepo::new(db.pool());
    for i in 0..5 {
        repo.append(&entry(&format!("cmd-{i}"), Verdict::AutoRun))
            .await
            .unwrap();
    }
    let rows = repo.recent(2).await.unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].action, "cmd-4", "最新的应该在最前");
    assert_eq!(rows[1].action, "cmd-3");
    // 负数不该 panic 也不该返回全部
    assert!(repo.recent(-1).await.unwrap().is_empty());
}

#[tokio::test]
async fn the_chain_survives_a_reopen() {
    // 哈希链的价值在于跨时间——重开库之后仍然校验得过
    let dir = tmp();
    let path = dir.join("fs.db");
    {
        let db = Db::open(&path).await.unwrap();
        let repo = AuditRepo::new(db.pool());
        repo.append(&entry("a", Verdict::AutoRun)).await.unwrap();
        repo.append(&entry("b", Verdict::Approved)).await.unwrap();
    }
    let db = Db::open(&path).await.unwrap();
    let repo = AuditRepo::new(db.pool());
    assert_eq!(
        repo.verify_chain().await.unwrap(),
        ChainStatus::Intact { rows: 2 }
    );
    // 重开后继续追加，链要接得上
    repo.append(&entry("c", Verdict::AutoRun)).await.unwrap();
    assert_eq!(
        repo.verify_chain().await.unwrap(),
        ChainStatus::Intact { rows: 3 }
    );
}
