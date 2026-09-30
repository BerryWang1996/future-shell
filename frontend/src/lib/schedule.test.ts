/**
 * 计划任务前端辅助的测试（M4a）。
 *
 * 头号目标是那个符号陷阱：`getTimezoneOffset()` 东八区返回 **-480**，而后端约定
 * 东为正。两边符号不统一的后果是所有任务差两个时区。
 */
import { describe, expect, it } from "vitest";
import {
  catchupLabel,
  formatAtOffset,
  formatLastFire,
  formatOffset,
  localTzOffsetMinutes,
  outcomeKind,
  outcomeLabel,
  scheduleSummary,
  type ScheduledTask,
} from "./schedule";

describe("localTzOffsetMinutes", () => {
  it("把 getTimezoneOffset 的符号翻过来（东为正）", () => {
    // 东八区：getTimezoneOffset() = -480 → 我们要 +480
    const cst = { getTimezoneOffset: () => -480 } as Date;
    expect(localTzOffsetMinutes(cst)).toBe(480);
    // 西五区：getTimezoneOffset() = 300 → 我们要 -300
    const est = { getTimezoneOffset: () => 300 } as Date;
    expect(localTzOffsetMinutes(est)).toBe(-300);
    // UTC
    expect(localTzOffsetMinutes({ getTimezoneOffset: () => 0 } as Date)).toBe(0);
    // 半小时时区（印度 +5:30 → getTimezoneOffset() = -330）
    expect(localTzOffsetMinutes({ getTimezoneOffset: () => -330 } as Date)).toBe(330);
  });

  it("真实 Date 上取到的值与 getTimezoneOffset 恰好互为相反数", () => {
    const now = new Date();
    // 期望值写成 `0 - x` 而不是 `-x`：UTC 下 `-0` 是 -0，而被测函数刻意归一成 +0，
    // `toBe` 用 Object.is 比较会把「函数做对了」判成红。1.0.0 候选首次 GitHub CI
    //（runner 默认 TZ=UTC）正是红在这一行；东八区开发机上永远是绿的。
    expect(localTzOffsetMinutes(now)).toBe(0 - now.getTimezoneOffset());
  });

  it("本机就在 UTC 时返回 +0 而不是 -0（CI runner 的默认时区）", () => {
    // 运行期改 TZ：Node 13+ 会在下一次 Date 调用时重读。这条让 UTC 路径在任何开发机上
    // 都跑得到，不必等到 CI 才发现——上面那条只在 runner 上才会走到 0 这个分支。
    const saved = process.env.TZ;
    process.env.TZ = "UTC";
    try {
      const v = localTzOffsetMinutes(new Date());
      expect(Object.is(v, 0), `实得 ${Object.is(v, -0) ? "-0" : v}`).toBe(true);
    } finally {
      if (saved === undefined) delete process.env.TZ;
      else process.env.TZ = saved;
    }
  });
});

describe("formatOffset", () => {
  it("整点与半点时区都对，符号跟着值走", () => {
    expect(formatOffset(480)).toBe("UTC+08:00");
    expect(formatOffset(0)).toBe("UTC+00:00");
    expect(formatOffset(-300)).toBe("UTC-05:00");
    expect(formatOffset(330)).toBe("UTC+05:30");
    expect(formatOffset(345)).toBe("UTC+05:45");
    expect(formatOffset(-210)).toBe("UTC-03:30");
    expect(formatOffset(840)).toBe("UTC+14:00");
  });

  it("非法值退到 UTC 而不是印出 NaN", () => {
    expect(formatOffset(Number.NaN)).toBe("UTC");
    expect(formatOffset(Number.POSITIVE_INFINITY)).toBe("UTC");
  });
});

describe("formatAtOffset", () => {
  // 2026-08-21 09:03:00 UTC（与 Rust 侧 civil.rs 的样例同源）
  const T = 1_787_302_980;

  it("按任务自己的偏移显示，而不是浏览器时区", () => {
    expect(formatAtOffset(T, 0)).toBe("08-21 09:03");
    expect(formatAtOffset(T, 8 * 60)).toBe("08-21 17:03");
    expect(formatAtOffset(T, -5 * 60)).toBe("08-21 04:03");
  });

  it("跨日与跨月都对", () => {
    // 2026-08-21 16:30 UTC + 8h → 08-22 00:30
    expect(formatAtOffset(1_787_329_800, 8 * 60)).toBe("08-22 00:30");
    // 2026-08-31 23:00 UTC + 2h → 09-01 01:00
    const aug31_2300 = 1_787_270_400 + 10 * 86_400 + 23 * 3600;
    expect(formatAtOffset(aug31_2300, 2 * 60)).toBe("09-01 01:00");
  });

  it("半小时偏移不被整点假设吃掉", () => {
    expect(formatAtOffset(T, 5 * 60 + 30)).toBe("08-21 14:33");
  });

  it("空/非法时间显示「—」而不是 1970", () => {
    expect(formatAtOffset(0, 480)).toBe("—");
    expect(formatAtOffset(-1, 480)).toBe("—");
    expect(formatAtOffset(Number.NaN, 480)).toBe("—");
  });
});

describe("formatLastFire", () => {
  it("从未触发过显示「—」", () => {
    expect(formatLastFire(null, 480)).toBe("—");
  });

  it("分钟序号被换算成时间（不是当成秒）", () => {
    // 1_787_302_980 秒 = 29_788_383 分
    expect(formatLastFire(29_788_383, 0)).toBe("08-21 09:03");
    // 若实现把分钟当秒用，会得到 1970 年 → 显示「—」或别的日期
    expect(formatLastFire(29_788_383, 0)).not.toBe("—");
  });
});

describe("执行结果三态", () => {
  it("三种结果各有各的说法（合成一个会抹掉线索）", () => {
    expect(outcomeLabel("ok")).toBe("成功");
    expect(outcomeLabel("failed")).toBe("失败");
    expect(outcomeLabel("skipped")).toBe("未执行");
    // 三者互不相同
    const labels = ["ok", "failed", "skipped"].map(outcomeLabel);
    expect(new Set(labels).size).toBe(3);
    // 未知值原样透出，不吞
    expect(outcomeLabel("weird")).toBe("weird");
  });

  it("skipped 是 warn 而不是 error——它是声明过的边界，不是故障", () => {
    expect(outcomeKind("ok")).toBe("ok");
    expect(outcomeKind("skipped")).toBe("warn");
    expect(outcomeKind("failed")).toBe("error");
    expect(outcomeKind("其它")).toBe("error");
  });
});

describe("摘要与策略文案", () => {
  const t = (o: Partial<ScheduledTask>): ScheduledTask => ({
    id: 1,
    name: "备份",
    cron: "30 3 * * *",
    command: "/opt/b.sh",
    profile_id: "p1",
    enabled: true,
    catchup: "skip",
    tz_offset_minutes: 480,
    tz_follows_dst: false,
    last_fire_minute: null,
    created_at: 0,
    ...o,
  });

  it("摘要如实给出 cron 原文与偏移（不做可能说错的人话化）", () => {
    expect(scheduleSummary(t({}))).toBe("30 3 * * *（UTC+08:00）");
    expect(scheduleSummary(t({ cron: "*/7 3-5 * * 1,3", tz_offset_minutes: -300 }))).toBe(
      "*/7 3-5 * * 1,3（UTC-05:00）",
    );
  });

  it("补跑策略两种文案不同", () => {
    expect(catchupLabel("skip")).toBe("错过则跳过");
    expect(catchupLabel("once")).toBe("错过后补跑一次");
    expect(catchupLabel("skip")).not.toBe(catchupLabel("once"));
    // 未知值按保守侧（跳过）呈现，与后端 CatchUp::parse 的回落一致
    expect(catchupLabel("unknown")).toBe("错过则跳过");
  });
});
