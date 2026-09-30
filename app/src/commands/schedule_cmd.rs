//! 计划任务命令（M4a）。调度循环在 [`crate::scheduler`]，判定在 `fs_connmgr::schedule`。

use crate::state::AppState;
use fs_connmgr::schedule::CatchUp;
use fs_connmgr::schedule_repo::{ScheduleRepo, ScheduledRun, ScheduledTask, TaskSpec};
use std::sync::Arc;
use tauri::State;

/// 执行记录一次最多回多少条（面板只看「最近怎么样」）。
const RUNS_LIMIT: i64 = 50;

/// 预览时给出几个触发时刻。
///
/// 3 个足够让人判断「我写的 cron 是不是我想的那个」：一个只能看出起点，两个能看出
/// 间隔，三个能确认间隔是稳定的（`*/15` 与 `0,15` 的区别在第三个上才显出来）。
const PREVIEW_COUNT: usize = 3;

/// 前端提交的任务表单。
///
/// `tz_offset_minutes` 由前端给（webview 天然知道本地时区，而后端不引时区库——
/// 见 `fs_connmgr::civil` 模块头）。**东为正**，与 JS 的 `getTimezoneOffset()` 符号
/// 相反：那个函数返回的是「本地时间要加多少分钟才等于 UTC」，东八区给 -480。
/// 前端必须取反后再传，契约写在这里以免两边各按自己的直觉理解。
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskForm {
    pub name: String,
    pub cron: String,
    pub command: String,
    pub profile_id: String,
    /// "skip" | "once"
    pub catchup: String,
    pub tz_offset_minutes: i32,
    /// true = 每一跳取系统当前本地偏移（跟随夏令时）；false = 用上面的固定偏移
    pub tz_follows_dst: bool,
}

impl TaskForm {
    fn spec(&self) -> TaskSpec<'_> {
        TaskSpec {
            name: &self.name,
            cron: &self.cron,
            command: &self.command,
            profile_id: &self.profile_id,
            catchup: CatchUp::parse(&self.catchup),
            tz_offset_minutes: self.tz_offset_minutes,
            tz_follows_dst: self.tz_follows_dst,
        }
    }
}

/// cron 预览结果。
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CronPreview {
    pub valid: bool,
    /// 不合法时的原因（指出是哪个字段错了）
    pub error: Option<String>,
    /// 接下来几次触发的 unix 秒；**空数组是有信息的**——它意味着「一年内不会触发」
    pub next_fires: Vec<i64>,
    /// `next_fires` 为空且 valid 时的说明（UI 必须显示，不能只给一个空列表）
    pub note: Option<String>,
}

/// 校验 cron 并预览接下来几次触发。
///
/// 存在的理由：一个人肉解读 `*/15 9-17 * * 1-5` 的对错率远低于看三个具体时间。
/// 校验与预览**共用后端这一份实现**——前端另写一份 cron 解析器就是两套规则，
/// 迟早出现「界面说合法、后端说不合法」。
#[tauri::command]
pub async fn schedule_preview_cron(cron: String, tz_offset_minutes: i32) -> CronPreview {
    match fs_connmgr::cron::Cron::parse(&cron) {
        Err(e) => CronPreview {
            valid: false,
            error: Some(e.to_string()),
            next_fires: Vec::new(),
            note: None,
        },
        Ok(c) => {
            let now = crate::scheduler::now_secs();
            let fires = c.next_fires(now, tz_offset_minutes, PREVIEW_COUNT);
            let note = if fires.is_empty() {
                Some(
                    "该表达式在未来一年内不会触发（例如 2 月 30 日这种不存在的日期）——请检查"
                        .into(),
                )
            } else {
                None
            };
            CronPreview {
                valid: true,
                error: None,
                next_fires: fires,
                note,
            }
        }
    }
}

#[tauri::command]
pub async fn schedule_list(state: State<'_, Arc<AppState>>) -> Result<Vec<ScheduledTask>, String> {
    ScheduleRepo::new(state.db.pool())
        .list()
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn schedule_create(
    form: TaskForm,
    state: State<'_, Arc<AppState>>,
) -> Result<i64, String> {
    ScheduleRepo::new(state.db.pool())
        .create(form.spec(), crate::scheduler::now_secs())
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn schedule_update(
    id: i64,
    form: TaskForm,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    let now_minute = fs_connmgr::civil::minute_index(crate::scheduler::now_secs());
    ScheduleRepo::new(state.db.pool())
        .update(id, form.spec(), now_minute)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn schedule_set_enabled(
    id: i64,
    enabled: bool,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    let now_minute = fs_connmgr::civil::minute_index(crate::scheduler::now_secs());
    ScheduleRepo::new(state.db.pool())
        .set_enabled(id, enabled, now_minute)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn schedule_delete(id: i64, state: State<'_, Arc<AppState>>) -> Result<(), String> {
    ScheduleRepo::new(state.db.pool())
        .delete(id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn schedule_runs(
    task_id: i64,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<ScheduledRun>, String> {
    ScheduleRepo::new(state.db.pool())
        .recent_runs(task_id, RUNS_LIMIT)
        .await
        .map_err(|e| e.to_string())
}

/// 立刻手动执行一次（不改 `last_fire_minute`，不影响定时节奏）。
///
/// 这条命令的价值在于**当场验证任务写对了没有**：不然用户要等到凌晨三点才知道命令
/// 拼错了、或者那台机器上压根没有那个脚本。手动执行的记录 `catchup = false`，
/// 与定时触发混在一列里按时间排——用户要的是「这条任务的执行史」，不是两份清单。
#[tauri::command]
pub async fn schedule_run_now(id: i64, state: State<'_, Arc<AppState>>) -> Result<(), String> {
    let repo = ScheduleRepo::new(state.db.pool());
    let task = repo
        .list()
        .await
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|t| t.id == id)
        .ok_or_else(|| format!("计划任务 {id} 不存在"))?;
    // 刻意**不**走重叠保护：这是用户显式按下的动作，他知道自己在做什么；
    // 而定时触发的重叠保护是为了防止无人值守时堆积。
    crate::scheduler::run_once_now(state.inner(), &task).await;
    Ok(())
}
