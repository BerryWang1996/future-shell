//! 审计链周期性快照对**真 SQLite** 的集成测试（M3 出口 7 的 ③）。
//!
//! 这一组测试的重心不在「快照能加速」——那是显然的、也是不值钱的那一半。
//! 重心在快照**引入的新失效面**：
//!
//! - 给一条已经断了的链落快照，此后每次增量校验都从断口之后开始，永远返回绿；
//! - 锚点之前的行被删/改，而增量校验根本不读那一段；
//! - 「增量通过」被当成「全链完整」转述出去。
//!
//! 每一条都各有一个测试钉着，且每一条都做过变异实证（把守卫拆掉必转红）。

mod common;
use common::tmpdir;
use fs_connmgr::audit_checkpoint as cp;
use fs_connmgr::audit_checkpoint::{AnchorFailure, ChainReport, CheckpointOutcome, Coverage};
use fs_connmgr::audit_repo::{
    digest_output, Actor, AuditRepo, BreakReason, ChainStatus, NewAuditEntry, Verdict,
};
use fs_connmgr::Db;

fn tmp() -> std::path::PathBuf {
    tmpdir("audit-cp")
}

fn entry(action: &str) -> NewAuditEntry {
    NewAuditEntry {
        actor: Actor::Ai,
        action: action.into(),
        target_session: Some("s-1".into()),
        risk_level: "write".into(),
        verdict: Verdict::Approved,
        exit_code: Some(0),
        output_digest: Some(digest_output(b"out")),
        created_at: "2026-08-26T10:00:00Z".into(),
    }
}

const NOW: &str = "2026-08-26T12:00:00Z";

#[tokio::test]
async fn a_checkpoint_lets_verification_skip_the_prefix() {
    let dir = tmp();
    let db = Db::open(&dir.join("fs.db")).await.unwrap();
    let repo = AuditRepo::new(db.pool());
    for i in 0..5 {
        repo.append(&entry(&format!("cmd-{i}"))).await.unwrap();
    }
    let CheckpointOutcome::Recorded(c) = cp::record(&repo, NOW).await.unwrap() else {
        panic!("链是好的，应当落得下快照");
    };
    assert_eq!(c.row_count, 5);
    for i in 5..8 {
        repo.append(&entry(&format!("cmd-{i}"))).await.unwrap();
    }

    let r = cp::verify(&repo).await.unwrap();
    // 只重算了后 3 条，不是 8 条——这才叫增量。
    assert_eq!(r.status, ChainStatus::Intact { rows: 3 }, "重算的条数不对");
    match r.coverage {
        Coverage::SinceCheckpoint {
            after_id,
            skipped_rows,
            ..
        } => {
            assert_eq!(after_id, c.row_id);
            assert_eq!(skipped_rows, 5);
        }
        c => panic!("应当走增量路径：{c:?}"),
    }
}

#[tokio::test]
async fn a_tampered_row_after_the_checkpoint_is_still_caught() {
    // 增量路径不能因为「跳过了前缀」就连尾巴也不查。
    let dir = tmp();
    let db = Db::open(&dir.join("fs.db")).await.unwrap();
    let repo = AuditRepo::new(db.pool());
    repo.append(&entry("a")).await.unwrap();
    cp::record(&repo, NOW).await.unwrap();
    let target = repo.append(&entry("rm -rf /")).await.unwrap();
    repo.append(&entry("c")).await.unwrap();

    sqlx::query("DROP TRIGGER audit_no_update")
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE audit SET action = 'ls' WHERE id = ?")
        .bind(target.id)
        .execute(db.pool())
        .await
        .unwrap();

    let r = cp::verify(&repo).await.unwrap();
    assert_eq!(
        r.status,
        ChainStatus::Broken {
            at_id: target.id,
            why: BreakReason::ContentAltered
        },
        "快照之后的篡改必须照样被发现"
    );
}

#[tokio::test]
async fn recording_a_checkpoint_on_a_broken_chain_is_refused() {
    // **本组最重要的一条。** 给断了的链落快照 = 把断口永久跳过，
    // 此后每次校验都返回绿。篡改者最想要的就是让程序落这一份快照。
    let dir = tmp();
    let db = Db::open(&dir.join("fs.db")).await.unwrap();
    let repo = AuditRepo::new(db.pool());
    repo.append(&entry("a")).await.unwrap();
    let target = repo.append(&entry("rm -rf /")).await.unwrap();
    repo.append(&entry("c")).await.unwrap();

    sqlx::query("DROP TRIGGER audit_no_update")
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE audit SET action = 'ls' WHERE id = ?")
        .bind(target.id)
        .execute(db.pool())
        .await
        .unwrap();

    match cp::record(&repo, NOW).await.unwrap() {
        CheckpointOutcome::RefusedChainBroken(ChainStatus::Broken { at_id, .. }) => {
            assert_eq!(at_id, target.id);
        }
        o => panic!("断链上不该落得下快照：{o:?}"),
    }
    // 且**一份都没落进库**——不是「落了但标记为可疑」。
    assert!(
        cp::last(&repo).await.unwrap().is_none(),
        "拒绝落快照就该真的一行都没写"
    );
}

#[tokio::test]
async fn deleting_the_anchor_row_falls_back_to_a_full_scan() {
    let dir = tmp();
    let db = Db::open(&dir.join("fs.db")).await.unwrap();
    let repo = AuditRepo::new(db.pool());
    repo.append(&entry("a")).await.unwrap();
    let CheckpointOutcome::Recorded(c) = cp::record(&repo, NOW).await.unwrap() else {
        panic!()
    };
    repo.append(&entry("b")).await.unwrap();

    sqlx::query("DROP TRIGGER audit_no_delete")
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("DELETE FROM audit WHERE id = ?")
        .bind(c.row_id)
        .execute(db.pool())
        .await
        .unwrap();

    let r = cp::verify(&repo).await.unwrap();
    assert!(
        matches!(
            r.coverage,
            Coverage::FullAfterStaleCheckpoint {
                why: AnchorFailure::RowGone,
                ..
            }
        ),
        "锚点行没了应当退回全表并说明原因：{:?}",
        r.coverage
    );
    // 退回全表之后必须真的发现链断了——退回本身不是结论。
    assert!(
        matches!(r.status, ChainStatus::Broken { .. }),
        "删了链头之后全表校验该报断：{:?}",
        r.status
    );
}

#[tokio::test]
async fn altering_the_anchor_rows_content_falls_back_to_a_full_scan() {
    // **这条测试的第一版是红的，逼出了锚点的第三问。**
    //
    // 原实现只比两个字符串：库里那一行存着的 hash，与快照记下的 hash。
    // 而 `UPDATE audit SET action='ls'` 根本不碰 hash 列——两个字符串纹丝不动，
    // 锚点检查全过，于是把「已被改过的那一行」当成可信锚点继续走增量路径。
    // 锚点是每次校验都要读、却从不重算的那一行；只比列不重算，
    // 等于让它成为全表唯一一处改了不会被发现的地方。
    let dir = tmp();
    let db = Db::open(&dir.join("fs.db")).await.unwrap();
    let repo = AuditRepo::new(db.pool());
    let a = repo.append(&entry("rm -rf /")).await.unwrap();
    cp::record(&repo, NOW).await.unwrap();
    repo.append(&entry("b")).await.unwrap();

    sqlx::query("DROP TRIGGER audit_no_update")
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE audit SET action = 'ls' WHERE id = ?")
        .bind(a.id)
        .execute(db.pool())
        .await
        .unwrap();

    // 前提：hash 列没被动过——第二问确实过得去，第三问才是拦住它的那一问。
    let stored: String = sqlx::query_scalar("SELECT hash FROM audit WHERE id = ?")
        .bind(a.id)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(stored, a.hash, "前提：篡改者没改 hash 列");

    let r = cp::verify(&repo).await.unwrap();
    assert!(
        matches!(
            r.coverage,
            Coverage::FullAfterStaleCheckpoint {
                why: AnchorFailure::ContentAltered,
                ..
            }
        ),
        "锚点行内容被改应当退回全表：{:?}",
        r.coverage
    );
    assert!(
        matches!(r.status, ChainStatus::Broken { .. }),
        "退回全表后要真的报断：{:?}",
        r.status
    );
}

#[tokio::test]
async fn replacing_the_anchor_rows_hash_column_falls_back_to_a_full_scan() {
    // 第二问的用武之地：连 hash 列一起改（伪造得更完整的那种篡改）。
    // 此时第三问会「过」——内容与被改后的 hash 自洽——所以第二问不是冗余。
    let dir = tmp();
    let db = Db::open(&dir.join("fs.db")).await.unwrap();
    let repo = AuditRepo::new(db.pool());
    let a = repo.append(&entry("rm -rf /")).await.unwrap();
    cp::record(&repo, NOW).await.unwrap();
    repo.append(&entry("b")).await.unwrap();

    sqlx::query("DROP TRIGGER audit_no_update")
        .execute(db.pool())
        .await
        .unwrap();
    let forged = fs_connmgr::audit_repo::row_hash_for_test(None, &entry("ls"));
    sqlx::query("UPDATE audit SET action = 'ls', hash = ? WHERE id = ?")
        .bind(&forged)
        .bind(a.id)
        .execute(db.pool())
        .await
        .unwrap();

    let r = cp::verify(&repo).await.unwrap();
    assert!(
        matches!(
            r.coverage,
            Coverage::FullAfterStaleCheckpoint {
                why: AnchorFailure::HashDiffers,
                ..
            }
        ),
        "锚点行连 hash 一起被换应当退回全表：{:?}",
        r.coverage
    );
}

#[tokio::test]
async fn deleting_a_row_before_the_anchor_is_caught_by_the_row_count() {
    // **锚点第三问存在的全部理由。**
    //
    // 删掉锚点**之前**的一行：锚点行自己纹丝不动（prev_hash 与 hash 都没变），
    // 前两问全过；而增量校验从锚点之后开始，永远读不到那个断口。
    // 行数是唯一能在不读那一段的前提下发现「那一段少了东西」的量。
    let dir = tmp();
    let db = Db::open(&dir.join("fs.db")).await.unwrap();
    let repo = AuditRepo::new(db.pool());
    repo.append(&entry("a")).await.unwrap();
    let victim = repo.append(&entry("rm -rf /")).await.unwrap();
    repo.append(&entry("c")).await.unwrap();
    let CheckpointOutcome::Recorded(c) = cp::record(&repo, NOW).await.unwrap() else {
        panic!()
    };
    repo.append(&entry("d")).await.unwrap();

    sqlx::query("DROP TRIGGER audit_no_delete")
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("DELETE FROM audit WHERE id = ?")
        .bind(victim.id)
        .execute(db.pool())
        .await
        .unwrap();

    // 锚点那一行本身完好——前两问确实过得去。
    let still_there: String = sqlx::query_scalar("SELECT hash FROM audit WHERE id = ?")
        .bind(c.row_id)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(still_there, c.row_hash, "前提：锚点行没被动过");

    let r = cp::verify(&repo).await.unwrap();
    match r.coverage {
        Coverage::FullAfterStaleCheckpoint {
            why: AnchorFailure::CountDiffers { recorded, actual },
            ..
        } => {
            assert_eq!(recorded, 3);
            assert_eq!(actual, 2);
        }
        c => panic!("锚点之前被删行必须靠行数发现：{c:?}"),
    }
    assert!(
        matches!(r.status, ChainStatus::Broken { .. }),
        "退回全表后要真的报断"
    );
}

#[tokio::test]
async fn an_incremental_pass_never_calls_itself_a_full_chain_verification() {
    // 「增量校验通过」与「哈希链完整」是两句不同的话。
    // API 必须让前者无法被印成后者——否则调用方只会看到一个绿勾。
    let dir = tmp();
    let db = Db::open(&dir.join("fs.db")).await.unwrap();
    let repo = AuditRepo::new(db.pool());
    repo.append(&entry("a")).await.unwrap();
    cp::record(&repo, NOW).await.unwrap();
    repo.append(&entry("b")).await.unwrap();

    let r = cp::verify(&repo).await.unwrap();
    assert_eq!(r.status, ChainStatus::Intact { rows: 1 });
    assert!(!r.is_full_coverage(), "增量路径不得自称全覆盖");
    let m = r.message();
    assert!(m.contains("未重算"), "结论必须点明有行没查：{m}");
    assert!(m.contains("全表校验"), "结论必须给出补救方向：{m}");

    // 反向对照：没有快照时是真全覆盖，那时不该出现「未重算」的措辞。
    let dir2 = tmp();
    let db2 = Db::open(&dir2.join("fs.db")).await.unwrap();
    let repo2 = AuditRepo::new(db2.pool());
    repo2.append(&entry("a")).await.unwrap();
    let full = cp::verify(&repo2).await.unwrap();
    assert!(full.is_full_coverage());
    assert!(
        !full.message().contains("未重算"),
        "全表校验不该说自己漏了行：{}",
        full.message()
    );
}

#[tokio::test]
async fn record_if_due_waits_for_the_row_threshold() {
    let dir = tmp();
    let db = Db::open(&dir.join("fs.db")).await.unwrap();
    let repo = AuditRepo::new(db.pool());
    for _ in 0..2 {
        repo.append(&entry("x")).await.unwrap();
    }
    assert!(
        cp::record_if_due(&repo, NOW, 3).await.unwrap().is_none(),
        "只攒了 2 行，不该落"
    );
    repo.append(&entry("x")).await.unwrap();
    assert!(
        matches!(
            cp::record_if_due(&repo, NOW, 3).await.unwrap(),
            Some(CheckpointOutcome::Recorded(_))
        ),
        "攒够 3 行该落一份"
    );
    // 落完之后计数从新快照起算，不是从头。
    assert!(
        cp::record_if_due(&repo, NOW, 3).await.unwrap().is_none(),
        "刚落完不该马上再落——否则每次追加都会触发一次全表校验"
    );
}

#[tokio::test]
async fn a_zero_threshold_does_not_degenerate_into_verifying_on_every_row() {
    // 阈值 0 会让每次追加都跑一次全表校验。这个参数将来从设置里来，
    // 而设置里的 0 是人人都会不小心填出来的值。
    let dir = tmp();
    let db = Db::open(&dir.join("fs.db")).await.unwrap();
    let repo = AuditRepo::new(db.pool());
    assert!(
        cp::record_if_due(&repo, NOW, 0).await.unwrap().is_none(),
        "空表 + 阈值 0 不该落快照"
    );
    repo.append(&entry("x")).await.unwrap();
    assert!(
        matches!(
            cp::record_if_due(&repo, NOW, 0).await.unwrap(),
            Some(CheckpointOutcome::Recorded(_))
        ),
        "阈值 0 被夹到 1，攒够 1 行才落"
    );
}

#[tokio::test]
async fn an_empty_audit_table_records_nothing() {
    let dir = tmp();
    let db = Db::open(&dir.join("fs.db")).await.unwrap();
    let repo = AuditRepo::new(db.pool());
    assert_eq!(
        cp::record(&repo, NOW).await.unwrap(),
        CheckpointOutcome::NothingToRecord,
        "空表没有可锚定的行"
    );
    // 空表也要能校验，且算全覆盖。
    let r = cp::verify(&repo).await.unwrap();
    assert_eq!(r.status, ChainStatus::Intact { rows: 0 });
    assert_eq!(r.coverage, Coverage::Full);
}

#[tokio::test]
async fn checkpoints_are_append_only_too() {
    // 快照的语义是「我在时刻 T 查过」。允许 UPDATE 就等于允许把一次失败的
    // 校验事后改成成功的——那正是审计要防的动作。
    let dir = tmp();
    let db = Db::open(&dir.join("fs.db")).await.unwrap();
    let repo = AuditRepo::new(db.pool());
    repo.append(&entry("a")).await.unwrap();
    cp::record(&repo, NOW).await.unwrap();

    let e = sqlx::query("UPDATE audit_checkpoint SET row_hash = 'forged'")
        .execute(db.pool())
        .await
        .unwrap_err();
    assert!(
        e.to_string().contains("append-only"),
        "UPDATE 该被触发器拦住：{e}"
    );
    let e = sqlx::query("DELETE FROM audit_checkpoint")
        .execute(db.pool())
        .await
        .unwrap_err();
    assert!(
        e.to_string().contains("append-only"),
        "DELETE 该被触发器拦住：{e}"
    );
}

#[tokio::test]
async fn a_forged_checkpoint_cannot_certify_a_broken_prefix() {
    // 端到端的那一问：篡改者绕过 SQL 层，既改了审计行，又自己塞了一份
    // 「到那一行为止都好」的快照。锚点检查必须在这里当场发现——
    // 否则快照就成了洗白工具。
    let dir = tmp();
    let db = Db::open(&dir.join("fs.db")).await.unwrap();
    let repo = AuditRepo::new(db.pool());
    repo.append(&entry("a")).await.unwrap();
    let target = repo.append(&entry("rm -rf /")).await.unwrap();
    let tail = repo.append(&entry("c")).await.unwrap();

    sqlx::query("DROP TRIGGER audit_no_update")
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE audit SET action = 'ls' WHERE id = ?")
        .bind(target.id)
        .execute(db.pool())
        .await
        .unwrap();
    // 手工塞一份快照，声称「到第 3 行为止查过，共 3 行」——hash 与行数都填得对，
    // 前两问、第三问都过得去。
    sqlx::query(
        "INSERT INTO audit_checkpoint (row_id, row_hash, row_count, created_at) VALUES (?,?,?,?)",
    )
    .bind(tail.id)
    .bind(&tail.hash)
    .bind(3i64)
    .bind(NOW)
    .execute(db.pool())
    .await
    .unwrap();

    let r = cp::verify(&repo).await.unwrap();
    // 锚点三问确实全过了，于是走的是增量路径、报「通过」——**而这正是快照
    // 不是信任锚的具体含义**。所以结论必须自带那句「前 3 条这次未重算」，
    // 让读的人知道这个绿勾没有覆盖被改的那一行。
    assert_eq!(r.status, ChainStatus::Intact { rows: 0 });
    assert!(!r.is_full_coverage());
    let m = r.message();
    assert!(m.contains("未重算"), "这个绿勾必须自带覆盖范围声明：{m}");
    assert!(
        m.contains("同库同权限"),
        "必须点明快照证明不了那一段的原因：{m}"
    );

    // 而全表校验一跑就现形——这是为什么「需要完整证明请跑全表校验」那句话
    // 必须真的可执行，而不是安慰。
    assert!(
        matches!(
            repo.verify_chain().await.unwrap(),
            ChainStatus::Broken { .. }
        ),
        "全表校验必须发现被伪造快照掩住的那处篡改"
    );
}

#[tokio::test]
async fn the_report_is_a_pair_not_a_bare_verdict() {
    // 结构性判据：ChainReport 必须同时携带 status 与 coverage。
    // 若将来有人为了省事把 coverage 摘掉，这里编译不过。
    let r = ChainReport {
        status: ChainStatus::Intact { rows: 1 },
        coverage: Coverage::Full,
    };
    assert!(r.is_full_coverage());
}
