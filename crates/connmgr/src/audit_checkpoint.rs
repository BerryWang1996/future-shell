//! 审计链的周期性快照（M3 出口 7 的 ③）。
//!
//! # 快照不是信任锚
//!
//! 这一句是本模块的全部设计前提，也是最容易被自己骗过去的一句。
//!
//! 快照与 `audit` 表同库、同权限、同一把写锁。能改 `audit` 的人也能改
//! `audit_checkpoint`。所以快照**不提供任何密码学保证**，它买到的是：
//!
//! - **性能**：校验从最后一个快照往后跑，而不是每次 O(全表)。审计表是只增不减的，
//!   跑够几个月之后「点一下校验」和「等半分钟」之间的差别是真实的。
//! - **一份异地副本**：链尾 hash 多存了一处。篡改者必须把两处改得互相自洽，
//!   否则[锚点检查](ChainReport)当场发现。这抬高了成本，不构成证明。
//!
//! # 于是快照通过 ≠ 全链完整
//!
//! 增量校验只重算了尾巴那一段。前面那些行这一次**根本没读**——它们的完整性
//! 只由快照断言，而快照不是信任锚。
//!
//! 把这件事交给调用方「记得说明」是不牢靠的：调用方只会拿到一个绿勾，
//! 然后照着绿勾写一句「哈希链完整」。所以[`ChainReport`] 把**覆盖范围与结论
//! 绑在同一个值里**，且 [`ChainReport::message`] 无条件把覆盖范围说出来——
//! 「只查了尾巴」在这个 API 下无法被转述成「全链完整」。
//!
//! # 锚点对不上时退回全表，不是报错了事
//!
//! 快照记着「第 M 行的 hash 是 H」。校验时先去读第 M 行：
//!
//! - 行没了 → 有人删过；
//! - 行还在但 hash 变了 → 有人改过；
//! - 行数对不上 → 前面有整段被删/插。
//!
//! 三种情形下**这份快照都不能用**，于是自动退回全表校验，并把锚点失败的原因
//! 一并报出来（[`Coverage::FullAfterStaleCheckpoint`]）。退回而不是直接报「篡改」，
//! 是因为全表校验能给出更准的定位：断在哪一行、断因是什么。

use crate::audit_repo::{verify_rows_from, AuditEntry, AuditRepo, ChainStatus};
use crate::Error;

/// 默认落快照的间隔：每追加这么多行落一次。
///
/// 这个数不是调出来的，是权衡出来的：小了则快照表自己涨得快（且每次落快照都要
/// 跑一次全表校验，那是这条路径上最贵的一步）；大了则增量校验省不下多少。
/// 500 行意味着最坏情况多重算 500 行，而重算 500 行是毫秒级的。
pub const DEFAULT_EVERY_ROWS: usize = 500;

/// 一次审计链快照。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checkpoint {
    pub id: i64,
    /// 覆盖到 `audit` 的这一行为止（含）。
    pub row_id: i64,
    /// 该行当时的 hash——锚点。
    pub row_hash: String,
    /// 到该行为止的总行数。
    pub row_count: usize,
    pub created_at: String,
}

/// 落快照的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckpointOutcome {
    /// 校验通过并落了一份快照。
    Recorded(Checkpoint),
    /// 审计表是空的——没有可锚定的行，不落。
    ///
    /// 不是错误：新装的程序就是这个样子。
    NothingToRecord,
    /// **校验没过，因此拒绝落快照**。
    ///
    /// 落一份「到断链处为止都好」的快照等于给一条已经断了的链背书，
    /// 而且此后的增量校验会从断口之后开始，把断口本身跳过去。
    RefusedChainBroken(ChainStatus),
}

/// 增量校验覆盖了哪一段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Coverage {
    /// 从第一行查到最后一行。
    Full,
    /// 只查了 `after_id` 之后的行。前面 `skipped_rows` 条这次没读。
    SinceCheckpoint {
        after_id: i64,
        checkpoint_at: String,
        skipped_rows: usize,
    },
    /// 有快照，但锚点对不上——已自动退回全表校验。
    FullAfterStaleCheckpoint { anchor_id: i64, why: AnchorFailure },
}

/// 快照锚点为什么用不了。
///
/// 四问的顺序即诊断的优先级，见 [`anchor_failure`]。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnchorFailure {
    /// 快照锚定的那一行已经不存在了。
    RowGone,
    /// 行还在，但它**存着的** hash 与快照记的不符——整行被换过。
    HashDiffers,
    /// 行还在、存着的 hash 也与快照记的一致，但**这一行的内容算不出这个 hash**。
    ///
    /// 字段被就地改过而 hash 列没跟着改。这一问是本模块第一版漏掉的：
    /// 当时只比「存着的 hash」与「快照记的 hash」两个字符串，而 `UPDATE audit
    /// SET action='ls'` 根本不碰 hash 列——两个字符串照样相等，锚点检查全过。
    /// **只比列不重算，等于让锚点那一行成为唯一一处改了不会被发现的地方**，
    /// 而它恰恰是每次增量校验都要读、却从不重算的那一行。
    ContentAltered,
    /// 到锚点为止的行数与快照记的不符。
    CountDiffers { recorded: usize, actual: usize },
}

/// 一次链校验的完整结论：**查出了什么** + **查了哪一段**。
///
/// 两者必须一起走。单独一个 `ChainStatus::Intact` 无法回答「你查的是全部吗」，
/// 而这个问题的答案决定了这句结论值多少钱。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainReport {
    pub status: ChainStatus,
    pub coverage: Coverage,
}

impl ChainReport {
    /// 这次校验是否覆盖了整条链。
    pub fn is_full_coverage(&self) -> bool {
        !matches!(self.coverage, Coverage::SinceCheckpoint { .. })
    }

    /// 给人看的一句话。**无条件把覆盖范围说出来。**
    ///
    /// 「增量校验通过」和「哈希链完整」是两句不同的话，这个方法不允许把前者
    /// 印成后者。
    pub fn message(&self) -> String {
        let head = match &self.status {
            ChainStatus::Intact { rows } => format!("校验通过，重算了 {rows} 条记录。"),
            ChainStatus::Broken { at_id, why } => {
                format!("哈希链在第 {at_id} 条处断开（{why:?}）。")
            }
        };
        let tail = match &self.coverage {
            Coverage::Full => "覆盖范围：全表。".to_string(),
            Coverage::SinceCheckpoint {
                after_id,
                checkpoint_at,
                skipped_rows,
            } => format!(
                "覆盖范围：仅第 {after_id} 条之后（{checkpoint_at} 的快照）。\
                 **第 {after_id} 条及之前的 {skipped_rows} 条这次未重算**——\
                 快照与审计表同库同权限，它证明不了那一段。需要完整证明请跑全表校验。"
            ),
            Coverage::FullAfterStaleCheckpoint { anchor_id, why } => {
                let w = match why {
                    AnchorFailure::RowGone => format!("第 {anchor_id} 条记录已不存在"),
                    AnchorFailure::HashDiffers => {
                        format!("第 {anchor_id} 条记录的哈希与快照记的不符")
                    }
                    AnchorFailure::ContentAltered => {
                        format!("第 {anchor_id} 条记录的内容与它自己的哈希不符")
                    }
                    AnchorFailure::CountDiffers { recorded, actual } => {
                        format!("到第 {anchor_id} 条为止应有 {recorded} 条记录，实际 {actual} 条")
                    }
                };
                format!("覆盖范围：全表（快照锚点对不上——{w}，已自动退回全表校验）。")
            }
        };
        format!("{head}{tail}")
    }
}

/// 读最后一个快照。
pub async fn last(repo: &AuditRepo<'_>) -> Result<Option<Checkpoint>, Error> {
    let row: Option<(i64, i64, String, i64, String)> = sqlx::query_as(
        "SELECT id, row_id, row_hash, row_count, created_at
         FROM audit_checkpoint ORDER BY id DESC LIMIT 1",
    )
    .fetch_optional(repo.pool())
    .await?;
    Ok(
        row.map(|(id, row_id, row_hash, row_count, created_at)| Checkpoint {
            id,
            row_id,
            row_hash,
            row_count: row_count.max(0) as usize,
            created_at,
        }),
    )
}

/// 落一份快照。
///
/// **先跑一次全表校验**，链不完整就拒绝落（[`CheckpointOutcome::RefusedChainBroken`]）。
/// 这一步不是谨慎，是必需：快照的语义是「到这一行为止我查过，是好的」。给一条
/// 已经断了的链落快照，此后的增量校验会从断口之后开始跑——断口被永久跳过，
/// 而每次校验都返回绿。**篡改者最想要的就是让你落这一份快照。**
pub async fn record(repo: &AuditRepo<'_>, now: &str) -> Result<CheckpointOutcome, Error> {
    let status = repo.verify_chain().await?;
    let rows = match &status {
        ChainStatus::Intact { rows } => *rows,
        ChainStatus::Broken { .. } => return Ok(CheckpointOutcome::RefusedChainBroken(status)),
    };
    if rows == 0 {
        return Ok(CheckpointOutcome::NothingToRecord);
    }
    let tail: (i64, String) = sqlx::query_as("SELECT id, hash FROM audit ORDER BY id DESC LIMIT 1")
        .fetch_one(repo.pool())
        .await?;
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO audit_checkpoint (row_id, row_hash, row_count, created_at)
         VALUES (?,?,?,?) RETURNING id",
    )
    .bind(tail.0)
    .bind(&tail.1)
    .bind(rows as i64)
    .bind(now)
    .fetch_one(repo.pool())
    .await?;
    Ok(CheckpointOutcome::Recorded(Checkpoint {
        id,
        row_id: tail.0,
        row_hash: tail.1,
        row_count: rows,
        created_at: now.to_string(),
    }))
}

/// 攒够 `every_rows` 行才落一份，否则返回 `None`。
///
/// 「周期」按**行数**而不是按时间：审计表不写的时候没有任何东西需要快照，
/// 按时间跑就是在空转（且每次空转都要跑一遍全表校验）。
pub async fn record_if_due(
    repo: &AuditRepo<'_>,
    now: &str,
    every_rows: usize,
) -> Result<Option<CheckpointOutcome>, Error> {
    // every_rows == 0 会让每次追加都触发一次全表校验。挡在这里而不是信任调用方：
    // 这个参数将来会从设置里来，而设置里的 0 是一个人人都会不小心填出来的值。
    let every = every_rows.max(1);
    let since = rows_since(repo, last(repo).await?.as_ref()).await?;
    if since < every {
        return Ok(None);
    }
    Ok(Some(record(repo, now).await?))
}

/// 自上一个快照以来又追加了多少行。
pub async fn rows_since(repo: &AuditRepo<'_>, last: Option<&Checkpoint>) -> Result<usize, Error> {
    let after = last.map(|c| c.row_id).unwrap_or(0);
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit WHERE id > ?")
        .bind(after)
        .fetch_one(repo.pool())
        .await?;
    Ok(n.max(0) as usize)
}

/// 增量校验：从最后一个快照往后重算。
///
/// 没有快照、或快照锚点对不上时，退回全表校验。返回的 [`ChainReport`] 永远带着
/// 覆盖范围——见模块头「于是快照通过 ≠ 全链完整」。
pub async fn verify(repo: &AuditRepo<'_>) -> Result<ChainReport, Error> {
    let Some(cp) = last(repo).await? else {
        return Ok(ChainReport {
            status: repo.verify_chain().await?,
            coverage: Coverage::Full,
        });
    };

    // 锚点检查：三问，任一不过就退回全表。
    //
    // 顺序有意：先问「行还在吗」再问「hash 对吗」——行没了的时候去比 hash
    // 只会得到一个 None，而把「删了」报成「改了」会把调查引向错误的方向。
    if let Some(why) = anchor_failure(repo, &cp).await? {
        return Ok(ChainReport {
            status: repo.verify_chain().await?,
            coverage: Coverage::FullAfterStaleCheckpoint {
                anchor_id: cp.row_id,
                why,
            },
        });
    }

    let rows: Vec<AuditEntry> = sqlx::query_as(
        "SELECT id, actor, action, target_session, risk_level, verdict, exit_code,
                output_digest, created_at, prev_hash, hash
         FROM audit WHERE id > ? ORDER BY id ASC",
    )
    .bind(cp.row_id)
    .fetch_all(repo.pool())
    .await?;

    // seed = 快照锚定那一行的 hash。锚点检查刚刚确认过它就是库里第 cp.row_id 行的
    // hash，所以这个 seed 是有据的——见 `verify_rows_from` 文档里那段「不是一个
    // 可以随便传 seed 的口子」。
    let status = match verify_rows_from(Some(&cp.row_hash), &rows) {
        Ok(()) => ChainStatus::Intact { rows: rows.len() },
        Err((at_id, why)) => ChainStatus::Broken { at_id, why },
    };
    Ok(ChainReport {
        status,
        coverage: Coverage::SinceCheckpoint {
            after_id: cp.row_id,
            checkpoint_at: cp.created_at.clone(),
            skipped_rows: cp.row_count,
        },
    })
}

/// 锚点四问。`None` 表示快照可用。
///
/// 顺序即诊断优先级，不是随手排的：
///
/// 1. **行还在吗** —— 行没了的时候去比 hash 只会拿到一个 `None`，
///    把「删了」报成「改了」会把调查引向错误的方向；
/// 2. **存着的 hash 与快照记的一致吗** —— 抓「整行被换掉」（连 hash 列一起改）；
/// 3. **这一行的内容算得出这个 hash 吗** —— 抓「只改内容、不动 hash 列」。
///    第 2 问对这种改法完全无效，因为两个字符串都没被碰过；
/// 4. **到锚点为止的行数对吗** —— 抓「锚点之前有整段被删/插」。
///
/// 第 3 问在第 2 问之后：内容与 hash 列一起被改过时，第 2 问先命中，
/// 而「整行被换掉」比「字段被就地改」是更准的一句诊断。
async fn anchor_failure(
    repo: &AuditRepo<'_>,
    cp: &Checkpoint,
) -> Result<Option<AnchorFailure>, Error> {
    let row: Option<AuditEntry> = sqlx::query_as(
        "SELECT id, actor, action, target_session, risk_level, verdict, exit_code,
                output_digest, created_at, prev_hash, hash
         FROM audit WHERE id = ?",
    )
    .bind(cp.row_id)
    .fetch_optional(repo.pool())
    .await?;
    let Some(row) = row else {
        return Ok(Some(AnchorFailure::RowGone));
    };
    if row.hash != cp.row_hash {
        return Ok(Some(AnchorFailure::HashDiffers));
    }
    // 重算这一行。seed 取它自己的 prev_hash——这里要问的只是「内容 ↔ hash 相符否」，
    // 「接不接得上前一行」不是锚点的职责（那一段本来就不在这次覆盖范围内，
    // 由行数那一问兜着）。复用 verify_rows_from 而不是另写一遍重算：
    // 两处各写一份的后果是全表校验过得了而锚点过不了，且没人分得清哪边错。
    if verify_rows_from(row.prev_hash.as_deref(), std::slice::from_ref(&row)).is_err() {
        return Ok(Some(AnchorFailure::ContentAltered));
    }
    // 第三问不是多余的：前两问只看锚点那一行。把锚点之前的某一行整段删掉，
    // 锚点行本身纹丝不动（它的 prev_hash 与 hash 都没变），前两问全过——
    // 而增量校验从锚点之后开始，永远读不到那个断口。行数是唯一能在不读那一段的
    // 前提下发现「那一段少了东西」的量。
    let actual: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit WHERE id <= ?")
        .bind(cp.row_id)
        .fetch_one(repo.pool())
        .await?;
    let actual = actual.max(0) as usize;
    if actual != cp.row_count {
        return Ok(Some(AnchorFailure::CountDiffers {
            recorded: cp.row_count,
            actual,
        }));
    }
    Ok(None)
}
