/**
 * 视图窗口的 URL 参数（M4b「标签拖出/平铺」）。
 *
 * Rust 侧 `window_new` 把 `?view=<sessionId>&title=<标题>` 拼进新窗口的 URL；
 * 这里是读回来的那一半。
 *
 * # 为什么单独一个模块
 *
 * 判据有**两个**消费者：`main.ts` 用它决定挂 App 还是 ViewWindow，
 * `ViewWindow.svelte` 用它取出具体的会话与标题。两边各写一份 `URLSearchParams`
 * 的话，"有没有 view 参数" 与 "view 参数是什么" 会各自演化——
 * 而它们走散的表现是：窗口挂成了视图模式，视图里却没有会话。
 */

export interface ViewParams {
  /** 要显示的会话；`null` 表示这是个空窗口（菜单「新建窗口」开出来的）。 */
  sessionId: string | null;
  /** 窗口标题。没给就是空串——调用方据此决定动不动 document.title。 */
  title: string;
}

/**
 * 这个 URL 是不是一个视图窗口。
 *
 * 判据是**有没有 `view` 参数**，不是它有没有值：`?view=` （空值）同样是视图窗口，
 * 只是它没有会话。若按「有值」判，那个 URL 会挂成主界面——而它是由 `window_new`
 * 开出来的窗口，主界面在那里会是完整的第二份应用（第二个侧栏、第二套模态）。
 */
export function isViewWindow(search: string): boolean {
  return new URLSearchParams(search).has("view");
}

/** 解析出会话与标题。 */
export function parseViewParams(search: string): ViewParams {
  const p = new URLSearchParams(search);
  const raw = p.get("view");
  return {
    // 空串按「没有会话」处理：一个 sessionId 为空串的终端连不上任何东西，
    // 而它看起来会像一个正常的、只是没有输出的终端。
    sessionId: raw && raw.trim() !== "" ? raw : null,
    // 标题限长与后端一致（120 字）。后端已经截过，这里再截一次是因为
    // URL 是可以被手工改的——开发时直接在地址栏里试是常事。
    title: (p.get("title") ?? "").slice(0, 120),
  };
}
