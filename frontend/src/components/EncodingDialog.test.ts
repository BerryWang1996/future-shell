import { beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, waitFor } from "@testing-library/svelte";
import EncodingDialog from "./EncodingDialog.svelte";
import SRC from "./EncodingDialog.svelte?raw";

const invokeMock = vi.hoisted(() => vi.fn(async (_cmd?: string, _args?: unknown): Promise<unknown> => undefined));
vi.mock("../lib/ipc", () => ({ invoke: invokeMock }));

const LIST: [string, string][] = [
  ["utf-8", "UTF-8（默认）"],
  ["gbk", "GBK / GB2312（简体中文）"],
  ["big5", "Big5（繁体中文）"],
];

/**
 * 换终端编码（M7.4）。
 *
 * 这个框要说清的两件事本身就是判据：**立即生效不重连**、**只作用于这一条会话**。
 * 少说一件，用户要么不敢当场试（以为要重连），要么以为已经存进档案了。
 */
describe("EncodingDialog", () => {
  const btn = (id: string) => document.querySelector(`[data-testid="${id}"]`) as HTMLButtonElement | null;

  beforeEach(() => {
    cleanup();
    invokeMock.mockReset();
    invokeMock.mockImplementation(async (cmd?: string) => (cmd === "term_encodings" ? LIST : "GBK"));
  });

  it("清单来自后端，不是前端写死的", async () => {
    render(EncodingDialog, { open: true, sessionId: "s1", current: "UTF-8" });
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("term_encodings"));
    for (const [v] of LIST) {
      await waitFor(() => expect(document.querySelector(`[data-testid="encoding-opt-${v}"]`), v).not.toBeNull());
    }
    expect(SRC).not.toMatch(/const\s+ENCODINGS\s*=\s*\[/);
  });

  it("没选之前「切换」是禁的（不替用户默认一个编码）", async () => {
    render(EncodingDialog, { open: true, sessionId: "s1" });
    await waitFor(() => expect(btn("encoding-ok")).not.toBeNull());
    expect(btn("encoding-ok")!.disabled).toBe(true);
  });

  it("选定后切换：带会话 id 与标签调后端，回报的规范名交回装配层", async () => {
    const got: string[] = [];
    render(EncodingDialog, { open: true, sessionId: "s1", onChanged: (n: string) => got.push(n) });
    await waitFor(() => expect(document.querySelector('[data-testid="encoding-opt-gbk"]')).not.toBeNull());
    await fireEvent.change(document.querySelector('[data-testid="encoding-opt-gbk"]')!);
    await fireEvent.click(btn("encoding-ok")!);
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("term_set_encoding", { sessionId: "s1", label: "gbk" }),
    );
    await waitFor(() => expect(got).toEqual(["GBK"]));
  });

  it("切换失败留在框里说清楚——关掉之后用户只会看到编码没变而不知道为什么", async () => {
    invokeMock.mockImplementation(async (cmd?: string) => {
      if (cmd === "term_encodings") return LIST;
      throw new Error("no session");
    });
    const got: string[] = [];
    render(EncodingDialog, { open: true, sessionId: "s1", onChanged: (n: string) => got.push(n) });
    await waitFor(() => expect(document.querySelector('[data-testid="encoding-opt-gbk"]')).not.toBeNull());
    await fireEvent.change(document.querySelector('[data-testid="encoding-opt-gbk"]')!);
    await fireEvent.click(btn("encoding-ok")!);
    await waitFor(() => expect(document.querySelector('[data-testid="encoding-error"]')!.textContent).toContain("no session"));
    expect(got).toEqual([]);
    // 框不关：报了错就把框收掉的话，那条错误也跟着没了
    expect(document.querySelector('[data-testid="encoding-dialog"]')).not.toBeNull();
  });

  it("当前编码要显示出来（否则用户不知道现在是什么，也就不知道该不该改）", async () => {
    render(EncodingDialog, { open: true, sessionId: "s1", current: "Big5" });
    await waitFor(() => expect(document.querySelector('[data-testid="encoding-current"]')!.textContent).toBe("Big5"));
  });

  it("文案必须说清「不需要重连」与「只作用于这一条会话」", async () => {
    render(EncodingDialog, { open: true, sessionId: "s1" });
    const text = document.querySelector('[data-testid="encoding-dialog"]')!.textContent ?? "";
    expect(text).toContain("不需要重连");
    expect(text).toContain("只作用于这一条会话");
    expect(text).toContain("会话属性");
  });

  it("Escape 取消", async () => {
    let cancelled = 0;
    render(EncodingDialog, { open: true, sessionId: "s1", onCancel: () => (cancelled += 1) });
    await waitFor(() => expect(document.querySelector('[data-testid="encoding-dialog"]')).not.toBeNull());
    await fireEvent.keyDown(document.querySelector('[data-testid="encoding-dialog"]')!, { key: "Escape" });
    expect(cancelled).toBe(1);
  });

  it("关着时什么都不渲染、也不发 IPC", () => {
    render(EncodingDialog, { open: false, sessionId: "s1" });
    expect(document.querySelector('[data-testid="encoding-dialog"]')).toBeNull();
    expect(invokeMock).not.toHaveBeenCalled();
  });
});
