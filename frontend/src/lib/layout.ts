import { get, writable } from "svelte/store";
import { settingGet, settingSet } from "./ipc";

/** 布局状态（UI 规格 §1.1）：侧栏左停靠默认 240px 可拉伸；各面板显隐；监控抽屉折叠插槽。 */

export const SIDEBAR_MIN = 160;
export const SIDEBAR_MAX = 480;
export const SIDEBAR_DEFAULT = 240;

export const sidebarVisible = writable(true);
export const sidebarWidth = writable(SIDEBAR_DEFAULT);
export const toolbarVisible = writable(true);
export const composeVisible = writable(true);
export const statusVisible = writable(true);
/** 快速命令条（M4a，UI 规格 §2.2 ⚡）：默认收起——空库时一条常驻横条只占地方。 */
export const quickbarVisible = writable(false);
/** 监控抽屉（UI 规格 §1.3）：MVP 预留折叠插槽，默认折叠，无数据采集（M4a 实装，UI §1.3/§8）。 */
export const monitorOpen = writable(false);

const STORES = {
  sidebar: sidebarVisible,
  toolbar: toolbarVisible,
  compose: composeVisible,
  status: statusVisible,
  quickbar: quickbarVisible,
  monitor: monitorOpen,
} as const;

export function toggle(key: keyof typeof STORES): void {
  STORES[key].update((v) => !v);
}

/**
 * 侧栏宽度钳位 160–480。
 * S263：settingGet 对持久值只做 `JSON.parse(raw) as T`、无运行期类型校验，键被写脏（字符串/对象/undefined）
 * 时 Math.round 产出 NaN，而 NaN 过两道钳位仍是 NaN ⇒ `style:width="NaNpx"` 是非法声明、整条侧栏塌陷。
 * 故非有限值一律回落默认宽度，保证 store 恒为合法像素值（拖拽入口 e.clientX 恒有限，不受影响）。
 */
export function setSidebarWidth(px: number): void {
  if (!Number.isFinite(px)) {
    sidebarWidth.set(SIDEBAR_DEFAULT);
    return;
  }
  sidebarWidth.set(Math.min(SIDEBAR_MAX, Math.max(SIDEBAR_MIN, Math.round(px))));
}

/** 载入持久化宽度（settings 键 ui.sidebarWidth；命令未注册时回落默认）。 */
export async function initLayout(): Promise<void> {
  const width = await settingGet<number>("ui.sidebarWidth", SIDEBAR_DEFAULT);
  setSidebarWidth(width);
  // 停靠侧（M4b）。脏值回落左停靠——一个认不出的值不该让侧栏消失。
  const dock = await settingGet<string>(SIDEBAR_DOCK_KEY, "left");
  sidebarDock.set(isSidebarDock(dock) ? dock : "left");
}

export function persistSidebarWidth(): void {
  void settingSet("ui.sidebarWidth", get(sidebarWidth));
}

/* ── 侧栏停靠 / 浮动 / 窄屏覆盖层（M4b 出口第 16、18 项）───────────────────── */

/**
 * 侧栏的停靠方式（settings 键 `ui.sidebarDock`）。
 *
 * `float` 是**用户主动选的**浮动；窄屏下的覆盖层是**自动的**，两者呈现一样但来源不同——
 * 见 [`effectivePresentation`] 里为什么这个区别必须保留。
 */
export type SidebarDock = "left" | "right" | "float";

export const SIDEBAR_DOCK_KEY = "ui.sidebarDock";
export const sidebarDock = writable<SidebarDock>("left");

/**
 * 窄屏断点。
 *
 * 出口原文：「窗口收窄至 **≤1099px** 时侧栏转绝对定位覆盖层……**≥1100px** 恢复停靠」。
 * 所以媒体查询写成 `(min-width: 1100px)` ——「宽屏」是 ≥1100，其补集恰好是 ≤1099。
 *
 * 写成 `max-width: 1099px` 也能得到同一组数，但在小数 DPI 缩放下 1099.5px 会两边都不满足
 * （CSS 的 min/max-width 都是闭区间，而 1099.5 既不 ≤1099 也不 ≥1100）。
 * 用「宽屏」做正判据、窄屏取补集，就没有这个缝。
 */
export const WIDE_SCREEN_QUERY = "(min-width: 1100px)";

/** 当前是不是窄屏。由 [`initResponsive`] 接上 matchMedia 后驱动。 */
export const narrowScreen = writable(false);

/** 覆盖层当前是否展开（只在覆盖形态下有意义）。 */
export const sidebarOverlayOpen = writable(false);

/** 侧栏最终以什么形态呈现。 */
export type SidebarPresentation = "left" | "right" | "overlay";

/**
 * 仲裁：停靠偏好 + 屏宽 → 实际呈现。
 *
 * **窄屏一律覆盖，无论用户选的是哪一侧。** 1100px 以下留给侧栏 240px 之后，
 * 终端只剩不到 860px——那时候「停靠」的意思是「终端太窄没法用」。
 *
 * 而 `dock` 本身**不被改写**：用户的偏好留在 store 里，窗口拉宽之后自动回到他选的那一侧。
 * 把窄屏写回 dock 会让「拉窄一次就永久变成浮动」——他没做过这个选择。
 * 这也是 `float` 与「窄屏覆盖」必须分开的原因：前者是选择，后者是当前处境。
 */
export function effectivePresentation(
  dock: SidebarDock,
  narrow: boolean,
): SidebarPresentation {
  if (narrow) return "overlay";
  return dock === "float" ? "overlay" : dock;
}

/**
 * 「显示/隐藏会话管理器」（`view.sidebar`，Ctrl+Shift+S）按当前呈现形态分流（路线图 4c，2026-09-02）。
 *
 * 停靠形态：翻 `sidebarVisible`——侧栏从版面里消失/回来，与此前一致。
 * overlay 形态（窄屏或用户选「浮动」）：侧栏本来就藏在屏外，用户按这个动作要的是**把它叫出来**。
 * 此前这里一律 `toggle("sidebar")`，效果是把 `sidebarVisible` 翻成 false——浮层的 `<aside>`
 * 整个不渲染，再按一次翻回 true 也只是回到「藏着」。审查时数过：`sidebarOverlayOpen` 生产代码里
 * 只有三处 `set(false)`，收起之后没有任何入口把它叫回来。这里就是那个入口：先保证
 * `sidebarVisible` 为真（否则 aside 不存在，开了也看不见），再翻浮层开关。
 */
export function toggleSidebarFor(presentation: SidebarPresentation): void {
  if (presentation === "overlay") {
    sidebarVisible.set(true);
    sidebarOverlayOpen.update((v) => !v);
    return;
  }
  toggle("sidebar");
}

/** 库里的脏值一律回落左停靠——一个认不出的值不该让侧栏消失。 */
export function isSidebarDock(v: unknown): v is SidebarDock {
  return v === "left" || v === "right" || v === "float";
}

/** 切换停靠侧并落库。 */
export async function setSidebarDock(next: SidebarDock): Promise<void> {
  if (!isSidebarDock(next)) return;
  if (await settingSet(SIDEBAR_DOCK_KEY, next) === false) return;
  sidebarDock.set(next);
  // 换成覆盖形态时默认收起：切过去就立刻挡住半个终端不是用户要的，
  // 他刚做的选择是「让侧栏别占地方」。
  if (next === "float") sidebarOverlayOpen.set(false);

}

/**
 * 接上 matchMedia，让 [`narrowScreen`] 跟着窗口宽度走。返回解绑函数。
 *
 * `matchMedia` 在某些环境里不存在（jsdom 默认、老 WebView）。这时**按宽屏处理**而不是窄屏：
 * 按窄屏的话，一个拿不到 matchMedia 的环境会永远显示覆盖层——而覆盖层是默认收起的，
 * 于是用户看到的是「侧栏不见了」。宽屏至少是当前布局。
 */
export function initResponsive(): () => void {
  if (typeof window === "undefined" || typeof window.matchMedia !== "function") {
    narrowScreen.set(false);
    return () => {};
  }
  const mql = window.matchMedia(WIDE_SCREEN_QUERY);
  const apply = (wide: boolean): void => {
    narrowScreen.set(!wide);
    // 从窄屏回到宽屏时把覆盖层状态清掉：留着的话，下次拉窄会直接是展开的，
    // 而用户上一次让它展开是在几分钟前的另一个窗口尺寸下。
    if (wide) sidebarOverlayOpen.set(false);
  };
  apply(mql.matches);
  const onChange = (e: MediaQueryListEvent): void => apply(e.matches);
  // 老 Safari/WebView 只有 addListener。两个都试，且解绑走对应的那个——
  // 只用 addEventListener 的话，那些环境里响应式布局整个不生效且毫无信号。
  if (typeof mql.addEventListener === "function") {
    mql.addEventListener("change", onChange);
    return () => mql.removeEventListener("change", onChange);
  }
  const legacy = mql as unknown as {
    addListener(fn: (e: MediaQueryListEvent) => void): void;
    removeListener(fn: (e: MediaQueryListEvent) => void): void;
  };
  legacy.addListener?.(onChange);
  return () => legacy.removeListener?.(onChange);
}
