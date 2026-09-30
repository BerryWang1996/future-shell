import { beforeEach, describe, expect, it, vi } from "vitest";
/** jsdom 里没有 `window.__TAURI_INTERNALS__`，真的 `Channel` 构造不出来。
 * 桩只保留本模块用到的面：构造 + onmessage 赋值——分发逻辑全在我们自己的代码里。
 * 类定义进 factory（vi.mock 被提升到文件顶部，外面的类还没初始化）。 */
vi.mock("@tauri-apps/api/core", () => {
  return {
    Channel: class {
      onmessage: ((b: never) => void) | null = null;
    },
  };
});

import {
  claimFramePainter,
  createFrameChannel,
  decodeFrame,
  dropFrameChannel,
  hasFrameChannel,
  rebindFrameChannel,
  releaseFramePainter,
  resetFrameSinksForTest,
} from "./rdp-frames";
import OPEN_SRC from "../lib/open-session.ts?raw";

/**
 * RDP 帧的 raw 直送通道（4d）。
 *
 * 两件事必须在这里钉死：
 * ① **线上格式与 Rust 对偶**——`decodeFrame` 与 `rdp.rs` 的 `frame_wire` 各自的单测
 *    钉住同一组字节，任何一端改了字节序/字段序/头长都当场红；
 * ② **生命周期**——通道在 `rdp_connect` 之前创建、画布晚到认领，中间的帧不丢
 *    （缓存最后一帧），画布卸载只解绑不拆通道（切标签回来还用它）。
 */

/** 按 Rust `frame_wire` 的格式造一条载荷。 */
function wire(x: number, y: number, w: number, h: number, rgba: Uint8Array): ArrayBuffer {
  const buf = new ArrayBuffer(16 + rgba.byteLength);
  const dv = new DataView(buf);
  dv.setUint32(0, x, true);
  dv.setUint32(4, y, true);
  dv.setUint32(8, w, true);
  dv.setUint32(12, h, true);
  new Uint8Array(buf, 16).set(rgba);
  return buf;
}

describe("decodeFrame：与 Rust frame_wire 对偶", () => {
  it("16 字节小端头 + RGBA", () => {
    const rgba = new Uint8Array(2 * 3 * 4);
    for (let i = 0; i < rgba.length; i++) rgba[i] = i;
    const f = decodeFrame(wire(11, 22, 2, 3, rgba));
    expect(f).toMatchObject({ x: 11, y: 22, w: 2, h: 3 });
    // rgba 是载荷的**视图**而不是拷贝——零拷贝是这条路径存在的理由之一
    expect(Array.from(f.rgba)).toEqual(Array.from(rgba));
  });

  it("小端是契约：同一个数在大端下解出来不是它", () => {
    const buf = new ArrayBuffer(16 + 4);
    const dv = new DataView(buf);
    dv.setUint32(0, 0x11223344, false); // 大端写的 0x11223344 → 字节 11 22 33 44
    dv.setUint32(8, 1, true);
    dv.setUint32(12, 1, true);
    new Uint8Array(buf, 16).set([1, 2, 3, 4]);
    const f = decodeFrame(buf);
    // 按小端读同一串字节必须是 0x44332211——选对称数（如 0x01000000）这条就测不出任何东西
    expect(f.x).toBe(0x44332211);
  });

  it("载荷过短 / 尺寸为零 / 像素不足都要报错而不是解出垃圾", () => {
    expect(() => decodeFrame(new ArrayBuffer(8))).toThrow(/过短/);
    const zero = new ArrayBuffer(16 + 4);
    new DataView(zero).setUint32(8, 0, true);
    expect(() => decodeFrame(zero)).toThrow(/为零/);
    const short = wire(0, 0, 4, 4, new Uint8Array(8)); // 头要 64 字节
    expect(() => decodeFrame(short)).toThrow(/像素不足/);
  });
});

describe("通道生命周期", () => {
  beforeEach(() => resetFrameSinksForTest());


  it("创建即注册；认领前到达的帧被缓存，认领时交还", () => {
    const ch = createFrameChannel("s1");
    expect(ch).toBeTruthy();
    expect(hasFrameChannel("s1")).toBe(true);
    // 画布未到——帧进缓存
    (ch as unknown as { onmessage: (b: ArrayBuffer) => void }).onmessage(wire(0, 0, 1, 1, new Uint8Array(4)));
    const got: ArrayBuffer[] = [];
    const missed = claimFramePainter("s1", (b) => got.push(b));
    expect(missed).not.toBeNull();
    expect(decodeFrame(missed!).w).toBe(1);
    expect(got).toEqual([]);
  });

  it("认领后的帧直达画布；卸载只解绑，通道仍在", () => {
    const ch = createFrameChannel("s1");
    const got: ArrayBuffer[] = [];
    claimFramePainter("s1", (b) => got.push(b));
    const feed = (ch as unknown as { onmessage: (b: ArrayBuffer) => void }).onmessage;
    feed(wire(0, 0, 1, 1, new Uint8Array(4)));
    expect(got.length).toBe(1);
    releaseFramePainter("s1");
    feed(wire(0, 0, 1, 1, new Uint8Array(4)));
    // 解绑后不再投递
    expect(got.length).toBe(1);
    // 通道还活着（切标签回来还用）
    expect(hasFrameChannel("s1")).toBe(true);
    // 再认领：把解绑期间缓存的最后一帧补上
    const missed = claimFramePainter("s1", () => {});
    expect(missed).not.toBeNull();
  });

  it("非 ArrayBuffer 的消息被丢弃而不是喂给画布（通道类型擦除的防御）", () => {
    const ch = createFrameChannel("s1");
    const got: ArrayBuffer[] = [];
    claimFramePainter("s1", (b) => got.push(b));
    (ch as unknown as { onmessage: (b: unknown) => void }).onmessage({ some: "json" });
    expect(got).toEqual([]);
  });

  it("rebind：占位 id → 真 session_id，通道对象不变", () => {
    createFrameChannel("pending:1");
    rebindFrameChannel("pending:1", "s-real");
    expect(hasFrameChannel("pending:1")).toBe(false);
    expect(hasFrameChannel("s-real")).toBe(true);
  });

  it("drop：通道与缓存帧一并消失", () => {
    const ch = createFrameChannel("s1");
    (ch as unknown as { onmessage: (b: ArrayBuffer) => void }).onmessage(wire(0, 0, 1, 1, new Uint8Array(4)));
    dropFrameChannel("s1");
    expect(hasFrameChannel("s1")).toBe(false);
    expect(claimFramePainter("s1", () => {})).toBeNull();
  });
});

describe("open-session 的接线（源码级）", () => {
  it("rdp_connect 之前建通道、成功后 rebind、失败即 drop", () => {
    expect(OPEN_SRC).toMatch(/createFrameChannel\(pendingId\);[\s\S]{0,520}?"rdp_connect"/);
    expect(OPEN_SRC).toMatch(/r\.ok && r\.session_id && frameChannel\) rebindFrameChannel\(pendingId, r\.session_id\)/);
    expect(OPEN_SRC).toMatch(/else if \(frameChannel\) dropFrameChannel\(pendingId\)/);
    expect(OPEN_SRC).toMatch(/catch \(e\) \{\s*if \(frameChannel\) dropFrameChannel\(pendingId\)/);
  });
});

/** A/B 开关（4d 出口判据）：rdp.frameTransport=event 时不建通道，后端回落事件路径。 */
describe("frameTransport 开关（源码级）", () => {
  it("读设置键；event 档不建通道、参数里也不带 frameChannel", () => {
    expect(OPEN_SRC).toMatch(/settingGet<string>\("rdp\.frameTransport", "raw"\)/);
    expect(OPEN_SRC).toMatch(/transport === "event" \? null : createFrameChannel\(pendingId\)/);
    expect(OPEN_SRC).toMatch(/frameChannel \? \{ profileId: profile\.id, frameChannel \} : \{ profileId: profile\.id \}/);
  });
});
