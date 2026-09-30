/**
 * 右键菜单的窗口边缘避让（2026-08-31 评审 P2-8）。
 *
 * 此前标签/终端/侧栏/工具栏的菜单一律用 `clientX/clientY` 直接定位，
 * 靠右或靠下打开就出界——窗口边缘会把它裁掉一截，而菜单项恰恰是
 * 「删除/关闭」这类最不能只显示一半的东西。
 *
 * 做法：`use:clampToViewport` 挂在菜单容器上。菜单是条件渲染的，
 * 每次 open 都重新挂载 → action 每次都跑 → 用**渲染后的实测外框**
 * 算偏移，不估宽高（估出来的数在改菜单内容后就会悄悄失效）。
 * 平移用 `translate` 而不是改 left/top——不与模板里的行内定位打架。
 */

/** 纯函数：给定外框与视口，算需要平移多少。导出供单测。 */
export function clampDelta(
  rect: { left: number; top: number; right: number; bottom: number },
  vw: number,
  vh: number,
  margin = 8,
): { dx: number; dy: number } {
  let dx = 0;
  let dy = 0;
  if (rect.right + dx > vw - margin) dx = vw - margin - rect.right;
  if (rect.bottom + dy > vh - margin) dy = vh - margin - rect.bottom;
  if (rect.left + dx < margin) dx = margin - rect.left;
  if (rect.top + dy < margin) dy = margin - rect.top;
  return { dx, dy };
}

/** 一个子菜单要占的横向空间（Sidebar .submenu min-width 140 + 边框阴影余量）。 */
export const SUBMENU_ROOM = 160;

/**
 * 子菜单往哪边展开（2026-09-02 响应式核查）：菜单被钳到视口右缘之后，`left: 100%` 的子菜单
 * 再向右就整个出界——右停靠 / 800px 窄窗口下「移动到…」的分组列表点不到。
 * 纯函数，导出供单测；宿主按 `[data-sub-side="left"]` 把 .submenu 翻到左侧。
 */
export function submenuSide(menuRight: number, vw: number, room = SUBMENU_ROOM): "right" | "left" {
  return menuRight + room > vw ? "left" : "right";
}

/** Svelte action：挂载时把节点平移回视口内，并标注子菜单该往哪边开。 */
export function clampToViewport(node: HTMLElement): void {
  const r = node.getBoundingClientRect();
  const { dx, dy } = clampDelta(r, window.innerWidth, window.innerHeight);
  if (dx !== 0 || dy !== 0) {
    node.style.translate = `${dx}px ${dy}px`;
  }
  // 用**平移后**的右缘算：钳位本身就是把菜单往左挪，挪完贴着右缘时子菜单只能向左开。
  node.dataset.subSide = submenuSide(r.right + dx, window.innerWidth);
}
