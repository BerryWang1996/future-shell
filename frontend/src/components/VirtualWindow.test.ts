import { beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render } from "@testing-library/svelte";
import { get } from "svelte/store";
import VirtualWindow from "./VirtualWindow.svelte";
import SRC from "./VirtualWindow.svelte?raw";
import {
  MIN_H,
  MIN_W,
  openVWindow,
  resetVWindowsForTest,
  vwindows,
  type VWindow,
} from "../lib/vwindow";

/**
 * 虚拟窗口外壳的**行为**面（几何在 lib/vwindow.test.ts）。
 *
 * jsdom 没有 Pointer Events 的实现，`setPointerCapture` 不存在——但它正是本组件在真机上
 * 保证「拖动不掉帧」的东西，不能因为测不了就删。故此处补一个桩，并单独钉一条：
 * 组件**确实**调用了它（掉了这一句在 jsdom 里什么都测不出来，在真机上表现为快速拖动时
 * 窗口跟丢指针）。
 */
const VP = { width: 1000, height: 700 };

function stubPointerCapture() {
  const proto = Element.prototype as unknown as Record<string, unknown>;
  proto.setPointerCapture ??= vi.fn();
  proto.releasePointerCapture ??= vi.fn();
  proto.hasPointerCapture ??= vi.fn(() => false);
}

/** 造一个指针事件（jsdom 里 PointerEvent 缺失时退回 MouseEvent 并补上 pointerId）。 */
function pointer(type: string, init: { clientX?: number; clientY?: number; button?: number } = {}) {
  const e = new MouseEvent(type, { bubbles: true, cancelable: true, ...init });
  Object.defineProperty(e, "pointerId", { value: 1 });
  return e;
}

const only = (): VWindow => get(vwindows)[0];

function mountOne() {
  const id = openVWindow({ kind: "sftp", sessionId: "s1", title: "文件 — a" }, VP);
  const view = render(VirtualWindow, { win: only(), viewport: VP, children: undefined as never });
  return { id, view };
}

/** win 是 prop：store 变了要手动 rerender（真实使用中由 {#each $vwindows} 驱动）。 */
async function sync(view: { rerender: (p: Record<string, unknown>) => Promise<void> }) {
  await view.rerender({ win: only(), viewport: VP, children: undefined as never });
}

describe("VirtualWindow 拖动 / 缩放 / 置顶 / 关闭", () => {
  beforeEach(() => {
    cleanup();
    resetVWindowsForTest();
    stubPointerCapture();
  });

  it("标题栏拖动改位置，且落点等于「指针位移」而不是「指针绝对坐标」", async () => {
    const { id, view } = mountOne();
    const start = { x: only().x, y: only().y };
    const title = document.querySelector(`[data-testid="vwin-title-${id}"]`)!;
    // 按在标题栏中部（与窗口左上角有偏移）——若实现忘了减去这个偏移，窗口会跳一下
    title.dispatchEvent(pointer("pointerdown", { clientX: start.x + 120, clientY: start.y + 10, button: 0 }));
    title.dispatchEvent(pointer("pointermove", { clientX: start.x + 170, clientY: start.y + 40 }));
    await sync(view);
    expect(only()).toMatchObject({ x: start.x + 50, y: start.y + 30 });
  });

  it("拖到画布外被夹回来（出口标准：窗口不可拖出可视区外）", async () => {
    const { id, view } = mountOne();
    const title = document.querySelector(`[data-testid="vwin-title-${id}"]`)!;
    title.dispatchEvent(pointer("pointerdown", { clientX: 100, clientY: 10, button: 0 }));
    title.dispatchEvent(pointer("pointermove", { clientX: 99_999, clientY: 99_999 }));
    await sync(view);
    expect(only().x + only().w).toBeLessThanOrEqual(VP.width);
    expect(only().y + only().h).toBeLessThanOrEqual(VP.height);
  });

  it("抬起指针之后再动不再跟随（松手就是松手）", async () => {
    const { id, view } = mountOne();
    const title = document.querySelector(`[data-testid="vwin-title-${id}"]`)!;
    title.dispatchEvent(pointer("pointerdown", { clientX: 100, clientY: 10, button: 0 }));
    title.dispatchEvent(pointer("pointermove", { clientX: 150, clientY: 40 }));
    await sync(view);
    const after = { x: only().x, y: only().y };
    title.dispatchEvent(pointer("pointerup", { clientX: 150, clientY: 40 }));
    title.dispatchEvent(pointer("pointermove", { clientX: 400, clientY: 300 }));
    await sync(view);
    expect(only()).toMatchObject(after);
  });

  it("右键/中键按在标题栏上不启动拖动", async () => {
    const { id, view } = mountOne();
    const before = { x: only().x, y: only().y };
    const title = document.querySelector(`[data-testid="vwin-title-${id}"]`)!;
    title.dispatchEvent(pointer("pointerdown", { clientX: 100, clientY: 10, button: 2 }));
    title.dispatchEvent(pointer("pointermove", { clientX: 400, clientY: 300 }));
    await sync(view);
    expect(only()).toMatchObject(before);
  });

  it("右下角把手改尺寸，缩到最小以下时停在最小值", async () => {
    const { id, view } = mountOne();
    const grip = document.querySelector(`[data-testid="vwin-grip-${id}"]`)!;
    grip.dispatchEvent(pointer("pointerdown", { clientX: 500, clientY: 400, button: 0 }));
    grip.dispatchEvent(pointer("pointermove", { clientX: 560, clientY: 450 }));
    await sync(view);
    expect(only().w).toBeGreaterThan(MIN_W);
    grip.dispatchEvent(pointer("pointermove", { clientX: -9000, clientY: -9000 }));
    await sync(view);
    expect(only()).toMatchObject({ w: MIN_W, h: MIN_H });
  });

  it("键盘也能移动与缩放（只能拖 = 只能用鼠标）", async () => {
    const { id, view } = mountOne();
    const title = document.querySelector(`[data-testid="vwin-title-${id}"]`) as HTMLElement;
    const before = { x: only().x, y: only().y, w: only().w };
    await fireEvent.keyDown(title, { key: "ArrowRight" });
    await sync(view);
    expect(only().x).toBeGreaterThan(before.x);
    await fireEvent.keyDown(title, { key: "ArrowDown", shiftKey: true });
    await sync(view);
    expect(only().y).toBeGreaterThan(before.y + 8); // Shift 是加速档
    await fireEvent.keyDown(title, { key: "ArrowRight", ctrlKey: true });
    await sync(view);
    expect(only().w).toBeGreaterThan(before.w);
  });

  it("关闭按钮真的关掉这个窗口", async () => {
    const { id } = mountOne();
    await fireEvent.click(document.querySelector(`[data-testid="vwin-close-${id}"]`)!);
    expect(get(vwindows).length).toBe(0);
  });

  it("窗口任意处按下即置顶", async () => {
    const a = openVWindow({ kind: "sftp", sessionId: "s1", title: "a" }, VP);
    openVWindow({ kind: "sftp", sessionId: "s2", title: "b" }, VP);
    const winA = get(vwindows).find((w) => w.id === a)!;
    render(VirtualWindow, { win: winA, viewport: VP, children: undefined as never });
    document.querySelector(`[data-testid="vwin-${a}"]`)!
      .dispatchEvent(pointer("pointerdown", { clientX: 200, clientY: 200, button: 0 }));
    const top = [...get(vwindows)].sort((p, q) => q.z - p.z)[0];
    expect(top.id).toBe(a);
  });

  it("窗口有可访问的名字与关闭按钮（无障碍不是可选润色）", () => {
    const { id } = mountOne();
    const win = document.querySelector(`[data-testid="vwin-${id}"]`)!;
    expect(win.getAttribute("role")).toBe("dialog");
    expect(win.getAttribute("aria-label")).toContain("文件");
    expect(document.querySelector(`[data-testid="vwin-close-${id}"]`)!.getAttribute("aria-label")).toContain("关闭");
    expect(document.querySelector(`[data-testid="vwin-title-${id}"]`)!.getAttribute("tabindex")).toBe("0");
  });

  /* ── 源码级：jsdom 测不到、但真机上决定成败的两件事 ── */

  it("拖动用指针捕获（不用 document 上的 mousemove——快速拖动会掉事件）", () => {
    expect(SRC).toContain("setPointerCapture");
    expect(SRC).not.toMatch(/document\.addEventListener\(\s*["']mousemove/);
  });

  it("置顶挂在捕获相位：内容里的控件 stopPropagation 也不能让窗口沉在底下", () => {
    expect(SRC).toContain("onpointerdowncapture");
  });

  it("标题栏与把手都关掉浏览器触摸手势（否则触屏上拖窗口变成滚页面）", () => {
    const styles = SRC.slice(SRC.indexOf("<style>"));
    expect(styles.match(/touch-action:\s*none/g)?.length).toBeGreaterThanOrEqual(2);
  });
});
