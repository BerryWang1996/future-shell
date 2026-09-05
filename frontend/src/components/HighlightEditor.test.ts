/**
 * HighlightEditor 组件测试（M4a 出口标准「CRUD UI 测试」；纯逻辑判据在
 * lib/highlights.test.ts 的 S319–S321）。
 */
import { describe, expect, it, vi } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/svelte";
import HighlightEditor from "./HighlightEditor.svelte";
import type { HighlightRule } from "../lib/highlights";

const rule = (over: Partial<HighlightRule> = {}): HighlightRule => ({
  id: "r1",
  name: "错误",
  pattern: "ERROR",
  kind: "literal",
  color: "#ff5555",
  alert: true,
  enabled: true,
  ...over,
});

describe("HighlightEditor（规则 CRUD）", () => {
  it("空规则时给出说明；新增后出现一行", async () => {
    render(HighlightEditor, { props: { onSave: vi.fn() } });
    expect(screen.getByTestId("highlight-editor").textContent).toContain("尚无规则");
    await fireEvent.click(screen.getByTestId("hl-add"));
    await waitFor(() => expect(screen.getAllByTestId("hl-row")).toHaveLength(1));
  });

  it("已有规则逐条渲染，字段可改并整体保存（快照式提交）", async () => {
    const onSave = vi.fn();
    render(HighlightEditor, { props: { rules: [rule()], onSave } });
    const pattern = screen.getByTestId("hl-pattern") as HTMLInputElement;
    expect(pattern.value).toBe("ERROR");
    await fireEvent.input(pattern, { target: { value: "FATAL" } });
    await fireEvent.click(screen.getByTestId("hl-save"));
    expect(onSave).toHaveBeenCalledTimes(1);
    expect(onSave.mock.calls[0][0]).toEqual([expect.objectContaining({ pattern: "FATAL" })]);
  });

  it("删除后保存不含该条", async () => {
    const onSave = vi.fn();
    render(HighlightEditor, {
      props: { rules: [rule(), rule({ id: "r2", pattern: "WARN" })], onSave },
    });
    await fireEvent.click(screen.getAllByTestId("hl-del")[0]);
    await fireEvent.click(screen.getByTestId("hl-save"));
    const saved = onSave.mock.calls[0][0] as HighlightRule[];
    expect(saved.map((r) => r.id)).toEqual(["r2"]);
  });

  it("非法正则就地显示错因——不能让用户保存一条永不生效的规则却毫无提示", async () => {
    render(HighlightEditor, { props: { rules: [rule({ kind: "regex", pattern: "(" })], onSave: vi.fn() } });
    const err = await screen.findByTestId("hl-error");
    expect(err.textContent).toContain("正则非法");
  });

  it("合法正则不显示错因（反向对照，防「恒显示错误」的假绿）", async () => {
    render(HighlightEditor, { props: { rules: [rule({ kind: "regex", pattern: "\\d+" })], onSave: vi.fn() } });
    await waitFor(() => expect(screen.queryByTestId("hl-error")).toBeNull());
  });

  it("提醒/启用两个勾选独立且随保存带出", async () => {
    const onSave = vi.fn();
    render(HighlightEditor, { props: { rules: [rule({ alert: false, enabled: true })], onSave } });
    await fireEvent.click(screen.getByTestId("hl-alert"));
    await fireEvent.click(screen.getByTestId("hl-enabled"));
    await fireEvent.click(screen.getByTestId("hl-save"));
    expect(onSave.mock.calls[0][0][0]).toMatchObject({ alert: true, enabled: false });
  });

  it("外部 rules 变化重同步草稿（保存后回传的新表要显示出来）", async () => {
    const { rerender } = render(HighlightEditor, { props: { rules: [], onSave: vi.fn() } });
    await rerender({ rules: [rule({ pattern: "DISK" })], onSave: vi.fn() });
    await waitFor(() =>
      expect((screen.getByTestId("hl-pattern") as HTMLInputElement).value).toBe("DISK"),
    );
  });
});
