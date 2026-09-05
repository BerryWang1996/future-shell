/**
 * KeymapEditor 组件测试（M4a 键盘配置文件的 UI 侧；纯函数判据在 lib/keymap.test.ts）。
 *
 * 只钉三件 UI 独有的事：录键把「用户按了什么」翻译成规范键位、S314 安全闸在录键
 * 时拦下不可绑定组合、保存把整份草稿交给装配层。导入导出的 round-trip 判据在
 * lib 层（本组件只是把 JSON 递进去/取出来）。
 */
import { describe, expect, it, vi } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/svelte";
import KeymapEditor from "./KeymapEditor.svelte";

describe("KeymapEditor（录键与保存）", () => {
  it("默认表逐行渲染，用中文动作名称说明键位用途", async () => {
    render(KeymapEditor, { props: { onSave: vi.fn() } });
    const table = screen.getByTestId("keymap-editor");
    expect(table.textContent).toContain("新建会话");
    expect(table.textContent).toContain("Ctrl+N");
    expect(table.textContent).toContain("最近使用的上一个标签");
    expect(table.textContent).toContain("Ctrl+Shift+Tab");
  });

  it("录键：按下 Ctrl+Alt+K 后该动作改绑到规范键位，保存把整份草稿交出", async () => {
    const onSave = vi.fn();
    render(KeymapEditor, { props: { onSave } });
    await fireEvent.click(screen.getByTestId("km-record-session.new"));
    // 纯修饰键先按下不成组合（等主键）
    await fireEvent.keyDown(window, { key: "Control", ctrlKey: true });
    await fireEvent.keyDown(window, { key: "k", ctrlKey: true, altKey: true });
    await waitFor(() =>
      expect(screen.getByTestId("keymap-editor").textContent).toContain("Ctrl+Alt+K"),
    );
    await fireEvent.click(screen.getByTestId("km-save"));
    expect(onSave).toHaveBeenCalledTimes(1);
    expect(onSave.mock.calls[0][0]).toMatchObject({ "Ctrl+Alt+K": "session.new" });
  });

  it("S314：录到无修饰字符键时拒绝并给出理由（绑了它终端里就打不出这个字符）", async () => {
    const onSave = vi.fn();
    render(KeymapEditor, { props: { onSave } });
    await fireEvent.click(screen.getByTestId("km-record-edit.find"));
    await fireEvent.keyDown(window, { key: "a" });
    const hint = await screen.findByTestId("km-hint");
    expect(hint.textContent).toContain("不可绑定");
    // 仍在录键态（没有把坏绑定写进草稿）
    await fireEvent.click(screen.getByTestId("km-save"));
    expect(onSave.mock.calls[0][0]).not.toHaveProperty("A");
  });

  it("Esc 取消录键，不改动草稿", async () => {
    const onSave = vi.fn();
    render(KeymapEditor, { props: { onSave } });
    await fireEvent.click(screen.getByTestId("km-record-edit.find"));
    await fireEvent.keyDown(window, { key: "Escape" });
    await fireEvent.click(screen.getByTestId("km-save"));
    expect(onSave.mock.calls[0][0]).toEqual({});
  });

  it("解绑默认键写入 null（显式覆盖），全部复位清空草稿", async () => {
    const onSave = vi.fn();
    render(KeymapEditor, { props: { onSave } });
    const table = screen.getByTestId("keymap-editor");
    // 找到 Ctrl+N 那颗解绑按钮（键位徽标内的 ✕）
    const kbd = [...table.querySelectorAll(".kbd")].find((el) => el.textContent?.includes("Ctrl+N"));
    await fireEvent.click(kbd!.querySelector("button")!);
    await fireEvent.click(screen.getByTestId("km-save"));
    expect(onSave.mock.calls[0][0]).toMatchObject({ "Ctrl+N": null });

    await fireEvent.click(screen.getByTestId("km-reset-all"));
    await fireEvent.click(screen.getByTestId("km-save"));
    expect(onSave.mock.calls[1][0]).toEqual({});
  });

  it("外部 user 表变化时草稿重同步（保存后回传的新表必须显示出来）", async () => {
    const { rerender } = render(KeymapEditor, {
      props: { user: {}, onSave: vi.fn() },
    });
    await rerender({ user: { "Ctrl+Alt+J": "edit.find" }, onSave: vi.fn() });
    await waitFor(() =>
      expect(screen.getByTestId("keymap-editor").textContent).toContain("Ctrl+Alt+J"),
    );
  });
});
