import { describe, expect, it, vi } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/svelte";
import QuickCommandsDialog from "./QuickCommandsDialog.svelte";
describe("快速命令集提交", () => {
  it("允许删除最后一项并保存空列表", async () => {
    const onSave = vi.fn().mockResolvedValue(true), onClose = vi.fn();
    render(QuickCommandsDialog, { open: true, items: [{ id: "one", name: "查看时间", command: "date" }], onSave, onClose });
    await fireEvent.click(screen.getByTestId("qc-del"));
    expect((screen.getByTestId("qc-commit") as HTMLButtonElement).disabled).toBe(false);
    await fireEvent.click(screen.getByTestId("qc-commit"));
    await waitFor(() => expect(onClose).toHaveBeenCalledOnce());
    expect(onSave).toHaveBeenCalledWith([]);
  });
  it("失败保留编辑内容，重试成功才关闭", async () => {
    const onSave = vi.fn().mockResolvedValueOnce(false).mockResolvedValueOnce(true), onClose = vi.fn();
    render(QuickCommandsDialog, { open: true, items: [{ id: "one", name: "查看时间", command: "date" }], onSave, onClose });
    await fireEvent.click(screen.getByTestId("qc-commit"));
    expect(onClose).not.toHaveBeenCalled();
    expect(screen.getByRole("alert").textContent).toContain("未保存");
    expect(screen.getByText("查看时间")).toBeTruthy();
    await fireEvent.click(screen.getByTestId("qc-commit"));
    await waitFor(() => expect(onClose).toHaveBeenCalledOnce());
  });
});
