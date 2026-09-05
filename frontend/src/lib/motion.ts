/**
 * motion.ts — 动效的 JS 侧唯一入口（路线图 4c「动效补齐」，2026-09-02）。
 *
 * 全仓动效一律是 CSS（transition / animation + @starting-style，见 styles.css「动效」段），
 * 由 `@media (prefers-reduced-motion: reduce) { * { transition: none !important; animation: none !important } }`
 * 一条兜底。CSS 管不到的只有**时序**：toast 退场要先标记、等动效走完再从 store 里删——
 * 系统设了「减少动态效果」时这一拍也要省掉，否则用户看到的是「点了关闭还赖着 160ms」。
 * 所以 JS 侧凡涉及动效时长的判断都从这里问，不各自散写 matchMedia。
 *
 * `matchMedia` 在 jsdom / 老 WebView 里可能不存在——那时按「没有偏好」处理（照常动效），
 * 与 layout.ts 的 initResponsive 同一口径：拿不到就取默认，不猜用户要什么。
 */
export function prefersReducedMotion(): boolean {
  if (typeof window === "undefined" || typeof window.matchMedia !== "function") return false;
  return window.matchMedia("(prefers-reduced-motion: reduce)").matches;
}
