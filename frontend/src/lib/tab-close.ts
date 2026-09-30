/**
 * 「关闭全部标签 / 关闭其他标签」要关哪些（M4b 出口第 6、7 项）。
 *
 * 抽成纯函数不是为了好看：这段逻辑里有一个**真实的设计选择点**——
 * 「关闭其他」留下的是哪一个。变异验证抓到过它：把「留右键点中的那个」改成
 * 「留当前活动的那个」，源码断言型的测试一条都不会红，而用户会眼睁睁看着
 * 自己刚右键点的那个标签被关掉。
 */

/** 只取本模块用得到的字段——不引 tabs.ts 的完整 Tab 类型，免得纯逻辑跟着它演化。 */
export interface ClosableTab {
  id: string;
}

export type CloseScope =
  /** 全部标签 */
  | { mode: "all" }
  /** 除了 `keepId` 之外的全部 */
  | { mode: "others"; keepId: string };

/**
 * 算出要关的标签 id。
 *
 * # 「关闭其他」留下的是右键点中的那个
 *
 * 不是当前活动的那个。用户在一个**非活动**标签上右键选「关闭其他」时，
 * 他要留的显然是手指底下这一个——按活动标签留的话，他刚点的那个会被关掉，
 * 而留下的是另一个他没在看的。
 *
 * 这个区别只在「右键的不是活动标签」时显形，而那恰恰是这个菜单项最常见的用法：
 * 活动标签就在眼前，要收拾的是旁边那一堆。
 */
export function tabsToClose(all: readonly ClosableTab[], scope: CloseScope): string[] {
  if (scope.mode === "all") return all.map((t) => t.id);
  return all.filter((t) => t.id !== scope.keepId).map((t) => t.id);
}

/**
 * 「关闭其他」这一项该不该可用。
 *
 * 只有一个标签时点了什么也不会发生，而一个点了没反应的菜单项看起来像 bug。
 * 判据是**总数**而不是「有没有别的标签」：两者在 `keepId` 不存在于列表里时会分叉
 * （比如标签刚被别处关掉），那时总数说「可用」而后者说「不可用」——
 * 前者更对：点下去会关掉全部，那正是用户此刻看到的界面所暗示的。
 */
export function canCloseOthers(all: readonly ClosableTab[]): boolean {
  return all.length >= 2;
}
