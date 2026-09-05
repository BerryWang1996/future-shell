//! `AuditPort` 的真实现：StepRecord → `fs_connmgr::audit_repo::AuditRepo::append`。
//!
//! **全 app 唯二**的审计写入者之一（另一个是 `ai_cmd::audit_ai`）——
//! `audit_cmd.rs` 的 `every_audit_writer_routes_through_redact` 守卫扫的就是这两处。
//!
//! # 为什么 digest 在这一层算
//!
//! `fs_ai` 不依赖 `fs_connmgr`（依赖面干净），而 `digest_output` 用的 sha2 在
//! connmgr 里。所以 `StepRecord.digest` 在纯逻辑层恒为 `None`，由这里补算——
//! 与 Actor/Verdict 用镜像枚举 + pinning 测试是同一个策略：**跨 crate 的东西
//! 要么共享类型、要么钉住字面**，不能靠「两边记得改」。

use fs_ai::agent::ports::AuditPort;
use fs_ai::agent::record::{StepRecord, StepVerdict};
use fs_connmgr::audit_repo::{Actor, AuditRepo, NewAuditEntry, Verdict};
use std::future::Future;
use std::pin::Pin;

pub struct DbAuditPort {
    pool: sqlx::SqlitePool,
    session_id: Option<String>,
    /// Vault 里的已知密钥（`RedactedAction` 已在机器侧脱过一遍；这里不再脱——
    /// 见 `append` 的注释）。
    _known: Vec<String>,
}

impl DbAuditPort {
    pub fn new(pool: sqlx::SqlitePool, session_id: Option<String>, known: Vec<String>) -> Self {
        Self {
            pool,
            session_id,
            _known: known,
        }
    }
}

/// 镜像枚举 → connmgr 的 Verdict。
///
/// 七个值一一对应。**不做 `from_str` 转换**：两边都是枚举，用字符串当中间层
/// 等于把编译期检查换成运行期字符串比较——而那正是 ai_cmd 那次手抄 match
/// 事故（连字符 vs 下划线）的形状。
fn map_verdict(v: &StepVerdict) -> Verdict {
    match v {
        StepVerdict::AutoRun => Verdict::AutoRun,
        StepVerdict::Approved => Verdict::Approved,
        StepVerdict::Rejected => Verdict::Rejected,
        StepVerdict::TimedOut => Verdict::TimedOut,
        StepVerdict::Abandoned => Verdict::Abandoned,
        StepVerdict::Denied => Verdict::Denied,
        StepVerdict::RequestOnly => Verdict::RequestOnly,
    }
}

impl AuditPort for DbAuditPort {
    fn append<'a>(
        &'a self,
        rec: StepRecord,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>> {
        Box::pin(async move {
            let (verdict, digest) = match &rec.outcome {
                Some(o) => {
                    // digest 含 truncated/timed_out 标志——同一段输出、不同标志，
                    // digest 不同（「这次是被截断的」本身是证据的一部分）。
                    let d = o.observation.as_ref().map(|obs| {
                        let bytes = serde_json::to_vec(obs).unwrap_or_default();
                        fs_connmgr::audit_repo::digest_output(&bytes)
                    });
                    (map_verdict(&o.audit_verdict), d)
                }
                // 无 outcome 的记录不该出现在写入路径（run.rs 的 RunEnd 也带
                // RequestOnly）。真出现了就按「只发起过」记，而不是丢掉这一行。
                None => (Verdict::RequestOnly, None),
            };
            let entry = NewAuditEntry {
                actor: Actor::Agent,
                // action 已是 RedactedAction（构造即脱敏）。这里**不再脱一遍**——
                // 与 ai_cmd::audit_ai 不同：那边的 action 是模型新造的命令文本
                // （可能把屏幕上的口令抄进去），而这边的文本出自 RedactedAction，
                // 脱敏已经发生在类型的构造函数里。再脱一次不会错，但会让
                // 「脱敏在哪儿发生」变成两个答案。
                action: {
                    // 显式写出类型：脱敏的保证来自 **RedactedAction 这个类型**
                    // （构造即脱敏，未脱敏的串在它之外无处安放），而不是本文件
                    // 调了什么函数。写成一行带类型的绑定有两个作用：读代码的人
                    // 一眼看见保证从哪来；audit_cmd 的脱敏绊线（扫的是代码不是
                    // 注释）也据此认得这条等价路径。
                    let display: &fs_ai::agent::record::RedactedAction = &rec.display;
                    format!("[{}] {}", rec.tool, display.as_str())
                },
                target_session: self.session_id.clone(),
                risk_level: rec.tier.as_str().to_string(),
                verdict,
                exit_code: rec
                    .outcome
                    .as_ref()
                    .and_then(|o| o.observation.as_ref())
                    .and_then(|o| o.exit_code)
                    // i32 → i64：审计表用 i64（SQLite 的整数就是 i64）
                    .map(i64::from),
                output_digest: digest,
                created_at: crate::commands::ai_cmd::format_rfc3339(now_secs()),
            };
            AuditRepo::new(&self.pool)
                .append(&entry)
                .await
                .map(|_| ())
                .map_err(|e| e.to_string())
        })
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **跨 crate 七串 pinning**：fs_ai 的镜像枚举与 connmgr 的 Verdict 逐字相等。
    ///
    /// 两边是不同 crate 的不同枚举（fs_ai 不依赖 connmgr），编译器管不到它们
    /// 的一致性。写错一个字 = 审计行落一个数据库里不存在的串，而链是按串算
    /// 哈希的——那一行会永久地对不上。
    #[test]
    fn the_seven_verdict_strings_match_across_crates() {
        let pairs = [
            (StepVerdict::AutoRun, Verdict::AutoRun),
            (StepVerdict::Approved, Verdict::Approved),
            (StepVerdict::Rejected, Verdict::Rejected),
            (StepVerdict::TimedOut, Verdict::TimedOut),
            (StepVerdict::Abandoned, Verdict::Abandoned),
            (StepVerdict::Denied, Verdict::Denied),
            (StepVerdict::RequestOnly, Verdict::RequestOnly),
        ];
        for (mirror, real) in pairs {
            assert_eq!(
                mirror.as_str(),
                real.as_str(),
                "镜像枚举与 connmgr 的稳定串分叉了"
            );
            // 映射函数也要对上（不是只有字符串对）
            assert_eq!(map_verdict(&mirror).as_str(), real.as_str());
        }
    }

    /// risk_level 用稳定串而不是 Debug——`"ReadOnly"` 会**永久写坏哈希链**
    /// （行是链的一环，落库之后改不了）。
    #[test]
    fn risk_level_uses_the_stable_string_not_debug() {
        for t in [
            fs_policy::Tier::ReadOnly,
            fs_policy::Tier::Write,
            fs_policy::Tier::Dangerous,
        ] {
            let s = t.as_str();
            assert!(
                s.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
                "稳定串该是小写下划线形式，实测 {s}"
            );
            assert_ne!(s, format!("{t:?}"), "落库的必须是稳定串不是 Debug 串");
        }
    }
}
