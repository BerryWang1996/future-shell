/**
 * window-actions（2026-09-03 从 App.svelte 搬出）。
 *
 * 三条判据各自对应一处「不这样做会怎样」：
 * · detachTab 对已关闭的标签不开窗——否则用户面前是一个显示已死会话、还能往里打字的窗口；
 * · closeViewWindowFor 发 sessionId 而不是窗口 label——label 怎么拼是后端的事，两份规则走散的
 *   表现正是这个窗口永远关不掉；
 * · arrangeWindows 只在放不下时才出声——沉默地摆成一堆，用户无从知道是没生效还是屏幕不够。
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";

const calls: { cmd: string; args?: Record<string, unknown> }[] = [];
const replies: Record<string, (a?: Record<string, unknown>) => unknown> = {};
vi.mock("./ipc", () => ({
  invoke: async (cmd: string, args?: Record<string, unknown>) => {
    calls.push({ cmd, args });
    const r = replies[cmd];
    if (!r) return undefined;
    return r(args);
  },
}));

import { resetToastsForTest, toast } from "./toast";
import { tabs } from "./tabs";
import { arrangeWindows, closeViewWindowFor, detachTab, openWindow } from "./window-actions";

const tab = (id: string) =>
  ({ id, title: id, bell: false, status: "connected", profileId: "p", host: "", kind: "ssh", keyboardMode: "remote" }) as never;

beforeEach(() => {
  calls.length = 0;
  for (const k of Object.keys(replies)) delete replies[k];
  resetToastsForTest(); // dismiss 分两拍（动效批），同步清不干净
  tabs.set([]);
});

describe("openWindow", () => {
  it("发 window_new，带 sessionId 与 title", async () => {
    await openWindow("s1", "web-01");
    expect(calls).toEqual([{ cmd: "window_new", args: { sessionId: "s1", title: "web-01" } }]);
  });

  it("失败要出声（开窗是用户主动动作，静默失败就是「点了没反应」）", async () => {
    replies.window_new = () => {
      throw new Error("no display");
    };
    await openWindow(null, null);
    expect(get(toast).some((t) => t.level === "error" && t.msg.includes("新建窗口失败"))).toBe(true);
  });
});

describe("detachTab", () => {
  it("标签还在 → 开视图窗口（克隆而非搬家，标签留在主窗口）", () => {
    tabs.set([tab("s1")]);
    detachTab("s1", "web-01");
    expect(calls[0]).toEqual({ cmd: "window_new", args: { sessionId: "s1", title: "web-01" } });
  });

  it("标签已被关掉 → 不开窗（否则是个显示已死会话、还能打字的窗口）", () => {
    tabs.set([tab("other")]);
    detachTab("s1", "web-01");
    expect(calls).toEqual([]);
  });
});

describe("closeViewWindowFor", () => {
  it("发的是 sessionId 而不是窗口 label", () => {
    closeViewWindowFor("s1");
    expect(calls).toEqual([{ cmd: "window_close_view", args: { sessionId: "s1" } }]);
  });

  it("窗口本来就不存在不打扰用户（大多数标签没被拖出过）", async () => {
    replies.window_close_view = () => {
      throw new Error("no such window");
    };
    closeViewWindowFor("s1");
    await Promise.resolve();
    await Promise.resolve();
    expect(get(toast)).toEqual([]);
  });
});

describe("arrangeWindows", () => {
  it("全部摆下了就不出声", async () => {
    replies.window_arrange = () => ({ arranged: 3, overflowed: 0 });
    await arrangeWindows("tileH");
    expect(calls[0]).toEqual({ cmd: "window_arrange", args: { mode: "tileH" } });
    expect(get(toast)).toEqual([]);
  });

  it("放不下时说清楚摆了几个、还有几个叠着", async () => {
    replies.window_arrange = () => ({ arranged: 4, overflowed: 2 });
    await arrangeWindows("tileV");
    const msg = get(toast)[0]?.msg ?? "";
    expect(msg).toContain("4");
    expect(msg).toContain("2");
  });

  it("失败要出声", async () => {
    replies.window_arrange = () => {
      throw new Error("boom");
    };
    await arrangeWindows("cascade");
    expect(get(toast).some((t) => t.level === "error" && t.msg.includes("排列窗口失败"))).toBe(true);
  });
});
