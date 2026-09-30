/**
 * ComposeBar 组件测试（M4a 广播通道①实装）。
 *
 * 组件本身只有两件事值得钉：
 * 1. 目标选择（含广播四态）随 onSend 传出——目标语义的判据在 lib/broadcast.test.ts，
 *    这里只钉「选择确实传到了装配层」，断了装配层就退回 MVP 的恒发当前会话；
 * 2. 手选面板的勾选写进 pickedSessions store、全选/清空可用——手选集合是
 *    resolveTargetSessions("pick") 的输入，UI 写不进去等于手选恒空。
 */
import { describe, expect, it, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/svelte";
import ComposeBar from "./ComposeBar.svelte";
import { composeTarget, pickedSessions, resetBroadcastStoresForTest } from "../lib/broadcast";
import { historyClear } from "../lib/compose-history";
import APP_SOURCE from "../App.svelte?raw";
import { get } from "svelte/store";

const openSessions = [
  { id: "s1", title: "web-01" },
  { id: "s2", title: "web-02" },
  { id: "s3", title: "db-01" },
];

describe("ComposeBar（M4a 广播目标）", () => {
  beforeEach(() => {
    resetBroadcastStoresForTest();
  });

  it("四个目标均可选且选择写入共享 store（与实时广播同源，S306）", async () => {
    render(ComposeBar, { props: { onSend: vi.fn(), openSessions } });
    const sel = screen.getByLabelText("发送目标") as HTMLSelectElement;
    const opts = [...sel.querySelectorAll("option")].map((o) => o.value);
    // MVP 期的 disabled 预留须全部实装
    expect(opts).toEqual(["current", "all", "group", "pick"]);
    await fireEvent.change(sel, { target: { value: "all" } });
    expect(get(composeTarget)).toBe("all");
  });

  it("onSend 携带目标与文本/后缀", async () => {
    const onSend = vi.fn();
    render(ComposeBar, { props: { onSend, openSessions } });
    await fireEvent.change(screen.getByLabelText("发送目标"), { target: { value: "all" } });
    const input = screen.getByRole("textbox");
    await fireEvent.input(input, { target: { value: "uptime" } });
    await fireEvent.keyDown(input, { key: "Enter" });
    expect(onSend).toHaveBeenCalledWith("all", "uptime", "\r");
  });

  it("手选面板：勾选写入 pickedSessions，全选/清空可用", async () => {
    render(ComposeBar, { props: { onSend: vi.fn(), openSessions } });
    await fireEvent.change(screen.getByLabelText("发送目标"), { target: { value: "pick" } });
    // 打开手选面板
    await fireEvent.click(screen.getByLabelText("手选目标会话"));
    const panel = await screen.findByTestId("pick-panel");
    expect(panel).toBeTruthy();
    const boxes = panel.querySelectorAll<HTMLInputElement>('input[type="checkbox"]');
    expect(boxes.length).toBe(3);
    await fireEvent.click(boxes[0]);
    await fireEvent.click(boxes[2]);
    expect(get(pickedSessions)).toEqual(["s1", "s3"]);
    // 全选 → 清空
    const buttons = [...panel.querySelectorAll("button")];
    await fireEvent.click(buttons.find((b) => b.textContent === "全选")!);
    await waitFor(() => expect(get(pickedSessions)).toEqual(["s1", "s2", "s3"]));
    await fireEvent.click(buttons.find((b) => b.textContent === "清空")!);
    await waitFor(() => expect(get(pickedSessions)).toEqual([]));
  });

  it("目标切离 pick 时收起面板（面板只属于 pick 态，S307）", async () => {
    render(ComposeBar, { props: { onSend: vi.fn(), openSessions } });
    await fireEvent.change(screen.getByLabelText("发送目标"), { target: { value: "pick" } });
    await fireEvent.click(screen.getByLabelText("手选目标会话"));
    expect(screen.queryByTestId("pick-panel")).toBeTruthy();
    await fireEvent.change(screen.getByLabelText("发送目标"), { target: { value: "all" } });
    await waitFor(() => expect(screen.queryByTestId("pick-panel")).toBeNull());
  });
});

/**
 * M1 出口「组合命令栏当前会话发送（§2.6）」：Enter 发送附后缀（CR/LF/CRLF 选择器生效）、
 * Ctrl+Enter 不附后缀、↑/↓ 走历史、无活动会话出 Toast 且不发 IPC。
 *
 * 修复前这半条一个断言都没有——本文件原有的 4 例全属 M4a 广播目标，
 * 而出口原文当初写的是「手工核验留痕」。手工留痕对**回归**无效：换行后缀是发到远端的
 * 真实字节，CR/LF 写反时 shell 要么不执行要么多吞一行，而这类错误只在特定远端上现形。
 *
 * `historyPush`/`historyPrev` 的语义判据在 lib/compose-history.test.ts；此处只钉
 * 「按键真的接到了它们」——那两者之间正是修复前完全空白的地带。
 */
describe("ComposeBar（M1 §2.6 发送与后缀）", () => {
  beforeEach(() => {
    resetBroadcastStoresForTest();
    historyClear();
  });

  const type = async (v: string) => {
    const input = screen.getByPlaceholderText(/输入命令/) as HTMLInputElement;
    await fireEvent.input(input, { target: { value: v } });
    return input;
  };

  it("Enter 发送并附加当前后缀；缺省后缀是 CR", async () => {
    const onSend = vi.fn();
    render(ComposeBar, { props: { onSend, openSessions } });
    const input = await type("ls -al");
    await fireEvent.keyDown(input, { key: "Enter" });
    expect(onSend).toHaveBeenCalledTimes(1);
    expect(onSend.mock.calls[0][1]).toBe("ls -al");
    expect(onSend.mock.calls[0][2]).toBe("\r");
  });

  it("后缀选择器三档各自生效（CR / LF / CRLF 是发到远端的真实字节）", async () => {
    for (const [label, bytes] of [["CR", "\r"], ["LF", "\n"], ["CRLF", "\r\n"]] as const) {
      const onSend = vi.fn();
      const view = render(ComposeBar, { props: { onSend, openSessions } });
      const sel = screen.getByLabelText("发送后缀") as HTMLSelectElement;
      await fireEvent.change(sel, { target: { value: bytes } });
      const input = await type("echo hi");
      await fireEvent.keyDown(input, { key: "Enter" });
      expect(onSend.mock.calls[0][2], `${label} 应发出 ${JSON.stringify(bytes)}`).toBe(bytes);
      view.unmount();
    }
  });

  it("Ctrl+Enter 不附后缀（§2.6：交互式提示符下先填字再自己决定回不回车）", async () => {
    const onSend = vi.fn();
    render(ComposeBar, { props: { onSend, openSessions } });
    const input = await type("yes");
    await fireEvent.keyDown(input, { key: "Enter", ctrlKey: true });
    expect(onSend).toHaveBeenCalledTimes(1);
    expect(onSend.mock.calls[0][2], "Ctrl+Enter 必须发空后缀").toBe("");
  });

  it("发送后清空输入框（否则下一条命令会拼在上一条后面）", async () => {
    render(ComposeBar, { props: { onSend: vi.fn(), openSessions } });
    const input = await type("uptime");
    await fireEvent.keyDown(input, { key: "Enter" });
    expect(input.value).toBe("");
  });

  it("空/纯空白不发送（Enter 连击不得往远端灌空行）", async () => {
    const onSend = vi.fn();
    render(ComposeBar, { props: { onSend, openSessions } });
    const input = await type("   ");
    await fireEvent.keyDown(input, { key: "Enter" });
    await fireEvent.keyDown(input, { key: "Enter" });
    expect(onSend).not.toHaveBeenCalled();
  });

  it("↑ 回填上一条、↓ 前进；↓ 走到底回空串（不是卡在最后一条）", async () => {
    render(ComposeBar, { props: { onSend: vi.fn(), openSessions } });
    const input = await type("first");
    await fireEvent.keyDown(input, { key: "Enter" });
    await type("second");
    await fireEvent.keyDown(input, { key: "Enter" });

    await fireEvent.keyDown(input, { key: "ArrowUp" });
    expect(input.value).toBe("second");
    await fireEvent.keyDown(input, { key: "ArrowUp" });
    expect(input.value).toBe("first");
    await fireEvent.keyDown(input, { key: "ArrowDown" });
    expect(input.value).toBe("second");
    await fireEvent.keyDown(input, { key: "ArrowDown" });
    expect(input.value, "走过最新一条应回空串，让用户能直接打新命令").toBe("");
  });

  it("↑↓ 与 Enter 都要 preventDefault（否则光标跳行首/表单提交刷页）", async () => {
    render(ComposeBar, { props: { onSend: vi.fn(), openSessions } });
    const input = await type("x");
    for (const key of ["Enter", "ArrowUp", "ArrowDown"]) {
      const ev = new KeyboardEvent("keydown", { key, bubbles: true, cancelable: true });
      input.dispatchEvent(ev);
      expect(ev.defaultPrevented, `${key} 未 preventDefault`).toBe(true);
    }
  });
});

/**
 * 出口那句「无活动会话出 Toast **且不发 IPC**」落在 App.svelte 的 sendCompose 里，
 * 不在本组件内。空集合的判据本身由 lib/broadcast.test.ts 覆盖
 * （「current：活动会话已连接则恰为其一；未连接/无活动为空」），这里补的是剩下那一半：
 * 空集合时那条 return 必须出现在**编码与发送之前**——写在后面即「先发了再提示」，
 * 而 IPC 一旦发出就收不回来。故用源码顺序断言（同仓既有口径）。
 */
describe("App.svelte sendCompose 空目标短路", () => {
  it("空目标提示 + return 出现在 encodeB64/sendToSessions 之前", () => {
    const body = APP_SOURCE.slice(APP_SOURCE.indexOf("async function sendCompose("));
    const guard = body.indexOf("if (ids.length === 0)");
    const encode = body.indexOf("encodeB64(");
    const send = body.indexOf("sendToSessions(");
    expect(guard, "sendCompose 必须有空目标短路").toBeGreaterThanOrEqual(0);
    expect(encode).toBeGreaterThan(guard);
    expect(send, "短路必须在发送之前——写在后面就是「先发了再提示」").toBeGreaterThan(guard);
    // 短路段内必须真的出声（只 return 不提示 = 用户按了 Enter 什么都没发生）
    expect(body.slice(guard, encode)).toMatch(/toast\.(warn|error)\(/);
    expect(body.slice(guard, encode)).toMatch(/\breturn\b/);
  });
});
