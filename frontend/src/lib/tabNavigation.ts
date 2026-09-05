/** 页签支持方向键及 Home/End；点击仍由组件决定选中状态。 */
export function tabNavigation(node: HTMLElement): { destroy(): void } {
  function keydown(e: KeyboardEvent) {
    if (!["ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight", "Home", "End"].includes(e.key)) return;
    const tabs = Array.from(node.querySelectorAll<HTMLButtonElement>('[role="tab"]:not(:disabled)'));
    const index = tabs.indexOf(document.activeElement as HTMLButtonElement);
    if (index < 0 || !tabs.length) return;
    e.preventDefault();
    e.stopPropagation();
    const next = e.key === "Home" ? 0 : e.key === "End" ? tabs.length - 1
      : (index + (["ArrowUp", "ArrowLeft"].includes(e.key) ? -1 : 1) + tabs.length) % tabs.length;
    tabs[next].focus();
    tabs[next].click();
  }
  node.addEventListener("keydown", keydown);
  return { destroy: () => node.removeEventListener("keydown", keydown) };
}
