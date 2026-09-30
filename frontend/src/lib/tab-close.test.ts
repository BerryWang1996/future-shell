import { describe, it, expect } from "vitest";
import { canCloseOthers, tabsToClose } from "./tab-close";
import APP_SOURCE from "../App.svelte?raw";

const T = (...ids: string[]) => ids.map((id) => ({ id }));

describe("要关哪些标签", () => {
  it("全部：一个不剩", () => {
    expect(tabsToClose(T("a", "b", "c"), { mode: "all" })).toEqual(["a", "b", "c"]);
    expect(tabsToClose(T(), { mode: "all" })).toEqual([]);
  });

  /**
   * 「关闭其他」留下的是**右键点中的那个**。
   *
   * 变异验证抓到过这一点：把它改成「留当前活动的那个」，源码断言型的测试
   * 一条都不会红——而用户会眼睁睁看着自己刚右键点的那个被关掉。
   * 这个区别只在「右键的不是活动标签」时显形，而那恰恰是这个菜单项最常见的用法。
   */
  it("其他：留下指定的那个，其余全关", () => {
    expect(tabsToClose(T("a", "b", "c"), { mode: "others", keepId: "b" })).toEqual(["a", "c"]);
    // 留第一个、留最后一个都对
    expect(tabsToClose(T("a", "b", "c"), { mode: "others", keepId: "a" })).toEqual(["b", "c"]);
    expect(tabsToClose(T("a", "b", "c"), { mode: "others", keepId: "c" })).toEqual(["a", "b"]);
  });

  it("只有一个标签时「关闭其他」关不掉任何东西", () => {
    expect(tabsToClose(T("a"), { mode: "others", keepId: "a" })).toEqual([]);
  });

  it("keepId 不在列表里时关掉全部（标签刚被别处关掉的情形）", () => {
    // 不是崩、也不是什么都不关：用户此刻看到的界面里没有那个标签，
    // 「关闭其他」在他眼里就是「关掉我看到的这些」。
    expect(tabsToClose(T("a", "b"), { mode: "others", keepId: "gone" })).toEqual(["a", "b"]);
  });

  it("不改原数组（调用方拿的是 store 的快照）", () => {
    const all = T("a", "b");
    tabsToClose(all, { mode: "others", keepId: "a" });
    expect(all.map((t) => t.id)).toEqual(["a", "b"]);
  });
});

describe("「关闭其他」什么时候可用", () => {
  it("两个起可用，一个或零个不可用", () => {
    expect(canCloseOthers(T())).toBe(false);
    expect(canCloseOthers(T("a"))).toBe(false);
    expect(canCloseOthers(T("a", "b"))).toBe(true);
    expect(canCloseOthers(T("a", "b", "c"))).toBe(true);
  });
});

/**
 * 接线：App.svelte 必须**用**这两个函数，而不是自己再写一遍 filter。
 *
 * 上面那些用例证明的是函数对；证明不了 App 有没有在用它。而这段逻辑原本就
 * 写在 App 里、被变异验证抓出问题之后才抽出来——抽出来却没接上，是同一个错误
 * 换了个位置。
 */
describe("App.svelte 用的是这两个函数", () => {
  it("两条路径都经 tabsToClose", () => {
    expect(APP_SOURCE).toMatch(/tabsToClose\(\$tabs, \{ mode: "all" \}\)/);
    expect(APP_SOURCE).toMatch(/tabsToClose\(\$tabs, \{ mode: "others", keepId \}\)/);
  });

  it("「关闭其他」传的是 keepId，不是活动标签", () => {
    // 反向断言：这条正是变异验证抓到的那个退化。
    const body = APP_SOURCE.slice(APP_SOURCE.indexOf("function closeOtherTabs("));
    expect(body.slice(0, 400)).not.toMatch(/\$activeTabId/);
  });

  /**
   * `prompt` 必须原样来自 `decideClose`，不许被覆盖。
   *
   * 变异验证抓到的另一条：在 `requestCloseTabs` 里加一行 `const prompt = false;`
   * 就能让「关一批标签」绕过确认，而「decideClose 调用了三次」那条计数守卫
   * 照样是绿的——它数的是调用，不是调用结果有没有被用上。
   */
  it("requestCloseTabs 里的 prompt 没有被重新赋值", () => {
    const start = APP_SOURCE.indexOf("async function requestCloseTabs(");
    expect(start, "找不到 requestCloseTabs——守卫已失效").toBeGreaterThan(0);
    const body = APP_SOURCE.slice(start, start + 1400);
    // 解构那一次是唯一允许的出现；此外不得再有 `prompt =` / `const prompt`
    const destructure = /const \{ prompt, sessions, queued, running \} = decideClose\(/;
    expect(body).toMatch(destructure);
    const rest = body.replace(destructure, "");
    expect(rest, "prompt 被重新赋值 = 确认框可以被绕过").not.toMatch(/\bprompt\s*=[^=]/);
    expect(rest).not.toMatch(/\b(const|let|var)\s+prompt\b/);
  });
});
