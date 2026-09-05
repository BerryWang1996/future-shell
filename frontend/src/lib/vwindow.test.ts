import { beforeEach, describe, expect, it } from "vitest";
import { get } from "svelte/store";
import {
  cascadeOrigin,
  clampRect,
  closeVWindow,
  closeVWindowsForSession,
  DEFAULT_H,
  DEFAULT_W,
  MIN_H,
  MIN_W,
  moveVWindow,
  openVWindow,
  raiseVWindow,
  reflowVWindows,
  resetVWindowsForTest,
  resizeVWindow,
  vwindows,
} from "./vwindow";

/**
 * M7.3 第三条出口的核心不变量：**窗口不可拖出可视区外**。
 *
 * 这不是「体验问题」——虚拟窗口没有任务栏、没有 Alt+Tab、没有「窗口」菜单，被拖到画布外的
 * 窗口是真的**找不回来**（只能重启程序）。所以下面按「所有写入路径」逐条测：拖动、缩放、
 * 视口变小。任何一条漏掉夹紧，都足以造出一个拿不回来的窗口。
 */
const VP = { width: 1000, height: 700 };

describe("clampRect：唯一的夹紧出口", () => {
  it("视口内的矩形原样返回", () => {
    expect(clampRect({ x: 100, y: 50, w: 600, h: 400 }, VP)).toEqual({ x: 100, y: 50, w: 600, h: 400 });
  });

  it("越过右下边界的被拉回来，恰好贴边", () => {
    const r = clampRect({ x: 9999, y: 9999, w: 600, h: 400 }, VP);
    expect(r).toEqual({ x: 400, y: 300, w: 600, h: 400 });
  });

  it("负坐标被拉回原点（拖到左上角外同样找不回来）", () => {
    expect(clampRect({ x: -300, y: -80, w: 600, h: 400 }, VP)).toMatchObject({ x: 0, y: 0 });
  });

  it("尺寸不得小于最小值", () => {
    expect(clampRect({ x: 0, y: 0, w: 10, h: 10 }, VP)).toMatchObject({ w: MIN_W, h: MIN_H });
  });

  it("比视口还大的窗口先被收进视口，再定位——反过来做会贴了左上角仍然溢出右下", () => {
    const r = clampRect({ x: 50, y: 50, w: 5000, h: 5000 }, VP);
    expect(r).toEqual({ x: 0, y: 0, w: 1000, h: 700 });
  });

  it("视口比最小尺寸还小时保底给最小尺寸并贴到原点（宁可露一角被裁，也不能挪到看不见的地方）", () => {
    const r = clampRect({ x: 40, y: 40, w: 600, h: 400 }, { width: 100, height: 80 });
    expect(r).toEqual({ x: 0, y: 0, w: MIN_W, h: MIN_H });
  });
});

describe("开关与叠放", () => {
  beforeEach(() => resetVWindowsForTest());

  it("开一个窗口，落在视口内且带默认尺寸", () => {
    openVWindow({ kind: "sftp", sessionId: "s1", title: "文件 — a" }, VP);
    const [w] = get(vwindows);
    expect(w).toMatchObject({ kind: "sftp", sessionId: "s1", w: DEFAULT_W, h: DEFAULT_H });
    expect(w.x + w.w).toBeLessThanOrEqual(VP.width);
    expect(w.y + w.h).toBeLessThanOrEqual(VP.height);
  });

  it("同一会话再点一次不新开，而是置顶原来那个", () => {
    const a = openVWindow({ kind: "sftp", sessionId: "s1", title: "文件 — a" }, VP);
    openVWindow({ kind: "sftp", sessionId: "s2", title: "文件 — b" }, VP);
    const again = openVWindow({ kind: "sftp", sessionId: "s1", title: "文件 — a" }, VP);
    expect(again).toBe(a);
    expect(get(vwindows).length).toBe(2);
    const top = [...get(vwindows)].sort((p, q) => q.z - p.z)[0];
    expect(top.id).toBe(a);
  });

  it("不同会话各开各的，且层叠错开（新窗口不会正好盖住旧的）", () => {
    openVWindow({ kind: "sftp", sessionId: "s1", title: "a" }, VP);
    openVWindow({ kind: "sftp", sessionId: "s2", title: "b" }, VP);
    const [a, b] = get(vwindows);
    expect(b.z).toBeGreaterThan(a.z);
    expect([b.x, b.y]).not.toEqual([a.x, a.y]);
  });

  it("置顶：已经在最上面时 z 不变（否则每次点击都让层号无谓地长）", () => {
    openVWindow({ kind: "sftp", sessionId: "s1", title: "a" }, VP);
    const b = openVWindow({ kind: "sftp", sessionId: "s2", title: "b" }, VP);
    const before = get(vwindows).map((w) => w.z);
    raiseVWindow(b);
    expect(get(vwindows).map((w) => w.z)).toEqual(before);
  });

  it("关闭只关那一个", () => {
    const a = openVWindow({ kind: "sftp", sessionId: "s1", title: "a" }, VP);
    openVWindow({ kind: "sftp", sessionId: "s2", title: "b" }, VP);
    closeVWindow(a);
    expect(get(vwindows).map((w) => w.sessionId)).toEqual(["s2"]);
  });

  it("会话结束时收走它的全部窗口——留着就是对着死会话刷错误的面板", () => {
    openVWindow({ kind: "sftp", sessionId: "s1", title: "a" }, VP);
    openVWindow({ kind: "sftp", sessionId: "s2", title: "b" }, VP);
    closeVWindowsForSession("s1");
    expect(get(vwindows).map((w) => w.sessionId)).toEqual(["s2"]);
  });
});

describe("拖动 / 缩放 / 视口变化都不得把窗口丢到画布外", () => {
  beforeEach(() => resetVWindowsForTest());

  const only = () => get(vwindows)[0];
  const inside = (vp = VP) => {
    const w = only();
    expect(w.x).toBeGreaterThanOrEqual(0);
    expect(w.y).toBeGreaterThanOrEqual(0);
    expect(w.x + w.w).toBeLessThanOrEqual(vp.width);
    expect(w.y + w.h).toBeLessThanOrEqual(vp.height);
  };

  it("往右下狂拖：贴边停住", () => {
    const id = openVWindow({ kind: "sftp", sessionId: "s1", title: "a" }, VP);
    moveVWindow(id, 100_000, 100_000, VP);
    inside();
    expect(only().x).toBe(VP.width - only().w);
  });

  it("往左上狂拖：贴边停住", () => {
    const id = openVWindow({ kind: "sftp", sessionId: "s1", title: "a" }, VP);
    moveVWindow(id, -5000, -5000, VP);
    expect(only()).toMatchObject({ x: 0, y: 0 });
  });

  it("拉大到超出画布：尺寸被收住，窗口仍完整可见", () => {
    const id = openVWindow({ kind: "sftp", sessionId: "s1", title: "a" }, VP);
    moveVWindow(id, 600, 400, VP);
    resizeVWindow(id, 5000, 5000, VP);
    inside();
  });

  it("缩到最小以下：停在最小尺寸", () => {
    const id = openVWindow({ kind: "sftp", sessionId: "s1", title: "a" }, VP);
    resizeVWindow(id, 10, 10, VP);
    expect(only()).toMatchObject({ w: MIN_W, h: MIN_H });
  });

  it("画布变小（主窗口缩小 / 拉开监控面板）→ 所有窗口重新夹回来", () => {
    const id = openVWindow({ kind: "sftp", sessionId: "s1", title: "a" }, VP);
    moveVWindow(id, 400, 300, VP);
    const small = { width: 600, height: 400 };
    reflowVWindows(small);
    inside(small);
  });

  it("画布变小再变大：窗口不会自己跑回去，但仍在画布内（不做记忆是刻意的——用户看到的位置就是位置）", () => {
    const id = openVWindow({ kind: "sftp", sessionId: "s1", title: "a" }, VP);
    moveVWindow(id, 400, 300, VP);
    reflowVWindows({ width: 600, height: 400 });
    const after = { ...only() };
    reflowVWindows(VP);
    expect(only()).toMatchObject({ x: after.x, y: after.y });
    inside();
  });

  it("不存在的 id 不抛也不改动别人", () => {
    const id = openVWindow({ kind: "sftp", sessionId: "s1", title: "a" }, VP);
    const before = { ...only() };
    moveVWindow("no-such", 10, 10, VP);
    resizeVWindow("no-such", 10, 10, VP);
    raiseVWindow("no-such");
    closeVWindow("no-such");
    expect(get(vwindows).length).toBe(1);
    expect(only()).toEqual(before);
    expect(id).toBeTruthy();
  });
});

describe("cascadeOrigin", () => {
  it("逐个错开，撞到边就绕回原点重来（否则第 N 个窗口会被夹到贴边、看起来像没开）", () => {
    const size = { w: DEFAULT_W, h: DEFAULT_H };
    const xs = [0, 1, 2, 50].map((n) => cascadeOrigin(n, size, VP).x);
    expect(xs[0]).toBe(0);
    expect(xs[1]).toBeGreaterThan(0);
    expect(xs[2]).toBeGreaterThan(xs[1]);
    for (const x of xs) expect(x).toBeLessThanOrEqual(VP.width - size.w);
  });

  it("画布装不下默认尺寸时也不产生负偏移", () => {
    const o = cascadeOrigin(3, { w: DEFAULT_W, h: DEFAULT_H }, { width: 400, height: 300 });
    expect(o.x).toBeGreaterThanOrEqual(0);
    expect(o.y).toBeGreaterThanOrEqual(0);
  });
});
