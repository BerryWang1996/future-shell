//! 手动版本检查的 IPC 入口（M4b「updater」条，按 2026-08-23 裁决改为只做检查）。
//!
//! 判定逻辑全在 [`crate::update_check`]，本文件只负责取配置、给 transport、落审计。

use crate::state::AppState;
use crate::update_check::{self, UpdateStatus};
use serde::Serialize;
use std::sync::Arc;
use tauri::State;

/// settings 里存更新检查地址的键。
///
/// **没有默认值。** 键不存在 → [`UpdateStatus::NotConfigured`] → 一个包都不发。
/// 这是 README「出站面」承诺的落点：不填就永不联网。
pub const UPDATE_URL_KEY: &str = "update.manifestUrl";

/// 回给前端的结果：状态本身 + 一句给人看的话。
///
/// 把 `message()` 在**后端**算好而不是让前端拼，是因为这几句话里含承诺性表述
/// （「本程序不会主动联网」「不会自动下载或替换」）——它们必须与后端的实际行为同源，
/// 否则前端改一版文案就可能与真实行为脱节。
#[derive(Debug, Serialize)]
pub struct UpdateCheckResult {
    #[serde(flatten)]
    pub status: UpdateStatus,
    pub message: String,
}

impl From<UpdateStatus> for UpdateCheckResult {
    fn from(status: UpdateStatus) -> Self {
        let message = status.message();
        Self { status, message }
    }
}

/// 手动检查一次更新。
///
/// # 每次新建 HTTP 客户端，不常驻
///
/// 检查更新是用户手点的低频操作，新建一个客户端的开销可忽略。
/// 换来的是**进程里不存在一个常驻的 HTTP 客户端对象**——
/// 对「零遥测、不主动联网」这条承诺来说，「没有那个对象」比「有但没用它」更容易说清，
/// 也更容易在代码审查里核对。
#[tauri::command]
pub async fn update_check(state: State<'_, Arc<AppState>>) -> Result<UpdateCheckResult, String> {
    let url = fs_connmgr::SettingsRepo::new(state.db.pool())
        .get(UPDATE_URL_KEY)
        .await
        .map_err(|e| e.to_string())?;
    // 没配就在这里返回——**连 HTTP 客户端都不建**
    let Some(url) = url.as_deref().map(str::trim).filter(|u| !u.is_empty()) else {
        return Ok(UpdateStatus::NotConfigured.into());
    };
    let transport = match fs_ai::transport::ReqwestTransport::new() {
        Ok(t) => t,
        Err(e) => {
            return Ok(UpdateStatus::Failed {
                detail: e.to_string(),
            }
            .into())
        }
    };
    Ok(
        update_check::check(Some(url), env!("CARGO_PKG_VERSION"), &transport)
            .await
            .into(),
    )
}

/// 本程序自己的版本号（UI 的「关于」与「检查更新」都要显示）。
#[tauri::command]
pub fn app_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_settings_key_has_no_default_value_anywhere() {
        // 「不填就永不联网」靠的是这个键**没有**默认值。
        // 哪天有人给它加一个 `unwrap_or("https://…")`，承诺就破了。
        const SRC: &str = include_str!("update_cmd.rs");
        let prod = &SRC[..SRC.find("\n#[cfg(test)]").expect("本文件必须有测试段")];
        assert!(
            !prod.contains("unwrap_or(\"http"),
            "更新地址不得有内置默认值"
        );
        // 键名本身不含 http，防止「默认值写在常量里」这种绕法
        assert!(!UPDATE_URL_KEY.contains("http"));
    }

    #[test]
    fn the_version_reported_is_the_crate_version() {
        assert_eq!(app_version(), env!("CARGO_PKG_VERSION"));
        // 必须是合法 semver——否则 update_check 会把自己判成 Failed
        assert!(
            crate::update_check::SemVer::parse(&app_version()).is_some(),
            "本程序的版本号 {} 不是合法 semver",
            app_version()
        );
    }
}
