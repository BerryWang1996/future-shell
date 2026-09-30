import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { captureTerminal, type ScreenshotSink, type TerminalLike } from "./screenshot";
import { browserCaptureDeps } from "./screenshot-browser";
import { SearchAddon } from "@xterm/addon-search";
import { WebLinksAddon } from "@xterm/addon-web-links";
import { WebglAddon } from "@xterm/addon-webgl";
import { DEFAULT_SCHEME, schemeToXtermTheme, type Scheme } from "./term-schemes";
import { compileRules, scanLine, shouldAlert, type HighlightRule } from "./highlights"; // M4a 高亮关键字（S322）
import { actionForKey, arbitrate, toggleKeyboardMode } from "./shortcuts"; // R58：终端焦点下就地仲裁 §2.7 白名单/键盘模式
import { tabKeyboardMode } from "./tabs"; // F25 per-session 键盘模式就地仲裁（运行期调用，ESM 循环安全）
import { invoke } from "@tauri-apps/api/core"; // Task 20 Step 4 前置：剪贴板通道 arboard IPC
import "@xterm/xterm/css/xterm.css";

export function encodeB64(bytes: Uint8Array): string {
  let bin = "";
  bytes.forEach((b) => (bin += String.fromCharCode(b)));
  return btoa(bin);
}

export function decodeB64(b64: string): Uint8Array {
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

/**
 * 审计2 #35：滚动回看行数钳位（纯函数，与 Rust 侧 connmgr `limits::SCROLLBACK_MAX` /
 * grid `MAX_SCROLLBACK_LINES` 同源）。缺省/未设置 → 10000；NaN/小数/越界一律钳进
 * [1, 1_000_000]——畸形档案值不得进 xterm 构造（旧实现恒 10000，档案设置从不生效）。
 */
export function clampScrollbackLines(n?: number | null): number {
  const raw = n ?? 10000;
  if (!Number.isFinite(raw)) return 10000;
  return Math.min(1_000_000, Math.max(1, Math.floor(raw)));
}

export interface TermHandlers {
  /** 键盘输入（UTF-8 → base64），由 TerminalPane 转发 term_input */
  onData(b64: string): void;
  onResize(cols: number, rows: number): void;
  /**
   * 背压 ack：write 回调触发即上报**该帧的 seq**（累计确认，S295；spec §2.2 ④）。
   *
   * 曾是 `onAck(bytes)`（上报已消费字节数）。字节口径下后端按字节自队首排水，
   * 一旦某帧投递丢失、它的 ack 永不到来，后续 ack 只能把队首缩短而弹不掉它，
   * 帧维水位单调爬升直至锁死会话（详见 Rust 侧 `fs_terminal::flow` 模块头）。
   * 序号口径下丢帧的债只欠到下一个 ack 为止；且前端不再上报字节数，
   * 无从污染后端字节账本。
   */
  onAck(seq: number): void;
  /** 帧 base64 解码失败（畸形载荷）：该帧已丢弃，上报出口而非静默吞掉（S300）。 */
  onDecodeError?(seq: number, message: string): void;
  /** M4a 高亮关键字：命中带「提醒」的规则时回投一次（装配层联动标签角标，UI 规格 §2.11）。 */
  onHighlightAlert?(): void;
  /** bell（ESC \a）→ per-session 角标 store；前端侧检测，不给 core 管道加事件（UI 规格 §5） */
  onBell(): void;
  /** Ctrl+点击 web 链接 → open_external 白名单（UI 规格 §2.8） */
  onLink(url: string): void;
  /** 非空选区右键（或恒弹菜单模式）→ 上下文菜单（UI 规格 §2.8 条目表），坐标 = clientX/clientY，组件渲染 */
  onContextMenu(x: number, y: number): void;
}

/** 终端交互设置（UI 规格 §2.8/§2.12 四键），经 TermOptions.interactions 闭包注入、设置变更即时生效不重建终端 */
export interface TermInteractions {
  rightClick: "paste" | "menu";
  copyOnSelect: boolean;
  multilinePasteConfirm: boolean;
  /** Ctrl+V 粘贴开关（settings 键 `ui.ctrlVPaste`，默认 true；UI 规格 §2.7/§2.12）：
   *  开 = 终端焦点下 Ctrl+V 恒本地截获走粘贴流程（含多行确认），**优先于**远程模式原样发送、不向会话发 \x16；
   *  关 = 该键退出恒本地白名单、交通用仲裁（远程模式原样发 \x16），粘贴仅剩 Ctrl+Shift+V。 */
  ctrlVPaste: boolean;
}

export interface TermOptions {
  /** 会话 id（**必填**，R106）：按键仲裁读 per-session 键盘模式 `tabKeyboardMode(sessionId)`、
   *  Scroll Lock 切换 `toggleKeyboardMode(sessionId)` 均以此为键（F25 / UI 规格 §2.7/§2.12）。
   *  无此字段则下方 `opts.sessionId` 两处引用为类型错误，`npm run check` 直接红。 */
  sessionId: string;
  /** 初始配色方案（缺省 → 默认 FutureShell Dark）；三级择一由 TerminalPane 解析后传入 */
  scheme?: Scheme;
  /** 初始字号（px），默认 13（UI 规格 §3.3） */
  fontSize?: number;
  /** 交互四键闭包（每次仲裁时调用；缺省全默认：paste / 不选择即复制 / 多行确认开 / Ctrl+V 粘贴开） */
  interactions?: () => TermInteractions;
  /** 多行粘贴确认（UI 规格 §4）：入参为行数，组件渲染气泡，resolve true = 确认粘贴 */
  requestPasteConfirm?: (lines: number) => Promise<boolean>;
  /** 会话级「记住本会话」读取（绕过多行粘贴确认），缺省 false */
  pasteRemembered?: () => boolean;
  /** R58：终端焦点下经 shortcuts.arbitrate 判为 "local" 的键，其应用动作 id 由此回投给 App 级 onAction。
   *  未注入时该键仍**恒不发远端**（仅不 preventDefault/stopPropagation，动作交窗口级派发器于冒泡阶段执行）。 */
  onLocalAction?: (actionId: string) => void;
  /** 全局背景透明度 0-100，settings 键 term.opacity（UI 规格 §2.12）；缺省/≥100 保持不透明 */
  opacity?: number;
  /** S283（二轮复审整改）：气泡悬置的**权威判定闭包**（组件侧 $state 驱动，如 `() => pasteConfirm !== null`）。
   *  注入时优先于内部计数——组件状态是结算的唯一权威源，天然免疫微任务时序造成的计数/标志陈旧；
   *  缺省回落 createTerminal 内部 pasteConfirmPendingCount（独立消费者仍正确）。 */
  pasteConfirmPending?: () => boolean;
  /** 滚动回看行数（审计2 #35：档案 `term.scrollback_lines` 贯通至此，旧实现恒 10000——
   *  档案里改多少都不生效的死配置）。缺省 10000；钳位 1..=1_000_000（与 Rust 侧
   *  connmgr limits::SCROLLBACK_MAX / grid MAX_SCROLLBACK_LINES 同源）。 */
  scrollback?: number | null; // TerminalPane 未收到档案值时为 null（钳位兜底 10000）
  /** M4a 高亮关键字规则（闭包：每批输出重取，设置页改完即时生效不重建终端）。
   *  缺省无规则 = 零开销（applyHighlights 只对齐水位即返回）。 */
  highlights?: () => HighlightRule[];
}

export interface TermController {
  /** 写入一帧渲染数据；`seq` 随 `term:data` 载荷下发，write 回调据此回 ack（S295）。 */
  write(seq: number, b64: string): void;
  /** 终端内搜索（SearchAddon，UI 规格 §2.8/§4）：返回是否命中；clearSearch 清除装饰 */
  findNext(text: string): boolean;
  findPrevious(text: string): boolean;
  clearSearch(): void;
  /** 选择面（UI 规格 §2.8：选择即复制 / Ctrl+C 双义 / Ctrl+Shift+C 恒复制 / 全选） */
  getSelection(): string;
  clearSelection(): void;
  selectAll(): void;
  /** 清除滚动缓冲保留视口（UI §2.1「清除滚动缓冲」菜单项消费体） */
  clearScrollback(): void;
  /** 程序化粘贴（菜单/右键粘贴入口），经 pasteFlow 多行确认仲裁 */
  pasteText(text: string): void;
  /** 配色切换：写 xterm.js `theme` + `drawBoldTextInBrightColors` 选项，即时生效不重建实例（UI 规格 §3.2） */
  setScheme(s: Scheme): void;
  /** 字号：runtime options 动态写入，即时生效（UI 规格 §3.2/§3.3） */
  setFontSize(px: number): void;
  /** 背景透明度 0–100（全局 settings 键 term.opacity）：保持当前配色不变、只改 theme.background 的 alpha，
   *  即时生效不重建实例（UI 规格 §3.2「透明度 = theme.background 的 alpha 改写，**即时生效**」）。 */
  setOpacity(percent: number): void;
  /** per-session 缩放：deltaPx 正放大/负缩小，clamp 8–32（UI 规格 §3.3：Ctrl+滚轮 / Ctrl+= / Ctrl+-） */
  zoomFont(deltaPx: number): void;
  /** 复位到构造期基线字号（UI 规格 §4：Ctrl+0） */
  resetFontSize(): void;
  /**
   * 截当前屏为 PNG（M4b「截图」出口）。返回存盘文件名，或 `null`（写进剪贴板时）。
   *
   * 放在 controller 上而不是让调用方拿 `Terminal` 实例自己截：截图要用到
   * **当前**的配色与字号，而那两样都是本模块的内部状态（`currentScheme` 会随
   * 三级作用域变、字号会随 per-session 缩放变）。把实例交出去，调用方就得
   * 自己再维护一份同样的状态——而两份状态迟早走散，表现是截出来的图配色不对。
   */
  screenshot(sink: ScreenshotSink, title?: string): Promise<string | null>;
  /**
   * 可见屏的纯文本（M2 AI 上下文用）。
   *
   * 只取**可见区域**，不含回滚——上下文有 8 KiB 的上限（`fs_ai::context` 那边截头留尾），
   * 把一万行回滚喂进去只会让那个截断把有用的部分丢掉。
   * 而 AI 要看的本来就是「屏幕上现在是什么」。
   */
  visibleText(): string;
  fit(): void;
  focus(): void;
  dispose(): void;
}

/* ---------- 纯仲裁函数（无 DOM 依赖，红测直接消费） ---------- */

/** Ctrl+C 双义仲裁（UI 规格 §2.8）：Ctrl+Shift+C 恒复制；Ctrl+C 有选区 → 复制并清选区（拦截），无选区 → 放行让 xterm 发 \x03 */
export function ctrlCAction(
  e: { ctrlKey: boolean; shiftKey: boolean; altKey: boolean; metaKey: boolean; key: string },
  hasSelection: boolean,
): "copy" | "passthrough" | "ignore" {
  if (!e.ctrlKey || e.altKey || e.metaKey || e.key.toLowerCase() !== "c") return "ignore";
  if (e.shiftKey) return "copy"; // Ctrl+Shift+C 恒复制（恒本地白名单，UI 规格 §2.7）
  return hasSelection ? "copy" : "passthrough";
}

/** 右键分流仲裁（UI 规格 §2.8）：menu 模式恒弹菜单；paste 模式无选区 → 粘贴，非空选区 → 弹菜单（M2 AI 解读入口依赖之） */
export function contextMenuAction(rightClick: "paste" | "menu", hasSelection: boolean): "paste" | "menu" {
  return rightClick === "menu" || hasSelection ? "menu" : "paste";
}

/** 多行粘贴确认仲裁（UI 规格 §4）：需确认时返回行数，否则 null（直接粘贴）。
 *  S278：计行先剥**一个**尾部换行——从终端/记事本复制的单命令几乎必然带尾随 \n/\r\n，
 *  旧公式 split("\n").length 把它虚报为 2 行（"ls -la\n" → 「将向会话粘贴 2 行」）。
 *  触发判定仍按原文 includes("\n")（§4「多行/含换行粘贴」字面口径，尾随 \n 恰是「可能立即执行」的语义来源）。 */
export function pasteConfirmLines(text: string, opts: { multilinePasteConfirm: boolean; remembered: boolean }): number | null {
  if (!opts.multilinePasteConfirm || opts.remembered) return null;
  if (!text.includes("\n")) return null;
  return text.replace(/\r?\n$/, "").split("\n").length;
}

/** 选择即复制仲裁（UI 规格 §2.8）：开关开且选区非空 → 复制 */
export function shouldCopyOnSelect(copyOnSelect: boolean, selection: string): boolean {
  return copyOnSelect && selection.length > 0;
}

/**
 * 剪贴板通道：Task 20 Step 4 前置择一定稿（S287）——选用 Rust arboard IPC（与 vault_copy_to_clipboard 同通道），
 * 替换 navigator.clipboard 占位。前端四路入口（Ctrl+C/Ctrl+Shift+C、选择即复制、右键菜单复制；
 * Ctrl+V/Ctrl+Shift+V、右键粘贴、中键粘贴、菜单粘贴）统一经此通道。未接通时粘贴静默 no-op，
 * 由 Task 20 Step 5 item 14 ⑦ 核验。
 */
export async function clipboardWrite(text: string): Promise<void> {
  if (!text) return; // S277：空选区写入是 no-op——否则无选区 Ctrl+Shift+C（ctrlCAction 恒 copy）会以 writeText("") 清空用户剪贴板；同侪路径（copySelection/edit.copy）本就有 if(sel) 守卫
  try {
    await invoke("clipboard_write", { text });
  } catch {
    /* arboard 初始化/写入失败：降级不阻断（Task 20 Step 5 item 14 ⑦ 运行态核验） */
  }
}

export async function clipboardRead(): Promise<string> {
  try {
    return await invoke<string>("clipboard_read");
  } catch {
    return ""; // arboard 初始化/读取失败：回空串降级，粘贴路径 if(t) 守卫会跳过空串
  }
}

/** per-session 字号区间（UI 规格 §3.3）。下界防「缩到看不见又找不回来」，上界防一屏放不下一行。 */
export const FONT_SIZE_MIN = 8;
export const FONT_SIZE_MAX = 32;

/**
 * 字号钳位（Ctrl+= / Ctrl+- / Ctrl+滚轮 共用）。
 *
 * 导出仅为可测：原实现是 `createTerminal` 里 `zoom` 闭包中的一行
 * `Math.min(32, Math.max(8, …))`，而 `createTerminal` 在 jsdom 下起不来
 * （`term.open(el)` 要真实排版），于是这条区间只活在注释里。
 * 钳位丢了不会报错——用户按住 Ctrl+- 一路缩到 0 或负数，终端整块消失，
 * 而此时字号选择器已经不可见，唯一的出路是 Ctrl+0（如果他知道的话）。
 */
export function clampFontSize(px: number): number {
  return Math.min(FONT_SIZE_MAX, Math.max(FONT_SIZE_MIN, px));
}

/**
 * 背景透明度改写小工具（UI 规格 §2.12 全局 term.opacity）：a 缺省或 ≥100 保持原六位 hex，
 * 否则改写 `rgba(r, g, b, a/100)`；`allowTransparency` 已恒 true。
 *
 * 导出仅为可测（M1 出口点名「置 100 恢复不透明」——那是这里的 `>= 100` 短路分支，
 * 而它在组件层看不见：`setOpacity(100)` 的替身只能证明「调用发生了」，证明不了
 * 算出来的颜色真的不带 alpha）。生产侧消费点仍只有本模块内的两处 applyTheme。
 */
export const withAlpha = (hex: string, a?: number) => (a == null || a >= 100) ? hex : `rgba(${parseInt(hex.slice(1, 3), 16)}, ${parseInt(hex.slice(3, 5), 16)}, ${parseInt(hex.slice(5, 7), 16)}, ${a / 100})`;

/** S280：键盘装配依赖面（全部由 createTerminal 注入）。提取动机（第一轮复审 #14）：
 *  createTerminal 本体在 jsdom 下不可实例化（无 canvas/布局度量），其内 184 行键盘/粘贴接线
 *  曾零测试；把装配逻辑抽成本函数后，§2.7/§2.8 全分支与 stopPropagation 不变式可纯函数红测。 */
export interface TermKeyDeps {
  /** 会话 id（R106/F25：per-session 键盘模式仲裁键） */
  sessionId: string;
  hasSelection(): boolean;
  /** 复制当前选区并清除选区（Ctrl+C 有选区 / Ctrl+Shift+C 恒复制；空选区写入由 clipboardWrite 短路兜住） */
  copySelection(): void;
  /** 读剪贴板并进粘贴流水线（Ctrl+Shift+V 恒粘贴 / Ctrl+V 条件白名单 R101） */
  pasteFromClipboard(): void;
  zoom(deltaPx: number): void;
  resetFont(): void;
  interactions(): TermInteractions;
  /** R58：仲裁为 local 的应用动作回投装配层；未注入时该键仍恒不发远端，动作交窗口级派发器于冒泡阶段执行 */
  onLocalAction?: (actionId: string) => void;
  /** S272：粘贴确认气泡是否悬置。气泡存续期焦点可能漂移回 xterm textarea，Enter/Escape 必须在
   *  xterm 目标阶段就地消费才能抢在 \r 透传之前——窗口级兜底只负责真正结算（见 TerminalPane）。 */
  pasteConfirmPending?: () => boolean;
}

/** S280：装配 xterm attachCustomKeyEventHandler 处理器（UI 规格 §2.7/§2.8，R58/F25/R101）。
 *  键盘仲裁契约（与原 createTerminal 内联实现同注）：xterm 在**目标阶段**按本 handler 的返回值
 *  决定是否把键发往会话，而 App 的窗口级 onWindowKeydown 于冒泡阶段才运行、preventDefault 已追不回——
 *  故 §2.7 键盘模式与白名单必须在此**就地**仲裁，不能「交 Task 17 派发器统一仲裁」（旧注机制上不成立）。
 *  终端焦点即 terminalFocused: true；返回 false = 恒不发远端，返回 true = 原样透传会话。
 *  口径注（R36）：「不发远端」仅指该按键**本身不原样透传**；截获后触发的应用动作仍可经 term_input
 *  下发等效字节（如 Ctrl+L → edit.clearScreen 发 \x0c）。
 *  stopPropagation 不变式（注 41 携入验收项 b）：所有就地消费的截获分支必须 stopPropagation，
 *  否则 xterm helper textarea（TEXTAREA）上的键会被本钩子与窗口级处理器双派发。 */
export function makeTermKeyHandler(d: TermKeyDeps): (e: KeyboardEvent) => boolean {
  return (e) => {
    if (e.type !== "keydown") return true;
    // S272（复审 #20）：粘贴确认气泡悬置期，焦点漂移到 xterm textarea 后 Enter 会被透传为 \r 误发远端
    // （shell 提示符下即执行当前命令行）、Escape 取消不可达。xterm 钩子在**目标阶段**运行，是唯一能
    // 抢在 \r 透传之前的拦截点，故就地消费。注意此分支**不** stopPropagation：真正结算在窗口级兜底
    // （TerminalPane svelte:window onkeydown 的 pasteConfirm 分支）于冒泡阶段完成，两层分工明确。
    if (d.pasteConfirmPending?.() && (e.key === "Enter" || e.key === "Escape")) {
      e.preventDefault();
      return false;
    }
    // ——终端专属特例先行（均落在 arbitrate 的 local 面内）——
    const act = ctrlCAction(e, d.hasSelection());
    if (act === "copy") {
      d.copySelection();
      e.preventDefault();
      e.stopPropagation();   // 已就地消费，勿让窗口级派发器二次执行
      return false;
    }
    if (act === "passthrough") return true; // 无选区：xterm 发 \x03 至会话
    // Ctrl+Shift+V 恒粘贴（恒本地白名单）：读剪贴板并走多行确认
    if (e.ctrlKey && e.shiftKey && !e.altKey && !e.metaKey && e.key.toLowerCase() === "v") {
      e.preventDefault();
      e.stopPropagation();
      d.pasteFromClipboard();
      return false;
    }
    // Ctrl+V 条件白名单（R101；UI 规格 §2.7/§2.12 `ui.ctrlVPaste`，默认开）：
    //   开 → 恒本地截获走 pasteFlow（含多行确认），**优先于**远程模式原样发送；
    //   关 → 退出白名单，按普通单 Ctrl 键交下方通用仲裁（远程模式原样发 \x16）。
    // 必须在本处理器内就地判定：远程模式下若返回 true 放行，xterm 的 evaluateKeyboardEvent 会把
    // Ctrl+V 映射为 \x16 并 preventDefault，下方 textarea 的 `paste` 监听**永不触发**，
    // §2.12 的开关将成死档（开或关行为完全一致，且默认开时用户按 Ctrl+V 只得到 \x16）。
    if (e.ctrlKey && !e.shiftKey && !e.altKey && !e.metaKey && e.key.toLowerCase() === "v") {
      if (!d.interactions().ctrlVPaste) {
        if (arbitrate(e, { terminalFocused: true, mode: tabKeyboardMode(d.sessionId) }) === "passthrough") return true;
        e.preventDefault(); e.stopPropagation(); return false;
      }
      e.preventDefault();
      e.stopPropagation();
      d.pasteFromClipboard();
      return false;
    }
    // Ctrl+= / Ctrl+- / Ctrl+0 per-session 字号（UI 规格 §4）；Shift 形态交下方通用仲裁
    if (e.ctrlKey && !e.shiftKey && !e.altKey && !e.metaKey) {
      if (e.code === "Equal") { e.preventDefault(); e.stopPropagation(); d.zoom(1); return false; }
      if (e.code === "Minus") { e.preventDefault(); e.stopPropagation(); d.zoom(-1); return false; }
      if (e.code === "Digit0") { e.preventDefault(); e.stopPropagation(); d.resetFont(); return false; }
    }
    // ——通用仲裁（§2.7）：passthrough → 原样发会话；local → 恒不发远端（白名单 Ctrl+L/W/F、F3、Alt+P、Ctrl+Tab 等）——
    if (arbitrate(e, { terminalFocused: true, mode: tabKeyboardMode(d.sessionId) }) === "passthrough") return true;
    const action = actionForKey(e);
    if (action.kind === "toggle-mode") { // Scroll Lock：本地/远程切换（不持久化，UI §2.12）
      e.preventDefault();
      e.stopPropagation();
      void toggleKeyboardMode(d.sessionId);
      return false;
    }
    if (action.kind === "action" && d.onLocalAction) {
      e.preventDefault();
      e.stopPropagation(); // 由 onLocalAction 直投 App onAction，避免窗口级派发器重复触发
      d.onLocalAction(action.id);
    }
    return false; // 未注入 onLocalAction 时不拦冒泡：动作仍由窗口级派发器执行，但该键**不发远端**
  };
}

/** S274：搜索匹配高亮装饰（SearchAddon 只在传入 decorations 时创建高亮，未传则命中仅「选中+滚动」、
 *  clearSearch 的双清沦为空操作——复审 #12）。色值取默认方案 FutureShell Dark 的 selection/cursor 色，
 *  均 #RRGGBB 格式（addon 硬性要求）；两个 overviewRuler 键为 typings 必填字段。
 *  换配色不联动此二色为刻意取舍：匹配高亮属搜索面而非配色面，避免 scheme 切换时重写搜索装饰状态。
 *  **S287 留档（二轮复审，low，不修）**：启用 decorations 同时激活了 addon 的 onWriteParsed/onResize
 *  重搜回路（_updateMatches 200ms 防抖）——浮层开启期间每批输出/每次 resize 会重搜并经 terminal.select()
 *  重锚当前匹配，三个副作用面：① 用户手动拖选的选区被下一批输出顶掉；② copyOnSelect 开启时剪贴板随
 *  输出节奏被匹配文本反复覆写（onSelectionChange 对程序化选区同样触发）；③ 高吞吐会话每批输出重建至多
 *  1000 个装饰的渲染 churn。属上游 addon 语义（VS Code 同款），Task 20 平台验收观察高吞吐实况后再裁决
 *  是否收敛（可选方向：浮层失焦/用户手动选区时 clearSearch 断 cachedSearchTerm）。 */
const SEARCH_DECORATIONS = {
  matchBackground: "#2b3a55",
  activeMatchBackground: "#4f8cff",
  matchOverviewRuler: "#2b3a55",
  activeMatchColorOverviewRuler: "#4f8cff",
};

/**
 * 高亮装饰的单会话上限（S322）。
 *
 * 装饰是真实 DOM/GPU 对象，无上限即等于让「一条匹配 `.` 的规则」把内存和渲染
 * 一起吃光。到顶后停止新增（不做 LRU 淘汰：旧行会滚出视口，淘汰逻辑的复杂度
 * 换不来可感收益），并在控制台留一句——高亮是辅助面，静默停更优于拖垮会话。
 */
const MAX_HIGHLIGHT_DECORATIONS = 2000;

export function createTerminal(el: HTMLElement, h: TermHandlers, opts: TermOptions): TermController { // R106：`opts` 无默认值——`sessionId` 必填，`= {}` 会绕过必填校验
  const scheme = opts.scheme ?? DEFAULT_SCHEME;
  const baselineFontSize = opts.fontSize ?? 13; // per-session 缩放基线，不回写 settings（UI 规格 §3.3）
  // 主题的两个自变量各自可变、且必须合成后一次性写入 term.options.theme（xterm 的 theme 是整体替换，
  // 不是逐字段合并）。原实现里 setScheme 闭包读的是构造期的 opts.opacity——透明度改了没有写入端，
  // 而**换配色时又会把当时的 opts.opacity 重新糊回去**，即便将来补上写入端也会被下一次换色打回。
  let currentScheme = scheme;
  let currentOpacity = opts.opacity;
  const applyTheme = (): void => {
    term.options.theme = { ...schemeToXtermTheme(currentScheme), background: withAlpha(currentScheme.background, currentOpacity) };
    term.options.drawBoldTextInBrightColors = currentScheme.boldBright;
  };
  const interactions = opts.interactions ?? (() => ({ rightClick: "paste", copyOnSelect: false, multilinePasteConfirm: true, ctrlVPaste: true }));
  const remembered = opts.pasteRemembered ?? (() => false);
  const scrollback = clampScrollbackLines(opts.scrollback); // 审计2 #35：档案滚动行数（缺省 10000，钳位兜底）
  const term = new Terminal({
    fontFamily: "'Cascadia Code', 'JetBrains Mono', 'Sarasa Mono SC', Consolas, 'Courier New', monospace",
    fontSize: baselineFontSize,
    scrollback,
    allowTransparency: true, // 构造期恒定；背景透明度为全局设置 term.opacity（UI 规格 §2.12），以 theme.background 的 alpha 改写应用（UI 规格 §3.2）
    allowProposedApi: true, // search addon decorations 依赖 Terminal.registerDecoration（6.0.0 标 EXPERIMENTAL）
    theme: { ...schemeToXtermTheme(scheme), background: withAlpha(scheme.background, opts.opacity) },
    drawBoldTextInBrightColors: scheme.boldBright,
  });
  const fit = new FitAddon();
  const search = new SearchAddon();
  term.loadAddon(fit);
  term.loadAddon(search);
  term.open(el);
  // 渲染器降级链：webgl → DOM 渲染器（xterm 6.0.0 内置默认；6.0 已移除 canvas 渲染器 #5105，降级链恰为 webgl→DOM 两档）。
  try { const gl = new WebglAddon(); gl.onContextLoss(() => gl.dispose()); term.loadAddon(gl); } catch { /* 无 GPU/WebGL 上下文时回退 DOM 渲染器 */ }
  // drawBoldTextInBrightColors 在 DOM 渲染器下同样生效（源码核验：DomRendererRowFactory P16/P256）。

  // web-links：仅 Ctrl+点击打开外链（UI 规格 §2.8），普通点击保留给终端选择，防误触导航
  term.loadAddon(new WebLinksAddon((event, uri) => {
    if (event.ctrlKey) h.onLink(uri);
  }));

  // S292（复审一波中危 #5）：onResize 转发必须在**首次 fit 之前**注册——fit.fit() 同步派发 resize 事件，
  // 若注册在后则初次 fit 的尺寸上报丢失，且 FitAddon 同尺寸守卫使 ResizeObserver 复跑不再触发，
  // 远端 PTY 将滞留后端硬编码的 24×80（vim/htop 错位，直到用户手动改变窗口尺寸）。
  term.onData((data) => h.onData(encodeB64(new TextEncoder().encode(data))));
  term.onResize(({ cols, rows }) => h.onResize(cols, rows));
  term.onBell(() => h.onBell());

  fit.fit();

  // per-session 字号缩放（UI 规格 §3.3/§4）：clamp 见 clampFontSize，缩放后 fit 重排
  const zoom = (deltaPx: number): void => {
    const cur = term.options.fontSize ?? baselineFontSize;
    term.options.fontSize = clampFontSize(cur + deltaPx);
    fit.fit();
  };
  const resetFont = (): void => {
    term.options.fontSize = baselineFontSize;
    fit.fit();
  };

  // 选择即复制（UI 规格 §2.8 开关 ui.copyOnSelect）：订阅即时生效
  term.onSelectionChange(() => {
    const sel = term.getSelection();
    if (shouldCopyOnSelect(interactions().copyOnSelect, sel)) void clipboardWrite(sel);
  });

  // S272/S283：气泡悬置**计数**（非布尔）——pasteFlow awaiting 期间 >0，供键盘钩子在 xterm 目标阶段拦
  // Enter/Escape（防 \r 误发）。S283（二轮复审整改）：重入（paste 模式右键 / 菜单「粘贴」第二次多行粘贴）
  // 时 TerminalPane 以 false 结算旧泡，旧流 finally 是微任务、必在新泡已置位后运行；若用布尔，旧流会把
  // 标志清成 false 而新泡仍在待决 ⇒ Enter 防透传保护对存续气泡失效。计数下旧流递减 2→1 仍判悬置。
  let pasteConfirmPendingCount = 0;

  // 粘贴流水线：多行粘贴确认气泡（UI 规格 §4「将向会话粘贴 N 行，可能立即执行」+「记住本会话」）
  const pasteFlow = async (text: string): Promise<void> => {
    const ia = interactions();
    const lines = pasteConfirmLines(text, { multilinePasteConfirm: ia.multilinePasteConfirm, remembered: remembered() });
    if (lines !== null) {
      pasteConfirmPendingCount++;
      let ok = true;
      try {
        ok = opts.requestPasteConfirm ? await opts.requestPasteConfirm(lines) : true;
      } finally {
        pasteConfirmPendingCount--;
      }
      if (!ok) return; // 取消：不落盘会话
    }
    term.paste(text);
  };

  // 键盘仲裁：装配逻辑已提取为 makeTermKeyHandler（S280，可纯函数红测；契约/口径注见该函数头）
  term.attachCustomKeyEventHandler(makeTermKeyHandler({
    sessionId: opts.sessionId, // R106/F25
    hasSelection: () => term.hasSelection(),
    copySelection: () => { void clipboardWrite(term.getSelection()); term.clearSelection(); }, // §2.8：复制并清除选区
    pasteFromClipboard: () => { void clipboardRead().then((t) => { if (t) void pasteFlow(t); }); },
    zoom,
    resetFont,
    interactions,
    onLocalAction: opts.onLocalAction, // R58
    pasteConfirmPending: opts.pasteConfirmPending ?? (() => pasteConfirmPendingCount > 0), // S272/S283：权威闭包优先，缺省回落内部计数
  }));

  // 粘贴拦截（xterm 内部 textarea）：覆盖系统/浏览器菜单粘贴、中键粘贴等路径（Ctrl+V 已于上方按键处理器就地截获——R101：远程模式下本监听收不到 Ctrl+V）
  term.textarea?.addEventListener("paste", (e) => {
    const text = e.clipboardData?.getData("text") ?? "";
    e.preventDefault();
    if (text) void pasteFlow(text);
  });

  // S279：容器 el 上的监听器经 AbortController 注册，dispose 时 abort 一次性拆除——
  // 原匿名监听器无引用可 removeEventListener，若 el 生命周期长于控制器（容器复用等时序）
  // 右键/Ctrl+滚轮会触到已 dispose 的终端（复审 #3 卫生缺口）。
  const ac = new AbortController();

  // 右键分流（UI 规格 §2.8）：paste 模式无选区 → 粘贴；非空选区或 menu 模式 → 组件渲染上下文菜单
  el.addEventListener("contextmenu", (e) => {
    e.preventDefault();
    const ia = interactions();
    if (contextMenuAction(ia.rightClick, term.hasSelection()) === "paste") {
      void clipboardRead().then((t) => { if (t) void pasteFlow(t); });
    } else {
      h.onContextMenu(e.clientX, e.clientY);
    }
  }, { signal: ac.signal });

  // Ctrl+滚轮 per-session 字号（UI 规格 §3.3）：preventDefault 防浏览器页面缩放
  const onWheel = (e: WheelEvent): void => {
    if (!e.ctrlKey) return;
    e.preventDefault();
    zoom(e.deltaY < 0 ? 1 : -1);
  };
  el.addEventListener("wheel", onWheel as EventListener, { passive: false, signal: ac.signal });

  const ro = new ResizeObserver(() => fit.fit());

  // ── M4a 高亮关键字（S322）────────────────────────────────────────────────
  // 规则由装配层供给（opts.highlights，读 settings 键 term.highlights），
  // 每次 write 回调扫新行、命中即注册装饰、带提醒的命中回投 onHighlightAlert。
  let hlCompiled = compileRules(opts.highlights?.() ?? []);
  /** 已扫过的绝对行号上界（buffer.baseY + cursorY 口径），避免重复扫同一行。 */
  let hlScannedTo = -1;
  let hlDecorations = 0;
  let hlCapWarned = false;

  function applyHighlights(): void {
    // 规则每批重取：设置页改完即时生效，不必重开会话（compileRules 对同一份
    // 数组是纯函数，代价是每批一次浅比较级别的编译——规则数 ≤64，可忽略）
    const rules = opts.highlights?.() ?? [];
    if (rules.length === 0) {
      hlScannedTo = term.buffer.active.baseY + term.buffer.active.cursorY;
      return;
    }
    hlCompiled = compileRules(rules);
    const buf = term.buffer.active;
    const cur = buf.baseY + buf.cursorY;
    // 首次调用只对齐水位、不回扫历史：连接瞬间的欢迎横幅不该被当成"新输出"
    // 触发一屏提醒角标。
    if (hlScannedTo < 0) {
      hlScannedTo = cur;
      return;
    }
    // 扫描窗口有界：至多回看一屏（滚动极快时漏掉中间行，胜过卡住 UI；
    // 高亮是辅助面，漏一行的代价远小于每批扫全量 scrollback）
    const from = Math.max(hlScannedTo + 1, cur - term.rows);
    let alerted = false;
    for (let y = from; y <= cur; y++) {
      const line = buf.getLine(y);
      if (!line) continue;
      const text = line.translateToString(true);
      if (!text) continue;
      const hits = scanLine(hlCompiled, text);
      if (hits.length === 0) continue;
      if (shouldAlert(hits)) alerted = true;
      for (const hit of hits) {
        if (hlDecorations >= MAX_HIGHLIGHT_DECORATIONS) {
          if (!hlCapWarned) {
            hlCapWarned = true;
            console.warn(`高亮装饰达上限 ${MAX_HIGHLIGHT_DECORATIONS}，本会话停止新增高亮`);
          }
          break;
        }
        const marker = term.registerMarker(y - cur);
        if (!marker) continue;
        const deco = term.registerDecoration({
          marker,
          x: hit.start,
          width: Math.max(1, hit.end - hit.start),
          layer: "bottom", // 底层：不盖住光标与选区
        });
        if (!deco) continue;
        hlDecorations++;
        deco.onRender((elm) => {
          elm.style.backgroundColor = hit.rule.color;
          // 命中底色可能与前景色撞（深色规则 + 深色文字），故同时给一个
          // 对比色文字：不改前景会出现「标了色反而看不见」的行。
          elm.style.opacity = "0.35";
        });
      }
    }
    hlScannedTo = cur;
    if (alerted) h.onHighlightAlert?.();
  }  ro.observe(el);

  return {
    write(seq: number, b64: string) {
      // S300：decodeB64 会同步抛（atob 遇畸形 base64）。抛在这里等于静默丢帧——
      // 该帧永不 write、永不 ack，而后端已记账。累计确认让这笔债有界，但出口
      // 仍须留痕，否则下次又只能靠猜。注意异常必须在 term.write **之外**处理：
      // 抛进 xterm 的 WriteBuffer 会让 _innerWrite 提前返回且不再重新调度
      // （WriteBuffer.ts 的 `if (!this._writeBuffer.length)` 守卫），整条渲染
      // 流水线连同所有回调永久冻结。
      let bytes: Uint8Array;
      try {
        bytes = decodeB64(b64);
      } catch (e) {
        h.onDecodeError?.(seq, String(e));
        return;
      }
      // xterm.js write 回调驱动背压 ack（spec §2.2 ④）：回 seq 而非字节数
      term.write(bytes, () => {
        // M4a 高亮关键字（S322）：write 回调里扫**已解析入缓冲**的新行。
        //
        // 时机是硬约束：必须在 write 回调内（此刻解析器已把这批字节写进 buffer，
        // 行内容与光标位置才是确定的），不能在 write 之前扫原始字节——那时行还
        // 没成形、ANSI 还在里面。扫描本身有界（每批至多 term.rows 行，单行截断
        // 到 MAX_LINE_SCAN），不随会话长度增长。
        applyHighlights();
        h.onAck(seq);
      });
    },
    findNext(text: string) { return search.findNext(text, { decorations: SEARCH_DECORATIONS }); }, // S274：匹配高亮（未传 decorations 则零高亮）
    findPrevious(text: string) { return search.findPrevious(text, { decorations: SEARCH_DECORATIONS }); },
    clearSearch() {
      // clearDecorations/clearActiveDecoration 为公开 API（0.16.0 typings 实证）；allowProposedApi 供 decorations 内部 registerDecoration 使用
      search.clearDecorations();
      search.clearActiveDecoration();
      // S273（复审 #11，medium）：SearchAddon 命中时经 terminal.select() 留下「机器选区」，双清不触碰选区。
      // 残留选区会翻转 §2.8 的选区态规则：关闭搜索后首按 Ctrl+C 吞掉 \x03 变成复制、右键 paste 模式弹菜单
      // 而非粘贴；copyOnSelect 开启时翻匹配还会逐次覆写剪贴板（onSelectionChange 对程序化选区同样触发）。
      // **取舍留字**：清选区同时抹掉 F3 续搜的锚点（SearchAddon 以当前选区位置为 findNext 续搜起点）——
      // 续搜经 SearchOverlay.findNextAgain 重开浮层后 findNext(query) 从缓冲区按词重搜，可用性不损；
      // 浮层开启期间的连续 Enter/F3 不受影响（选区在、锚点仍在）。VS Code 保留残留选区，本项目以 §2.8
      // 肌肉记忆语义优先取清除。
      term.clearSelection();
    },
    getSelection() { return term.getSelection(); },
    clearSelection() { term.clearSelection(); },
    selectAll() { term.selectAll(); },
    clearScrollback() { term.clear(); },
    pasteText(text: string) { void pasteFlow(text); },
    setScheme(s: Scheme) {
      currentScheme = s;
      applyTheme(); // 用 currentOpacity 而非构造期 opts.opacity：否则换色即把透明度打回初值
    },
    setOpacity(percent: number) {
      if (percent === currentOpacity) return; // 等值早退：三级配色 $effect 每次都会顺带调它，不该白写一次 theme（整体替换 ⇒ 全屏重绘）
      currentOpacity = percent;
      applyTheme();
    },
    // S271（复审 #7）：与 zoom/resetFont 对齐——仅写 options 时 RenderService 以既有 cols/rows 重渲染、
    // 不按容器重排（node_modules RenderService.ts fontSize 变更路径核验），运行期改字号会溢出裁切且
    // 不发 term_resize；补 fit() 令网格按容器像素重算并经既有 onResize 管线自动上报。
    setFontSize(px: number) { term.options.fontSize = px; fit.fit(); },
    zoomFont(deltaPx: number) { zoom(deltaPx); },
    resetFontSize() { resetFont(); },
    /**
     * 截当前屏。配色与字号取的是**此刻**的运行期值，不是构造期的 opts——
     * 那两样都会变（三级作用域换配色、per-session 缩放改字号），
     * 用构造期的值会截出一张与屏幕上不一样的图。
     */
    screenshot(sink: ScreenshotSink, title?: string) {
      return captureTerminal(
        term as unknown as TerminalLike,
        {
          foreground: currentScheme.foreground,
          background: currentScheme.background,
          ansi: currentScheme.ansi,
          fontFamily: term.options.fontFamily ?? "monospace",
          fontSize: term.options.fontSize ?? baselineFontSize,
          // xterm 的 lineHeight 是倍率，缺省 1.0。这里跟着它走而不是写死，
          // 否则改过行高的终端截出来行距会不对。
          lineHeight: term.options.lineHeight ?? 1,
        },
        sink,
        browserCaptureDeps,
        title,
      );
    },
    visibleText() {
      const buf = term.buffer.active;
      const lines: string[] = [];
      for (let row = 0; row < term.rows; row++) {
        const line = buf.getLine(buf.viewportY + row);
        // translateToString(true) 去掉行尾空白。不去的话每行都补到 80 列，
        // 一屏就是几千个空格——而上下文有字节上限，那些空格会挤掉真正的内容。
        lines.push(line ? line.translateToString(true) : "");
      }
      // 尾部连续空行去掉：终端下半屏通常是空的，留着同样是在挤上限。
      while (lines.length > 0 && lines[lines.length - 1] === "") lines.pop();
      return lines.join("\n");
    },
    fit() { fit.fit(); },
    focus() { term.focus(); },
    dispose() { ac.abort(); ro.disconnect(); term.dispose(); }, // S279：abort 一并摘除 el 上 contextmenu/wheel 监听
  };
}
