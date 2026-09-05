//! 计划任务的调度循环（M4a）。
//!
//! 判定在 [`fs_connmgr::schedule`]（纯函数，离线可钉）；这里只做三件外部世界的事：
//! **按 tick 走时钟**、**找一条能执行的会话**、**把结果记下来并通知前端**。
//!
//! ## 只在已有会话上执行，不自动建连接
//!
//! 到点了而那台机器没有活动会话时，本调度器**记一条 `skipped` 并说明原因**，
//! 不去自动拨号。这是一个刻意的取舍，而不是没做完：
//!
//! - 自动拨号要凭据，而凭据在保险库里，保险库有闲置自动锁定（`vault.autoLockMinutes`）。
//!   凌晨三点无人值守时保险库大概率是锁着的——要让它不锁，就得让用户为了一条定时任务
//!   永久关掉自动锁定。那是把一个安全决策**替用户做了**。
//! - 「任务没跑」是可见且可解释的（记录里写着「没有该连接的活动会话」）；
//!   「任务替你解锁了保险库并连了上去」是不可见的。
//!
//! 所以产品口径是：计划任务在**你开着会话的时候**替你按点执行。UI 必须把这条说清楚，
//! 而不是让用户以为它是 crontab 的替代品。
//!
//! ## 同一连接上串行，不排队
//!
//! 同一档案可能同时挂多条任务，而它们全部经 `exec_adapter_for` 拿**同一条**会话锁
//! （state.rs 的 RusshExecAdapter：开通道→执行→收尾全程持锁）。不做互斥的话，A 跑
//! 长命令时 B 只在锁上排队，B 的超时预算在纯等待中烧光——最后落一条「远端可能仍在
//! 运行」的 timeout 记录，而 B 的命令**从未发出**：错误的理由比没有理由更坏。故此处
//! 按 profile 做互斥：同连接已有任务在执行时，后来者记 `skipped` 并点名阻塞者。
//!
//! ## 为什么每个 tick 都查库
//!
//! 任务表只有几行，一次 `SELECT` 是微秒级；而「内存缓存 + 失效」要在创建/修改/删除/
//! 启停四个入口都记得刷新，漏一个的表现是「改了 cron 但还按老的跑」——一个极难自证
//! 的 bug。用查库换掉整类缓存失效错误，是划算的。

use fs_connmgr::schedule::{decide, CatchUp, Decision, Input, TICK_SECONDS};
use fs_connmgr::schedule_repo::{ScheduleRepo, ScheduledTask};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter};

/// 单条任务执行的外层兜底超时。
///
/// **必须大于** `exec_adapter_for` 内层的 `EXEC_TOTAL_TIMEOUT`（600s）：内层先到点、
/// 带着「未观测到退出码」的语义正常返回，本层只是兜底（防内层预算被计算错）。
/// 若本层先到点，落下的原因会是「已放弃等待」——那对一条其实没跑多久的命令是
/// 误导（见上方「同一连接上串行」：早期版本曾把本层设成与内层相等，抢跑的原因
/// 指向一个不存在的远端超时）。
const RUN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(660);

/// 执行中占位：按任务 id（防同任务重叠）与按 profile（防同连接排队）各一份。
#[derive(Default)]
struct InFlight {
    tasks: Mutex<HashSet<i64>>,
    profiles: Mutex<HashSet<String>>,
}

/// 占位（重叠保护）：已在执行中则返回 false。
///
/// 抽成独立的**同步**函数不是风格问题：`MutexGuard` 不是 `Send`，只要它的作用域被
/// 编译器判成跨过一个 `.await`，整个调度 future 就不再 `Send`，`spawn` 直接编译不过。
/// 内联写「加锁→判断→drop→await」时编译器就是这么判的（显式 `drop` 也不管用）。
/// 独立函数让锁在返回时必然释放，跨 await 持锁在结构上不可能发生。
impl InFlight {
    /// 占位：两个维度**都空闲才都占上**，否则一个都不占。
    ///
    /// 「先插后报」的写法有一个泄漏：任务维度被拒时，profile 维度已经被插进去了
    /// ——此后该连接永远显示「忙」，直到进程重启（占位测试的第一版就是被这个
    /// 咬红的）。先判后插、要么都占要么都不占，泄漏在结构上不可能发生。
    fn claim(&self, task_id: i64, profile_id: &str) -> (bool, bool) {
        let mut t = self.tasks.lock().unwrap_or_else(|p| p.into_inner());
        let mut p = self.profiles.lock().unwrap_or_else(|p| p.into_inner());
        let task_free = !t.contains(&task_id);
        let profile_free = !p.contains(profile_id);
        if task_free && profile_free {
            t.insert(task_id);
            p.insert(profile_id.to_string());
        }
        (task_free, profile_free)
    }

    fn release(&self, task_id: i64, profile_id: &str) {
        self.tasks
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&task_id);
        self.profiles
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(profile_id);
    }
}

/// 回拨暂停的「已在暂停中」标记：让暂停留痕只在**进入**暂停那一刻落一条，
/// 而不是每 2 秒一拍都落一条（回拨 10 分钟 = 300 条重复行）。
///
/// 进程重启会丢这个内存标记，最坏情形是重启后又落一条暂停行——可接受，比持久化
/// 一个只为此服务的字段便宜得多。
#[derive(Default)]
struct PauseTracker {
    paused: Mutex<HashSet<i64>>,
}

impl PauseTracker {
    /// 返回 true = 本次是**进入**暂停（该落痕）；false = 仍在暂停中（别重复落）。
    fn enter(&self, task_id: i64) -> bool {
        self.paused
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(task_id)
    }
    fn leave(&self, task_id: i64) {
        self.paused
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&task_id);
    }
}

/// 启动调度循环。随进程存活（与 vault 自动锁定巡检同款：无停止入口，进程退出即止）。
pub fn spawn(state: Arc<crate::state::AppState>) {
    tauri::async_runtime::spawn(async move {
        let in_flight = Arc::new(InFlight::default());
        let pauses = Arc::new(PauseTracker::default());
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(TICK_SECONDS));
        // Burst 会在卡顿后连补几拍，而我们要的是「下一拍照常」——补拍对定时任务毫无意义
        // （同一分钟的重复 tick 会被评估水位挡掉，只是白跑几次判定）。
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            if let Err(e) = tick_once(&state, &in_flight, &pauses).await {
                // 一次 tick 失败不能让循环退出：退出即「计划任务从此静默失效」，
                // 而这正是最难发现的一类故障（库被锁一瞬、磁盘抖一下都会走到这里）。
                tracing::warn!(error = %e, "计划任务 tick 失败，下一拍重试");
            }
        }
    });
}

/// 走一拍：把到点的任务派发出去。
async fn tick_once(
    state: &Arc<crate::state::AppState>,
    in_flight: &Arc<InFlight>,
    pauses: &Arc<PauseTracker>,
) -> Result<(), String> {
    let now = now_secs();
    let now_minute = fs_connmgr::civil::minute_index(now);
    let repo = ScheduleRepo::new(state.db.pool());
    let tasks = repo.list().await.map_err(|e| e.to_string())?;

    for t in tasks {
        if !t.enabled {
            continue;
        }
        // cron 在写入时已校验；这里再失败只可能是库被手改过。跳过并留痕，不让一条
        // 坏数据把整轮 tick 带崩（那会让**其余任务**也一起不跑）。
        let cron = match fs_connmgr::cron::Cron::parse(&t.cron) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(task = t.id, cron = %t.cron, error = %e, "计划任务的 cron 不合法，已跳过");
                continue;
            }
        };
        // 跟随 DST 的任务每跳取**当前**本地偏移；固定偏移任务用存的值（可复现）
        let tz = if t.tz_follows_dst {
            local_offset_minutes()
        } else {
            t.tz_offset_minutes as i32
        };
        let decision = decide(
            &cron,
            Input {
                now_secs: now,
                created_minute: fs_connmgr::civil::minute_index(t.created_at),
                checked_minute: t.checked_minute,
                tz_offset_minutes: tz,
                catchup: CatchUp::parse(&t.catchup),
            },
        );
        match decision {
            Decision::Idle => {
                pauses.leave(t.id); // 恢复正常（含上一拍还在暂停、这一拍已追上的情形）
                                    // 推水位：否则 Skip 策略的每 2 秒一跳都重扫同一段窗口
                repo.advance_checked(t.id, now_minute)
                    .await
                    .map_err(|e| e.to_string())?;
            }
            Decision::PausedForClockRollback => {
                // 有痕暂停：进入暂停的那一刻落一条 skipped 说明「判定停了、为什么停」，
                // 而不是任务列表看着正常却再也不触发。持续暂停期间不重复落
                // （PauseTracker 内存去重；水位由 repo 的 MAX 语义保持不动）。
                let gap = t.checked_minute - now_minute;
                if pauses.enter(t.id) {
                    let detail = format!("系统时钟被回拨约 {gap} 分钟，调度暂停至系统时间追上");
                    repo.record_skip_and_advance(t.id, now, &detail, now_minute)
                        .await
                        .map_err(|e| e.to_string())?;
                    tracing::warn!(task = t.id, gap, "计划任务：时钟回拨，判定暂停");
                }
            }
            Decision::SkipPolicy {
                missed,
                last_missed_minute,
                clamped,
            } => {
                pauses.leave(t.id);
                // Skip 也要留痕：「已按策略跳过 N 次」。留痕与推水位是同一事务
                // （repo.record_skip_and_advance），不会重复落、不会无声。
                let mut detail = format!(
                    "已按策略跳过 {missed} 次触发，最近一次原定 {}",
                    minute_label(last_missed_minute, tz)
                );
                if clamped {
                    detail.push_str("（更早的错过已超出 7 天回看窗口，无法计入）");
                }
                repo.record_skip_and_advance(t.id, now, &detail, now_minute)
                    .await
                    .map_err(|e| e.to_string())?;
                tracing::info!(task = t.id, name = %t.name, missed, "计划任务按策略跳过并留痕");
                emit_run(&state.app, &t, now, "skipped", false, None, &detail);
            }
            Decision::Fire { catchup } => {
                pauses.leave(t.id);
                // **先落触发再执行**（commit_fire 单语句写 last_fire+水位）：命令可能
                // 跑十分钟，而 tick 每 2 秒一拍，不先落会连发。取舍是「至多一次」：
                // 落完与 spawn 之间被杀的窗口里该次触发不会重跑——last_fire 显示
                // 触发过但没有执行记录，是用户可察觉的异常组合，不是静默丢失。
                // 取「至少一次」会让非幂等命令（删旧备份再重建）重复执行，更危险。
                repo.commit_fire(t.id, now_minute)
                    .await
                    .map_err(|e| e.to_string())?;

                // 双重占位：同任务不重叠；同连接不排队（排队会在等锁中烧光超时
                // 预算，并落一条指向不存在之远端超时的误导记录——见模块头）。
                let (task_free, profile_free) = in_flight.claim(t.id, &t.profile_id);
                if !task_free {
                    record(
                        state,
                        &t,
                        now,
                        "skipped",
                        catchup,
                        None,
                        "上一轮仍在执行（本次跳过）",
                    )
                    .await;
                    continue;
                }
                if !profile_free {
                    // claim 要么都占要么都不占（见 InFlight::claim），这里无需回滚
                    record(
                        state,
                        &t,
                        now,
                        "skipped",
                        catchup,
                        None,
                        "同一连接上另一条计划任务正在执行（本程序在同一条连接上串行执行，不排队）",
                    )
                    .await;
                    continue;
                }

                let st = state.clone();
                let flight = in_flight.clone();
                let task = t.clone();
                tauri::async_runtime::spawn(async move {
                    run_one(&st, &task, now, catchup).await;
                    flight.release(task.id, &task.profile_id);
                });
            }
        }
    }
    Ok(())
}

/// 执行一条任务并记录结果。
async fn run_one(
    state: &Arc<crate::state::AppState>,
    t: &ScheduledTask,
    fired_at: i64,
    catchup: bool,
) {
    // 找一条该档案的活动会话（代次最大者 = 最近建立的那条）
    let found = state
        .registry
        .find_latest_by(|s: &crate::sessions::LiveSession| s.profile_id == t.profile_id);
    let Some((session_id, _)) = found else {
        record(
            state,
            t,
            fired_at,
            "skipped",
            catchup,
            None,
            "没有该连接的活动会话——计划任务只在已开启的会话上执行（见「计划任务」说明）",
        )
        .await;
        return;
    };

    let exec = match state.exec_adapter_for(&session_id).await {
        Ok(e) => e,
        Err(e) => {
            record(
                state,
                t,
                fired_at,
                "skipped",
                catchup,
                None,
                &format!("取不到执行通道：{e}"),
            )
            .await;
            return;
        }
    };

    // 外层兜底（见 RUN_TIMEOUT）：内层 EXEC_TOTAL_TIMEOUT 先到点、以「未观测到
    // 退出码」的语义返回；本层只在预算算错时兜底。
    let outcome = tokio::time::timeout(RUN_TIMEOUT, exec.exec_once(&t.command)).await;
    match outcome {
        Err(_) => {
            record(
                state, t, fired_at, "failed", catchup, None,
                &format!(
                    "执行超过 {} 秒未返回（兜底超时；内层 {} 秒预算应先到点，走到这里说明预算被算错）",
                    RUN_TIMEOUT.as_secs(),
                    fs_sshengine::timeouts::EXEC_TOTAL_TIMEOUT.as_secs()
                ),
            )
            .await;
        }
        Ok(Err(e)) => {
            record(
                state,
                t,
                fired_at,
                "failed",
                catchup,
                None,
                &format!("通道错误：{e}"),
            )
            .await;
        }
        Ok(Ok(o)) => {
            // code=None（超时/通道异常）**不是**退出码 255：作为「未观测到」如实记录。
            // 成败以退出码为准，不以有无输出为准（很多命令成功时一个字都不打印）。
            match o.code {
                Some(0) => {
                    record(state, t, fired_at, "ok", catchup, Some(0), o.stdout.trim()).await
                }
                Some(c) => {
                    let e = o.stderr.trim();
                    let detail = if e.is_empty() {
                        format!("远端 exit {c}，无 stderr 输出")
                    } else {
                        e.to_string()
                    };
                    record(state, t, fired_at, "failed", catchup, Some(c), &detail).await;
                }
                None => {
                    record(
                        state, t, fired_at, "failed", catchup, None,
                        "未观测到退出码（命令超时或通道异常）；远端进程可能仍在运行，本程序无法确认其状态",
                    )
                    .await;
                }
            }
        }
    }
}

/// 落一条执行记录并通知前端。
#[allow(clippy::too_many_arguments)] // 参数即记录字段，打包成结构体只是把同样的东西挪个地方
async fn record(
    state: &Arc<crate::state::AppState>,
    t: &ScheduledTask,
    fired_at: i64,
    outcome: &str,
    catchup: bool,
    exit_code: Option<i32>,
    detail: &str,
) {
    let repo = ScheduleRepo::new(state.db.pool());
    if let Err(e) = repo
        .record_run(t.id, fired_at, outcome, catchup, exit_code, detail)
        .await
    {
        tracing::warn!(task = t.id, error = %e, "写执行记录失败");
    }
    match outcome {
        "ok" => tracing::info!(task = t.id, name = %t.name, catchup, "计划任务执行成功"),
        // 失败与跳过都记 warn 并带上原因：这两条是用户第二天要看的东西
        _ => {
            tracing::warn!(task = t.id, name = %t.name, outcome, catchup, detail, "计划任务未成功")
        }
    }
    emit_run(&state.app, t, fired_at, outcome, catchup, exit_code, detail);
}

/// 手动「立即执行一次」（`schedule_run_now` 命令）。
///
/// 与定时触发共用同一条执行路径 [`run_one`]，只是**不动评估水位**：手动跑一次
/// 不该把定时节奏往后推（用户 10:00 手动试了一次，凌晨 3:00 那次照跑）。
/// 也不走重叠保护——这是用户显式按下的动作。
pub async fn run_once_now(state: &Arc<crate::state::AppState>, t: &ScheduledTask) {
    run_one(state, t, now_secs(), false).await;
}

/// 唯一的 `schedule:run` 发送点（键集恒定，理由同 zmodem_bridge 的 emit_progress）。
#[allow(clippy::too_many_arguments)]
fn emit_run(
    app: &AppHandle,
    t: &ScheduledTask,
    fired_at: i64,
    outcome: &str,
    catchup: bool,
    exit_code: Option<i32>,
    detail: &str,
) {
    let _ = app.emit(
        "schedule:run",
        serde_json::json!({
            "taskId": t.id,
            "name": t.name,
            "firedAt": fired_at,
            "outcome": outcome,
            "catchup": catchup,
            "exitCode": exit_code,
            "detail": detail,
        }),
    );
}

/// 当前本地时区偏移（分钟，东为正）。跟随 DST 的任务每一跳取这个值。
///
/// 取不到（极旧平台）回落 0 并留痕：不让一条任务的调度因为取不到偏移而整个停摆，
/// 但要把「按 UTC 判定了」说出来——那是可从记录里查到的事实。
fn local_offset_minutes() -> i32 {
    use chrono::Offset;
    let off = chrono::Local::now().offset().fix().local_minus_utc() / 60;
    if !(-12 * 60..=14 * 60).contains(&off) {
        tracing::warn!(off, "本地时区偏移超出常识范围，按 UTC 判定");
        return 0;
    }
    off
}

/// 分钟序号 → 人可读（按任务自己的偏移）。
fn minute_label(minute: i64, tz_offset_minutes: i32) -> String {
    let p = fs_connmgr::civil::parts_at(minute * 60, tz_offset_minutes);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        p.year, p.month, p.day, p.hour, p.minute
    )
}

/// 当前 unix 秒。时钟早于 1970 时回 0（坏时钟不该让调度循环 panic）。
pub fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 外层兜底必须**大于**内层总预算：内层先到点才能带着「未观测到退出码」的
    /// 语义返回；外层抢跑会落一条指向不存在之远端超时的误导记录。
    #[test]
    fn outer_timeout_strictly_exceeds_the_inner_one() {
        assert!(
            RUN_TIMEOUT > fs_sshengine::timeouts::EXEC_TOTAL_TIMEOUT,
            "外层 {}s 必须大于内层 {}s",
            RUN_TIMEOUT.as_secs(),
            fs_sshengine::timeouts::EXEC_TOTAL_TIMEOUT.as_secs()
        );
        assert!(RUN_TIMEOUT.as_secs() <= 3600, "太大等于没有兜底");
    }

    /// 双重占位的语义：同任务第二次被拒、同 profile 第二次被拒、互不干扰。
    ///
    /// 这两条是「同一任务不并发跑两份」与「同连接不排队烧预算」的**全部**机制；
    /// 换成不去重的容器，保护就整体失效而没有任何编译期信号。
    #[test]
    fn in_flight_rejects_same_task_and_same_profile_twice() {
        let f = InFlight::default();
        assert_eq!(f.claim(7, "p1"), (true, true), "首次应全部占上");
        assert_eq!(f.claim(7, "p1"), (false, false), "同任务同连接都必须被拒");
        // 同任务不同连接：任务维度拒
        assert!(!f.claim(7, "p2").0, "同任务第二次必须被拒——否则并发跑两份");
        // 同任务被拒时 profile **不得被占上**（先插后报的旧写法在这里泄漏 p2：
        // 此后该连接永远显示忙，直到进程重启）
        assert_eq!(
            f.claim(9, "p2"),
            (true, true),
            "上一次被拒的 claim 不得把 p2 占住"
        );
        // 不同任务同连接：连接维度拒
        assert!(
            !f.claim(8, "p1").1,
            "同连接第二条任务必须被拒——否则排队烧预算"
        );
        // 被拒的 claim 不得把任务 8 留成半占位：换一条空闲连接应能全占上
        assert_eq!(
            f.claim(8, "p9"),
            (true, true),
            "上一次被拒的 claim 不得把任务 8 占住"
        );
        // 释放后可再次执行
        f.release(7, "p1");
        assert_eq!(f.claim(7, "p1"), (true, true), "跑完释放后可再次执行");
    }

    /// 本地偏移落在真实时区范围内（护栏：异常平台回落 0 而不是拿着离谱值判定）。
    #[test]
    fn local_offset_is_within_real_timezone_bounds() {
        let off = local_offset_minutes();
        assert!(
            (-12 * 60..=14 * 60).contains(&off),
            "本机偏移 {off} 分钟超出真实时区范围"
        );
    }

    #[test]
    fn now_secs_is_in_a_sane_range() {
        let n = now_secs();
        assert!(n > 1_577_836_800, "应晚于 2020，实得 {n}");
        assert!(
            n < 4_102_444_800,
            "应早于 2100（单位是秒而非毫秒），实得 {n}"
        );
    }

    /// 分钟序号的人可读标签（按任务偏移）。
    #[test]
    fn minute_label_uses_the_given_offset() {
        // 2026-08-21 09:03 UTC（civil.rs 样例同源）→ +8h = 17:03
        let m = 1_787_302_980 / 60;
        assert_eq!(minute_label(m, 0), "2026-08-21 09:03");
        assert_eq!(minute_label(m, 8 * 60), "2026-08-21 17:03");
    }
}
