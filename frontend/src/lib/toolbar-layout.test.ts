import { describe, it, expect } from "vitest";
import {
  EMPTY_LAYOUT,
  applyToolbarLayout,
  needsSeparator,
  parseToolbarLayout,
  reorderToolbar,
  toggleToolbarItem,
  type ToolbarItem,
} from "./toolbar-layout";

const item = (id: string, group = 0): ToolbarItem => ({
  id,
  icon: "x",
  tip: id,
  enabled: true,
  group,
});
const ITEMS = [item("a", 0), item("b", 1), item("c", 1), item("d", 2)];
const ids = (list: ToolbarItem[]) => list.map((x) => x.id);

describe("解析持久化的布局", () => {
  it("空/缺省一律回落空布局", () => {
    for (const raw of [null, undefined, "", 0, [], "not json"]) {
      expect(parseToolbarLayout(raw)).toEqual(EMPTY_LAYOUT);
    }
  });

  /**
   * 脏值回落**空布局**，不是「全部隐藏」。
   *
   * 把 hidden 解析错的实现会让整条工具栏消失，而用户完全不知道发生了什么；
   * 回落空布局最坏只是「自定义没生效」。
   */
  it("字段类型不对时那一项当空，不影响另一项", () => {
    expect(parseToolbarLayout({ hidden: "a", order: ["x"] })).toEqual({ hidden: [], order: ["x"] });
    expect(parseToolbarLayout({ hidden: ["a"], order: 42 })).toEqual({ hidden: ["a"], order: [] });
  });

  it("数组里的非字符串与空串被滤掉", () => {
    expect(parseToolbarLayout({ hidden: ["a", 1, null, "", "b"], order: [] }).hidden).toEqual(["a", "b"]);
  });
});

describe("按布局排列", () => {
  it("空布局 = 原始顺序、全部可见", () => {
    expect(ids(applyToolbarLayout(ITEMS, EMPTY_LAYOUT))).toEqual(["a", "b", "c", "d"]);
  });

  it("隐藏的不出现", () => {
    expect(ids(applyToolbarLayout(ITEMS, { hidden: ["b", "d"], order: [] }))).toEqual(["a", "c"]);
  });

  it("order 里的按它排", () => {
    expect(ids(applyToolbarLayout(ITEMS, { hidden: [], order: ["d", "a"] }))).toEqual([
      "d", "a", "b", "c",
    ]);
  });

  /**
   * **新版本加的按钮排到末尾**，不打乱用户调好的顺序。
   *
   * 这是「顺序存 id 列表而不是给每项存序号」的直接好处：序号方案下新按钮没有序号，
   * 而工具栏的按钮集**会随版本变**（本次就把截图按钮从禁用转成了可用）。
   * 排到末尾让用户看得见它（不至于以为功能没做），又不插进他的顺序中间。
   */
  it("不在 order 里的按钮排到末尾，且保持它们之间的原始顺序", () => {
    const withNew = [...ITEMS, item("新按钮", 2), item("再一个", 2)];
    expect(ids(applyToolbarLayout(withNew, { hidden: [], order: ["d", "c"] }))).toEqual([
      "d", "c", "a", "b", "新按钮", "再一个",
    ]);
  });

  it("order 里已经不存在的 id 直接忽略（旧版本删掉的按钮）", () => {
    expect(ids(applyToolbarLayout(ITEMS, { hidden: [], order: ["没了", "c"] }))).toEqual([
      "c", "a", "b", "d",
    ]);
  });

  it("不改原数组（调用方拿的是模块级常量）", () => {
    const before = ids(ITEMS);
    applyToolbarLayout(ITEMS, { hidden: ["a"], order: ["d"] });
    expect(ids(ITEMS)).toEqual(before);
  });
});

describe("勾选显示/隐藏", () => {
  it("来回切换", () => {
    let l = EMPTY_LAYOUT;
    l = toggleToolbarItem(l, "b");
    expect(l.hidden).toEqual(["b"]);
    l = toggleToolbarItem(l, "b");
    expect(l.hidden).toEqual([]);
  });

  it("不动 order", () => {
    const l = toggleToolbarItem({ hidden: [], order: ["c", "a"] }, "b");
    expect(l.order).toEqual(["c", "a"]);
  });

  it("即时生效：切换之后 applyToolbarLayout 立刻不再返回它", () => {
    // 出口原文：「右键菜单勾选隐藏某项后该项**即时消失**」。
    const l = toggleToolbarItem(EMPTY_LAYOUT, "b");
    expect(ids(applyToolbarLayout(ITEMS, l))).not.toContain("b");
  });
});

describe("拖拽排序", () => {
  it("把拖的那个移到目标之前", () => {
    const l = reorderToolbar(EMPTY_LAYOUT, ["a", "b", "c", "d"], "d", "b");
    expect(l.order).toEqual(["a", "d", "b", "c"]);
  });

  it("往前拖与往后拖都对", () => {
    expect(reorderToolbar(EMPTY_LAYOUT, ["a", "b", "c"], "a", "c").order).toEqual(["b", "a", "c"]);
    expect(reorderToolbar(EMPTY_LAYOUT, ["a", "b", "c"], "c", "a").order).toEqual(["c", "a", "b"]);
  });

  it("拖到自己身上什么都不做", () => {
    const before = { hidden: [], order: ["a", "b"] };
    expect(reorderToolbar(before, ["a", "b"], "a", "a")).toBe(before);
  });

  /**
   * 目标不在当前序列里（刚被隐藏了、或是个陈旧 id）：**什么都不做**。
   *
   * 追加到末尾是更糟的选择——用户拖向一个他看得见的按钮，结果那一项跑到了最后。
   */
  it("目标不在序列里时不动", () => {
    const before = { hidden: [], order: ["a", "b"] };
    expect(reorderToolbar(before, ["a", "b"], "a", "不存在")).toBe(before);
  });

  /**
   * 排序结果是一份**完整**的 order。
   *
   * 只记被拖过的那几个的话，下一次排序还要依赖「其余接在后面」那条规则，
   * 顺序就不完全确定了。
   */
  it("产出完整顺序，不只记被拖的那个", () => {
    const l = reorderToolbar(EMPTY_LAYOUT, ["a", "b", "c", "d"], "d", "a");
    expect(l.order).toEqual(["d", "a", "b", "c"]);
    // 拿它去排，结果与 order 逐字一致（不再有"其余"）
    expect(ids(applyToolbarLayout(ITEMS, l))).toEqual(l.order);
  });

  it("隐藏的项不进 order（当前序列里本来就没有它）", () => {
    const l = reorderToolbar({ hidden: ["b"], order: [] }, ["a", "c", "d"], "d", "a");
    expect(l.order).not.toContain("b");
    expect(l.hidden).toEqual(["b"]);
  });
});

describe("分隔符", () => {
  /**
   * 判据是**原始分组不同**，不是「固定每 N 个画一条」。
   *
   * 用户排序之后分组会被打散，那时按固定间隔画出来的线毫无意义——
   * 它标记的不是任何边界。
   */
  it("相邻两项分组不同才画", () => {
    expect(needsSeparator(item("a", 0), item("b", 1))).toBe(true);
    expect(needsSeparator(item("b", 1), item("c", 1))).toBe(false);
  });

  it("第一项之前不画", () => {
    expect(needsSeparator(undefined, item("a", 0))).toBe(false);
  });

  it("排序打散分组后，分隔符跟着实际相邻关系走", () => {
    // 把 d（组 2）拖到 b（组 1）之前 → a|d|b c，分隔符出现在 a-d 与 d-b 之间
    const l = reorderToolbar(EMPTY_LAYOUT, ["a", "b", "c", "d"], "d", "b");
    const list = applyToolbarLayout(ITEMS, l);
    expect(ids(list)).toEqual(["a", "d", "b", "c"]);
    expect(list.map((x, i) => needsSeparator(list[i - 1], x))).toEqual([false, true, true, false]);
  });
});
