/**
 * 回放调度的测试（M4a）。
 */
import { describe, expect, it } from "vitest";
import {
  countUpTo,
  delays,
  durationSecs,
  formatBytes,
  formatDuration,
  shouldWarnPrivacy,
  type CastEvent,
} from "./record";

const ev = (t: number, data = "x"): CastEvent => ({ t, kind: "o", data });

describe("delays", () => {
  it("增量 = 与前一个事件的差；首个 = 它自己的相对时刻（开头静默也忠实回放）", () => {
    const es = [ev(0), ev(1.5), ev(1.5), ev(4)];
    expect(delays(es)).toEqual([0, 1500, 0, 2500]);
    // 首字节迟到 2 秒：回放也该等 2 秒
    expect(delays([ev(2), ev(3)])).toEqual([2000, 1000]);
  });

  it("负增量钳到 0（手改文件/时钟怪相不产生负延迟）", () => {
    const es = [ev(5), ev(2), ev(3)];
    // 首个 = 它自己的 5s；随后 2<5 与 3>2 各自钳/计
    expect(delays(es)).toEqual([5000, 0, 1000]);
  });

  it("空列表与单事件", () => {
    expect(delays([])).toEqual([]);
    expect(delays([ev(7)])).toEqual([7000]); // 单事件等它自己的时刻
  });
});

describe("durationSecs / countUpTo", () => {
  const es = [ev(0), ev(2), ev(5), ev(5.5), ev(9)];

  it("时长 = 最后事件时刻", () => {
    expect(durationSecs(es)).toBe(9);
    expect(durationSecs([])).toBe(0);
  });

  it("拖到某秒：已应用的事件个数", () => {
    // 3 秒处：t<=3 的事件是 0、2 → 2 个
    expect(countUpTo(es, 3)).toBe(2);
    // 5 秒处：0、2、5（t<=5 含 5）→ 3 个
    expect(countUpTo(es, 5)).toBe(3);
    // 超出时长钳到末尾
    expect(countUpTo(es, 999)).toBe(es.length);
    // 0 秒处含 t=0 的首事件（它在起点就发生了）
    expect(countUpTo(es, -5)).toBe(1);
    // 首事件在 1s，0s 处还没发生
    expect(countUpTo([ev(1)], 0)).toBe(0);
  });

  it("拖动一律从 0 数（终端状态累积，回退也是从头快喂）", () => {
    expect(countUpTo(es, 1)).toBe(1);
    // 5.5 > 5.2，不含
    expect(countUpTo(es, 5.2)).toBe(3);
  });
});

describe("格式化", () => {
  it("formatBytes 分级", () => {
    expect(formatBytes(0)).toBe("0 B");
    expect(formatBytes(512)).toBe("512 B");
    expect(formatBytes(4096)).toBe("4.0 KiB");
    expect(formatBytes(5 * 1024 * 1024)).toBe("5.0 MiB");
    expect(formatBytes(Number.NaN)).toBe("—");
  });

  it("formatDuration", () => {
    expect(formatDuration(0)).toBe("0s");
    expect(formatDuration(59)).toBe("59s");
    expect(formatDuration(83)).toBe("1m23s");
    expect(formatDuration(-1)).toBe("—");
  });

  it("隐私提醒只在有内容的文件上出现", () => {
    expect(shouldWarnPrivacy(0)).toBe(false);
    expect(shouldWarnPrivacy(100)).toBe(false);
    expect(shouldWarnPrivacy(2048)).toBe(true);
  });
});
