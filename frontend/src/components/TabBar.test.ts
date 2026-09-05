import { describe, it, expect, beforeEach, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/svelte";
import TabBar from "./TabBar.svelte";
import { tabs, activeTabId, addTab, activateTab, removeTab, transientSchemes } from "../lib/tabs";
import { SCHEMES } from "../lib/term-schemes";
import { get } from "svelte/store";

describe("TabBar", () => {
  beforeEach(() => {
    // 清空 tabs store
    const current = get(tabs);
    current.forEach((t) => removeTab(t.id));
  });

  it("renders empty state when no tabs", () => {
    const { container } = render(TabBar, { props: { onClose: vi.fn() } });
    const tabbar = container.querySelector(".tabbar");
    expect(tabbar).toBeTruthy();
    expect(container.querySelectorAll(".tab").length).toBe(0);
  });

  it("renders tabs from store", () => {
    addTab("tab1", "Session 1", "profile1", "connecting", "host1");
    addTab("tab2", "Session 2", "profile2", "connected", "host2");
    render(TabBar, { props: { onClose: vi.fn() } });
    expect(screen.getAllByRole("tab").length).toBe(2);
    expect(screen.getByText("Session 1")).toBeTruthy();
    expect(screen.getByText("Session 2")).toBeTruthy();
  });

  it("highlights active tab", () => {
    addTab("tab1", "Session 1", "profile1", "connecting", "host1");
    addTab("tab2", "Session 2", "profile2", "connected", "host2");
    activateTab("tab2");
    const { container } = render(TabBar, { props: { onClose: vi.fn() } });
    const tabs = container.querySelectorAll(".tab");
    expect(tabs[0].classList.contains("active")).toBe(false);
    expect(tabs[1].classList.contains("active")).toBe(true);
  });

  it("activates tab on click", async () => {
    addTab("tab1", "Session 1", "profile1", "connecting", "host1");
    addTab("tab2", "Session 2", "profile2", "connected", "host2");
    activateTab("tab1");
    const { container } = render(TabBar, { props: { onClose: vi.fn() } });
    const tabs = container.querySelectorAll(".tab");
    await fireEvent.click(tabs[1]);
    expect(get(activeTabId)).toBe("tab2");
  });

  it("calls onClose when close button clicked", async () => {
    const onClose = vi.fn();
    addTab("tab1", "Session 1", "profile1", "connecting", "host1");
    const { container } = render(TabBar, { props: { onClose } });
    const closeBtn = container.querySelector(".close");
    expect(closeBtn).toBeTruthy();
    await fireEvent.click(closeBtn!);
    expect(onClose).toHaveBeenCalledWith("tab1");
  });

  it("calls onClose on middle click", async () => {
    const onClose = vi.fn();
    addTab("tab1", "Session 1", "profile1", "connecting", "host1");
    const { container } = render(TabBar, { props: { onClose } });
    const tab = container.querySelector(".tab");
    expect(tab).toBeTruthy();
    // 中键点击触发 auxclick 事件，不是 click 事件
    await fireEvent(tab!, new MouseEvent("auxclick", { button: 1, bubbles: true }));
    expect(onClose).toHaveBeenCalledWith("tab1");
  });

  it("shows status indicator", () => {
    addTab("tab1", "Session 1", "profile1", "connecting", "host1");
    const { container } = render(TabBar, { props: { onClose: vi.fn() } });
    const status = container.querySelector(".status");
    expect(status).toBeTruthy();
    expect(status?.classList.contains("connecting")).toBe(true);
  });
});

/**
 * 标签拖出（M4b「标签拖出/平铺」）。
 *
 * 判据是**离开标签栏的距离**，与 Chrome / VS Code 同款。用 dragend 的
 * `dropEffect === "none"` 来判会更"精确"，但那只在拖到**窗口外**时成立——
 * 而人拖标签时大多只是往下拽到页面中间，那样就永远触发不了。
 */
describe("TabBar 标签拖出", () => {
  beforeEach(() => {
    get(tabs).forEach((t) => removeTab(t.id));
  });

  /** jsdom 里所有元素的 getBoundingClientRect 都是 0——给 .strip 一个真实的矩形。 */
  function stubStripRect(container: HTMLElement, rect: Partial<DOMRect>): void {
    const strip = container.querySelector(".strip") as HTMLElement;
    strip.getBoundingClientRect = () =>
      ({ top: 0, bottom: 32, left: 0, right: 800, width: 800, height: 32, x: 0, y: 0, toJSON: () => ({}), ...rect }) as DOMRect;
  }

  /** 造一个带坐标的 dragend。 */
  const dragEnd = (el: Element, clientX: number, clientY: number) =>
    fireEvent(el, Object.assign(new Event("dragend", { bubbles: true }), { clientX, clientY }));

  it("往下拽出标签栏 80px 以上 → 请求分离，带上 id 与标题", async () => {
    addTab("s1", "生产 web", "p1", "connected", "h1");
    const onDetach = vi.fn();
    const { container } = render(TabBar, { props: { onClose: vi.fn(), onDetach } });
    stubStripRect(container, {});

    await dragEnd(screen.getAllByRole("tab")[0], 400, 200); // bottom(32) + 80 = 112 < 200
    expect(onDetach).toHaveBeenCalledWith("s1", "生产 web");
  });

  it("在标签栏内拖动（重排）不分离", async () => {
    // 这是最常见的操作。误判成分离的话，用户每次调整标签顺序都会多出一个窗口。
    addTab("s1", "A", "p1", "connected", "h1");
    addTab("s2", "B", "p2", "connected", "h2");
    const onDetach = vi.fn();
    const { container } = render(TabBar, { props: { onClose: vi.fn(), onDetach } });
    stubStripRect(container, {});

    await dragEnd(screen.getAllByRole("tab")[0], 300, 20); // 还在标签栏里
    await dragEnd(screen.getAllByRole("tab")[0], 300, 60); // 出去了但不足 80px
    expect(onDetach).not.toHaveBeenCalled();
  });

  it("四个方向都算拖出（往上/往左/往右拽同样是拖出）", async () => {
    addTab("s1", "A", "p1", "connected", "h1");
    const onDetach = vi.fn();
    const { container } = render(TabBar, { props: { onClose: vi.fn(), onDetach } });
    stubStripRect(container, {});
    const tab = screen.getAllByRole("tab")[0];

    for (const [x, y] of [
      [400, 200],   // 下
      [400, -100],  // 上（拖到窗口标题栏之外）
      [-100, 16],   // 左
      [900, 16],    // 右
    ] as const) {
      onDetach.mockClear();
      await dragEnd(tab, x, y);
      expect(onDetach, `(${x},${y}) 应算拖出`).toHaveBeenCalledTimes(1);
    }
  });

  it("坐标是 (0,0) 时不分离——那是「拿不到坐标」，不是「拖到了左上角」", async () => {
    // 某些平台的 dragend 不带坐标。凭一个假坐标凭空开个窗口，
    // 比少分离一次糟得多。
    //
    // 夹具**必须**让标签栏离开左上角：真实布局里侧栏占着左边 240px，
    // 标签栏从那儿才开始。原点在 (0,0) 的夹具会让这条用例假绿——
    // (0,0) 落在矩形内，不分离与那道闸没有关系。变异验证抓到过这一点：
    // 把那道闸整条删掉，用例照样全绿。
    addTab("s1", "A", "p1", "connected", "h1");
    const onDetach = vi.fn();
    const { container } = render(TabBar, { props: { onClose: vi.fn(), onDetach } });
    stubStripRect(container, { left: 240, right: 1040, top: 40, bottom: 72 });

    // 先确认这个夹具下 (0,0) **确实**会被判成拖出（否则本条仍是假绿）：
    // 0 < left(240) - 80 = 160 成立。
    await dragEnd(screen.getAllByRole("tab")[0], 1, 1);
    expect(onDetach, "夹具不对：(1,1) 本该算拖出").toHaveBeenCalledTimes(1);

    onDetach.mockClear();
    await dragEnd(screen.getAllByRole("tab")[0], 0, 0);
    expect(onDetach, "(0,0) 是「拿不到坐标」，不该分离").not.toHaveBeenCalled();
  });

  it("没传 onDetach 时拖出不报错（这个 prop 是可选的）", async () => {
    addTab("s1", "A", "p1", "connected", "h1");
    const { container } = render(TabBar, { props: { onClose: vi.fn() } });
    stubStripRect(container, {});
    await expect(dragEnd(screen.getAllByRole("tab")[0], 400, 200)).resolves.not.toThrow();
  });

  it("拖出之后标签仍在（克隆，不是搬家）", async () => {
    // UI 规格 §66 的措辞是「平铺为视图克隆」。搬家的话误拖一次就要用户
    // 自己去把标签找回来；克隆的话多开的窗口关掉即可。
    addTab("s1", "A", "p1", "connected", "h1");
    const { container } = render(TabBar, { props: { onClose: vi.fn(), onDetach: vi.fn() } });
    stubStripRect(container, {});
    await dragEnd(screen.getAllByRole("tab")[0], 400, 200);
    expect(get(tabs).map((t) => t.id)).toEqual(["s1"]);
    expect(screen.getAllByRole("tab").length).toBe(1);
  });
});

/**
 * 「关闭其他标签」（M4b 出口第 7 项）。
 *
 * 留下的是**右键点中的那个**，不是当前活动的那个：用户在一个非活动标签上右键选
 * 「关闭其他」，他要留的显然是手指底下这个。按活动标签留的话，他会眼睁睁看着
 * 自己刚点的那个被关掉。
 */
describe("TabBar 关闭其他标签", () => {
  beforeEach(() => {
    get(tabs).forEach((t) => removeTab(t.id));
  });

  /** 在某个标签上右键，把菜单打开。 */
  async function ctx(index: number) {
    await fireEvent.contextMenu(screen.getAllByRole("tab")[index], { clientX: 10, clientY: 10 });
  }

  it("传出去的是右键点中的那个，而不是活动标签", async () => {
    addTab("s1", "A", "p1", "connected", "h1");
    addTab("s2", "B", "p2", "connected", "h2");
    addTab("s3", "C", "p3", "connected", "h3");
    activateTab("s1"); // 活动的是 s1
    const onCloseOthers = vi.fn();
    render(TabBar, { props: { onClose: vi.fn(), onCloseOthers } });

    await ctx(2); // 在 s3 上右键
    await fireEvent.click(screen.getByTestId("ctx-close-others"));
    expect(onCloseOthers).toHaveBeenCalledWith("s3");
  });

  it("只有一个标签时该项禁用（点了什么也不会发生的菜单项看起来像 bug）", async () => {
    addTab("s1", "A", "p1", "connected", "h1");
    const onCloseOthers = vi.fn();
    render(TabBar, { props: { onClose: vi.fn(), onCloseOthers } });

    await ctx(0);
    const btn = screen.getByTestId("ctx-close-others") as HTMLButtonElement;
    expect(btn.disabled).toBe(true);
    expect(btn.title).toContain("只有这一个");
    await fireEvent.click(btn);
    expect(onCloseOthers).not.toHaveBeenCalled();
  });

  it("两个标签起就可用（禁用条件是「少于 2」，不是「少于 3」）", async () => {
    addTab("s1", "A", "p1", "connected", "h1");
    addTab("s2", "B", "p2", "connected", "h2");
    render(TabBar, { props: { onClose: vi.fn(), onCloseOthers: vi.fn() } });
    await ctx(0);
    expect((screen.getByTestId("ctx-close-others") as HTMLButtonElement).disabled).toBe(false);
  });

  it("点完菜单就关（菜单赖着不走会挡住下一次右键）", async () => {
    addTab("s1", "A", "p1", "connected", "h1");
    addTab("s2", "B", "p2", "connected", "h2");
    render(TabBar, { props: { onClose: vi.fn(), onCloseOthers: vi.fn() } });
    await ctx(0);
    await fireEvent.click(screen.getByTestId("ctx-close-others"));
    expect(screen.queryByTestId("ctx-close-others")).toBeNull();
  });
});

/**
 * 右键临时配色（三级作用域第三级，R16）的**菜单面**（交叉审计 2026-08-25）。
 *
 * store 层（`tabs.test.ts::setTransientScheme`）早已覆盖「设置/清除/随标签释放」，
 * 但「右键子菜单真的把 **全部** 方案列出来、点一个真的落进 transientSchemes」这
 * 一层没有组件级断言——`{#each SCHEMES}` 哪天漏渲染一套、或点击没接到
 * setTransientScheme，store 测试全绿也发现不了。这里从菜单这一侧钉。
 */
describe("TabBar 右键临时配色菜单（R16）", () => {
  beforeEach(() => {
    const current = get(tabs);
    current.forEach((t) => removeTab(t.id));
  });

  async function openMenu() {
    await fireEvent.contextMenu(screen.getAllByRole("tab")[0], { clientX: 10, clientY: 10 });
  }

  it("子菜单列全当前清单里的每一套 + 「跟随默认」，一套不少", async () => {
    addTab("s1", "A", "p1", "connected", "h1");
    render(TabBar, { props: { onClose: vi.fn() } });
    await openMenu();

    // 用 menuitemcheckbox 角色数方案项：必须与 SCHEMES 清单逐一对上，不多不少。
    // 断言数量相等之外还逐个点名——「数量对但渲染的是别的 12 样东西」也要红。
    const items = screen.getAllByRole("menuitemcheckbox");
    expect(items.length).toBe(SCHEMES.length);
    for (const s of SCHEMES) {
      expect(screen.getByRole("menuitemcheckbox", { name: new RegExp(s.name) })).toBeTruthy();
    }
    // 「跟随默认」是普通 menuitem，不在 checkbox 计数里，但必须在。
    expect(screen.getByText(/跟随默认/)).toBeTruthy();
    // 做空防护：清单本身不能是空的，否则上面的循环零次也全绿。
    expect(SCHEMES.length).toBeGreaterThanOrEqual(10);
  });

  it("点一套 → 落进该标签的 transientSchemes；再点「跟随默认」→ 清除", async () => {
    addTab("s1", "A", "p1", "connected", "h1");
    render(TabBar, { props: { onClose: vi.fn() } });

    await openMenu();
    const target = SCHEMES[0];
    await fireEvent.click(screen.getByRole("menuitemcheckbox", { name: new RegExp(target.name) }));
    expect(get(transientSchemes).get("s1")).toBe(target.id);

    // 菜单点完即关；重开再点「跟随默认」清除覆盖。清除是 delete 掉键
    // （setTransientScheme(id, null) ⇒ Map.delete），故断言键不存在而非值为 null。
    await openMenu();
    await fireEvent.click(screen.getByText(/跟随默认/));
    expect(get(transientSchemes).has("s1")).toBe(false);
  });

  it("勾选标记跟着当前覆盖走：已选的那套 aria-checked 为真，其余为假", async () => {
    addTab("s1", "A", "p1", "connected", "h1");
    render(TabBar, { props: { onClose: vi.fn() } });

    await openMenu();
    const target = SCHEMES[1];
    await fireEvent.click(screen.getByRole("menuitemcheckbox", { name: new RegExp(target.name) }));

    await openMenu();
    const checked = screen.getAllByRole("menuitemcheckbox").filter(
      (el) => el.getAttribute("aria-checked") === "true",
    );
    expect(checked.length).toBe(1);
    expect(checked[0].textContent).toContain(target.name);
  });
});
