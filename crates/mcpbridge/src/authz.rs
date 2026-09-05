//! 授权层（总设计 §4.5 + M3 出口第 5 项）。
//!
//! 出口原文：「未授权工具与 Vault 类工具**不可枚举**；」——不可枚举比不可调用
//! 强一档：不可调用只是拒绝，不可枚举是**连存在性都不透露**。
//!
//! # 授权层在闸门之前
//!
//! 顺序是 授权 → 闸门 → 确认，不能换。授权层判拒返回 `FS_POLICY_FORBIDDEN`，
//! 而那个码「不可经确认放行」（规格原文）。若把授权放在确认之后，就会弹出一个
//! **批准了也照样失败**的对话框——那是在教用户对话框可以随便点。
//!
//! # `sessions.open` 的两道门：规格给的是「或」，这里取「且」
//!
//! 规格原文：「MCP 方向按 client + profile 首次打开需确认；**或**仅限
//! `ai_policy.mcp_allowed` 标记的 profile 可被打开。」两者是备选。这里两道都要，
//! 因为它们防的不是同一件事：
//!
//! - `mcp_allowed` 是**安全控制**：用户没给某个 profile 打勾，外部客户端就
//!   碰不到那台机器的凭据。默认 `false`（`fs_connmgr::AiPolicy` 手写的 Default）。
//! - (client, profile) 首次确认是**可见性机制，不是安全边界**——规格自己写了
//!   「stdio 传输无协议级调用方身份……`{client}` 展示用，非安全边界」，任何本地
//!   进程都能自称是 `claude-desktop`。它的价值在于：一个**没见过的标签**冒出来
//!   时用户会被问一次，从而注意到「我没装过这个东西」。
//!
//! 把它当安全边界用（比如「已见过的 client 就免确认」）是错的；当可见性用
//! （「没见过的 client 多问一次」）是对的。这个区别决定了 [`OpenLedger`] 只能
//! **提级**、不能降级——见那个类型的注释。

use std::collections::BTreeSet;

use crate::reject::ToolError;
use crate::tool::{ManifestEntry, MCP_MANIFEST};

/// 这个调用方被允许使用哪些工具。
///
/// 存 `&'static str`（清单里那份），**不存用户给的 String**：这样「授权了一个
/// 不存在的工具」在构造时就无处安放，而不是变成一个永远匹配不上的死条目。
#[derive(Debug, Clone, Default)]
pub struct Authorization {
    allowed: BTreeSet<&'static str>,
    /// 配置里出现过、但清单里没有的名字。**留着不是为了用，是为了报**——
    /// 用户在配置界面写了 `vault.read` 并以为自己授权成功，是最坏的一种沉默。
    unknown: Vec<String>,
}

impl Authorization {
    /// 从配置里的名字列表构造。清单外的名字进 [`Self::unknown`]，不进白名单。
    pub fn new<I, S>(names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut allowed = BTreeSet::new();
        let mut unknown = Vec::new();
        for n in names {
            match MCP_MANIFEST.iter().find(|m| m.name == n.as_ref()) {
                Some(m) => {
                    allowed.insert(m.name);
                }
                None => unknown.push(n.as_ref().to_string()),
            }
        }
        Self { allowed, unknown }
    }

    /// 什么都不授权。**这是默认**（`Default` 给的就是空集）——规格说 MCP
    /// 「默认关闭」，那么即使传输起来了，工具面也该是空的。
    pub fn nothing() -> Self {
        Self::default()
    }

    /// 配置里那些不存在的名字。配置界面应当把它们显示出来。
    pub fn unknown_names(&self) -> &[String] {
        &self.unknown
    }

    pub fn is_allowed(&self, tool: &str) -> bool {
        self.allowed.contains(tool)
    }

    /// `tools/list` 的内容：**从清单过滤**，不另写一份。
    ///
    /// 「另写一份」是「未授权工具不可枚举」最常见的失效方式——两份列表里
    /// 总有一份会被人忘记更新，而被忘记的那份通常是更宽的那份。
    pub fn enumerable(&self) -> Vec<&'static ManifestEntry> {
        MCP_MANIFEST
            .iter()
            .filter(|m| self.allowed.contains(m.name))
            .collect()
    }

    /// 调用前的授权检查。
    ///
    /// 未授权与不存在**给同一个回答**（都是 `FORBIDDEN`，且文案不区分）：
    /// 若未授权答「你无权使用 sftp.write」而不存在答「没有这个工具」，
    /// 调用方就能靠对比回答枚举出我们**支持但没授权**的工具集——那正是
    /// 「不可枚举」要挡的事。
    pub fn check(&self, tool: &str) -> Result<(), ToolError> {
        if self.is_allowed(tool) {
            return Ok(());
        }
        Err(ToolError::forbidden(
            "这个工具在当前配置下不可用。请到 FutureShell 的 MCP 设置里检查已授权的工具列表。",
        ))
    }
}

/// 哪些 (client, profile) 组合已经打开过。
///
/// **只记不忘**：没有 `forget` / `remove` / `clear`。这不是疏漏——
/// 台账的用途是「没见过的标签要多问一次」（见模块头）。给它一个遗忘接口，
/// 就等于给「让台账忘掉我，这样我下次也算首次」开了门；而由于客户端标签
/// 本来就不是安全边界，这里唯一的价值就是那一次「咦，这是什么」的提问。
///
/// 台账**不持久化到本模块之外**：谁存、存多久由 app 层决定。进程重启后清空
/// 是可以接受的（多问一次，方向是严的那边）。
#[derive(Debug, Clone, Default)]
pub struct OpenLedger {
    seen: BTreeSet<(String, String)>,
}

impl OpenLedger {
    pub fn new() -> Self {
        Self::default()
    }

    /// 这对组合是不是头一回。
    pub fn is_first_time(&self, caller: &str, profile_id: &str) -> bool {
        !self
            .seen
            .contains(&(caller.to_string(), profile_id.to_string()))
    }

    /// 记下这对组合**已经成功打开过**。
    ///
    /// 只应在会话真的开起来之后调用。在「用户批准了」那一刻就记，会让一次
    /// 因为网络失败而没开成的尝试也算数——下次就不问了。
    pub fn remember(&mut self, caller: &str, profile_id: &str) {
        self.seen
            .insert((caller.to_string(), profile_id.to_string()));
    }

    pub fn len(&self) -> usize {
        self.seen.len()
    }

    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }
}

/// `sessions.open` 的授权结论。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenAuthz {
    /// profile 不存在，或没被标记允许 MCP 打开。**确认也救不了**。
    Forbidden(ToolError),
    /// 允许，且这对 (client, profile) 是**头一回**——确认要提级到强确认。
    FirstTime,
    /// 允许，且见过。走闸门给的常规裁决（`Write` ⇒ 单次确认）。
    Familiar,
}

impl OpenAuthz {
    /// 这一步要不要把确认提级到强确认。
    ///
    /// **只提级，不降级**：`Familiar` 返回 `false` 表示「不额外提级」，
    /// 而不是「可以免确认」。闸门给的 `Confirm` 照常生效——授权层没有、
    /// 也不该有把一个需要确认的操作变成自动放行的能力。
    pub fn escalates_to_strong(&self) -> bool {
        matches!(self, Self::FirstTime)
    }
}

/// 授权一次 `sessions.open`。
///
/// `mcp_allowed`：`None` = 查无此 profile；`Some(false)` = 有但没打勾。
/// 两者**给同一个回答**，理由同 [`Authorization::check`]：区分开就等于给
/// 调用方一个枚举本机 profile 是否存在的探针，而 profile 名里常有主机名。
pub fn authorize_open(
    mcp_allowed: Option<bool>,
    caller: &str,
    profile_id: &str,
    ledger: &OpenLedger,
) -> OpenAuthz {
    if mcp_allowed != Some(true) {
        return OpenAuthz::Forbidden(ToolError::forbidden(
            "这个连接配置不允许被 MCP 客户端打开。\
             若确实需要，请在 FutureShell 里为它勾选「允许 MCP 打开」。",
        ));
    }
    if ledger.is_first_time(caller, profile_id) {
        OpenAuthz::FirstTime
    } else {
        OpenAuthz::Familiar
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 默认什么都不授权——规格说 MCP「默认关闭」。
    ///
    /// 反向对照：显式授权之后确实能枚举出来，否则把 `enumerable` 写成
    /// 恒空也能让前半条过。
    #[test]
    fn nothing_is_authorized_by_default() {
        let a = Authorization::nothing();
        assert!(a.enumerable().is_empty());
        assert!(a.check("sessions.list").is_err());
        assert_eq!(Authorization::default().enumerable().len(), 0);

        let b = Authorization::new(["sessions.list"]);
        assert_eq!(b.enumerable().len(), 1);
        assert!(b.check("sessions.list").is_ok());
    }

    /// 未授权工具**不可枚举**——不是「列出来但调不动」。
    #[test]
    fn unauthorized_tools_do_not_appear_in_the_listing_at_all() {
        let a = Authorization::new(["sessions.list", "terminal.read", "sftp.read"]);
        let names: Vec<&str> = a.enumerable().iter().map(|m| m.name).collect();
        assert_eq!(names, vec!["sessions.list", "terminal.read", "sftp.read"]);
        // 清单里有、但没授权的，一个都不许出现。
        for absent in [
            "command.run",
            "sftp.write",
            "terminal.send",
            "sessions.open",
        ] {
            assert!(!names.contains(&absent), "{absent} 不该被枚举");
            assert!(a.check(absent).is_err(), "{absent} 不该可调用");
        }
        // 枚举顺序跟随清单，不跟随配置里的书写顺序（清单是唯一事实源）。
        let reordered = Authorization::new(["sftp.read", "sessions.list", "terminal.read"]);
        let n2: Vec<&str> = reordered.enumerable().iter().map(|m| m.name).collect();
        assert_eq!(names, n2);
    }

    /// 「未授权」与「不存在」给同一个回答——否则回答本身就是一台枚举器。
    #[test]
    fn a_forbidden_tool_and_a_nonexistent_one_are_indistinguishable() {
        let a = Authorization::new(["sessions.list"]);
        let unauthorized = a.check("command.run").unwrap_err();
        let nonexistent = a.check("vault.read").unwrap_err();
        assert_eq!(unauthorized.code, nonexistent.code);
        assert_eq!(unauthorized.message, nonexistent.message);
        assert_eq!(unauthorized.tier, None, "授权层还没分过类");
        assert!(!unauthorized.confirmation_required);
    }

    /// 配置里写了不存在的工具名，**要能报出来**，而不是静默丢弃。
    #[test]
    fn names_that_are_not_real_tools_are_reported_rather_than_silently_dropped() {
        let a = Authorization::new(["sessions.list", "vault.read", "command.run", "sftp.delete"]);
        assert_eq!(a.unknown_names(), &["vault.read", "sftp.delete"]);
        // 它们没有混进白名单。
        assert!(!a.is_allowed("vault.read"));
        assert_eq!(a.enumerable().len(), 2);
        // 反向：全部合法时 unknown 是空的。
        assert!(Authorization::new(["sessions.list"])
            .unknown_names()
            .is_empty());
    }

    /// 重复授权同一个名字不会重复枚举。
    #[test]
    fn duplicate_entries_collapse() {
        let a = Authorization::new(["sftp.read", "sftp.read", "sftp.read"]);
        assert_eq!(a.enumerable().len(), 1);
    }

    /// `mcp_allowed` 没打勾就是 FORBIDDEN，**且确认救不了**。
    #[test]
    fn a_profile_without_the_flag_can_never_be_opened() {
        let l = OpenLedger::new();
        for flag in [None, Some(false)] {
            match authorize_open(flag, "claude-desktop", "p1", &l) {
                OpenAuthz::Forbidden(e) => {
                    assert_eq!(e.code, "FS_POLICY_FORBIDDEN");
                    assert!(!e.confirmation_required, "不该弹一个批准了也没用的框");
                }
                other => panic!("{flag:?} 不该通过：{other:?}"),
            }
        }
        // 反向对照：打了勾就不是 Forbidden，否则把这函数写成恒 Forbidden 也全绿。
        assert!(!matches!(
            authorize_open(Some(true), "claude-desktop", "p1", &l),
            OpenAuthz::Forbidden(_)
        ));
    }

    /// 「查无此 profile」与「有但没打勾」不可区分——profile 名里常有主机名。
    #[test]
    fn a_missing_profile_and_an_unflagged_one_are_indistinguishable() {
        let l = OpenLedger::new();
        let missing = authorize_open(None, "c", "p", &l);
        let unflagged = authorize_open(Some(false), "c", "p", &l);
        assert_eq!(missing, unflagged);
    }

    /// 首次/非首次按 **(client, profile) 组合**记，两个维度都要真的参与。
    #[test]
    fn the_ledger_keys_on_both_the_client_and_the_profile() {
        let mut l = OpenLedger::new();
        let go = |l: &OpenLedger, c, p| authorize_open(Some(true), c, p, l);

        assert_eq!(go(&l, "desktop", "prod"), OpenAuthz::FirstTime);
        l.remember("desktop", "prod");
        assert_eq!(go(&l, "desktop", "prod"), OpenAuthz::Familiar);

        // 换 profile：还是首次（否则台账只记了 client）。
        assert_eq!(go(&l, "desktop", "staging"), OpenAuthz::FirstTime);
        // 换 client：还是首次（否则台账只记了 profile）——这正是它的用途，
        // 一个没见过的标签冒出来时用户会被问一次。
        assert_eq!(go(&l, "cursor", "prod"), OpenAuthz::FirstTime);

        assert_eq!(l.len(), 1);
    }

    /// 首次**提级到强确认**；非首次不提级，但也不降级。
    #[test]
    fn a_first_time_pair_escalates_and_a_familiar_one_merely_does_not() {
        assert!(OpenAuthz::FirstTime.escalates_to_strong());
        assert!(!OpenAuthz::Familiar.escalates_to_strong());
        // Forbidden 根本走不到确认，谈不上提级。
        assert!(!OpenAuthz::Forbidden(ToolError::forbidden("x")).escalates_to_strong());
    }

    /// 台账**没有遗忘接口**——「让台账忘掉我，这样我下次也算首次」不可表达。
    ///
    /// 用源码扫描而不是靠人记得：加一个 `pub fn forget` 会当场红。
    #[test]
    fn the_ledger_has_no_way_to_forget() {
        let src = include_str!("authz.rs");
        let prod = match src.find("#[cfg(test)]") {
            Some(i) => &src[..i],
            None => src,
        };
        for banned in ["fn forget", "fn remove", "fn clear", "fn reset"] {
            assert!(!prod.contains(banned), "台账不该有 {banned}");
        }
        // 做空防护：确认扫的是真源码。
        assert!(prod.contains("fn remember"), "扫查坏了");
    }
}
