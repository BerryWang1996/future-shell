//! 五段 cron 表达式匹配（M4a：计划任务的时间判据）。
//!
//! ## 为什么自己写而不是引 `cron` crate
//!
//! 需要的只是「这一分钟该不该跑」，即五个字段各自的集合判定——大约一百行、可以
//! 逐条钉住。而供应链卫生是本仓未决的审计项（审计2 #3/#4），为一个百行的判定引一棵
//! 依赖树、还要跟着它的时区/DST 语义走，代价与收益不成比例。
//!
//! ## 支持与不支持（不假装支持）
//!
//! 支持：`*`、`N`、`a-b`、`*/n`、`a-b/n`、逗号列表，五个字段
//! （分 时 日 月 周）。周里 `0` 与 `7` 都是周日。
//!
//! **不支持**：`@daily` 之类的宏、`L`/`W`/`#` 等 Quartz 扩展、秒字段、年字段。
//! 不支持的写法在解析时**报错**而不是忽略——静默忽略一个字段会让任务在用户完全
//! 没料到的时间点跑起来，那比「表达式不合法」难查得多。
//!
//! ## 时间基准
//!
//! 判定吃的是**本地时间的各个分量**（[`Fields`]），换算由调用方负责。本模块不引
//! 时间库、不做时区推断：夏令时切换那一小时该跑几次是产品决策，不该藏在一个
//! 匹配函数里（当前决策见 `app` 层调度器：按本地分钟推进，重复的分钟只跑一次）。

use crate::Error;

/// 一个字段的取值集合。
#[derive(Debug, Clone, PartialEq, Eq)]
enum Field {
    /// `*`：任意值
    Any,
    /// 显式集合（已展开，去重升序）
    Set(Vec<u32>),
}

impl Field {
    fn matches(&self, v: u32) -> bool {
        match self {
            Self::Any => true,
            Self::Set(s) => s.binary_search(&v).is_ok(),
        }
    }
}

/// 解析好的 cron 表达式。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cron {
    minute: Field,
    hour: Field,
    day_of_month: Field,
    month: Field,
    day_of_week: Field,
}

/// 一个时刻的本地时间分量。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fields {
    pub minute: u32,
    pub hour: u32,
    /// 1..=31
    pub day_of_month: u32,
    /// 1..=12
    pub month: u32,
    /// 0=周日 .. 6=周六
    pub day_of_week: u32,
}

impl Cron {
    /// 解析五段表达式。字段之间可用任意数量的空白分隔。
    pub fn parse(expr: &str) -> Result<Self, Error> {
        let parts: Vec<&str> = expr.split_whitespace().collect();
        if parts.len() != 5 {
            return Err(Error::Validation(format!(
                "cron 表达式需要 5 个字段（分 时 日 月 周），实得 {} 个：{expr:?}",
                parts.len()
            )));
        }
        Ok(Self {
            minute: parse_field(parts[0], 0, 59, "分")?,
            hour: parse_field(parts[1], 0, 23, "时")?,
            day_of_month: parse_field(parts[2], 1, 31, "日")?,
            month: parse_field(parts[3], 1, 12, "月")?,
            day_of_week: parse_dow(parts[4])?,
        })
    }

    /// 该时刻是否命中。
    ///
    /// 「日」与「周」都不是 `*` 时按**或**匹配（Vixie cron 的历史行为，`crontab(5)`
    /// 有明文）：`0 0 13 * 5` 的含义是「每月 13 号**或**每周五」，不是「13 号且
    /// 恰好是周五」。这条极易写反，写反的表现是任务在用户预期的日子里不跑。
    pub fn matches(&self, f: Fields) -> bool {
        if !self.minute.matches(f.minute) || !self.hour.matches(f.hour) {
            return false;
        }
        if !self.month.matches(f.month) {
            return false;
        }
        let dom_restricted = self.day_of_month != Field::Any;
        let dow_restricted = self.day_of_week != Field::Any;
        match (dom_restricted, dow_restricted) {
            (false, false) => true,
            (true, false) => self.day_of_month.matches(f.day_of_month),
            (false, true) => self.day_of_week.matches(f.day_of_week),
            // 两者都受限 → 或
            (true, true) => {
                self.day_of_month.matches(f.day_of_month) || self.day_of_week.matches(f.day_of_week)
            }
        }
    }
}

/// 向前找触发时刻时最多往后看多久（分钟）。
///
/// 必须有上限：有些合法表达式**永远不会触发**（`0 0 30 2 *` —— 2 月 30 日不存在，
/// `0 0 31 4 *` —— 4 月只有 30 天）。没有上限就是一个死循环，而它会挂在用户点
/// 「保存任务」的那一刻。取 366 天：任何真实周期的任务都会在一年内出现至少一次，
/// 一年内不出现的表达式，与「永不触发」在产品上是同一件事，应当如实告诉用户。
const MAX_LOOKAHEAD_MINUTES: i64 = 366 * 24 * 60;

impl Cron {
    /// 从 `from_secs`（不含）起，往后找最多 `n` 个触发时刻，返回 unix 秒（整分）。
    ///
    /// 用途是让用户在保存任务**之前**核对「我写的这个 cron 到底什么时候跑」——
    /// 一个人肉解读 `*/15 9-17 * * 1-5` 的对错率远低于看三个具体时间。
    ///
    /// 找不满 `n` 个就返回已找到的（可能是空 `Vec`）：**空结果是有信息的**，它意味着
    /// 「这个表达式在一年内不会触发」，UI 必须把这件事说出来而不是显示一个空列表。
    pub fn next_fires(&self, from_secs: i64, offset_minutes: i32, n: usize) -> Vec<i64> {
        let mut out = Vec::with_capacity(n);
        if n == 0 {
            return out;
        }
        // 从下一个整分开始扫：同一分钟内已经触发过的不该再算一次
        let mut m = crate::civil::minute_index(from_secs) + 1;
        let end = m + MAX_LOOKAHEAD_MINUTES;
        while m < end && out.len() < n {
            let secs = m * 60;
            if self.matches(Fields::from_parts(crate::civil::parts_at(
                secs,
                offset_minutes,
            ))) {
                out.push(secs);
            }
            m += 1;
        }
        out
    }
}

impl Fields {
    /// 从公历分量取 cron 需要的那五个。
    ///
    /// 单独一个转换函数而不是让 `civil::Parts` 直接实现 cron 的接口：cron 不关心
    /// 年与秒，而 `Parts` 不该知道 cron 存在。
    pub fn from_parts(p: crate::civil::Parts) -> Self {
        Self {
            minute: p.minute,
            hour: p.hour,
            day_of_month: p.day,
            month: p.month,
            day_of_week: p.day_of_week,
        }
    }
}

fn parse_field(spec: &str, min: u32, max: u32, name: &str) -> Result<Field, Error> {
    let spec = spec.trim();
    if spec.is_empty() {
        return Err(Error::Validation(format!("cron 的「{name}」字段为空")));
    }
    if spec == "*" {
        return Ok(Field::Any);
    }
    let mut set: Vec<u32> = Vec::new();
    for item in spec.split(',') {
        let item = item.trim();
        if item.is_empty() {
            return Err(Error::Validation(format!(
                "cron 的「{name}」字段里有空项（多余的逗号）：{spec:?}"
            )));
        }
        // `范围/步长` 或 `*/步长`
        let (range_part, step) = match item.split_once('/') {
            None => (item, 1u32),
            Some((r, s)) => {
                let step: u32 = s.parse().map_err(|_| {
                    Error::Validation(format!("cron 的「{name}」字段步长不是数字：{item:?}"))
                })?;
                if step == 0 {
                    return Err(Error::Validation(format!(
                        "cron 的「{name}」字段步长不能为 0：{item:?}"
                    )));
                }
                (r, step)
            }
        };
        let (lo, hi) = if range_part == "*" {
            (min, max)
        } else if let Some((a, b)) = range_part.split_once('-') {
            (num(a, min, max, name)?, num(b, min, max, name)?)
        } else {
            let v = num(range_part, min, max, name)?;
            (v, v)
        };
        if lo > hi {
            return Err(Error::Validation(format!(
                "cron 的「{name}」字段范围颠倒（{lo}-{hi}）：{item:?}"
            )));
        }
        let mut v = lo;
        while v <= hi {
            set.push(v);
            v += step;
        }
    }
    set.sort_unstable();
    set.dedup();
    Ok(Field::Set(set))
}

/// 「周」字段：`0` 与 `7` 都是周日，统一折到 0。
fn parse_dow(spec: &str) -> Result<Field, Error> {
    let f = parse_field(spec, 0, 7, "周")?;
    Ok(match f {
        Field::Any => Field::Any,
        Field::Set(mut s) => {
            for v in s.iter_mut() {
                if *v == 7 {
                    *v = 0;
                }
            }
            s.sort_unstable();
            s.dedup();
            Field::Set(s)
        }
    })
}

fn num(s: &str, min: u32, max: u32, name: &str) -> Result<u32, Error> {
    let v: u32 = s
        .trim()
        .parse()
        .map_err(|_| Error::Validation(format!("cron 的「{name}」字段不是数字：{s:?}")))?;
    if v < min || v > max {
        return Err(Error::Validation(format!(
            "cron 的「{name}」字段越界：{v}（允许 {min}..={max}）"
        )));
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(minute: u32, hour: u32, dom: u32, month: u32, dow: u32) -> Fields {
        Fields {
            minute,
            hour,
            day_of_month: dom,
            month,
            day_of_week: dow,
        }
    }

    #[test]
    fn every_minute() {
        let c = Cron::parse("* * * * *").unwrap();
        assert!(c.matches(f(0, 0, 1, 1, 0)));
        assert!(c.matches(f(59, 23, 31, 12, 6)));
    }

    #[test]
    fn fixed_time() {
        let c = Cron::parse("30 3 * * *").unwrap();
        assert!(c.matches(f(30, 3, 15, 6, 2)));
        assert!(!c.matches(f(31, 3, 15, 6, 2)));
        assert!(!c.matches(f(30, 4, 15, 6, 2)));
    }

    #[test]
    fn step_and_range_and_list() {
        let c = Cron::parse("*/15 9-17 * * 1-5").unwrap();
        for m in [0, 15, 30, 45] {
            assert!(c.matches(f(m, 9, 1, 1, 1)), "分 {m} 应命中");
        }
        assert!(!c.matches(f(7, 9, 1, 1, 1)));
        assert!(!c.matches(f(0, 8, 1, 1, 1)), "8 点不在 9-17");
        assert!(!c.matches(f(0, 9, 1, 1, 6)), "周六不在 1-5");

        let c2 = Cron::parse("0,30 * * * *").unwrap();
        assert!(c2.matches(f(0, 5, 1, 1, 1)));
        assert!(c2.matches(f(30, 5, 1, 1, 1)));
        assert!(!c2.matches(f(15, 5, 1, 1, 1)));

        // 带步长的范围
        let c3 = Cron::parse("0 0-12/6 * * *").unwrap();
        for h in [0, 6, 12] {
            assert!(c3.matches(f(0, h, 1, 1, 1)), "时 {h} 应命中");
        }
        assert!(!c3.matches(f(0, 7, 1, 1, 1)));
    }

    /// 周日既是 0 也是 7。
    #[test]
    fn sunday_is_both_zero_and_seven() {
        let a = Cron::parse("0 0 * * 0").unwrap();
        let b = Cron::parse("0 0 * * 7").unwrap();
        assert!(a.matches(f(0, 0, 1, 1, 0)));
        assert!(b.matches(f(0, 0, 1, 1, 0)), "7 也必须是周日");
        assert!(!b.matches(f(0, 0, 1, 1, 1)));
        // 两种写法应解析成同一个集合
        assert_eq!(a, b);
    }

    /// 「日」与「周」都受限时按**或**匹配（Vixie cron 的历史行为）。
    ///
    /// 写成「且」的表现是任务在用户预期的日子里不跑——而且要等到下一个
    /// 「13 号恰好是周五」才可能被发现。
    #[test]
    fn dom_and_dow_are_ored_when_both_restricted() {
        let c = Cron::parse("0 0 13 * 5").unwrap();
        // 13 号（不是周五）→ 命中
        assert!(c.matches(f(0, 0, 13, 6, 2)), "13 号应命中");
        // 周五（不是 13 号）→ 命中
        assert!(c.matches(f(0, 0, 20, 6, 5)), "周五应命中");
        // 既不是 13 号也不是周五 → 不命中
        assert!(!c.matches(f(0, 0, 20, 6, 2)));

        // 只有「日」受限时，周几无关
        let only_dom = Cron::parse("0 0 1 * *").unwrap();
        assert!(only_dom.matches(f(0, 0, 1, 1, 3)));
        assert!(!only_dom.matches(f(0, 0, 2, 1, 3)));
        // 只有「周」受限时，几号无关
        let only_dow = Cron::parse("0 0 * * 1").unwrap();
        assert!(only_dow.matches(f(0, 0, 17, 8, 1)));
        assert!(!only_dow.matches(f(0, 0, 17, 8, 2)));
    }

    #[test]
    fn month_field() {
        let c = Cron::parse("0 0 1 1,7 *").unwrap();
        assert!(c.matches(f(0, 0, 1, 1, 0)));
        assert!(c.matches(f(0, 0, 1, 7, 0)));
        assert!(!c.matches(f(0, 0, 1, 3, 0)));
    }

    /// 不合法表达式必须**报错**而不是被忽略。
    ///
    /// 静默忽略一个字段会让任务在用户完全没料到的时间点跑起来，比「表达式不合法」
    /// 难查得多。宏（`@daily`）与 Quartz 扩展（`L`/`W`/`#`）都在此列——不支持就明说。
    #[test]
    fn invalid_expressions_are_rejected() {
        for bad in [
            "",             // 空
            "* * * *",      // 4 段
            "* * * * * *",  // 6 段
            "60 * * * *",   // 分越界
            "* 24 * * *",   // 时越界
            "* * 0 * *",    // 日从 1 起
            "* * 32 * *",   // 日越界
            "* * * 0 *",    // 月从 1 起
            "* * * 13 *",   // 月越界
            "* * * * 8",    // 周越界
            "*/0 * * * *",  // 步长 0
            "5-1 * * * *",  // 范围颠倒
            "a * * * *",    // 非数字
            "*/x * * * *",  // 步长非数字
            "0,,5 * * * *", // 多余逗号
            "@daily",       // 宏：不支持就报错
            "0 0 L * *",    // Quartz 扩展
            "0 0 * * 5#2",  // Quartz 扩展
            "0 0 * * FRI",  // 英文缩写（不支持）
        ] {
            assert!(Cron::parse(bad).is_err(), "不合法表达式必须被拒绝：{bad:?}");
        }
    }

    /// 报错信息要指出**哪个字段**错了，不能只说「表达式不合法」。
    #[test]
    fn errors_name_the_offending_field() {
        let e = Cron::parse("60 * * * *").unwrap_err().to_string();
        assert!(e.contains('分'), "实得 {e}");
        let e = Cron::parse("* * * 13 *").unwrap_err().to_string();
        assert!(e.contains('月'), "实得 {e}");
        let e = Cron::parse("* * * *").unwrap_err().to_string();
        assert!(
            e.contains('5') && e.contains('4'),
            "须说明需要 5 段而实得 4 段，实得 {e}"
        );
    }

    /// 字段之间多余的空白不影响解析（用户从文档里粘贴常带对齐空格）。
    #[test]
    fn extra_whitespace_is_tolerated() {
        let c = Cron::parse("  0   3  *  *   * ").unwrap();
        assert!(c.matches(f(0, 3, 1, 1, 1)));
    }

    // ── next_fires ────────────────────────────────────────────────────────────

    /// 2026-08-21 09:03:11 UTC（见 civil.rs 的样例表，同一时刻）。
    const T_2026_08_21_090311: i64 = 1_787_302_991;

    /// 每天 03:30：连续三次触发相隔恰好一天，且都落在 03:30:00。
    #[test]
    fn next_fires_daily() {
        let c = Cron::parse("30 3 * * *").unwrap();
        let got = c.next_fires(T_2026_08_21_090311, 0, 3);
        assert_eq!(got.len(), 3);
        for s in &got {
            let p = crate::civil::parts_at(*s, 0);
            assert_eq!((p.hour, p.minute, p.second), (3, 30, 0), "实得 {p:?}");
        }
        assert_eq!(got[1] - got[0], 86_400, "相邻两次应相隔一天");
        assert_eq!(got[2] - got[1], 86_400);
        // 当天 09:03 已过 03:30，第一次应是**次日** 03:30
        let first = crate::civil::parts_at(got[0], 0);
        assert_eq!((first.month, first.day), (8, 22), "实得 {first:?}");
    }

    /// 从「恰好命中的那一分钟」出发时，不把当前这一分钟算成下一次。
    ///
    /// 否则用户在 03:30:05 保存任务，界面会显示「下次触发 03:30」——看起来像是马上要
    /// 再跑一次，而实际上这一分钟的触发刚刚发生过。
    #[test]
    fn next_fires_excludes_the_current_minute() {
        let c = Cron::parse("30 3 * * *").unwrap();
        // 构造一个恰好 03:30:05 的时刻
        let day = 1_787_270_400; // 2026-08-21 00:00:00 UTC
        let at = day + 3 * 3600 + 30 * 60 + 5;
        let got = c.next_fires(at, 0, 1);
        assert_eq!(got.len(), 1);
        assert_eq!(
            got[0],
            day + 86_400 + 3 * 3600 + 30 * 60,
            "应跳到次日 03:30"
        );
    }

    /// 时区偏移作用在触发时刻上：东八区的 03:30 是 UTC 的 19:30（前一天）。
    #[test]
    fn next_fires_honors_offset() {
        let c = Cron::parse("30 3 * * *").unwrap();
        let utc = c.next_fires(T_2026_08_21_090311, 0, 1)[0];
        let cst = c.next_fires(T_2026_08_21_090311, 8 * 60, 1)[0];
        // 同一条 cron 在不同偏移下的绝对时刻应相差 8 小时（东八区更早到）
        assert_eq!(
            utc - cst,
            8 * 3600,
            "东八区的 03:30 应比 UTC 的 03:30 早 8 小时"
        );
        // 且在各自的本地时间里都确实是 03:30
        assert_eq!(
            (
                crate::civil::parts_at(cst, 8 * 60).hour,
                crate::civil::parts_at(cst, 8 * 60).minute
            ),
            (3, 30)
        );
    }

    /// 每分钟触发：n 个结果就是连续的 n 分钟。
    #[test]
    fn next_fires_every_minute_is_contiguous() {
        let c = Cron::parse("* * * * *").unwrap();
        let got = c.next_fires(T_2026_08_21_090311, 0, 5);
        assert_eq!(got.len(), 5);
        for w in got.windows(2) {
            assert_eq!(w[1] - w[0], 60);
        }
    }

    /// **永不触发**的合法表达式必须返回空，而不是死循环。
    ///
    /// `0 0 30 2 *`（2 月 30 日）与 `0 0 31 4 *`（4 月 31 日）都是合法 cron，但那些
    /// 日期不存在。没有回看上限的实现会挂在用户点「保存」的那一刻。
    #[test]
    fn impossible_expressions_return_empty_without_hanging() {
        for expr in ["0 0 30 2 *", "0 0 31 4 *", "0 0 31 2 *"] {
            let c = Cron::parse(expr).unwrap();
            let got = c.next_fires(T_2026_08_21_090311, 0, 3);
            assert!(got.is_empty(), "{expr} 永不触发，应返回空，实得 {got:?}");
        }
    }

    /// 一年内只触发一次的表达式也能找到（回看上限是 366 天，不是 365）。
    ///
    /// 366 而不是 365 的理由就在这条：`0 0 29 2 *` 只在闰年 2 月 29 日触发，
    /// 而从某些起点出发，下一个闰日会落在第 365 天之后。
    #[test]
    fn yearly_and_leap_day_expressions_are_found() {
        // 每年 1 月 1 日
        let c = Cron::parse("0 0 1 1 *").unwrap();
        let got = c.next_fires(T_2026_08_21_090311, 0, 1);
        assert_eq!(got.len(), 1, "每年一次也该找到");
        let p = crate::civil::parts_at(got[0], 0);
        assert_eq!(
            (p.year, p.month, p.day, p.hour, p.minute),
            (2027, 1, 1, 0, 0),
            "实得 {p:?}"
        );

        // 闰日：2026-08-21 之后的下一个 2 月 29 日在 2028 年，超过 366 天 → 找不到，
        // 这是回看上限的**已知代价**，如实记下（UI 会显示「一年内不会触发」）。
        let leap = Cron::parse("0 0 29 2 *").unwrap();
        assert!(
            leap.next_fires(T_2026_08_21_090311, 0, 1).is_empty(),
            "2028 的闰日超出 366 天回看窗口——已知代价，UI 须如实说明"
        );
        // 但从 2028 年初出发就能找到（证明不是「闰日永远找不到」）
        let jan_2028 = 1_830_297_600; // 2028-01-01 00:00:00 UTC
        let found = leap.next_fires(jan_2028, 0, 1);
        assert_eq!(found.len(), 1, "从 2028 年初出发应找到当年的闰日");
        let fp = crate::civil::parts_at(found[0], 0);
        assert_eq!((fp.year, fp.month, fp.day), (2028, 2, 29), "实得 {fp:?}");
    }

    /// `n = 0` 返回空且不做任何扫描（调用方传 0 时不该白扫一年）。
    #[test]
    fn next_fires_with_zero_n_is_empty() {
        let c = Cron::parse("* * * * *").unwrap();
        assert!(c.next_fires(T_2026_08_21_090311, 0, 0).is_empty());
    }

    /// 「日与周都受限 → 或」在 next_fires 里同样成立（与 matches 同一套规则）。
    #[test]
    fn next_fires_uses_the_same_or_rule() {
        // 13 号或周五
        let c = Cron::parse("0 0 13 * 5").unwrap();
        let got = c.next_fires(T_2026_08_21_090311, 0, 4);
        assert_eq!(got.len(), 4);
        for s in &got {
            let p = crate::civil::parts_at(*s, 0);
            assert!(
                p.day == 13 || p.day_of_week == 5,
                "每一次触发都应满足「13 号或周五」，实得 {p:?}"
            );
        }
        // 2026-08-21 本身是周五；下一次应是 8/28（周五），而不是 9/13
        let first = crate::civil::parts_at(got[0], 0);
        assert_eq!((first.month, first.day), (8, 28), "实得 {first:?}");
    }

    /// 集合内部去重且有序（`matches` 用二分查找，无序会漏判）。
    #[test]
    fn duplicate_values_are_deduped_and_sorted() {
        let c = Cron::parse("5,5,1,3 * * * *").unwrap();
        for m in [1, 3, 5] {
            assert!(c.matches(f(m, 0, 1, 1, 1)), "分 {m} 应命中");
        }
        assert!(!c.matches(f(2, 0, 1, 1, 1)));
        // 重复的 `*/1` 与 `*` 等价但走 Set 分支：全 60 分钟都该命中
        let c2 = Cron::parse("*/1 * * * *").unwrap();
        for m in 0..60 {
            assert!(c2.matches(f(m, 0, 1, 1, 1)), "分 {m} 应命中");
        }
    }
}
