/**
 * vwindow.ts — 应用内**虚拟窗口**的状态与几何（M7.3 第三条出口）。
 *
 * # 为什么是虚拟窗口而不是真窗口
 *
 * 形态裁定（用户 2026-08-28）：应用内虚拟窗口，不做真原生多窗口。实测依据是
 * `app/capabilities/default.json` 的作用域只有 `"windows": ["main"]`，权限集里没有
 * `core:webview:allow-create-webview-window`——前端今天创建不了第二个原生窗口。
 * 走虚拟窗口则不动 capabilities、不扩权限面，且拖动/叠放/关闭的行为三平台逐像素一致
 * （全在 WebView 里算，不经过任何窗口管理器）。
 *
 * （M4b 的「标签拖出」是另一回事：那条路的窗口由 **Rust 侧** 创建，见 `window_cmd.rs`。）
 *
 * # 出口标准里那句「不可拖出可视区外」是本模块的核心
 *
 * 一个被拖到画布外的窗口**找不回来**：它没有任务栏、没有 Alt+Tab、没有「窗口」菜单。
 * 所以位置不是「记下用户拖到哪」，而是「记下**夹紧之后**的位置」——[`clampRect`] 是
 * 所有位移/缩放/视口变化的唯一出口，任何绕过它的写法都会造出一个拿不回来的窗口。
 *
 * 视口变小（用户缩小主窗口、拉开监控面板）同样会把窗口挤出去，所以 [`reflowVWindows`]
 * 必须在容器尺寸变化时被调用——只在拖动时夹紧是不够的。
 */
import { get, writable } from "svelte/store";

/** 窗口里装什么。目前只有一种；留成联合类型是为了下一种进来时不用改调用点。 */
export type VWindowKind = "sftp";

export interface VWindow {
  id: string;
  kind: VWindowKind;
  /** 这个窗口服务于哪个会话（内容组件据此取数据）。 */
  sessionId: string;
  title: string;
  x: number;
  y: number;
  w: number;
  h: number;
  /** 叠放次序，越大越靠前。 */
  z: number;
}

export interface Viewport {
  width: number;
  height: number;
}

/** 最小尺寸：再小就看不到文件名列，窗口成了一个没用的方块。 */
export const MIN_W = 360;
export const MIN_H = 220;
/** 新窗口的默认尺寸（放不下时由 clampRect 收到视口内）。 */
export const DEFAULT_W = 760;
export const DEFAULT_H = 460;
/** 层叠新窗口的偏移量。 */
const CASCADE = 28;

export const vwindows = writable<VWindow[]>([]);

let seq = 0;

interface Rect {
  x: number;
  y: number;
  w: number;
  h: number;
}

/**
 * 把一个矩形夹进视口。**位置与尺寸一起夹**，顺序是先尺寸后位置：
 * 先把过大的窗口收到视口以内，再定位；反过来做的话，一个比视口还宽的窗口会先被推到 x=0，
 * 然后仍然溢出右边。
 *
 * 视口比最小尺寸还小时（用户把主窗口拉得极窄）不再强求装下——那时保底给出 `MIN_*`，
 * 位置夹到 0：宁可露出一角被裁，也不能把窗口挪到看不见的地方。
 */
export function clampRect(r: Rect, vp: Viewport): Rect {
  const w = Math.max(MIN_W, Math.min(r.w, Math.max(MIN_W, vp.width)));
  const h = Math.max(MIN_H, Math.min(r.h, Math.max(MIN_H, vp.height)));
  const maxX = Math.max(0, vp.width - w);
  const maxY = Math.max(0, vp.height - h);
  return {
    w,
    h,
    x: Math.round(Math.min(Math.max(0, r.x), maxX)),
    y: Math.round(Math.min(Math.max(0, r.y), maxY)),
  };
}

/** 新窗口的落点：从已有窗口数往下层叠，撞到边就绕回原点重来。 */
export function cascadeOrigin(count: number, size: { w: number; h: number }, vp: Viewport): { x: number; y: number } {
  const stepsX = Math.max(1, Math.floor((vp.width - size.w) / CASCADE) || 1);
  const stepsY = Math.max(1, Math.floor((vp.height - size.h) / CASCADE) || 1);
  const steps = Math.max(1, Math.min(stepsX, stepsY));
  const i = count % steps;
  return { x: CASCADE * i, y: CASCADE * i };
}

/** 当前最高层号（空表时 0）。 */
function topZ(list: VWindow[]): number {
  return list.reduce((m, w) => Math.max(m, w.z), 0);
}

/**
 * 打开一个虚拟窗口；同 `kind + sessionId` 的窗口**已存在时不新开**，而是置顶并返回原 id。
 *
 * 重复打开会造出两个内容一模一样、只有位置不同的窗口——用户点第二次的意思是
 * 「把那个窗口拿到前面来」，不是「再来一个」。
 */
export function openVWindow(spec: { kind: VWindowKind; sessionId: string; title: string }, vp: Viewport): string {
  const list = get(vwindows);
  const existing = list.find((w) => w.kind === spec.kind && w.sessionId === spec.sessionId);
  if (existing) {
    raiseVWindow(existing.id);
    return existing.id;
  }
  const origin = cascadeOrigin(list.length, { w: DEFAULT_W, h: DEFAULT_H }, vp);
  const rect = clampRect({ ...origin, w: DEFAULT_W, h: DEFAULT_H }, vp);
  const id = `vw-${++seq}`;
  vwindows.set([...list, { id, z: topZ(list) + 1, ...spec, ...rect }]);
  return id;
}

export function closeVWindow(id: string): void {
  vwindows.update((l) => l.filter((w) => w.id !== id));
}

/** 会话没了：它的窗口一并收走。留着的话里面是一个对着死会话刷错误的文件列表。 */
export function closeVWindowsForSession(sessionId: string): void {
  vwindows.update((l) => l.filter((w) => w.sessionId !== sessionId));
}

/** 置顶。已经在最上面时**不改动**——否则每次点击都让 z 无谓地长一格。 */
export function raiseVWindow(id: string): void {
  vwindows.update((l) => {
    const w = l.find((x) => x.id === id);
    if (!w || w.z === topZ(l)) return l;
    return l.map((x) => (x.id === id ? { ...x, z: topZ(l) + 1 } : x));
  });
}

/** 移动到（夹紧后的）新位置。 */
export function moveVWindow(id: string, x: number, y: number, vp: Viewport): void {
  vwindows.update((l) =>
    l.map((w) => (w.id === id ? { ...w, ...clampRect({ x, y, w: w.w, h: w.h }, vp) } : w)),
  );
}

/** 改尺寸（右下角）。位置不动，但尺寸变化可能把窗口挤出视口，故照样过夹紧。 */
export function resizeVWindow(id: string, w: number, h: number, vp: Viewport): void {
  vwindows.update((l) =>
    l.map((x) => (x.id === id ? { ...x, ...clampRect({ x: x.x, y: x.y, w, h }, vp) } : x)),
  );
}

/**
 * 视口变了：把所有窗口重新夹一遍。
 *
 * 不做这一步的话，用户把主窗口拉小、或者拉开底部监控面板，右下角那些窗口就整块留在
 * 可视区外了——而它们没有任务栏可以找回来。
 */
export function reflowVWindows(vp: Viewport): void {
  vwindows.update((l) => l.map((w) => ({ ...w, ...clampRect(w, vp) })));
}

/** 仅测试用。 */
export function resetVWindowsForTest(): void {
  vwindows.set([]);
  seq = 0;
}
