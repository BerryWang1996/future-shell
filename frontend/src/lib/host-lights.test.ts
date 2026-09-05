/** S311：主机状态灯状态机（M4a 出口标准三用例：停机检测/恢复检测/连续失败降级）。 */
import { describe, expect, it, beforeEach } from "vitest";
import {
  nextLightState,
  applyProbe,
  lightOf,
  clearLight,
  HOST_PROBE_ERROR_LIMIT,
  resetHostLightsForTest,
  type HostLight,
} from "./host-lights";

describe("nextLightState（S311 纯状态机）", () => {
  it("停机检测：可达 → 不可达，一个探测周期内翻红", () => {
    const r = nextLightState("green", 0, { ok: false });
    expect(r).toEqual({ light: "red", consecutiveErrors: 0 });
  });

  it("恢复检测：不可达 → 可达，一个探测周期内翻绿", () => {
    const r = nextLightState("red", 0, { ok: true });
    expect(r).toEqual({ light: "green", consecutiveErrors: 0 });
  });

  it("IPC 抖动不翻灯：单次探测错误保持旧色（探测自身的问题不是主机的问题）", () => {
    const r = nextLightState("green", 0, { error: true });
    expect(r).toEqual({ light: "green", consecutiveErrors: 1 });
  });

  it("连续失败降级：错误计数达上限翻灰停更（红绿灯都不可信时，诚实的是「不知道」）", () => {
    let light: HostLight = "green";
    let errs = 0;
    for (let i = 0; i < HOST_PROBE_ERROR_LIMIT; i++) {
      const r = nextLightState(light, errs, { error: true });
      light = r.light;
      errs = r.consecutiveErrors;
    }
    expect(light).toBe("gray");
    // 灰后错误继续：仍是灰（停更），不翻红
    const r = nextLightState(light, errs, { error: true });
    expect(r.light).toBe("gray");
  });

  it("降级后一次成功探测恢复三态循环（灰 → 绿/红）", () => {
    let r = nextLightState("gray", HOST_PROBE_ERROR_LIMIT, { ok: true });
    expect(r).toEqual({ light: "green", consecutiveErrors: 0 });
    r = nextLightState("gray", HOST_PROBE_ERROR_LIMIT, { ok: false });
    expect(r).toEqual({ light: "red", consecutiveErrors: 0 });
  });

  it("「不可达」是正常结果不是错误：清零错误计数", () => {
    const r = nextLightState("green", HOST_PROBE_ERROR_LIMIT - 1, { ok: false });
    expect(r).toEqual({ light: "red", consecutiveErrors: 0 });
  });
});

describe("applyProbe / lightOf / clearLight（store 侧）", () => {
  beforeEach(() => resetHostLightsForTest());

  it("未探测过 = 灰（灰也是「还没探测」的初始态）", () => {
    expect(lightOf("p1")).toBe("gray");
  });

  it("applyProbe 连续打点：绿 →（停机）红 →（恢复）绿 →（错误×上限）灰", () => {
    applyProbe("p1", { ok: true });
    expect(lightOf("p1")).toBe("green");
    applyProbe("p1", { ok: false });
    expect(lightOf("p1")).toBe("red");
    applyProbe("p1", { ok: true });
    expect(lightOf("p1")).toBe("green");
    for (let i = 0; i < HOST_PROBE_ERROR_LIMIT; i++) applyProbe("p1", { error: true });
    expect(lightOf("p1")).toBe("gray");
  });

  it("clearLight：档案删除后灯清除（残留灯会在同名新建档案上诈尸）", () => {
    applyProbe("p1", { ok: true });
    clearLight("p1");
    expect(lightOf("p1")).toBe("gray");
  });
});
