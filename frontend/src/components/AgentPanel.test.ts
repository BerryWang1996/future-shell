import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { render, screen, fireEvent, waitFor, cleanup } from "@testing-library/svelte";

/**
 * Agent 面板（M3 出口 1/2/3 的界面面）。
 *
 * 重心在三件安全相关的事：
 *  ① **强确认要逐字输入**——手势成本是 dangerous 与 write 的唯一区别；
 *     两者交互一样的话，分级在界面上就等于没有。
 *  ② **急停按钮在跑的时候必须在**，且后端说「已经结束了」时按钮态要跟着回落
 *     （不能留一个永远转的「停止中…」）。
 *  ③ **run 在后台跑**：面板关掉再开，不能显示「开始」——那会让用户并发起
 *     第二个 run，两个 Agent 同时对着一台机器发命令。
 */

const calls: { cmd: string; args?: Record<string, unknown> }[] = [];
let startReply: unknown = { run_id: "run-1" };
let abortReply = true;
let confirmAnswerReply = true;
let isRunningReply = true;
let throwOn: string | null = null;

/** 事件监听表：测试直接触发后端事件。 */
const listeners: Record<string, ((e: { payload: unknown }) => void)[]> = {};

vi.mock("../lib/ipc", () => ({
  invoke: async <T>(cmd: string, args?: Record<string, unknown>): Promise<T> => {
    calls.push({ cmd, args });
    if (throwOn === cmd) throw new Error(`${cmd} 拒绝了`);
    if (cmd === "agent_start") return startReply as T;
    if (cmd === "agent_abort") return abortReply as T;
    if (cmd === "agent_confirm_answer") return confirmAnswerReply as T;
    if (cmd === "agent_ask_reply") return true as T;
    if (cmd === "agent_is_running") return isRunningReply as T;
    return undefined as T;
  },
  listen: async (name: string, cb: (e: { payload: unknown }) => void) => {
    (listeners[name] ??= []).push(cb);
    return () => {
      listeners[name] = (listeners[name] ?? []).filter((f) => f !== cb);
    };
  },
}));

vi.mock("../lib/toast", () => ({
  toast: { info: vi.fn(), warn: vi.fn(), error: vi.fn() },
}));

import AgentPanel from "./AgentPanel.svelte";
import { toast } from "../lib/toast";

/**
 * 触发一次后端事件。
 *
 * **先等监听器注册完**：面板的三个 listen 是串行 await 的，render 返回时
 * 它们还在注册途中。直接发事件会随机落进「已注册」与「还没注册」之间——
 * 表现是同一批用例里有的过有的挂（第一版就是 ask 那条挂了，因为它排第二）。
 */
async function emit(name: string, payload: unknown): Promise<void> {
  await waitFor(() => expect((listeners[name] ?? []).length).toBeGreaterThan(0));
  for (const cb of listeners[name] ?? []) cb({ payload });
}

const confirmPayload = (over: Record<string, unknown> = {}) => ({
  request_id: 7,
  run_id: "run-1",
  action: "rm -rf /var/log/x",
  tier: "dangerous",
  strong: true,
  rules: ["recursive_delete_at_root_or_home"],
  details: ["递归删除"],
  deadline_secs: 60,
  ...over,
});

beforeEach(() => {
  calls.length = 0;
  for (const k of Object.keys(listeners)) delete listeners[k];
  startReply = { run_id: "run-1" };
  abortReply = true;
  confirmAnswerReply = true;
  isRunningReply = true;
  throwOn = null;
});
afterEach(cleanup);

describe("启动与急停", () => {
  it("没有会话时不发请求——Agent 要在某个会话里干活", async () => {
    render(AgentPanel, { props: { open: true, sessionId: null } });
    await fireEvent.input(screen.getByTestId("agent-task"), { target: { value: "排查磁盘" } });
    await fireEvent.click(screen.getByTestId("agent-start"));
    expect(calls.filter((c) => c.cmd === "agent_start")).toEqual([]);
    expect(toast.warn).toHaveBeenCalled();
  });

  it("启动后显示急停按钮（而不是再显示一次「开始」）", async () => {
    render(AgentPanel, { props: { open: true, sessionId: "s1" } });
    await fireEvent.input(screen.getByTestId("agent-task"), { target: { value: "排查磁盘" } });
    await fireEvent.click(screen.getByTestId("agent-start"));
    await waitFor(() => expect(screen.getByTestId("agent-abort")).toBeTruthy());
    expect(screen.queryByTestId("agent-start")).toBeNull();
    const c = calls.find((x) => x.cmd === "agent_start")!;
    expect(c.args).toMatchObject({ task: "排查磁盘", sessionId: "s1" });
  });

  it("启动期拒绝走 toast（那时 run 还不存在）", async () => {
    throwOn = "agent_start";
    render(AgentPanel, { props: { open: true, sessionId: "s1" } });
    await fireEvent.input(screen.getByTestId("agent-task"), { target: { value: "x" } });
    await fireEvent.click(screen.getByTestId("agent-start"));
    await waitFor(() => expect(toast.error).toHaveBeenCalled());
    // 按钮回到「开始」——不是留一个卡住的「启动中…」
    expect(screen.getByTestId("agent-start")).toBeTruthy();
  });

  it("急停时后端说「已经结束了」⇒ 按钮态回落，不留一个永远转的停止中", async () => {
    abortReply = false;
    render(AgentPanel, { props: { open: true, sessionId: "s1" } });
    await fireEvent.input(screen.getByTestId("agent-task"), { target: { value: "x" } });
    await fireEvent.click(screen.getByTestId("agent-start"));
    await waitFor(() => expect(screen.getByTestId("agent-abort")).toBeTruthy());
    await fireEvent.click(screen.getByTestId("agent-abort"));
    await waitFor(() => expect(screen.getByTestId("agent-start")).toBeTruthy());
  });
});

describe("确认对话框（分级的手势成本）", () => {
  it("**强确认必须逐字输入「确认」**——没输就不放行，也不发请求", async () => {
    render(AgentPanel, { props: { open: true, sessionId: "s1" } });
    await emit("agent:confirm", confirmPayload());
    await waitFor(() => expect(screen.getByTestId("agent-confirm")).toBeTruthy());
    // 直接点「执行」：不该发请求
    await fireEvent.click(screen.getByTestId("agent-confirm-yes"));
    expect(calls.filter((c) => c.cmd === "agent_confirm_answer")).toEqual([]);
    expect(toast.warn).toHaveBeenCalled();
    // 输错也不行
    await fireEvent.input(screen.getByTestId("agent-strong-input"), { target: { value: "确定" } });
    await fireEvent.click(screen.getByTestId("agent-confirm-yes"));
    expect(calls.filter((c) => c.cmd === "agent_confirm_answer")).toEqual([]);
    // 逐字输对才放行
    await fireEvent.input(screen.getByTestId("agent-strong-input"), { target: { value: "确认" } });
    await fireEvent.click(screen.getByTestId("agent-confirm-yes"));
    await waitFor(() =>
      expect(calls.filter((c) => c.cmd === "agent_confirm_answer")).toHaveLength(1),
    );
    expect(calls.find((c) => c.cmd === "agent_confirm_answer")!.args).toMatchObject({
      requestId: 7,
      approved: true,
    });
  });

  it("普通确认（write）不要求输入——手势成本只加在 dangerous 上", async () => {
    render(AgentPanel, { props: { open: true, sessionId: "s1" } });
    await emit("agent:confirm", confirmPayload({ tier: "write", strong: false, action: "touch /tmp/x" }));
    await waitFor(() => expect(screen.getByTestId("agent-confirm")).toBeTruthy());
    expect(screen.queryByTestId("agent-strong-input")).toBeNull();
    await fireEvent.click(screen.getByTestId("agent-confirm-yes"));
    await waitFor(() =>
      expect(calls.filter((c) => c.cmd === "agent_confirm_answer")).toHaveLength(1),
    );
  });

  it("拒绝不需要输入（拒绝永远是一次点击的事）", async () => {
    render(AgentPanel, { props: { open: true, sessionId: "s1" } });
    await emit("agent:confirm", confirmPayload());
    await waitFor(() => expect(screen.getByTestId("agent-confirm")).toBeTruthy());
    await fireEvent.click(screen.getByTestId("agent-confirm-no"));
    await waitFor(() =>
      expect(calls.filter((c) => c.cmd === "agent_confirm_answer")).toHaveLength(1),
    );
    expect(calls.find((c) => c.cmd === "agent_confirm_answer")!.args).toMatchObject({
      approved: false,
    });
  });

  it("命令原文、分级、规则 id 与中文理由都上屏（那是判断的依据）", async () => {
    render(AgentPanel, { props: { open: true, sessionId: "s1" } });
    await emit("agent:confirm", confirmPayload());
    await waitFor(() => expect(screen.getByTestId("agent-confirm")).toBeTruthy());
    expect(screen.getByTestId("agent-confirm-action").textContent).toContain("rm -rf /var/log/x");
    const tier = screen.getByTestId("agent-confirm-tier").textContent ?? "";
    expect(tier).toContain("危险");
    expect(tier).toContain("recursive_delete_at_root_or_home");
    expect(screen.getByTestId("agent-confirm").textContent).toContain("递归删除");
  });
});

describe("提问与终态", () => {
  it("ask 事件弹出提问框，回答回投 request_id", async () => {
    render(AgentPanel, { props: { open: true, sessionId: "s1" } });
    await emit("agent:ask", { request_id: 9, run_id: "run-1", question: "要用哪个用户？", deadline_secs: 60 });
    await waitFor(() => expect(screen.getByTestId("agent-ask")).toBeTruthy());
    expect(screen.getByTestId("agent-ask-question").textContent).toContain("哪个用户");
    await fireEvent.input(screen.getByTestId("agent-ask-input"), { target: { value: "deploy" } });
    await fireEvent.click(screen.getByTestId("agent-ask-send"));
    await waitFor(() => expect(calls.some((c) => c.cmd === "agent_ask_reply")).toBe(true));
    expect(calls.find((c) => c.cmd === "agent_ask_reply")!.args).toMatchObject({
      requestId: 9,
      text: "deploy",
    });
  });

  it("stopped 事件：显示停因与预算三件套，并清掉待答项", async () => {
    render(AgentPanel, { props: { open: true, sessionId: "s1" } });
    await emit("agent:confirm", confirmPayload());
    await waitFor(() => expect(screen.getByTestId("agent-confirm")).toBeTruthy());
    await emit("agent:stopped", {
      run_id: "run-1",
      reason: "连续自动命令数触顶（8/8）",
      steps_taken: 9,
      streak: 8,
      tokens_known: 1234,
      tokens_unaccounted: 2,
      tokens_charged: 9999,
      human_touchpoints: 1,
    });
    await waitFor(() => expect(screen.getByTestId("agent-finished")).toBeTruthy());
    const t = screen.getByTestId("agent-finished").textContent ?? "";
    expect(t).toContain("连续自动命令数触顶");
    expect(t).toContain("9");
    // 无账回合要说出来（不假装知道精确数）
    expect(t).toContain("2 回合无账");
    // run 结束后待答项被清掉——它们的票据后端已撤回，留着就是一个点了没反应的框
    expect(screen.queryByTestId("agent-confirm")).toBeNull();
  });
});

describe("面板关掉再开", () => {
  it("run 还在时问一句「它还在吗」——否则用户会并发起第二个 run", async () => {
    render(AgentPanel, { props: { open: true, sessionId: "s1" } });
    await fireEvent.input(screen.getByTestId("agent-task"), { target: { value: "x" } });
    await fireEvent.click(screen.getByTestId("agent-start"));
    await waitFor(() => expect(screen.getByTestId("agent-abort")).toBeTruthy());
    await waitFor(() => expect(calls.some((c) => c.cmd === "agent_is_running")).toBe(true));
    expect(calls.find((c) => c.cmd === "agent_is_running")!.args).toMatchObject({ runId: "run-1" });
  });

  it("后端说 run 已经不在了 ⇒ 按钮态回落到「开始」", async () => {
    isRunningReply = false;
    render(AgentPanel, { props: { open: true, sessionId: "s1" } });
    await fireEvent.input(screen.getByTestId("agent-task"), { target: { value: "x" } });
    await fireEvent.click(screen.getByTestId("agent-start"));
    // 启动后那次 is_running 查回 false ⇒ 回落
    await waitFor(() => expect(screen.getByTestId("agent-start")).toBeTruthy());
  });
});

describe("关着的时候", () => {
  it("不渲染、不发请求", async () => {
    render(AgentPanel, { props: { open: false, sessionId: "s1" } });
    await new Promise((r) => setTimeout(r, 20));
    expect(screen.queryByTestId("agent-panel")).toBeNull();
    expect(calls).toEqual([]);
  });
});
