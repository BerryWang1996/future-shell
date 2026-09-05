/**
 * 主机状态灯轮询（M4a，路线图 §M4 / UI 规格 §1.1）。
 *
 * 对**未连接**的档案按可配置间隔（settings 键 `sidebar.hostProbeSeconds`，默认 30s，
 * 5..=600）做 TCP 探测（后端 `host_probe`），侧栏行显示三态灯：
 *   绿 = 探测可达（或该档案已有活动会话——连接本身是最强的可达证明）
 *   红 = 探测不可达（TCP 拒绝/超时/DNS 失败——机器或 sshd 挂了）
 *   灰 = 未知（未探测过 / 连续探测**错误**后的降级态）
 *
 * 状态机（S311，M4a 出口标准三用例的判据载体）：
 * - 停机检测：可达 → 不可达，一个周期内翻红；
 * - 恢复检测：不可达 → 可达，一个周期内翻绿；
 * - 连续失败降级：探测**错误**（IPC 层面失败，而非「探测结果为不可达」）连续
 *   HOST_PROBE_ERROR_LIMIT 次 → 翻灰并停更（网络栈在撒谎时，红绿灯都不可信；
 *   灰 = 「我不知道」，比一个编造的红诚实）。之后任何一次成功探测恢复三态循环。
 */
import { writable, get } from "svelte/store";
import { invoke } from "./ipc";

/** 灯三态（UI 规格 §1.1；四态权威见 §5 状态表——第四态是连接中的蓝/绿，归
 *  sessionStates 而非本模块：已连接档案根本不需要探测）。 */
export type HostLight = "green" | "red" | "gray";

/** 探测**错误**的降级阈值：连错这么多次就翻灰停更。 */
export const HOST_PROBE_ERROR_LIMIT = 3;

export const HOST_PROBE_INTERVAL_DEFAULT_SECONDS = 30;
export const HOST_PROBE_INTERVAL_MIN_SECONDS = 5;
export const HOST_PROBE_INTERVAL_MAX_SECONDS = 600;

/** profileId → 灯态（含内部计数；对外读只看 light 字段）。 */
interface HostLightEntry {
  light: HostLight;
  /** 连续 IPC 错误计数（成功探测清零；「不可达」结果也清零——它是正常返回）。 */
  consecutiveErrors: number;
}

export const hostLights = writable<Map<string, HostLight>>(new Map());
const entries = new Map<string, HostLightEntry>();

function publish(profileId: string, e: HostLightEntry) {
  entries.set(profileId, e);
  hostLights.update((m) => new Map(m).set(profileId, e.light));
}

export function lightOf(profileId: string): HostLight {
  return entries.get(profileId)?.light ?? "gray";
}

/** 探测结果 → 灯态迁移（S311 纯函数——状态机必须是纯的，UI 才测得起）。 */
export function nextLightState(
  prev: HostLight,
  consecutiveErrors: number,
  probe: { ok: boolean } | { error: true },
): { light: HostLight; consecutiveErrors: number } {
  if ("error" in probe) {
    const n = consecutiveErrors + 1;
    if (n >= HOST_PROBE_ERROR_LIMIT) {
      return { light: "gray", consecutiveErrors: n };
    }
    // 未达阈值：保持旧灯色——一次 IPC 抖动不该把绿灯翻红（那是探测自己的问题，
    // 不是主机的问题）；计数值继续累积供阈值判定。
    return { light: prev, consecutiveErrors: n };
  }
  return { light: probe.ok ? "green" : "red", consecutiveErrors: 0 };
}

/** 供轮询器与测试直接注入一次探测结果。 */
export function applyProbe(profileId: string, probe: { ok: boolean } | { error: true }): void {
  const prev = entries.get(profileId) ?? { light: "gray" as HostLight, consecutiveErrors: 0 };
  publish(profileId, nextLightState(prev.light, prev.consecutiveErrors, probe));
}

/** 档案被删除/列表刷新时清灯（残留灯会在同名新建档案上诈尸）。 */
export function clearLight(profileId: string): void {
  entries.delete(profileId);
  hostLights.update((m) => {
    const next = new Map(m);
    next.delete(profileId);
    return next;
  });
}

/** 测试隔离：清空全部灯态（生产代码不得调用）。 */
export function resetHostLightsForTest(): void {
  entries.clear();
  hostLights.set(new Map());
}

// ── 轮询器 ────────────────────────────────────────────────────────────────────

/**
 * 启动轮询：对 `targets`（未连接档案的 host/port，由调用方每轮重新供给——连接状态
 * 变化时目标集随之变化）逐台探测。
 *
 * 失败静默降级（M4a 出口标准）：探测错误只落 console.warn（后端日志留给
 * log_frontend_error 的既有通道），UI 不弹错误框——状态灯轮询坏了不该打断任何
 * 正在进行的工作。
 *
 * 返回停止函数，函数上挂一个 `poke()`：目标集变了（档案首次载入 / 新建 / 删除）时调它，
 * 300ms 内补探一轮而**不**重建定时器——A1 修法把首轮推出了 effect 追踪后，启动时首轮跑在
 * profiles 还是空数组的时刻，探了个空，没有 poke 的话状态灯要等下一个 30s 刻才亮（真机实测
 * 25 秒仍无灯）。重复调用前先停上一轮（调用方责任，见 App 的 effect 清理）。
 */
export interface HostProbeLoop {
  (): void;
  /** 目标集变了：尽快（300ms 去抖）补探一轮；在途或已停止则忽略。 */
  poke(): void;
}

export function startHostProbeLoop(
  targets: () => Array<{ profileId: string; host: string; port: number }>,
  intervalSeconds: number,
): HostProbeLoop {
  const interval = Math.max(
    HOST_PROBE_INTERVAL_MIN_SECONDS,
    Math.min(HOST_PROBE_INTERVAL_MAX_SECONDS, Math.floor(intervalSeconds)),
  );
  let stopped = false;
  let running = false;
  const run = async () => {
    if (running || stopped) return;
    running = true;
    try {
      // 逐台串行：并行的几十个 TCP 连接对 VPN/堡垒机环境是自造的流量尖峰；
      // 单台超时 1.5s（后端钳），串行一轮的时长上界 = N × 1.5s。30s 默认间隔下
      // 一轮没跑完下一轮就跳过（running 守卫）——宁可慢一拍，不堆并发。
      for (const t of targets()) {
        if (stopped) return;
        try {
          const ok = await invoke<boolean>("host_probe", { host: t.host, port: t.port });
          applyProbe(t.profileId, { ok });
        } catch (e) {
          console.warn(`主机探测失败（${t.host}）：`, e);
          applyProbe(t.profileId, { error: true });
        }
      }
    } finally {
      running = false;
    }
  };
  // 首轮**不在调用栈上同步跑**（路线图 4c 架构缺陷 A1）：本函数在 App 的 $effect 里被调，
  // Svelte 5 会把同步读到的一切——`targets()` 里的 profiles / sessionStates——记为该 effect
  // 的依赖，于是任何一台会话状态变化都触发 effect 重跑：stop → 重建 setInterval → 再同步探一轮
  // 全部主机。表现为「每开/关一个标签就对所有主机探一遍」，定时器被反复拆建。
  // 推到微任务里，依赖读取发生在 effect 追踪结束之后，effect 只依赖间隔秒数这一个值。
  queueMicrotask(() => {
    if (!stopped) void run();
  });
  const timer = setInterval(() => void run(), interval * 1000);
  let pokeTimer: ReturnType<typeof setTimeout> | null = null;
  const stop = (() => {
    stopped = true;
    clearInterval(timer);
    if (pokeTimer) {
      clearTimeout(pokeTimer);
      pokeTimer = null;
    }
  }) as HostProbeLoop;
  stop.poke = () => {
    if (stopped || pokeTimer) return; // 已停 / 已排上：不重复排
    pokeTimer = setTimeout(() => {
      pokeTimer = null;
      void run();
    }, 300);
  };
  return stop;
}
