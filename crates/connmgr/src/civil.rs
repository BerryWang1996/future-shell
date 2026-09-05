//! 公历分解：unix 秒 (+ 时区偏移) → 年月日时分秒 + 星期。**不引时间库。**
//!
//! ## 为什么自己算
//!
//! 全仓需要的只是「把一个 unix 秒拆成人看的分量」这一件事：会话日志的命名模板
//! （`{yyyy}{MM}{dd}_{HH}{mm}{ss}`）与计划任务的 cron 判定。为这一件事引 chrono/time
//! 要背上一棵依赖树、一套时区数据库和它们的 DST 语义，而供应链卫生是本仓未决的
//! 审计项（审计2 #3/#4）。算法本身是确定的公历算术，可以逐条钉住。
//!
//! ## 唯一实现
//!
//! 这里是**唯一**一份公历算术。`app/src/state.rs` 的会话日志命名走这里
//! （它此前有一份自己的 `utc_parts`，两份同样的历法算术是典型的「两套机制做同一件
//! 事」，改一处漏一处）。cron 判定也走这里。
//!
//! 算法是 Howard Hinnant 的 days-from-civil 逆运算：以 0000-03-01 为纪元起点，
//! 把闰日挪到「年末」从而不必对 2 月特判；闰年/世纪闰年由 era 推导，不查表。
//!
//! ## 时区的边界
//!
//! 本模块只做「加一个固定偏移再分解」。它**不知道**夏令时，也不该知道：偏移从哪来、
//! 什么时候变，是调用方的产品决策（当前决策见 `fs_connmgr::cron` 模块头与调度器）。
//! 把 DST 塞进一个纯算术函数里，等于把一个产品决策藏在没人会去读的地方。

/// 分解后的分量。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Parts {
    pub year: i64,
    /// 1..=12
    pub month: u32,
    /// 1..=31
    pub day: u32,
    /// 0..=23
    pub hour: u32,
    /// 0..=59
    pub minute: u32,
    /// 0..=59（不处理闰秒：unix 时间本身就不含闰秒）
    pub second: u32,
    /// 0=周日 .. 6=周六
    pub day_of_week: u32,
}

/// unix 秒 + 时区偏移（分钟，东为正）→ 分量。
///
/// 全程用欧几里得除法：`offset_minutes` 为负、或时刻早于 1970 时，普通的 `/` 与 `%`
/// 会朝零截断而给出错误的「负余数」，表现为 1970 年附近与负偏移下日期整体差一天。
pub fn parts_at(unix_secs: i64, offset_minutes: i32) -> Parts {
    let local = unix_secs + (offset_minutes as i64) * 60;
    let days = local.div_euclid(86_400);
    let rem = local.rem_euclid(86_400);
    let hour = (rem / 3600) as u32;
    let minute = ((rem % 3600) / 60) as u32;
    let second = (rem % 60) as u32;

    // 1970-01-01 是**星期四**。0=周日 ⇒ 那天是 4。
    // 再取一次 rem_euclid：days 为负时 `+4` 仍可能是负数。
    let day_of_week = (days + 4).rem_euclid(7) as u32;

    // days-from-civil 逆运算
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097); // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]（3 月 1 日为 0）
    let mp = (5 * doy + 2) / 153; // [0, 11]（3 月为 0）
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    let year = if month <= 2 { y + 1 } else { y };

    Parts {
        year,
        month,
        day,
        hour,
        minute,
        second,
        day_of_week,
    }
}

/// 取该时刻所在**分钟**的起点（秒数抹零）。
///
/// 调度以分钟为粒度（cron 最细就是分钟）。用「分钟序号」而不是原始秒做去重键，
/// 才能保证同一分钟内多次 tick 只触发一次——用秒做键的话每次 tick 都是新键。
pub fn minute_index(unix_secs: i64) -> i64 {
    unix_secs.div_euclid(60)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(secs: i64) -> Parts {
        parts_at(secs, 0)
    }

    /// 纪元本身：1970-01-01 00:00:00 UTC，星期四。
    #[test]
    fn epoch_is_a_thursday() {
        let e = p(0);
        assert_eq!((e.year, e.month, e.day), (1970, 1, 1));
        assert_eq!((e.hour, e.minute, e.second), (0, 0, 0));
        assert_eq!(e.day_of_week, 4, "1970-01-01 是星期四（0=周日）");
    }

    /// 一条「已知时刻 → 已知分量」的样例。
    ///
    /// 用具名结构而不是八元组：八个位置的元组读起来根本分不清哪个是月哪个是分，
    /// 而这张表的全部价值就在于人能核对它。
    struct Case {
        secs: i64,
        note: &'static str,
        want: Parts,
    }

    /// 建一条样例。逐字段具名——这张表的全部价值在于人能核对它。
    macro_rules! case {
        ($secs:expr, $note:literal, $y:expr, $mo:expr, $d:expr, $h:expr, $mi:expr, $s:expr, $dow:expr) => {
            Case {
                secs: $secs,
                note: $note,
                want: Parts {
                    year: $y,
                    month: $mo,
                    day: $d,
                    hour: $h,
                    minute: $mi,
                    second: $s,
                    day_of_week: $dow,
                },
            }
        };
    }

    /// 一串已知日期逐一核对（含星期），覆盖闰年、世纪闰年、月末、年末、溢出点。
    #[test]
    fn known_timestamps_decode_exactly() {
        let cases = [
            case!(
                951_827_696,
                "2000-02-29 世纪闰年（查表实现最容易在这里错）",
                2000,
                2,
                29,
                12,
                34,
                56,
                2
            ),
            case!(
                4_107_456_000,
                "2100-02-28（2100 不是闰年，没有 2 月 29 日）",
                2100,
                2,
                28,
                0,
                0,
                0,
                0
            ),
            case!(
                946_684_799,
                "1999-12-31 23:59:59 年末最后一秒",
                1999,
                12,
                31,
                23,
                59,
                59,
                5
            ),
            case!(
                946_684_800,
                "2000-01-01 00:00:00 跨年那一刻",
                2000,
                1,
                1,
                0,
                0,
                0,
                6
            ),
            case!(
                1_709_164_800,
                "2024-02-29 普通闰年的闰日",
                2024,
                2,
                29,
                0,
                0,
                0,
                4
            ),
            case!(1_709_251_200, "2024-03-01 闰日次日", 2024, 3, 1, 0, 0, 0, 5),
            case!(
                1_677_628_800,
                "2023-03-01 非闰年 2 月只有 28 天",
                2023,
                3,
                1,
                0,
                0,
                0,
                3
            ),
            case!(
                1_787_302_991,
                "2026-08-21 09:03:11 本项目落地当天",
                2026,
                8,
                21,
                9,
                3,
                11,
                5
            ),
            case!(
                2_147_483_647,
                "2038-01-19 32 位 time_t 溢出点，i64 下必须照常",
                2038,
                1,
                19,
                3,
                14,
                7,
                2
            ),
            case!(
                4_294_967_295,
                "2106-02-07 u32 秒溢出点之后",
                2106,
                2,
                7,
                6,
                28,
                15,
                0
            ),
        ];
        for case in cases {
            assert_eq!(
                p(case.secs),
                case.want,
                "unix {} 分解错误（{}）",
                case.secs,
                case.note
            );
        }
    }

    /// 星期必须**逐日递增并回绕**，而不是只在某几个点碰巧对。
    #[test]
    fn day_of_week_advances_and_wraps() {
        // 从 1970-01-01（周四=4）起连续 30 天
        for i in 0..30i64 {
            let got = p(i * 86_400).day_of_week;
            assert_eq!(got, ((4 + i) % 7) as u32, "第 {i} 天的星期错了");
        }
    }

    /// 时区偏移在分解**之前**加上；东八区跨日的那一刻要对。
    #[test]
    fn offset_is_applied_before_decomposition() {
        // 2026-08-21 16:30:00 UTC → 东八区 2026-08-22 00:30:00（跨了一天，星期也 +1）
        let utc = parts_at(1_787_329_800, 0);
        assert_eq!((utc.year, utc.month, utc.day, utc.hour), (2026, 8, 21, 16));
        let cst = parts_at(1_787_329_800, 8 * 60);
        assert_eq!(
            (cst.year, cst.month, cst.day, cst.hour, cst.minute),
            (2026, 8, 22, 0, 30),
            "东八区应跨到次日，实得 {cst:?}"
        );
        assert_eq!(
            cst.day_of_week,
            (utc.day_of_week + 1) % 7,
            "跨日后星期须 +1"
        );
    }

    /// **负偏移**必须正确（西半球）。截断除法在这里会整体差一天。
    #[test]
    fn negative_offset_does_not_drift_by_a_day() {
        // 2026-08-21 03:00:00 UTC → 西五区 2026-08-20 22:00:00（退到前一天）
        let t = 1_787_281_200;
        let utc = parts_at(t, 0);
        assert_eq!((utc.month, utc.day, utc.hour), (8, 21, 3));
        let est = parts_at(t, -5 * 60);
        assert_eq!(
            (est.year, est.month, est.day, est.hour),
            (2026, 8, 20, 22),
            "西五区应退到前一天，实得 {est:?}"
        );
        assert_eq!(
            est.day_of_week,
            (utc.day_of_week + 6) % 7,
            "退一天后星期须 -1"
        );
    }

    /// 纪元附近 + 负偏移：局部时间落到 1969 年，仍须正确（不能是 1970-01-00 之类）。
    #[test]
    fn pre_epoch_local_time_is_correct() {
        // 1970-01-01 00:00:00 UTC，西五区 → 1969-12-31 19:00:00 周三
        let e = parts_at(0, -5 * 60);
        assert_eq!(
            (e.year, e.month, e.day, e.hour, e.minute, e.second),
            (1969, 12, 31, 19, 0, 0),
            "实得 {e:?}"
        );
        assert_eq!(e.day_of_week, 3, "1969-12-31 是星期三");
        // 再往前一整年也要对
        let y = parts_at(0, -366 * 24 * 60);
        assert_eq!((y.year, y.month, y.day), (1968, 12, 31), "实得 {y:?}");
    }

    /// 半小时/三刻钟偏移（印度 +5:30、尼泊尔 +5:45）不能被整点假设吃掉。
    #[test]
    fn half_hour_offsets_work() {
        // 2026-08-21 09:00:00 UTC → +5:30 → 14:30
        let ist = parts_at(1_787_302_800, 5 * 60 + 30);
        assert_eq!((ist.hour, ist.minute), (14, 30), "实得 {ist:?}");
        // +5:45 → 14:45
        let npt = parts_at(1_787_302_800, 5 * 60 + 45);
        assert_eq!((npt.hour, npt.minute), (14, 45), "实得 {npt:?}");
    }

    /// 每个月的天数与月末翻页都对（逐月走一遍 2024 与 2023）。
    #[test]
    fn month_lengths_are_right_in_leap_and_common_years() {
        let expect_2024 = [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
        let expect_2023 = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
        for (year, expect) in [(2024, expect_2024), (2023, expect_2023)] {
            for (i, len) in expect.iter().enumerate() {
                let month = i as u32 + 1;
                // 找该月 1 号 00:00 的 unix 秒：从 1970 累加天数
                let start = days_from_civil(year, month, 1) * 86_400;
                let last = parts_at(start + (*len as i64 - 1) * 86_400, 0);
                assert_eq!(
                    (last.year, last.month, last.day),
                    (year, month, *len),
                    "{year}-{month} 的第 {len} 天应仍在本月"
                );
                let next = parts_at(start + *len as i64 * 86_400, 0);
                assert_ne!(
                    next.month,
                    month,
                    "{year}-{month} 的第 {} 天应已翻月",
                    len + 1
                );
            }
        }
    }

    /// 测试自用的正向换算（days-from-civil），与被测的逆运算互为反函数。
    ///
    /// 刻意**不**复用被测代码来造输入：那样只能证明它自洽。这里独立实现正向换算，
    /// 再要求两者严格互逆——一个方向写错就对不上。
    fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
        let y = if m <= 2 { y - 1 } else { y };
        let era = y.div_euclid(400);
        let yoe = y - era * 400; // [0, 399]
        let mp = if m > 2 { m - 3 } else { m + 9 } as i64; // 3 月为 0
        let doy = (153 * mp + 2) / 5 + d as i64 - 1; // [0, 365]
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
        era * 146_097 + doe - 719_468
    }

    /// 正逆互逆：随机抽一大批日期往返，一个都不许错。
    #[test]
    fn round_trips_with_independent_forward_conversion() {
        // 覆盖 1900..2200 的每月 1/15/28 号，跨 era 边界（1900/2000/2100/2200）
        for year in 1900..=2200i64 {
            for month in 1..=12u32 {
                for day in [1u32, 15, 28] {
                    let days = days_from_civil(year, month, day);
                    let got = parts_at(days * 86_400, 0);
                    assert_eq!(
                        (got.year, got.month, got.day),
                        (year, month, day),
                        "往返失败：{year}-{month}-{day} → days {days} → {got:?}"
                    );
                }
            }
        }
    }

    /// 分钟序号：同一分钟内的任意秒得到同一个键，跨分钟必然变。
    ///
    /// 用秒做去重键的话每次 tick 都是新键，「同一分钟只触发一次」就失效了。
    #[test]
    fn minute_index_is_stable_within_a_minute() {
        let base = 1_787_562_180; // 恰好某分钟的 0 秒
        assert_eq!(parts_at(base, 0).second, 0, "自检：base 应是整分钟");
        let k = minute_index(base);
        for s in 0..60 {
            assert_eq!(minute_index(base + s), k, "第 {s} 秒应仍是同一分钟");
        }
        assert_eq!(minute_index(base + 60), k + 1);
        assert_eq!(minute_index(base - 1), k - 1);
        // 负数时刻也要单调（截断除法在这里会把 -1 与 0 归成同一分钟）
        assert_eq!(minute_index(-1), -1, "-1 秒属于纪元前那一分钟");
        assert_eq!(minute_index(-60), -1);
        assert_eq!(minute_index(-61), -2);
    }
}
