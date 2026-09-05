import { describe, expect, it, vi } from "vitest";

// Task 20 Step 4 前置：mock @tauri-apps/api/core 的 invoke（剪贴板通道红测固化，S287）
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

// S280：键盘装配测试需要受控的仲裁结果——mock 掉 shortcuts/tabs 两模块（term.ts 运行期依赖），
// 逐分支设定 arbitrate/actionForKey/toggleKeyboardMode/tabKeyboardMode 的返回。
vi.mock("./shortcuts", () => ({
  actionForKey: vi.fn(() => ({ kind: "passthrough" })),
  arbitrate: vi.fn(() => "passthrough"),
  toggleKeyboardMode: vi.fn(),
}));
vi.mock("./tabs", () => ({
  tabKeyboardMode: vi.fn(() => "remote"),
}));

import {
  clampScrollbackLines, clipboardWrite, clipboardRead, contextMenuAction, ctrlCAction, decodeB64,
  clampFontSize, encodeB64, FONT_SIZE_MAX, FONT_SIZE_MIN, makeTermKeyHandler, pasteConfirmLines,
  shouldCopyOnSelect, withAlpha, type TermKeyDeps,
} from "./term";
import { actionForKey, arbitrate, toggleKeyboardMode } from "./shortcuts";
import { invoke } from "@tauri-apps/api/core";

describe("b64 codec", () => {
  it("roundtrips utf8 and binary", () => {
    const bytes = new TextEncoder().encode("服务器 ok \u0000");
    expect(decodeB64(encodeB64(bytes))).toEqual(bytes);
  });
});

describe("滚动行数钳位（审计2 #35，与 Rust 侧 SCROLLBACK_MAX 同源）", () => {
  it("缺省/未设置 → 10000（与后端 DEFAULT_SCROLLBACK_LINES 一致）", () => {
    expect(clampScrollbackLines(undefined)).toBe(10000);
    expect(clampScrollbackLines(null)).toBe(10000);
  });

  it("档案值原样生效（旧实现恒 10000，档案改多少都不生效——死配置）", () => {
    expect(clampScrollbackLines(5000)).toBe(5000);
    expect(clampScrollbackLines(20000)).toBe(20000);
  });

  it("越界钳进 [1, 1_000_000]：上界、下界、非有限值", () => {
    expect(clampScrollbackLines(1_000_000)).toBe(1_000_000);
    expect(clampScrollbackLines(5_000_000)).toBe(1_000_000);
    expect(clampScrollbackLines(0)).toBe(1);
    expect(clampScrollbackLines(-7)).toBe(1);
    expect(clampScrollbackLines(Number.NaN)).toBe(10000);
    expect(clampScrollbackLines(Number.POSITIVE_INFINITY)).toBe(10000);
  });
});

describe("终端交互仲裁（UI 规格 §2.8/§4，纯函数无 DOM 依赖）", () => {
  const C = { ctrlKey: true, shiftKey: false, altKey: false, metaKey: false, key: "c" };

  it("Ctrl+C 双义：有选区 → 复制（拦截），无选区 → 放行发 \\x03", () => {
    expect(ctrlCAction(C, true)).toBe("copy");
    expect(ctrlCAction(C, false)).toBe("passthrough");
  });

  it("Ctrl+Shift+C 恒复制（无选区亦拦截，恒本地白名单 §2.7）", () => {
    expect(ctrlCAction({ ...C, shiftKey: true }, false)).toBe("copy");
  });

  it("非 Ctrl 或非 c 键不参与本仲裁", () => {
    expect(ctrlCAction({ ...C, ctrlKey: false }, true)).toBe("ignore");
    expect(ctrlCAction({ ...C, key: "v" }, true)).toBe("ignore");
  });

  it("右键分流两态：paste 模式无选区 → 粘贴、非空选区 → 菜单；menu 模式恒弹菜单", () => {
    expect(contextMenuAction("paste", false)).toBe("paste");
    expect(contextMenuAction("paste", true)).toBe("menu");
    expect(contextMenuAction("menu", false)).toBe("menu");
  });

  it("多行粘贴走确认仲裁：多行且未记住 → 返回行数；单行/已记住/开关关 → 直接粘贴", () => {
    expect(pasteConfirmLines("a\nb\nc", { multilinePasteConfirm: true, remembered: false })).toBe(3);
    expect(pasteConfirmLines("abc", { multilinePasteConfirm: true, remembered: false })).toBeNull();
    expect(pasteConfirmLines("a\nb", { multilinePasteConfirm: true, remembered: true })).toBeNull();
    expect(pasteConfirmLines("a\nb", { multilinePasteConfirm: false, remembered: false })).toBeNull();
  });

  it("S278 尾随换行不多计一行：终端复制的单命令几乎必然带尾 \\n（LF/CRLF 同判）", () => {
    expect(pasteConfirmLines("ls -la\n", { multilinePasteConfirm: true, remembered: false })).toBe(1);
    expect(pasteConfirmLines("ls -la\r\n", { multilinePasteConfirm: true, remembered: false })).toBe(1);
    expect(pasteConfirmLines("a\nb\n", { multilinePasteConfirm: true, remembered: false })).toBe(2);
    expect(pasteConfirmLines("a\r\nb\r\nc\r\n", { multilinePasteConfirm: true, remembered: false })).toBe(3);
  });

  it("选择即复制：开关开且选区非空才触发", () => {
    expect(shouldCopyOnSelect(true, "sel")).toBe(true);
    expect(shouldCopyOnSelect(true, "")).toBe(false);
    expect(shouldCopyOnSelect(false, "sel")).toBe(false);
  });
});

describe("剪贴板通道（Task 20 Step 4 前置，S287：arboard IPC 择一定稿 + 红测固化）", () => {
  it("clipboardWrite 调 clipboard_write 命令，空串 no-op（S277 前端守卫）", async () => {
    vi.mocked(invoke).mockResolvedValueOnce(undefined);
    await clipboardWrite("test content");
    expect(invoke).toHaveBeenCalledWith("clipboard_write", { text: "test content" });

    vi.mocked(invoke).mockClear();
    await clipboardWrite(""); // 空串短路，不调命令
    expect(invoke).not.toHaveBeenCalled();
  });

  it("clipboardWrite arboard 失败时降级不抛（四路复制入口均不阻断）", async () => {
    vi.mocked(invoke).mockRejectedValueOnce(new Error("arboard init failed"));
    await expect(clipboardWrite("data")).resolves.toBeUndefined(); // 不抛异常
  });

  it("clipboardRead 调 clipboard_read 命令并返回内容", async () => {
    vi.mocked(invoke).mockResolvedValueOnce("clipboard data");
    const result = await clipboardRead();
    expect(invoke).toHaveBeenCalledWith("clipboard_read");
    expect(result).toBe("clipboard data");
  });

  it("clipboardRead arboard 失败时回空串降级（粘贴路径 if(t) 守卫会跳过）", async () => {
    vi.mocked(invoke).mockRejectedValueOnce(new Error("arboard read failed"));
    const result = await clipboardRead();
    expect(result).toBe("");
  });
});

describe("S280 ack 字节等式前置不变式", () => {
  it("多字节载荷 b64 长度 ≠ 字节数：ack 必须按解码字节数计，按 b64 长度计会虚增", () => {
    const bytes = new TextEncoder().encode("服务器");
    const b64 = encodeB64(bytes);
    expect(b64.length).not.toBe(bytes.length);
    expect(decodeB64(b64).length).toBe(bytes.length);
  });
});

describe("S280 键盘装配 makeTermKeyHandler（§2.7/§2.8 分支 + stopPropagation 不变式）", () => {
  function deps(over: Partial<TermKeyDeps> = {}): TermKeyDeps {
    return {
      sessionId: "s1",
      hasSelection: () => false,
      copySelection: vi.fn(),
      pasteFromClipboard: vi.fn(),
      zoom: vi.fn(),
      resetFont: vi.fn(),
      interactions: () => ({ rightClick: "paste", copyOnSelect: false, multilinePasteConfirm: true, ctrlVPaste: true }),
      ...over,
    };
  }
  function key(over: Record<string, unknown> = {}): KeyboardEvent {
    return {
      type: "keydown", ctrlKey: false, shiftKey: false, altKey: false, metaKey: false,
      key: "", code: "", preventDefault: vi.fn(), stopPropagation: vi.fn(), ...over,
    } as unknown as KeyboardEvent;
  }

  it("Ctrl+C 有选区 → copySelection + 拦截（preventDefault/stopPropagation），无选区 → 透传", () => {
    const d = deps({ hasSelection: () => true });
    const e = key({ ctrlKey: true, key: "c" });
    expect(makeTermKeyHandler(d)(e)).toBe(false);
    expect(d.copySelection).toHaveBeenCalled();
    expect(e.preventDefault).toHaveBeenCalled();
    expect(e.stopPropagation).toHaveBeenCalled();
    const d2 = deps();
    const e2 = key({ ctrlKey: true, key: "c" });
    expect(makeTermKeyHandler(d2)(e2)).toBe(true);
    expect(e2.preventDefault).not.toHaveBeenCalled();
  });

  it("Ctrl+Shift+C 无选区亦拦截走复制（恒本地，路由不变；空写副作用由 clipboardWrite 短路兜住）", () => {
    const d = deps();
    const e = key({ ctrlKey: true, shiftKey: true, key: "c" });
    expect(makeTermKeyHandler(d)(e)).toBe(false);
    expect(d.copySelection).toHaveBeenCalled();
  });

  it("Ctrl+Shift+V 恒粘贴：读剪贴板入口 + 拦截", () => {
    const d = deps();
    const e = key({ ctrlKey: true, shiftKey: true, key: "v" });
    expect(makeTermKeyHandler(d)(e)).toBe(false);
    expect(d.pasteFromClipboard).toHaveBeenCalled();
    expect(e.stopPropagation).toHaveBeenCalled();
  });

  it("Ctrl+V：ctrlVPaste 开 → 就地截获走粘贴；关 + 远程 passthrough → 放行 \\x16；关 + local → 拦截不放行", () => {
    const on = deps();
    const eOn = key({ ctrlKey: true, key: "v" });
    expect(makeTermKeyHandler(on)(eOn)).toBe(false);
    expect(on.pasteFromClipboard).toHaveBeenCalled();

    vi.mocked(arbitrate).mockReturnValueOnce("passthrough");
    const off = deps({ interactions: () => ({ rightClick: "paste", copyOnSelect: false, multilinePasteConfirm: true, ctrlVPaste: false }) });
    expect(makeTermKeyHandler(off)(key({ ctrlKey: true, key: "v" }))).toBe(true);

    vi.mocked(arbitrate).mockReturnValueOnce("local");
    const offLocal = deps({ interactions: () => ({ rightClick: "paste", copyOnSelect: false, multilinePasteConfirm: true, ctrlVPaste: false }) });
    const eOff = key({ ctrlKey: true, key: "v" });
    expect(makeTermKeyHandler(offLocal)(eOff)).toBe(false);
    expect(eOff.stopPropagation).toHaveBeenCalled();
  });

  it("Ctrl+= / Ctrl+- / Ctrl+0 → zoom(+1) / zoom(-1) / resetFont，均拦截", () => {
    const d = deps();
    const h = makeTermKeyHandler(d);
    const eq = key({ ctrlKey: true, code: "Equal", key: "=" });
    expect(h(eq)).toBe(false);
    expect(d.zoom).toHaveBeenCalledWith(1);
    const minus = key({ ctrlKey: true, code: "Minus", key: "-" });
    expect(h(minus)).toBe(false);
    expect(d.zoom).toHaveBeenCalledWith(-1);
    const zero = key({ ctrlKey: true, code: "Digit0", key: "0" });
    expect(h(zero)).toBe(false);
    expect(d.resetFont).toHaveBeenCalled();
  });

  it("通用仲裁 local + toggle-mode（Scroll Lock）→ toggleKeyboardMode(sessionId)（F25）", () => {
    vi.mocked(arbitrate).mockReturnValueOnce("local");
    vi.mocked(actionForKey).mockReturnValueOnce({ kind: "toggle-mode" });
    const d = deps();
    const e = key({ key: "ScrollLock" });
    expect(makeTermKeyHandler(d)(e)).toBe(false);
    expect(vi.mocked(toggleKeyboardMode)).toHaveBeenCalledWith("s1");
    expect(e.stopPropagation).toHaveBeenCalled();
  });

  it("通用仲裁 local + action：注入 onLocalAction → 直投动作 id（R58）；未注入 → 拦截但不阻冒泡", () => {
    vi.mocked(arbitrate).mockReturnValueOnce("local");
    vi.mocked(actionForKey).mockReturnValueOnce({ kind: "action", id: "edit.find" });
    const onLocalAction = vi.fn();
    const d = deps({ onLocalAction });
    const e = key({ ctrlKey: true, key: "f" });
    expect(makeTermKeyHandler(d)(e)).toBe(false);
    expect(onLocalAction).toHaveBeenCalledWith("edit.find");
    expect(e.stopPropagation).toHaveBeenCalled();

    vi.mocked(arbitrate).mockReturnValueOnce("local");
    vi.mocked(actionForKey).mockReturnValueOnce({ kind: "action", id: "edit.find" });
    const d2 = deps(); // 未注入 onLocalAction
    const e2 = key({ ctrlKey: true, key: "f" });
    expect(makeTermKeyHandler(d2)(e2)).toBe(false);
    expect(e2.stopPropagation).not.toHaveBeenCalled(); // 动作交窗口级派发器于冒泡阶段执行
  });

  it("非 keydown 事件一律放行（click/keyup 不参与仲裁）", () => {
    const d = deps();
    expect(makeTermKeyHandler(d)(key({ type: "keyup", ctrlKey: true, key: "c" }))).toBe(true);
  });

  it("S272 气泡悬置期 Enter/Escape 就地消费防 \\r 误发，但不阻冒泡（结算在窗口级兜底）", () => {
    const d = deps({ pasteConfirmPending: () => true });
    const eEnter = key({ key: "Enter" });
    expect(makeTermKeyHandler(d)(eEnter)).toBe(false);
    expect(eEnter.preventDefault).toHaveBeenCalled();
    expect(eEnter.stopPropagation).not.toHaveBeenCalled(); // 窗口级兜底于冒泡阶段结算
    expect(makeTermKeyHandler(d)(key({ key: "Escape" }))).toBe(false);
    const d2 = deps({ pasteConfirmPending: () => false });
    expect(makeTermKeyHandler(d2)(key({ key: "Enter" }))).toBe(true); // 无气泡：照常透传
  });
});

describe("剪贴板通道（Task 20 Step 4 前置，S287：arboard IPC 择一定稿 + 红测固化）", () => {
  it("clipboardWrite 调 clipboard_write 命令，空串 no-op（S277 前端守卫）", async () => {
    vi.mocked(invoke).mockResolvedValueOnce(undefined);
    await clipboardWrite("test content");
    expect(invoke).toHaveBeenCalledWith("clipboard_write", { text: "test content" });

    vi.mocked(invoke).mockClear();
    await clipboardWrite(""); // 空串短路，不调命令
    expect(invoke).not.toHaveBeenCalled();
  });

  it("clipboardWrite arboard 失败时降级不抛（四路复制入口均不阻断）", async () => {
    vi.mocked(invoke).mockRejectedValueOnce(new Error("arboard init failed"));
    await expect(clipboardWrite("data")).resolves.toBeUndefined(); // 不抛异常
  });

  it("clipboardRead 调 clipboard_read 命令并返回内容", async () => {
    vi.mocked(invoke).mockResolvedValueOnce("clipboard data");
    const result = await clipboardRead();
    expect(invoke).toHaveBeenCalledWith("clipboard_read");
    expect(result).toBe("clipboard data");
  });

  it("clipboardRead arboard 失败时回空串降级（粘贴路径 if(t) 守卫会跳过）", async () => {
    vi.mocked(invoke).mockRejectedValueOnce(new Error("arboard read failed"));
    const result = await clipboardRead();
    expect(result).toBe("");
  });
});

/**
 * 背景透明度的 alpha 改写（M1 出口「背景透明度」条）。
 *
 * 组件层已有「广播 term.opacity → setOpacity 被调用」的断言，但那只证明**调用发生了**。
 * 出口原文的两句实话——「转半透明」与「置 100 恢复不透明」——落在算出来的那个颜色值上，
 * 而 setOpacity 的替身对它一无所知。「>= 100」那条短路分支尤其无人证：写成 「> 100」
 * 时 100 会走进 rgba 分支产出 rgba(r,g,b,1)，肉眼与 hex 无异，却把终端从「不透明」
 * 悄悄换成「合成层上的全不透明」——xterm 的 allowTransparency 路径与直接背景色不是
 * 同一条渲染路，代价是全屏刷新时的性能与字形抗锯齿。
 */
describe("背景透明度 alpha 改写 withAlpha（UI 规格 §2.12）", () => {
  it("缺省（未设置过）保持原六位 hex", () => {
    expect(withAlpha("#1e1e2e", undefined)).toBe("#1e1e2e");
  });

  it("置 100 恢复不透明：原样返回 hex，不产出 rgba(...,1)", () => {
    expect(withAlpha("#1e1e2e", 100)).toBe("#1e1e2e");
  });

  it("0–99 改写为 rgba，通道值按十六进制正确拆分", () => {
    // #1e1e2e = (30, 30, 46)
    expect(withAlpha("#1e1e2e", 80)).toBe("rgba(30, 30, 46, 0.8)");
    expect(withAlpha("#ff8000", 50)).toBe("rgba(255, 128, 0, 0.5)");
  });

  it("0 = 全透明（不是被当成 falsy 而回退成不透明）", () => {
    expect(withAlpha("#1e1e2e", 0)).toBe("rgba(30, 30, 46, 0)");
  });

  it("越界值（>100）按不透明处理，不产出非法 alpha", () => {
    expect(withAlpha("#1e1e2e", 140)).toBe("#1e1e2e");
  });

  it("反向对照：99 与 100 必须落在不同分支（边界写偏一格即红）", () => {
    expect(withAlpha("#1e1e2e", 99).startsWith("rgba(")).toBe(true);
    expect(withAlpha("#1e1e2e", 100).startsWith("rgba(")).toBe(false);
  });
});

/**
 * per-session 字号钳位（M1 出口「字体缩放…8–32 区间」）。
 *
 * 此前这条区间只活在 `createTerminal` 里 `zoom` 闭包的一行注释旁边，零断言：
 * `createTerminal` 在 jsdom 下起不来（`term.open(el)` 要真实排版），而
 * `TerminalPane.test.ts` 的替身把 `zoomFont` 换成了 `vi.fn()`——替身对钳位一无所知。
 * 钳位丢了不会报错：用户按住 Ctrl+- 一路缩到 0 甚至负数，终端整块消失，
 * 而这时字号选择器也已经看不见了。
 */
describe("字号钳位 clampFontSize（UI 规格 §3.3，Ctrl+= / Ctrl+- / Ctrl+滚轮 共用）", () => {
  it("区间内原样返回", () => {
    expect(clampFontSize(13)).toBe(13);
    expect(clampFontSize(FONT_SIZE_MIN)).toBe(FONT_SIZE_MIN);
    expect(clampFontSize(FONT_SIZE_MAX)).toBe(FONT_SIZE_MAX);
  });

  it("缩过头钳到下界，不得到 0 或负数（终端会整块消失）", () => {
    expect(clampFontSize(FONT_SIZE_MIN - 1)).toBe(FONT_SIZE_MIN);
    expect(clampFontSize(0)).toBe(FONT_SIZE_MIN);
    expect(clampFontSize(-40)).toBe(FONT_SIZE_MIN);
  });

  it("放过头钳到上界", () => {
    expect(clampFontSize(FONT_SIZE_MAX + 1)).toBe(FONT_SIZE_MAX);
    expect(clampFontSize(999)).toBe(FONT_SIZE_MAX);
  });

  it("区间取值就是出口标准写的 8–32（改了区间即改了出口标准，必须显式）", () => {
    expect([FONT_SIZE_MIN, FONT_SIZE_MAX]).toEqual([8, 32]);
  });

  it("反向对照：钳位不是恒等也不是恒定值", () => {
    expect(clampFontSize(20)).not.toBe(clampFontSize(21)); // 非恒定
    expect(clampFontSize(1000)).not.toBe(1000); // 非恒等
  });
});
