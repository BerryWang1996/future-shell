/**
 * 工具栏自定义（M4b 出口第 17 项）：隐藏某几项、拖拽排序，重启保持。
 *
 * settings 键 `ui.toolbarLayout`。
 *
 * # 为什么把顺序存成一个 id 列表，而不是给每项存一个序号
 *
 * 序号方案在**增删按钮**时会散架：新版本加了一个按钮，它没有序号；删了一个按钮，
 * 序号出现空洞。而工具栏的按钮集**会随版本变**（本次就把 `edit.screenshot` 从禁用态
 * 转成了可用）。
 *
 * id 列表方案下这两件事都有确定答案：不在列表里的按钮排到**末尾**（新按钮出现在最后，
 * 用户能发现它但不会打乱他调好的顺序），列表里已经不存在的 id 直接忽略。
 */

/** 工具栏的一项。`group` 只用来画分隔符——见 `withSeparators`。 */
export interface ToolbarItem {
  id: string;
  icon: string;
  tip: string;
  enabled: boolean;
  /** 原始分组序号。排序之后仍按它画分隔符。 */
  group: number;
  primary?: boolean;
}

/** 持久化的布局。两个字段都可以为空——空 = 全部默认。 */
export interface ToolbarLayout {
  /** 被用户隐藏的 id。 */
  hidden: string[];
  /** 自定义顺序。不在此列的按钮排到末尾。 */
  order: string[];
}

export const TOOLBAR_LAYOUT_KEY = "ui.toolbarLayout";

export const EMPTY_LAYOUT: ToolbarLayout = { hidden: [], order: [] };

/**
 * 从 settings 读来的原始值里解出布局。
 *
 * 脏值一律回落空布局——**不是**回落「全部隐藏」。一个把 hidden 解析错的实现会让
 * 整条工具栏消失，而用户完全不知道发生了什么；回落空布局最坏只是「自定义没生效」。
 */
export function parseToolbarLayout(raw: unknown): ToolbarLayout {
  if (typeof raw !== "object" || raw === null) return EMPTY_LAYOUT;
  const o = raw as Record<string, unknown>;
  const strings = (v: unknown): string[] =>
    Array.isArray(v) ? v.filter((x): x is string => typeof x === "string" && x.length > 0) : [];
  return { hidden: strings(o.hidden), order: strings(o.order) };
}

/**
 * 按布局排列并过滤。
 *
 * 顺序规则：`order` 里出现过的按它排，其余按**原始顺序**接在后面。
 * 「其余接在后面」不是随便定的：新版本加的按钮会落在这里，出现在工具栏末尾——
 * 用户看得见它（不至于以为功能没做），又不会插进他调好的顺序中间。
 */
export function applyToolbarLayout(
  items: readonly ToolbarItem[],
  layout: ToolbarLayout,
): ToolbarItem[] {
  const hidden = new Set(layout.hidden);
  const visible = items.filter((i) => !hidden.has(i.id));
  const rank = new Map(layout.order.map((id, i) => [id, i]));
  // 没排过的项全给同一个 rank，靠 `sort` 的**稳定性**保持它们的原始相对顺序。
  //
  // 这里一度还带了个 `|| a.i - b.i` 的次序键当"保险"。变异验证证明那是**死代码**：
  // 删掉它一条测试都不红——`Array.prototype.sort` 从 ES2019 起规范就要求稳定，
  // 而本仓的目标环境（WebView2 / WKWebView / WebKitGTK）全都远新于那个版本。
  // 一个测不出来的分支不该留着：它看起来像在防某件事，实际什么也没防。
  return visible
    .map((item) => ({ item, rank: rank.get(item.id) ?? Number.MAX_SAFE_INTEGER }))
    .sort((a, b) => a.rank - b.rank)
    .map((x) => x.item);
}

/** 切换某项的隐藏状态。 */
export function toggleToolbarItem(layout: ToolbarLayout, id: string): ToolbarLayout {
  const hidden = layout.hidden.includes(id)
    ? layout.hidden.filter((x) => x !== id)
    : [...layout.hidden, id];
  return { ...layout, hidden };
}

/**
 * 把 `dragId` 移到 `targetId` 之前。
 *
 * 传入 `currentIds`（当前可见且已排序的 id 序列）而不是只传两个 id：
 * `order` 里可能只记着一部分按钮（用户只拖过其中几个），单靠它算不出完整的新顺序。
 * 用当前实际序列重算，结果是一份**完整**的 order——于是下一次排序不再依赖
 * 「其余接在后面」这条规则，顺序变得完全确定。
 */
export function reorderToolbar(
  layout: ToolbarLayout,
  currentIds: readonly string[],
  dragId: string,
  targetId: string,
): ToolbarLayout {
  if (dragId === targetId) return layout;
  const ids = currentIds.filter((x) => x !== dragId);
  const at = ids.indexOf(targetId);
  // 目标不在序列里（刚被隐藏了、或者是个陈旧的 id）：什么都不做。
  // 追加到末尾是更糟的选择——用户拖向一个他看得见的按钮，结果那一项跑到了最后。
  if (at < 0) return layout;
  ids.splice(at, 0, dragId);
  return { ...layout, order: ids };
}

/**
 * 相邻两项之间要不要画分隔符。
 *
 * 判据是**原始分组不同**，而不是「固定每 N 个画一条」。用户排序之后分组会被打散，
 * 那时按固定间隔画出来的线毫无意义——它标记的不是任何边界。
 */
export function needsSeparator(prev: ToolbarItem | undefined, cur: ToolbarItem): boolean {
  return prev !== undefined && prev.group !== cur.group;
}
