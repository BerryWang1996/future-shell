import { afterEach, describe, expect, it } from "vitest";
import { useFocusTrap } from "./focusTrap";

/**
 * 对话框焦点契约红测（UI 规格 §7）。
 * 本 Task 测试依赖仅 vitest + jsdom（frontend/package.json devDependencies），用 jsdom 原生 DOM，不引 @testing-library。
 */

function makeDialog(): { dialog: HTMLElement; first: HTMLElement; cancel: HTMLElement; last: HTMLElement } {
  const dialog = document.createElement("div");
  dialog.setAttribute("role", "dialog");
  const first = document.createElement("button");
  first.textContent = "首部";
  const cancel = document.createElement("button");
  cancel.textContent = "取消"; // CloseConfirm 默认焦点约定（§2.9）
  const last = document.createElement("button");
  last.textContent = "尾部";
  const disabled = document.createElement("button");
  disabled.setAttribute("disabled", "");
  disabled.textContent = "不可触发";
  // disabled 置于尾部：若圈闭未过滤 disabled，回绕目标将错指此钮，此布局使过滤断言具鉴别力
  dialog.append(first, cancel, last, disabled);
  document.body.appendChild(dialog);
  return { dialog, first, cancel, last };
}

/** 模拟 Tab/Shift+Tab：于当前焦点元素上派发，冒泡至 dialog 的 keydown 监听。 */
function pressTab(shift = false): KeyboardEvent {
  const e = new KeyboardEvent("keydown", { key: "Tab", shiftKey: shift, bubbles: true, cancelable: true });
  (document.activeElement ?? document.body).dispatchEvent(e);
  return e;
}

const cleanups: Array<() => void> = [];
afterEach(() => {
  while (cleanups.length > 0) cleanups.pop()!();
  document.body.innerHTML = "";
});

describe("useFocusTrap 对话框焦点契约（UI 规格 §7）", () => {
  it("回绕跳过隐藏、负 tabindex 和 inert 中的控件", () => {
    const { dialog, first, last } = makeDialog();
    dialog.insertAdjacentHTML("beforeend", '<input type="hidden"><button tabindex="-1">跳过</button><div hidden><button>隐藏</button></div><div inert><input></div><button style="display:none">不可见</button>');
    cleanups.push(useFocusTrap(dialog, { initial: first }));
    pressTab(true);
    expect(document.activeElement).toBe(last);
    pressTab();
    expect(document.activeElement).toBe(first);
  });
  it("无可用控件时聚焦弹窗本身", () => {
    const dialog = document.createElement("div");
    document.body.append(dialog);
    cleanups.push(useFocusTrap(dialog, { initial: null }));
    expect(document.activeElement).toBe(dialog);
    expect(pressTab().defaultPrevented).toBe(true);
  });
  it("打开即聚焦 initial（默认焦点元素；CloseConfirm=「取消」约定，§2.9）", () => {
    const { dialog, cancel } = makeDialog();
    cleanups.push(useFocusTrap(dialog, { initial: cancel }));
    expect(document.activeElement).toBe(cancel);
  });

  it("Tab 末元素 → 首元素回绕（disabled 不计入圈闭）", () => {
    const { dialog, first, last } = makeDialog();
    cleanups.push(useFocusTrap(dialog, { initial: last }));
    pressTab();
    expect(document.activeElement).toBe(first);
  });

  it("Shift+Tab 首元素 → 末元素反向回绕（disabled 不计入圈闭）", () => {
    const { dialog, first, last } = makeDialog();
    cleanups.push(useFocusTrap(dialog, { initial: first }));
    pressTab(true);
    expect(document.activeElement).toBe(last);
  });

  it("非边界 Tab 不劫持（不 preventDefault，原生导航照常）", () => {
    const { dialog, first } = makeDialog();
    cleanups.push(useFocusTrap(dialog, { initial: first }));
    const e = pressTab();
    expect(e.defaultPrevented).toBe(false);
  });

  it("cleanup 后焦点归还打开前的活动元素（触发元素）", () => {
    const trigger = document.createElement("button");
    trigger.textContent = "触发打开";
    document.body.appendChild(trigger);
    trigger.focus();
    expect(document.activeElement).toBe(trigger);
    const { dialog, cancel } = makeDialog();
    const cleanup = useFocusTrap(dialog, { initial: cancel });
    expect(document.activeElement).toBe(cancel);
    cleanup();
    expect(document.activeElement).toBe(trigger);
  });

  it("onRestore 于清理时调用一次", () => {
    const { dialog, first } = makeDialog();
    let restored = 0;
    const cleanup = useFocusTrap(dialog, { initial: first, onRestore: () => { restored += 1; } });
    cleanup();
    expect(restored).toBe(1);
  });
});
