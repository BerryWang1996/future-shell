//! 多窗口：新建、排列、标签拖出（M4b「标签拖出/平铺」出口）。
//!
//! # 「平铺为视图克隆，输入同步到同一会话」是免费的
//!
//! UI 规格 §66 的原话是「同一会话可在多个视图中并排查看（平铺为视图克隆，
//! 输入同步到同一会话）」。这句话在本架构下**不需要任何同步机制**：
//!
//! 会话活在后端（`AppState` 的 session map），终端输出经 `term:data:{session_id}`
//! 事件广播——Tauri 的 emit 是**全窗口**的，任何订阅了那个事件名的 webview 都收得到。
//! 输入走 `term_input(session_id, …)`，两个窗口发的字节进的是同一个 PTY。
//!
//! 所以「视图克隆」＝ 再开一个 webview、让它订阅同一个 session_id，如此而已。
//! 真正要写的只有：开窗口、给窗口传参数、排列窗口。
//!
//! 这也意味着**不存在「两个视图内容不一致」这种 bug**——它们看的本来就是同一份流。
//! 唯一会不一致的是各自的滚动位置与选区，而那正该各自独立。

use crate::window_layout::{arrange, ArrangeMode, Arrangement, Rect, MIN_H, MIN_W};
use serde::Serialize;
use tauri::{
    AppHandle, LogicalSize, Manager, PhysicalPosition, PhysicalSize, WebviewUrl,
    WebviewWindowBuilder,
};

/// 主窗口的 label。`tauri.conf.json` 里那个未命名窗口的默认 label 就是 `main`。
pub const MAIN_WINDOW: &str = "main";

/// 视图窗口 label 的前缀。
///
/// 用前缀而不是记一张表，是为了让「这是不是一个视图窗口」在**只拿得到 label**
/// 的地方也判得出来——排列窗口时拿到的就只有 label。
pub const VIEW_PREFIX: &str = "view-";

/// 一次排列的结果，回给前端。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ArrangeOutcome {
    /// 实际被摆放的窗口数。
    pub arranged: usize,
    /// 有几个因为屏幕放不下而只能重叠。
    ///
    /// 前端据此提示。沉默地摆成一堆是最坏的结果——用户看到的会是
    /// 「点了平铺，窗口还是叠着的」，而他无从知道是没生效还是放不下。
    pub overflowed: usize,
}

/// 取工作区（去掉任务栏/Dock 的可用区域）。
///
/// 用**主窗口所在的那块屏**，不是主显示器：用户把主窗口拖到副屏之后点平铺，
/// 窗口应该排在他正在看的那块屏上，而不是跳回主屏。
fn work_area(app: &AppHandle) -> Result<Rect, String> {
    let win = app.get_webview_window(MAIN_WINDOW).ok_or("主窗口不存在")?;
    let monitor = win
        .current_monitor()
        .map_err(|e| e.to_string())?
        // 窗口刚好在两块屏之间、或系统还没报告显示器时会是 None。
        // 退回主显示器而不是报错——用户点的是「平铺」，摆不到最理想的那块屏
        // 也好过弹一个他看不懂的错误。
        .or(win.primary_monitor().map_err(|e| e.to_string())?)
        .ok_or("拿不到任何显示器信息")?;
    let p = monitor.position();
    let s = monitor.size();
    Ok(Rect::new(p.x, p.y, s.width, s.height))
}

/// 当前所有窗口的 label，主窗口排在最前。
///
/// 顺序要**稳定**，否则连点两次「平铺」窗口会互相换位置——看起来像是没生效，
/// 或者更糟：像是随机的。`app.webview_windows()` 返回的是 HashMap，顺序不定，
/// 所以这里显式排序。
fn ordered_labels(app: &AppHandle) -> Vec<String> {
    let mut labels: Vec<String> = app.webview_windows().keys().cloned().collect();
    labels.sort_by(|a, b| match (a == MAIN_WINDOW, b == MAIN_WINDOW) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.cmp(b),
    });
    labels
}

/// 开一个视图窗口，显示指定会话。
///
/// `session_id` 为 `None` 时开一个空的新窗口（菜单「窗口 → 新建窗口」）。
///
/// # 为什么参数走 URL query 而不是 IPC
///
/// 新窗口的前端要在**第一帧**就知道自己该显示什么。走 IPC 的话，窗口会先渲染成
/// 主界面的样子、再被通知「其实你是个视图窗口」——用户看到的是一次闪烁，
/// 而在慢机器上那次闪烁里侧栏会完整地画出来又消失。
///
/// # `title`
///
/// 窗口标题（标签上那行字），由**拖出的那一侧**给。不新开一个命令去后端问：
/// 标题是前端的概念，后端的会话里根本没有这个字段——为一个窗口标题在核心状态里
/// 造一个「会话标题」，是把纯展示的东西塞进本不该有它的层。
#[tauri::command]
pub async fn window_new(
    app: AppHandle,
    session_id: Option<String>,
    title: Option<String>,
) -> Result<String, String> {
    // label 必须全局唯一且**可预测地不冲突**。用会话 id 拼是自然的选择：
    // 同一个会话再拖出一次应当聚焦已有窗口，而不是开第二个一模一样的。
    let label = match &session_id {
        Some(sid) => view_label(sid),
        None => format!("{VIEW_PREFIX}blank-{}", app.webview_windows().len()),
    };

    if let Some(existing) = app.get_webview_window(&label) {
        // 已经有这个会话的视图窗口了：聚焦它。再开一个完全一样的窗口是纯粹的困扰——
        // 两个窗口显示同一份流，用户分不清哪个是哪个。
        existing.set_focus().map_err(|e| e.to_string())?;
        return Ok(label);
    }

    let url = match &session_id {
        Some(sid) => {
            let mut u = format!("index.html?view={}", urlencoding(sid));
            // 标题限长：它进的是 URL 与窗口标题栏，一个几 KB 的「标题」两处都受不了。
            if let Some(t) = title.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
                let t: String = t.chars().take(120).collect();
                u.push_str(&format!("&title={}", urlencoding(&t)));
            }
            u
        }
        None => "index.html".to_string(),
    };
    let mut builder = WebviewWindowBuilder::new(&app, &label, WebviewUrl::App(url.into()))
        .title(match &session_id {
            Some(_) => "FutureShell — 视图",
            None => "FutureShell",
        })
        .inner_size(960.0, 640.0);

    // 视图窗口的最小尺寸比主窗口小：它只有一个终端，没有会话管理器/工具栏/状态栏。
    // 不设的话窗口管理器按 tauri.conf.json 的 800×600 夹，平铺算出来的矩形会被改掉——
    // 实际布局与算出来的不一样，而代码里的断言全是绿的（见 window_layout::MIN_W 的注释）。
    if session_id.is_some() {
        builder = builder.min_inner_size(MIN_W as f64, MIN_H as f64);
    }

    let win = builder.build().map_err(|e| e.to_string())?;
    // 新窗口默认继承主窗口的缩放，逻辑尺寸即可
    let _ = win.set_size(LogicalSize::new(960.0, 640.0));
    Ok(label)
}

/// 排列全部窗口。
#[tauri::command]
pub async fn window_arrange(app: AppHandle, mode: ArrangeMode) -> Result<ArrangeOutcome, String> {
    let labels = ordered_labels(&app);
    let work = work_area(&app)?;
    let Arrangement { rects, overflowed } = arrange(work, labels.len(), mode);

    for (label, r) in labels.iter().zip(rects.iter()) {
        let Some(win) = app.get_webview_window(label) else {
            continue; // 窗口在我们枚举之后被关掉了——跳过，不是错误
        };
        // 最小化的窗口先还原：给一个最小化的窗口设几何是无声的空操作，
        // 用户会看到「点了平铺，那个窗口还缩在任务栏里」。
        let _ = win.unminimize();
        // 最大化的窗口也要先取消：最大化状态下设尺寸同样不生效。
        let _ = win.unmaximize();
        win.set_position(PhysicalPosition::new(r.x, r.y))
            .map_err(|e| e.to_string())?;
        win.set_size(PhysicalSize::new(r.w, r.h))
            .map_err(|e| e.to_string())?;
    }
    Ok(ArrangeOutcome {
        arranged: labels.len().min(rects.len()),
        overflowed,
    })
}

/// 从会话 id 推出视图窗口的 label。
///
/// **只此一处**。前端一度自己拼过一份（`"view-" + sessionId.replace(...)`），
/// 那是两份必然走散的实现——而走散的表现是「这个窗口永远关不掉」：
/// 会话已经死了，窗口还在那儿、还能往里打字，用户会以为连接还在。
/// 现在 `window_close_view` 收的是 session_id，拼 label 的规则前端根本不需要知道。
fn view_label(session_id: &str) -> String {
    format!("{VIEW_PREFIX}{}", sanitize_label(session_id))
}

/// 关掉某个会话的视图窗口（如果有的话）。
///
/// 会话关了却留着窗口，等于留下一个显示**已死会话**的窗口：还能往里打字，
/// 但字节没有去处。用户会以为连接还在，直到发现什么都没回应——
/// 而那时他已经在一个不存在的会话里敲了一串命令。
///
/// 窗口不存在不是错误：大多数标签从没被拖出过，关标签时这里本来就该什么都不做。
#[tauri::command]
pub async fn window_close_view(app: AppHandle, session_id: String) -> Result<(), String> {
    let label = view_label(&session_id);
    // 兜底：拼出来的 label 撞上主窗口时宁可什么都不做。目前撞不上（前缀不同，
    // 有测试钉），但这道闸的代价是一次字符串比较，而它挡的是「关掉主窗口」。
    if label == MAIN_WINDOW {
        return Ok(());
    }
    if let Some(w) = app.get_webview_window(&label) {
        w.close().map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// 把任意字符串收成一个能当窗口 label 用的串。
///
/// Tauri 的 label 只允许字母数字与 `-`/`/`/`:`/`_`，其余字符会让 `build()` 直接失败。
/// 会话 id 目前是 UUID（本来就合规），但它不是一个由本模块保证的事实——
/// 哪天 id 变成含别的字符的形式，没有这一步就是一个运行期才炸的窗口创建。
fn sanitize_label(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// URL query 值的最小转义。
///
/// 只处理会真正出问题的几个字符，不引 `urlencoding` 依赖——这里的输入是会话 id，
/// 一个 UUID；加一整个 crate 来编码 36 个十六进制字符是不划算的。
/// 前端用 `URLSearchParams` 解析，它认标准百分号编码。
fn urlencoding(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' | '~' => c.to_string(),
            _ => format!("%{:02X}", c as u32),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_are_sanitized_into_something_tauri_accepts() {
        // Tauri 的 label 只允许字母数字与 -/:_，别的字符让 build() 直接失败——
        // 那是一个只在运行期、只在特定 id 下才炸的窗口创建。
        assert_eq!(
            sanitize_label("0d5a2f11-8c3e-4a6b-9f01-2b7c8d9e0f11"),
            "0d5a2f11-8c3e-4a6b-9f01-2b7c8d9e0f11"
        );
        assert_eq!(sanitize_label("a b/c"), "a-b-c");
        assert_eq!(sanitize_label("会话"), "--");
        // 结果里只剩合法字符
        for s in ["会话 1", "a\u{0}b", "x?y=z"] {
            assert!(
                sanitize_label(s)
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-'),
                "{s} 收出来还有非法字符：{}",
                sanitize_label(s)
            );
        }
    }

    #[test]
    fn url_encoding_covers_what_would_break_the_query() {
        // UUID 原样穿过（这是实际会走的那条路）
        let uuid = "0d5a2f11-8c3e-4a6b-9f01-2b7c8d9e0f11";
        assert_eq!(urlencoding(uuid), uuid);
        // 会拆坏 query 的字符必须被编码，否则 `?view=a&b=c` 会多解析出一个参数
        assert_eq!(urlencoding("a&b"), "a%26b");
        assert_eq!(urlencoding("a=b"), "a%3Db");
        assert_eq!(urlencoding("a b"), "a%20b");
        assert_eq!(urlencoding("a#b"), "a%23b");
        // 编码结果里不含任何会拆坏 query 的字符
        for s in ["a&b=c#d e", "../../x"] {
            assert!(
                !urlencoding(s).contains(['&', '=', '#', ' ', '?']),
                "{s} → {}",
                urlencoding(s)
            );
        }
    }

    #[test]
    fn the_view_prefix_does_not_collide_with_the_main_window() {
        // `window_close_view` 靠 label != MAIN_WINDOW 挡住关主窗口。
        // 若前缀恰好能拼出 "main"，那道闸就有个洞。
        assert!(!MAIN_WINDOW.starts_with(VIEW_PREFIX));
        assert_ne!(format!("{VIEW_PREFIX}x"), MAIN_WINDOW);
    }

    /// 视图窗口的最小尺寸必须**小于**主窗口在 tauri.conf.json 里的最小尺寸。
    ///
    /// 反了的话平铺算出来的矩形会被窗口管理器夹掉，实际布局与算出来的不一致，
    /// 而 `window_layout` 那一整组测试仍然全绿——它测的是算术，不是窗口。
    #[test]
    fn view_windows_may_be_smaller_than_the_main_window() {
        const CONF: &str = include_str!("../../tauri.conf.json");
        let conf: serde_json::Value =
            serde_json::from_str(CONF).expect("tauri.conf.json 不是合法 JSON");
        let w0 = &conf["app"]["windows"][0];
        let min_w = w0["minWidth"].as_u64().expect("主窗口没有 minWidth") as u32;
        let min_h = w0["minHeight"].as_u64().expect("主窗口没有 minHeight") as u32;
        assert!(
            MIN_W < min_w && MIN_H < min_h,
            "视图窗口下限 {MIN_W}×{MIN_H} 不小于主窗口的 {min_w}×{min_h}，平铺会被夹"
        );
        // 「下限本身要够一个终端用」是**编译期**断言，见 window_layout 里的 const _。
        // 写在这里会是一个恒真的运行期断言（两边都是常量），clippy 正确地把它判成
        // 「不能失败的门禁」——而不能失败的门禁比没有门禁更坏，它看起来像有人在把关。
    }
}
