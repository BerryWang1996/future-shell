//! 窗口排列的几何计算（M4b「标签拖出/平铺」出口）。
//!
//! 纯函数、不碰 Tauri：排列窗口这件事里唯一会算错的部分就是几何，
//! 而几何一旦要通过一个真实窗口才能验证，就基本等于没有测试。
//! `window_cmd` 负责把这里算出的矩形套到真实窗口上。
//!
//! # 「水平平铺」到底是哪个方向
//!
//! 这两个词在各家产品里是**反的**。Windows 任务栏右键写的是「堆叠显示窗口」
//! （上下）与「并排显示窗口」（左右），而 Xshell/SecureCRT 用的是「水平平铺 /
//! 垂直平铺」——后者按谁的定义都能讲通：
//!
//! - 一种读法：「水平平铺」＝ 沿水平方向排开 ⇒ **左右并排**
//! - 另一种读法：「水平平铺」＝ 用水平的线去切 ⇒ **上下堆叠**
//!
//! 本模块取第一种（`TileHorizontal` ＝ 左右并排），但**不指望用户知道**：
//! 菜单文案里直接写「水平平铺（左右并排）」。术语有歧义时，正确的做法是在界面上
//! 消歧，而不是挑一个定义然后指望所有人都同意。
//!
//! ```text
//!   TileHorizontal (左右并排)        TileVertical (上下堆叠)
//!   ┌─────┬─────┬─────┐             ┌─────────────────┐
//!   │     │     │     │             │                 │
//!   │  1  │  2  │  3  │             ├─────────────────┤
//!   │     │     │     │             │        2        │
//!   └─────┴─────┴─────┘             └─────────────────┘
//! ```

/// 一个矩形，单位是物理像素（与 Tauri 的 `PhysicalPosition`/`PhysicalSize` 同一坐标系）。
///
/// `x`/`y` 用 `i32` 而不是 `u32`：多显示器布局里副屏可能在主屏**左边或上边**，
/// 那时工作区原点是负数。用无符号会让那种布局悄悄地把所有窗口挤到主屏上。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    pub fn new(x: i32, y: i32, w: u32, h: u32) -> Self {
        Self { x, y, w, h }
    }
}

/// 排列方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ArrangeMode {
    /// 层叠：标题栏错开叠放，每个窗口都露出一角可以点。
    Cascade,
    /// 水平平铺 = 左右并排（见模块头的图）。
    TileHorizontal,
    /// 垂直平铺 = 上下堆叠。
    TileVertical,
}

/// **平铺出来的视图窗口**的最小尺寸。
///
/// 刻意小于主窗口的 800×600（`tauri.conf.json` 的 `minWidth`/`minHeight`）：
/// 那个数是为主窗口定的——它要装下会话管理器、工具栏、状态栏。而平铺出来的窗口
/// 只有一个终端，没有那些东西。
///
/// 这不是「顺手放宽一点」，是**用 800×600 的话垂直平铺在 1080p 上直接不可用**：
/// 1040 高分两份是 520，已经低于 600，于是两个窗口就报「摆不下」并相互重叠。
/// 一个在最常见的分辨率上连两个窗口都排不开的平铺功能，等于没有这个功能。
///
/// 480×320 的下界来自终端本身：13px 字体下 80×24 的标准终端约 640×350，
/// 再小就要开始横向滚动了；480×320 是「还能看清一屏输出」与「排得开」之间的折中——
/// 1920 宽排得下 4 个，1040 高排得下 3 个。
///
/// 视图窗口创建时会把这两个数设成它自己的 min size，否则窗口管理器仍按 800×600 夹，
/// **实际布局与算出来的不一样**——平铺完窗口重叠，而代码里所有断言都还是绿的。
/// `window_cmd` 有一条测试钉住这个传递。
pub const MIN_W: u32 = 480;
pub const MIN_H: u32 = 320;

/// 下限本身要够一个终端用。**编译期**断言：把它调到 400×300 以下直接编译失败。
///
/// 写成运行期测试的话两边都是常量，断言恒真——clippy 会正确地判它
/// 「不能失败」，而一个不能失败的门禁比没有门禁更坏：它看起来像有人在把关。
const _: () = assert!(
    MIN_W >= 400 && MIN_H >= 300,
    "视图窗口下限小到终端没法用了（80×24 在 13px 字体下约 640×350）"
);

/// 层叠时每一级的偏移量（像素）。约等于一个标题栏的高度——
/// 再小就看不出层次，再大则第四五个窗口就跑出工作区了。
const CASCADE_STEP: i32 = 32;

/// 层叠窗口占工作区的比例。留边是为了让「层叠」看起来确实是一摞，
/// 而不是几个几乎铺满、只差一点点的窗口。
const CASCADE_RATIO: f64 = 0.75;

/// 一次排列的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Arrangement {
    /// 每个窗口的目标矩形，顺序与传入的窗口顺序一致。
    pub rects: Vec<Rect>,
    /// 有几个窗口被压到了最小尺寸以下、只能相互重叠。
    ///
    /// 这个数不是给日志看的，是给**界面**看的：平铺 8 个窗口在 1080p 上必然摆不开，
    /// 而用户看到的会是「点了平铺，窗口还是叠着的」。有这个数才能说一句
    /// 「屏幕放不下 8 个，已尽量排开」——沉默地摆成一堆是最坏的那种结果。
    pub overflowed: usize,
}

/// 算出 `count` 个窗口在 `work` 里的排列。
///
/// `count == 0` 返回空；`count == 1` 时平铺就是铺满整个工作区（层叠仍然层叠——
/// 只有一个窗口时「层叠」等于「放到左上角、留点边」，那正是它该做的）。
pub fn arrange(work: Rect, count: usize, mode: ArrangeMode) -> Arrangement {
    if count == 0 {
        return Arrangement {
            rects: vec![],
            overflowed: 0,
        };
    }
    match mode {
        ArrangeMode::Cascade => cascade(work, count),
        ArrangeMode::TileHorizontal => tile(work, count, true),
        ArrangeMode::TileVertical => tile(work, count, false),
    }
}

fn cascade(work: Rect, count: usize) -> Arrangement {
    let w = ((work.w as f64 * CASCADE_RATIO) as u32).max(MIN_W.min(work.w));
    let h = ((work.h as f64 * CASCADE_RATIO) as u32).max(MIN_H.min(work.h));
    // 一摞最多能叠多少级才不会掉出工作区。至少 1——工作区比窗口还小时不能除出 0，
    // 那会让下面的取模除零。
    let steps_x = (((work.w.saturating_sub(w)) as i32) / CASCADE_STEP).max(1);
    let steps_y = (((work.h.saturating_sub(h)) as i32) / CASCADE_STEP).max(1);
    let steps = steps_x.min(steps_y);

    let rects = (0..count)
        .map(|i| {
            // 叠满一摞就从头再来，而不是继续往外跑。回绕后的窗口与第一批完全重合，
            // 但那好过把窗口摆到屏幕外面——用户找不回来的窗口比重叠的窗口麻烦得多。
            let level = (i as i32) % steps;
            Rect::new(
                work.x + level * CASCADE_STEP,
                work.y + level * CASCADE_STEP,
                w,
                h,
            )
        })
        .collect();
    Arrangement {
        rects,
        // 层叠本来就是重叠的，没有「摆不下」这回事。
        overflowed: 0,
    }
}

/// 一维等分，余数**分散**给前几份而不是全给最后一份。
///
/// 全给最后一份的写法（`last = total - (n-1) * each`）在 1000÷3 时得到 333/333/334，
/// 看不出问题；但 1000÷7 会得到六个 142 和一个 148——最后那个明显宽一截，
/// 而用户会以为自己碰到了 bug。
fn split(total: u32, n: usize) -> Vec<u32> {
    let each = total / n as u32;
    let rem = (total % n as u32) as usize;
    (0..n)
        .map(|i| if i < rem { each + 1 } else { each })
        .collect()
}

fn tile(work: Rect, count: usize, horizontal: bool) -> Arrangement {
    let along = if horizontal { work.w } else { work.h };
    let min = if horizontal { MIN_W } else { MIN_H };
    let parts = split(along, count);

    // 摆不下的判据：**按等分算出来的**份额小于最小尺寸。
    // 用等分份额而非最终份额来判，是因为最终份额下面会被夹到 min，
    // 拿夹过的值去比 min 永远得不出「摆不下」。
    let overflowed = parts.iter().filter(|&&p| p < min).count();

    let mut rects = Vec::with_capacity(count);
    let mut cursor = if horizontal { work.x } else { work.y };
    for p in &parts {
        let size = (*p).max(min);
        rects.push(if horizontal {
            Rect::new(cursor, work.y, size, work.h.max(MIN_H))
        } else {
            Rect::new(work.x, cursor, work.w.max(MIN_W), size)
        });
        // 游标按**等分份额**推进，不按夹过的尺寸。按夹过的推进会让后面的窗口
        // 一个比一个更靠外，最后几个整个跑出工作区——而那正是「找不回来的窗口」。
        cursor += *p as i32;
    }
    Arrangement { rects, overflowed }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一块常见的 1080p 工作区（去掉任务栏）。
    const WORK: Rect = Rect {
        x: 0,
        y: 0,
        w: 1920,
        h: 1040,
    };

    #[test]
    fn no_windows_no_rects() {
        for mode in [
            ArrangeMode::Cascade,
            ArrangeMode::TileHorizontal,
            ArrangeMode::TileVertical,
        ] {
            assert_eq!(arrange(WORK, 0, mode).rects, vec![]);
        }
    }

    #[test]
    fn a_single_window_tiles_to_the_whole_work_area() {
        for mode in [ArrangeMode::TileHorizontal, ArrangeMode::TileVertical] {
            let a = arrange(WORK, 1, mode);
            assert_eq!(a.rects, vec![WORK], "{mode:?}");
            assert_eq!(a.overflowed, 0);
        }
    }

    #[test]
    fn horizontal_is_side_by_side_and_vertical_is_stacked() {
        // 这两个词各家产品是反的（见模块头）。这条把本仓的定义钉死，
        // 免得哪天有人「顺手改回来」而没人发现——那会让菜单里两项互换而毫无提示。
        let h = arrange(WORK, 2, ArrangeMode::TileHorizontal).rects;
        assert_eq!(h[0].y, h[1].y, "左右并排时两个窗口的 y 应相同");
        assert!(h[1].x > h[0].x, "第二个应在右边");
        assert_eq!(h[0].h, WORK.h, "左右并排时各占满高度");

        let v = arrange(WORK, 2, ArrangeMode::TileVertical).rects;
        assert_eq!(v[0].x, v[1].x, "上下堆叠时两个窗口的 x 应相同");
        assert!(v[1].y > v[0].y, "第二个应在下边");
        assert_eq!(v[0].w, WORK.w, "上下堆叠时各占满宽度");
    }

    #[test]
    fn tiles_exactly_cover_the_work_area_without_gaps_or_overlap() {
        // 平铺的定义就是这个：不重叠、不留缝、正好铺满。
        // 少一个像素的缝在 1920 宽上看不出来，但那说明分配逻辑有 off-by-one，
        // 而同一个 off-by-one 在窗口多的时候会攒成一条明显的黑边。
        //
        // 只在**排得开**（overflowed == 0）时成立：摆不下时尺寸会被抬到 MIN，
        // 那时窗口必然重叠，"精确覆盖"本就不可能。摆不下的行为由
        // too_many_windows_are_reported_rather_than_silently_piled_up 单独钉。
        // 上界 3：1040 高分 4 份是 260，已低于 MIN_H(320)。这不是测试将就实现——
        // 1080p 上垂直平铺最多就是排得开 3 个，第 4 个必然重叠，而那由下面那条钉。
        for n in 2..=3usize {
            let ah = arrange(WORK, n, ArrangeMode::TileHorizontal);
            let av = arrange(WORK, n, ArrangeMode::TileVertical);
            assert_eq!(ah.overflowed, 0, "n={n} 水平向本该排得开（1920/{n}）");
            assert_eq!(av.overflowed, 0, "n={n} 垂直向本该排得开（1040/{n}）");
            let h = ah.rects;
            assert_eq!(h.len(), n);
            assert_eq!(h[0].x, WORK.x, "n={n} 第一个应贴左边");
            for i in 1..n {
                assert_eq!(
                    h[i].x,
                    h[i - 1].x + h[i - 1].w as i32,
                    "n={n} 第 {i} 个与前一个之间有缝或重叠"
                );
            }
            let last = h[n - 1];
            assert_eq!(
                last.x + last.w as i32,
                WORK.x + WORK.w as i32,
                "n={n} 最后一个应贴右边"
            );
            assert_eq!(h.iter().map(|r| r.w).sum::<u32>(), WORK.w, "n={n} 宽度总和");

            let v = av.rects;
            for i in 1..n {
                assert_eq!(
                    v[i].y,
                    v[i - 1].y + v[i - 1].h as i32,
                    "n={n} 垂直向第 {i} 个"
                );
            }
            assert_eq!(v.iter().map(|r| r.h).sum::<u32>(), WORK.h, "n={n} 高度总和");
        }
    }

    #[test]
    fn the_remainder_is_spread_not_dumped_on_the_last_one() {
        // 全给最后一份的写法在 1000÷3 时得到 333/333/334，看不出问题；
        // 但 1000÷7 会得到六个 142 和一个 148——最后那个明显宽一截，
        // 用户会以为碰到了 bug。
        //
        // 直接测 `split` 而不是走 `arrange`：走 arrange 的话 142 会被 MIN_W(480)
        // 夹上去，七份全变成 480，余数分配这件事就被盖住了——那样的用例看着绿，
        // 实际什么也没测。
        let parts = split(1000, 7);
        assert_eq!(parts.iter().sum::<u32>(), 1000, "总和必须守恒");
        let (min, max) = (parts.iter().min().unwrap(), parts.iter().max().unwrap());
        assert!(max - min <= 1, "宽度差 {} 像素：{parts:?}", max - min);
        // 多出来的那 6 个像素给了**前** 6 份（靠左的略宽，视觉上比末尾突然变宽自然）
        assert_eq!(parts, vec![143, 143, 143, 143, 143, 143, 142]);

        // 整除时人人相等
        assert_eq!(split(900, 3), vec![300, 300, 300]);
        // 一份就是全部
        assert_eq!(split(1000, 1), vec![1000]);
    }

    #[test]
    fn a_wide_enough_work_area_really_does_spread_the_remainder() {
        // 端到端对照上一条：工作区够宽（每份都超过 MIN_W）时，
        // arrange 出来的宽度差同样不超过 1 像素。
        let work = Rect::new(0, 0, 7000, 1040);
        let ws: Vec<u32> = arrange(work, 7, ArrangeMode::TileHorizontal)
            .rects
            .iter()
            .map(|r| r.w)
            .collect();
        assert_eq!(ws.iter().sum::<u32>(), 7000);
        assert!(
            ws.iter().max().unwrap() - ws.iter().min().unwrap() <= 1,
            "{ws:?}"
        );
    }

    #[test]
    fn a_negative_origin_work_area_is_respected() {
        // 副屏在主屏左边时工作区原点是负的。用 u32 存 x 的实现会把所有窗口
        // 悄悄挤回主屏，而用户在副屏上点了「平铺」却什么也没看见。
        let work = Rect::new(-1920, -200, 1920, 1040);
        let h = arrange(work, 2, ArrangeMode::TileHorizontal).rects;
        assert_eq!(h[0].x, -1920);
        assert_eq!(h[0].y, -200);
        assert_eq!(h[1].x, -1920 + 960);
    }

    #[test]
    fn too_many_windows_are_reported_rather_than_silently_piled_up() {
        // 1920 宽放 8 个窗口，每个 240 < MIN_W(800)。摆不下是事实，
        // 沉默地摆成一堆才是问题——用户看到的会是「点了平铺，窗口还是叠着的」。
        let a = arrange(WORK, 8, ArrangeMode::TileHorizontal);
        assert_eq!(a.overflowed, 8, "八个都摆不下，应当八个都报");
        // 尺寸被抬到最小值（不然窗口管理器也会夹，结果与算出来的不一致）
        assert!(a.rects.iter().all(|r| r.w >= MIN_W));
        // 但游标仍按等分推进：最后一个的**左上角**不许跑出工作区，
        // 否则那是一个用户找不回来的窗口。
        let last = a.rects.last().unwrap();
        assert!(
            last.x < WORK.x + WORK.w as i32,
            "最后一个窗口的左上角跑出工作区了：{last:?}"
        );
    }

    #[test]
    fn a_comfortable_count_reports_no_overflow() {
        // 反向对照：上一条若因 overflowed 恒等于 count 而通过，这一条会红。
        for n in 1..=2usize {
            assert_eq!(
                arrange(WORK, n, ArrangeMode::TileHorizontal).overflowed,
                0,
                "n={n}"
            );
        }
    }

    #[test]
    fn cascade_steps_each_window_and_keeps_them_all_reachable() {
        let a = arrange(WORK, 4, ArrangeMode::Cascade);
        assert_eq!(a.rects.len(), 4);
        // 每一级都错开，否则「层叠」等于「全部重合」，下面的窗口一个都点不到
        for i in 1..4 {
            assert_eq!(a.rects[i].x, a.rects[i - 1].x + CASCADE_STEP);
            assert_eq!(a.rects[i].y, a.rects[i - 1].y + CASCADE_STEP);
        }
        // 每个都完整落在工作区内——层叠的意义就是每个都还点得到
        for r in &a.rects {
            assert!(r.x >= WORK.x && r.y >= WORK.y, "{r:?}");
            assert!(r.x + r.w as i32 <= WORK.x + WORK.w as i32, "{r:?} 右边出界");
            assert!(r.y + r.h as i32 <= WORK.y + WORK.h as i32, "{r:?} 下边出界");
        }
    }

    #[test]
    fn cascade_wraps_instead_of_marching_off_screen() {
        // 叠满一摞就从头再来。回绕后的窗口与第一批重合，但那好过把窗口摆到屏幕外面——
        // 用户找不回来的窗口比重叠的窗口麻烦得多。
        let a = arrange(WORK, 40, ArrangeMode::Cascade);
        for r in &a.rects {
            assert!(
                r.x + r.w as i32 <= WORK.x + WORK.w as i32
                    && r.y + r.h as i32 <= WORK.y + WORK.h as i32,
                "第 40 个窗口跑出去了：{r:?}"
            );
        }
        // 确实发生了回绕（否则这条测的是「40 个都还在里面」，那说明步长小到没意义）
        assert!(
            a.rects.iter().any(|r| *r == a.rects[0]) && a.rects.len() > 1,
            "没有回绕"
        );
    }

    #[test]
    fn a_work_area_smaller_than_the_minimum_window_does_not_panic() {
        // 小屏/缩放很高的机器上工作区可能比 MIN_W×MIN_H 还小。
        // 这里唯一的要求是**不崩**——除零、减法溢出、取模零都在这条路径上。
        let tiny = Rect::new(0, 0, 640, 480);
        for mode in [
            ArrangeMode::Cascade,
            ArrangeMode::TileHorizontal,
            ArrangeMode::TileVertical,
        ] {
            for n in [1usize, 2, 5] {
                let a = arrange(tiny, n, mode);
                assert_eq!(a.rects.len(), n, "{mode:?} n={n}");
            }
        }
    }

    #[test]
    fn the_mode_serializes_to_the_names_the_frontend_uses() {
        // 前端菜单 id 是 window.cascade / window.tileH / window.tileV，
        // 载荷走这三个串。拼错的话命令会在运行期才失败，而不是编译期。
        for (m, s) in [
            (ArrangeMode::Cascade, "\"cascade\""),
            (ArrangeMode::TileHorizontal, "\"tile-horizontal\""),
            (ArrangeMode::TileVertical, "\"tile-vertical\""),
        ] {
            assert_eq!(serde_json::to_string(&m).unwrap(), s);
            assert_eq!(serde_json::from_str::<ArrangeMode>(s).unwrap(), m);
        }
    }
}
