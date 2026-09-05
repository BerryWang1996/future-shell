/**
 * 历史命令的前端类型与显示辅助（M4a）。
 *
 * 纯函数，可离线钉。
 */

/** 后端 `history_search` 的一行（对齐 fs_connmgr::HistoryEntry）。 */
export interface HistoryEntry {
  id: number;
  command: string;
  host: string;
  profile_id: string;
  /** `sent` = 亲手发出（字节精确）；`grid` = 屏幕启发式提取（近似） */
  source: "sent" | "grid";
  /** unix 秒 */
  used_at: number;
  use_count: number;
}

/**
 * 「多久以前」的相对时间文案。
 *
 * 用相对而非绝对时间：历史面板里要回答的是「这是我刚才跑的还是上个月跑的」，
 * 相对时间一眼就能判断，而 `2026-08-21 17:03` 需要用户自己做减法。超过一周才
 * 退回绝对日期——那时候「9 天前」已经不比日期更有信息量了。
 *
 * `now` 可注入，测试才不必依赖真实时钟。
 */
export function formatUsedAt(usedAt: number, now: number = Date.now() / 1000): string {
  if (!Number.isFinite(usedAt) || usedAt <= 0) return "—";
  const d = Math.floor(now - usedAt);
  // 时钟回拨、或记录时间戳来自「未来」时 d 为负，落进下面这条 `< 60` 就是「刚刚」
  // ——不会出现「-3 分钟前」。曾在这里放过一条单独的 `d < 0` 前置判断，变异测试
  // 证明它杀不掉：所有阈值都是下界，负数必然先命中最小的那一条。删掉重复的机制。
  if (d < 60) return "刚刚";
  if (d < 3600) return `${Math.floor(d / 60)} 分钟前`;
  if (d < 86400) return `${Math.floor(d / 3600)} 小时前`;
  if (d < 7 * 86400) return `${Math.floor(d / 86400)} 天前`;
  const dt = new Date(usedAt * 1000);
  const p = (n: number) => String(n).padStart(2, "0");
  return `${dt.getFullYear()}-${p(dt.getMonth() + 1)}-${p(dt.getDate())}`;
}

/** 来源标签。`grid` 必须与 `sent` 视觉可分——见 HistoryDialog 组件头。 */
export function sourceLabel(source: HistoryEntry["source"]): string {
  return source === "grid" ? "屏幕" : "发出";
}

/**
 * 该条目是否允许「回车直接重发」。
 *
 * `grid` 来源不允许：它可能带提示符残渣，必须让人看着点一下。这不是多余的谨慎
 * ——把一段被误切的输出当命令发出去是真事故。
 */
export function canQuickResend(entry: HistoryEntry): boolean {
  return entry.source === "sent";
}

/**
 * 先发送、**成功之后**才记历史。
 *
 * 抽成带注入的函数只为一件事：让这个次序可测。次序反了的后果不是小瑕疵——历史里
 * 会出现一条其实从未发出去的命令，而用户下次翻历史时会据此认为「这个我跑过了」。
 * `send` 抛出时本函数直接向上抛，`record` 不会被调用。
 *
 * `record` 刻意是同步的、且**不返回**任何东西：记历史失败不该影响「命令已发出」
 * 这个结果，所以它自己内部吞掉错误（见 App.svelte 的 recordHistory）。
 */
export async function sendThenRecord(
  command: string,
  sessionId: string,
  send: (command: string) => Promise<void>,
  record: (command: string, sessionId: string) => void,
): Promise<void> {
  await send(command);
  record(command, sessionId);
}
