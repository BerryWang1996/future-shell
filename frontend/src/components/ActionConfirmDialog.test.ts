/**
 * 危险动作确认框（路线图 M7.2 出口标准①「把将要执行的命令全文摊给用户看，不是『确定吗？』」）。
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import { get } from "svelte/store";

vi.mock("../lib/ipc", () => ({
  invoke: vi.fn(async () => undefined),
  settingGet: async <T>(_k: string, fb: T) => fb,
  settingSet: vi.fn(async () => {}),
  reportFrontendError: vi.fn(),
}));

import ActionConfirmDialog from "./ActionConfirmDialog.svelte";
import { pendingConfirm, resetConfirmGateForTest } from "../lib/confirm-gate";

const LONG = "systemctl restart nginx.service && journalctl -u nginx -n 50 --no-pager";
const answer = vi.fn();

function open(over: Record<string, unknown> = {}) {
  pendingConfirm.set({
    kind: "service.restart",
    title: "重启服务",
    command: LONG,
    note: "重启期间该服务不可用。",
    danger: true,
    sessionId: "s1",
    answer,
    ...over,
  } as never);
  render(ActionConfirmDialog);
}

beforeEach(() => {
  answer.mockClear();
  resetConfirmGateForTest();
});
afterEach(() => cleanup());

describe("出口标准①：命令全文", () => {
  it("命令原样、完整、可选中地摊在框里（不是「确定吗？」）", () => {
    open();
    const pre = screen.getByTestId("action-confirm-command");
    expect(pre.textContent).toBe(LONG); // 逐字，不截断不摘要
    expect(getComputedStyle(pre).userSelect || "text").not.toBe("none");
  });

  it("后果说明独立成段，不和命令混在一起", () => {
    open();
    expect(screen.getByTestId("action-confirm-note").textContent).toContain("不可用");
  });

  it("没有后果说明时不留一个空段落", () => {
    open({ note: undefined });
    expect(screen.queryByTestId("action-confirm-note")).toBeNull();
  });
});

describe("出口标准②：勾选项要写清楚放行的是哪一类", () => {
  it("勾选文案带类别名（「以后不再显示」四个字单独出现时，用户不知道自己放行多大一片）", () => {
    open();
    const label = screen.getByTestId("action-confirm-suppress").closest("label");
    expect(label!.textContent).toContain("重启服务");
  });

  it("勾了再确认 → answer(true, true)", async () => {
    open();
    await fireEvent.click(screen.getByTestId("action-confirm-suppress"));
    await fireEvent.click(screen.getByTestId("action-confirm-ok"));
    expect(answer).toHaveBeenCalledWith(true, true);
  });

  it("没勾就确认 → answer(true, false)", async () => {
    open();
    await fireEvent.click(screen.getByTestId("action-confirm-ok"));
    expect(answer).toHaveBeenCalledWith(true, false);
  });

  it("勾了却按取消 → answer(false, false)：拒绝的同时放行它自相矛盾", async () => {
    open();
    await fireEvent.click(screen.getByTestId("action-confirm-suppress"));
    await fireEvent.click(screen.getByTestId("action-confirm-cancel"));
    expect(answer).toHaveBeenCalledWith(false, false);
  });
});

describe("出口标准③：勾完能去哪儿反悔，当场说", () => {
  it("框里指明撤销入口在「工具 → 选项 → 安全与 Vault」", () => {
    open();
    expect(screen.getByTestId("action-confirm").textContent).toContain("安全与 Vault");
  });
});

describe("交互纪律", () => {
  it("Escape = 取消", async () => {
    open();
    await fireEvent.keyDown(screen.getByTestId("action-confirm"), { key: "Escape" });
    expect(answer).toHaveBeenCalledWith(false, false);
  });

  it("换一条待确认时勾选复位（上一条勾过不代表这一条也要勾——那是两个类别）", async () => {
    open();
    await fireEvent.click(screen.getByTestId("action-confirm-suppress"));
    expect((screen.getByTestId("action-confirm-suppress") as HTMLInputElement).checked).toBe(true);
    pendingConfirm.set({ kind: "process.kill", title: "终止进程", command: "kill -9 1", answer } as never);
    await Promise.resolve();
    expect((screen.getByTestId("action-confirm-suppress") as HTMLInputElement).checked).toBe(false);
  });

  it("没有待确认时不渲染任何东西", () => {
    render(ActionConfirmDialog);
    expect(screen.queryByTestId("action-confirm")).toBeNull();
  });
});
