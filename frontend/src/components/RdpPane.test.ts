/**
 * RdpPane 组件测试（RDP 阶段 1/2）。
 *
 * 钉的是**界面会不会骗人 / 会不会误伤**那几处：
 * · 远端优雅收尾（用户在远端注销）不该弹「重新连接」——用户就是要退出；
 * · 意外断线才给重连入口，且断线覆盖层**盖在最后一帧上**而不是替换它；
 * · 键盘走扫描码且 extended 位不能丢（左方向键 vs 小键盘 4 的经典混淆）；
 * · 鼠标按下前先同步位置（「移动中点击」序列不能落到旧坐标）。
 */
import { describe, expect, it, vi, beforeEach } from "vitest";
import { claimFramePainter, createFrameChannel, hasFrameChannel, releaseFramePainter, resetFrameSinksForTest } from "../lib/rdp-frames";
import { render, screen, fireEvent, waitFor } from "@testing-library/svelte";

const calls: { cmd: string; args?: Record<string, unknown> }[] = [];
let throwOn: string | null = null;
/** 下一次 invoke 的定制返回（用后即清）。 */
let invokeOverride: ((cmd: string, args?: Record<string, unknown>) => unknown) | null = null;

/** 事件监听表：测试直接触发后端事件。 */
const listeners: Record<string, ((e: { payload: unknown }) => void)[]> = {};
/** 前端错误上报（画帧失败必须留痕，不能静默吞——见 2026-08-19 事故）。 */
const reported: { source: string; err: unknown }[] = [];

// 4d：重连路径会 createFrameChannel → new Channel()，jsdom 里没有
// `window.__TAURI_INTERNALS__` 会当场炸。桩掉（分发逻辑的测试在 rdp-frames.test.ts）。
vi.mock("@tauri-apps/api/core", () => ({
  Channel: class {
    onmessage: ((b: never) => void) | null = null;
  },
}));
vi.mock("../lib/ipc", () => ({
  reportFrontendError: (source: string, err: unknown) => {
    reported.push({ source, err });
  },
  invoke: async <T,>(cmd: string, args?: Record<string, unknown>): Promise<T> => {
    calls.push({ cmd, args });
    if (throwOn === cmd) throw new Error(`${cmd} 失败：认证被拒`);
    // 一次性定制（共享目录测试用）：设了就消费掉，之后回落默认。
    if (invokeOverride) {
      const f = invokeOverride;
      invokeOverride = null;
      return f(cmd, args) as T;
    }
    return undefined as T;
  },
  listen: async (name: string, cb: (e: { payload: unknown }) => void) => {
    (listeners[name] ??= []).push(cb);
    return () => {
      listeners[name] = (listeners[name] ?? []).filter((f) => f !== cb);
    };
  },
}));

import RdpPane from "./RdpPane.svelte";

function emit(name: string, payload: unknown): void {
  for (const cb of listeners[name] ?? []) cb({ payload });
}

beforeEach(() => {
  calls.length = 0;
  reported.length = 0;
  throwOn = null;
  for (const k of Object.keys(listeners)) delete listeners[k];
});

describe("RdpPane", () => {
  it("本地共享控件中的按键不发送给远程桌面", async () => {
    render(RdpPane, { sessionId: "local-controls" });
    const button = await screen.findByTestId("rdp-share-add");
    calls.length = 0;
    await fireEvent.keyDown(button, { key: "Enter", code: "Enter" });
    await fireEvent.keyUp(button, { key: "Enter", code: "Enter" });
    expect(calls.filter(c => c.cmd === "rdp_input")).toHaveLength(0);
  });
  it("Ctrl+Alt+Home 从远程桌面回到本地控件", async () => {
    render(RdpPane, { sessionId: "return-local" });
    const wrap = screen.getByTestId("rdp-wrap");
    wrap.focus();
    await fireEvent.keyDown(wrap, { key: "Home", code: "Home", ctrlKey: true, altKey: true });
    expect(document.activeElement).toBe(screen.getByTestId("rdp-share-add"));
  });
  it("远端优雅收尾不弹重连入口（用户就是要退出）", async () => {
    render(RdpPane, { props: { sessionId: "s1" } });
    await waitFor(() => expect(listeners["rdp:closed:s1"]?.length).toBe(1));
    emit("rdp:closed:s1", { sessionId: "s1", graceful: true });
    await new Promise((r) => setTimeout(r, 10));
    expect(screen.queryByTestId("rdp-disconnected")).toBeNull();
  });

  it("意外断线给出重连入口，且点按调到 rdp_reconnect", async () => {
    render(RdpPane, { props: { sessionId: "s2" } });
    await waitFor(() => expect(listeners["rdp:closed:s2"]?.length).toBe(1));
    emit("rdp:closed:s2", { sessionId: "s2", graceful: false });
    const btn = await screen.findByTestId("rdp-reconnect");
    await fireEvent.click(btn);
    await waitFor(() =>
      expect(calls.some((c) => c.cmd === "rdp_reconnect" && c.args?.sessionId === "s2")).toBe(true),
    );
  });

  it("断线覆盖层不替换画面：canvas 仍在（最后一帧留着）", async () => {
    render(RdpPane, { props: { sessionId: "s3" } });
    await waitFor(() => expect(listeners["rdp:closed:s3"]?.length).toBe(1));
    emit("rdp:closed:s3", { sessionId: "s3", graceful: false });
    await screen.findByTestId("rdp-disconnected");
    // 覆盖层出现的同时，画布必须还在——用户要看得见断线前的状态
    expect(screen.getByTestId("rdp-canvas")).toBeTruthy();
  });

  it("重连失败把后端的人话原样呈现（不吞成一句「失败」）", async () => {
    render(RdpPane, { props: { sessionId: "s4" } });
    await waitFor(() => expect(listeners["rdp:closed:s4"]?.length).toBe(1));
    emit("rdp:closed:s4", { sessionId: "s4", graceful: false });
    throwOn = "rdp_reconnect";
    await fireEvent.click(await screen.findByTestId("rdp-reconnect"));
    const err = await screen.findByTestId("rdp-reconnect-error");
    expect(err.textContent).toContain("认证被拒");
  });

  it("connected 事件把画布位图尺寸定为远端桌面分辨率", async () => {
    render(RdpPane, { props: { sessionId: "s5" } });
    await waitFor(() => expect(listeners["rdp:connected:s5"]?.length).toBe(1));
    emit("rdp:connected:s5", { width: 1024, height: 768 });
    await waitFor(() => {
      const c = screen.getByTestId("rdp-canvas") as HTMLCanvasElement;
      expect(c.width).toBe(1024);
      expect(c.height).toBe(768);
    });
  });

  it("**扩展键的 extended 位不能丢**（左方向键 ≠ 小键盘 4）", async () => {
    render(RdpPane, { props: { sessionId: "s6" } });
    const wrap = screen.getByTestId("rdp-wrap");
    await fireEvent.keyDown(wrap, { code: "ArrowLeft" });
    await waitFor(() => {
      const c = calls.find((x) => x.cmd === "rdp_input");
      expect(c?.args?.event).toMatchObject({
        kind: "key",
        scancode: 0x4b,
        extended: true,
        down: true,
      });
    });
  });

  it("普通键 extended=false（同一张表两侧都要对）", async () => {
    render(RdpPane, { props: { sessionId: "s7" } });
    await fireEvent.keyDown(screen.getByTestId("rdp-wrap"), { code: "KeyA" });
    await waitFor(() => {
      const c = calls.find((x) => x.cmd === "rdp_input");
      expect(c?.args?.event).toMatchObject({ scancode: 0x1e, extended: false });
    });
  });

  it("Ctrl+W 不透传给远端（本地快捷键留给本地）", async () => {
    render(RdpPane, { props: { sessionId: "s8" } });
    await fireEvent.keyDown(screen.getByTestId("rdp-wrap"), {
      code: "KeyW",
      key: "w",
      ctrlKey: true,
    });
    await new Promise((r) => setTimeout(r, 10));
    expect(calls.some((c) => c.cmd === "rdp_input")).toBe(false);
  });

  // 帧级背压：**每帧必须恰好一条回执**。helper 侧是配额计数
  // （在途上限 2），合并/漏发回执会让配额只减不还——几帧之后画面
  // 永久停住，而且没有任何报错。这条判据钉的就是那个一对一。
  it("每收一帧回一条 rdp_frame_ack（配额不能只减不还）", async () => {
    render(RdpPane, { props: { sessionId: "s10" } });
    await waitFor(() => expect(listeners["rdp:frame:s10"]?.length).toBe(1));
    // 尺寸先定格，否则 paint 提前返回（ctx 为空）也不影响回执，但让用例更接近真实
    emit("rdp:connected:s10", { width: 64, height: 64 });
    for (let i = 0; i < 3; i++) {
      emit("rdp:frame:s10", { x: 0, y: 0, w: 0, h: 0, rgbaB64: "" });
    }
    await waitFor(() =>
      expect(calls.filter((c) => c.cmd === "rdp_frame_ack").length).toBe(3),
    );
    expect(
      calls.filter((c) => c.cmd === "rdp_frame_ack").every((c) => c.args?.sessionId === "s10"),
    ).toBe(true);
  });

  // **画帧抛异常时回执仍然必须发出**——这条判据钉的是一个会「画面永久冻死
  // 但鼠标键盘还通、日志里什么都没有」的死锁：helper 侧配额只减不还，
  // 漏两次就再也不发帧了。同时要索要整屏重绘（帧是增量的，画失败那块
  // 没有任何机制会自愈）。
  //
  // 载荷必须是**会真的把 paint 打挂**的那种：w/h 与实际字节数不符，
  // new ImageData 会抛。之前那条判据发的是 w:0,h:0，在 paint 第一行就
  // return 了——结构上不可能测到这个 bug。
  it("画帧抛异常也照发回执，并索要整屏（否则画面永久冻死）", async () => {
    // jsdom 的 canvas 没有 2d 上下文（getContext 返回 null），paint 会在
    // 第二行就 return——那样测不到我们要钉的东西。注入一个**会抛**的上下文，
    // 模拟真实 webview 里 putImageData 失败（尺寸不符/内存不足）。
    const proto = window.HTMLCanvasElement.prototype as unknown as {
      getContext: unknown;
    };
    const original = proto.getContext;
    proto.getContext = () => ({
      putImageData: () => {
        throw new Error("putImageData 炸了");
      },
    });
    try {
      render(RdpPane, { props: { sessionId: "s11" } });
      await waitFor(() => expect(listeners["rdp:frame:s11"]?.length).toBe(1));
      emit("rdp:connected:s11", { width: 64, height: 64 });
      calls.length = 0; // 忽略 connected 触发的那次整屏请求

      emit("rdp:frame:s11", { x: 0, y: 0, w: 1, h: 1, rgbaB64: "AAAAAA==" });

      await waitFor(() =>
        expect(calls.some((c) => c.cmd === "rdp_frame_ack")).toBe(true),
      );
      expect(calls.some((c) => c.cmd === "rdp_request_full_frame")).toBe(true);
      // 静默吞掉是 2026-08-19 渲染帧丢失事故的根因：必须留痕
      expect(reported.some((r) => r.source === "rdp:paint")).toBe(true);
    } finally {
      proto.getContext = original;
    }
  });

  // 画布位图尺寸变化会**清空**画布（canvas 的 width/height 属性是这个语义），
  // 而帧是增量的——不索要整屏，用户盯着空白等远端自己重绘。
  it("connected（含重激活改分辨率）之后索要整屏重绘", async () => {
    render(RdpPane, { props: { sessionId: "s12" } });
    await waitFor(() => expect(listeners["rdp:connected:s12"]?.length).toBe(1));
    emit("rdp:connected:s12", { width: 800, height: 600 });
    await waitFor(() =>
      expect(
        calls.some(
          (c) => c.cmd === "rdp_request_full_frame" && c.args?.sessionId === "s12",
        ),
      ).toBe(true),
    );
  });

  // 扫描码四处硬错（2026-08-27 对表上游 web-client 的 scancodes.ts 发现）：
  // 小键盘 * 与 - 误置扩展表 → 发出 E0 37(=PrintScreen) 与 E0 4A(无定义)；
  // PrintScreen 误置主区 → 发裸 0x37(=小键盘 *)；Win 键漏扩展位 → 开始菜单
  // 按不出来。四条对**所有用户、所有布局**当前失效，故逐条钉死。
  it.each([
    ["NumpadMultiply", 0x37, false, "小键盘 * 是裸码，带 E0 会变成 PrintScreen"],
    ["NumpadSubtract", 0x4a, false, "小键盘 - 是裸码，带 E0 是无定义键"],
    ["PrintScreen", 0x37, true, "PrintScreen 是 E0 37，裸 0x37 是小键盘 *"],
    ["MetaLeft", 0x5b, true, "Win 键必须带扩展位，否则开始菜单不弹"],
    ["MetaRight", 0x5c, true, "同左 Win 键"],
    ["NumpadDivide", 0x35, true, "小键盘 / 是 E0 35（与主区 Slash 的裸 0x35 区分）"],
  ])("扫描码 %s = %s (extended=%s)", async (code, scancode, extended, why) => {
    render(RdpPane, { props: { sessionId: `sk-${code}` } });
    await fireEvent.keyDown(screen.getByTestId("rdp-wrap"), { code });
    await waitFor(() => {
      const c = calls.find((x) => x.cmd === "rdp_input");
      expect(c?.args?.event, why).toMatchObject({ scancode, extended });
    });
  });

  // 失焦黏键：按下与松开是两条独立事件，失焦之后松开**不会再来**。
  // 用户按住 Alt 摁 Tab 切走，远端就一直认为 Alt 按着——切回来每个字母
  // 都成了 Alt+ 组合键。mstsc 在这里会复位，我们此前全仓零处理。
  it.each(["focusout", "mouseleave"])("%s 时让远端松开所有按键", async (evName) => {
    render(RdpPane, { props: { sessionId: "s13" } });
    const wrap = screen.getByTestId("rdp-wrap");
    calls.length = 0;
    await fireEvent(wrap, new Event(evName, { bubbles: true }));
    await waitFor(() =>
      expect(
        calls.some(
          (c) =>
            c.cmd === "rdp_input" &&
            (c.args?.event as { kind?: string })?.kind === "release_all",
        ),
      ).toBe(true),
    );
  });

  // 右键菜单拦在**容器**上：画布与窗口宽高比不一致时画布外有一圈黑边，
  // 右键黑边/断线覆盖层曾弹出 WebView 的右键菜单——用户在「远程桌面」里
  // 看到浏览器菜单既出戏也丢事件。preventDefault 必须挂在 rdp-wrap 上
  // （只挂画布挡不住黑边），事件冒泡路径上任何元素触发都该被吃掉。
  it("右键不弹浏览器菜单——容器层吃掉 contextmenu（黑边也覆盖）", async () => {
    render(RdpPane, { props: { sessionId: "s9" } });
    const wrap = screen.getByTestId("rdp-wrap");
    const ev = new Event("contextmenu", { bubbles: true, cancelable: true });
    // 在画布上触发，靠冒泡到容器——验证的是容器层防线，不是画布自己的
    wrap.querySelector("canvas")!.dispatchEvent(ev);
    expect(ev.defaultPrevented).toBe(true);
    // 在容器本体（黑边区域）直接触发，同样必须被吃掉
    const ev2 = new Event("contextmenu", { bubbles: true, cancelable: true });
    wrap.dispatchEvent(ev2);
    expect(ev2.defaultPrevented).toBe(true);
  });
});

/* ══════════════════ 4d：raw 帧通道（tauri Channel）══════════════════ */

/**
 * raw 路径（`lib/rdp-frames.ts`）在 RdpPane 里的接线。事件路径（回落用）的判据
 * 在上面原有各条里，没动——两条路径的**绘制核心**是同一个 `paintBytes`，
 * 拆分本身由 `两条路径汇到同一个绘制核心` 那条源码级判据钉住。
 */
describe("RdpPane · raw 帧通道（4d）", () => {
  // 拿的就是真实现（不经 IPC）：这个 describe 测的是组件与 sink 表的交互本身。
  // import 在文件顶部（describe 回调是同步的，不能 await）。
  beforeEach(() => {
    resetFrameSinksForTest();
  });

  /** 造一条与 Rust frame_wire 同格式的 raw 载荷。 */
  function rawFrame(x: number, y: number, w: number, h: number): ArrayBuffer {
    const buf = new ArrayBuffer(16 + w * h * 4);
    const dv = new DataView(buf);
    dv.setUint32(0, x, true);
    dv.setUint32(4, y, true);
    dv.setUint32(8, w, true);
    dv.setUint32(12, h, true);
    return buf;
  }

  it("有通道时不听 rdp:frame 事件（双路径并存会双画/双 ack）", async () => {
    createFrameChannel("s-raw1");
    render(RdpPane, { props: { sessionId: "s-raw1" } });
    await waitFor(() => expect(hasFrameChannel("s-raw1")).toBe(true));
    await new Promise((r) => setTimeout(r, 20));
    expect(listeners["rdp:frame:s-raw1"]).toBeUndefined();
  });

  it("认领前到达的帧：挂载即补画并补 ack（漏 ack = 配额永久少一格）", async () => {
    const ch = createFrameChannel("s-raw2");
    // 画布还没挂——帧进缓存
    (ch as unknown as { onmessage: (b: ArrayBuffer) => void }).onmessage(rawFrame(0, 0, 1, 1));
    calls.length = 0;
    render(RdpPane, { props: { sessionId: "s-raw2" } });
    await waitFor(() => expect(calls.some((c) => c.cmd === "rdp_frame_ack" && c.args?.sessionId === "s-raw2")).toBe(true));
  });

  it("认领后的帧直达绘制：坏载荷上报 rdp:paint、发 ack、索要整屏", async () => {
    const ch = createFrameChannel("s-raw3");
    render(RdpPane, { props: { sessionId: "s-raw3" } });
    await waitFor(() => expect(hasFrameChannel("s-raw3")).toBe(true));
    await new Promise((r) => setTimeout(r, 20));
    calls.length = 0;
    reported.length = 0;
    // 头说 2×2 但载荷只有 1 像素——decodeFrame 抛「像素不足」
    const bad = rawFrame(0, 0, 2, 2);
    const trimmed = bad.slice(0, 16 + 4);
    (ch as unknown as { onmessage: (b: ArrayBuffer) => void }).onmessage(trimmed);
    await waitFor(() => expect(reported.some((r) => r.source === "rdp:paint")).toBe(true));
    expect(calls.some((c) => c.cmd === "rdp_frame_ack")).toBe(true);
    expect(calls.some((c) => c.cmd === "rdp_request_full_frame")).toBe(true);
  });

  it("卸载只解绑回调，通道留在表里（切标签回来还用它）", async () => {
    const ch = createFrameChannel("s-raw4");
    const { unmount } = render(RdpPane, { props: { sessionId: "s-raw4" } });
    await waitFor(() => expect(hasFrameChannel("s-raw4")).toBe(true));
    unmount();
    expect(hasFrameChannel("s-raw4")).toBe(true);
    // 解绑后帧只进缓存，不炸
    (ch as unknown as { onmessage: (b: ArrayBuffer) => void }).onmessage(rawFrame(0, 0, 1, 1));
    expect(hasFrameChannel("s-raw4")).toBe(true);
  });

  it("两条路径汇到同一个绘制核心（源码级：拆出 paintBytes 后不得分叉）", async () => {
    const src = await import("../components/RdpPane.svelte?raw").then((m) => m.default as string);
    expect(src).toMatch(/function paintBytes\(/);
    // 事件壳解出字节后调 paintBytes；raw 壳解出头后也调 paintBytes
    expect(src).toMatch(/paintBytes\(\{ x: p\.x, y: p\.y, w: p\.w, h: p\.h, rgba: bytes \}\)/);
    expect(src).toMatch(/paintBytes\(\{ x: f\.x, y: f\.y, w: f\.w, h: f\.h, rgba: f\.rgba \}\)/);
    // 组件自己不得再实现第二份「字节 → ImageData」
    expect(src.match(/new ImageData\(/g)?.length).toBe(1);
    void claimFramePainter;
    void releaseFramePainter;
  });
});

/* ══════════════════ 共享目录（RDPDR 批）══════════════════ */

/**
 * 共享面是 RDP 的真安全面（出口标准原话）：共享哪个目录、是否只读、每次挂载
 * 写审计，三件缺一不可。前端这半边钉前两件的路由与呈现——第三件在后端
 * （rdp_share.rs 的单测 + audit_cmd 全局门禁）。
 */
describe("RdpPane · 共享目录（RDPDR）", () => {
  const q = (id: string) => document.querySelector(`[data-testid="${id}"]`) as HTMLElement | null;

  it("没有挂载时入口在、只读开关默认开（第一次共享先从只读开始）", async () => {
    render(RdpPane, { props: { sessionId: "s-sh1" } });
    await waitFor(() => expect(q("rdp-share-add")).not.toBeNull());
    expect((q("rdp-share-readonly") as HTMLInputElement).checked).toBe(true);
  });

  it("源码级：挂载前必须过 fs.share 确认闸；卸载不需要（卸载是收窄权限）", async () => {
    const src = (await import("./RdpPane.svelte?raw")).default as string;
    expect(src).toMatch(/kind: "fs\.share"/);
    expect(src.indexOf("confirmAction({")).toBeLessThan(src.indexOf('"rdp_share_mount"'));
    expect(src).not.toMatch(/doUnmount[\s\S]{0,260}confirmAction/);
  });

  it("确认框的命令全文 = 用户正在批准的那句话（目录 + 读写模式），可写档危险色", async () => {
    const src = (await import("./RdpPane.svelte?raw")).default as string;
    expect(src).toMatch(/command: `远端桌面将\$\{readonly \? "只读" : "读写"\}本机目录：\$\{dir\}`/);
    expect(src).toMatch(/danger: !readonly/);
  });

  it("已挂的盘逐个列出（只读/可写标注），点 × 调卸载", async () => {
    invokeOverride = (cmd: string) =>
      cmd === "rdp_share_status" ? [{ device: 1, mounted: true, readonly: true }] : undefined;
    render(RdpPane, { props: { sessionId: "s-sh3" } });
    await waitFor(() => expect(q("rdp-share-chip-1")).not.toBeNull());
    expect(q("rdp-share-chip-1")!.textContent).toContain("只读");
    await fireEvent.click(q("rdp-share-unmount-1")!);
    await waitFor(() =>
      expect(calls.some((c) => c.cmd === "rdp_share_unmount" && c.args?.device === 1)).toBe(true),
    );
  });

  it("挂载/卸载失败要把后端的人话显示出来（静默 = 用户以为共享上了）", async () => {
    const src = (await import("./RdpPane.svelte?raw")).default as string;
    expect(src).toMatch(/挂载失败：\$\{e\}/);
    expect(src).toMatch(/卸载失败：\$\{e\}/);
  });

  it("四个槽位满后不再显示「共享目录…」（限的是用户管理的心智，不是协议）", async () => {
    const src = (await import("./RdpPane.svelte?raw")).default as string;
    expect(src).toMatch(/\{#if shares\.length < 4\}/);
  });

  it("后端形状异常（非数组）退空表而不是炸画布", async () => {
    invokeOverride = () => "not-an-array";
    render(RdpPane, { props: { sessionId: "s-sh7" } });
    await waitFor(() => expect(q("rdp-share-add")).not.toBeNull());
    expect(q("rdp-share-add")).not.toBeNull();
  });
});

/** D11 守卫：枚举少了 fs.share 时 vitest 不做类型检查，必须显式断言。 */
describe("RdpPane · fs.share 类别的存在性", () => {
  it("action-confirm 的枚举与人读名都有 fs.share（两端白名单的锚）", async () => {
    const mod = await import("../lib/action-confirm");
    expect(mod.ACTION_LABEL["fs.share"]).toBeTruthy();
    expect((mod.ALL_ACTIONS as string[]).includes("fs.share")).toBe(true);
  });
});
