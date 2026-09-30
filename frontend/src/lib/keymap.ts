/**
 * 键盘配置文件（M4a：快捷键全量重绑 + 导入导出；路线图 §M4 / UI 规格 §2.7/§4）。
 *
 * MVP 的 `actionForKey` 是一串硬编码 if：键位是代码，改不了。本模块把它换成
 * **数据驱动的键位表**——默认表（DEFAULT_BINDINGS，逐条等价于原 if 链）+ 用户
 * 覆盖表（settings 键 `keyboard.bindings`），合并后由 `shortcuts.ts` 消费。
 *
 * ## 一条硬不变式（S312）
 *
 * **凡有绑定的键必须恒本地**（`isAlwaysLocal` 为真），否则远程模式 + 终端焦点下
 * 该键会被直通送往远端（arbitrate 分支一，R118），应用动作永不执行——用户重绑
 * 了一个键，然后发现它「有时候不管用」，而不管用的恰恰是最常用的场合（终端里）。
 * 原实现靠人工维护两张表的交集（shortcuts.test.ts 有守卫用例逐条列举），自定义
 * 键位无法这样列举，故 `isAlwaysLocalWith` 改为**从键位表反推**：绑定表即白名单
 * 的一部分，两张表不可能再分叉。
 *
 * ## 键位的规范形（canonical form）
 *
 * `"Ctrl+Shift+Tab"` / `"Alt+P"` / `"F11"` / `"ScrollLock"`：修饰键固定序
 * Ctrl→Alt→Shift，主键单字符大写、命名键保原名。规范化让「同一个键的两种写法」
 * （`ctrl+shift+tab` / `Shift+Ctrl+Tab`）落到同一个表键上——否则导入别人的配置
 * 时会出现两条冲突绑定而只有一条生效。
 */
import type { ShortcutAction } from "./shortcuts";

/** 键位串 → 动作 id 的映射。值为 null = **显式解绑**（覆盖默认表里的这一条）。 */
export type KeyBindings = Record<string, string | null>;

/**
 * 可绑定判据（S314 安全闸）：**无修饰键的可打印字符不可绑定**。
 *
 * 反例即理由：把 `A` 绑给「新建会话」之后，`isAlwaysLocalWith` 会让 `a` 恒本地、
 * actionForKey 返回动作 ⇒ 这个字母再也到不了 shell，用户在终端里打不出 a 且
 * 完全不知道为什么。命名键（F1–F12/ScrollLock/Escape…）无此问题：它们本来就
 * 不是字符输入。
 *
 * 单修饰 Shift 也不放行：`Shift+A` 就是大写 A，同样是字符输入。
 */
export function isBindableCombo(combo: string): boolean {
  const segs = combo.split("+");
  const main = combo.trimEnd().endsWith("+") ? "+" : segs[segs.length - 1];
  const mods = new Set((main === "+" ? segs : segs.slice(0, -1)).map((s) => s.toLowerCase()));
  const hasRealMod = mods.has("ctrl") || mods.has("alt");
  if (hasRealMod) return true;
  // 无 Ctrl/Alt：只允许命名键（长度 > 1 的键名，如 F5/ScrollLock/Escape）
  return normalizeKeyName(main).length > 1;
}

/** 修饰键固定序：Ctrl→Alt→Shift。metaKey（macOS ⌘）与 ctrl 同义合并（与既有仲裁一致）。 */
export function canonicalCombo(e: {
  key: string;
  ctrlKey?: boolean;
  metaKey?: boolean;
  altKey?: boolean;
  shiftKey?: boolean;
}): string {
  const parts: string[] = [];
  if (e.ctrlKey || e.metaKey) parts.push("Ctrl");
  if (e.altKey) parts.push("Alt");
  if (e.shiftKey) parts.push("Shift");
  parts.push(normalizeKeyName(e.key));
  return parts.join("+");
}

/**
 * 主键名规范化：单字符一律大写（`a`→`A`）；命名键（Tab/F11/ScrollLock/Escape…）
 * 保留原名但按已知表纠正大小写。
 *
 * `+` 与 `=` 的取舍（S313）：两者在不同布局下报告不同（美式 Ctrl+Shift+= 报 "+"），
 * 而用户想的是「放大」——故规范化把 `+` 归到 `=`，一条绑定同时吃两形。原 if 链
 * 的 `k === "=" || k === "+"` 就是这个意思，这里把它变成表可以表达的形状。
 */
export function normalizeKeyName(key: string): string {
  if (key === "+") return "=";
  if (key.length === 1) return key.toUpperCase();
  const known = [
    "Tab", "Escape", "Enter", "Backspace", "Delete", "Insert", "Home", "End",
    "PageUp", "PageDown", "ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight",
    "ScrollLock", "Space",
    ...Array.from({ length: 12 }, (_, i) => `F${i + 1}`),
  ];
  const hit = known.find((k) => k.toLowerCase() === key.toLowerCase());
  return hit ?? key;
}

/**
 * 默认键位表：逐条等价于 MVP 的 `actionForKey` if 链（UI 规格 §4 固定集）。
 *
 * `Ctrl+Q` 刻意不在表内（终端流控制 XOFF，必须透传）；`Ctrl+C`/`Ctrl+V` 亦不在
 * （选区双义由终端侧钩子就地决策，§2.8）——把它们放进可重绑表等于让用户能把
 * 「复制」绑成一个会吞掉 SIGINT 的键。
 */
export const DEFAULT_BINDINGS: Readonly<Record<string, string>> = Object.freeze({
  "ScrollLock": "keyboard.toggleMode",
  "Ctrl+N": "session.new",
  "Ctrl+W": "session.closeActive",
  "Ctrl+F": "edit.find",
  "Ctrl+,": "tools.settings",
  "Ctrl+L": "edit.clearScreen",
  "Ctrl+=": "view.fontGrow",
  "Ctrl+-": "view.fontShrink",
  "Ctrl+0": "view.fontReset",
  "Ctrl+Tab": "tab.mruNext",
  "Ctrl+Shift+Tab": "tab.mruPrev",
  "Ctrl+Shift+S": "view.sidebar",
  "Ctrl+Shift+M": "view.monitor",
  "Ctrl+Shift+H": "tools.history",
  "Ctrl+Shift+A": "tools.ai",
  "F11": "view.fullscreen",
  "F3": "edit.findNext",
  "Alt+P": "session.properties",
});

/**
 * 合并默认表与用户覆盖：用户表的 `null` 值**删除**该默认绑定（显式解绑），
 * 字符串值覆盖或新增。返回的表是新对象，默认表不被改写。
 */
export function mergeBindings(user: KeyBindings | null | undefined): Record<string, string> {
  const out: Record<string, string> = { ...DEFAULT_BINDINGS };
  if (!user) return out;
  for (const [rawCombo, action] of Object.entries(user)) {
    const combo = canonicalizeComboString(rawCombo);
    if (!combo) continue; // 畸形键位串静默跳过（导入的配置不可尽信）
    if (action === null || action === "") delete out[combo];
    else out[combo] = action;
  }
  return out;
}

/**
 * 把任意书写的键位串规范化（导入他人配置/手写 JSON 的入口）。
 * `"ctrl+shift+tab"` → `"Ctrl+Shift+Tab"`；无法识别返回 null。
 */
export function canonicalizeComboString(raw: string): string | null {
  const segs = raw.split("+").map((s) => s.trim()).filter(Boolean);
  if (segs.length === 0) return null;
  // 末段是主键；`Ctrl++`（主键为 +）经 split 后末段为空已被 filter 掉——
  // 补回：原串以 "+" 结尾即主键是 "+"（规范化后归 "="）
  const mainRaw = raw.trimEnd().endsWith("+") && segs.length > 0 ? "+" : segs[segs.length - 1];
  const mods = new Set(
    (mainRaw === "+" ? segs : segs.slice(0, -1)).map((s) => s.toLowerCase()),
  );
  const known = ["ctrl", "control", "cmd", "meta", "alt", "option", "shift"];
  for (const m of mods) {
    if (!known.includes(m)) return null;
  }
  const parts: string[] = [];
  if (mods.has("ctrl") || mods.has("control") || mods.has("cmd") || mods.has("meta")) parts.push("Ctrl");
  if (mods.has("alt") || mods.has("option")) parts.push("Alt");
  if (mods.has("shift")) parts.push("Shift");
  const main = normalizeKeyName(mainRaw);
  if (!main) return null;
  parts.push(main);
  return parts.join("+");
}

/** 键位表 → 动作查询（`shortcuts.ts` 的 actionForKey 消费）。 */
export function actionForCombo(
  bindings: Record<string, string>,
  e: KeyboardEvent,
): ShortcutAction {
  const id = bindings[canonicalCombo(e)];
  if (!id) return { kind: "passthrough" };
  if (id === "keyboard.toggleMode") return { kind: "toggle-mode" };
  return { kind: "action", id };
}

/** 解析用户键位表（settings JSON）。烂值不抛、回落空表（配置坏了退回默认键位，
 *  而不是让整个快捷键系统失效）。 */
export function parseBindings(json: string | null | undefined): KeyBindings {
  if (!json) return {};
  try {
    const v = JSON.parse(json);
    if (v === null || typeof v !== "object" || Array.isArray(v)) return {};
    const out: KeyBindings = {};
    for (const [k, val] of Object.entries(v as Record<string, unknown>)) {
      if (val === null || typeof val === "string") out[k] = val as string | null;
    }
    return out;
  } catch {
    return {};
  }
}

/** 冲突检测：同一动作绑定到多个键是允许的（多入口）；**同一键绑多个动作**在表
 *  结构上不可能（对象键唯一）——真正要报的是「用户新绑的键已被别的动作占用」。 */
export function comboOwner(
  bindings: Record<string, string>,
  combo: string,
): string | null {
  return bindings[combo] ?? null;
}
