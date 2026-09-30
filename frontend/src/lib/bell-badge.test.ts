import { describe, expect, it, vi, beforeEach } from "vitest";
import { render, screen, fireEvent } from "@testing-library/svelte";
import { get } from "svelte/store";
import { Terminal } from "@xterm/xterm";
import TERM_SOURCE from "./term.ts?raw";
import PANE_SOURCE from "../components/TerminalPane.svelte?raw";
import TabBar from "../components/TabBar.svelte";
import { tabs, addTab, activateTab, removeTab, ringBell } from "./tabs";

/**
 * 输出提醒 BEL → 铃铛角标（M1 出口：「会话收到 BEL/`\a` 后标签出现橙色铃铛角标，
 * 切回该标签或手动清除后消失」）。
 *
 * 修复前的覆盖状况：`tabs.test.ts` 测了 `ringBell`/`clearBell`（**链条末端**的 store 操作），
 * 但从「会话真的吐出一个 0x07 字节」到「store 被拨动」之间的每一跳都没有载体——
 * 即 store 测试全绿的同时，BEL 可以完全到不了它。
 *
 * `createTerminal` 无法在 jsdom 里整条跑（`term.open(el)` 要真实浏览器排版，见
 * TerminalPane.test.ts:35 的同一条限制），所以链条按跳分层取证，每层各自可被证伪：
 *
 *   ① 上游契约：真实 xterm 解析 0x07 会派发 `onBell`（本文件，真 Terminal，不 open）
 *   ② 订阅存在：`term.ts` 确实把 `onBell` 接到了 handler（本文件，源文本反向断言）
 *   ③ handler 落到 store：`TerminalPane` 的 `onBell` 调 `ringBell(sessionId)`
 *      （TerminalPane.test.ts，那里有捕获 handlers 的替身）
 *   ④ store 到像素：`bell: true` 渲染出角标，点击/切回该标签即消失（本文件，真组件）
 *
 * 少任何一层，「BEL 会点亮角标」都只是相邻两跳各自成立的推论。
 */

describe("① 上游契约：真实 xterm 对 BEL 字节派发 onBell", () => {
  /** 不调 `term.open()`：解析器不依赖 DOM 排版，而 open 在 jsdom 下会抛。 */
  function headless() {
    const t = new Terminal({ allowProposedApi: true });
    const bell = vi.fn();
    t.onBell(bell);
    return { t, bell };
  }
  const write = (t: Terminal, s: string) => new Promise<void>((r) => t.write(s, () => r()));

  it("0x07 触发 onBell（这是 term.ts 依赖的上游假设，xterm 换版即在此转红）", async () => {
    const { t, bell } = headless();
    await write(t, "output\x07more");
    expect(bell).toHaveBeenCalledTimes(1);
  });

  it("普通文本不触发（反向对照：契约若恒真，角标会在任何输出上乱闪）", async () => {
    const { t, bell } = headless();
    await write(t, "just plain text 中文 \x1b[32mgreen\x1b[0m\r\n");
    expect(bell).not.toHaveBeenCalled();
  });

  it("多个 BEL 各触发一次（计数而非只看「有没有」）", async () => {
    const { t, bell } = headless();
    await write(t, "\x07a\x07b\x07");
    expect(bell).toHaveBeenCalledTimes(3);
  });
});

describe("② 订阅存在：term.ts 把 xterm 的 bell 事件接到了 handler", () => {
  it("createTerminal 中订阅 term.onBell 并转交 h.onBell", () => {
    // 反向断言：这一行被删掉后，上面 ① 与下面 ③④ 仍然全绿——真实链条却断了。
    expect(
      /term\.onBell\(\s*\(\)\s*=>\s*h\.onBell\(\)\s*\)/.test(TERM_SOURCE),
      "term.ts 必须订阅 xterm 的 onBell 并转交 handler；缺了这一跳，BEL 到不了 store",
    ).toBe(true);
  });

  it("TerminalPane 把 onBell 接到 ringBell（组件侧那一跳的接线存在）", () => {
    expect(
      /onBell:\s*\(\)\s*=>\s*ringBell\(sessionId\)/.test(PANE_SOURCE),
      "TerminalPane 必须把 onBell 落到 ringBell(sessionId)",
    ).toBe(true);
  });
});

describe("④ store 到像素：角标渲染与两条清除手势", () => {
  beforeEach(() => {
    get(tabs).forEach((t) => removeTab(t.id));
  });

  it("bell 置位后标签上出现角标；未置位的标签没有", () => {
    addTab("t1", "Session 1", "p1", "connected", "h1");
    addTab("t2", "Session 2", "p2", "connected", "h2");
    ringBell("t1");
    render(TabBar, { props: { onClose: vi.fn() } });
    expect(screen.getByTestId("bell-t1")).toBeTruthy();
    expect(screen.queryByTestId("bell-t2"), "没响铃的标签不该长出角标").toBeNull();
  });

  it("点角标即清除（手动清除手势），且不连带切换标签", async () => {
    addTab("t1", "Session 1", "p1", "connected", "h1");
    addTab("t2", "Session 2", "p2", "connected", "h2");
    activateTab("t2"); // 活动标签是 t2；点 t1 的角标不该把活动标签抢过去
    ringBell("t1");
    render(TabBar, { props: { onClose: vi.fn() } });
    await fireEvent.click(screen.getByTestId("bell-t1"));
    expect(get(tabs).find((t) => t.id === "t1")?.bell).toBe(false);
    expect(screen.queryByTestId("bell-t1")).toBeNull();
  });

  it("切回该标签即清除（另一条手势：用户已经看到输出了）", async () => {
    addTab("t1", "Session 1", "p1", "connected", "h1");
    addTab("t2", "Session 2", "p2", "connected", "h2");
    activateTab("t2");
    ringBell("t1");
    render(TabBar, { props: { onClose: vi.fn() } });
    expect(screen.getByTestId("bell-t1")).toBeTruthy();
    await fireEvent.click(screen.getAllByRole("tab")[0]);
    expect(get(tabs).find((t) => t.id === "t1")?.bell, "切回该标签应清除角标").toBe(false);
    expect(screen.queryByTestId("bell-t1")).toBeNull();
  });

  it("角标是可操作控件而非纯装饰：有 role/键盘可达/无障碍名", () => {
    addTab("t1", "Session 1", "p1", "connected", "h1");
    ringBell("t1");
    render(TabBar, { props: { onClose: vi.fn() } });
    const badge = screen.getByTestId("bell-t1");
    expect(badge.getAttribute("role")).toBe("button");
    expect(badge.getAttribute("tabindex")).toBe("0");
    expect(badge.getAttribute("aria-label")).toBeTruthy();
  });
});
