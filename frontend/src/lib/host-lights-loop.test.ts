/**
 * `startHostProbeLoop` 的调度纪律（路线图 4c 架构缺陷 A1「定时器泄漏 / 反复重建」）。
 *
 * 症结：它在 App 的 `$effect` 里被调用，而首轮探测此前在**调用栈上同步**跑——
 * `targets()` 里读的 `profiles` / `sessionStates` 因此被 Svelte 5 记成该 effect 的依赖。
 * 于是任何一个会话状态变化都让 effect 重跑：stop → 重建 setInterval → 再同步探一遍全部主机。
 * 表现为「每开/关一个标签就对所有主机探一轮」，定时器被反复拆建。
 *
 * 修法是把首轮推到微任务里：依赖读取发生在 effect 追踪结束之后，effect 只依赖间隔秒数。
 * 本文件与 host-lights.test.ts 分开：那份测的是纯状态机、不桩 ipc；这份必须桩 `host_probe`。
 */
import { afterEach, describe, expect, it, vi } from "vitest";
import APP_SRC from "../App.svelte?raw";

vi.mock("./ipc", () => ({ invoke: vi.fn(async () => true) }));

import { resetHostLightsForTest, startHostProbeLoop } from "./host-lights";

afterEach(() => {
  vi.useRealTimers();
  resetHostLightsForTest();
});

describe("startHostProbeLoop 的首轮调度", () => {
  it("首轮不在调用栈上同步读 targets——否则 App 的 $effect 会把 profiles/sessionStates 记为依赖", async () => {
    const targets = vi.fn(() => [] as { profileId: string; host: string; port: number }[]);
    const stop = startHostProbeLoop(targets, 30);
    expect(targets, "同步读了 targets：effect 依赖面又回到了 profiles/sessionStates").not.toHaveBeenCalled();
    await Promise.resolve(); // 微任务落定
    expect(targets).toHaveBeenCalledTimes(1);
    stop();
  });

  it("首轮微任务之前就 stop() → 首轮不跑（effect 重跑时旧循环的首轮不得漏出）", async () => {
    const targets = vi.fn(() => []);
    const stop = startHostProbeLoop(targets, 30);
    stop();
    await Promise.resolve();
    await Promise.resolve();
    expect(targets).not.toHaveBeenCalled();
  });

  /* A1 出栈之后的代价：启动时首轮跑在 profiles 还是空数组的时刻，探了个空；没有补探的话，
   * 状态灯要等下一个 30s 刻才亮——新包真机实测 25 秒仍无灯。poke() 就是补这一半。 */
  it("poke()：目标集变了 → 300ms 内补探一轮，定时器不重建，重复 poke 只排一次", async () => {
    vi.useFakeTimers({ toFake: ["setInterval", "clearInterval", "setTimeout", "clearTimeout"] });
    const targets = vi.fn(() => []);
    const loop = startHostProbeLoop(targets, 30);
    await Promise.resolve();
    expect(targets).toHaveBeenCalledTimes(1); // 首轮（空档案）
    loop.poke();
    loop.poke();
    loop.poke();
    vi.advanceTimersByTime(299);
    await Promise.resolve();
    expect(targets).toHaveBeenCalledTimes(1);
    vi.advanceTimersByTime(1);
    await Promise.resolve();
    expect(targets).toHaveBeenCalledTimes(2); // 去抖后恰好一轮
    vi.advanceTimersByTime(30_000 - 300);
    await Promise.resolve();
    expect(targets).toHaveBeenCalledTimes(3); // 30s 刻照常，说明定时器没被重建/重置
    loop();
  });

  it("stop 之后 poke 不再探；stop 前排上的 poke 也被取消", async () => {
    vi.useFakeTimers({ toFake: ["setInterval", "clearInterval", "setTimeout", "clearTimeout"] });
    const targets = vi.fn(() => []);
    const loop = startHostProbeLoop(targets, 30);
    await Promise.resolve();
    loop.poke();
    loop();
    // A1 的主题就是定时器泄漏：stop 之后一个定时器都不许留——包括刚排上的 poke。
    // 只看 targets 次数不够：run() 自带 stopped 守卫，漏清的 poke 定时器到点也探不出东西，
    // 但它仍是一个挂着的定时器（引用着闭包与 targets）。
    expect(vi.getTimerCount(), "stop 后仍有定时器挂着").toBe(0);
    loop.poke();
    vi.advanceTimersByTime(60_000);
    await Promise.resolve();
    expect(targets).toHaveBeenCalledTimes(1);
  });

  it("App 接线：档案列表一变就 poke，且这个 effect 不读 sessionStates（会话开关不得引发全量重探）", () => {
    const m = /\$effect\(\(\) => \{\s*void profiles;\s*hostProbeLoop\?\.poke\(\);\s*\}\);/.exec(APP_SRC);
    expect(m, "App.svelte 里找不到「void profiles; hostProbeLoop?.poke()」的 effect").not.toBeNull();
    expect(m![0]).not.toContain("sessionStates");
  });

  it("间隔到点照常再探一轮；stop 之后不再探", async () => {
    vi.useFakeTimers({ toFake: ["setInterval", "clearInterval"] }); // 微任务保持真实，只假计时器
    const targets = vi.fn(() => []);
    const stop = startHostProbeLoop(targets, 30);
    await Promise.resolve();
    expect(targets).toHaveBeenCalledTimes(1);
    vi.advanceTimersByTime(30_000);
    await Promise.resolve();
    expect(targets).toHaveBeenCalledTimes(2);
    stop();
    vi.advanceTimersByTime(90_000);
    await Promise.resolve();
    expect(targets).toHaveBeenCalledTimes(2);
  });
});
