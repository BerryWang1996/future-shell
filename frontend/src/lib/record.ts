/**
 * 会话录屏与回放的前端类型与回放调度（M4a）。纯逻辑可离线钉。
 */

/** 后端 `recording_read_events` 的返回。 */
export interface CastContent {
  header: { version: number; width: number; height: number; timestamp: number };
  events: CastEvent[];
}

export interface CastEvent {
  t: number;
  kind: "o" | "r" | string;
  data: string;
}

/** 后端 `recordings_list` 的一行。 */
export interface CastFile {
  name: string;
  path: string;
  bytes: number;
  modified: number;
}

/**
 * 计算回放计划：每个事件的**延迟增量**（毫秒）。
 *
 * - 首个事件的延迟 = 它自己的相对时刻（录制开头若静默了 2 秒才有首字节，回放也
 *   该静默 2 秒——忠实回放，而不是「立刻全倒出来」）；
 * - 其后 = 与前一个事件的差。
 *
 * 回放器逐事件 `setTimeout(delay/speed)`；给增量而不是绝对时刻，变速与暂停恢复
 * 才简单。负增量（时钟怪相或手改文件）钳到 0。
 */
export function delays(events: CastEvent[]): number[] {
  const out: number[] = [];
  let prev = 0;
  for (const e of events) {
    const d = Math.max(0, (e.t - prev) * 1000);
    out.push(d);
    prev = e.t;
  }
  return out;
}

/** 录屏总时长（秒）= 最后一个事件的相对时刻。 */
export function durationSecs(events: CastEvent[]): number {
  return events.length ? events[events.length - 1].t : 0;
}

/**
 * 拖到 `secs` 处时，应已应用到终端的事件个数（不含延迟，直接喂）。
 *
 * 拖动方向不分前后：调用方一律从头重放到该下标（终端状态是累积的，「倒放」
 * 不存在，快进/回退都是「从 0 快速喂到目标」）——所以本函数不需要 from 参数。
 * `secs` 越界钳到 [0, 时长]。
 */
export function countUpTo(events: CastEvent[], secs: number): number {
  const clamped = Math.max(0, Math.min(secs, durationSecs(events)));
  let i = 0;
  while (i < events.length && events[i].t <= clamped) i++;
  return i;
}

/** 文件大小的人话（与 transfers 的口径一致，这里独立一份避免跨模块拉依赖）。 */
export function formatBytes(n: number): string {
  if (!Number.isFinite(n) || n < 0) return "—";
  if (n < 1024) return `${n} B`;
  const kb = n / 1024;
  if (kb < 1024) return `${kb.toFixed(1)} KiB`;
  const mb = kb / 1024;
  if (mb < 1024) return `${mb.toFixed(1)} MiB`;
  return `${(mb / 1024).toFixed(2)} GiB`;
}

/** 秒数 → `1m23s`（时长展示）。 */
export function formatDuration(secs: number): string {
  if (!Number.isFinite(secs) || secs < 0) return "—";
  const s = Math.round(secs);
  const m = Math.floor(s / 60);
  if (m <= 0) return `${s}s`;
  return `${m}m${String(s % 60).padStart(2, "0")}s`;
}

/**
 * 该不该在打开回放前提醒「含明文」。
 * 只在文件 ≥ 1 KiB 时提醒一次——空录屏没有可提醒的内容，每次弹窗只会磨掉注意力。
 */
export function shouldWarnPrivacy(bytes: number): boolean {
  return bytes >= 1024;
}
