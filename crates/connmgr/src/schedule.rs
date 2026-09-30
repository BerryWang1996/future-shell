//! 计划任务的**触发判定**（M4a）。纯函数，时钟由调用方注入。
//!
//! 出口标准是「按 cron 定时触发（误差 ±5s）、重启后补跑策略单测」。把判定与副作用
//! 切开正是为了让这两条能**离线**钉住：不靠 sleep、不靠真实时钟、不靠真服务器。
//!
//! ## 「误差 ±5s」是什么误差
//!
//! 是**派发**误差，不含远端执行耗时。cron 的粒度是分钟，所以判定只回答「这一分钟
//! 该不该跑」；从分钟边界到实际派发的延迟上界就是调度器的 tick 周期（见
//! [`TICK_SECONDS`]）。远端命令自己跑多久与本判定无关，也不该被算进这个误差里——
//! 把执行耗时算进来的话，任何一条跑 10 秒的命令都会让「±5s」永远不达标。
//!
//! ## 两个水位，不许混
//!
//! - `checked_minute`：**评估水位**。「到这一分钟为止的全部触发点都已处置完毕」。
//!   它只升不降；判定只扫 `(checked, now]`。它的存在让 Skip 策略的留痕**只落一次**
//!   （处置完就推水位，下一 tick 不再重扫同一段），也让时钟回拨不可能重放。
//! - `last_fire_minute`：**上次真实触发**，纯展示用（任务列表的「上次触发」列）。
//!   它不参与判定——一旦让它兼职水位，「按策略跳过」就必须推它，展示就成了谎话。
//!
//! 两者曾在初版混成一个量（`last_fire_minute` 兼任水位），交叉对抗审计指出那样要么
//! 留痕重复落库、要么展示撒谎，此处是按审计结论重写的。
//!
//! ## 补跑（重启后）
//!
//! 只有两种策略，且刻意**不提供「全部补跑」**：应用关了三天再打开，一条每分钟的
//! 任务会瞬间排出 4320 次执行——那不是补跑而是自我拒绝服务。
//!
//! **Skip 也要留痕**：「跳过」不等于「无声」。静默跳过会让用户以为任务跑过了；
//! [`Decision::SkipPolicy`] 带回 `missed` 计数与最近一次原定时刻，调用方据此落一条
//! 执行记录（「已按策略跳过 3 次」），而不是什么都不发生。
//!
//! ## 时钟回拨
//!
//! 回拨后 `now < checked`，判定返回 `Idle`——**暂停但不重放**。恢复到水位之后自然
//! 续上。回拨期间绝不在本地时间戳上做加法去「发明」不存在的分钟（那会凭空造出
//! 跳变区间里的触发点）。
//!
//! ## DST（夏令时）
//!
//! 判定吃的是「unix 分钟 + 一个偏移」；偏移**跟随与否**由调用方决定（任务可存固定
//! 偏移，也可标记为跟随系统时钟，由调度器每跳重取当前偏移）。春季前跳时不存在的那
//! 一小时：补跑扫描遍历的是真实 unix 瞬间（每步 +60 秒再换算），不存在的本地分钟
//! 永远不会被观察到，因此那天不跑、也不发明替代运行。**绝不能在本地时间戳上做
//! 加法**——这句是最容易被后来者「优化」掉的一处。

use crate::civil::minute_index;
use crate::cron::{Cron, Fields};

/// 调度器的 tick 周期（秒）。
///
/// 分钟边界到实际派发的延迟上界就是这个值，所以它直接决定出口标准里的「±5s」。
/// 取 2 秒而不是 5 秒：5 秒恰好等于上限，一点抖动（一次 GC、一次磁盘停顿）就越界；
/// 2 秒留了 2.5 倍余量。代价是每 2 秒一次**纯内存**的匹配（任务表在内存里缓存，
/// 见调度器），不查库，可忽略。
pub const TICK_SECONDS: u64 = 2;

// 「±5s」的算术依据是编译期事实，用 const 断言钉住（比 #[test] 更早、更硬）：
// ① tick 周期就是派发误差的上界，超过 5 秒直接违反出口标准；
// ② 留至少 2 倍余量——恰好等于 5 秒时一次抖动（GC、磁盘停顿）就越界；
// ③ 别做成忙等。
const _: () = assert!(TICK_SECONDS <= 5);
const _: () = assert!(TICK_SECONDS * 2 <= 5);
const _: () = assert!(TICK_SECONDS >= 1);

/// 补跑回看窗口上限（分钟）。7 天。
///
/// 走这么长的分钟循环是为了回答「关机期间有没有错过一次」。7 天足以覆盖任何
/// 周期 ≤ 一周的任务；更长周期的任务（每月 1 号）错过之后不补跑——窗口被 [`Decision`]
/// 的 `clamped` 标记如实带出（「更早的错过已超出回看窗口」），而不是把窗口开到一年、
/// 让每次 tick 都可能走 52 万次循环。
pub const MAX_CATCHUP_LOOKBACK_MINUTES: i64 = 7 * 24 * 60;
// 护栏是编译期事实：上限防「每 tick 走几十万次循环」，下限保证「隔夜关机」补得上
const _: () = assert!(MAX_CATCHUP_LOOKBACK_MINUTES <= 10_080);
const _: () = assert!(MAX_CATCHUP_LOOKBACK_MINUTES >= 1440);

/// 补跑策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatchUp {
    /// 错过就算了（默认）——但会留痕（[`Decision::SkipPolicy`]）。
    Skip,
    /// 错过多次也只补跑一次。
    Once,
}

impl CatchUp {
    /// 从库里的字符串解析。未知值回落 [`CatchUp::Skip`]。
    ///
    /// 这里**可以**回落，与信号名解析（procs.rs）不同：那边猜错会杀掉用户没打算杀的
    /// 进程，这边猜错的后果是「少补跑一次」——而 Skip 恰好是更保守的那一侧。
    /// 未来若加了第三种策略而旧版本读到它，退化成 Skip 也是安全方向。
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "once" => Self::Once,
            _ => Self::Skip,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Skip => "skip",
            Self::Once => "once",
        }
    }
}

/// 判定结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// 这一刻无事可做（含时钟回拨期间的**有痕暂停**——见 [`Decision::PausedForClockRollback`]）。
    Idle,
    /// 该跑。`catchup = true` 表示这是一次补跑（UI 要能看出「这次是补的」）
    Fire { catchup: bool },
    /// 按 Skip 策略跳过，但**必须留痕**：
    /// `missed` = 错过的触发次数；`last_missed_minute` = 最近一次原定时刻（分钟序号）；
    /// `clamped` = 还有更早的错过已被回看窗口甩掉（超出 7 天），留痕文案要提到。
    SkipPolicy {
        missed: usize,
        last_missed_minute: i64,
        clamped: bool,
    },
    /// 时钟被回拨到了评估水位之前：判定暂停，直到系统时间追上。
    ///
    /// 单独一个变体而不是折进 `Idle`，因为调用方要对它**落一条用户可见的记录**
    /// （「时钟回拨 N 分钟，调度暂停至系统时间追上」）——静默暂停正是最难查的
    /// 那类故障：任务列表看起来一切正常，只是再也不触发。
    PausedForClockRollback,
}

/// 判定的输入。
///
/// 用具名结构而不是一串位置参数：几个 `i64` 挨在一起，调用方把 `created_minute` 和
/// `checked_minute` 传反了编译器一声不响，而那个错误的表现是「新建的任务立刻补跑了
/// 它诞生之前的触发」。
#[derive(Debug, Clone, Copy)]
pub struct Input {
    /// 现在（unix 秒）
    pub now_secs: i64,
    /// 该任务创建时刻的分钟序号——补跑窗口的**硬左界**。
    ///
    /// 少了这一项就会有这个 bug：早上 10:00 新建一条 `0 3 * * *` 且策略为 Once 的任务，
    /// 下一个 tick 立刻「补跑」今天凌晨 3 点那次——而那次触发发生在任务存在之前。
    pub created_minute: i64,
    /// 评估水位：到这一分钟为止的触发点都已处置完毕。只升不降。
    pub checked_minute: i64,
    /// 任务时区偏移（分钟，东为正）。跟随 DST 的任务由调用方在**每一跳**取当前值。
    pub tz_offset_minutes: i32,
    pub catchup: CatchUp,
}

/// 该不该在此刻触发。
///
/// 规则（顺序即优先级）：
/// 1. `now <= checked` → 回拨则 [`Decision::PausedForClockRollback`]（水位之前），
///    否则 `Idle`（本分钟已评估过）。
/// 2. 本分钟命中 cron → [`Decision::Fire`]（`catchup = false`）。
/// 3. `(左界, 本分钟)` 之间存在命中：
///    - Skip → [`Decision::SkipPolicy`]（留痕但不执行）；
///    - Once → [`Decision::Fire`]（`catchup = true`，只补最近一场）。
/// 4. 其余 → `Idle`。
///
/// 左界 = max(checked+1, created, 本分钟 − 回看上限)；被回看上限截到时 `clamped` 置真。
///
/// **调用方义务**：拿到任何非回拨的结果后，把水位推到本分钟（只升不降）。
/// 推水位与本结果的处理应当落库为一次原子写——先推水位再执行是「至多一次」，
/// 先执行再推水位是「至少一次」，两者取一并在调度器注释里写明，不许悬空。
pub fn decide(cron: &Cron, input: Input) -> Decision {
    let now_min = minute_index(input.now_secs);

    // ① 已评估过（含同一分钟内的重复 tick 与时钟回拨）
    if now_min <= input.checked_minute {
        if now_min < input.checked_minute {
            return Decision::PausedForClockRollback;
        }
        return Decision::Idle;
    }

    // ② 本分钟命中 → 正常触发（不是补跑）
    if matches_minute(cron, now_min, input.tz_offset_minutes) {
        return Decision::Fire { catchup: false };
    }

    // ③ 窗口内的历史命中
    let mut left = input.checked_minute + 1;
    left = left.max(input.created_minute);
    let unclamped_left = left;
    left = left.max(now_min - MAX_CATCHUP_LOOKBACK_MINUTES);
    let clamped = left > unclamped_left;

    let mut missed: usize = 0;
    let mut last_missed_minute: i64 = 0;
    let mut m = left;
    while m < now_min {
        if matches_minute(cron, m, input.tz_offset_minutes) {
            missed += 1;
            last_missed_minute = m; // 循环递增，最后一个即最近的一场
        }
        m += 1;
    }
    if missed == 0 {
        return Decision::Idle;
    }
    match input.catchup {
        CatchUp::Skip => Decision::SkipPolicy {
            missed,
            last_missed_minute,
            clamped,
        },
        CatchUp::Once => Decision::Fire { catchup: true },
    }
}

fn matches_minute(cron: &Cron, minute: i64, tz_offset_minutes: i32) -> bool {
    // 补跑扫描每步 +60 **unix 秒**再换算——绝不在本地时间戳上做加法：
    // 那会凭空造出 DST 前跳区间里不存在的本地分钟（见模块头）。
    cron.matches(Fields::from_parts(crate::civil::parts_at(
        minute * 60,
        tz_offset_minutes,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-08-21 00:00:00 UTC（与 civil.rs 的样例同源）
    const DAY: i64 = 1_787_270_400;
    const MIN: i64 = 60;

    fn cron(expr: &str) -> Cron {
        Cron::parse(expr).unwrap()
    }

    /// 便捷构造：默认「刚创建、已评估到创建时刻、UTC、不补跑」。
    fn input(now_secs: i64) -> Input {
        Input {
            now_secs,
            created_minute: minute_index(DAY),
            checked_minute: minute_index(DAY),
            tz_offset_minutes: 0,
            catchup: CatchUp::Skip,
        }
    }

    // ── 正常触发 ──────────────────────────────────────────────────────────────

    /// 命中的那一分钟内，**任意一秒**都判 Fire。
    ///
    /// 这就是「±5s」能成立的前提：调度器只要在这一分钟里 tick 到一次就会派发，
    /// 而 tick 周期是 2 秒，所以从分钟边界到派发的延迟上界是 2 秒。
    #[test]
    fn fires_at_any_second_within_the_matching_minute() {
        let c = cron("30 3 * * *");
        let at_0330 = DAY + 3 * 3600 + 30 * MIN;
        for sec in [0, 1, 2, 5, 30, 59] {
            assert_eq!(
                decide(&c, input(at_0330 + sec)),
                Decision::Fire { catchup: false },
                "03:30:{sec:02} 应触发"
            );
        }
        // 前后相邻的分钟都不该触发。03:31 那一条要把水位设到「已评估过 03:30」，
        // 否则窗口里含 03:30 的命中，判定为 SkipPolicy（留痕）而非 Idle——那是
        // 正确行为，只是不是本条要测的。
        assert_eq!(decide(&c, input(at_0330 - 1)), Decision::Idle, "03:29:59");
        let mut after = input(at_0330 + 60);
        after.checked_minute = minute_index(at_0330);
        assert_eq!(decide(&c, after), Decision::Idle, "03:31:00");
    }

    /// 同一分钟内的第二次 tick 判 Idle（水位已推到本分钟）。
    ///
    /// 去重靠的是**分钟序号 + 水位**，而不是原始秒：一次 tick 周期内可能查到两次，
    /// 用秒做判定每次都是新的。
    #[test]
    fn does_not_fire_twice_in_the_same_minute() {
        let c = cron("* * * * *");
        let t = DAY + 10 * MIN;
        let mut inp = input(t);
        assert_eq!(decide(&c, inp), Decision::Fire { catchup: false });
        // 调度器触发后把水位推到本分钟
        inp.checked_minute = minute_index(t);
        for sec in [0, 2, 4, 58] {
            inp.now_secs = t + sec;
            assert_eq!(
                decide(&c, inp),
                Decision::Idle,
                "同一分钟第 {sec} 秒不该再触发"
            );
        }
        // 下一分钟照常
        inp.now_secs = t + 60;
        assert_eq!(decide(&c, inp), Decision::Fire { catchup: false });
    }

    // ── 时钟回拨 ──────────────────────────────────────────────────────────────

    /// 时钟被往回拨时**暂停且留痕**，绝不重放。
    ///
    /// NTP 回拨、虚拟机快照恢复都会造成。此时重新触发就是重复执行一条已经执行过的
    /// 命令——对「删除昨天的备份再重建」这类任务是真事故。
    #[test]
    fn clock_stepping_backwards_pauses_without_replaying() {
        let c = cron("* * * * *");
        let t = DAY + 100 * MIN;
        // 水位必须**就在 t**（已评估到那时）才能构成回拨场景：input() 的默认水位
        // 停在 DAY，那只是「很旧的窗口」，判定走的是补跑/留痕分支而非回拨分支。
        let inp = Input {
            now_secs: t - 10 * MIN, // 时钟被拨回 10 分钟（水位仍在 t）
            checked_minute: minute_index(t),
            ..input(t)
        };
        assert_eq!(
            decide(&c, inp),
            Decision::PausedForClockRollback,
            "回拨期间必须显式暂停（调用方据此留痕），不得重放"
        );
        // 暂停期间每跳都是同一个判定（不重扫、不触发）
        for extra in [0, MIN, 5 * MIN] {
            let p = Input {
                now_secs: t - 10 * MIN + extra,
                ..inp
            };
            assert_eq!(decide(&c, p), Decision::PausedForClockRollback);
        }
        // 时钟走回到水位之后才恢复触发
        let ahead = Input {
            now_secs: t + MIN,
            ..inp
        };
        assert_eq!(decide(&c, ahead), Decision::Fire { catchup: false });
    }

    /// 回拨期间窗口内的历史命中不会被「补跑」——Once 策略也不行。
    #[test]
    fn rollback_period_is_not_treated_as_missed_for_catchup() {
        let c = cron("*/10 * * * *");
        let t = DAY + 6 * 3600;
        // 水位在 t；时钟拨回到 t - 3 小时（其间有若干 */10 命中）
        let inp = Input {
            now_secs: t - 3 * 3600,
            checked_minute: minute_index(t),
            catchup: CatchUp::Once,
            ..input(t)
        };
        assert_eq!(
            decide(&c, inp),
            Decision::PausedForClockRollback,
            "回拨区间不得被当成「错过的触发」去补跑"
        );
    }

    // ── Skip 留痕 ─────────────────────────────────────────────────────────────

    /// Skip 策略：错过的触发**必须**以 SkipPolicy 带回（留痕的依据），而不是静默 Idle。
    #[test]
    fn skip_policy_reports_missed_instead_of_idling() {
        let c = cron("30 3 * * *");
        let now = DAY + 10 * 3600; // 10:00
        let inp = Input {
            now_secs: now,
            created_minute: minute_index(DAY - 86_400), // 昨天就建好
            checked_minute: minute_index(DAY - 86_400 + 3 * 3600 + 30 * MIN), // 昨天 03:30 评估过
            tz_offset_minutes: 0,
            catchup: CatchUp::Skip,
        };
        // 窗口内恰有一次命中：今天 03:30
        assert_eq!(
            decide(&c, inp),
            Decision::SkipPolicy {
                missed: 1,
                last_missed_minute: minute_index(DAY + 3 * 3600 + 30 * MIN),
                clamped: false,
            },
            "Skip 策略须带回「错过了什么」，静默 Idle 会让用户以为跑过了"
        );
    }

    /// 多次错过一次报齐：`missed` 是窗口内总数，`last_missed_minute` 是最近一场。
    #[test]
    fn skip_policy_counts_all_misses_and_reports_the_latest() {
        let c = cron("0 * * * *"); // 每小时整点
        let now = DAY + 10 * 3600 + 5 * MIN; // 10:05
        let inp = Input {
            now_secs: now,
            created_minute: minute_index(DAY), // 今天 00:00 创建
            checked_minute: minute_index(DAY), // 且已评估到那时
            tz_offset_minutes: 0,
            catchup: CatchUp::Skip,
        };
        // 00:00 已评估（水位含它）；窗口 (00:00, 10:05) 内的整点：01..10 共 10 个
        let d = decide(&c, inp);
        match d {
            Decision::SkipPolicy {
                missed,
                last_missed_minute,
                clamped,
            } => {
                assert_eq!(missed, 10, "实得 missed={missed}");
                assert_eq!(
                    last_missed_minute,
                    minute_index(DAY + 10 * 3600),
                    "最近一场是 10:00"
                );
                assert!(!clamped);
            }
            other => panic!("应 SkipPolicy，实得 {other:?}"),
        }
    }

    /// 留痕只落一次：水位推过之后，同一批错过不再重复报。
    ///
    /// 这是评估水位与上次触发**必须分开**的直接理由：合用一个量的话，Skip 策略
    /// 不能推「上次触发」（展示会撒谎），于是每 2 秒一跳都重扫同一段、重复落痕。
    #[test]
    fn skip_trace_is_reported_once_then_not_again() {
        let c = cron("30 3 * * *");
        let now = DAY + 10 * 3600;
        let mut inp = Input {
            now_secs: now,
            created_minute: minute_index(DAY - 86_400),
            checked_minute: minute_index(DAY - 86_400),
            tz_offset_minutes: 0,
            catchup: CatchUp::Skip,
        };
        assert!(matches!(decide(&c, inp), Decision::SkipPolicy { .. }));
        // 调度器留痕后推水位
        inp.checked_minute = minute_index(now);
        // 之后每跳都 Idle——直到下一个真实触发点
        for extra in [2, 5, 600] {
            let p = Input {
                now_secs: now + extra,
                ..inp
            };
            assert_eq!(decide(&c, p), Decision::Idle, "留痕不得重复（+{extra}s）");
        }
        // 次日 03:30 照常正常触发
        let next = Input {
            now_secs: DAY + 86_400 + 3 * 3600 + 30 * MIN,
            ..inp
        };
        assert_eq!(decide(&c, next), Decision::Fire { catchup: false });
    }

    /// 更早的错过被 7 天回看窗口甩掉时，`clamped` 置真——留痕文案要提到。
    #[test]
    fn misses_beyond_the_lookback_are_flagged_not_hidden() {
        let c = cron("0 3 * * *"); // 每天 03:00
                                   // 任务在 30 天前创建并评估到创建时刻；关机 30 天后回来
        let now = DAY + 10 * 3600;
        let inp = Input {
            now_secs: now,
            created_minute: minute_index(now) - 30 * 24 * 60,
            checked_minute: minute_index(now) - 30 * 24 * 60,
            tz_offset_minutes: 0,
            catchup: CatchUp::Skip,
        };
        match decide(&c, inp) {
            Decision::SkipPolicy {
                missed, clamped, ..
            } => {
                assert!(clamped, "窗口外还有更早的错过，必须标记出来");
                assert_eq!(missed, 7, "窗口内应有 7 场（7 天 × 每天 1 场）");
            }
            other => panic!("实得 {other:?}"),
        }
        // 停机不足 7 天（窗口装得下）则不标
        let short = Input {
            created_minute: minute_index(now) - 5 * 24 * 60,
            checked_minute: minute_index(now) - 5 * 24 * 60,
            ..inp
        };
        match decide(&c, short) {
            Decision::SkipPolicy {
                missed, clamped, ..
            } => {
                assert!(!clamped);
                assert_eq!(missed, 5);
            }
            other => panic!("实得 {other:?}"),
        }
    }

    // ── 补跑（Once）──────────────────────────────────────────────────────────

    /// 策略 Once：错过一次 → 补跑一次，且标记为补跑。
    #[test]
    fn once_policy_catches_up_and_marks_it() {
        let c = cron("30 3 * * *");
        let now = DAY + 10 * 3600; // 10:00
        let inp = Input {
            now_secs: now,
            created_minute: minute_index(DAY - 86_400),
            checked_minute: minute_index(DAY - 86_400 + 3 * 3600 + 30 * MIN), // 昨天 03:30 评估过
            tz_offset_minutes: 0,
            catchup: CatchUp::Once,
        };
        assert_eq!(
            decide(&c, inp),
            Decision::Fire { catchup: true },
            "错过了今天 03:30，应补跑一次"
        );
    }

    /// 策略 Once：**错过多次也只补跑一次**（补完推水位，下一 tick 即 Idle）。
    ///
    /// 「全部补跑」会让关机三天后的每分钟任务瞬间排出几千次执行。
    #[test]
    fn once_policy_fires_only_once_even_after_many_misses() {
        let c = cron("*/10 * * * *");
        let now = DAY + 6 * 3600 + 5 * MIN; // 06:05（本分钟不命中）
        let mut inp = Input {
            now_secs: now,
            created_minute: minute_index(DAY - 86_400),
            checked_minute: minute_index(DAY - 86_400),
            tz_offset_minutes: 0,
            catchup: CatchUp::Once,
        };
        assert_eq!(decide(&c, inp), Decision::Fire { catchup: true }, "应补跑");
        // 调度器补跑后把水位推到当前分钟
        inp.checked_minute = minute_index(now);
        assert_eq!(
            decide(&c, inp),
            Decision::Idle,
            "补跑一次之后不得继续补——否则关机三天会排出几千次执行"
        );
    }

    /// 新建的任务**不补跑它诞生之前**的触发。
    ///
    /// 这是 `created_minute` 存在的唯一理由：早上 10:00 新建一条 `0 3 * * *` 且策略
    /// 为 Once 的任务，没有这道左界就会在下一个 tick 立刻「补跑」今天凌晨 3 点那次。
    #[test]
    fn a_brand_new_task_does_not_catch_up_its_prehistory() {
        let c = cron("0 3 * * *");
        let now = DAY + 10 * 3600;
        let inp = Input {
            now_secs: now,
            created_minute: minute_index(now), // 此刻创建
            checked_minute: minute_index(now), // 创建即评估到当前
            tz_offset_minutes: 0,
            catchup: CatchUp::Once,
        };
        assert_eq!(
            decide(&c, inp),
            Decision::Idle,
            "03:00 发生在任务创建之前，不该补跑"
        );
        // 契约的另一半：**即使调用方给了低于 created 的水位**（正常仓储不会，
        // 但判定函数的输入是自由构造的），也绝不看 created 之前——水位被拉低
        // 只应缩小到回看上限，不得越过任务的诞生时刻。
        let corrupt = Input {
            checked_minute: 0, // 异常低的水位（等价于缺省/坏行）
            ..inp
        };
        assert_eq!(
            decide(&c, corrupt),
            Decision::Idle,
            "水位异常低时窗口左界仍须被 created 挡住，否则新任务补跑史前"
        );
        // 而次日 03:00 照常正常触发（不是补跑）
        let next = Input {
            now_secs: DAY + 86_400 + 3 * 3600,
            ..inp
        };
        assert_eq!(decide(&c, next), Decision::Fire { catchup: false });
    }

    /// 周期长于回看窗口的任务（每月 1 号）在窗口内没有命中 → 不补，如实 Idle。
    #[test]
    fn tasks_longer_than_the_window_are_not_caught_up() {
        // 2026-08-21 往前 7 天是 08-14，其间没有 1 号
        let monthly = cron("0 3 1 * *");
        let now = DAY + 10 * 3600;
        let inp = Input {
            now_secs: now,
            created_minute: minute_index(now) - 30 * 24 * 60,
            checked_minute: minute_index(now) - 30 * 24 * 60,
            tz_offset_minutes: 0,
            catchup: CatchUp::Once,
        };
        assert_eq!(
            decide(&monthly, inp),
            Decision::Idle,
            "窗口内无命中即不补——已知代价，UI 须说明"
        );
    }

    /// 补跑判定不看**未来**：本分钟之后的命中不算错过。
    #[test]
    fn catchup_does_not_look_into_the_future() {
        let c = cron("0 23 * * *"); // 每天 23:00
        let now = DAY + 10 * 3600; // 10:00，今天的 23:00 还没到
        let inp = Input {
            now_secs: now,
            created_minute: minute_index(now) - 60,
            checked_minute: minute_index(now) - 60,
            tz_offset_minutes: 0,
            catchup: CatchUp::Once,
        };
        assert_eq!(
            decide(&c, inp),
            Decision::Idle,
            "23:00 还没到，不该当成错过"
        );
    }

    /// 本分钟就命中时走**正常触发**而不是补跑（catchup 标记必须准）。
    #[test]
    fn a_hit_on_the_current_minute_is_not_a_catchup() {
        let c = cron("*/10 * * * *");
        let now = DAY + 6 * 3600 + 10 * MIN; // 06:10，本分钟命中
        let inp = Input {
            now_secs: now,
            created_minute: minute_index(DAY - 86_400),
            checked_minute: minute_index(DAY - 86_400), // 也错过了很多次
            tz_offset_minutes: 0,
            catchup: CatchUp::Once,
        };
        assert_eq!(
            decide(&c, inp),
            Decision::Fire { catchup: false },
            "本分钟命中就是正常触发，标成补跑会误导用户"
        );
    }

    // ── 时区 ──────────────────────────────────────────────────────────────────

    /// 判定按**任务自己的偏移**算，不按 UTC。
    #[test]
    fn decision_uses_the_task_offset() {
        let c = cron("30 3 * * *");
        // 东八区的 03:30 = UTC 前一天 19:30。水位必须早于该时刻——input() 的默认
        // 水位是 DAY（在其后），否则判定是「时钟回拨」而非触发。
        let utc_1930_prev_day = DAY - 86_400 + 19 * 3600 + 30 * MIN;
        let cst = Input {
            now_secs: utc_1930_prev_day,
            tz_offset_minutes: 8 * 60,
            created_minute: minute_index(DAY - 2 * 86_400),
            checked_minute: minute_index(DAY - 2 * 86_400),
            catchup: CatchUp::Skip,
        };
        assert_eq!(
            decide(&c, cst),
            Decision::Fire { catchup: false },
            "东八区任务应在 UTC 19:30 触发"
        );
        // 同一时刻，UTC 任务不该**触发**。它的窗口里有 UTC 侧的历史命中
        // （UTC 03:30 每天），所以结果是留痕而非 Idle——那也正确，本条只关心
        // 「同一瞬间只有带对偏移的任务才 Fire」。
        let utc = Input {
            tz_offset_minutes: 0,
            ..cst
        };
        assert!(
            !matches!(decide(&c, utc), Decision::Fire { .. }),
            "UTC 任务在 UTC 19:30 不该触发"
        );
    }

    /// 负偏移（西半球）同样正确。
    #[test]
    fn negative_offset_decides_correctly() {
        let c = cron("0 0 * * *"); // 每天午夜
                                   // 西五区的午夜 = UTC 05:00
        let utc_0500 = DAY + 5 * 3600;
        let est = Input {
            now_secs: utc_0500,
            tz_offset_minutes: -5 * 60,
            ..input(utc_0500)
        };
        assert_eq!(decide(&c, est), Decision::Fire { catchup: false });
    }

    /// DST 春季前跳：不存在的本地小时不会被观察到（扫描的是真实 unix 瞬间）。
    ///
    /// 用 +1h 的「跳变偏移」模拟：偏移在某时刻从 +0 变为 +60。本地 02:30 若不存在，
    /// 换算出来的 unix 瞬间落在跳变的另一侧，cron `30 2 * * *` 那天不会命中任何分钟。
    #[test]
    fn dst_spring_forward_invisible_hour_produces_no_fire() {
        // 场景：某时区 2026-03-08 春季前跳，本地 02:00-03:00 不存在。
        // 构造：用两段偏移扫一整天，任何一分钟换算到「跳变后」的本地分量都到不了 02:xx。
        let c = cron("30 2 * * *");
        // 2026-03-08 00:00 UTC 起 24 小时，偏移在 UTC 02:00 从 0 跳到 60
        let day = 1_772_928_000i64; // 2026-03-08 00:00:00 UTC
        let jump_at = day + 2 * 3600;
        let mut fired = 0;
        let mut m = day;
        while m < day + 86_400 {
            let off = if m < jump_at { 0 } else { 60 };
            let p = crate::civil::parts_at(m, off);
            if c.matches(Fields::from_parts(p)) {
                fired += 1;
            }
            m += 60;
        }
        // 本地 02:30 不存在 → 那天这场不触发；跳变前后的换算不会凭空造出它
        assert_eq!(
            fired, 0,
            "前跳区间里不存在的本地分钟绝不能被换算观察到（实得 {fired} 次）"
        );
    }

    /// DST 秋季回拨：同一本地时间出现两次，但它们是**两个不同的 unix 分钟**——
    /// 每个都会被触发（按各自的 unix 瞬间评估），这是可接受且可解释的行为。
    #[test]
    fn dst_fall_back_two_unix_instants_are_both_real() {
        let c = cron("30 1 * * *");
        // 2025-10-26（欧洲秋季回拨日）：UTC 01:00 偏移从 +120 回到 +60。
        // 时刻取值经独立换算核对（第一版手算差了两天，被自检断言当场抓出）：
        //   A = 2025-10-25T23:30Z，在 +120 下 = 本地 10-26 01:30
        //   B = 2025-10-26T00:30Z，在 +60  下 = 本地 10-26 01:30
        let instants: [(i64, i32); 2] = [(1_761_435_000, 120), (1_761_438_600, 60)];
        for &(t, off) in &instants {
            let p = crate::civil::parts_at(t, off);
            assert_eq!(
                (p.hour, p.minute),
                (1, 30),
                "自检：unix {t} 在偏移 {off} 下应是本地 01:30，实得 {p:?}"
            );
            assert!(
                c.matches(Fields::from_parts(p)),
                "unix {t} 这一场是真实存在的瞬间"
            );
        }
        assert_ne!(instants[0].0, instants[1].0, "它们是两个不同的 unix 分钟");
    }

    // ── 策略解析与护栏 ────────────────────────────────────────────────────────

    /// 策略字符串往返；未知值回落 Skip（保守侧）。
    #[test]
    fn catchup_parses_and_round_trips() {
        assert_eq!(CatchUp::parse("skip"), CatchUp::Skip);
        assert_eq!(CatchUp::parse("once"), CatchUp::Once);
        assert_eq!(CatchUp::parse("ONCE"), CatchUp::Once);
        assert_eq!(CatchUp::parse("  once  "), CatchUp::Once);
        // 未知值 → Skip（少补跑一次是安全侧；未来加了第三种策略、旧版本读到它也安全）
        for s in ["", "all", "每次", "0", "true"] {
            assert_eq!(CatchUp::parse(s), CatchUp::Skip, "{s:?} 应回落 Skip");
        }
        for p in [CatchUp::Skip, CatchUp::Once] {
            assert_eq!(CatchUp::parse(p.as_str()), p, "{p:?} 往返必须一致");
        }
    }

    /// 补跑扫描在最坏情况下也不会退化成长时间循环（性能护栏）。
    ///
    /// 它跑在每 2 秒一拍的 tick 里，必须远小于一个 tick 周期。
    #[test]
    fn worst_case_scan_is_far_shorter_than_one_tick() {
        // 永不命中的 cron + 满窗口回看 = 最坏情况：走满 MAX_CATCHUP_LOOKBACK_MINUTES 次
        let never = cron("0 0 30 2 *"); // 2 月 30 日
        let now = DAY + 10 * 3600;
        let inp = Input {
            now_secs: now,
            created_minute: minute_index(now) - 365 * 24 * 60,
            checked_minute: minute_index(now) - 365 * 24 * 60,
            tz_offset_minutes: 0,
            catchup: CatchUp::Once,
        };
        let t0 = std::time::Instant::now();
        assert_eq!(decide(&never, inp), Decision::Idle);
        let dt = t0.elapsed();
        assert!(
            dt < std::time::Duration::from_millis(500),
            "最坏情况扫描应在 500ms 内（实测 {dt:?}）——它跑在 2 秒一次的 tick 里"
        );
    }
}
