/**
 * 终端内传输前端归约的测试（M4a）。
 *
 * 钉的都是「看起来对、实际会误导用户」的那几处：总大小未知时不许伪造百分比、
 * 里程碑事件不许把文件名闪成空白、终态不许把已传字节清零。
 */
import { describe, expect, it } from "vitest";
import { IDLE, formatBytes, progressText, ratioOf, reduce, type ZmodemProgress } from "./zmodem";

const ev = (p: Partial<ZmodemProgress>): ZmodemProgress => ({
  sessionId: "s1",
  phase: "receiving",
  name: "",
  path: null,
  done: 0,
  total: null,
  message: "",
  ...p,
});

describe("zmodem 前端归约", () => {
  it("need-file 只请求选文件，不显示进度条", () => {
    const v = reduce(IDLE, ev({ phase: "need-file" }));
    expect(v.needFile).toBe(true);
    expect(v.visible).toBe(false);
    expect(v.direction).toBe("send");
  });

  it("接收中显示进度与方向", () => {
    const v = reduce(IDLE, ev({ phase: "receiving", name: "a.bin", done: 512, total: 2048 }));
    expect(v.visible).toBe(true);
    expect(v.direction).toBe("receive");
    expect(v.name).toBe("a.bin");
    expect(v.ratio).toBeCloseTo(0.25);
  });

  it("里程碑事件不带文件名时沿用上一次的名字（不许闪成空白）", () => {
    let v = reduce(IDLE, ev({ phase: "receiving", name: "big.tar", done: 0, total: 1000 }));
    v = reduce(v, ev({ phase: "receiving", name: "", done: 500, total: 1000 }));
    expect(v.name).toBe("big.tar");
    expect(v.done).toBe(500);
  });

  it("总大小未知时 ratio 为 null——不伪造假进度条", () => {
    const v = reduce(IDLE, ev({ phase: "receiving", done: 4096, total: null }));
    expect(v.ratio).toBeNull();
    expect(progressText(v)).toBe("4.0 KiB");
    expect(progressText(v)).not.toContain("%");
  });

  it("saved 保留路径与字节数，ratio 置满", () => {
    let v = reduce(IDLE, ev({ phase: "receiving", name: "x.bin", done: 10, total: 100 }));
    v = reduce(v, ev({ phase: "saved", name: "x.bin", path: "C:\\dl\\x.bin", done: 100, total: 100 }));
    expect(v.ratio).toBe(1);
    expect(v.path).toBe("C:\\dl\\x.bin");
  });

  it("终态保留已传字节与路径（清零看起来像传输凭空消失）", () => {
    let v = reduce(IDLE, ev({ phase: "receiving", name: "y.bin", done: 900, total: 900, path: "C:\\dl\\y.bin" }));
    v = reduce(v, ev({ phase: "done", message: "接收完成" }));
    expect(v.ok).toBe(true);
    expect(v.message).toBe("接收完成");
    expect(v.done).toBe(900);
    expect(v.path).toBe("C:\\dl\\y.bin");
  });

  it("失败终态带上原因（静默失败是最难排查的形态）", () => {
    const v = reduce(IDLE, ev({ phase: "failed", message: "对端中止了传输" }));
    expect(v.ok).toBe(false);
    expect(v.message).toBe("对端中止了传输");
  });

  it("未知 phase 不改状态", () => {
    const before = reduce(IDLE, ev({ phase: "receiving", name: "z", done: 5, total: 10 }));
    // 后端将来新增 phase 时的行为：停在上一态，而不是把进度条清空
    const after = reduce(before, ev({ phase: "brand-new" as ZmodemProgress["phase"] }));
    expect(after).toEqual(before);
  });

  it("ratioOf 对非法总量一律 null，且钳到 0..1", () => {
    expect(ratioOf(5, null)).toBeNull();
    expect(ratioOf(5, 0)).toBeNull();
    expect(ratioOf(5, -1)).toBeNull();
    expect(ratioOf(5, Number.NaN)).toBeNull();
    expect(ratioOf(5, Number.POSITIVE_INFINITY)).toBeNull();
    // 对端少报了总量时不得越过 100%
    expect(ratioOf(200, 100)).toBe(1);
    expect(ratioOf(-5, 100)).toBe(0);
  });

  it("formatBytes 整数字节不带小数点", () => {
    expect(formatBytes(0)).toBe("0 B");
    expect(formatBytes(512)).toBe("512 B");
    expect(formatBytes(1024)).toBe("1.0 KiB");
    expect(formatBytes(1536)).toBe("1.5 KiB");
    expect(formatBytes(1024 ** 3)).toBe("1.0 GiB");
    expect(formatBytes(-1)).toBe("—");
    expect(formatBytes(Number.NaN)).toBe("—");
  });
});
