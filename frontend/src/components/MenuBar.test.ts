import { describe, expect, it, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/svelte";
vi.mock("../lib/menus", async () => {
  const { writable } = await import("svelte/store");
  return { menus: writable([
    { id: "file", label: "文件", items: [
      { id: "new", label: "新建", enabled: true },
      { id: "soon", label: "开发中的项目", enabled: false, note: "M4b" },
      { id: "import", label: "导入", enabled: true },
    ] },
    { id: "view", label: "视图", items: [{ id: "theme", label: "主题", enabled: true, children: [{ id: "dark", label: "深色", enabled: true }] }] },
  ]) };
});
import MenuBar from "./MenuBar.svelte";
describe("主菜单键盘操作", () => {
  it("方向键打开和移动，End 跳到末项，Escape 回到菜单按钮", async () => {
    render(MenuBar, { onAction: vi.fn() });
    const file = screen.getByRole("menuitem", { name: "文件" });
    file.focus();
    await fireEvent.keyDown(file, { key: "ArrowDown" });
    expect(document.activeElement).toBe(screen.getByRole("menuitem", { name: "新建" }));
    await fireEvent.keyDown(document.activeElement!, { key: "End" });
    expect(document.activeElement).toBe(screen.getByRole("menuitem", { name: "导入" }));
    await fireEvent.keyDown(document.activeElement!, { key: "Escape" });
    expect(document.activeElement).toBe(file);
    expect(screen.queryByRole("menu")).toBeNull();
  });
  it("右方向键进入子菜单，左方向键返回，执行后归还焦点", async () => {
    const onAction = vi.fn();
    render(MenuBar, { onAction });
    const view = screen.getByRole("menuitem", { name: "视图" });
    await fireEvent.keyDown(view, { key: "ArrowDown" });
    const theme = screen.getByRole("menuitem", { name: /主题/ });
    await fireEvent.keyDown(theme, { key: "ArrowRight" });
    const dark = screen.getByRole("menuitem", { name: "深色", hidden: true });
    expect(document.activeElement).toBe(dark);
    await fireEvent.keyDown(dark, { key: "ArrowLeft" });
    expect(document.activeElement).toBe(theme);
    await fireEvent.click(dark);
    expect(onAction).toHaveBeenCalledWith("dark");
    expect(document.activeElement).toBe(view);
  });
  it("不展示带开发阶段说明的禁用项目，点击外部关闭菜单", async () => {
    render(MenuBar, { onAction: vi.fn() });
    await fireEvent.click(screen.getByRole("menuitem", { name: "文件" }));
    expect(screen.queryByText("开发中的项目")).toBeNull();
    await fireEvent.pointerDown(document.body);
    expect(screen.queryByRole("menu")).toBeNull();
  });
});
