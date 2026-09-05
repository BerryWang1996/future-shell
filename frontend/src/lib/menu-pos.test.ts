import { describe, it, expect } from "vitest";
import { SUBMENU_ROOM, clampDelta, submenuSide } from "./menu-pos";

/** 右键菜单边缘避让（2026-08-31 评审 P2-8）的判据。
 *
 * 五处菜单一律 clientX/clientY 直接定位，靠右/靠下打开就出界——
 * 而菜单末尾往往正是「删除/关闭」这类最不能被裁一半的项。 */
describe("clampDelta：菜单平移量的四个方向", () => {
  const FULL = { left: 0, top: 0, right: 0, bottom: 0 };

  it("完全在视口内 → 零平移", () => {
    expect(clampDelta({ left: 100, top: 100, right: 260, bottom: 300 }, 1280, 800)).toEqual({ dx: 0, dy: 0 });
  });

  it("右侧出界 → 左移到边内（不是翻转到鼠标左侧——右键菜单翻转会盖住光标指向的项）", () => {
    const r = { left: 1200, top: 100, right: 1380, bottom: 300 }; // 1280 宽视口，出界 100
    const { dx } = clampDelta(r, 1280, 800);
    expect(dx).toBe(-108); // 1280-8-1380
    expect(r.right + dx).toBeLessThanOrEqual(1272);
  });

  it("底部出界 → 上移（终端右键菜单在窗口最下行打开是常态）", () => {
    const r = { left: 100, top: 700, right: 260, bottom: 900 }; // 800 高视口
    const { dy } = clampDelta(r, 1280, 800);
    expect(r.bottom + dy).toBeLessThanOrEqual(792);
  });

  it("左/上也越界（极端小视口）→ 拉回 margin 内，不产生负坐标", () => {
    const { dx, dy } = clampDelta({ left: -50, top: -30, right: 100, bottom: 200 }, 1280, 800);
    expect(dx).toBe(58); // 8-(-50)
    expect(dy).toBe(38);
  });

  it("右与下同时出界 → 两个方向都修（这条钉住「只修一个方向」的半吊子实现）", () => {
    const r = { left: 1200, top: 780, right: 1400, bottom: 950 };
    const d = clampDelta(r, 1280, 800);
    expect(r.right + d.dx).toBeLessThanOrEqual(1272);
    expect(r.bottom + d.dy).toBeLessThanOrEqual(792);
  });

  it("零尺寸外框不 panic（菜单还没布局完的边界）", () => {
    expect(clampDelta(FULL, 1280, 800)).toBeDefined();
  });
});

/* 2026-09-02 响应式核查：菜单被钳到右缘后，left:100% 的子菜单再向右就出界（右停靠 / 800px 窄窗口）。 */
describe("submenuSide：子菜单的展开方向", () => {
  it("右侧还容得下一个子菜单 → 向右（默认形态不变）", () => {
    expect(submenuSide(400, 1280)).toBe("right");
    expect(submenuSide(1280 - SUBMENU_ROOM, 1280)).toBe("right"); // 恰好容得下
  });

  it("右侧不够一个子菜单宽度 → 翻到左侧", () => {
    expect(submenuSide(1280 - SUBMENU_ROOM + 1, 1280)).toBe("left");
    expect(submenuSide(792, 800)).toBe("left"); // 800 宽窄窗口里贴右缘的菜单
  });

  it("room 可调：宿主子菜单更宽时阈值随之前移", () => {
    expect(submenuSide(1000, 1280, 300)).toBe("left");
    expect(submenuSide(1000, 1280, 200)).toBe("right");
  });
});
