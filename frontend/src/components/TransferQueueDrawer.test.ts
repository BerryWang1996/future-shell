/**
 * 传输队列抽屉的「全部取消」（M4a）。
 *
 * 钉三件容易做错、错了还看起来对的事：
 * ① 取消前必须确认（取消会留下 .fspart 半成品，而用户可能只想停一件）；
 * ② 部分失败时不能报「已取消」——那是假话，且掩盖了「哪几件没停下来」；
 * ③ 不乐观改状态：取消是请求，终态由后端事件带回来。
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/svelte";
import TransferQueueDrawer from "./TransferQueueDrawer.svelte";
import { rows, drawerOpen, type Row } from "../lib/transfers";

const invokeMock = vi.hoisted(() =>
  vi.fn(async (_cmd: string, _args?: Record<string, unknown>): Promise<unknown> => undefined),
);
// subscribe 的形参必须写出来：组件是 `settingChanged.subscribe((ev) => …)`，
// 零参数的 mock 类型对不上（vitest 跑得过，svelte-check 报错）。
const settingChangedMock = vi.hoisted(() => ({
  subscribe: vi.fn((_run: (value: unknown) => void) => () => {}),
}));
vi.mock("../lib/ipc", () => ({
  invoke: invokeMock,
  settingChanged: settingChangedMock,
  settingGet: vi.fn(async (_k: string, fallback: unknown) => fallback),
  settingSet: vi.fn(async () => {}),
}));
const toastMock = vi.hoisted(() => ({
  info: vi.fn(),
  warn: vi.fn(),
  error: vi.fn(),
  push: vi.fn(),
  dismiss: vi.fn(),
  subscribe: vi.fn(() => () => {}),
}));
vi.mock("../lib/toast", () => ({ toast: toastMock }));
// 事件订阅由 lib/transfers 持有；本测试直接写 store，不需要真的 listen
vi.mock("@tauri-apps/api/event", () => ({ listen: () => Promise.resolve(() => {}) }));

const mk = (id: number, state: string, sessionId = "s1"): Row => ({
  id,
  direction: "up",
  local: `C:/l/${id}.bin`,
  remote: `/srv/${id}.bin`,
  sessionId,
  done: 10,
  total: 100,
  state,
  startedAt: Date.now(),
});

function seed(...rs: Row[]) {
  rows.set(new Map(rs.map((r) => [r.id, r])));
}

beforeEach(() => {
  cleanup();
  invokeMock.mockReset();
  invokeMock.mockImplementation(async () => undefined);
  Object.values(toastMock).forEach((f) => f.mockReset?.());
  rows.set(new Map());
  drawerOpen.set(true);
});

const btn = () => screen.getByTestId("transfer-cancel-all") as HTMLButtonElement;

describe("TransferQueueDrawer 全部取消", () => {
  it("按钮显示在途件数", async () => {
    seed(mk(1, "Queued"), mk(2, "Running"), mk(3, "Done"));
    render(TransferQueueDrawer);
    await waitFor(() => expect(btn()).toBeTruthy());
    expect(btn().textContent).toContain("2");
  });

  it("无在途作业时禁用而不是隐藏（时隐时现会让人以为界面坏了）", async () => {
    seed(mk(1, "Done"), mk(2, "Cancelled"));
    render(TransferQueueDrawer);
    await waitFor(() => expect(btn()).toBeTruthy());
    expect(btn().disabled).toBe(true);
    expect(btn().title).toContain("没有在途传输");
  });

  it("先弹确认，取消确认则不下发任何 transfer_cancel", async () => {
    seed(mk(1, "Running"));
    render(TransferQueueDrawer);
    await fireEvent.click(btn());
    await screen.findByTestId("confirm-dialog");
    await fireEvent.click(screen.getByTestId("confirm-cancel"));
    expect(invokeMock).not.toHaveBeenCalledWith("transfer_cancel", expect.anything());
  });

  it("确认文案说明会留下 .fspart 半成品，并在跨会话时报出会话数", async () => {
    seed(mk(1, "Running", "s1"), mk(2, "Queued", "s2"));
    render(TransferQueueDrawer);
    await fireEvent.click(btn());
    const msg = await screen.findByTestId("confirm-msg");
    expect(msg.textContent).toContain(".fspart");
    expect(msg.textContent).toContain("2 个会话");
  });

  it("确认后逐件下发（带各自的 sessionId），全成功报件数", async () => {
    seed(mk(10, "Running", "s1"), mk(20, "Queued", "s2"), mk(30, "Done", "s1"));
    render(TransferQueueDrawer);
    await fireEvent.click(btn());
    await fireEvent.click(await screen.findByTestId("confirm-ok"));
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("transfer_cancel", { sessionId: "s1", id: 10 }),
    );
    expect(invokeMock).toHaveBeenCalledWith("transfer_cancel", { sessionId: "s2", id: 20 });
    // 终态行不该被下发
    expect(invokeMock).not.toHaveBeenCalledWith("transfer_cancel", { sessionId: "s1", id: 30 });
    await waitFor(() => expect(toastMock.info).toHaveBeenCalled());
    expect(String(toastMock.info.mock.calls[0][0])).toContain("2");
  });

  it("部分失败时报「N 已请求 / M 失败」并点名失败的 id，而不是一句「已取消」", async () => {
    seed(mk(1, "Running"), mk(2, "Running"), mk(3, "Running"));
    invokeMock.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
      if (cmd === "transfer_cancel" && args?.id === 2) throw new Error("会话已关闭");
      return undefined;
    });
    render(TransferQueueDrawer);
    await fireEvent.click(btn());
    await fireEvent.click(await screen.findByTestId("confirm-ok"));
    await waitFor(() => expect(toastMock.error).toHaveBeenCalled());
    const msg = String(toastMock.error.mock.calls[0][0]);
    expect(msg).toContain("2 已请求");
    expect(msg).toContain("1 失败");
    expect(msg).toContain("#2");
    expect(toastMock.info).not.toHaveBeenCalled();
  });

  it("一件失败不打断其余（第 1 件失败，第 2、3 件仍下发）", async () => {
    seed(mk(1, "Running"), mk(2, "Running"), mk(3, "Running"));
    invokeMock.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
      if (cmd === "transfer_cancel" && args?.id === 1) throw new Error("boom");
      return undefined;
    });
    render(TransferQueueDrawer);
    await fireEvent.click(btn());
    await fireEvent.click(await screen.findByTestId("confirm-ok"));
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("transfer_cancel", { sessionId: "s1", id: 3 }),
    );
  });

  it("不乐观改状态——取消是请求，终态由后端事件带回来", async () => {
    seed(mk(1, "Running"));
    render(TransferQueueDrawer);
    await fireEvent.click(btn());
    await fireEvent.click(await screen.findByTestId("confirm-ok"));
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("transfer_cancel", { sessionId: "s1", id: 1 }));
    // 行仍是 Running：把它改成「已取消」会让用户看到一个还在涨进度条的「已取消」行
    const { get } = await import("svelte/store");
    expect(get(rows).get(1)!.state).toBe("Running");
  });
});
