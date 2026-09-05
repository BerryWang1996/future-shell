import { writable } from "svelte/store";
import { settingGet } from "./ipc";
import { setTabKeyboardMode, tabKeyboardMode } from "./tabs"; // 运行期调用，ESM 循环安全（F25 per-session 键盘模式）
import {
  actionForCombo, canonicalCombo, mergeBindings, parseBindings, type KeyBindings,
} from "./keymap"; // M4a 键盘配置文件：键位表数据驱动（S312–S314）

export type KeyboardMode = "remote" | "local";
export const KEYBOARD_MODE_SETTING_KEY = "keyboard.mode";

/** 键盘模式：remote=按键原样发往会话（含终端常截获的 Ctrl+S/Ctrl+Q）；local=快捷键作用于应用 UI。MVP 默认远程（UI 规格 §2.7）。 */
export const keyboardModeDefault = writable<KeyboardMode>("remote"); // 默认值（仅 SettingsDialog 读写）；会话内模式在 Tab.keyboardMode（UI §2.12，不持久化）

/** 启动时载入持久化默认值（settings 命令未注册时回落 remote，不阻塞）。 */
export async function initKeyboardMode(): Promise<void> {
  const m = await settingGet<KeyboardMode>(KEYBOARD_MODE_SETTING_KEY, "remote");
  keyboardModeDefault.set(m === "local" ? "local" : "remote");
}

/** 切换当前会话模式（不持久化——UI §2.12；Scroll Lock 与状态栏段等价）。 */
export function toggleKeyboardMode(sessionId: string): void {
  const next: KeyboardMode = tabKeyboardMode(sessionId) === "remote" ? "local" : "remote";
  setTabKeyboardMode(sessionId, next);
}

/**
 * §2.7 恒本地白名单：无论键盘模式与终端焦点，以下键从不发往会话——
 * Ctrl+Shift+* 全家（复制/粘贴/侧栏/监控等）、Ctrl+F/W/Tab/,/N/L、
 * Ctrl+=/Ctrl+-/Ctrl+0（字号）、F3、F11、Alt+P（会话属性）、Scroll Lock（模式切换本身）。
 * 口径注（R36，与 UI 规格 §2.7 权威口径一致）：恒本地截获（不作按键原样透传）；
 * 截获后触发的应用动作可经 term_input 下发等效字节，如 Ctrl+L→edit.clearScreen 发送 \x0c。
 * S253：两处 AltGr 守卫。Windows/Linux 上 AltGr 在 KeyboardEvent 里报 ctrlKey+altKey 同时为真，
 * 故 Ctrl+Shift+* 与 Alt+P 两支必须排除 alt/ctrl 同按，否则波兰/德语等布局的合成字符
 * （AltGr+Shift+A=Ą、AltGr+E=€、AltGr+P）会被误判恒本地，被终端钩子吞掉、永不达远端。
 * 与 actionForKey 同口径（该函数 Alt+P 支本就带 !ctrl）。
 * 注：Ctrl+Q 不在白名单（终端流控制 XOFF，必须透传）。
 */
export function isAlwaysLocal(e: KeyboardEvent): boolean {
  const k = e.key.toLowerCase();
  const ctrl = e.ctrlKey || e.metaKey;
  if (e.key === "ScrollLock" || e.key === "F11" || e.key === "F3") return true;
  if (e.altKey && !ctrl && k === "p") return true; // S253
  if (ctrl && e.shiftKey && !e.altKey) return true; // S253
  if (ctrl && !e.altKey) {
    if (["f", "w", "n", ",", "l", "=", "+", "-", "0"].includes(k)) return true;
    if (e.key === "Tab") return true;
  }
  // S312（M4a 键盘配置文件）：**自定义绑定的键一并恒本地**。
  //
  // 不变式是「凡有绑定的键必恒本地」——否则远程模式 + 终端焦点下该键走 arbitrate
  // 分支一被直通送往远端（R118），应用动作永不执行：用户重绑了一个键，然后发现
  // 它「在终端里不管用」，而终端恰是唯一常用的场合。MVP 期这条靠人工维护上面
  // 那串字面量与 actionForKey 的交集（shortcuts.test.ts 逐条列举守卫），自定义
  // 键位无法这样列举，故此处**从键位表反推**：绑定表即白名单的一部分，两张表
  // 再不可能分叉。
  return activeBindings()[canonicalCombo(e)] !== undefined;
}

export interface ArbitrateCtx {
  /** 焦点是否在终端内（终端侧仲裁由 Task 18 组 G 的 attachCustomKeyEventHandler 传入 true）。 */
  terminalFocused: boolean;
  mode: KeyboardMode;
}

/**
 * 按键仲裁（UI 规格 §2.7），返回 "local"（应用截获）/ "passthrough"（原样发会话）：
 * 1. 白名单键恒本地；
 * 2. 本地模式 → 其余亦本地；
 * 3. 远程模式 + 终端焦点 → 其余键一律 passthrough（R118）；
 * 4. 终端外（菜单/对话框焦点）恒本地。
 * S254 / 裁决 R118：原实现把「非白名单的复合修饰键」（Alt+*、Ctrl+Alt+*）判 local，
 * 而 actionForKey 对其无任何绑定 ⇒ 终端钩子按 local 拦下后无动作执行，按键被静默吞掉。
 * 受害面正是终端肌肉记忆：Alt+B/Alt+F（readline 词移动）、Alt+Backspace（删词）、
 * Alt+.（取上条末参）、Alt+数字（tmux/emacs 前缀）、以及 AltGr 合成字符。
 * 远程模式的定义即「按键原样发往会话」，故白名单之外不再按修饰键二次筛选。
 * 不变式（shortcuts.test.ts 有守卫用例）：凡 actionForKey 有绑定的键必属 isAlwaysLocal，
 * 故本条放宽不会送走任何应用动作。Ctrl+C/Ctrl+V 双义仍由终端侧钩子在仲裁前就地决策（§2.8）。
 */
export function arbitrate(e: KeyboardEvent, ctx: ArbitrateCtx): "local" | "passthrough" {
  if (isAlwaysLocal(e)) return "local";
  if (ctx.mode === "local") return "local";
  if (!ctx.terminalFocused) return "local";
  return "passthrough"; // S254（R118）
}

/** §4 键→动作映射结果；passthrough = 无应用动作、原样发会话。 */
export type ShortcutAction =
  | { kind: "action"; id: string }
  | { kind: "toggle-mode" }
  | { kind: "passthrough" };

/** §4 MVP 固定集 → 动作 id 表（M4a：改为数据驱动键位表，见 lib/keymap.ts）。
 * 注：Ctrl+Q 不映射（终端流控制 XOFF，必须透传）；Ctrl+C/V 双义在终端侧钩子处理。
 */
export function actionForKey(e: KeyboardEvent): ShortcutAction {
  return actionForCombo(activeBindings(), e);
}

// ── M4a 键位表运行期状态 ──────────────────────────────────────────────────────
//
// 键位表放模块级变量而不是 store：`isAlwaysLocal`/`actionForKey` 是**同步纯查询**，
// 被 xterm 的 customKeyEventHandler 每次按键调用（热路径），订阅 store 再取值会
// 把一次 Map 查找变成响应式读取。写入方唯一（loadKeyBindings/setKeyBindings），
// 读取方只读——竞态面为零。
let bindings: Record<string, string> = mergeBindings(null);

/** 当前生效键位表（默认表 ∪ 用户覆盖）。 */
export function activeBindings(): Record<string, string> {
  return bindings;
}

/** 用户键位表（settings 键 `keyboard.bindings`）→ 生效表。UI 保存后调它即时生效。 */
export function setKeyBindings(user: KeyBindings | null): void {
  bindings = mergeBindings(user);
}

/** 启动时载入用户键位表。读失败/烂值回落默认表（配置坏了退回默认键位，
 *  而不是让整个快捷键系统失效）。 */
export async function initKeyBindings(): Promise<void> {
  const raw = await settingGet<string | null>(KEYBOARD_BINDINGS_SETTING_KEY, null).catch(() => null);
  setKeyBindings(parseBindings(raw));
}

export const KEYBOARD_BINDINGS_SETTING_KEY = "keyboard.bindings";
