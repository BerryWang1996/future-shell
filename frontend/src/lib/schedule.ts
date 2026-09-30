/**
 * 计划任务的前端类型与显示辅助（M4a）。纯函数，可离线钉。
 */

import { writable } from "svelte/store";

export type CatchUp = "skip" | "once";

/** 后端 `schedule:run` 的载荷（键集恒定，见 app/src/scheduler.rs 的 emit_run）。 */
export interface ScheduleRunEvent {
  taskId: number;
  name: string;
  firedAt: number;
  outcome: string;
  catchup: boolean;
  exitCode: number | null;
  detail: string;
}

/**
 * 「有任务执行过」的计数器。面板订阅它来刷新列表与记录。
 *
 * 用一个自增计数而不是把事件本身塞进 store：面板要重取的是**全量**（上次触发时间、
 * 执行记录都变了），而不是把单条事件拼进现有列表——拼接会在面板关着的那段时间漏掉
 * 若干条，随后显示一份不完整的历史。
 */
export const scheduleRunTick = writable(0);

/**
 * 该不该为这条执行结果打扰用户。
 *
 * 只有 `failed` 值得弹提示：
 * - `ok` 每分钟一条的任务会把提示条刷爆；
 * - `skipped` 最常见的原因是「那台机器没连着」，而那是本产品**声明过**的边界
 *   （见 ScheduleDialog 顶部说明），反复提醒一件用户已经知道的事就是噪音；
 * - `failed` 是命令本身出了问题，用户不看提示就永远不会知道——除非他主动去翻记录。
 */
export function shouldNotify(outcome: string): boolean {
  return outcome === "failed";
}

/** 后端 `schedule_list` 的一行（对齐 fs_connmgr::schedule_repo::ScheduledTask）。 */
export interface ScheduledTask {
  id: number;
  name: string;
  cron: string;
  command: string;
  profile_id: string;
  enabled: boolean;
  catchup: string;
  tz_offset_minutes: number;
  /** true = 每跳取系统当前本地偏移（跟随夏令时）；false = 用固定偏移 */
  tz_follows_dst: boolean;
  last_fire_minute: number | null;
  created_at: number;
}

/** 一条执行记录（对齐 fs_connmgr::schedule_repo::ScheduledRun）。 */
export interface ScheduledRun {
  id: number;
  task_id: number;
  fired_at: number;
  /** 'ok' | 'failed' | 'skipped' —— 三态必须分开呈现，见 outcomeLabel */
  outcome: string;
  catchup: boolean;
  exit_code: number | null;
  detail: string;
}

/** 后端 `schedule_preview_cron` 的返回。 */
export interface CronPreview {
  valid: boolean;
  error: string | null;
  nextFires: number[];
  note: string | null;
}

/**
 * 本机当前的时区偏移，**东为正**（分钟）。
 *
 * `Date.prototype.getTimezoneOffset()` 的符号是**反的**：它返回「本地时间加多少分钟
 * 等于 UTC」，所以东八区给 **-480**。后端的约定是东为正（+480），两边符号不统一的
 * 后果是所有任务差两个时区——而这种错在东八区看起来像「差 16 小时」，很容易被当成
 * 别的 bug 去查。取反这一步就是本函数存在的全部理由。
 */
export function localTzOffsetMinutes(now: Date = new Date()): number {
  const v = -now.getTimezoneOffset();
  // UTC 下 `-0` 会原样冒出来（JS 的合法值，且 `Object.is(-0, 0)` 为 false）。
  // 归一成 0：一个负零在比较、序列化、日志里都只会让人多花时间确认它无害。
  return v === 0 ? 0 : v;
}

/** 偏移 → `UTC+08:00` / `UTC-05:30` 形式；跟随 DST 的任务显示系统当前值。 */
export function formatOffset(minutes: number): string {
  if (!Number.isFinite(minutes)) return "UTC";
  const sign = minutes < 0 ? "-" : "+";
  const abs = Math.abs(Math.trunc(minutes));
  const h = Math.floor(abs / 60);
  const m = abs % 60;
  const p = (n: number) => String(n).padStart(2, "0");
  return `UTC${sign}${p(h)}:${p(m)}`;
}

/**
 * unix 秒 + 偏移 → 该时区的 `MM-DD HH:mm`。
 *
 * 自己按偏移算而不用 `toLocaleString`：任务的偏移是**它自己的**（创建时定格），
 * 可能与浏览器当前时区不同；用本地化函数会按浏览器时区显示，于是一条 UTC+8 的任务
 * 在一台设成 UTC-5 的机器上显示成另一个时间——而界面上明明标着 UTC+08:00。
 */
export function formatAtOffset(unixSecs: number, offsetMinutes: number): string {
  if (!Number.isFinite(unixSecs) || unixSecs <= 0) return "—";
  const d = new Date((unixSecs + offsetMinutes * 60) * 1000);
  const p = (n: number) => String(n).padStart(2, "0");
  // 用 UTC 取值：时间已经加过偏移了，再让 Date 按本地时区解释就是加两次
  return `${p(d.getUTCMonth() + 1)}-${p(d.getUTCDate())} ${p(d.getUTCHours())}:${p(d.getUTCMinutes())}`;
}

/** 上次触发（分钟序号 → 文案）。从未触发过显示「—」而不是 1970。 */
export function formatLastFire(lastFireMinute: number | null, offsetMinutes: number): string {
  if (lastFireMinute === null || lastFireMinute === undefined) return "—";
  return formatAtOffset(lastFireMinute * 60, offsetMinutes);
}

/** 三态各有各的说法——合成一个「未成功」会把唯一的线索抹掉。 */
export function outcomeLabel(outcome: string): string {
  switch (outcome) {
    case "ok":
      return "成功";
    case "failed":
      return "失败";
    case "skipped":
      return "未执行";
    default:
      return outcome;
  }
}

/**
 * 执行记录该显示成哪种颜色语义。
 *
 * `skipped` 刻意是 warn 而不是 error：「没有活动会话」不是故障，是本产品**明确声明
 * 过**的边界（计划任务只在已开启的会话上执行）。标成红色会让用户以为出了错去排查，
 * 而正确的反应是「哦，那台机器当时没连着」。
 */
export function outcomeKind(outcome: string): "ok" | "warn" | "error" {
  if (outcome === "ok") return "ok";
  if (outcome === "skipped") return "warn";
  return "error";
}

/** 补跑策略的人话。 */
export function catchupLabel(catchup: string): string {
  return catchup === "once" ? "错过后补跑一次" : "错过则跳过";
}

/**
 * 任务在界面上的一句话摘要：如实显示 cron 原文 + 偏移。
 *
 * 不做「cron 人话化」是有意的：带步长与列表的表达式（分段写 7 步长、时段 3-5、
 * 周一和周三）的自然语言描述既长又容易写错，而错误的描述比没有描述更坏——用户会
 * 信它。取而代之的是**预览接下来三次触发**，那是不会说错的。
 *
 * （这段注释刻意不写出那个表达式的字面形式：星号加斜杠在块注释里会把注释本身截断。
 * 第一版就是这么写的，vite 报的是「缺少分号」——一个与真实原因毫无关系的错。）
 */
export function scheduleSummary(t: ScheduledTask): string {
  return `${t.cron}（${formatOffset(t.tz_offset_minutes)}）`;
}
