/**
 * S312–S314：键盘配置文件（M4a 全量重绑 + 导入导出）的判据。
 *
 * 出口标准原文：「键盘配置文件：重绑生效 + 导入导出 round-trip 测试」。
 */
import { describe, expect, it } from "vitest";
import {
  DEFAULT_BINDINGS,
  actionForCombo,
  canonicalCombo,
  canonicalizeComboString,
  comboOwner,
  isBindableCombo,
  mergeBindings,
  normalizeKeyName,
  parseBindings,
} from "./keymap";

function ev(init: { key: string; ctrl?: boolean; shift?: boolean; alt?: boolean; meta?: boolean }): KeyboardEvent {
  return new KeyboardEvent("keydown", {
    key: init.key,
    ctrlKey: init.ctrl ?? false,
    shiftKey: init.shift ?? false,
    altKey: init.alt ?? false,
    metaKey: init.meta ?? false,
  });
}

describe("canonicalCombo / normalizeKeyName（规范形，S313）", () => {
  it("修饰键固定序 Ctrl→Alt→Shift，主键单字符大写", () => {
    expect(canonicalCombo(ev({ key: "n", ctrl: true }))).toBe("Ctrl+N");
    expect(canonicalCombo(ev({ key: "Tab", ctrl: true, shift: true }))).toBe("Ctrl+Shift+Tab");
    expect(canonicalCombo(ev({ key: "p", alt: true }))).toBe("Alt+P");
    // 修饰键书写序不影响结果（同一键的两种写法必须落到同一表键，否则导入配置出双绑）
    expect(canonicalCombo(ev({ key: "m", ctrl: true, alt: true, shift: true }))).toBe("Ctrl+Alt+Shift+M");
  });

  it("metaKey（⌘）与 ctrl 同义（与既有仲裁口径一致）", () => {
    expect(canonicalCombo(ev({ key: "f", meta: true }))).toBe("Ctrl+F");
  });

  it("`+` 归一到 `=`：美式布局 Ctrl+Shift+= 报 '+'，用户想的是「放大」（S313）", () => {
    expect(normalizeKeyName("+")).toBe("=");
    expect(canonicalCombo(ev({ key: "+", ctrl: true }))).toBe("Ctrl+=");
    expect(canonicalCombo(ev({ key: "=", ctrl: true }))).toBe("Ctrl+=");
  });

  it("命名键大小写纠正、未知键原样透传", () => {
    expect(normalizeKeyName("scrolllock")).toBe("ScrollLock");
    expect(normalizeKeyName("f11")).toBe("F11");
    expect(normalizeKeyName("Unknown9")).toBe("Unknown9");
  });
});

describe("默认表等价性（重构不改行为）", () => {
  it("MVP 固定集逐条在表内且动作 id 不变", () => {
    const b = mergeBindings(null);
    expect(b["Ctrl+N"]).toBe("session.new");
    expect(b["Ctrl+W"]).toBe("session.closeActive");
    expect(b["Ctrl+Shift+Tab"]).toBe("tab.mruPrev");
    expect(b["Ctrl+="]).toBe("view.fontGrow");
    expect(b["F3"]).toBe("edit.findNext");
    expect(b["Alt+P"]).toBe("session.properties");
    expect(b["ScrollLock"]).toBe("keyboard.toggleMode");
  });

  it("Ctrl+Q / Ctrl+C / Ctrl+V 刻意不在可重绑表内（流控与选区双义）", () => {
    const b = mergeBindings(null);
    expect(b["Ctrl+Q"]).toBeUndefined();
    expect(b["Ctrl+C"]).toBeUndefined();
    expect(b["Ctrl+V"]).toBeUndefined();
  });

  it("ScrollLock 走 toggle-mode 分支，其余走 action", () => {
    const b = mergeBindings(null);
    expect(actionForCombo(b, ev({ key: "ScrollLock" }))).toEqual({ kind: "toggle-mode" });
    expect(actionForCombo(b, ev({ key: "n", ctrl: true }))).toEqual({ kind: "action", id: "session.new" });
    expect(actionForCombo(b, ev({ key: "j", ctrl: true }))).toEqual({ kind: "passthrough" });
  });
});

describe("mergeBindings 重绑生效（出口标准「重绑生效」）", () => {
  it("覆盖默认键：Ctrl+N 改绑到别的动作", () => {
    const b = mergeBindings({ "Ctrl+N": "tools.settings" });
    expect(actionForCombo(b, ev({ key: "n", ctrl: true }))).toEqual({ kind: "action", id: "tools.settings" });
  });

  it("新增键：默认表没有的组合可绑", () => {
    const b = mergeBindings({ "Ctrl+Alt+K": "edit.clearScreen" });
    expect(actionForCombo(b, ev({ key: "k", ctrl: true, alt: true }))).toEqual({
      kind: "action",
      id: "edit.clearScreen",
    });
  });

  it("null = 显式解绑：默认键被删除后 passthrough", () => {
    const b = mergeBindings({ "Ctrl+F": null });
    expect(actionForCombo(b, ev({ key: "f", ctrl: true }))).toEqual({ kind: "passthrough" });
  });

  it("用户表的书写不规范也能命中（导入他人配置的常态）", () => {
    const b = mergeBindings({ "ctrl+shift+m": "view.sidebar", "ALT+p": null });
    expect(b["Ctrl+Shift+M"]).toBe("view.sidebar");
    expect(b["Alt+P"]).toBeUndefined();
  });

  it("默认表不被改写（返回新对象；冻结表被写会静默失败或抛，两者都是缺陷）", () => {
    const before = { ...DEFAULT_BINDINGS };
    mergeBindings({ "Ctrl+N": "tools.settings", "Ctrl+F": null });
    expect({ ...DEFAULT_BINDINGS }).toEqual(before);
  });

  it("畸形键位串静默跳过（导入的配置不可尽信，不为一条坏行毁整表）", () => {
    const b = mergeBindings({ "Hyper+Q": "session.new", "": "x", "+++": "y" });
    expect(b["Hyper+Q"]).toBeUndefined();
    expect(Object.keys(b).sort()).toEqual(Object.keys(DEFAULT_BINDINGS).sort());
  });
});

describe("parseBindings 导入导出 round-trip（出口标准原文）", () => {
  it("导出（JSON.stringify）→ 导入（parseBindings）→ 合并，结果与原表一致", () => {
    const user = { "Ctrl+Alt+K": "edit.clearScreen", "Ctrl+F": null, "Ctrl+N": "tools.settings" };
    const roundTripped = parseBindings(JSON.stringify(user));
    expect(roundTripped).toEqual(user);
    expect(mergeBindings(roundTripped)).toEqual(mergeBindings(user));
  });

  it("烂值回落空表（配置坏了退回默认键位，而不是让快捷键系统整体失效）", () => {
    expect(parseBindings(null)).toEqual({});
    expect(parseBindings("not json")).toEqual({});
    expect(parseBindings("[1,2]")).toEqual({});
    expect(parseBindings('{"Ctrl+N":123}')).toEqual({}); // 值类型不对的条目剔除
    expect(mergeBindings(parseBindings("garbage"))).toEqual({ ...DEFAULT_BINDINGS });
  });
});

describe("isBindableCombo 安全闸（S314）", () => {
  it("无修饰可打印字符不可绑——绑了它终端里就再也打不出这个字符", () => {
    expect(isBindableCombo("A")).toBe(false);
    expect(isBindableCombo("Shift+A")).toBe(false); // Shift+A 就是大写 A，同样是字符输入
    expect(isBindableCombo("=")).toBe(false);
  });

  it("命名键无需修饰即可绑（它们本来就不是字符输入）", () => {
    expect(isBindableCombo("F5")).toBe(true);
    expect(isBindableCombo("ScrollLock")).toBe(true);
    expect(isBindableCombo("Escape")).toBe(true);
  });

  it("带 Ctrl/Alt 一律可绑", () => {
    expect(isBindableCombo("Ctrl+A")).toBe(true);
    expect(isBindableCombo("Alt+A")).toBe(true);
    expect(isBindableCombo("Ctrl+Shift+A")).toBe(true);
  });
});

describe("comboOwner 冲突检测（UI 提示「该键已被占用」的判据）", () => {
  it("已占用返回动作 id，空闲返回 null", () => {
    const b = mergeBindings(null);
    expect(comboOwner(b, "Ctrl+N")).toBe("session.new");
    expect(comboOwner(b, "Ctrl+Alt+Z")).toBeNull();
  });
});

describe("canonicalizeComboString 边界", () => {
  it("主键为 `+` 的组合可解析（Ctrl++ → Ctrl+=）", () => {
    expect(canonicalizeComboString("Ctrl++")).toBe("Ctrl+=");
  });
  it("未知修饰键整条拒绝（返回 null 而非猜）", () => {
    expect(canonicalizeComboString("Hyper+K")).toBeNull();
    expect(canonicalizeComboString("")).toBeNull();
  });
  it("cmd/meta/option 别名归一（macOS 配置导入）", () => {
    expect(canonicalizeComboString("cmd+k")).toBe("Ctrl+K");
    expect(canonicalizeComboString("option+k")).toBe("Alt+K");
  });
});
