/**
 * app-driver.mjs — 在页面里驱动 FutureShell 界面的动作表（供 responsive-audit / motion-audit 共用）。
 *
 * 每个成员是一段要送进 Runtime.evaluate 的 JS 源串：点菜单、右键、按快捷键、关掉所有对话框、
 * 数标签页……写成源串而不是函数，是因为它们必须在**页面**里跑，这里拿不到页面的 document。
 * 菜单靠中文标签前缀定位（与 lib/i18n/zh-CN.ts 的 menu.* 同源），界面语言切成英文时需换标签。
 */
export const js = {
  clickMenu: (menu, item) => String.raw`(async () => {
    const wait = (ms) => new Promise((r) => setTimeout(r, ms));
    const top = [...document.querySelectorAll('[data-testid="menubar"] .menu > button')].find((b) => b.textContent.trim().startsWith(${JSON.stringify(menu)}));
    if (!top) return "no-menu";
    top.click(); await wait(120);
    if (${JSON.stringify(item)} === null) return "menu-open";
    const it = [...document.querySelectorAll('[data-testid="menubar"] .dropdown .item > button')].find((b) => (b.querySelector(".label")?.textContent || b.textContent).trim().startsWith(${JSON.stringify(item)}));
    if (!it) { top.click(); return "no-item"; }
    if (it.disabled) { top.click(); return "disabled"; }
    it.click(); await wait(300);
    return "ok";
  })()`,
  key: (key, code, mods) => String.raw`(() => { document.dispatchEvent(new KeyboardEvent("keydown", { key: ${JSON.stringify(key)}, code: ${JSON.stringify(code)}, ctrlKey: ${!!mods?.ctrl}, shiftKey: ${!!mods?.shift}, bubbles: true })); return "sent"; })()`,
  /**
   * 右键打开上下文菜单。`where`：
   *   "right"  → 目标元素右缘附近（真实用法）；
   *   "edge"   → 强制贴视口右缘（模拟右停靠 / 靠右的元素），并把子菜单**强制展开**——
   *              :hover 无法用合成事件触发，量子菜单出不出界只能这样量。
   */
  contextMenu: (selector, where) => String.raw`(async () => {
    const wait = (ms) => new Promise((r) => setTimeout(r, ms));
    // 目标在 overlay 侧栏里而侧栏收着（屏外 x<0）：先用快捷键叫出来，否则右键落在屏外没有意义
    const aside = document.querySelector('[data-testid="sidebar"]');
    window.__ra_openedOverlay = false;
    if (aside && aside.dataset.presentation === "overlay" && !aside.classList.contains("overlay-open") && aside.contains(document.querySelector(${JSON.stringify(selector)}))) {
      document.dispatchEvent(new KeyboardEvent("keydown", { key: "S", code: "KeyS", ctrlKey: true, shiftKey: true, bubbles: true }));
      window.__ra_openedOverlay = true; await wait(300);
    }
    const el = document.querySelector(${JSON.stringify(selector)}); if (!el) return "no-el";
    const r = el.getBoundingClientRect(); const vw = document.documentElement.clientWidth;
    const where = ${JSON.stringify(where)};
    const x = where === "edge" ? vw - 6 : where === "right" ? Math.min(vw - 6, r.right - 6) : r.left + 6;
    const y = r.top + Math.min(12, r.height / 2);
    el.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, clientX: x, clientY: y, button: 2 }));
    await wait(120);
    let subs = 0;
    if (where === "edge") for (const s of document.querySelectorAll('[role="menu"] .submenu')) { s.style.display = "block"; subs++; }
    return "ctx@" + Math.round(x) + "," + Math.round(y) + (subs ? " +" + subs + "sub" : "");
  })()`,
  /** 右键菜单收尾：点遮罩关菜单；若是本次为了右键才叫出的 overlay 侧栏，顺手收回去。 */
  closeCtx: (scrim) => String.raw`(async () => {
    const wait = (ms) => new Promise((r) => setTimeout(r, ms));
    document.querySelector(${JSON.stringify(scrim)})?.click(); await wait(150);
    if (window.__ra_openedOverlay) { document.dispatchEvent(new KeyboardEvent("keydown", { key: "S", code: "KeyS", ctrlKey: true, shiftKey: true, bubbles: true })); window.__ra_openedOverlay = false; await wait(200); }
    return "closed";
  })()`,
  clickSel: (selector) => String.raw`(() => { const el = document.querySelector(${JSON.stringify(selector)}); if (!el) return "no-el"; el.click(); return "clicked"; })()`,
  closeAll: String.raw`(async () => {
    const wait = (ms) => new Promise((r) => setTimeout(r, ms));
    for (let i = 0; i < 6; i++) {
      const dlgs = [...document.querySelectorAll('[role="dialog"]')];
      if (!dlgs.length) break;
      const d = dlgs[dlgs.length - 1];
      d.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", code: "Escape", bubbles: true }));
      await wait(160);
      if (document.contains(d)) {
        const btn = [...d.querySelectorAll("button")].find((b) => /^(取消|关闭|完成|×|✕|关 闭|Close|Cancel)$/.test(b.textContent.trim()));
        if (btn) { btn.click(); await wait(160); }
      }
      if (document.contains(d)) break;
    }
    const open = document.querySelector('[data-testid="menubar"] .dropdown');
    if (open) { open.parentElement.querySelector("button")?.click(); }
    for (const scrim of document.querySelectorAll(".tb-scrim, .menu-overlay")) scrim.click();
    await wait(100);
    return document.querySelectorAll('[role="dialog"]').length;
  })()`,
  tabs: String.raw`(() => [...document.querySelectorAll('[role="dialog"] [role="tablist"] button, [role="dialog"] .tabs button')].map((b) => b.textContent.trim()))()`,
  clickTab: (i) => String.raw`(() => { const b = [...document.querySelectorAll('[role="dialog"] [role="tablist"] button, [role="dialog"] .tabs button')][${i}]; if (!b) return "no-tab"; b.click(); return "tab"; })()`,
  overlay: String.raw`(() => document.querySelector('[data-testid="sidebar"]')?.dataset.presentation === "overlay")()`,
};
