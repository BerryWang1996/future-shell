//! AI 档位的**唯一**读取处（实现规格 §2 的 app 层第一项）。
//!
//! `ai_cmd.rs` 里原有一份 `read_mode`；Agent 也要读同一个键。两份读取器意味着
//! 两套「缺键怎么办」的答案，而这个键的回退语义**是不对称的**：
//!
//! * 键**缺失** ⇒ `WithConfirm`（默认档；新用户没设过，不能因此不可用）；
//! * 键**在但解析不了** ⇒ `Disabled`（有人写了个我们不认识的值，那是配置事故，
//!   fail closed）。
//!
//! 这个不对称只有一份实现才不会走样。

use fs_connmgr::model::AutoExec;
use fs_policy::AiMode;

// 【本批没有 GlobalMode 三态】
//
// 实现规格里设计过 `GlobalMode{Unloaded, Loaded(_)}`——「还没读到」不是「已禁用」。
// 落地时发现它在**这条路径上不可达**：档位读取走 `ai_cmd::read_mode`，它是对
// 已就绪的 DB 的一次 async 查询（命令执行时 DB 必然在），缺键回默认档、脏值回
// Disabled，没有第三种结局。
//
// 于是这里不造那个枚举：一个永远取不到 Unloaded 的三态，只会让读代码的人
// 以为「未加载」这条路被处理过了。等将来真出现「设置未就绪就能调命令」的
// 路径（比如启动画面里的预取），再连同它的产生点一起加。
/// 会话级覆盖的换算：`AutoExec` → `Option<AiMode>`。
///
/// **`Off` ⇒ `None`（未表态）而不是 `Disabled`**。理由是存储层的现实：
/// `AiPolicy.auto_execute` 是带 `#[default] Off` 的非 Option 字段，
/// 存储层**分不清**「用户选了关」与「从没设过」。若把 Off 读成 Disabled，
/// 每个新建 profile 开箱即禁用——与 roadmap「默认档位 with_confirm」相悖。
///
/// 代价是「按 profile 硬关 Agent」只能靠全局开关。真正的修法是模型改三态
/// （`Unset|Off|…`），已登记为 connmgr 的待决变更；改动落地时**只有这一个函数**
/// 和它的测试要动。
pub fn ai_mode_of(auto: AutoExec) -> Option<AiMode> {
    match auto {
        AutoExec::Off => None,
        AutoExec::ReadOnly => Some(AiMode::ReadOnlyOnly),
        AutoExec::WithConfirm => Some(AiMode::WithConfirm),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 三臂换算。**注意 `ReadOnly` → `ReadOnlyOnly`**：两个枚举的变体名不同
    /// （connmgr 的是 `ReadOnly`，policy 的是 `ReadOnlyOnly`），而稳定串又都是
    /// `read_only`——这正是手抄 match 最容易错位的形状。
    #[test]
    fn auto_exec_maps_with_off_meaning_no_opinion() {
        assert_eq!(
            ai_mode_of(AutoExec::Off),
            None,
            "Off = 未表态，不是 Disabled"
        );
        assert_eq!(ai_mode_of(AutoExec::ReadOnly), Some(AiMode::ReadOnlyOnly));
        assert_eq!(ai_mode_of(AutoExec::WithConfirm), Some(AiMode::WithConfirm));
    }

    /// 稳定串同名但变体名不同——这条钉住那个陷阱本身。
    #[test]
    fn the_two_enums_share_a_stable_string_but_not_a_variant_name() {
        assert_eq!(AiMode::ReadOnlyOnly.as_str(), "read_only");
        // connmgr 侧的 serde 串也是 read_only（snake_case 派生）
        assert_eq!(
            serde_json::to_string(&AutoExec::ReadOnly).unwrap(),
            "\"read_only\""
        );
    }
}
