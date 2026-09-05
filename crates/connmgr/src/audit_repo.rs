//! 审计库（总设计 §6.2 / M2 出口第 7 项「audit 表可查到每条 AI 请求与执行结果」）。
//!
//! M1 只建了表与 append-only 触发器；本模块补上写入、查询与**链校验**。
//!
//! # 触发器挡不住的那一半
//!
//! `0003_audit.sql` 的两个触发器禁掉了经 SQL 的 UPDATE/DELETE。但审计库是一个
//! 用户自己机器上的 SQLite 文件——拿 `sqlite3` 打开它、或者直接用十六进制编辑器改字节，
//! 触发器一概管不着。
//!
//! 所以每行带一个哈希，且把上一行的哈希算进去：改任意一行的任意字段，
//! 从那行往后的链就断了。这不能**阻止**篡改（本地文件做不到），但能让篡改**被发现**——
//! 而对审计记录来说，「能证明它没被改过」正是它的全部价值。
//!
//! # 规范化编码为什么用长度前缀
//!
//! 拼字段算哈希时，用分隔符（`a|b|c`）是不安全的：
//!
//! ```text
//! actor="x"    action="y|z"   → "x|y|z"
//! actor="x|y"  action="z"     → "x|y|z"     ← 同一个哈希
//! ```
//!
//! 于是可以把内容在字段之间挪动而哈希不变——改完之后链还是完整的。
//! 长度前缀（每段前面写它的字节数）没有这个问题：两种切分方式的编码不同。

use crate::Error;
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;

/// 发起方（总设计 §6.2「发起方」字段）。
///
/// 做成枚举而不是任意字符串：这个字段是事后追查的第一个筛选条件，
/// 拼写不一致（`ai` / `AI` / `assistant`）会让「列出所有 AI 发起的操作」这件事
/// 变成一次猜谜。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Actor {
    /// 用户自己在终端里敲的。
    User,
    /// AI 建议、用户确认后执行的。
    Ai,
    /// 自主 Agent 的一步（M3）。
    Agent,
    /// 经 MCP 的外部调用方（M3）。`caller` 是调用方标识。
    Mcp { caller: String },
}

impl Actor {
    /// 稳定串（**永不改名**——已落库的行按它比对）。
    pub fn as_str(&self) -> String {
        match self {
            Self::User => "user".into(),
            Self::Ai => "ai".into(),
            Self::Agent => "agent".into(),
            Self::Mcp { caller } => format!("mcp:{caller}"),
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "user" => Some(Self::User),
            "ai" => Some(Self::Ai),
            "agent" => Some(Self::Agent),
            _ => s.strip_prefix("mcp:").map(|c| Self::Mcp {
                caller: c.to_string(),
            }),
        }
    }
}

/// 裁决结果（总设计 §6.2「裁决」字段）。
///
/// 覆盖闸门的每一种出路。**必须包含没有执行的那些**——只记「执行过什么」的审计
/// 回答不了「有没有什么被拦下来了」，而后者往往才是要查的东西
/// （比如「上周 AI 有没有试图动过 /etc」）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// 只读，自动放行。
    AutoRun,
    /// 用户确认后执行。
    Approved,
    /// 用户明确拒绝。
    Rejected,
    /// 确认超时（60s）。
    TimedOut,
    /// 会话在等待期间关闭。
    Abandoned,
    /// 被策略挡住，连确认都没弹（总开关关闭 / 仅只读档）。
    Denied,
    /// 只是一次 AI 请求，没有要执行的东西（例如「解读输出」）。
    RequestOnly,
}

impl Verdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AutoRun => "auto_run",
            Self::Approved => "approved",
            Self::Rejected => "rejected",
            Self::TimedOut => "timed_out",
            Self::Abandoned => "abandoned",
            Self::Denied => "denied",
            Self::RequestOnly => "request_only",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "auto_run" => Self::AutoRun,
            "approved" => Self::Approved,
            "rejected" => Self::Rejected,
            "timed_out" => Self::TimedOut,
            "abandoned" => Self::Abandoned,
            "denied" => Self::Denied,
            "request_only" => Self::RequestOnly,
            _ => return None,
        })
    }

    /// 这个裁决对应的操作有没有真的执行。
    pub fn executed(self) -> bool {
        matches!(self, Self::AutoRun | Self::Approved)
    }
}

/// 一条待写入的审计行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewAuditEntry {
    pub actor: Actor,
    /// 命令文本或工具调用描述。
    ///
    /// **调用方负责先脱敏**（`fs_ai::redact`）。这里不做脱敏，因为本 crate 不知道
    /// 哪些字符串是密钥——Vault 里那几条只有 app 层拿得到。
    /// 在类型上无法强制，故写在这里并由 app 层的测试守着。
    pub action: String,
    pub target_session: Option<String>,
    /// `fs_policy::Tier` 的稳定串。
    pub risk_level: String,
    pub verdict: Verdict,
    pub exit_code: Option<i64>,
    /// 输出的**摘要**（sha256 十六进制），不是输出本身。
    ///
    /// 存摘要而不是原文：命令输出可能很大，也可能含密钥与业务数据。
    /// 摘要够用来回答「这次的输出和上次是不是同一份」，而不把内容留在库里。
    pub output_digest: Option<String>,
    /// RFC3339 时间戳。由调用方给，不在这里取当前时间——
    /// 取当前时间会让本模块不可测（同 `fs_policy::gate::is_expired` 的理由）。
    pub created_at: String,
}

/// 库里的一条审计行。
/// 取证包要能被**读回来**校验，故需要 Deserialize——见 forensics 模块。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, sqlx::FromRow)]
pub struct AuditEntry {
    pub id: i64,
    pub actor: String,
    pub action: String,
    pub target_session: Option<String>,
    pub risk_level: String,
    pub verdict: String,
    pub exit_code: Option<i64>,
    pub output_digest: Option<String>,
    pub created_at: String,
    pub prev_hash: Option<String>,
    pub hash: String,
}

/// 计算一行的哈希。
///
/// 长度前缀编码，见模块头「规范化编码为什么用长度前缀」。
fn row_hash(prev: Option<&str>, e: &NewAuditEntry) -> String {
    let mut h = Sha256::new();
    let mut feed = |s: &str| {
        // 每段先写字节长度（定长 8 字节大端），再写内容。
        // 这样任何两种不同的字段切分都得到不同的编码。
        h.update((s.len() as u64).to_be_bytes());
        h.update(s.as_bytes());
    };
    // 顺序固定，且 Option 的「无」与空串必须可区分——
    // 用一个前缀字节表示有无，否则 `None` 与 `Some("")` 会算出同一个哈希。
    feed(prev.unwrap_or(""));
    feed(if prev.is_some() { "1" } else { "0" });
    feed(&e.actor.as_str());
    feed(&e.action);
    feed(e.target_session.as_deref().unwrap_or(""));
    feed(if e.target_session.is_some() { "1" } else { "0" });
    feed(&e.risk_level);
    feed(e.verdict.as_str());
    feed(&e.exit_code.map(|c| c.to_string()).unwrap_or_default());
    feed(if e.exit_code.is_some() { "1" } else { "0" });
    feed(e.output_digest.as_deref().unwrap_or(""));
    feed(if e.output_digest.is_some() { "1" } else { "0" });
    feed(&e.created_at);
    format!("{:x}", h.finalize())
}

/// 对一段输出算摘要。
pub fn digest_output(output: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(output);
    format!("{:x}", h.finalize())
}

pub struct AuditRepo<'a> {
    pool: &'a SqlitePool,
}

/// 链校验的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChainStatus {
    /// 整条链完整（空库也算完整）。
    Intact { rows: usize },
    /// 从某一行起断了。
    Broken {
        /// 第一处不一致的行 id。
        at_id: i64,
        why: BreakReason,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BreakReason {
    /// 这一行的内容与它自己的 hash 不符——字段被改过。
    ContentAltered,
    /// 这一行记的 prev_hash 与上一行的 hash 不符——有行被删掉或插入过。
    LinkMismatch,
}

impl<'a> AuditRepo<'a> {
    pub fn new(pool: &'a SqlitePool) -> Self {
        Self { pool }
    }

    /// 给同 crate 的[快照模块](crate::audit_checkpoint)用。
    ///
    /// 不公开出 crate：审计表的写入面必须只有 [`append`](Self::append) 一个入口
    /// （它保证了 IMMEDIATE 事务下取 prev_hash 与插行的原子性）。放开一个 pool
    /// 就等于放开一条绕过它的路。
    pub(crate) fn pool(&self) -> &'a SqlitePool {
        self.pool
    }

    /// 追加一行。
    ///
    /// 全程在一个 **IMMEDIATE** 事务里：取上一行的 hash 与插入新行必须是原子的。
    /// 分开做的话，两个并发写入会取到**同一个** prev_hash，于是链上出现分叉——
    /// 而分叉在事后看起来与「有一行被删掉」一模一样。
    ///
    /// # 为什么必须显式写 IMMEDIATE
    ///
    /// `pool.begin()` 给的是 **DEFERRED** 事务：先拿共享锁读，写的时候才升级为排他锁。
    /// 两个并发事务都读完、都想升级时，双方都在等对方放手——SQLite 认出这是
    /// 「等下去也不可能成功」，于是**立刻**返回 `SQLITE_BUSY` 而不理 `busy_timeout`。
    ///
    /// 这一条是实测撞出来的：本模块第一版注释里写着「IMMEDIATE 事务」而代码用的是
    /// `pool.begin()`，12 个并发写入只成功 3 个。`busy_timeout` 已经是 15 秒也没用——
    /// 它只对「排队等锁」有效，对锁升级死锁无效。
    ///
    /// IMMEDIATE 一开始就拿写锁，于是并发者变成**排队**，`busy_timeout` 就能生效了。
    pub async fn append(&self, e: &NewAuditEntry) -> Result<AuditEntry, Error> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let prev: Option<String> =
            sqlx::query_scalar("SELECT hash FROM audit ORDER BY id DESC LIMIT 1")
                .fetch_optional(&mut *tx)
                .await?;
        let hash = row_hash(prev.as_deref(), e);
        let actor = e.actor.as_str();
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO audit
               (actor, action, target_session, risk_level, verdict, exit_code,
                output_digest, created_at, prev_hash, hash)
             VALUES (?,?,?,?,?,?,?,?,?,?)
             RETURNING id",
        )
        .bind(&actor)
        .bind(&e.action)
        .bind(&e.target_session)
        .bind(&e.risk_level)
        .bind(e.verdict.as_str())
        .bind(e.exit_code)
        .bind(&e.output_digest)
        .bind(&e.created_at)
        .bind(&prev)
        .bind(&hash)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(AuditEntry {
            id,
            actor,
            action: e.action.clone(),
            target_session: e.target_session.clone(),
            risk_level: e.risk_level.clone(),
            verdict: e.verdict.as_str().to_string(),
            exit_code: e.exit_code,
            output_digest: e.output_digest.clone(),
            created_at: e.created_at.clone(),
            prev_hash: prev,
            hash,
        })
    }

    /// 最近 `limit` 条，新的在前。
    pub async fn recent(&self, limit: i64) -> Result<Vec<AuditEntry>, Error> {
        Ok(sqlx::query_as::<_, AuditEntry>(
            "SELECT id, actor, action, target_session, risk_level, verdict, exit_code,
                    output_digest, created_at, prev_hash, hash
             FROM audit ORDER BY id DESC LIMIT ?",
        )
        .bind(limit.max(0))
        .fetch_all(self.pool)
        .await?)
    }

    /// 按发起方筛选（「上周 AI 干了什么」）。
    pub async fn by_actor(&self, actor: &Actor, limit: i64) -> Result<Vec<AuditEntry>, Error> {
        Ok(sqlx::query_as::<_, AuditEntry>(
            "SELECT id, actor, action, target_session, risk_level, verdict, exit_code,
                    output_digest, created_at, prev_hash, hash
             FROM audit WHERE actor = ? ORDER BY id DESC LIMIT ?",
        )
        .bind(actor.as_str())
        .bind(limit.max(0))
        .fetch_all(self.pool)
        .await?)
    }

    /// 全部行，**按 id 升序**。取证包导出用（顺序参与校验，不能用 `recent()` 的倒序）。
    pub async fn all_ascending(&self) -> Result<Vec<AuditEntry>, Error> {
        Ok(sqlx::query_as::<_, AuditEntry>(
            "SELECT id, actor, action, target_session, risk_level, verdict, exit_code,
                    output_digest, created_at, prev_hash, hash
             FROM audit ORDER BY id ASC",
        )
        .fetch_all(self.pool)
        .await?)
    }

    /// 校验整条哈希链。
    pub async fn verify_chain(&self) -> Result<ChainStatus, Error> {
        let rows = self.all_ascending().await?;
        Ok(match verify_rows(&rows) {
            Ok(()) => ChainStatus::Intact { rows: rows.len() },
            Err((at_id, why)) => ChainStatus::Broken { at_id, why },
        })
    }
}

/// 逐行校验一段**已按 id 升序**的审计行。
///
/// 抽成自由函数是为了让[取证包](crate::forensics)复用**同一份**实现。
/// 两处各写一份校验的后果很具体：库里校验得过而取证包校验不过（或反过来），
/// 而那时没人分得清是哪一侧写错了——而审计的价值恰恰建立在「说得清」上。
///
/// 两件事都要查：
/// - 本行内容与本行 hash 是否相符（抓改字段）；
/// - 本行 prev_hash 与上一行 hash 是否接得上（抓删行/插行）。
///
/// 只查前者挡不住整行被删——剩下的每行自己都还自洽。
pub fn verify_rows(rows: &[AuditEntry]) -> Result<(), (i64, BreakReason)> {
    verify_rows_from(None, rows)
}

/// 同 [`verify_rows`]，但从一个**已知的前一行 hash** 起算。
///
/// [周期性快照](crate::audit_checkpoint)的增量校验用它：那一段的第一行接的是快照
/// 锚定的那一行，而不是链头。参数为 `None` 时与 `verify_rows` 完全等价——
/// 两者是同一份实现，故不存在「全表校验过了增量校验不过」这种只能靠猜的分歧。
///
/// # 这不是一个可以随便传 seed 的口子
///
/// 传进来的 seed 决定了这一段的第一行接不接得上。传错 seed（比如传当前尾行的
/// hash 去校验尾行本身）会得到一个**看起来通过了**的结论。调用方必须先确认
/// seed 确实是那一段之前那一行的 hash——快照路径靠锚点检查做这件事。
pub fn verify_rows_from(seed: Option<&str>, rows: &[AuditEntry]) -> Result<(), (i64, BreakReason)> {
    let mut expected_prev: Option<String> = seed.map(|s| s.to_string());
    for r in rows {
        if r.prev_hash != expected_prev {
            return Err((r.id, BreakReason::LinkMismatch));
        }
        let Some(actor) = Actor::parse(&r.actor) else {
            return Err((r.id, BreakReason::ContentAltered));
        };
        let Some(verdict) = Verdict::parse(&r.verdict) else {
            return Err((r.id, BreakReason::ContentAltered));
        };
        let recomputed = row_hash(
            r.prev_hash.as_deref(),
            &NewAuditEntry {
                actor,
                action: r.action.clone(),
                target_session: r.target_session.clone(),
                risk_level: r.risk_level.clone(),
                verdict,
                exit_code: r.exit_code,
                output_digest: r.output_digest.clone(),
                created_at: r.created_at.clone(),
            },
        );
        if recomputed != r.hash {
            return Err((r.id, BreakReason::ContentAltered));
        }
        expected_prev = Some(r.hash.clone());
    }
    Ok(())
}

/// 供[取证包](crate::forensics)的测试构造自洽的行用。
///
/// 暴露它而不是让测试自己再实现一遍哈希：测试里另写一份就等于测试在验证自己，
/// 而不是在验证产品代码。
#[doc(hidden)]
pub fn row_hash_for_test(prev: Option<&str>, e: &NewAuditEntry) -> String {
    row_hash(prev, e)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(action: &str) -> NewAuditEntry {
        NewAuditEntry {
            actor: Actor::Ai,
            action: action.into(),
            target_session: Some("s-1".into()),
            risk_level: "write".into(),
            verdict: Verdict::Approved,
            exit_code: Some(0),
            output_digest: Some(digest_output(b"out")),
            created_at: "2026-08-23T10:00:00Z".into(),
        }
    }

    // ── 规范化编码 ──

    #[test]
    fn length_prefixing_stops_content_sliding_between_fields() {
        // 这一对是**刻意构造**的碰撞：若实现改成 `字段 + "|"` 的拼接，两行的字节序列
        // 完全相同，于是可以把内容在字段之间挪动而哈希不变、链依然完整。
        //
        //   a：action="A"    target_session="B|1"  → "A|" "B|1|" "1|"  = "A|B|1|1|"
        //   b：action="A|B"  target_session="1"    → "A|B|" "1|"  "1|" = "A|B|1|1|"
        //                                             （末尾那个 "1|" 是「有值」标记）
        //
        // 碰撞必须构造在两个**自由文本**字段之间：`actor`/`verdict` 是受限枚举，
        // 它们的取值集合本身就挡住了滑动——第一版把碰撞构造在 actor 上，
        // 于是这条测试对分隔符实现照样通过（变异验证抓出来的）。
        let mut a = entry("A");
        a.target_session = Some("B|1".into());
        let mut b = entry("A|B");
        b.target_session = Some("1".into());
        assert_ne!(
            row_hash(None, &a),
            row_hash(None, &b),
            "两行本该有不同的哈希——内容能在字段之间滑动就意味着可以改字段而不断链"
        );
    }

    #[test]
    fn none_and_empty_string_hash_differently() {
        // 不带「有无」标记的编码会让这两者相同，
        // 于是可以把一个字段从「有值（空串）」改成「无」而不被发现
        let mut a = entry("x");
        a.target_session = None;
        let mut b = entry("x");
        b.target_session = Some(String::new());
        assert_ne!(row_hash(None, &a), row_hash(None, &b));

        let mut c = entry("x");
        c.exit_code = None;
        let mut d = entry("x");
        d.exit_code = Some(0);
        assert_ne!(row_hash(None, &c), row_hash(None, &d));
    }

    #[test]
    fn changing_any_field_changes_the_hash() {
        let base = entry("ls");
        let h = row_hash(None, &base);
        let mut variants = Vec::new();
        let mut v = base.clone();
        v.actor = Actor::User;
        variants.push(v);
        let mut v = base.clone();
        v.action = "rm -rf /".into();
        variants.push(v);
        let mut v = base.clone();
        v.risk_level = "read_only".into();
        variants.push(v);
        let mut v = base.clone();
        v.verdict = Verdict::Denied;
        variants.push(v);
        let mut v = base.clone();
        v.exit_code = Some(1);
        variants.push(v);
        let mut v = base.clone();
        v.created_at = "2026-08-23T10:00:01Z".into();
        variants.push(v);
        let mut v = base.clone();
        v.target_session = Some("s-2".into());
        variants.push(v);
        let mut v = base.clone();
        v.output_digest = Some(digest_output(b"other"));
        variants.push(v);
        for (i, v) in variants.iter().enumerate() {
            assert_ne!(row_hash(None, v), h, "第 {i} 个变体的哈希没变");
        }
    }

    #[test]
    fn prev_hash_participates() {
        let e = entry("ls");
        assert_ne!(row_hash(None, &e), row_hash(Some("abc"), &e));
        assert_ne!(row_hash(Some("abc"), &e), row_hash(Some("abd"), &e));
    }

    #[test]
    fn hashing_is_deterministic() {
        let e = entry("ls");
        assert_eq!(row_hash(Some("p"), &e), row_hash(Some("p"), &e));
    }

    // ── 稳定串 ──

    #[test]
    fn actor_strings_round_trip_including_mcp_caller() {
        for a in [
            Actor::User,
            Actor::Ai,
            Actor::Agent,
            Actor::Mcp {
                caller: "vscode".into(),
            },
        ] {
            assert_eq!(Actor::parse(&a.as_str()), Some(a.clone()), "{a:?}");
        }
        assert_eq!(Actor::parse("AI"), None, "大小写不得放宽");
        assert_eq!(Actor::parse("assistant"), None);
    }

    #[test]
    fn verdict_strings_round_trip() {
        for v in [
            Verdict::AutoRun,
            Verdict::Approved,
            Verdict::Rejected,
            Verdict::TimedOut,
            Verdict::Abandoned,
            Verdict::Denied,
            Verdict::RequestOnly,
        ] {
            assert_eq!(Verdict::parse(v.as_str()), Some(v), "{v:?}");
        }
        assert_eq!(Verdict::parse("ok"), None);
    }

    #[test]
    fn only_two_verdicts_mean_it_actually_ran() {
        // 只记「执行过什么」的审计回答不了「有没有什么被拦下来了」，
        // 而后者往往才是要查的（「上周 AI 有没有试图动过 /etc」）
        assert!(Verdict::AutoRun.executed());
        assert!(Verdict::Approved.executed());
        for v in [
            Verdict::Rejected,
            Verdict::TimedOut,
            Verdict::Abandoned,
            Verdict::Denied,
            Verdict::RequestOnly,
        ] {
            assert!(!v.executed(), "{v:?} 不该算已执行");
        }
    }

    #[test]
    fn output_digest_is_a_digest_not_the_output() {
        // 存摘要而不是原文：输出可能很大，也可能含密钥与业务数据
        let d = digest_output(b"secret output here");
        assert_eq!(d.len(), 64, "sha256 十六进制应为 64 字符");
        assert!(!d.contains("secret"));
        assert_ne!(digest_output(b"a"), digest_output(b"b"));
        assert_eq!(digest_output(b"a"), digest_output(b"a"));
    }
}
