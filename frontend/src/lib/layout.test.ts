import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";
// App 源文本（同 menus.test.ts 的既有口径）：下面那组测的是「装配写对了没有」，
// 而整体渲染 App 要桩掉几十个依赖，代价远大于收益。
import APP_SRC from "../App.svelte?raw";

vi.mock("./ipc", () => ({
  settingGet: vi.fn(async (_k: string, fb: unknown) => fb),
  settingSet: vi.fn(async () => {}),
  invoke: vi.fn(async () => null),
  listen: vi.fn(async () => () => {}),
  openExternal: vi.fn(async () => {}),
}));

import {
  SIDEBAR_DEFAULT, WIDE_SCREEN_QUERY, composeVisible, effectivePresentation, initResponsive,
  isSidebarDock, monitorOpen, narrowScreen, setSidebarWidth, sidebarOverlayOpen,
  sidebarVisible, sidebarWidth, statusVisible, toggle, toggleSidebarFor, toolbarVisible,
} from "./layout";

describe("layout store（UI 规格 §1.1）", () => {
  beforeEach(() => {
    sidebarVisible.set(true); sidebarWidth.set(SIDEBAR_DEFAULT);
    toolbarVisible.set(true); composeVisible.set(true);
    statusVisible.set(true); monitorOpen.set(false);
  });
  it("默认：侧栏左停靠可见、240px；监控抽屉折叠", () => {
    expect(get(sidebarVisible)).toBe(true);
    expect(get(sidebarWidth)).toBe(240);
    expect(get(monitorOpen)).toBe(false);
  });
  it("toggle 翻转各面板", () => {
    toggle("sidebar"); expect(get(sidebarVisible)).toBe(false);
    toggle("monitor"); expect(get(monitorOpen)).toBe(true);
    toggle("compose"); expect(get(composeVisible)).toBe(false);
    toggle("toolbar"); expect(get(toolbarVisible)).toBe(false);
    toggle("status"); expect(get(statusVisible)).toBe(false);
  });
  it("侧栏宽度钳位 160–480", () => {
    setSidebarWidth(100); expect(get(sidebarWidth)).toBe(160);
    setSidebarWidth(999); expect(get(sidebarWidth)).toBe(480);
    setSidebarWidth(300); expect(get(sidebarWidth)).toBe(300);
  });
  it("S263：脏持久值（非有限）回落默认，不让 NaN 流进 style:width", () => {
    // settingGet 只做 JSON.parse + as T 断言，字符串/对象/未定义都能抵达此处；
    // 无守卫时 Math.round → NaN，两道钳位吃不掉 NaN，最终写出非法的 width:NaNpx。
    for (const bad of [NaN, Infinity, -Infinity, "abc", {}, undefined]) {
      sidebarWidth.set(300);
      setSidebarWidth(bad as unknown as number);
      expect(get(sidebarWidth), String(bad)).toBe(SIDEBAR_DEFAULT);
    }
    // Number.isFinite 不做隐式转换（区别于全局 isFinite）：JSON null 同样判非法、回落默认，
    // 不走 Math.round(null)=0 → 钳到 160 的老路径。此处钉死的正是「不隐式转换」这一取舍。
    sidebarWidth.set(300);
    setSidebarWidth(null as unknown as number);
    expect(get(sidebarWidth)).toBe(SIDEBAR_DEFAULT);
  });
});

/* ── 侧栏停靠 / 浮动 / 窄屏覆盖层（M4b 出口第 16、18 项） ─────────────────── */

describe("停靠偏好 + 屏宽 → 实际呈现", () => {
  it("宽屏下就是用户选的那一侧", () => {
    expect(effectivePresentation("left", false)).toBe("left");
    expect(effectivePresentation("right", false)).toBe("right");
  });

  it("用户主动选浮动时，宽屏下也是覆盖层", () => {
    expect(effectivePresentation("float", false)).toBe("overlay");
  });

  /**
   * 窄屏一律覆盖，**无论用户选的是哪一侧**。
   *
   * 1100px 以下留给侧栏 240px 之后，终端只剩不到 860px——那时候「停靠」的意思是
   * 「终端太窄没法用」。
   */
  it("窄屏下三种偏好都变覆盖层", () => {
    for (const dock of ["left", "right", "float"] as const) {
      expect(effectivePresentation(dock, true), dock).toBe("overlay");
    }
  });

  /**
   * 关键的一条：窄屏**不改写**偏好本身。
   *
   * 把窄屏写回 dock 会让「拉窄一次就永久变成浮动」——而用户没做过这个选择。
   * 这也是 `float` 与「窄屏覆盖」必须是两回事的原因：前者是选择，后者是当前处境。
   */
  it("窄屏是仲裁结果，不是把偏好改掉", () => {
    // 同一个偏好在两种屏宽下给出不同结果，且偏好本身是入参、函数不改它
    expect(effectivePresentation("right", true)).toBe("overlay");
    expect(effectivePresentation("right", false)).toBe("right");
  });
});

describe("停靠侧的合法值", () => {
  it("只认三个值", () => {
    for (const ok of ["left", "right", "float"]) expect(isSidebarDock(ok)).toBe(true);
    for (const bad of ["LEFT", "top", "", null, undefined, 0, {}]) {
      expect(isSidebarDock(bad), String(bad)).toBe(false);
    }
  });
});

describe("窄屏断点用「宽屏」做正判据", () => {
  it("查询是 min-width: 1100px", () => {
    // 出口原文：「≤1099px 转覆盖层……≥1100px 恢复停靠」。
    // 写成 max-width: 1099px 也能得到同一组整数，但小数 DPI 缩放下 1099.5px
    // 会两边都不满足（CSS 的 min/max-width 都是闭区间）。
    // 用「宽屏」做正判据、窄屏取补集，就没有这个缝。
    expect(WIDE_SCREEN_QUERY).toBe("(min-width: 1100px)");
  });
});

describe("matchMedia 接线", () => {
  /** 可驱动的 matchMedia 替身。 */
  function stubMatchMedia(initialMatches: boolean) {
    const listeners = new Set<(e: MediaQueryListEvent) => void>();
    let removed = 0;
    const mql = {
      matches: initialMatches,
      addEventListener: (_: string, fn: (e: MediaQueryListEvent) => void) => listeners.add(fn),
      removeEventListener: (_: string, fn: (e: MediaQueryListEvent) => void) => {
        listeners.delete(fn);
        removed++;
      },
    };
    vi.stubGlobal("matchMedia", () => mql);
    (window as unknown as { matchMedia: unknown }).matchMedia = () => mql;
    return {
      fire: (matches: boolean) => {
        mql.matches = matches;
        for (const fn of listeners) fn({ matches } as MediaQueryListEvent);
      },
      get removed() {
        return removed;
      },
      get listenerCount() {
        return listeners.size;
      },
    };
  }

  afterEach(() => {
    vi.unstubAllGlobals();
    narrowScreen.set(false);
    sidebarOverlayOpen.set(false);
  });

  it("挂载时立刻按当前宽度定状态，不等第一次 resize", () => {
    // 不立刻应用的话，一个在窄窗口里启动的程序会先按宽屏画一帧，
    // 侧栏挤掉终端，直到用户碰一下窗口才纠正。
    const m = stubMatchMedia(false); // 不是宽屏 = 窄屏
    const off = initResponsive();
    expect(get(narrowScreen)).toBe(true);
    off();
    void m;
  });

  it("窗口变宽变窄时跟着变", () => {
    const m = stubMatchMedia(true);
    const off = initResponsive();
    expect(get(narrowScreen)).toBe(false);
    m.fire(false);
    expect(get(narrowScreen)).toBe(true);
    m.fire(true);
    expect(get(narrowScreen)).toBe(false);
    off();
  });

  it("回到宽屏时把覆盖层状态清掉", () => {
    // 留着的话，下次拉窄会直接是展开的——而用户上一次让它展开是在几分钟前的
    // 另一个窗口尺寸下。
    const m = stubMatchMedia(false);
    const off = initResponsive();
    sidebarOverlayOpen.set(true);
    m.fire(true); // 变宽
    expect(get(sidebarOverlayOpen)).toBe(false);
    off();
  });

  it("解绑函数真的摘掉监听（每次重挂载留一个 = 泄漏）", () => {
    const m = stubMatchMedia(true);
    const off = initResponsive();
    expect(m.listenerCount).toBe(1);
    off();
    expect(m.listenerCount).toBe(0);
    expect(m.removed).toBe(1);
  });

  /**
   * 拿不到 matchMedia 时**按宽屏处理**。
   *
   * 按窄屏的话，那种环境会永远显示覆盖层——而覆盖层默认收起，
   * 于是用户看到的是「侧栏不见了」。宽屏至少是当前布局。
   */
  it("没有 matchMedia 的环境按宽屏处理，且不崩", () => {
    vi.stubGlobal("matchMedia", undefined);
    (window as unknown as { matchMedia: unknown }).matchMedia = undefined;
    narrowScreen.set(true); // 先弄脏
    const off = initResponsive();
    expect(get(narrowScreen)).toBe(false);
    expect(() => off()).not.toThrow();
  });
});

/**
 * 侧栏在 App 里的实际呈现（M4b 出口第 16、18 项的界面半边）。
 *
 * 出口点名「组件测试（matchMedia mock）」。App.svelte 是个 1300 行的装配件，
 * 整体渲染它要桩掉几十个依赖；这里改测**它写出来的那几行**——
 * 呈现形态怎么算、算出来之后 CSS 类怎么挂、遮罩什么时候出现。
 * 仲裁本身的行为由上面那组纯函数用例覆盖。
 */
describe("App 里的侧栏呈现接线", () => {
  it("呈现形态由 effectivePresentation 算，不是又写一遍 if", () => {
    // 又写一遍的话，两处判据会走散——而走散的表现是「菜单里选了右停靠，
    // 侧栏还在左边」，且没有任何东西会提示。
    expect(APP_SRC).toMatch(/effectivePresentation\(\$sidebarDock, \$narrowScreen\)/);
  });

  it("三种形态各自挂类，且覆盖态与展开态是两个类", () => {
    // 覆盖态（是不是浮着）与展开态（浮着的那个开没开）必须分开：
    // 合成一个类的话，收起状态就没法用 transform 做过渡，只能 display:none，
    // 而那会让 240px 的面板瞬间出现，看起来像界面跳了一下。
    expect(APP_SRC).toMatch(/class:dock-right=\{sidebarPresentation === "right"\}/);
    expect(APP_SRC).toMatch(/class:overlay=\{sidebarPresentation === "overlay"\}/);
    expect(APP_SRC).toMatch(/class:overlay-open=/);
  });

  it("覆盖态下不渲染拖宽手柄", () => {
    // 覆盖层不占位，拖它改不了主区宽度——一个拖了没反应的手柄比没有更糟。
    expect(APP_SRC).toMatch(/\{#if sidebarPresentation !== "overlay"\}/);
  });

  it("点击外部收起走的是遮罩，不是在主区上挂 click", () => {
    // 在 .main 上挂 click 会把「点终端里的某个位置」也算成「点了外部」，
    // 于是用户想在终端里放光标却先收起了侧栏，还得再点一次。
    expect(APP_SRC).toMatch(/data-testid="sidebar-scrim"/);
    expect(APP_SRC).toMatch(/sidebarOverlayOpen\.set\(false\)/);
  });

  it("遮罩只在覆盖且展开时渲染", () => {
    // 常驻遮罩会吃掉主区的所有点击。
    expect(APP_SRC).toMatch(
      /\{#if sidebarPresentation === "overlay" && \$sidebarOverlayOpen\}/,
    );
  });

  it("matchMedia 监听随组件卸载解绑", () => {
    // 不解绑的话，每次重挂载留一个持有旧 store 的监听。
    expect(APP_SRC).toMatch(/appUnlisteners\.push\(initResponsive\(\)\)/);
  });

  it("右停靠靠 flex order 换位，.main 有显式 order", () => {
    // .main 不显式写 order 的话它是默认 0，与侧栏并列，换位失效。
    expect(APP_SRC).toMatch(/\.sidebar\.dock-right \{[^}]*order: 2/);
    expect(APP_SRC).toMatch(/\.main \{[^}]*order: 1/);
  });

  it(".body 是定位上下文（不然覆盖层会逃出去盖住菜单栏）", () => {
    expect(APP_SRC).toMatch(/\.body \{[^}]*position: relative/);
  });

  it("覆盖层有 z-index 与投影（出口原文点名这两件事）", () => {
    expect(APP_SRC).toMatch(/\.sidebar\.overlay \{[\s\S]*?z-index:/);
    expect(APP_SRC).toMatch(/\.sidebar\.overlay \{[\s\S]*?box-shadow:/);
  });
});

/* 路线图 4c「overlay 形态的侧栏缺开启入口」（2026-09-02）。
 * 审查时数过：sidebarOverlayOpen 生产代码里只有三处 set(false)，唯一的 set(true) 在本文件的夹具里——
 * 窄屏 / 浮动停靠下侧栏收起之后，没有任何入口把它叫回来。 */
describe("view.sidebar 按呈现形态分流（toggleSidebarFor）", () => {
  beforeEach(() => { sidebarVisible.set(true); sidebarOverlayOpen.set(false); });

  it("overlay：翻浮层开关、不动 sidebarVisible——这是收起后能把侧栏叫回来的那个入口", () => {
    toggleSidebarFor("overlay");
    expect(get(sidebarOverlayOpen)).toBe(true);
    expect(get(sidebarVisible)).toBe(true);
    toggleSidebarFor("overlay");
    expect(get(sidebarOverlayOpen)).toBe(false);
  });

  it("overlay 且 sidebarVisible 已是 false（停靠时藏过）：先让 aside 存在再展开，否则开了也看不见", () => {
    sidebarVisible.set(false);
    toggleSidebarFor("overlay");
    expect(get(sidebarVisible)).toBe(true);
    expect(get(sidebarOverlayOpen)).toBe(true);
  });

  it("停靠形态：仍是翻 sidebarVisible，浮层开关不动（此前行为原样保留）", () => {
    toggleSidebarFor("left");
    expect(get(sidebarVisible)).toBe(false);
    expect(get(sidebarOverlayOpen)).toBe(false);
    toggleSidebarFor("right");
    expect(get(sidebarVisible)).toBe(true);
  });

  it("App 接线：view.sidebar 走 toggleSidebarFor(sidebarPresentation)；工具栏拿到 overlaySidebar / overlayOpen", () => {
    expect(APP_SRC).toContain('if (id === "view.sidebar") { toggleSidebarFor(sidebarPresentation); return; }');
    expect(APP_SRC, "旧写法回来了：overlay 下它只会把 aside 整个藏掉").not.toContain('{ toggle("sidebar"); return; }');
    expect(APP_SRC).toMatch(/<ToolBar[\s\S]*?overlaySidebar=\{sidebarPresentation === "overlay"\}[\s\S]*?overlayOpen=\{\$sidebarOverlayOpen\}/);
  });
});

/**
 * 虚拟窗口画布的挂载点（M7.3 出口标准③）。
 *
 * 画布必须在**终端区之内**：它是 `position: absolute; inset: 0` 的一层，靠最近的定位祖先
 * 定边界。挂到 `.app` 或 `.main` 上，窗口就能盖住状态栏与标签栏，而夹紧算的又是画布尺寸
 * ——那时「不可拖出可视区」这句话守的是错的那个矩形。行为测试测不到这一条（jsdom 不排版），
 * 所以在这里按源码位置钉。
 */
describe("虚拟窗口画布挂在终端区里（M7.3）", () => {
  const term = APP_SRC.slice(APP_SRC.indexOf('<div class="terminal"'));
  const layerAt = term.indexOf("<VWindowLayer");
  const termEnd = term.indexOf("</div>\n      {#if $monitorOpen}");

  it("挂载点在 .terminal 容器内", () => {
    expect(layerAt).toBeGreaterThan(0);
    expect(termEnd).toBeGreaterThan(0);
    expect(layerAt).toBeLessThan(termEnd);
  });

  it(".terminal 是定位祖先（否则 inset:0 会跑到更外层的容器上）", () => {
    expect(APP_SRC).toMatch(/\.terminal \{[^}]*position: relative/);
  });

  it("会话关闭时收走它的虚拟窗口", () => {
    expect(APP_SRC).toContain("closeVWindowsForSession(e.payload.session_id)");
  });

  it("每个终端标签都接了「浮动文件视图」，且带自己的 sessionId", () => {
    expect(APP_SRC).toMatch(/onFloatFiles=\{\(\) =>[\s\S]{0,200}sessionId: tab\.id/);
  });
});
