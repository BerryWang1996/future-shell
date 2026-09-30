/**
 * RDP 音频解码与排程（阶段 3）。
 *
 * 钉的是**三件一旦错就是刺耳噪音**的事：小端、有符号、归一化除数；
 * 外加积压丢弃的语义（丢的是排在队尾没播的，不是正在播的）。
 */
import { describe, expect, it } from "vitest";
import { decodePcm16, MAX_BUFFER_SECONDS } from "./rdp-audio";

describe("decodePcm16", () => {
  it("**小端**解码（读成大端得到的是噪音）", () => {
    // 0x0100 小端 = 256；若按大端读会得到 1
    const bytes = new Uint8Array([0x00, 0x01]);
    const [ch] = decodePcm16(bytes, 1);
    expect(ch[0]).toBeCloseTo(256 / 32768, 10);
  });

  it("**有符号**解码（读成无符号会把波形整体抬高半个量程）", () => {
    // 0xFFFF 小端 = -1（i16）；按 u16 读是 65535
    const bytes = new Uint8Array([0xff, 0xff]);
    const [ch] = decodePcm16(bytes, 1);
    expect(ch[0]).toBeCloseTo(-1 / 32768, 10);
    expect(ch[0]).toBeLessThan(0);
  });

  it("归一化除数是 32768：最小值恰好落在 -1.0（不越界）", () => {
    // i16 最小值 -32768 = 0x8000 小端
    const bytes = new Uint8Array([0x00, 0x80]);
    const [ch] = decodePcm16(bytes, 1);
    expect(ch[0]).toBe(-1);
    // 用 32767 当除数会得到 -1.0000305——超出 Float32 音频的 [-1,1] 约定
    expect(ch[0]).toBeGreaterThanOrEqual(-1);
  });

  it("最大值 32767 略小于 1.0（同一除数的另一半）", () => {
    const bytes = new Uint8Array([0xff, 0x7f]);
    const [ch] = decodePcm16(bytes, 1);
    expect(ch[0]).toBeCloseTo(32767 / 32768, 10);
    expect(ch[0]).toBeLessThan(1);
  });

  it("立体声**交错**拆分：L R L R → 两条独立声道", () => {
    // 帧1: L=1, R=2；帧2: L=3, R=4
    const bytes = new Uint8Array([1, 0, 2, 0, 3, 0, 4, 0]);
    const [l, r] = decodePcm16(bytes, 2);
    expect(l.length).toBe(2);
    expect(r.length).toBe(2);
    expect(l[0]).toBeCloseTo(1 / 32768, 10);
    expect(r[0]).toBeCloseTo(2 / 32768, 10);
    expect(l[1]).toBeCloseTo(3 / 32768, 10);
    expect(r[1]).toBeCloseTo(4 / 32768, 10);
  });

  it("残缺的最后一帧被丢掉而不是补零（补零是一次爆音）", () => {
    // 立体声但只有 3 个样本（1.5 帧）
    const bytes = new Uint8Array([1, 0, 2, 0, 3, 0]);
    const [l, r] = decodePcm16(bytes, 2);
    expect(l.length).toBe(1);
    expect(r.length).toBe(1);
  });

  it("空输入 → 空声道（不 panic，也不产生一帧静音）", () => {
    const [ch] = decodePcm16(new Uint8Array([]), 1);
    expect(ch.length).toBe(0);
  });

  it("缓冲上限是个**小于半秒**的值（人耳听得出 200ms 不同步）", () => {
    expect(MAX_BUFFER_SECONDS).toBeGreaterThan(0);
    expect(MAX_BUFFER_SECONDS).toBeLessThan(0.5);
  });
});
