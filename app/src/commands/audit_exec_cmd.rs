//! 执行侧审计（M3 出口 7 的 ② 残留）。
//!
//! # 为什么不在 `term_input` 里记
//!
//! `term_input` 是**逐字节流**——用户每敲一个键都走它一次。在那里记审计会得到
//! 一行一个字符的垃圾，且把真正有意义的那条命令淹掉。审计要记的是「用户提交了
//! 一条命令」这个**事件**，而那个事件只在整条发送时才存在（组合命令栏、快速命令、
//! 历史重发）。
//!
//! # 它补的是哪个洞
//!
//! `ai_cmd` 给 AI 建议记 `RequestOnly`（发起了、未执行），并说「真正的执行结果由
//! 执行路径另记一条」——而在此之前，**执行路径一行审计都没写过**
//!（`session_cmd.rs` 全文零 `NewAuditEntry`）。于是一次「AI 建议 → 用户批准 →
//! 真的跑了」在链上只有前半截，事后追查「那条 rm 到底跑没跑」无从回答。
//!
//! # 记的是「发出」不是「跑完」
//!
//! Verdict 用 `Approved`：用户看过并按下了发送键，这是本层能诚实断言的全部。
//! 命令在远端的退出码要等 PTY 输出解析，那是另一回事——不为了凑一个 exit_code
//! 去猜，`exit_code: None` 就是「本层不知道」的诚实表达。

use std::sync::Arc;

use fs_connmgr::audit_repo::{Actor, AuditRepo, NewAuditEntry, Verdict};
use tauri::State;

use crate::state::AppState;

/// 记一条「用户提交了整条命令」。
///
/// 分级现算（`fs_policy::classify`）——与 AI 建议那条同一个分级器，于是两条
/// 审计行的 `risk_level` 可比：同一条命令在「建议」与「执行」两行上是同一档。
///
/// **失败只 warn 不上抛**：与 `audit_ai` 同口径。把一次成功的发送因为审计写失败
/// 报成失败，用户会重发一次——而那才是真的多跑了一条命令。
#[tauri::command]
pub async fn audit_command_sent(
    state: State<'_, Arc<AppState>>,
    session_id: Option<String>,
    command: String,
) -> Result<(), String> {
    let command = command.trim().to_string();
    if command.is_empty() {
        // 空发送（只按了回车）不是一条命令，不记。
        return Ok(());
    }
    let known = crate::commands::ai_cmd::known_secrets(&state).await;
    let tier = fs_policy::classify(&command).tier;
    let entry = NewAuditEntry {
        actor: Actor::User,
        // 脱敏走与 Agent/MCP 同一个把关类型：命令里可能带 `mysql -pXXX`。
        action: {
            let display: fs_ai::agent::record::RedactedAction =
                fs_ai::agent::record::RedactedAction::new(&command, &known);
            format!("[发送] {}", display.as_str())
        },
        target_session: session_id,
        risk_level: tier.as_str().to_string(),
        // 用户按下发送键 = 他看过并批准了这条命令。这是本层能诚实断言的全部。
        verdict: Verdict::Approved,
        // 远端退出码要等 PTY 输出解析——本层不知道，就说不知道。
        exit_code: None,
        output_digest: None,
        created_at: crate::commands::ai_cmd::now_rfc3339(),
    };
    if let Err(e) = AuditRepo::new(state.db.pool()).append(&entry).await {
        tracing::warn!(error = %e, "命令发送的审计行写入失败（发送本身已成功）");
    }
    Ok(())
}

/// 记一条「用户对危险动作作出了裁决」（路线图 M7.2「统一确认口径」，2026-09-03）。
///
/// # 出口标准④：无论是否显示确认框，审计照写
///
/// 前端 `lib/confirm-gate.ts` 在**三条**路径上都调它：弹框批准、弹框拒绝、以及因为用户此前
/// 勾过「以后不再显示」而没弹框的那一条。豁免掉的是**确认框**，不是审计——一次没人看见的
/// 重启也必须在链上留下痕迹，否则「以后不再显示」就成了一个能把审计关掉的开关。
///
/// # verdict 为什么不用 `AutoRun`
///
/// `AutoRun` 在 audit_repo 里的文档是「只读，自动放行」。把一次预授权的服务重启记成
/// 「只读自动放行」会误导事后追查的人。免确认这件事记在 action 文本的前缀上
/// （`[免确认]` vs `[确认]`），可 grep、语义不走样。
///
/// # 分级
///
/// `risk_level` 现算（`fs_policy::classify`），与 `audit_command_sent` / AI 建议同一个分级器，
/// 于是同一条命令在「建议 / 确认 / 执行」三行上是同一档，可比。
#[tauri::command]
pub async fn audit_dangerous_action(
    state: State<'_, Arc<AppState>>,
    kind: String,
    command: String,
    session_id: Option<String>,
    approved: bool,
    auto: bool,
) -> Result<(), String> {
    let command = command.trim().to_string();
    if command.is_empty() {
        // 没有命令全文的确认不构成一条可追查的审计（也违反出口标准①），不记。
        return Ok(());
    }
    let known = crate::commands::ai_cmd::known_secrets(&state).await;
    let tier = fs_policy::classify(&command).tier;
    let display: fs_ai::agent::record::RedactedAction =
        fs_ai::agent::record::RedactedAction::new(&command, &known);
    let entry = NewAuditEntry {
        actor: Actor::User,
        action: format!(
            "[{}] {}：{}",
            if auto { "免确认" } else { "确认" },
            kind,
            display.as_str()
        ),
        target_session: session_id,
        risk_level: tier.as_str().to_string(),
        verdict: if approved {
            Verdict::Approved
        } else {
            Verdict::Rejected
        },
        exit_code: None,
        output_digest: None,
        created_at: crate::commands::ai_cmd::now_rfc3339(),
    };
    if let Err(e) = AuditRepo::new(state.db.pool()).append(&entry).await {
        tracing::warn!(error = %e, "危险动作的审计行写入失败（动作本身已裁决）");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    /// 空命令不记账——只按回车不是一条命令。
    ///
    /// 用源码扫描而非行为测试：命令体要 Tauri State + 真库，而这条不变量说的是
    /// 「有没有那个早返回」。
    #[test]
    fn an_empty_command_is_not_recorded() {
        let src = include_str!("audit_exec_cmd.rs");
        let prod = match src.find("#[cfg(test)]") {
            Some(i) => &src[..i],
            None => src,
        };
        assert!(
            prod.contains("if command.is_empty()"),
            "空命令必须早返回，否则每次按回车都记一行"
        );
    }

    /// 记的是 `Approved` 而**不是** `AutoRun`——用户亲手按的发送键。
    ///
    /// 反向对照一并断言：这条路上不该出现 RequestOnly（那是「发起了、未执行」，
    /// 属于建议路径；执行路径记它就等于永远不知道命令有没有真的发出去）。
    #[test]
    fn a_sent_command_is_approved_not_request_only() {
        let src = include_str!("audit_exec_cmd.rs");
        let prod = match src.find("#[cfg(test)]") {
            Some(i) => &src[..i],
            None => src,
        };
        assert!(prod.contains("verdict: Verdict::Approved"));
        assert!(
            !prod.contains("Verdict::RequestOnly"),
            "执行路径记 RequestOnly 等于永远不知道命令发没发出去"
        );
        // 分级现算，不接受调用方传进来的档位（那等于让被审计者写审计结论）
        assert!(prod.contains("fs_policy::classify(&command)"));
        assert!(
            !prod.contains("tier: fs_policy::Tier") && !prod.contains("tier_from"),
            "档位必须现算，不能由调用方给"
        );
    }
}
