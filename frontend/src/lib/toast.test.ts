import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";
import { TOAST_EXIT_MS, resetToastsForTest, toast } from "./toast";

/** 某条是否还在列表里（含退场中）。 */
const has = (id: number) => get(toast).some((t) => t.id === id);
const leaving = (id: number) => get(toast).find((t) => t.id === id)?.leaving === true;

describe("toast store", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    resetToastsForTest();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  /* 2026-09-02（路线图 4c 动效补齐）起，dismiss 分两拍：先标 leaving（组件播退场动效），
   * TOAST_EXIT_MS 后再从列表删。下面凡「消失」都按两拍断言。 */

  it("push 单条 toast，store 内追加该 item，自动消失计时器启动", () => {
    const id = toast.push("info", "测试消息");
    const list = get(toast);
    expect(list).toHaveLength(1);
    expect(list[0]).toEqual({ id, level: "info", msg: "测试消息" });

    // 3s 后进入退场（info 级别），再过退场一拍才真正删
    vi.advanceTimersByTime(2999);
    expect(has(id)).toBe(true);
    expect(leaving(id)).toBe(false);
    vi.advanceTimersByTime(1);
    expect(has(id)).toBe(true);
    expect(leaving(id)).toBe(true);
    vi.advanceTimersByTime(TOAST_EXIT_MS - 1);
    expect(has(id)).toBe(true);
    vi.advanceTimersByTime(1);
    expect(has(id)).toBe(false);
  });

  it("不同级别自动消失时长：info 3s / warn 5s / error 8s", () => {
    const id1 = toast.push("info", "info");
    const id2 = toast.push("warn", "warn");
    const id3 = toast.push("error", "error");
    expect(get(toast)).toHaveLength(3);

    vi.advanceTimersByTime(3000 + TOAST_EXIT_MS);
    expect(has(id1)).toBe(false); // info 已消失
    expect(has(id2)).toBe(true); // warn 仍在
    expect(leaving(id2)).toBe(false);
    expect(has(id3)).toBe(true); // error 仍在

    vi.advanceTimersByTime(2000); // 累计 5s + 退场
    expect(has(id2)).toBe(false); // warn 已消失
    expect(has(id3)).toBe(true); // error 仍在

    vi.advanceTimersByTime(3000); // 累计 8s + 退场
    expect(has(id3)).toBe(false); // error 已消失
  });

  it("手动 dismiss：先标 leaving，一拍之后移除", () => {
    const id = toast.push("info", "手动关闭");
    expect(get(toast)).toHaveLength(1);
    toast.dismiss(id);
    expect(has(id)).toBe(true);
    expect(leaving(id)).toBe(true);
    vi.advanceTimersByTime(TOAST_EXIT_MS);
    expect(get(toast)).toHaveLength(0);
  });

  it("退场中再 dismiss（自动消失撞上用户点 ×）：不重复排计时器、不报错、只删一次", () => {
    const id = toast.push("info", "撞车");
    toast.dismiss(id);
    const timersAfterFirst = vi.getTimerCount(); // dwell 那只 + 退场那只
    toast.dismiss(id);
    toast.dismiss(id);
    expect(vi.getTimerCount()).toBe(timersAfterFirst);
    vi.advanceTimersByTime(TOAST_EXIT_MS);
    expect(get(toast)).toHaveLength(0);
    expect(() => vi.advanceTimersByTime(10_000)).not.toThrow(); // dwell 到点对着已删的 id：无操作
    expect(get(toast)).toHaveLength(0);
  });

  it("prefers-reduced-motion：dismiss 立即移除，不等退场那一拍", () => {
    const original = window.matchMedia;
    window.matchMedia = vi.fn(() => ({ matches: true }) as unknown as MediaQueryList);
    try {
      const id = toast.push("info", "减少动态效果");
      toast.dismiss(id);
      expect(get(toast)).toHaveLength(0);
    } finally {
      window.matchMedia = original;
    }
  });

  it("超过 MAX_STACKED（4条）自动挤出最旧：第 5 条推入时移除第 1 条", () => {
    const id1 = toast.push("info", "msg1");
    const id2 = toast.push("info", "msg2");
    const id3 = toast.push("info", "msg3");
    const id4 = toast.push("info", "msg4");
    expect(get(toast)).toHaveLength(4);
    expect(get(toast).map((t) => t.id)).toEqual([id1, id2, id3, id4]);

    const id5 = toast.push("info", "msg5"); // 触发挤出
    const list = get(toast);
    expect(list).toHaveLength(4);
    expect(list.map((t) => t.id)).toEqual([id2, id3, id4, id5]); // id1 被挤出
    expect(list.find((t) => t.id === id1)).toBeUndefined();
  });

  it("重复推送维持最大堆叠：第 6 条推入时保留后 4 条（id3/4/5/6）", () => {
    toast.push("info", "1");
    toast.push("info", "2");
    const id3 = toast.push("info", "3");
    const id4 = toast.push("info", "4");
    const id5 = toast.push("info", "5");
    const id6 = toast.push("info", "6");
    const list = get(toast);
    expect(list).toHaveLength(4);
    expect(list.map((t) => t.id)).toEqual([id3, id4, id5, id6]);
  });

  it("dismiss 不存在的 id：无操作，不报错", () => {
    toast.push("info", "存在");
    expect(get(toast)).toHaveLength(1);
    toast.dismiss(999); // 不存在的 id
    expect(get(toast)).toHaveLength(1); // 列表不变
  });

  it("push 返回递增 id，可用于后续 dismiss", () => {
    const id1 = toast.push("info", "msg1");
    const id2 = toast.push("warn", "msg2");
    const id3 = toast.push("error", "msg3");
    expect(id2).toBe(id1 + 1);
    expect(id3).toBe(id2 + 1);

    toast.dismiss(id2);
    vi.advanceTimersByTime(TOAST_EXIT_MS);
    const list = get(toast);
    expect(list).toHaveLength(2);
    expect(list.map((t) => t.id)).toEqual([id1, id3]);
  });
});
