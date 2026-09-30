/**
 * 进程列表过滤/排序的测试（M4a）。
 *
 * 重点钉两件容易做错、错了还看起来对的事：空值不得当 0 排、同值顺序必须稳定
 * （否则 2 秒一轮的列表在抖）。
 */
import { describe, expect, it } from "vitest";
import {
  filterProcesses,
  formatPct,
  formatRss,
  killConfirmText,
  needsLoudConfirm,
  sortProcesses,
  type ProcessInfo,
} from "./processes";

const p = (o: Partial<ProcessInfo> & { pid: number }): ProcessInfo => ({
  ppid: 1,
  user: "root",
  cpu: 0,
  mem: 0,
  rss_kb: 100,
  state: "S",
  command: "cmd",
  ...o,
});

describe("filterProcesses", () => {
  const list = [
    p({ pid: 1, user: "root", command: "/sbin/init" }),
    p({ pid: 1234, user: "alice", command: "python3 train.py" }),
    p({ pid: 99, user: "postgres", command: "postgres: writer process" }),
  ];

  it("空查询返回原表（同一引用即可，不必拷贝）", () => {
    expect(filterProcesses(list, "")).toHaveLength(3);
    expect(filterProcesses(list, "   ")).toHaveLength(3);
  });

  it("命中命令行、用户名、pid 任一", () => {
    expect(filterProcesses(list, "train").map((x) => x.pid)).toEqual([1234]);
    expect(filterProcesses(list, "postgres").map((x) => x.pid)).toEqual([99]);
    expect(filterProcesses(list, "99").map((x) => x.pid)).toEqual([99]);
  });

  it("pid 按包含匹配——「我记得是 12 开头的」这种用法要成立", () => {
    expect(filterProcesses(list, "12").map((x) => x.pid)).toEqual([1234]);
    expect(filterProcesses(list, "23").map((x) => x.pid)).toEqual([1234]);
  });

  it("大小写不敏感", () => {
    expect(filterProcesses(list, "ALICE").map((x) => x.pid)).toEqual([1234]);
    expect(filterProcesses(list, "Python3").map((x) => x.pid)).toEqual([1234]);
  });
});

describe("sortProcesses", () => {
  it("数值列升降序都正确", () => {
    const list = [p({ pid: 3, cpu: 5 }), p({ pid: 1, cpu: 50 }), p({ pid: 2, cpu: 0.5 })];
    expect(sortProcesses(list, "cpu", "asc").map((x) => x.cpu)).toEqual([0.5, 5, 50]);
    expect(sortProcesses(list, "cpu", "desc").map((x) => x.cpu)).toEqual([50, 5, 0.5]);
  });

  it("空值恒排最后，与升降序无关——null 是「不知道」不是「最小」", () => {
    // 空值**不能**摆在输入首位：V8 的小数组插入排序在那种排列下压根不会问
    // 「a 是空值时该往哪边」这个问题，于是把该规则改反了测试也照过（变异验证
    // 实测：null 在首位时正反两种实现输出完全一致）。这里让空值出现在中间与末尾。
    const list = [p({ pid: 2, cpu: 9 }), p({ pid: 1, cpu: null }), p({ pid: 3, cpu: 1 })];
    expect(sortProcesses(list, "cpu", "asc").map((x) => x.pid)).toEqual([3, 2, 1]);
    expect(sortProcesses(list, "cpu", "desc").map((x) => x.pid)).toEqual([2, 3, 1]);
    // 多个空值分散在各处时同样排最后，且它们之间按 pid 稳定
    const many = [
      p({ pid: 2, cpu: 9 }),
      p({ pid: 1, cpu: null }),
      p({ pid: 3, cpu: 1 }),
      p({ pid: 4, cpu: null }),
    ];
    expect(sortProcesses(many, "cpu", "asc").map((x) => x.pid)).toEqual([3, 2, 1, 4]);
    expect(sortProcesses(many, "cpu", "desc").map((x) => x.pid)).toEqual([2, 3, 1, 4]);
  });

  it("同值按 pid 兜底，顺序稳定（否则每轮轮询列表都在抖）", () => {
    const list = [p({ pid: 30, cpu: 1 }), p({ pid: 10, cpu: 1 }), p({ pid: 20, cpu: 1 })];
    const once = sortProcesses(list, "cpu", "desc").map((x) => x.pid);
    expect(once).toEqual([10, 20, 30]);
    // 打乱输入顺序后结果必须一致
    const again = sortProcesses([list[2], list[0], list[1]], "cpu", "desc").map((x) => x.pid);
    expect(again).toEqual(once);
  });

  it("字符串列按本地序比较", () => {
    const list = [p({ pid: 1, user: "zoe" }), p({ pid: 2, user: "alice" }), p({ pid: 3, user: "bob" })];
    expect(sortProcesses(list, "user", "asc").map((x) => x.user)).toEqual(["alice", "bob", "zoe"]);
  });

  it("空字符串也算空值排最后（用户名/命令缺失时）", () => {
    const list = [p({ pid: 1, command: "" }), p({ pid: 2, command: "a" })];
    expect(sortProcesses(list, "command", "asc").map((x) => x.pid)).toEqual([2, 1]);
    expect(sortProcesses(list, "command", "desc").map((x) => x.pid)).toEqual([2, 1]);
  });

  it("不改动入参数组（列表是 $state，原地排会让 Svelte 漏掉更新）", () => {
    // 入参顺序必须与排序结果**不同**，否则原地排也看不出区别——该断言曾经就是
    // 这样空转的（输入恰好已是升序）。
    const list = [p({ pid: 1, cpu: 9 }), p({ pid: 2, cpu: 1 }), p({ pid: 3, cpu: 5 })];
    const before = list.map((x) => x.pid);
    const sorted = sortProcesses(list, "cpu", "asc").map((x) => x.pid);
    expect(sorted).not.toEqual(before); // 自检：这组输入确实会被重排
    expect(list.map((x) => x.pid)).toEqual(before);
  });
});

describe("格式化", () => {
  it("formatRss 分级且空值显示「—」", () => {
    expect(formatRss(0)).toBe("0 KiB");
    expect(formatRss(512)).toBe("512 KiB");
    expect(formatRss(2048)).toBe("2.0 MiB");
    expect(formatRss(3 * 1024 * 1024)).toBe("3.00 GiB");
    expect(formatRss(null)).toBe("—");
    expect(formatRss(-1)).toBe("—");
  });

  it("formatPct 空值「—」而不是 0.0%", () => {
    expect(formatPct(0)).toBe("0.0%");
    expect(formatPct(12.34)).toBe("12.3%");
    expect(formatPct(null)).toBe("—");
  });
});

describe("终止确认", () => {
  it("PID 1 与 KILL 都要重话确认，普通 TERM 不要", () => {
    expect(needsLoudConfirm(1, "TERM")).toBe(true);
    expect(needsLoudConfirm(1234, "KILL")).toBe(true);
    expect(needsLoudConfirm(1234, "kill")).toBe(true);
    expect(needsLoudConfirm(1234, "TERM")).toBe(false);
    expect(needsLoudConfirm(1234, "HUP")).toBe(false);
  });

  it("确认文案写出后果，而不是笼统的「确定要终止吗」", () => {
    const t1 = killConfirmText(p({ pid: 1, command: "/sbin/init" }), "TERM");
    expect(t1).toContain("整台机器或整个容器");
    const t2 = killConfirmText(p({ pid: 500, command: "postgres" }), "KILL");
    expect(t2).toContain("未落盘的数据会丢失");
    expect(t2).toContain("可先试 TERM");
    // 普通情形不加恐吓，但要带上 pid/用户/命令让人能核对杀对了没有
    const t3 = killConfirmText(p({ pid: 500, user: "bob", command: "sleep 100" }), "TERM");
    expect(t3).toContain("500");
    expect(t3).toContain("bob");
    expect(t3).toContain("sleep 100");
    expect(t3).not.toContain("⚠");
  });

  it("命令行为空时确认文案仍可读", () => {
    expect(killConfirmText(p({ pid: 7, command: "" }), "TERM")).toContain("（无命令行）");
  });
});
