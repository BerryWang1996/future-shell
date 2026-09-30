import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/svelte";
import SearchOverlay from "./SearchOverlay.svelte";
import type { TermController } from "../lib/term";

function mockController() {
  return {
    findNext: vi.fn(() => true),
    findPrevious: vi.fn(() => true),
    clearSearch: vi.fn(),
    focus: vi.fn(),
  };
}

describe("SearchOverlay（终端内搜索，UI 规格 §2.8/§4）", () => {
  it("openSearch 显示浮层；Enter findNext / Shift+Enter findPrevious", async () => {
    const c = mockController();
    const { component } = render(SearchOverlay, { props: { controller: c as unknown as TermController } });
    component.openSearch();
    const input = await screen.findByTestId("search-input");
    await fireEvent.input(input, { target: { value: "error" } });
    await fireEvent.keyDown(input, { key: "Enter" });
    expect(c.findNext).toHaveBeenCalledWith("error");
    await fireEvent.keyDown(input, { key: "Enter", shiftKey: true });
    expect(c.findPrevious).toHaveBeenCalledWith("error");
  });

  it("Esc 关闭：clearSearch 并还焦终端", async () => {
    const c = mockController();
    const { component } = render(SearchOverlay, { props: { controller: c as unknown as TermController } });
    component.openSearch();
    const input = await screen.findByTestId("search-input");
    await fireEvent.keyDown(input, { key: "Escape" });
    expect(c.clearSearch).toHaveBeenCalled();
    expect(c.focus).toHaveBeenCalled();
    expect(screen.queryByTestId("search-input")).toBeNull();
  });

  it("R57 续搜：查询词为空 → 打开浮层录入，不调 findNext", async () => {
    const c = mockController();
    const { component } = render(SearchOverlay, { props: { controller: c as unknown as TermController } });
    component.findNextAgain(); // 空查询词
    expect(await screen.findByTestId("search-input")).not.toBeNull(); // 浮层打开待录入
    expect(c.findNext).not.toHaveBeenCalled();
  });

  it("R57 续搜：非空查询词且浮层已关 → 重开浮层并 findNext(query)", async () => {
    const c = mockController();
    const { component } = render(SearchOverlay, { props: { controller: c as unknown as TermController } });
    component.openSearch();
    const input = await screen.findByTestId("search-input");
    await fireEvent.input(input, { target: { value: "error" } });
    await fireEvent.keyDown(input, { key: "Escape" }); // 关闭但查询词保留
    component.findNextAgain();
    expect(c.findNext).toHaveBeenCalledWith("error");
    expect(await screen.findByTestId("search-input")).not.toBeNull(); // 续搜同时重开浮层
  });

  it("S274 无命中反馈：findNext 返回 false → 输入框短暂警示样式", async () => {
    const c = { ...mockController(), findNext: vi.fn(() => false) };
    const { component } = render(SearchOverlay, { props: { controller: c as unknown as TermController } });
    component.openSearch();
    const input = await screen.findByTestId("search-input");
    await fireEvent.input(input, { target: { value: "nomatch" } });
    await fireEvent.keyDown(input, { key: "Enter" });
    expect(input.classList.contains("miss")).toBe(true);
  });
});
