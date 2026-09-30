import { beforeEach, describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";

vi.mock("./ipc", () => ({
  settingGet: vi.fn(async (_k: string, fb: unknown) => fb),
  settingSet: vi.fn(async () => {}),
  invoke: vi.fn(async () => null),
  listen: vi.fn(async () => () => {}),
  openExternal: vi.fn(async () => {}),
}));

import { settingGet, settingSet } from "./ipc";
import {
  KEYBOARD_MODE_SETTING_KEY, actionForKey, arbitrate, initKeyboardMode,
  isAlwaysLocal, keyboardModeDefault, setKeyBindings, toggleKeyboardMode,
} from "./shortcuts";
import { addTab, resetTabStore, tabs } from "./tabs"; // F25 per-session 键盘模式红测消费（运行期调用，ESM 循环安全）

/** 构造按键事件（jsdom 环境）；仅给仲裁读取的字段：key/ctrlKey/metaKey/shiftKey/altKey。 */
function ev(init: { key: string; ctrl?: boolean; shift?: boolean; alt?: boolean; meta?: boolean }): KeyboardEvent {
  return new KeyboardEvent("keydown", {
    key: init.key,
    ctrlKey: init.ctrl ?? false,
    shiftKey: init.shift ?? false,
    altKey: init.alt ?? false,
    metaKey: init.meta ?? false,
  });
}

describe("actionForKey §4 键表逐行动作（MVP 固定集；全量重绑 M4a 键盘配置文件）", () => {
  it("Ctrl+N 新建会话 / Ctrl+W 关闭标签 / Ctrl+F 查找 / Ctrl+, 设置 / Ctrl+L 清屏", () => {
    expect(actionForKey(ev({ key: "n", ctrl: true }))).toEqual({ kind: "action", id: "session.new" });
    expect(actionForKey(ev({ key: "w", ctrl: true }))).toEqual({ kind: "action", id: "session.closeActive" });
    expect(actionForKey(ev({ key: "f", ctrl: true }))).toEqual({ kind: "action", id: "edit.find" });
    expect(actionForKey(ev({ key: ",", ctrl: true }))).toEqual({ kind: "action", id: "tools.settings" });
    expect(actionForKey(ev({ key: "l", ctrl: true }))).toEqual({ kind: "action", id: "edit.clearScreen" });
  });
  it("Ctrl+Tab / Ctrl+Shift+Tab MRU 序切换标签（Shift 双形、方向对偶）", () => {
    expect(actionForKey(ev({ key: "Tab", ctrl: true }))).toEqual({ kind: "action", id: "tab.mruNext" });
    expect(actionForKey(ev({ key: "Tab", ctrl: true, shift: true }))).toEqual({ kind: "action", id: "tab.mruPrev" });
    expect(actionForKey(ev({ key: "a", ctrl: true, shift: true }))).toEqual({ kind: "action", id: "tools.ai" });
  });
  it("Ctrl+Shift+S 会话管理器 / Ctrl+Shift+M 监控抽屉", () => {
    expect(actionForKey(ev({ key: "s", ctrl: true, shift: true }))).toEqual({ kind: "action", id: "view.sidebar" });
    expect(actionForKey(ev({ key: "m", ctrl: true, shift: true }))).toEqual({ kind: "action", id: "view.monitor" });
  });
  it("字号：Ctrl+= 放大（布局差异下键报告 '='/'+' 两形皆放大）/ Ctrl+- 缩小 / Ctrl+0 复位", () => {
    expect(actionForKey(ev({ key: "=", ctrl: true }))).toEqual({ kind: "action", id: "view.fontGrow" });
    expect(actionForKey(ev({ key: "+", ctrl: true }))).toEqual({ kind: "action", id: "view.fontGrow" });
    expect(actionForKey(ev({ key: "-", ctrl: true }))).toEqual({ kind: "action", id: "view.fontShrink" });
    expect(actionForKey(ev({ key: "0", ctrl: true }))).toEqual({ kind: "action", id: "view.fontReset" });
  });
  it("F11 全屏 / F3 查找下一个 / Alt+P 会话属性", () => {
    expect(actionForKey(ev({ key: "F11" }))).toEqual({ kind: "action", id: "view.fullscreen" });
    expect(actionForKey(ev({ key: "F3" }))).toEqual({ kind: "action", id: "edit.findNext" });
    expect(actionForKey(ev({ key: "p", alt: true }))).toEqual({ kind: "action", id: "session.properties" });
  });
  it("Scroll Lock 切换键盘模式（toggle-mode；§2.7 与状态栏键盘段点击等价）", () => {
    expect(actionForKey(ev({ key: "ScrollLock" }))).toEqual({ kind: "toggle-mode" });
  });
  // Ctrl+Shift+A 曾在这一条里作为「无绑定」的例子，注释写着「AI，Phase 2」。
  // M2 把 AI 面板接上之后它有绑定了，于是这条用例红——**这正是它该做的**：
  // 一个键从「无人认领」变成「有动作」，必须有东西逼人回来改这里，
  // 否则「终端焦点下这个键会不会被吞」就悄悄换了答案。
  it("Ctrl+Shift+A → AI 面板（M2 接线；此前为无绑定 passthrough）", () => {
    expect(actionForKey(ev({ key: "a", ctrl: true, shift: true }))).toEqual({ kind: "action", id: "tools.ai" });
  });
  it("MVP 固定集之外的键 passthrough：Ctrl+V（终端侧粘贴 §2.8）/ 普通字符键", () => {
    expect(actionForKey(ev({ key: "v", ctrl: true }))).toEqual({ kind: "passthrough" });
    expect(actionForKey(ev({ key: "a" }))).toEqual({ kind: "passthrough" });
  });
  it("不变式（R118 前提）：凡 actionForKey 有绑定的键必在 §2.7 恒本地白名单内——否则远程直通会把应用动作一并送走", () => {
    const bound = [
      { key: "ScrollLock" }, { key: "n", ctrl: true }, { key: "w", ctrl: true }, { key: "f", ctrl: true },
      { key: ",", ctrl: true }, { key: "l", ctrl: true }, { key: "=", ctrl: true }, { key: "+", ctrl: true },
      { key: "-", ctrl: true }, { key: "0", ctrl: true }, { key: "Tab", ctrl: true },
      { key: "Tab", ctrl: true, shift: true }, { key: "s", ctrl: true, shift: true }, { key: "m", ctrl: true, shift: true },
      { key: "F11" }, { key: "F3" }, { key: "p", alt: true },
    ];
    for (const init of bound) {
      const e = ev(init);
      expect(actionForKey(e).kind, `${init.key} 应有动作绑定`).not.toBe("passthrough");
      expect(isAlwaysLocal(e), `${init.key} 应属恒本地白名单`).toBe(true);
    }
  });

  /**
   * S312（M4a 键盘配置文件）：**自定义绑定的键也必须恒本地**。
   *
   * 这条是上一条不变式在「键位可重绑」之后的续命形式：MVP 期靠上面那张字面量
   * 清单人工维护交集，自定义键位无法列举——若 isAlwaysLocal 不从键位表反推，
   * 用户新绑的键在远程模式 + 终端焦点下会被直通送往远端（arbitrate 分支一），
   * 动作永不执行，而终端恰是唯一常用的场合。
   */
  it("S312：重绑后新键恒本地且动作生效；解绑后旧键回落 passthrough", () => {
    const custom = ev({ key: "k", ctrl: true, alt: true });
    // 默认表里没有 Ctrl+Alt+K：passthrough 且非恒本地
    expect(actionForKey(custom).kind).toBe("passthrough");
    expect(isAlwaysLocal(custom)).toBe(false);

    setKeyBindings({ "Ctrl+Alt+K": "edit.clearScreen", "Ctrl+F": null });
    try {
      // 新绑的键：有动作 + 恒本地（两者缺一即为「重绑了但终端里不管用」）
      expect(actionForKey(custom)).toEqual({ kind: "action", id: "edit.clearScreen" });
      expect(isAlwaysLocal(custom), "自定义键必须恒本地，否则远程直通把动作送走").toBe(true);
      // 显式解绑的默认键：回落 passthrough（Ctrl+F 仍因 §2.7 字面量白名单恒本地，
      // 那是「恒本地但无绑定」的既有形状，S265 已钉：不吞、原样交给下游）
      expect(actionForKey(ev({ key: "f", ctrl: true })).kind).toBe("passthrough");
    } finally {
      setKeyBindings(null); // 复位，避免污染同文件其余用例
    }
  });
});

describe("isAlwaysLocal 恒本地白名单边界（UI 规格 §2.7，R19 补齐）", () => {
  it("R19 所补键恒本地：Ctrl+L / Ctrl+= / Ctrl+- / Ctrl+0 / F3 / Alt+P", () => {
    expect(isAlwaysLocal(ev({ key: "l", ctrl: true }))).toBe(true);
    expect(isAlwaysLocal(ev({ key: "=", ctrl: true }))).toBe(true);
    expect(isAlwaysLocal(ev({ key: "-", ctrl: true }))).toBe(true);
    expect(isAlwaysLocal(ev({ key: "0", ctrl: true }))).toBe(true);
    expect(isAlwaysLocal(ev({ key: "F3" }))).toBe(true);
    expect(isAlwaysLocal(ev({ key: "p", alt: true }))).toBe(true);
  });
  it("既有白名单代表：F11 / Scroll Lock / Ctrl+Shift+* 全家 / Ctrl+Tab / Ctrl+F / Ctrl+W / Ctrl+N / Ctrl+,", () => {
    expect(isAlwaysLocal(ev({ key: "F11" }))).toBe(true);
    expect(isAlwaysLocal(ev({ key: "ScrollLock" }))).toBe(true);
    expect(isAlwaysLocal(ev({ key: "v", ctrl: true, shift: true }))).toBe(true);
    expect(isAlwaysLocal(ev({ key: "Tab", ctrl: true }))).toBe(true);
    expect(isAlwaysLocal(ev({ key: "f", ctrl: true }))).toBe(true);
    expect(isAlwaysLocal(ev({ key: "w", ctrl: true }))).toBe(true);
    expect(isAlwaysLocal(ev({ key: "n", ctrl: true }))).toBe(true);
    expect(isAlwaysLocal(ev({ key: ",", ctrl: true }))).toBe(true);
  });
  it("反例：Ctrl+V / Ctrl+C 非恒本地（Ctrl+C 双义=选区依赖，由 Task 18 组 G 终端侧钩子处理，R19 条款不动）", () => {
    expect(isAlwaysLocal(ev({ key: "v", ctrl: true }))).toBe(false);
    expect(isAlwaysLocal(ev({ key: "c", ctrl: true }))).toBe(false);
    expect(isAlwaysLocal(ev({ key: "a" }))).toBe(false);
  });
  it("S253 AltGr 反例：Windows/Linux 上 AltGr 报 ctrlKey+altKey，合成字符不得被判恒本地（波兰 AltGr+A=ą、AltGr+Shift+A=Ą、德语 AltGr+E=€）", () => {
    expect(isAlwaysLocal(ev({ key: "ą", ctrl: true, alt: true }))).toBe(false);
    expect(isAlwaysLocal(ev({ key: "Ą", ctrl: true, shift: true, alt: true }))).toBe(false);
    expect(isAlwaysLocal(ev({ key: "€", ctrl: true, alt: true }))).toBe(false);
    expect(isAlwaysLocal(ev({ key: "p", ctrl: true, alt: true }))).toBe(false); // AltGr+P 非 Alt+P 会话属性
  });
  /**
   * S265（Task 17 第二轮复审，R118/S253 回归面）：S253 把 Ctrl+Shift+* 一支放宽为「整族恒本地」
   * （与 §2.7「`Ctrl+Shift+*` 全家」口径一致），代价是 App.svelte 的输入框守卫
   * `if (inField && !isAlwaysLocal(e)) return;` 对整族失效——Ctrl+Shift+ArrowLeft 等原生编辑手势
   * 会继续走到 actionForKey。此时不被 preventDefault 吞掉的唯一依据，就是「无 §4 绑定者返回 passthrough」
   * 且 App.svelte 在 `action.kind === "passthrough"` 处早退（先于 e.preventDefault()）。本用例钉死该前提。
   */
  it("S265：Ctrl+Shift+* 恒本地但无 §4 绑定者必须 passthrough（否则组合命令栏内原生编辑手势被吞）", () => {
    // "a" 于 M2 移出本表：它现在绑 tools.ai（见上方专条）。留在这里就是要求它既有绑定又 passthrough。
    for (const key of ["ArrowLeft", "ArrowRight", "Home", "End", "Delete", "z", "k", "c", "v"]) {
      const e = ev({ key, ctrl: true, shift: true });
      expect(isAlwaysLocal(e), `${key} 应恒本地`).toBe(true);
      expect(actionForKey(e).kind, `${key} 应无窗口级绑定`).toBe("passthrough");
    }
    // 对照：该族里确有绑定的三键仍须命中动作，放宽不得误伤（c/v 属终端侧，见 §4 复制/粘贴行）
    expect(actionForKey(ev({ key: "s", ctrl: true, shift: true }))).toEqual({ kind: "action", id: "view.sidebar" });
    expect(actionForKey(ev({ key: "m", ctrl: true, shift: true }))).toEqual({ kind: "action", id: "view.monitor" });
    expect(actionForKey(ev({ key: "Tab", ctrl: true, shift: true }))).toEqual({ kind: "action", id: "tab.mruPrev" });
  });
});

describe("arbitrate 三分支（UI 规格 §2.7）", () => {
  it("分支一：远程 + 终端焦点 + 无修饰或单 Ctrl 普通键 → passthrough（终端侧钩子返回 false，xterm 自处）", () => {
    expect(arbitrate(ev({ key: "a" }), { terminalFocused: true, mode: "remote" })).toBe("passthrough");
    expect(arbitrate(ev({ key: "s", ctrl: true }), { terminalFocused: true, mode: "remote" })).toBe("passthrough");
    expect(arbitrate(ev({ key: "q", ctrl: true }), { terminalFocused: true, mode: "remote" })).toBe("passthrough");
  });
  it("白名单优先于分支一：远程 + 终端焦点下白名单键仍恒本地", () => {
    expect(arbitrate(ev({ key: "w", ctrl: true }), { terminalFocused: true, mode: "remote" })).toBe("local");
    expect(arbitrate(ev({ key: "f", ctrl: true }), { terminalFocused: true, mode: "remote" })).toBe("local");
  });
  it("分支二：本地模式 → 其余键亦本地", () => {
    expect(arbitrate(ev({ key: "a" }), { terminalFocused: true, mode: "local" })).toBe("local");
    expect(arbitrate(ev({ key: "s", ctrl: true }), { terminalFocused: true, mode: "local" })).toBe("local");
  });
  it("分支三：终端外（菜单/对话框焦点）→ 恒本地", () => {
    expect(arbitrate(ev({ key: "a" }), { terminalFocused: false, mode: "remote" })).toBe("local");
    expect(arbitrate(ev({ key: "s", ctrl: true }), { terminalFocused: false, mode: "remote" })).toBe("local");
  });
  it("R118：远程 + 终端焦点 + 非白名单复合修饰 → passthrough（Alt 词移动/AltGr 合成字符必须原样达远端）", () => {
    expect(arbitrate(ev({ key: "b", alt: true }), { terminalFocused: true, mode: "remote" })).toBe("passthrough"); // readline 后退一词
    expect(arbitrate(ev({ key: "f", alt: true }), { terminalFocused: true, mode: "remote" })).toBe("passthrough"); // 前进一词
    expect(arbitrate(ev({ key: "Backspace", alt: true }), { terminalFocused: true, mode: "remote" })).toBe("passthrough"); // 向前删词
    expect(arbitrate(ev({ key: ".", alt: true }), { terminalFocused: true, mode: "remote" })).toBe("passthrough"); // 取上条命令末参
    expect(arbitrate(ev({ key: "b", ctrl: true, alt: true }), { terminalFocused: true, mode: "remote" })).toBe("passthrough");
    expect(arbitrate(ev({ key: "Ą", ctrl: true, shift: true, alt: true }), { terminalFocused: true, mode: "remote" })).toBe("passthrough");
  });
  it("R118 不越界：白名单键与本地模式/终端外焦点三条边界不受直通放宽影响", () => {
    expect(arbitrate(ev({ key: "m", ctrl: true, shift: true }), { terminalFocused: true, mode: "remote" })).toBe("local");
    expect(arbitrate(ev({ key: "p", alt: true }), { terminalFocused: true, mode: "remote" })).toBe("local");
    expect(arbitrate(ev({ key: "b", alt: true }), { terminalFocused: true, mode: "local" })).toBe("local");
    expect(arbitrate(ev({ key: "b", alt: true }), { terminalFocused: false, mode: "remote" })).toBe("local");
  });
});

describe("keyboardModeDefault / toggleKeyboardMode（§2.7/§2.12：默认值持久、会话内切换不持久；./ipc 已 mock）", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    keyboardModeDefault.set("remote");
    resetTabStore();
  });
  describe("keyboardModeDefault 默认值（持久默认；仅 SettingsDialog 读写）", () => {
    it("默认远程", () => {
      expect(get(keyboardModeDefault)).toBe("remote");
    });
    it("initKeyboardMode 载入 settings 值至 keyboardModeDefault；非法值回落 remote", async () => {
      vi.mocked(settingGet).mockResolvedValueOnce("local");
      await initKeyboardMode();
      expect(get(keyboardModeDefault)).toBe("local");
      expect(settingGet).toHaveBeenCalledWith(KEYBOARD_MODE_SETTING_KEY, "remote");
      vi.mocked(settingGet).mockResolvedValueOnce("bogus");
      await initKeyboardMode();
      expect(get(keyboardModeDefault)).toBe("remote");
    });
    it("addTab 初始 keyboardMode 取 keyboardModeDefault 当前值", () => {
      keyboardModeDefault.set("local");
      addTab("s1", "web-01", "p1");
      expect(get(tabs).find((t) => t.id === "s1")?.keyboardMode).toBe("local");
    });
  });
  it("toggleKeyboardMode(sessionId) 仅翻转该会话 Tab.keyboardMode，不触碰 settingSet", () => {
    addTab("s1", "web-01", "p1");
    addTab("s2", "db-02", "p2");
    toggleKeyboardMode("s1");
    expect(get(tabs).find((t) => t.id === "s1")?.keyboardMode).toBe("local");
    expect(get(tabs).find((t) => t.id === "s2")?.keyboardMode).toBe("remote"); // 其他会话不受影响
    expect(settingSet).not.toHaveBeenCalled(); // 会话内切换不持久化（UI §2.12）
    toggleKeyboardMode("s1");
    expect(get(tabs).find((t) => t.id === "s1")?.keyboardMode).toBe("remote");
  });
});
