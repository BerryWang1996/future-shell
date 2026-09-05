/**
 * 历史命令面板测试（M4a）。
 *
 * 重点钉「来源必须可分辨」这条：`grid`（屏幕启发式提取）可能带提示符残渣，
 * 不能与 `sent`（亲手发出的字节）同等对待——尤其不能走「回车直接重发」。
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/svelte";
import HistoryDialog from "./HistoryDialog.svelte";
import type { HistoryEntry } from "../lib/history";

const invokeMock = vi.hoisted(() =>
  vi.fn(async (_cmd: string, _args?: Record<string, unknown>): Promise<unknown> => undefined),
);
vi.mock("../lib/ipc", () => ({ invoke: invokeMock }));
const toastMock = vi.hoisted(() => ({
  info: vi.fn(),
  warn: vi.fn(),
  error: vi.fn(),
  push: vi.fn(),
  dismiss: vi.fn(),
  subscribe: vi.fn(() => () => {}),
}));
vi.mock("../lib/toast", () => ({ toast: toastMock }));

const ROWS: HistoryEntry[] = [
  { id: 1, command: "systemctl restart nginx", host: "web-01", profile_id: "p1", source: "sent", used_at: 1_700_000_000, use_count: 3 },
  { id: 2, command: "done", host: "web-01", profile_id: "p1", source: "grid", used_at: 1_699_999_000, use_count: 1 },
];

function mount(props: Record<string, unknown> = {}) {
  return render(HistoryDialog, {
    props: {
      open: true,
      sessionId: "s1",
      host: "web-01",
      onClose: vi.fn(),
      onSend: vi.fn(),
      ...props,
    },
  });
}

beforeEach(() => {
  cleanup();
  invokeMock.mockReset();
  Object.values(toastMock).forEach((f) => typeof f === "function" && f.mockReset?.());
  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd === "history_search") return ROWS;
    if (cmd === "history_scan_grid") return 4;
    return undefined;
  });
});

describe("HistoryDialog", () => {
  it("打开即检索并列出条目", async () => {
    mount();
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("history_search", { query: "", host: null }));
    await screen.findByTestId("hist-list");
    expect(screen.getByTestId("history-dialog").textContent).toContain("systemctl restart nginx");
  });

  it("两种来源在界面上可分辨（屏幕提取的必须带明显标记）", async () => {
    mount();
    await screen.findByTestId("hist-list");
    const t = screen.getByTestId("history-dialog").textContent!;
    expect(t).toContain("发出");
    expect(t).toContain("屏幕");
  });

  it("点条目把命令交给 onSend 并关闭（不自己发字节，避免两条发送路径分叉）", async () => {
    const onSend = vi.fn();
    const onClose = vi.fn();
    mount({ onSend, onClose });
    await screen.findByTestId("hist-list");
    await fireEvent.click(screen.getByTestId("hist-send-1"));
    expect(onSend).toHaveBeenCalledWith("systemctl restart nginx");
    expect(onClose).toHaveBeenCalled();
    // 面板自己不得直接调 term_input
    expect(invokeMock).not.toHaveBeenCalledWith("term_input", expect.anything());
  });

  it("回车重发亲手发出的那条", async () => {
    const onSend = vi.fn();
    mount({ onSend });
    await screen.findByTestId("hist-list");
    await fireEvent.keyDown(screen.getByTestId("history-dialog"), { key: "Enter" });
    expect(onSend).toHaveBeenCalledWith("systemctl restart nginx");
  });

  it("回车**不**重发屏幕提取的那条，而是提示先确认", async () => {
    const onSend = vi.fn();
    mount({ onSend });
    await screen.findByTestId("hist-list");
    const dlg = screen.getByTestId("history-dialog");
    await fireEvent.keyDown(dlg, { key: "ArrowDown" }); // 移到第二条（grid）
    await fireEvent.keyDown(dlg, { key: "Enter" });
    expect(onSend).not.toHaveBeenCalled();
    expect(toastMock.warn).toHaveBeenCalled();
    // 但手动点击仍可发送（用户看着点的）
    await fireEvent.click(screen.getByTestId("hist-send-2"));
    expect(onSend).toHaveBeenCalledWith("done");
  });

  it("「仅本机」把 host 传给后端", async () => {
    mount();
    await screen.findByTestId("hist-list");
    await fireEvent.click(screen.getByTestId("hist-only-host"));
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("history_search", { query: "", host: "web-01" }),
    );
  });

  it("无会话主机时「仅本机」不可用（否则勾了也筛不出东西）", async () => {
    mount({ host: "" });
    await screen.findByTestId("hist-list");
    expect((screen.getByTestId("hist-only-host") as HTMLInputElement).disabled).toBe(true);
  });

  it("从屏幕提取后刷新列表并报条数", async () => {
    mount();
    await screen.findByTestId("hist-list");
    await fireEvent.click(screen.getByTestId("hist-scan"));
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("history_scan_grid", { sessionId: "s1", maxLines: 5000 }),
    );
    await waitFor(() => expect(toastMock.info).toHaveBeenCalled());
    expect(String(toastMock.info.mock.calls[0][0])).toContain("4");
  });

  it("无会话时提取给出提示而不是静默无事发生", async () => {
    mount({ sessionId: null });
    await screen.findByTestId("hist-list");
    await fireEvent.click(screen.getByTestId("hist-scan"));
    expect(toastMock.warn).toHaveBeenCalled();
    expect(invokeMock).not.toHaveBeenCalledWith("history_scan_grid", expect.anything());
  });

  it("清空要先确认，确认后才调 history_clear", async () => {
    mount();
    await screen.findByTestId("hist-list");
    await fireEvent.click(screen.getByTestId("hist-clear"));
    await screen.findByTestId("confirm-dialog");
    expect(invokeMock).not.toHaveBeenCalledWith("history_clear", expect.anything());
    // 确认文案要说明这也是一次隐私清理
    expect(screen.getByTestId("confirm-msg").textContent).toContain("口令");
    await fireEvent.click(screen.getByTestId("confirm-ok"));
    // 单实参调用：`invoke("history_clear")` 没有第二个参数，断言里也不能写 undefined
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("history_clear"));
  });

  it("删除单条后从列表移除", async () => {
    mount();
    await screen.findByTestId("hist-list");
    await fireEvent.click(screen.getByTestId("hist-del-1"));
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("history_delete", { id: 1 }));
    await waitFor(() => expect(screen.queryByTestId("hist-send-1")).toBeNull());
  });

  it("检索失败显示原因，不显示成「历史是空的」", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "history_search") throw new Error("database is locked");
      return undefined;
    });
    mount();
    const err = await screen.findByTestId("hist-error");
    expect(err.textContent).toContain("database is locked");
    expect(screen.queryByTestId("hist-empty")).toBeNull();
  });

  it("空历史的空态文案告诉用户怎么让它有内容", async () => {
    invokeMock.mockImplementation(async (cmd: string) => (cmd === "history_search" ? [] : undefined));
    mount();
    const empty = await screen.findByTestId("hist-empty");
    expect(empty.textContent).toContain("从屏幕提取");
  });

  it("Esc 关闭", async () => {
    const onClose = vi.fn();
    mount({ onClose });
    await screen.findByTestId("hist-list");
    await fireEvent.keyDown(screen.getByTestId("history-dialog"), { key: "Escape" });
    expect(onClose).toHaveBeenCalled();
  });
});
