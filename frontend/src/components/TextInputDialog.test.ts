/**
 * TextInputDialog 组件测试（M4a：替换原生 prompt() 的单行输入模态）。
 *
 * 钉的是「换掉 prompt 之后要有、而 prompt 给不了」的那几件：就地校验、
 * Enter 确认 / Esc 取消、校验不通过时不放行。
 */
import { describe, expect, it, vi } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/svelte";
import TextInputDialog from "./TextInputDialog.svelte";

describe("TextInputDialog", () => {
  it("中文输入法选词的 Enter 不提交表单", async () => {
    const onConfirm = vi.fn();
    render(TextInputDialog, { open: true, onConfirm, onCancel: vi.fn() });
    const input = screen.getByTestId("ti-input");
    await fireEvent.input(input, { target: { value: "生产环境" } });
    await fireEvent.keyDown(input, { key: "Enter", isComposing: true });
    expect(onConfirm).not.toHaveBeenCalled();
    await fireEvent.keyDown(input, { key: "Enter", isComposing: false });
    expect(onConfirm).toHaveBeenCalledWith("生产环境");
  });
  it("确认把输入值交出去", async () => {
    const onConfirm = vi.fn();
    render(TextInputDialog, { props: { open: true, title: "新建文件夹", onConfirm, onCancel: vi.fn() } });
    await fireEvent.input(screen.getByTestId("ti-input"), { target: { value: "工作/生产" } });
    await fireEvent.click(screen.getByTestId("ti-ok"));
    expect(onConfirm).toHaveBeenCalledWith("工作/生产");
  });

  it("Enter 等价确认、Esc 等价取消（原生 prompt 的键盘语义要保住）", async () => {
    const onConfirm = vi.fn();
    const onCancel = vi.fn();
    render(TextInputDialog, { props: { open: true, onConfirm, onCancel } });
    const input = screen.getByTestId("ti-input");
    await fireEvent.input(input, { target: { value: "abc" } });
    await fireEvent.keyDown(input, { key: "Enter" });
    expect(onConfirm).toHaveBeenCalledWith("abc");
    await fireEvent.keyDown(screen.getByTestId("text-input-dialog"), { key: "Escape" });
    expect(onCancel).toHaveBeenCalled();
  });

  it("就地校验：不通过时显示错因且确认被禁用（prompt 做不到的那一半）", async () => {
    const onConfirm = vi.fn();
    render(TextInputDialog, {
      props: {
        open: true,
        validate: (v: string) => (v.trim() ? undefined : "请输入文件夹名"),
        onConfirm,
        onCancel: vi.fn(),
      },
    });
    const err = await screen.findByTestId("ti-err");
    expect(err.textContent).toContain("请输入");
    expect((screen.getByTestId("ti-ok") as HTMLButtonElement).disabled).toBe(true);
    // Enter 也不得放行（禁用按钮只挡鼠标，键盘路径要单独挡）
    await fireEvent.keyDown(screen.getByTestId("ti-input"), { key: "Enter" });
    expect(onConfirm).not.toHaveBeenCalled();
  });

  it("每次打开都重置为 initial（复用模态不该留着上次的输入）", async () => {
    const { rerender } = render(TextInputDialog, {
      props: { open: true, initial: "first", onConfirm: vi.fn(), onCancel: vi.fn() },
    });
    await waitFor(() => expect((screen.getByTestId("ti-input") as HTMLInputElement).value).toBe("first"));
    await fireEvent.input(screen.getByTestId("ti-input"), { target: { value: "改过的" } });
    await rerender({ open: false, initial: "first", onConfirm: vi.fn(), onCancel: vi.fn() });
    await rerender({ open: true, initial: "second", onConfirm: vi.fn(), onCancel: vi.fn() });
    await waitFor(() => expect((screen.getByTestId("ti-input") as HTMLInputElement).value).toBe("second"));
  });
});
