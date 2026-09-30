//! 时间线与审计的同一事实源。
//!
//! [`StepRecord`] 同时驱动 UI 时间线与审计行——两处显示的是同一个对象，
//! 不可能「面板说 approved 而审计行说 request_only」。

use crate::exec::Observation;
use fs_policy::{Decision, Tier};

/// 脱敏把关 newtype（照搬 `ScreenExcerpt` 禁 ring-buffer 的先例）：
/// 审计行、确认对话框、UI 时间线拿到的 action 只可能是脱敏产物——
/// 不是「写之前记得脱敏」，而是**未脱敏的串在这个类型之外无处安放**。
///
/// `Clone` 是安全的：复制一个已脱敏的值仍然是脱敏的，构造仍然只有 [`Self::new`]
/// 这一条路。**不 derive 的是 `Default`**——那会凭空造出一个「已脱敏」的空串，
/// 于是「忘了填 action」与「这次确实没什么好说的」在类型上不可区分。
#[derive(Clone, PartialEq, Eq)]
pub struct RedactedAction(String);

impl RedactedAction {
    /// 唯一构造函数：内部必经 `fs_ai::redact::redact`。
    pub fn new(action: &str, known_secrets: &[String]) -> Self {
        let refs: Vec<&str> = known_secrets.iter().map(|s| s.as_str()).collect();
        let r = crate::redact::redact(action, &refs);
        Self(r.text)
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for RedactedAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Debug 也只给脱敏后的内容——一个 {:?} 不该成为泄漏口。
        write!(f, "RedactedAction({:?})", self.0)
    }
}

/// audit 七值的镜像枚举（fs_ai 不依赖 fs_connmgr，保持依赖面干净）。
///
/// app 层一个 pinning 测试钉死与 `fs_connmgr::audit_repo::Verdict` 七串一一相等。
pub enum StepVerdict {
    AutoRun,
    Approved,
    Rejected,
    TimedOut,
    Abandoned,
    Denied,
    RequestOnly,
}

impl StepVerdict {
    /// 稳定串（与 `fs_connmgr::audit_repo::Verdict::as_str` 逐字相同——
    /// app 层 pinning 测试钉死；写错一个字 = 审计行落一个不存在的串）。
    pub fn as_str(&self) -> &'static str {
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
}

/// 一步的完整记录：UI 时间线、RunReport、审计行三方共用的那一份事实。
pub struct StepRecord {
    /// 本 run 内步序（时间线/报告用；审计行只有 id 序——秒级 created_at
    /// 分不清同秒步序，这是已知缺口，见实现规格 §8）。
    pub index: u32,
    /// 所属模型回合。
    pub turn: u32,
    /// `"run_command"` / `"sftp_get"` / …（manifest 同源名）。
    /// 工具的线上名。
    ///
    /// `String` 而非 `&'static str`：外部 MCP 工具（M3 出口 6）的名字是运行期
    /// 拼的 `mcp__<server>__<tool>`，静态生命周期装不下它。内置工具仍然来自
    /// manifest 常量，只是多一次 `to_string`——那点开销换掉的是「外部工具进不了
    /// 审计行」这个洞。
    pub tool: String,
    /// 参数摘要——构造即脱敏（RedactedAction）。
    pub display: RedactedAction,
    /// 真实分类（永远出自 `classify_for_gate`）。
    pub tier: Tier,
    /// `verdict.primary().map(|r| r.rule)`；reasons 可空（`ls` 的 primary() 是
    /// None），绝不 unwrap。
    pub rule: Option<&'static str>,
    pub decision: Decision,
    /// `None` = 未执行（等确认/被拒/被预算拦）——「没执行」与「执行了」的类型级区分。
    pub outcome: Option<StepOutcome>,
}

pub struct StepOutcome {
    /// 只在真实结局后写：`Approved` 只可能出现在 `ConfirmOutcome::Approved` 之后
    /// ——ai_cmd 那条「建议即 Approved」的不实映射在 Agent 侧结构性消失。
    pub audit_verdict: StepVerdict,
    /// 原样保留（truncated/timed_out/stderr 通道错误行都在）——不猜值、不修值。
    pub observation: Option<Observation>,
    /// `digest_output(serde_json::to_vec(&observation))`；无输出步为 None。
    pub digest: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 脱敏 newtype：已知密钥被替换。
    #[test]
    fn the_newtype_redacts_on_construction() {
        let known = vec!["sk-must-not-leak".to_string()];
        let a = RedactedAction::new("curl -H 'Authorization: Bearer sk-must-not-leak' x", &known);
        assert!(
            !a.as_str().contains("sk-must-not-leak"),
            "构造后仍含明文：{}",
            a.as_str()
        );
        assert!(a.as_str().contains("redacted"));
    }

    /// 七串 pinning：与 fs_connmgr 的 Verdict 稳定串逐字相等（app 层还有一份
    /// 跨 crate 的 pinning；这里先钉 fs_ai 侧的字面，防止本枚举自己漂）。
    #[test]
    fn step_verdict_strings_are_the_audit_seven() {
        assert_eq!(
            [
                StepVerdict::AutoRun.as_str(),
                StepVerdict::Approved.as_str(),
                StepVerdict::Rejected.as_str(),
                StepVerdict::TimedOut.as_str(),
                StepVerdict::Abandoned.as_str(),
                StepVerdict::Denied.as_str(),
                StepVerdict::RequestOnly.as_str(),
            ],
            [
                "auto_run",
                "approved",
                "rejected",
                "timed_out",
                "abandoned",
                "denied",
                "request_only"
            ],
        );
    }
}
