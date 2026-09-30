import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render } from "@testing-library/svelte";
import type { ComponentProps } from "svelte";

// 测试替身（vi.hoisted：mock 工厂提升前即可引用）：捕获 createTerminal 的 handlers 与 options
const termMock = vi.hoisted(() => {
  /* eslint-disable @typescript-eslint/no-explicit-any -- 测试替身：捕获 createTerminal 实际入参供断言 */
  const state: { handlers: any; opts: any } = { handlers: null, opts: null };
  const controller = {
    write: vi.fn(),
    findNext: vi.fn(() => true),
    findPrevious: vi.fn(() => true),
    clearSearch: vi.fn(),
    getSelection: vi.fn(() => ""),
    clearSelection: vi.fn(),
    selectAll: vi.fn(),
    pasteText: vi.fn(),
    setScheme: vi.fn(),
    setFontSize: vi.fn(),
    setOpacity: vi.fn(), // 缺这一项时组件的即时生效 $effect 会抛 "not a function"（即：替身漏配也会转红）
    zoomFont: vi.fn(),
    resetFontSize: vi.fn(),
    fit: vi.fn(),
    focus: vi.fn(),
    dispose: vi.fn(),
  };
  const createTerminal = vi.fn((_el: HTMLElement, handlers: any, opts: any) => {
    state.handlers = handlers;
    state.opts = opts;
    return controller;
  });
  return { state, controller, createTerminal };
});

// S281（复审 #18）：只替换 createTerminal（jsdom 无法渲染真实 xterm），其余导出（encodeB64 等）
// 一律 importActual 取真身——原工厂内嵌的 encodeB64 逐字节副本是零调用的死重复制，
// 真身变更后不会转红；改 importActual 后组件内唯一消费点（右键菜单清屏 \x0c）走真实编码。
vi.mock("../lib/term", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../lib/term")>();
  return { ...actual, createTerminal: termMock.createTerminal };
});

/**
 * settingChanged 的可驱动替身（形状同 ipc.ts 导出的 Readable：subscribe 返回退订函数）。
 * `emit()` 等价于「用户在选项页改了一项设置且已落库」。
 * 真身是 writable，会向后到的订阅者重放末值（初始 null）——这里照做，组件的 `if (!ev) return`
 * 必须吃掉它；若哪天组件去掉那个判空，重放的 null 会立刻把它打红。
 */
const bus = vi.hoisted(() => {
  type Ev = { key: string; value: unknown } | null;
  const subs = new Set<(ev: Ev) => void>();
  return {
    subs,
    store: {
      subscribe(fn: (ev: Ev) => void) {
        subs.add(fn);
        fn(null); // 重放末值
        return () => subs.delete(fn);
      },
    },
    emit(key: string, value: unknown) {
      for (const fn of [...subs]) fn({ key, value });
    },
  };
});

vi.mock("../lib/ipc", () => ({
  invoke: vi.fn(async () => null),
  listen: vi.fn(async () => () => {}),
  settingGet: vi.fn(async (_key: string, fallback: unknown) => fallback),
  settingSet: vi.fn(async () => {}),
  settingChanged: bus.store,
  openExternal: vi.fn(async () => {}),
  // S300：真实现走 invoke("log_frontend_error") 落后端日志；此处只需证明
  // 「出口出了声」，故用朴素替身直接断言调用。
  reportFrontendError: vi.fn(),
}));

// 拖拽上传（Task 52）：TerminalPane 的 $effect 里订阅 getCurrentWebview().onDragDropEvent。
// jsdom/vitest 下 Tauri internals 不存在，真调会抛——这里给一个可捕获处理器、立即解绑的替身。
vi.mock("@tauri-apps/api/webview", () => ({
  getCurrentWebview: () => ({ onDragDropEvent: vi.fn(async () => () => {}) }),
}));

import { invoke, listen, reportFrontendError, settingGet } from "../lib/ipc";
import { DEFAULT_SCHEME, getScheme } from "../lib/term-schemes";
import TerminalPane from "./TerminalPane.svelte";
import { get } from "svelte/store";
import { tabs, addTab, activateTab, resetTabStore } from "../lib/tabs";

type Props = ComponentProps<typeof TerminalPane>;

async function mountPane(props: Partial<Props> = {}) {
  const view = render(TerminalPane, { props: { sessionId: "s1", ...props } });
  await vi.waitFor(() => expect(termMock.createTerminal).toHaveBeenCalledTimes(1));
  return view;
}

describe("TerminalPane（挂载与事件转发）", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    termMock.state.handlers = null;
    termMock.state.opts = null;
  });

  it("挂载建终端并订阅 term:data 流；onData 转发 term_input（b64 原样透传）", async () => {
    await mountPane();
    expect(vi.mocked(listen).mock.calls[0][0]).toBe("term:data:s1");
    termMock.state.handlers.onData("b2s=");
    expect(invoke).toHaveBeenCalledWith("term_input", { sessionId: "s1", dataB64: "b2s=" });
  });

  it("S308：onBroadcastData 在自身 term_input 之后收到同一 b64 原文（字节级一致的前端出口）", async () => {
    const onBroadcastData = vi.fn();
    await mountPane({ onBroadcastData });
    termMock.state.handlers.onData("a2V5");
    // 自身会话先发（顺序契约：广播是附加副本，到达序与单发一致）
    const ownCall = vi.mocked(invoke).mock.calls.find((c) => c[0] === "term_input");
    expect(ownCall).toEqual(["term_input", { sessionId: "s1", dataB64: "a2V5" }]);
    expect(onBroadcastData).toHaveBeenCalledTimes(1);
    // 转发的是 b64 原文，不得解码重编（字节级一致是 M4a 出口标准的硬契约）
    expect(onBroadcastData).toHaveBeenCalledWith("a2V5");
  });

  it("term:data 回放：载荷键 seq/data_b64（snake，与 term_input 的 dataB64 相反）原样入 ctrl.write", async () => {
    await mountPane();
    // 按 Tauri Event 形状回放（{event,id,payload}）；载荷键名漂移（如误发驼峰 dataB64）将令 write 收到 undefined
    const handler = vi.mocked(listen).mock.calls[0][1] as unknown as (e: { event: string; id: number; payload: { seq: number; data_b64: string } }) => void;
    handler({ event: "term:data:s1", id: 1, payload: { seq: 7, data_b64: "b2s=" } });
    expect(termMock.controller.write).toHaveBeenCalledWith(7, "b2s=");
  });

  /**
   * BEL → 角标链条的第 ③ 跳（M1 出口「输出提醒」）：handler 真的落到 tabs store。
   * 其余三跳见 lib/bell-badge.test.ts；那里跑不了这一跳，因为捕获 handlers 的替身在本文件。
   */
  it("onBell 落到 tabs store 的 bell 位（不是只在控制台响一声）", async () => {
    resetTabStore();
    addTab("s1", "Session 1", "p1", "connected", "h1");
    activateTab("s2-elsewhere-none"); // 让 s1 不是活动标签，否则切换手势会顺手清掉刚置的位
    await mountPane();
    expect(get(tabs).find((t) => t.id === "s1")?.bell, "自检：初始不该有角标").toBe(false);
    termMock.state.handlers.onBell();
    expect(get(tabs).find((t) => t.id === "s1")?.bell).toBe(true);
    resetTabStore();
  });

  /**
   * 关键字高亮命中与 BEL **共用**角标通道（UI 规格 §2.11）。两者接的是同一个
   * ringBell，但接线是两行独立的代码——只测其一，另一行删掉不会有任何东西转红。
   */
  it("onHighlightAlert 同样点亮角标（与 BEL 共用通道）", async () => {
    resetTabStore();
    addTab("s1", "Session 1", "p1", "connected", "h1");
    activateTab("s2-elsewhere-none");
    await mountPane();
    termMock.state.handlers.onHighlightAlert();
    expect(get(tabs).find((t) => t.id === "s1")?.bell).toBe(true);
    resetTabStore();
  });

  it("onAck 转发 term_ack IPC 且回的是 seq（S295 累计确认的前端出口）", async () => {
    await mountPane();
    termMock.state.handlers.onAck(42);
    // 钉「回 seq 不回字节数」：字节口径下丢一帧即队首永久欠账、帧维水位单调爬升至
    // 锁死会话（见 Rust 侧 fs_terminal::flow 模块头与 S296 最小复现）。参数名退回
    // bytes 即是把那个死锁重新装回来。
    expect(invoke).toHaveBeenCalledWith("term_ack", { sessionId: "s1", seq: 42 });
  });

  it("S299：ctrl 就绪前到达的 term:data 帧入队并按序回放，零丢弃", async () => {
    // 订阅建立于 onMount 的**第一个** await 之前，而 ctrl 要等 7 次 settingGet +
    // createTerminal（实测约 33 ms）才就绪。这中间到达的帧曾被 `ctrl?.write` 的
    // 可选链静默丢掉——后端已把它们记进 sent_frames，前端却永远不会 ack。
    const { listen: listenMock } = await import("../lib/ipc");
    const calls = vi.mocked(listenMock).mock.calls;
    const before = calls.length;
    const p = mountPane();
    // 抢在挂载异步链完成前拿到 term:data 处理器并投三帧
    await vi.waitFor(() => expect(vi.mocked(listenMock).mock.calls.length).toBeGreaterThan(before));
    const handler = vi.mocked(listenMock).mock.calls[before][1] as unknown as (e: { event: string; id: number; payload: { seq: number; data_b64: string } }) => void;
    handler({ event: "term:data:s1", id: 1, payload: { seq: 0, data_b64: "YQ==" } });
    handler({ event: "term:data:s1", id: 2, payload: { seq: 1, data_b64: "Yg==" } });
    handler({ event: "term:data:s1", id: 3, payload: { seq: 2, data_b64: "Yw==" } });
    await p;
    await vi.waitFor(() => expect(termMock.controller.write).toHaveBeenCalledTimes(3));
    expect(vi.mocked(termMock.controller.write).mock.calls).toEqual([
      [0, "YQ=="],
      [1, "Yg=="],
      [2, "Yw=="],
    ]);
  });

  it("S300：term_ack 的 IPC rejection 上报后端而非静默吞掉", async () => {
    await mountPane();
    vi.mocked(invoke).mockRejectedValueOnce(new Error("no session"));
    termMock.state.handlers.onAck(1);
    // 原实现是 `void invoke(...)`：term_ack 对未知会话返回 Err("no session")，
    // 被静默吞掉就等于把「ack 根本没入账」这条线索抹干净——2026-08-19 事故复盘时
    // 无法区分「前端没发 ack」与「发了但后端没收」，正因为这里不出声。
    await vi.waitFor(() => expect(reportFrontendError).toHaveBeenCalled());
    expect(vi.mocked(reportFrontendError).mock.calls[0][0]).toBe("term_ack");
  });

  it("onReady 投影三键：openSearch/findNext/controller（R57 续搜入口）", async () => {
    const onReady = vi.fn();
    await mountPane({ onReady });
    await vi.waitFor(() => expect(onReady).toHaveBeenCalledTimes(1)); // onMount 异步链（settingGet→建终端→listen）完成后才回调
    const api = onReady.mock.calls[0][0];
    expect(typeof api.openSearch).toBe("function");
    expect(typeof api.findNext).toBe("function");
    // 注：ctrl 经 $state() 包成响应式 proxy，整体 !== 原对象；以成员引用同一性断言（proxy get 透传）
    expect(api.controller()?.write).toBe(termMock.controller.write);
  });
});

describe("TerminalPane（三级配色应用，R11）", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    termMock.state.handlers = null;
    termMock.state.opts = null;
  });

  it("无 profile 覆盖：全局默认方案 + 字号 13（global 层）", async () => {
    await mountPane();
    expect(termMock.state.opts.scheme.id).toBe(DEFAULT_SCHEME.id);
    expect(termMock.state.opts.fontSize).toBe(13);
  });

  it("profile themeOverride 字段级覆盖生效（scheme 覆盖、未覆盖的 fontSize 保留全局值）", async () => {
    await mountPane({ profileThemeOverride: { scheme: "dracula" } });
    expect(termMock.state.opts.scheme).toEqual(getScheme("dracula"));
    expect(termMock.state.opts.fontSize).toBe(13);
  });

  it("settings 变更再应用：profileThemeOverride 变化 → $effect 经 resolveTheme 重解析，setScheme/setFontSize 不重建终端", async () => {
    const view = await mountPane({ profileThemeOverride: { scheme: "dracula" } });
    await view.rerender({ sessionId: "s1", profileThemeOverride: { scheme: "nord", fontSize: 15 } });
    await vi.waitFor(() =>
      expect(termMock.controller.setScheme).toHaveBeenLastCalledWith(getScheme("nord")),
    );
    expect(termMock.controller.setFontSize).toHaveBeenLastCalledWith(15);
    expect(termMock.createTerminal).toHaveBeenCalledTimes(1); // 即时生效不重建
  });

  it("S270 scheme-only 变化不复写字号：换配色不得把 per-session 缩放打回基线（§3.2 切换作用域/§3.3 per-session）", async () => {
    const view = await mountPane({ profileThemeOverride: { scheme: "dracula", fontSize: 15 } });
    await vi.waitFor(() => expect(termMock.controller.setFontSize).toHaveBeenLastCalledWith(15));
    const callsBefore = vi.mocked(termMock.controller.setFontSize).mock.calls.length;
    await view.rerender({ sessionId: "s1", profileThemeOverride: { scheme: "nord", fontSize: 15 } }); // 仅 scheme 变
    await vi.waitFor(() => expect(termMock.controller.setScheme).toHaveBeenLastCalledWith(getScheme("nord")));
    expect(vi.mocked(termMock.controller.setFontSize).mock.calls.length).toBe(callsBefore); // fontSize 未变 → 不重写
  });
});

/**
 * C-1：全局设置改完，对**已经挂载的**终端要立刻生效（UI 规格 §3.2 三处「即时生效」）。
 *
 * 原缺陷不在 $effect，也不在 term.ts——两者都是好的：组件在 onMount 里把 7 个全局键读进 $state，
 * 之后**再没有人写过这 7 个变量**。标签是恒挂载的（隐藏≠销毁），所以「改了设置去别的标签看看」
 * 也不会重挂载，实际表现是：除非重启，全局层改什么都没用；而组件自己的注释写着即时生效。
 *
 * 上面那组用例测不出它：它们全走 profileThemeOverride（props），props 变了 Svelte 自然会重跑 $effect。
 * 缺的恰恰是「谁来改 $state」，所以这里从 settingChanged 这一侧驱动。
 */
describe("TerminalPane（全局设置变更即时生效，C-1）", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    termMock.state.handlers = null;
    termMock.state.opts = null;
  });

  it("term.scheme 广播 → 已挂载终端换色，且不重建实例", async () => {
    await mountPane();
    expect(termMock.state.opts.scheme.id).toBe(DEFAULT_SCHEME.id);
    bus.emit("term.scheme", "nord");
    await vi.waitFor(() => expect(termMock.controller.setScheme).toHaveBeenLastCalledWith(getScheme("nord")));
    expect(termMock.createTerminal).toHaveBeenCalledTimes(1);
  });

  /**
   * M1 出口「背景透明度」的另外两句：「**新建会话**终端背景转半透明」与「**重启保持**」。
   * 两者是同一个机制——挂载时从 settings 读回 term.opacity 并经 opts.opacity 注入
   * createTerminal（重启后 settings 仍在库里，新会话读到的就是上次的值）。
   * 下面那条广播用例证明的是「已开会话即时生效」，两者互不覆盖：
   * 只留广播那条时，把挂载期的 settingGet 删掉，新开的标签会静默退回不透明。
   */
  it("新建会话从 settings 读回 term.opacity 并注入 createTerminal（= 重启保持）", async () => {
    vi.mocked(settingGet).mockImplementation(async (key: string, fallback: unknown) =>
      key === "term.opacity" ? 65 : fallback);
    await mountPane();
    expect(termMock.state.opts.opacity, "挂载期未读回持久值 → 新标签恒不透明").toBe(65);
    vi.mocked(settingGet).mockImplementation(async (_k: string, fallback: unknown) => fallback);
  });

  it("未设置过时新建会话取缺省 100（不透明），不是 undefined", async () => {
    await mountPane();
    expect(termMock.state.opts.opacity).toBe(100);
  });

  it("term.fontSize / term.opacity 广播 → 字号与透明度即时应用", async () => {
    await mountPane();
    bus.emit("term.fontSize", 17);
    await vi.waitFor(() => expect(termMock.controller.setFontSize).toHaveBeenLastCalledWith(17));
    bus.emit("term.opacity", 80);
    await vi.waitFor(() => expect(termMock.controller.setOpacity).toHaveBeenLastCalledWith(80));
  });

  // 交互类的 4 个键没有 controller 方法可断言：它们经 `interactions` 闭包被 term.ts 每次事件现读
  // （term.ts 里真正读它的那段 DOM 处理器在本文件被 createTerminal 替身挡住了，故不走 DOM 事件）。
  // 直接调用组件交给 createTerminal 的那个闭包，钉住「订阅处理器写的就是闭包实际读的那个变量」。
  it("交互类 4 键广播 → interactions 闭包当场读到新值（不必重开标签）", async () => {
    await mountPane();
    expect(termMock.state.opts.interactions()).toEqual({
      rightClick: "paste", copyOnSelect: false, multilinePasteConfirm: true, ctrlVPaste: true,
    });
    bus.emit("ui.rightClick", "menu");
    bus.emit("ui.copyOnSelect", true);
    bus.emit("ui.multilinePasteConfirm", false);
    bus.emit("ui.ctrlVPaste", false);
    await vi.waitFor(() =>
      expect(termMock.state.opts.interactions()).toEqual({
        rightClick: "menu", copyOnSelect: true, multilinePasteConfirm: false, ctrlVPaste: false,
      }),
    );
  });

  // 非法/异常值不能把已生效的设置打成 NaN——num() 的 fallback 分支。
  it("坏值不落地：term.fontSize 收到非数字时保持原值", async () => {
    await mountPane();
    bus.emit("term.fontSize", 17);
    await vi.waitFor(() => expect(termMock.controller.setFontSize).toHaveBeenLastCalledWith(17));
    bus.emit("term.fontSize", "abc");
    await Promise.resolve();
    expect(termMock.controller.setFontSize).toHaveBeenLastCalledWith(17);
    expect(vi.mocked(termMock.controller.setFontSize).mock.calls.every(([n]) => Number.isFinite(n))).toBe(true);
  });

  // 不退订 ⇒ 每关一个标签就在 store 的订阅表里留一个闭包（连同它捕获的整棵组件状态）永不释放。
  // 注意断的是**订阅表本身**，不是「销毁后 setScheme 还会不会被调到」：Svelte 卸载时已经拆掉了
  // $effect，漏退订也观察不到多余的 setScheme 调用——那样的断言对本缺陷是恒真的（V19-M4 实测：
  // 把 unSetting?.() 换成 void 0，那条断言照样绿）。订阅数才是这个泄漏的唯一可观测量。
  it("卸载即退订：订阅表不随开关标签只增不减", async () => {
    const before = bus.subs.size;
    const a = await mountPane();
    await vi.waitFor(() => expect(bus.subs.size).toBe(before + 1));
    const b = render(TerminalPane, { props: { sessionId: "s2" } });
    await vi.waitFor(() => expect(bus.subs.size).toBe(before + 2));
    b.unmount();
    a.unmount();
    expect(bus.subs.size).toBe(before);
  });
});

describe("TerminalPane（resize 上报，R4）", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    termMock.state.handlers = null;
    termMock.state.opts = null;
  });

  it("onResize 并列 term_resize IPC 与 onResized(rows, cols) 回调（供状态栏 rows×cols 段）", async () => {
    const onResized = vi.fn();
    await mountPane({ onResized });
    termMock.state.handlers.onResize(100, 30); // (cols, rows) —— term.ts 契约
    expect(invoke).toHaveBeenCalledWith("term_resize", { sessionId: "s1", cols: 100, rows: 30 });
    expect(onResized).toHaveBeenCalledWith(30, 100); // (rows, cols) —— Task 20 组 H ⑦ 契约
  });
});

describe("TerminalPane（粘贴确认气泡键盘契约，F34）", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    termMock.state.handlers = null;
    termMock.state.opts = null;
  });

  it("多行粘贴气泡弹出即聚焦；Enter 确认（true）·Escape 取消（false）", async () => {
    const view = await mountPane();
    const p1 = termMock.state.opts.requestPasteConfirm(3) as Promise<boolean>;
    await vi.waitFor(() => expect(document.querySelector(".paste-confirm")).not.toBeNull());
    // F34「弹出即聚焦」名副其实（复审 #16）：标题声称的聚焦此前无断言，fireEvent 直发元素绕过焦点路由
    await vi.waitFor(() => expect(document.activeElement).toBe(document.querySelector(".paste-confirm")));
    await fireEvent.keyDown(document.querySelector(".paste-confirm") as HTMLElement, { key: "Enter" });
    await expect(p1).resolves.toBe(true);

    const p2 = termMock.state.opts.requestPasteConfirm(2) as Promise<boolean>;
    await vi.waitFor(() => expect(document.querySelector(".paste-confirm")).not.toBeNull());
    await fireEvent.keyDown(document.querySelector(".paste-confirm") as HTMLElement, { key: "Escape" });
    await expect(p2).resolves.toBe(false);
    view.unmount();
  });

  it("S272 焦点漂移兜底：气泡悬置时 Enter/Escape 落在本窗格内（xterm textarea 所在 el）仍由窗口级结算（#20：Esc 取消不可达/Enter 误发 \\r）", async () => {
    const view = await mountPane();
    const paneEl = document.querySelector(".terminal-pane") as HTMLElement; // 焦点漂移的真实目标在 el 内（xterm helper textarea）
    const p1 = termMock.state.opts.requestPasteConfirm(3) as Promise<boolean>;
    await vi.waitFor(() => expect(document.querySelector(".paste-confirm")).not.toBeNull());
    fireEvent.keyDown(paneEl, { key: "Enter" }); // 事件不经气泡 onkeydown，但 target 在本窗格 el 内（S284 归属判别放行）
    await expect(p1).resolves.toBe(true);
    const p2 = termMock.state.opts.requestPasteConfirm(2) as Promise<boolean>;
    await vi.waitFor(() => expect(document.querySelector(".paste-confirm")).not.toBeNull());
    fireEvent.keyDown(paneEl, { key: "Escape" });
    await expect(p2).resolves.toBe(false);
    view.unmount();
  });

  it("S272 气泡重入：第二次多行粘贴先以取消结算第一条（pasteFlow 不悬挂、不丢 resolve）", async () => {
    const view = await mountPane();
    const p1 = termMock.state.opts.requestPasteConfirm(3) as Promise<boolean>;
    await vi.waitFor(() => expect(document.querySelector(".paste-confirm")).not.toBeNull());
    const p2 = termMock.state.opts.requestPasteConfirm(2) as Promise<boolean>; // 重入
    await expect(p1).resolves.toBe(false); // 旧泡按取消结算，而非永挂
    const bubble = await vi.waitFor(() => {
      const el = document.querySelector(".paste-confirm");
      expect(el).not.toBeNull();
      return el as HTMLElement;
    });
    expect(bubble.textContent).toContain("2 行"); // 新泡显示第二次行数
    await fireEvent.keyDown(bubble, { key: "Enter" });
    await expect(p2).resolves.toBe(true);
    view.unmount();
  });
});

describe("TerminalPane（右键上下文菜单窗口级关闭，S267/S273）", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    termMock.state.handlers = null;
    termMock.state.opts = null;
  });

  async function openMenu() {
    await mountPane();
    termMock.state.handlers.onContextMenu(10, 20);
    await vi.waitFor(() => expect(document.querySelector(".ctx-menu")).not.toBeNull());
  }

  it("onContextMenu 渲染菜单；窗口级 click 关闭（面板外任意位置）", async () => {
    await openMenu();
    fireEvent.click(document.body);
    await vi.waitFor(() => expect(document.querySelector(".ctx-menu")).toBeNull());
  });

  it("Escape 关闭（S267 键盘等价物）", async () => {
    await openMenu();
    fireEvent.keyDown(window, { key: "Escape" });
    await vi.waitFor(() => expect(document.querySelector(".ctx-menu")).toBeNull());
  });

  it("S275 窗口级 pointerdown 关闭：拖选文本起手即关，不等 mouseup", async () => {
    await openMenu();
    fireEvent.pointerDown(document.body);
    await vi.waitFor(() => expect(document.querySelector(".ctx-menu")).toBeNull());
  });
});

describe("TerminalPane（二轮复审整改 S283/S284/S285）", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    termMock.state.handlers = null;
    termMock.state.opts = null;
  });

  it("S283 pasteConfirmPending 权威闭包经 opts 注入：气泡悬置判真、结算判假、重入期间恒真（陈旧标志回归钉）", async () => {
    const view = await mountPane();
    const opts = termMock.state.opts;
    expect(typeof opts.pasteConfirmPending).toBe("function"); // 整改前 opts 无此键 → 红
    expect(opts.pasteConfirmPending()).toBe(false);
    const p1 = opts.requestPasteConfirm(3) as Promise<boolean>;
    await vi.waitFor(() => expect(opts.pasteConfirmPending()).toBe(true));
    const p2 = opts.requestPasteConfirm(2) as Promise<boolean>; // 重入：旧泡 false 结算、新泡置位
    await expect(p1).resolves.toBe(false);
    expect(opts.pasteConfirmPending()).toBe(true); // 新泡仍悬置 → 恒真（布尔标志此处已陈旧为 false）
    const bubble = document.querySelector(".paste-confirm") as HTMLElement;
    await fireEvent.keyDown(bubble, { key: "Enter" });
    await expect(p2).resolves.toBe(true);
    expect(opts.pasteConfirmPending()).toBe(false);
    view.unmount();
  });

  it("S284 窗格归属判别①：他标签窗格内的 Enter 不得结算本窗格悬置气泡（多标签恒挂载装配）", async () => {
    render(TerminalPane, { props: { sessionId: "sA" } });
    await vi.waitFor(() => expect(termMock.createTerminal).toHaveBeenCalledTimes(1));
    render(TerminalPane, { props: { sessionId: "sB" } });
    await vi.waitFor(() => expect(termMock.createTerminal).toHaveBeenCalledTimes(2));
    const optsA = vi.mocked(termMock.createTerminal).mock.calls[0][2];
    const p = optsA.requestPasteConfirm(3) as Promise<boolean>; // 标签 A 悬置气泡
    await vi.waitFor(() => expect(document.querySelector(".paste-confirm")).not.toBeNull());
    const paneB = document.querySelectorAll(".terminal-pane")[1] as HTMLElement;
    fireEvent.keyDown(paneB, { key: "Enter" }); // 用户面对标签 B 按键
    await new Promise((r) => setTimeout(r, 20)); // 给（若存在的）错误结算留时序窗
    expect(document.querySelector(".paste-confirm")).not.toBeNull(); // A 的气泡仍在 ⇒ 未被越权结算
    let settled = false;
    void p.then(() => (settled = true));
    await new Promise((r) => setTimeout(r, 0));
    expect(settled).toBe(false);
  });

  it("S284 窗格归属判别②：共存输入面（Settings/Compose 类 INPUT）内 Enter 不得误结算悬置气泡", async () => {
    const view = await mountPane();
    const p = termMock.state.opts.requestPasteConfirm(3) as Promise<boolean>;
    await vi.waitFor(() => expect(document.querySelector(".paste-confirm")).not.toBeNull());
    const foreignInput = document.createElement("input"); // 模拟 SettingsDialog 数字框等窗格外输入面
    document.body.appendChild(foreignInput);
    fireEvent.keyDown(foreignInput, { key: "Enter" });
    await new Promise((r) => setTimeout(r, 20));
    expect(document.querySelector(".paste-confirm")).not.toBeNull(); // 气泡未被输入面按键误结算
    let settled = false;
    void p.then(() => (settled = true));
    await new Promise((r) => setTimeout(r, 0));
    expect(settled).toBe(false);
    foreignInput.remove();
    view.unmount();
  });

  it("S285 气泡 onkeydown stopPropagation 守卫：结算事件不得冒泡至窗口（变异⑥缺口钉死）", async () => {
    const view = await mountPane();
    const p1 = termMock.state.opts.requestPasteConfirm(3) as Promise<boolean>;
    const bubble = await vi.waitFor(() => {
      const b = document.querySelector(".paste-confirm");
      expect(b).not.toBeNull();
      return b as HTMLElement;
    });
    const windowSpy = vi.fn();
    window.addEventListener("keydown", windowSpy);
    bubble.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true, cancelable: true }));
    await expect(p1).resolves.toBe(true); // 气泡自身处理器结算
    expect(windowSpy).not.toHaveBeenCalled(); // stopPropagation 生效 ⇒ 窗口层（含共存菜单 Esc 支）收不到
    window.removeEventListener("keydown", windowSpy);
    view.unmount();
  });
});

/**
 * 审计2 #35：档案 `term.scrollback_lines` 此前是死配置——App 根本不把它传进 TerminalPane，
 * 面板里也没有消费点（term.ts 恒 `scrollback: 10000`）。修复链：App → TerminalPane prop →
 * term.ts opts.scrollback → clampScrollbackLines（边界 [1, 1_000_000]，与 Rust 侧
 * SCROLLBACK_MAX 同源）。此处只钉「prop 原样进入 createTerminal opts」这一环——钳位逻辑本身
 * 在 term.test.ts 的纯函数用例里钉，替身不重复实现。
 */
describe("TerminalPane（scrollback 贯通，审计2 #35）", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    termMock.state.handlers = null;
    termMock.state.opts = null;
  });

  it("缺省：opts.scrollback 为 null，钳位兜底 10000 交给 term.ts（组件不得私造常量）", async () => {
    await mountPane();
    expect(termMock.state.opts.scrollback).toBeNull();
  });

  it("档案值原样进入 opts：App 传 5000 → createTerminal 收到 5000（旧实现：无此键）", async () => {
    await mountPane({ scrollbackLines: 5000 });
    expect(termMock.state.opts.scrollback).toBe(5000);
  });
});

/**
 * 自定义/导入的配色（M4b）。
 *
 * 这一组补的是第 1 项（第三方导入）留下的一个真缺口：`schemeOf` 原本只查内置 12 套，
 * 用户导进来一套配色、在设置页里选上它之后，终端会**静默回落默认色**——
 * 他刚做完一件事，界面毫无变化，而没有任何东西说明为什么。
 *
 * 第二条与第三条一起构成出口「新主题文件热加载 ≤1s 生效」的判据：
 * 广播是同步的（settingChanged 是 store，不是轮询），远小于 1 秒。
 */
describe("TerminalPane（自定义配色，M4b）", () => {
  const CUSTOM = {
    name: "MyDark",
    foreground: "#cccccc",
    background: "#1e1e2e",
    ansi: Array.from({ length: 16 }, (_, i) => `#0000${i.toString(16)}${i.toString(16)}`),
    cursor: "#f5e0dc",
  };

  beforeEach(() => {
    vi.clearAllMocks();
    termMock.state.handlers = null;
    termMock.state.opts = null;
    vi.mocked(settingGet).mockImplementation(async (key: string, fallback: unknown) => {
      if (key === "term.customSchemes") return [CUSTOM];
      if (key === "term.scheme") return "custom:MyDark";
      return fallback;
    });
  });

  it("选中的是导入的配色时，终端真的用它（而不是静默回落默认色）", async () => {
    await mountPane();
    const s = termMock.state.opts.scheme;
    expect(s.id).toBe("custom:MyDark");
    expect(s.background).toBe("#1e1e2e");
    expect(s.ansi).toEqual(CUSTOM.ansi);
    // 反向对照：确实不是默认那套
    expect(s.id).not.toBe(DEFAULT_SCHEME.id);
  });

  it("改了这套配色的内容 → 已打开的终端当场换色，不重建实例", async () => {
    await mountPane();
    expect(termMock.state.opts.scheme.background).toBe("#1e1e2e");

    bus.emit("term.customSchemes", [{ ...CUSTOM, background: "#004400" }]);
    await vi.waitFor(() =>
      expect(termMock.controller.setScheme).toHaveBeenLastCalledWith(
        expect.objectContaining({ id: "custom:MyDark", background: "#004400" }),
      ),
    );
    expect(termMock.createTerminal).toHaveBeenCalledTimes(1); // 换色不重建
  });

  it("那套被删掉之后回落默认色，而不是抓着一个不存在的 id 崩掉", async () => {
    await mountPane();
    bus.emit("term.customSchemes", []);
    await vi.waitFor(() =>
      expect(termMock.controller.setScheme).toHaveBeenLastCalledWith(DEFAULT_SCHEME),
    );
  });

  it("库里混进形状不合规的条目时整条跳过，不把坏数据喂给 xterm", async () => {
    // 这些值直接进 xterm 的 theme 对象。一条坏数据能把整个终端渲染搞崩，
    // 而那种崩法（白屏/无字）用户完全无从判断原因。
    await mountPane();
    bus.emit("term.customSchemes", [{ name: "MyDark", foreground: "#cccccc" }]); // 缺 background/ansi
    await vi.waitFor(() =>
      expect(termMock.controller.setScheme).toHaveBeenLastCalledWith(DEFAULT_SCHEME),
    );
  });
});

/**
 * 断线自动重连的 **banner 三态**（交叉审计 2026-08-25）。
 *
 * 此前本文件对 `disconnect-banner` 零覆盖——退避秒数有
 * `session_cmd.rs::reconnect_delay_*` 钉着，但「断线后用户到底看见什么、
 * 能不能手动拉回来」这一整块没有任何断言。而这块恰恰是断线时用户唯一
 * 能感知、能操作的面：文案错一格，用户要么以为连接还活着，要么以为彻底断了。
 *
 * 三态由后端 `session:disconnected` 事件的 `attempt` / `gave_up` 驱动：
 *   · attempt=0            → 「连接断开，准备重连…」（还没开始试）
 *   · attempt≥1 且未放弃    → 「正在重连（第 N 次）」+ 立即重连 / 停止重连
 *   · gave_up              → 「重连失败（已达上限）」+ 立即重连
 */
describe("TerminalPane · 断线重连 banner", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    termMock.state.handlers = null;
  });

  /** 从 listen 的登记里取出 `session:disconnected` 的回调。 */
  function disconnectHandler(): (e: { payload: any }) => void {
    const call = vi
      .mocked(listen)
      .mock.calls.find((c) => c[0] === "session:disconnected");
    if (!call) throw new Error("组件未订阅 session:disconnected");
    return call[1] as (e: { payload: any }) => void;
  }

  function emitDisconnect(payload: Record<string, unknown>) {
    disconnectHandler()({ payload: { session_id: "s1", ...payload } });
  }

  const banner = () => document.querySelector(".disconnect-banner") as HTMLElement | null;

  it("attempt=0 → 「连接断开，准备重连」，此刻还没有重连按钮", async () => {
    await mountPane();
    emitDisconnect({ reason: "transport", attempt: 0 });
    await vi.waitFor(() => expect(banner()).not.toBeNull());
    expect(banner()!.textContent).toContain("连接断开");
    expect(banner()!.textContent).toContain("准备重连");
    // 还没开始重试，给用户一个「立即重连」没有意义，反而暗示「点了才会重连」。
    expect(banner()!.textContent).not.toContain("立即重连");
  });

  it("attempt≥1 → 「正在重连（第 N 次）」，且给出立即重连/停止重连两个出口", async () => {
    await mountPane();
    emitDisconnect({ reason: "transport", attempt: 3 });
    await vi.waitFor(() => expect(banner()).not.toBeNull());
    expect(banner()!.textContent).toContain("正在重连");
    expect(banner()!.textContent).toContain("第 3 次");
    expect(banner()!.textContent).toContain("立即重连");
    expect(banner()!.textContent).toContain("停止重连");
  });

  it("gave_up → 「重连失败（已达上限）」，只留立即重连（停止已无意义）", async () => {
    await mountPane();
    emitDisconnect({ reason: "transport", attempt: 6, gave_up: true });
    await vi.waitFor(() => expect(banner()).not.toBeNull());
    expect(banner()!.textContent).toContain("重连失败");
    expect(banner()!.textContent).toContain("已达上限");
    expect(banner()!.textContent).toContain("立即重连");
    expect(banner()!.textContent).not.toContain("停止重连");
  });

  it("exit_nonzero 时把远端退出码报出来——那是排障的第一线索", async () => {
    await mountPane();
    emitDisconnect({ reason: "exit_nonzero", attempt: 1, exit_code: 137 });
    await vi.waitFor(() => expect(banner()).not.toBeNull());
    expect(banner()!.textContent).toContain("137");
  });

  it("点「立即重连」→ 发 session_reconnect（带 sessionId）", async () => {
    await mountPane();
    emitDisconnect({ reason: "transport", attempt: 2 });
    await vi.waitFor(() => expect(banner()).not.toBeNull());
    const btn = [...document.querySelectorAll(".disconnect-banner button")].find(
      (b) => b.textContent === "立即重连",
    ) as HTMLButtonElement;
    expect(btn).toBeTruthy();
    await fireEvent.click(btn);
    expect(invoke).toHaveBeenCalledWith("session_reconnect", { sessionId: "s1" });
  });

  it("点「停止重连」→ 发 session_reconnect_stop", async () => {
    await mountPane();
    emitDisconnect({ reason: "transport", attempt: 2 });
    await vi.waitFor(() => expect(banner()).not.toBeNull());
    const btn = [...document.querySelectorAll(".disconnect-banner button")].find(
      (b) => b.textContent === "停止重连",
    ) as HTMLButtonElement;
    await fireEvent.click(btn);
    expect(invoke).toHaveBeenCalledWith("session_reconnect_stop", { sessionId: "s1" });
  });

  // 反向对照：别的会话的断线事件不得在本面板弹 banner——多标签时每个面板只认
  // 自己的 sessionId，否则一个会话断线会在所有标签上同时弹「连接断开」。
  it("别的会话的断线事件不触发本面板的 banner", async () => {
    await mountPane();
    disconnectHandler()({
      payload: { session_id: "some-other-session", reason: "transport", attempt: 1 },
    });
    // 给渲染一个机会再断言它确实没出现
    await new Promise((r) => setTimeout(r, 20));
    expect(banner()).toBeNull();
  });
});
