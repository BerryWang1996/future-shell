//! fs_policy — 风险分级/策略引擎（AI、MCP、传输共用的硬闸门，总设计 §4.3）。
//!
//! # 这个 crate 存在的理由
//!
//! AI 能生成命令，Agent 能自己跑命令，MCP 能让外部调用方跑命令。这三条路上的
//! 「要不要真的执行」不能由生成方自己说，也不能由提示词约束——提示词是可绕过的，
//! 而一次 `rm -rf /` 不可撤销。所以判定必须落在一段**与模型无关**的确定性代码里，
//! 这就是本 crate。
//!
//! # 三条硬纪律（总设计 §4.3）
//!
//! 1. **不得对原始字符串做正则/关键词匹配**。先经 [`shell`] 做 shell 感知分解，
//!    再对每个组分分别判定，整体取最高级。理由与具体绕过样例见 [`shell`] 模块头。
//! 2. **失败闭合**。解析失败 → `Dangerous`；混淆形态 → 至少 `Write`，永不 `ReadOnly`。
//! 3. **LLM 自评仅作辅票**。与引擎分级不一致时取高者；LLM 声称安全**不改变**分级
//!    （见 [`combine_with_llm_opinion`]）。
//!
//! # 判定顺序
//!
//! 危险谓词 → 白名单。白名单**仅当所有组分各自单独命中**时整体才为 `ReadOnly`。
//! 顺序反过来就会出现「首个组分白名单命中即放行」这类漏检。

pub mod gate;
pub mod paths;
pub mod rules;
pub mod shell;

pub use gate::{decide, AiMode, ConfirmOutcome, Decision, DenyReason, CONFIRM_TIMEOUT};
pub use paths::{classify_local_path, classify_path, PathClass};
pub use rules::{classify, classify_transfer, Reason, Verdict};
pub use shell::{ParseError, Script, SimpleCommand};

/// 三级风险分类（总设计 §4.3）。
///
/// `Ord` 的方向就是「严格程度」：`ReadOnly < Write < Dangerous`。
/// 取最高级一律用 [`Ord::max`]，不要手写比较——手写的比较是漏检的常见来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Tier {
    /// 白名单只读：自动放行。
    ReadOnly,
    /// 非只读且非危险（兜底级）：单次确认。
    Write,
    /// 语义黑名单：强确认。
    Dangerous,
}

impl Tier {
    /// 稳定串（审计与 IPC 边界用；**永不改名**——已落库的审计行按这个串比对）。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read_only",
            Self::Write => "write",
            Self::Dangerous => "dangerous",
        }
    }

    /// 从稳定串解析（读审计行 / 收 IPC 用）。
    pub fn from_str_exact(s: &str) -> Option<Self> {
        match s {
            "read_only" => Some(Self::ReadOnly),
            "write" => Some(Self::Write),
            "dangerous" => Some(Self::Dangerous),
            _ => None,
        }
    }
}

impl std::fmt::Display for Tier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 把引擎分级与 LLM 自评合并（总设计 §4.3「LLM 自评仅作辅票」）。
///
/// **取高者**，且方向是单向的：LLM 说更危险 → 采纳（模型可能看出引擎没有的语义）；
/// LLM 说更安全 → **忽略**。后者是这条规则的全部意义所在：如果 LLM 能把分级往下压，
/// 那么一次提示注入就能把 `rm -rf /` 说成只读，整个闸门等于不存在。
pub fn combine_with_llm_opinion(engine: Tier, llm: Option<Tier>) -> Tier {
    match llm {
        Some(l) => engine.max(l),
        None => engine,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier_ordering_is_severity_order() {
        assert!(Tier::ReadOnly < Tier::Write);
        assert!(Tier::Write < Tier::Dangerous);
        assert_eq!(Tier::ReadOnly.max(Tier::Dangerous), Tier::Dangerous);
    }

    #[test]
    fn stable_strings_round_trip() {
        for t in [Tier::ReadOnly, Tier::Write, Tier::Dangerous] {
            assert_eq!(Tier::from_str_exact(t.as_str()), Some(t));
        }
        assert_eq!(Tier::from_str_exact("READ_ONLY"), None, "大小写不得放宽");
        assert_eq!(Tier::from_str_exact(""), None);
    }

    #[test]
    fn llm_can_only_raise_never_lower() {
        // 采纳「更危险」
        assert_eq!(
            combine_with_llm_opinion(Tier::Write, Some(Tier::Dangerous)),
            Tier::Dangerous
        );
        // 忽略「更安全」——这条是闸门存在的前提：能压低就等于没有闸门
        assert_eq!(
            combine_with_llm_opinion(Tier::Dangerous, Some(Tier::ReadOnly)),
            Tier::Dangerous
        );
        assert_eq!(
            combine_with_llm_opinion(Tier::Write, Some(Tier::ReadOnly)),
            Tier::Write
        );
        // 没有意见就按引擎
        assert_eq!(combine_with_llm_opinion(Tier::Write, None), Tier::Write);
    }
}
