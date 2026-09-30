import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { render, screen, fireEvent, waitFor, cleanup } from "@testing-library/svelte";

/** 跨「重启」存活的假 settings 库——出口原文要「重启保持」。 */
const store = new Map<string, unknown>();
const writes: { key: string; value: unknown }[] = [];

vi.mock("../lib/ipc", () => ({
  settingGet: async <T>(key: string, fallback: T): Promise<T> =>
    store.has(key) ? (store.get(key) as T) : fallback,
  settingSet: async (key: string, value: unknown): Promise<void> => {
    writes.push({ key, value });
    store.set(key, value);
  },
  invoke: vi.fn(async () => null),
}));
vi.mock("../lib/theme/store", () => ({
  setTheme: vi.fn(async () => {}),
  themeId: { subscribe: (fn: (v: string) => void) => (fn("obsidian"), () => {}) },
}));

import ToolBar from "./ToolBar.svelte";

const KEY = "ui.toolbarLayout";

beforeEach(() => {
  store.clear();
  writes.length = 0;
});
afterEach(() => cleanup());

/** 挂载并等 onMount 的 settingGet 落地。 */
async function mount(onAction = vi.fn(), extra: Record<string, unknown> = {}) {
  render(ToolBar, { props: { onAction, ...extra } });
  await new Promise((r) => setTimeout(r, 0));
  return onAction;
}

const shownIds = (): string[] =>
  Array.from(document.querySelectorAll<HTMLElement>('[data-testid^="tbtn-"]')).map(
    (e) => e.dataset.testid!.slice("tbtn-".length),
  );

describe("工具栏默认形态", () => {
  it("按钮都在，顺序是默认顺序", async () => {
    await mount();
    const ids = shownIds();
    expect(ids[0]).toBe("session.new");
    expect(ids).toContain("tools.settings");
    expect(ids.length).toBeGreaterThanOrEqual(8);
  });

  /**
   * 截图按钮此前一直是禁用态、tip 写着「即将推出 Phase 4b」——而 M4b 的截图
   * （`lib/screenshot.ts`）早就做完了。功能做完却没人回来解禁按钮，是很典型的一处遗漏。
   */
  it("截图按钮已解禁，且指向剪贴板那一路", async () => {
    await mount();
    const btn = screen.getByTestId("tbtn-edit.screenshotCopy") as HTMLButtonElement;
    expect(btn.disabled).toBe(false);
    expect(btn.title).not.toContain("即将推出");
  });

  it("点按钮把 id 发给 onAction", async () => {
    const onAction = await mount();
    await fireEvent.click(screen.getByTestId("tbtn-tools.settings"));
    expect(onAction).toHaveBeenCalledWith("tools.settings");
  });
});

describe("右键菜单：勾选显示哪些按钮（出口第 17 项）", () => {
  it("右键工具栏打开菜单", async () => {
    await mount();
    await fireEvent.contextMenu(screen.getByTestId("toolbar"), { clientX: 10, clientY: 10 });
    expect(screen.getByTestId("toolbar-menu")).toBeTruthy();
  });

  /**
   * 菜单挂在**容器**上而不是逐个按钮：用户想显示一个已经隐藏的项时，
   * 那个按钮不在页面上，没法右键它——菜单必须能从别处打开，且要列出**全部**项。
   */
  it("菜单列出全部项，包括已经隐藏的", async () => {
    store.set(KEY, { hidden: ["tools.settings"], order: [] });
    await mount();
    expect(shownIds()).not.toContain("tools.settings");
    await fireEvent.contextMenu(screen.getByTestId("toolbar"), { clientX: 10, clientY: 10 });
    // 隐藏了的那个仍在菜单里，且是未勾选态——否则它就再也显示不回来了
    const t = screen.getByTestId("tb-toggle-tools.settings");
    expect(t).toBeTruthy();
    expect(t.getAttribute("aria-checked")).toBe("false");
  });

  it("勾掉一项 → 该项即时消失 + 落库", async () => {
    await mount();
    expect(shownIds()).toContain("edit.find");
    await fireEvent.contextMenu(screen.getByTestId("toolbar"), { clientX: 10, clientY: 10 });
    await fireEvent.click(screen.getByTestId("tb-toggle-edit.find"));

    await waitFor(() => expect(shownIds()).not.toContain("edit.find"));
    expect(store.get(KEY)).toEqual({ hidden: ["edit.find"], order: [] });
  });

  it("勾回来 → 该项重新出现", async () => {
    store.set(KEY, { hidden: ["edit.find"], order: [] });
    await mount();
    expect(shownIds()).not.toContain("edit.find");
    await fireEvent.contextMenu(screen.getByTestId("toolbar"), { clientX: 10, clientY: 10 });
    await fireEvent.click(screen.getByTestId("tb-toggle-edit.find"));
    await waitFor(() => expect(shownIds()).toContain("edit.find"));
  });

  it("「恢复默认」清掉自定义", async () => {
    store.set(KEY, { hidden: ["edit.find", "edit.copy"], order: ["tools.settings"] });
    await mount();
    await fireEvent.contextMenu(screen.getByTestId("toolbar"), { clientX: 10, clientY: 10 });
    await fireEvent.click(screen.getByTestId("tb-reset"));
    await waitFor(() => expect(shownIds()).toContain("edit.find"));
    expect(store.get(KEY)).toEqual({ hidden: [], order: [] });
    // 菜单顺手关掉——「恢复默认」之后还开着的菜单会让人以为没生效
    expect(screen.queryByTestId("toolbar-menu")).toBeNull();
  });

  it("点别处关菜单", async () => {
    await mount();
    await fireEvent.contextMenu(screen.getByTestId("toolbar"), { clientX: 10, clientY: 10 });
    await fireEvent.click(document.querySelector(".tb-scrim")!);
    await waitFor(() => expect(screen.queryByTestId("toolbar-menu")).toBeNull());
  });
});

describe("拖拽排序（出口第 17 项）", () => {
  /** 造一次 HTML5 拖放。 */
  async function dragTo(fromId: string, toId: string) {
    await fireEvent.dragStart(screen.getByTestId(`tbtn-${fromId}`));
    await fireEvent.drop(screen.getByTestId(`tbtn-${toId}`));
  }

  it("拖过去之后顺序变了，并落库", async () => {
    await mount();
    const before = shownIds();
    await dragTo("tools.settings", "edit.copy");
    await waitFor(() => expect(shownIds()).not.toEqual(before));

    const ids = shownIds();
    expect(ids.indexOf("tools.settings")).toBe(ids.indexOf("edit.copy") - 1);
    const saved = store.get(KEY) as { order: string[] };
    expect(saved.order[saved.order.indexOf("edit.copy") - 1]).toBe("tools.settings");
  });

  /** 出口原文点名「**重启保持**」。 */
  it("重启后顺序仍是上次拖出来的那个", async () => {
    await mount();
    await dragTo("tools.settings", "edit.copy");
    await waitFor(() => {
      const ids = shownIds();
      return expect(ids.indexOf("tools.settings")).toBe(ids.indexOf("edit.copy") - 1);
    });
    const after = shownIds();

    cleanup(); // ← 「退出应用」：组件没了，store 还在
    await mount();
    expect(shownIds()).toEqual(after);
  });

  it("重启后隐藏项仍是隐藏的", async () => {
    await mount();
    await fireEvent.contextMenu(screen.getByTestId("toolbar"), { clientX: 10, clientY: 10 });
    await fireEvent.click(screen.getByTestId("tb-toggle-edit.find"));
    await waitFor(() => expect(shownIds()).not.toContain("edit.find"));

    cleanup();
    await mount();
    expect(shownIds()).not.toContain("edit.find");
  });

  it("拖到自己身上不产生写入", async () => {
    await mount();
    writes.length = 0;
    await dragTo("edit.copy", "edit.copy");
    await new Promise((r) => setTimeout(r, 0));
    expect(writes).toEqual([]);
  });
});

describe("库里是脏值时", () => {
  /**
   * 脏值回落空布局，**不是**「全部隐藏」。把 hidden 解析错的实现会让整条工具栏消失，
   * 而用户完全不知道发生了什么。
   */
  it("整条工具栏仍然完整", async () => {
    for (const dirty of ["不是对象", 42, { hidden: "a" }, { order: null }, []]) {
      store.set(KEY, dirty);
      await mount();
      expect(shownIds().length, JSON.stringify(dirty)).toBeGreaterThanOrEqual(8);
      cleanup();
    }
  });
});

/* 路线图 4c「overlay 形态的侧栏缺开启入口」（2026-09-02） */
describe("overlay 形态的侧栏叫回按钮", () => {
  it("overlaySidebar=true → 按钮在最前、点它发 view.sidebar；浮层展开时有按压态", async () => {
    const onAction = await mount(vi.fn(), { overlaySidebar: true, overlayOpen: true });
    const btn = screen.getByTestId("tbtn-view.sidebar");
    expect(shownIds()[0]).toBe("view.sidebar");
    expect(btn.getAttribute("aria-pressed")).toBe("true");
    await fireEvent.click(btn);
    expect(onAction).toHaveBeenCalledWith("view.sidebar");
  });

  it("停靠形态不显示：侧栏就在版面里，视图菜单已够用", async () => {
    await mount();
    expect(screen.queryByTestId("tbtn-view.sidebar")).toBeNull();
  });

  it("不进右键「显示这些按钮」菜单：它是收起后唯一的鼠标入口，藏掉就只剩快捷键一条暗路", async () => {
    await mount(vi.fn(), { overlaySidebar: true });
    await fireEvent.contextMenu(screen.getByTestId("toolbar"), { clientX: 10, clientY: 10 });
    expect(screen.queryByTestId("tb-toggle-view.sidebar")).toBeNull();
  });

  it("库里 hidden 即便写了 view.sidebar 也照样显示（不受自定义布局管辖）", async () => {
    store.set(KEY, { hidden: ["view.sidebar"], order: [] });
    await mount(vi.fn(), { overlaySidebar: true });
    expect(screen.getByTestId("tbtn-view.sidebar")).toBeTruthy();
  });
});
