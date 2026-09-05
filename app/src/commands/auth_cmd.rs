// auth_cmd.rs
use crate::state::AppState;
use fs_sshengine::events::HostKeyChoice;
use std::sync::Arc;
use tauri::State;

/// 投递 kbd-interactive 应答（审计 P1-24）。
///
/// `prompt_id`（前端参数名 `promptId`）取自 `auth:prompt` 事件载荷，标识**这一回合**提问。
/// 现为 `Option` 只是过渡兼容尚未升级的对话框：缺省时回落到「该会话唯一待决 prompt」，
/// 会话上并存多枚时直接报错而非瞎猜（详见 `events::take_pending`）。
/// 前端接上透传后应改为必填，并删除 `events` 里的回落分支。
#[tauri::command]
pub async fn auth_respond(
    session_id: String,
    responses: Vec<String>,
    prompt_id: Option<String>,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    let tx = state.pending.take_auth(&session_id, prompt_id.as_deref())?;
    let _ = tx.send(responses);
    Ok(())
}

/// 解析前端下发的裁决取值。契约取值见计划 §Task 17：`accept_record` / `accept_once` / `refuse`。
///
/// 返回 `None` = 取值不在契约内。**绝不能**把它折叠成 `_ => Refuse` 的 catch-all——
/// 那正是这里出过的事故：前端主按钮「接受并记录」发的是 `accept_persist`，与此处认的
/// `accept_record` 差一个词，于是每一次首连 TOFU 的「接受并记录」都被静默翻译成**拒绝**。
/// 用户看到的只是「连接被拒」，前端没报错、后端没打日志、类型系统两侧各自自洽，
/// 唯一的线索是两个字符串字面量对不上，而它们分处 Rust 与 TypeScript，谁也看不见谁。
///
/// `reject` 作为 `refuse` 的别名收下：宽容只施于 **fail-closed 方向**。把「拒绝」的近义词
/// 认成拒绝，最坏结果是用户重试一次；把「接受」的近义词猜成接受，则是拿主机密钥校验做赌注。
fn parse_hostkey_choice(choice: &str) -> Option<HostKeyChoice> {
    match choice {
        "accept_record" => Some(HostKeyChoice::AcceptAndRecord),
        "accept_once" => Some(HostKeyChoice::AcceptOnce),
        "refuse" | "reject" => Some(HostKeyChoice::Refuse),
        _ => None,
    }
}

/// 投递 host key 裁决（审计 P1-24，`prompt_id` 语义同 `auth_respond`）。
#[tauri::command]
pub async fn hostkey_decide(
    session_id: String,
    choice: String,
    prompt_id: Option<String>,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    let parsed = parse_hostkey_choice(&choice);
    let tx = state
        .pending
        .take_hostkey(&session_id, prompt_id.as_deref())?;
    // 未知取值仍然照送 Refuse 而不是直接 return：待决通道已经取走，不回话的话连接会一直
    // 挂到后端超时，用户对着一个不动的界面等着。先让连接以安全方向收场，再把错误抛上去。
    let _ = tx.send(parsed.unwrap_or(HostKeyChoice::Refuse));
    if parsed.is_none() {
        tracing::error!(
            target: "future_shell_app::auth",
            choice = %choice,
            "主机密钥裁决取值不在契约内，本次按拒绝处理（前后端契约漂移）"
        );
        return Err(format!(
            "未知的主机密钥裁决取值「{choice}」，本次已按拒绝处理；\
             合法取值：accept_record / accept_once / refuse"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{parse_hostkey_choice, HostKeyChoice};

    /// 钉死三个契约取值。前端 `HostKeyDialog.svelte` 的 `decide()` 联合类型必须与此逐字一致。
    #[test]
    fn contract_choices_parse() {
        assert_eq!(
            parse_hostkey_choice("accept_record"),
            Some(HostKeyChoice::AcceptAndRecord)
        );
        assert_eq!(
            parse_hostkey_choice("accept_once"),
            Some(HostKeyChoice::AcceptOnce)
        );
        assert_eq!(parse_hostkey_choice("refuse"), Some(HostKeyChoice::Refuse));
        assert_eq!(parse_hostkey_choice("reject"), Some(HostKeyChoice::Refuse));
    }

    /// 契约外取值必须是 `None`（→ 命令上抛错误），而不是悄悄变成某个默认裁决。
    /// `accept_persist` 单独列出来：它就是前端曾经实发的那个值。
    #[test]
    fn unknown_choice_is_not_silently_defaulted() {
        for bad in ["accept_persist", "accept", "yes", "", "ACCEPT_RECORD"] {
            assert_eq!(
                parse_hostkey_choice(bad),
                None,
                "契约外取值「{bad}」被静默接受了——catch-all 又回来了"
            );
        }
    }
}
