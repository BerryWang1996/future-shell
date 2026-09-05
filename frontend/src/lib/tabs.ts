import { get, writable } from "svelte/store";
import type { ConnectFailure } from "./types";
import { keyboardModeDefault, type KeyboardMode } from "./shortcuts"; // 运行期调用，ESM 循环安全（F25：addTab 初值取默认值 store）

/** 连接态四态（UI 规格 §5 三处一致呈现：标签状态图标 / 侧栏状态灯 / 状态栏文本） */
export type TabStatus = "connecting" | "connected" | "disconnected" | "error";

/** 标签承载的会话类型（阶段 1 RDP）：决定渲染 TerminalPane 还是 RdpPane。 */
/** 标签底下是什么传输。`serial` 与 `ssh` 都渲染终端，区别在于串口没有 SFTP/监控/隧道
 *  这些依赖 SSH 通道的东西——装配层据此把那些入口收起来，而不是让用户点了报「no session」。 */
export type TabKind = "ssh" | "rdp" | "serial";

/** 标签模型（UI 规格 §1.2 标签内容 = 状态图标 + 名称 + 关闭 ×；§5 铃铛角标） */
export interface Tab {
  /** 与 sessionId 同值，唯一键 */
  id: string;
  title: string;
  /** 输出提醒角标（bell/关键词触发），切回该标签或单击角标即清除 */
  bell: boolean;
  /** 连接态：connecting 蓝色旋转环 / connected 绿点 / disconnected 灰 / error 红（UI 规格 §5） */
  status: TabStatus;
  /** 所属 Profile id：侧栏状态灯按 profileId 反查挂灯（Task 20 sessionStates 接线） */
  profileId: string;
  /** host:port 展示串：状态栏 host 段（Task 20 openSession 写入；空串 = 未写入） */
  host: string;
  /** 会话类型（默认 ssh：既有标签数据没有这个概念，缺省即 SSH） */
  kind: TabKind;
  /** 键盘模式（F25 per-session，UI 规格 §2.12）：初值取 keyboardModeDefault；会话内切换（Scroll Lock / 状态栏键盘段）不持久化 */
  keyboardMode: KeyboardMode;
  /** 错误文案（F29①）：setTabError 随 status="error" 并写；关闭确认 / 重连 banner 文案消费 */
  errorText?: string;
  /** 结构化失败（2026-09-01）：setTabFailure 随 status="error" 并写；失败面板据此决定挂哪些按钮。
   *  老式字符串失败（如 MCP/其它路径）只有 errorText，本字段缺省——面板退化成纯文案 + 重试。 */
  failure?: ConnectFailure;
}

export const tabs = writable<Tab[]>([]);
export const activeTabId = writable<string | null>(null);

/** 会话临时配色 store（R11 transient 层，R16 收进 M1）：key = sessionId（tab.id 与 sessionId 1:1，总设计 §2.4-3），
 *  value = term-schemes 方案 id；标签右键「配色方案 ▾」子菜单经 setTransientScheme 写入（Task 20 TabBar），
 *  装配层经 $transientSchemes.get(sessionId) 读取并传 TerminalPane 的 tempSchemeId prop；会话内存态：不写库、关标签即释放 */
export const transientSchemes = writable<Map<string, string>>(new Map());

/** 设置/清除本会话临时配色（schemeId = null 即「跟随默认」清除项，UI 规格 §1.3）；新 Map 替换保证订户感知变更 */
export function setTransientScheme(sessionId: string, schemeId: string | null): void {
  transientSchemes.update((m) => {
    const next = new Map(m);
    if (schemeId === null) next.delete(sessionId);
    else next.set(sessionId, schemeId);
    return next;
  });
}

/** MRU 访问序栈（头 = 当前活动；Ctrl+Tab/Ctrl+Shift+Tab 消费，UI 规格 §1.2/§4） */
let mru: string[] = [];
let mruCursor = 0;

/** 红测重置入口：同时清空 tabs/activeTabId/MRU 栈/transientSchemes 临时配色 */
export function resetTabStore(): void {
  tabs.set([]);
  activeTabId.set(null);
  mru = [];
  mruCursor = 0;
  transientSchemes.set(new Map());
}

/** 建标签并立即激活（Task 20：Ctrl+N / 侧栏双击连接）；默认 connecting 占位标签，消除连接期静默（UI 规格 §5 连接中行） */
export function addTab(
  sessionId: string,
  title: string,
  profileId: string,
  status: TabStatus = "connecting",
  host: string = "",
  kind: TabKind = "ssh",
): void {
  tabs.update((list) => [
    ...list,
    { id: sessionId, title, bell: false, status, profileId, host, kind, keyboardMode: get(keyboardModeDefault) },
  ]);
  mru = [sessionId, ...mru.filter((id) => id !== sessionId)];
  mruCursor = 0;
  activeTabId.set(sessionId);
}

/** 移除标签；若移除的是活动标签，回退 MRU 最近访问者，无 MRU 取相邻标签；随释会话临时配色（R16） */
export function removeTab(sessionId: string): void {
  const list = get(tabs);
  const idx = list.findIndex((t) => t.id === sessionId);
  if (idx === -1) return;
  const rest = list.filter((t) => t.id !== sessionId);
  tabs.set(rest);
  mru = mru.filter((id) => id !== sessionId);
  mruCursor = 0;
  setTransientScheme(sessionId, null); // 关闭标签即释放临时配色（R16，Task 25 出口项）
  if (get(activeTabId) === sessionId) {
    activeTabId.set(mru[0] ?? rest[idx - 1]?.id ?? rest[idx]?.id ?? null);
  }
}

/** 切换标签并清其角标（UI 规格 §5 清除手势：切回该标签自动清除）；同步维护 MRU 访问序 */
export function activateTab(sessionId: string): void {
  activeTabId.set(sessionId);
  clearBell(sessionId);
  mru = [sessionId, ...mru.filter((id) => id !== sessionId)];
  mruCursor = 0;
}

/** Ctrl+Tab：按访问序切到上一个标签（循环；遍历中切回亦清角标） */
export function activateMruNext(): void {
  if (mru.length < 2) return;
  mruCursor = (mruCursor + 1) % mru.length;
  activeTabId.set(mru[mruCursor]);
  clearBell(mru[mruCursor]);
}

/** Ctrl+Shift+Tab：访问序反向遍历（循环） */
export function activateMruPrev(): void {
  if (mru.length < 2) return;
  mruCursor = (mruCursor - 1 + mru.length) % mru.length;
  activeTabId.set(mru[mruCursor]);
  clearBell(mru[mruCursor]);
}

/** 连接态迁移（Task 20 三事件桥 session:status / session:disconnected / session:closed 驱动）。
 *  P1-11：迁到 connected 时必须一并抹掉 errorText——否则重连成功后状态栏 error 段仍挂着上一轮
 *  「重连失败，已停止自动重连」，与绿点状态自相矛盾（errorText 只由 setTabError 写、从无清除方）。
 *  非 connected 的迁移不动 errorText：disconnected 期间 banner 仍需读上一次失败原因。 */
export function setTabStatus(sessionId: string, status: TabStatus): void {
  tabs.update((list) =>
    list.map((t) =>
      t.id === sessionId
        ? (status === "connected" ? { ...t, status, errorText: undefined } : { ...t, status })
        : t,
    ),
  );
}

/** 本会话键盘模式切换（F25 per-session，不持久化——UI 规格 §2.12；Scroll Lock 与状态栏键盘段点击共用此入口） */
export function setTabKeyboardMode(sessionId: string, mode: KeyboardMode): void {
  tabs.update((list) => list.map((t) => (t.id === sessionId ? { ...t, keyboardMode: mode } : t)));
}

/** 错误态并写错误文案（F29①：关闭确认 / 重连 banner 文案消费；与 setTabStatus 同批） */
export function setTabError(sessionId: string, message: string): void {
  tabs.update((list) => list.map((t) => (t.id === sessionId ? { ...t, status: "error", errorText: message } : t)));
}

/** 结构化失败落标签（2026-09-01）：errorText 取 summary（与 setTabError 同口径），failure 整份留给面板。 */
export function setTabFailure(sessionId: string, failure: ConnectFailure): void {
  tabs.update((list) =>
    list.map((t) => (t.id === sessionId ? { ...t, status: "error", errorText: failure.summary, failure } : t)),
  );
}

/** 读本会话键盘模式（未知会话缺省 "remote"；shortcuts 就地仲裁运行期调用，ESM 循环安全） */
export function tabKeyboardMode(sessionId: string): KeyboardMode {
  return get(tabs).find((t) => t.id === sessionId)?.keyboardMode ?? "remote";
}

/** 占位标签转正：connecting 占位拿到真实 id 后替换（Task 20 openSession），保持位置/标题/状态/活动性 */
export function replaceTabId(oldId: string, newId: string): void {
  tabs.update((list) => list.map((t) => (t.id === oldId ? { ...t, id: newId } : t)));
  mru = mru.map((id) => (id === oldId ? newId : id));
  if (get(activeTabId) === oldId) activeTabId.set(newId);
  // S264：临时配色以 sessionId 为键，转正后若不随迁则双向失效——读取方按新 id 取不到（占位期右键设的配色静默还原），
  // removeTab 亦按新 id 释放（旧键永久滞留 Map）。单次 update 内迁移，不产生中间态。
  transientSchemes.update((m) => {
    if (!m.has(oldId)) return m;
    const next = new Map(m);
    next.set(newId, next.get(oldId)!);
    next.delete(oldId);
    return next;
  });
}

/** 拖拽排序：将 dragId 移动到 targetId 的位置（MVP 同窗拖拽排序，UI 规格 §1.2） */
export function reorderTab(dragId: string, targetId: string): void {
  if (dragId === targetId) return;
  tabs.update((list) => {
    const from = list.findIndex((t) => t.id === dragId);
    const to = list.findIndex((t) => t.id === targetId);
    if (from === -1 || to === -1) return list;
    const next = [...list];
    const [moved] = next.splice(from, 1);
    next.splice(to, 0, moved);
    return next;
  });
}

/** 置位角标（TerminalPane 在 term.onBell 回调中调用） */
export function ringBell(sessionId: string): void {
  tabs.update((list) => list.map((t) => (t.id === sessionId ? { ...t, bell: true } : t)));
}

/** 手动清除角标（单击角标，UI 规格 §5） */
export function clearBell(sessionId: string): void {
  tabs.update((list) => list.map((t) => (t.id === sessionId ? { ...t, bell: false } : t)));
}
