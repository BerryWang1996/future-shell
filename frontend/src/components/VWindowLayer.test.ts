import { beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, waitFor } from "@testing-library/svelte";
import { get } from "svelte/store";
import VWindowLayer from "./VWindowLayer.svelte";
import LAYER_SRC from "./VWindowLayer.svelte?raw";
import { openVWindow, resetVWindowsForTest, vwindows } from "../lib/vwindow";

const invokeMock = vi.hoisted(() =>
  vi.fn(async (_cmd?: string, _args?: unknown): Promise<unknown> => ({ entries: [], truncated: false })),
);
vi.mock("../lib/ipc", () => ({
  invoke: invokeMock,
  settingGet: vi.fn(async (_k: string, f: unknown) => f),
  settingSet: vi.fn(async () => {}),
  reportFrontendError: vi.fn(),
}));

/**
 * 虚拟窗口画布（M7.3 出口标准③的装配面）。
 *
 * 两件事在这里、且只能在这里测：
 * ① 画布**自己量尺寸**并据此夹紧——夹紧要的是画布尺寸而不是浏览器视口（画布上面有标签栏、
 *    下面有状态栏、右边可能开着 AI 面板）。拿 window.innerWidth 去夹，窗口能被推到状态栏
 *    底下，那正是「拖丢了找不回来」；
 * ② 空白处的点击要落到底下的终端上（画布 `pointer-events: none`）——否则用户看到的是终端、
 *    点下去却没反应，会以为终端卡死。
 */
const VP = { width: 1000, height: 700 };

/** jsdom 里 clientWidth/Height 恒为 0：给画布元素装上可控的尺寸。 */
function sizeLayer(w: number, h: number) {
  const el = document.querySelector('[data-testid="vwindow-layer"]') as HTMLElement;
  Object.defineProperty(el, "clientWidth", { value: w, configurable: true });
  Object.defineProperty(el, "clientHeight", { value: h, configurable: true });
  return el;
}

/** 触发画布上那个 ResizeObserver（桩见 test-setup.ts）。 */
function fireResize() {
  const RO = (globalThis as unknown as { ResizeObserver: { instances?: { fire(): void }[] } }).ResizeObserver;
  for (const i of RO.instances ?? []) i.fire();
}

describe("VWindowLayer", () => {
  beforeEach(() => {
    cleanup();
    resetVWindowsForTest();
    invokeMock.mockReset();
    invokeMock.mockImplementation(async () => ({ entries: [], truncated: false }));
  });

  it("每个打开的窗口渲染一个外壳，标题可见", async () => {
    const a = openVWindow({ kind: "sftp", sessionId: "s1", title: "文件 — 甲" }, VP);
    const b = openVWindow({ kind: "sftp", sessionId: "s2", title: "文件 — 乙" }, VP);
    render(VWindowLayer, {});
    await waitFor(() => expect(document.querySelectorAll(".vwin").length).toBe(2));
    expect(document.querySelector(`[data-testid="vwin-${a}"]`)!.textContent).toContain("文件 — 甲");
    expect(document.querySelector(`[data-testid="vwin-${b}"]`)!.textContent).toContain("文件 — 乙");
  });

  it("窗口里装的是那个会话的文件面板（sessionId 真的传下去了）", async () => {
    openVWindow({ kind: "sftp", sessionId: "s-42", title: "文件" }, VP);
    render(VWindowLayer, {});
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("sftp_list", expect.objectContaining({ sessionId: "s-42" })),
    );
  });

  it("窗口里的文件面板不再显示「浮动」按钮（再点一次只会把同一个窗口置顶）", async () => {
    openVWindow({ kind: "sftp", sessionId: "s1", title: "文件" }, VP);
    render(VWindowLayer, {});
    await waitFor(() => expect(document.querySelectorAll(".vwin").length).toBe(1));
    expect(document.querySelector('[data-testid="remote-float"]')).toBeNull();
  });

  it("档案默认目录经 dirsFor 传进窗口里的面板", async () => {
    openVWindow({ kind: "sftp", sessionId: "s1", title: "文件" }, VP);
    render(VWindowLayer, { dirsFor: () => ({ local: "/L", remote: "/R" }) });
    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("sftp_list", expect.objectContaining({ path: "/R" }));
      expect(invokeMock).toHaveBeenCalledWith("local_list", expect.objectContaining({ path: "/L" }));
    });
  });

  it("画布变小 → 窗口被夹回画布内（主窗口缩小 / 拉开监控面板都走这条路）", async () => {
    openVWindow({ kind: "sftp", sessionId: "s1", title: "文件" }, VP);
    render(VWindowLayer, {});
    await waitFor(() => expect(document.querySelectorAll(".vwin").length).toBe(1));
    // 先按大画布把窗口推到右下角
    sizeLayer(VP.width, VP.height);
    fireResize();
    const w0 = get(vwindows)[0];
    expect(w0.x + w0.w).toBeLessThanOrEqual(VP.width);
    // 画布缩到一半
    sizeLayer(520, 380);
    fireResize();
    const w1 = get(vwindows)[0];
    expect(w1.x + w1.w).toBeLessThanOrEqual(520);
    expect(w1.y + w1.h).toBeLessThanOrEqual(380);
  });

  it("画布不吃事件、窗口吃（空白处点击要落到底下的终端上）", () => {
    render(VWindowLayer, {});
    const styles = LAYER_SRC.slice(LAYER_SRC.indexOf("<style>"));
    expect(styles).toMatch(/\.layer\s*\{[^}]*pointer-events:\s*none/);
    expect(styles).toMatch(/pointer-events:\s*auto/);
  });
});
