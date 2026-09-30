import { beforeEach, describe, expect, it } from "vitest";
import { HISTORY_LIMIT, historyClear, historyNext, historyPrev, historyPush } from "./compose-history";

describe("组合命令栏历史（UI 规格 §2.6：每目标独立 100 条）", () => {
  beforeEach(() => historyClear());

  it("↑↓ 循环最近历史", () => {
    historyPush("current", "ls");
    historyPush("current", "htop");
    expect(historyPrev("current")).toBe("htop");
    expect(historyPrev("current")).toBe("ls");
    expect(historyPrev("current")).toBe("ls"); // 到顶保持
    expect(historyNext("current")).toBe("htop");
    expect(historyNext("current")).toBeNull(); // 越过最新 → 调用方清空
  });

  it("目标间隔离", () => {
    historyPush("current", "a");
    historyPush("all", "b");
    expect(historyPrev("current")).toBe("a");
    expect(historyPrev("all")).toBe("b");
    expect(historyPrev("group")).toBeNull();
  });

  it("连续重复不重复入栈；超 100 条淘汰最旧", () => {
    historyPush("current", "x");
    historyPush("current", "x");
    for (let i = 0; i < HISTORY_LIMIT + 10; i++) historyPush("current", `c${i}`);
    let prev = historyPrev("current");
    let count = prev === null ? 0 : 1;
    while (prev !== null) {
      const n = historyPrev("current");
      if (n === null || n === prev) break;
      count += 1;
      prev = n;
    }
    expect(count).toBe(HISTORY_LIMIT);
  });
});
