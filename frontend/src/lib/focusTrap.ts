const FOCUSABLE =
  'button, [href], input, select, textarea, [tabindex]:not([tabindex="-1"])';

function canFocus(el: HTMLElement): boolean {
  if (el.matches(':disabled, input[type="hidden"]') || el.tabIndex < 0) return false;
  for (let parent: HTMLElement | null = el; parent; parent = parent.parentElement) {
    if (parent.hidden || parent.hasAttribute("inert") || parent.getAttribute("aria-hidden") === "true") return false;
    const style = getComputedStyle(parent);
    if (style.display === "none" || style.visibility === "hidden") return false;
  }
  return true;
}

/**
 * 打开 → 聚焦 opts.initial；Tab/Shift+Tab 循环圈闭于 dialog 内可聚焦元素；
 * 返回的清理函数在关闭时调用，焦点归还打开前的活动元素。
 * 用法：对话框组件 `$effect` 内 `return useFocusTrap(dialogEl, { initial: defaultBtn });`。
 */
export function useFocusTrap(
  dialog: HTMLElement,
  opts: { initial: HTMLElement | null; onRestore?: () => void },
): () => void {
  const restoreTarget = document.activeElement as HTMLElement | null; // 归还目标
  const candidates = () => Array.from(dialog.querySelectorAll<HTMLElement>(FOCUSABLE)).filter(canFocus);
  const initial = opts.initial && (opts.initial === dialog || canFocus(opts.initial))
    ? opts.initial : candidates()[0] ?? dialog;
  if (initial === dialog && !dialog.hasAttribute("tabindex")) dialog.tabIndex = -1;
  initial.focus();

  function onKeydown(e: KeyboardEvent) {
    if (e.key !== "Tab") return;
    e.stopPropagation(); // 内层弹窗处理 Tab 后，外层不能再次移动焦点。
    const items = candidates();
    if (items.length === 0) {
      e.preventDefault();
      return;
    }
    const first = items[0];
    const last = items[items.length - 1];
    const active = document.activeElement;
    if (e.shiftKey && (active === first || !dialog.contains(active))) {
      e.preventDefault();
      last.focus(); // Shift+Tab 首部回绕至尾部
    } else if (!e.shiftKey && (active === last || !dialog.contains(active))) {
      e.preventDefault();
      first.focus(); // Tab 尾部回绕至首部
    }
  }

  dialog.addEventListener("keydown", onKeydown);
  return () => {
    dialog.removeEventListener("keydown", onKeydown);
    opts.onRestore?.();
    if (restoreTarget?.isConnected) restoreTarget.focus();
  };
}
