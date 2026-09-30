import { describe, expect, it, vi } from "vitest";
import { render, screen, cleanup } from "@testing-library/svelte";
import CloseConfirmDialog from "./CloseConfirmDialog.svelte";
// 取 App.svelte 源文本走 vite 的 ?raw（同 menus.test.ts / session-restore.test.ts 的既有口径）。
import APP_SOURCE from "../App.svelte?raw";

/**
 * 关闭确认框（M1 出口「关闭确认」条的呈现面）。
 *
 * 出口原文要求弹框「列会话数+传输数」。判据收敛到 `decideClose` 之后，会话数可以合法地
 * 为 0——「一个断开态的残标签 + 一批在途传输」就是这种情形——于是这里必须钉住
 * **不虚报**：无条件渲染会话行会写出「将断开 0 个会话（）」，用户读到的是一句自相矛盾的话，
 * 而这正是判据从「只数传输」改成「活动会话 or 传输」时最容易漏掉的副作用。
 *
 * 另一半在 `lib/window-close.test.ts`（判据本身）与本文件末尾的接线断言（两个 scope
 * 是否真的都走了那个判据）。三者缺一，「弹框逻辑正确」就只是个说法。
 */
describe("CloseConfirmDialog（会话数/传输数呈现）", () => {
  const props = (over: Record<string, unknown> = {}) => ({
    open: true,
    scope: "tab" as const,
    sessions: ["s-1"],
    queued: 0,
    running: 0,
    onConfirm: vi.fn(),
    onCancel: vi.fn(),
    ...over,
  });

  it("有活动会话时列出会话数", async () => {
    render(CloseConfirmDialog, { props: props({ sessions: ["s-1", "s-2"] }) });
    expect(await screen.findByText(/将断开 2 个会话/)).toBeTruthy();
  });

  it("会话数为 0（只剩在途传输）时不出现会话行——不写「将断开 0 个会话」", () => {
    render(CloseConfirmDialog, { props: props({ sessions: [], queued: 2, running: 1 }) });
    expect(screen.queryByText(/将断开/), "空会话不得渲染会话行").toBeNull();
    // 传输行仍须在：否则用户看到的是一个不说明理由的确认框
    expect(screen.getByText(/取消 3 个传输/)).toBeTruthy();
  });

  it("无传输时不渲染传输行（对照：两行各自独立受控，不是一起显隐）", () => {
    render(CloseConfirmDialog, { props: props({ sessions: ["s-1"] }) });
    expect(screen.queryByText(/个传输/)).toBeNull();
    expect(screen.getByText(/将断开 1 个会话/)).toBeTruthy();
  });

  it("会话数超 10 折叠显示，但计数用全量（截断的是名字不是数字）", () => {
    const many = Array.from({ length: 14 }, (_, i) => `s-${i}`);
    render(CloseConfirmDialog, { props: props({ sessions: many }) });
    expect(screen.getByText(/将断开 14 个会话/)).toBeTruthy();
    expect(screen.getByText(/等 14 个/)).toBeTruthy();
    expect(screen.queryByText(/s-13/), "第 11 个之后的名字应被折叠").toBeNull();
  });

  it("默认焦点在「取消」：惯性回车不会断开任何会话", async () => {
    render(CloseConfirmDialog, { props: props() });
    await new Promise((r) => setTimeout(r, 0)); // 焦点陷阱在 $effect 里装
    expect(document.activeElement?.textContent?.trim()).toBe("取消");
  });
});

/**
 * 接线断言：两个 scope 都必须经由 `decideClose` 决定弹不弹。
 *
 * 组件测试证明的是「给定 props 怎么渲染」，证明不了 App.svelte 有没有把判据接上去。
 * 修复前的原状恰恰是接线错误而非渲染错误：`requestCloseTab` 里写着 `queued + running > 0`，
 * 一条活连接因此从不进入本组件。所以此处对源文本下反向断言——旧判据复活即红。
 */
describe("App.svelte 关闭判据接线", () => {
  it("三条关闭路径各有一处 decideClose 调用", () => {
    // 三条：关窗口（requestCloseWindow）、关一个标签（requestCloseTab）、
    // 关一批标签（requestCloseTabs，M4b「关闭全部/其他标签」）。
    //
    // 钉总数而不是逐个找函数名：新加一条关闭路径时这里会先红，逼着人回答
    // 「它走没走同一个判据」——而绕过判据的那条路径不会有任何别的信号。
    // 少一次同样红：那意味着有一路能静默关掉活连接。
    const calls = APP_SOURCE.match(/decideClose\(/g) ?? [];
    expect(calls.length, "每条关闭路径各一次；多一次或少一次都要在此处做决定").toBe(3);
  });

  it("关一批标签走的是同一个确认模态，不是自己弹一个", () => {
    // 出口原文点名要「触发**同一**关闭确认模态」。另起一套的话，
    // 「关一个标签会问、关十个反而不问」这种事迟早出现，而它出现时没有任何提示。
    expect(APP_SOURCE).toMatch(/async function requestCloseTabs\(/);
    // 它必须写进 closeConfirmState（那是唯一的模态载体），而不是别的什么
    const body = APP_SOURCE.slice(APP_SOURCE.indexOf("async function requestCloseTabs("));
    expect(body.slice(0, 1200)).toMatch(/closeConfirmState = \{/);
  });

  it("传输计数用全局在途量，不是逐标签累加", () => {
    // 传输队列本来就是全局的。逐标签累加会把同一件传输数很多次，
    // 于是确认框上写着「取消 12 个传输」而实际只有 3 个。
    const body = APP_SOURCE.slice(APP_SOURCE.indexOf("async function requestCloseTabs("));
    expect(body.slice(0, 1200)).toMatch(/decideClose\(targets, activeTransfers\(\)\)/);
  });

  it("标签级不再用「只数传输」的旧判据", () => {
    // 旧判据原文：`if (queued + running > 0) {`。它一旦回来，活连接就又能被静默关掉。
    expect(
      /if \(queued \+ running > 0\)/.test(APP_SOURCE),
      "requestCloseTab 不得退回「只有传输才确认」",
    ).toBe(false);
  });

  it("窗口级不再用 tabs.length 当判据（残标签也弹框，违反「全空闲直接关闭」）", () => {
    expect(
      /activeSessions\.length === 0/.test(APP_SOURCE),
      "requestCloseWindow 不得退回按标签总数判断",
    ).toBe(false);
  });
});

/**
 * scope 影响文案（M4b）。
 *
 * 这个 prop 曾经**声明了却从没被消费**——模板里一个字都没用到它，于是
 * 「关整个窗口」与「关一个标签」弹的是一模一样的框。危险程度差着量级的两件事
 * 长得一样，是确认框最不该有的样子：用户按下那个红按钮时心里想的是哪一件，
 * 只能靠他记得自己刚才点了什么。
 */
describe("CloseConfirmDialog 的 scope 要看得出来", () => {
  const base = { open: true, sessions: ["s1"], queued: 0, running: 0, onConfirm: () => {}, onCancel: () => {} };

  it("三种 scope 的标题各不相同", () => {
    const titles = new Set<string>();
    for (const scope of ["window", "tab", "tabs"] as const) {
      cleanup();
      render(CloseConfirmDialog, { props: { ...base, scope } });
      titles.add(screen.getByTestId("close-confirm-title").textContent ?? "");
    }
    // 三个各不相同——两个一样就说明有一种 scope 没被区分出来
    expect(titles.size).toBe(3);
  });

  it("关窗口的标题说「窗口」，关标签的不说", () => {
    render(CloseConfirmDialog, { props: { ...base, scope: "window" } });
    expect(screen.getByTestId("close-confirm-title").textContent).toContain("窗口");
    expect(screen.getByTestId("close-confirm-ok").textContent).toContain("窗口");
    cleanup();

    render(CloseConfirmDialog, { props: { ...base, scope: "tab" } });
    expect(screen.getByTestId("close-confirm-title").textContent).not.toContain("窗口");
    expect(screen.getByTestId("close-confirm-ok").textContent).not.toContain("窗口");
  });

  it("关一批标签时标题说「多个」", () => {
    render(CloseConfirmDialog, { props: { ...base, scope: "tabs", sessions: ["s1", "s2", "s3"] } });
    expect(screen.getByTestId("close-confirm-title").textContent).toContain("多个");
    // 名单仍然列出来——「关闭多个标签」而不说是哪几个，等于让用户盲按
    expect(screen.getByText(/s1、s2、s3/)).toBeTruthy();
  });
});
