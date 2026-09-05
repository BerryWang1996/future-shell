//! MCP 结构化拒绝码（总设计 §4.5）。
//!
//! stdio 传输下的确认契约是**唯一的人机回路**，规格原文写着「不得静默放行」。
//! 因此这个文件里没有任何一条路径能构造出「放行」——它只表达拒绝。放行由
//! [`crate::server`] 在拿到用户的肯定答复之后走另一条类型完全不同的路，
//! 两者在类型上不通婚。
//!
//! 前三个码逐字来自规格：`FS_POLICY_DENIED` / `FS_POLICY_TIMEOUT` /
//! `FS_POLICY_FORBIDDEN`，且「`FORBIDDEN` 不可经确认放行」。
//!
//! # 第四个码 `FS_POLICY_ABORTED`（本批增补）
//!
//! M3 实现规格把这个决定显式留给了 MCP 批次：「上下文消亡（会话掉线/窗口销毁）
//! **不得**映射为 `FS_POLICY_TIMEOUT`；要么推动 spec 增补第四码（建议
//! `FS_POLICY_ABORTED`），要么在传输层以断开表达，二者择一显式落文档。」
//!
//! **选增补第四码。** 走传输层断开在 stdio 下是错的：MCP 会话与 SSH 会话是两回事，
//! 一个 SSH 会话掉线就掐掉整条 stdio 传输，会连带杀掉同一客户端上**别的**、
//! 与那台主机无关的调用。
//!
//! 不能复用 `TIMEOUT` 的理由则是它会**指错下一步**：超时的正确反应是「再等等/
//! 重发一次」，而上下文消亡的正确反应是「先把会话弄回来」。调用方按码分支，
//! 给错码就是让它去做一件必然再失败一次的事。
//!
//! 总设计 §4.5 已同步增补（见该节确认契约那一段）。

use fs_policy::Tier;
use serde::Serialize;

/// 规格 §4.5 的三个码。**封闭枚举**：加第四种拒绝理由是改一处编译期事实，
/// 而不是在某个 `format!` 里多拼一个字符串。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectCode {
    /// 闸门判 `Deny`（总开关关闭 / 只读档遇到写操作），**或**用户在确认框上答了「否」。
    ///
    /// 这两件事共用一个码是规格定的（只给了三个码）；调用方要区分它们看
    /// [`ToolError::confirmation_required`]——闸门 Deny 从不询问（`false`），
    /// 用户答否则是「问过了，答案是否」（`true`）。
    Denied,
    /// 确认超时（默认 [`crate::confirm::MCP_CONFIRM_TIMEOUT_SECS`]）。
    Timeout,
    /// 授权层拒绝：工具不在白名单 / 目标 profile 未标 `mcp_allowed` / Vault 类。
    ///
    /// **不可经确认放行**——见 [`RejectCode::can_be_resolved_by_confirmation`]。
    Forbidden,
    /// 上下文在等待确认期间消亡（目标会话掉线 / 主窗口销毁）。见模块头。
    Aborted,
}

impl RejectCode {
    /// 线上字面量。第三方客户端按这三个串做分支，改动即破坏兼容。
    pub fn wire(self) -> &'static str {
        match self {
            Self::Denied => "FS_POLICY_DENIED",
            Self::Timeout => "FS_POLICY_TIMEOUT",
            Self::Forbidden => "FS_POLICY_FORBIDDEN",
            Self::Aborted => "FS_POLICY_ABORTED",
        }
    }

    /// 调用方拿到这个码之后，重发一次同样的请求有没有意义。
    ///
    /// 这是四个码存在的实用理由——不是给人看的分类学，是给调用方分支用的：
    /// - `Timeout`：**有**意义（人当时不在，现在可能在了）；
    /// - `Denied`：没有（闸门档位不允许，或用户刚刚说了不）；
    /// - `Forbidden`：没有（要先去改授权）；
    /// - `Aborted`：没有（要先把会话弄回来）。
    ///
    /// 三个 `false` 各有各的下一步，这正是它们不能合并成一个码的原因。
    pub fn retry_makes_sense(self) -> bool {
        match self {
            Self::Timeout => true,
            Self::Denied | Self::Forbidden | Self::Aborted => false,
        }
    }

    /// 这个拒绝有没有可能被一次用户确认翻转成放行。
    ///
    /// `Forbidden` 恒 `false`，这是规格里那句「FORBIDDEN 不可经确认放行」的
    /// 机器可读形式。**它同时是一条实现纪律**：授权层判定发生在闸门与确认
    /// **之前**，所以带 `Forbidden` 的调用根本不会走到确认队列——
    /// 弹一个「批准了也照样失败」的对话框，是在教用户对话框可以随便点。
    pub fn can_be_resolved_by_confirmation(self) -> bool {
        match self {
            Self::Denied | Self::Timeout => true,
            // Forbidden：授权层判定发生在闸门与确认**之前**。
            // Aborted：要确认的那个东西已经没了，批准也没有落点。
            Self::Forbidden | Self::Aborted => false,
        }
    }
}

/// 工具错误的线上形状：`{isError: true, code, tier, confirmation_required}`（§4.5）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ToolError {
    /// 恒 `true`。MCP 的 `CallToolResult` 用这个字段区分「工具报错」与「传输报错」，
    /// 前者是给模型看的、可被它读懂并改变下一步的数据。
    #[serde(rename = "isError")]
    pub is_error: bool,
    pub code: &'static str,
    /// 分级。授权层拒绝时**为 `None`**（序列化成 `null`）——那时我们还没有分类过
    /// 任何东西。给一个假的档位（哪怕是 `"dangerous"`）是在一个审计相邻的载荷里
    /// 撒谎；写 `"read_only"` 更糟，那会让调用方以为它请求的是个无害操作。
    pub tier: Option<&'static str>,
    /// 这次操作**本来**需不需要人工确认。见 [`RejectCode::Denied`] 的注释。
    pub confirmation_required: bool,
    /// 给人看的一句话。模型也会读到它，所以它得说清「下一步该去哪」而不只是「不行」。
    pub message: String,
}

impl ToolError {
    /// 授权层拒绝：没有 tier（还没分类），也不可能靠确认翻转。
    pub fn forbidden(message: impl Into<String>) -> Self {
        Self {
            is_error: true,
            code: RejectCode::Forbidden.wire(),
            tier: None,
            confirmation_required: false,
            message: message.into(),
        }
    }

    /// 闸门判拒：有 tier，但闸门从不询问，故 `confirmation_required: false`。
    pub fn denied_by_gate(tier: Tier, message: impl Into<String>) -> Self {
        Self {
            is_error: true,
            code: RejectCode::Denied.wire(),
            tier: Some(tier_wire(tier)),
            confirmation_required: false,
            message: message.into(),
        }
    }

    /// 用户答了「否」。问过了，所以 `confirmation_required: true`。
    pub fn denied_by_user(tier: Tier) -> Self {
        Self {
            is_error: true,
            code: RejectCode::Denied.wire(),
            tier: Some(tier_wire(tier)),
            confirmation_required: true,
            message: "用户在 FutureShell 的确认框上拒绝了这次调用。".into(),
        }
    }

    /// 确认超时。
    pub fn timed_out(tier: Tier, secs: u64) -> Self {
        Self {
            is_error: true,
            code: RejectCode::Timeout.wire(),
            tier: Some(tier_wire(tier)),
            confirmation_required: true,
            message: format!(
                "等待用户确认超时（{secs} 秒），本次调用未执行。\
                 若确实需要，请到 FutureShell 窗口重新发起。"
            ),
        }
    }

    /// 上下文在等待确认期间消亡。**不是超时**——见模块头第四码那一段。
    ///
    /// 文案里刻意不说「请重试」：重试会再失败一次。要说的是「先把会话弄回来」。
    pub fn aborted(tier: Tier) -> Self {
        Self {
            is_error: true,
            code: RejectCode::Aborted.wire(),
            tier: Some(tier_wire(tier)),
            confirmation_required: true,
            message: "等待确认期间目标会话已经消失（掉线或被关闭），本次调用未执行。\
                      重发同样的请求不会成功，请先在 FutureShell 里恢复这个会话。"
                .into(),
        }
    }
}

/// `Tier` 的线上字面量。
///
/// **转调 `Tier::as_str()`，不自己写 match。** 第一版这里是一份独立的三臂
/// 匹配，返回值与 `as_str()` 逐字相同——那是一份等着漂移的副本：
/// fs_policy 哪天给 `Tier` 加一档，独立的 match 编译不过（好），但如果有人
/// 图省事改掉某一档的串，两份就悄悄分叉了，而线上字面量分叉意味着第三方
/// 客户端的分支逻辑对一半错一半。
pub fn tier_wire(t: Tier) -> &'static str {
    t.as_str()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 全部四个码，一个都不许改字面量。
    #[test]
    fn the_wire_codes_are_exactly_the_spec_strings() {
        let all = [
            (RejectCode::Denied, "FS_POLICY_DENIED"),
            (RejectCode::Timeout, "FS_POLICY_TIMEOUT"),
            (RejectCode::Forbidden, "FS_POLICY_FORBIDDEN"),
            (RejectCode::Aborted, "FS_POLICY_ABORTED"),
        ];
        for (c, want) in all {
            assert_eq!(c.wire(), want);
        }
        // 互不相同，且都带 FS_POLICY_ 前缀。
        let uniq: std::collections::BTreeSet<_> = all.iter().map(|(c, _)| c.wire()).collect();
        assert_eq!(uniq.len(), 4);
        assert!(all.iter().all(|(c, _)| c.wire().starts_with("FS_POLICY_")));
    }

    /// 码表与**规格原文**对账：总设计 §4.5 里列的码，与这里的枚举必须一一对应。
    ///
    /// 线上字面量是对第三方客户端的承诺，而承诺写在规格里、兑现写在代码里。
    /// 两处任意一边单独动，这条红。没有这条的话，最可能的事故是：有人加了
    /// 第五个码并在代码里用起来，规格却还是四个——于是第三方按规格写的分支
    /// 会掉进 `default` 分支，而 `default` 分支通常是「当成一般错误重试」。
    #[test]
    fn the_code_table_agrees_with_the_design_spec() {
        let doc = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/design/architecture.md");
        let src = std::fs::read_to_string(&doc)
            .unwrap_or_else(|e| panic!("读不到总设计 {}：{e}", doc.display()));

        // 只看 §4.5 那一节，避免别处的顺带提及混进来。
        let sec = src
            .split("### 4.5")
            .nth(1)
            .expect("总设计里找不到 §4.5")
            .split("\n### ")
            .next()
            .expect("切不出 §4.5 的结尾");
        assert!(sec.contains("MCP"), "切出来的不是 §4.5：扫查坏了");

        // 规格里出现的全部 FS_POLICY_* 码位。
        let mut in_spec: std::collections::BTreeSet<String> = Default::default();
        for (i, _) in sec.match_indices("FS_POLICY_") {
            let tail = &sec[i..];
            let end = tail
                .char_indices()
                .find(|(_, c)| !c.is_ascii_uppercase() && *c != '_')
                .map(|(n, _)| n)
                .unwrap_or(tail.len());
            in_spec.insert(tail[..end].to_string());
        }

        let in_code: std::collections::BTreeSet<String> = [
            RejectCode::Denied,
            RejectCode::Timeout,
            RejectCode::Forbidden,
            RejectCode::Aborted,
        ]
        .iter()
        .map(|c| c.wire().to_string())
        .collect();

        assert!(!in_spec.is_empty(), "做空防护：§4.5 里一个码都没扫到");
        assert_eq!(
            in_spec,
            in_code,
            "规格与代码的码表不一致：只在规格里的 {:?}，只在代码里的 {:?}",
            in_spec.difference(&in_code).collect::<Vec<_>>(),
            in_code.difference(&in_spec).collect::<Vec<_>>()
        );
    }

    /// 规格那句「FORBIDDEN 不可经确认放行」，外加第四码。
    ///
    /// 反向对照一并断言：另外两个码**可以**被确认翻转，否则把这个函数写成
    /// `false` 常量也能让前半条过。
    #[test]
    fn only_denied_and_timeout_could_have_been_resolved_by_confirmation() {
        assert!(RejectCode::Denied.can_be_resolved_by_confirmation());
        assert!(RejectCode::Timeout.can_be_resolved_by_confirmation());
        assert!(!RejectCode::Forbidden.can_be_resolved_by_confirmation());
        assert!(!RejectCode::Aborted.can_be_resolved_by_confirmation());
    }

    /// 四个码的**下一步各不相同**——这是它们不能合并的全部理由。
    ///
    /// 「重发有没有意义」上只有 Timeout 是 true。若把上下文消亡映射成
    /// TIMEOUT（规格明令禁止的那件事），调用方会去重发一个必然再失败的请求。
    #[test]
    fn retry_only_makes_sense_after_a_timeout() {
        assert!(RejectCode::Timeout.retry_makes_sense());
        for c in [
            RejectCode::Denied,
            RejectCode::Forbidden,
            RejectCode::Aborted,
        ] {
            assert!(!c.retry_makes_sense(), "{:?} 不该建议重发", c.wire());
        }
    }

    /// 上下文消亡的文案**不许**说重试，且必须是 ABORTED 而不是 TIMEOUT。
    #[test]
    fn an_aborted_call_never_tells_the_caller_to_retry() {
        let e = ToolError::aborted(Tier::Write);
        assert_eq!(e.code, "FS_POLICY_ABORTED");
        assert_ne!(e.code, RejectCode::Timeout.wire(), "规格明令不得映射为超时");
        assert!(
            e.message.contains("不会成功"),
            "文案要说清重发没用：{}",
            e.message
        );
        assert!(e.tier.is_some(), "到这一步已经分过类了，tier 不该是 null");
        // 对照：超时的文案**要**引导重新发起。
        let t = ToolError::timed_out(Tier::Write, 120);
        assert!(t.message.contains("重新发起"));
    }

    /// 授权层拒绝不带 tier——不编一个出来。
    #[test]
    fn an_authorization_rejection_reports_no_tier_at_all() {
        let e = ToolError::forbidden("工具未授权");
        assert_eq!(e.tier, None);
        assert!(!e.confirmation_required);
        let j: serde_json::Value = serde_json::to_value(&e).unwrap();
        assert_eq!(j["isError"], serde_json::json!(true));
        assert_eq!(j["code"], "FS_POLICY_FORBIDDEN");
        // 显式断言是 null 而不是缺字段：调用方按 `"tier" in payload` 分支时，
        // 缺字段与 null 是两种不同的读数。
        assert!(j.get("tier").is_some(), "tier 键必须在");
        assert!(j["tier"].is_null(), "tier 值必须是 null");
    }

    /// 闸门拒绝与用户拒绝共用 `DENIED` 码，靠 `confirmation_required` 区分。
    ///
    /// 这条是三个码够用的**全部理由**：少了这个区分，调用方分不清
    /// 「这台机器的档位不允许」（换台机器/改设置才有用）与「用户刚刚拒绝了你」
    /// （换个说法再问一次可能有用）——而这两者的下一步完全相反。
    #[test]
    fn gate_denial_and_user_denial_are_distinguishable_within_the_same_code() {
        let g = ToolError::denied_by_gate(Tier::Write, "只读档");
        let u = ToolError::denied_by_user(Tier::Write);
        assert_eq!(g.code, u.code);
        assert!(!g.confirmation_required);
        assert!(u.confirmation_required);
    }

    #[test]
    fn a_timeout_message_names_the_budget_it_blew() {
        let e = ToolError::timed_out(Tier::Dangerous, 120);
        assert!(
            e.message.contains("120"),
            "超时数值要出现在文案里：{}",
            e.message
        );
        assert_eq!(e.tier, Some("dangerous"));
        assert!(e.confirmation_required);
    }

    #[test]
    fn tier_wire_strings_are_snake_case_and_distinct() {
        let all = [Tier::ReadOnly, Tier::Write, Tier::Dangerous].map(tier_wire);
        assert_eq!(all, ["read_only", "write", "dangerous"]);
        let uniq: std::collections::BTreeSet<_> = all.iter().collect();
        assert_eq!(uniq.len(), 3);
    }

    /// `is_error` 在**每一个**构造函数里都是 true。
    ///
    /// 少写一个就是一次静默放行：MCP 客户端看到 `isError: false` 会把
    /// `message` 当成工具的正常返回值交给模型，而那句话是「用户拒绝了」。
    #[test]
    fn every_constructor_sets_is_error() {
        let all = [
            ToolError::forbidden("x"),
            ToolError::denied_by_gate(Tier::Write, "x"),
            ToolError::denied_by_user(Tier::Write),
            ToolError::timed_out(Tier::Write, 1),
            ToolError::aborted(Tier::Write),
        ];
        for e in &all {
            assert!(e.is_error, "{e:?} 的 is_error 必须为真");
            assert!(e.code.starts_with("FS_POLICY_"), "{e:?} 的码不合规");
            assert!(!e.message.is_empty(), "{e:?} 没有给人看的文案");
        }
        assert_eq!(all.len(), 5, "新增构造函数时把它加进这条断言");
        // 每个构造函数产出的码都能反解回一个枚举变体——没有手拼的野码。
        let codes: std::collections::BTreeSet<&str> = all.iter().map(|e| e.code).collect();
        let known: std::collections::BTreeSet<&str> = [
            RejectCode::Denied,
            RejectCode::Timeout,
            RejectCode::Forbidden,
            RejectCode::Aborted,
        ]
        .iter()
        .map(|c| c.wire())
        .collect();
        assert!(codes.is_subset(&known), "出现了枚举外的码：{codes:?}");
    }
}
