/**
 * `untilUnmount`（路线图 4c 架构缺陷 A2「事件监听器竞态」）。
 *
 * 症结复述：`listen()` 异步落定，组件清理同步执行。组件在 Promise 落定**之前**卸载时，
 * 各组件手写的 `un = await listen(...)` / `.then(fn => unlisten = fn)` 在清理那一刻手里
 * 还没有解绑函数 → 什么都没解 → 稍后落定，一个指向已卸载组件的处理器被永久登记。
 * 这三条用例钉的就是那个窗口：**卸载先于落定时，落定当刻必须立刻解绑**。
 */
import { describe, expect, it, vi } from "vitest";

const invokeMock = vi.fn(async () => undefined);
vi.mock("@tauri-apps/api/core", () => ({ invoke: (...a: unknown[]) => invokeMock(...(a as [])) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

import { untilUnmount } from "./lifecycle";

/** 手控落定时机的 Promise<UnlistenFn>。 */
function deferredListen() {
  let resolve!: (fn: () => void) => void;
  let reject!: (e: unknown) => void;
  const p = new Promise<() => void>((res, rej) => { resolve = res; reject = rej; });
  const unlisten = vi.fn();
  return { p, unlisten, resolve: () => resolve(unlisten), reject };
}

describe("untilUnmount：卸载与 listen 落定的先后无关，处理器都不会活过组件", () => {
  it("卸载先于落定 → 落定当刻立刻解绑（这是各组件手写版本漏掉的那条路）", async () => {
    const d = deferredListen();
    const dispose = untilUnmount(d.p);
    dispose(); // 组件此刻卸载：解绑函数还没到手
    expect(d.unlisten).not.toHaveBeenCalled(); // 没东西可解，这一步本身没问题
    d.resolve(); // 后端登记完成
    await d.p;
    await Promise.resolve();
    expect(d.unlisten).toHaveBeenCalledTimes(1); // 落定即解，零存活窗口
  });

  it("落定先于卸载 → 卸载时解绑，且重复卸载幂等（只解一次）", async () => {
    const d = deferredListen();
    const dispose = untilUnmount(d.p);
    d.resolve();
    await dispose.ready;
    expect(d.unlisten).not.toHaveBeenCalled(); // 组件还活着，不能提前解
    dispose();
    dispose();
    expect(d.unlisten).toHaveBeenCalledTimes(1);
  });

  it("ready 在登记完成后落定；登记失败不抛给调用方，而是上报后端日志", async () => {
    const d = deferredListen();
    const dispose = untilUnmount(d.p);
    d.reject(new Error("ipc 未就绪"));
    await expect(dispose.ready).resolves.toBeUndefined(); // 不 reject：诊断侧支不改主流程
    await Promise.resolve();
    const reported = invokeMock.mock.calls.find((c) => (c as unknown[])[0] === "log_frontend_error");
    expect(reported, "登记失败必须留痕（log_frontend_error），否则「监听没挂上」无从查起").toBeTruthy();
    expect(() => dispose()).not.toThrow(); // 失败后的卸载同样安全
  });
});
