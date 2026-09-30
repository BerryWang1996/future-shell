/**
 * 进程列表的过滤与排序（M4a：服务器进程管理）。
 *
 * 纯函数，不碰 DOM、不发 invoke——「一次筛选/排序该得到什么」可以离线钉住，
 * 不必起一台真服务器。
 */

/** 后端 `session_processes` 的一行（对齐 fs_sshengine::procs::ProcessInfo）。 */
export interface ProcessInfo {
  pid: number;
  ppid: number | null;
  user: string;
  cpu: number | null;
  mem: number | null;
  rss_kb: number | null;
  state: string;
  command: string;
}

export type SortKey = "pid" | "user" | "cpu" | "mem" | "rss_kb" | "command";
export type SortDir = "asc" | "desc";

/**
 * 按关键字过滤。命中 pid（整串或前缀）、用户名、命令行任一即保留，大小写不敏感。
 *
 * pid 按**字符串包含**匹配而不是数值相等：运维实际的用法是「我记得是 12 开头的」
 * 或从日志里粘一段进来，要求精确等于会让这两种用法都落空。
 */
export function filterProcesses(list: ProcessInfo[], query: string): ProcessInfo[] {
  const q = query.trim().toLowerCase();
  if (!q) return list;
  return list.filter(
    (p) =>
      String(p.pid).includes(q) ||
      p.user.toLowerCase().includes(q) ||
      p.command.toLowerCase().includes(q),
  );
}

/**
 * 排序。
 *
 * **空值恒排最后**，与升降序无关：`null` 是「不知道」，不是「最小」。把未知值
 * 当 0 排到升序头部，会让内核线程那一堆 `—` 霸占「CPU 占用最低」的位置，而用户
 * 点这一列想看的是真实的最低占用。
 *
 * 同值时按 pid 兜底，保证顺序稳定——否则每次 2 秒轮询回来同 CPU% 的几行会互相
 * 换位，列表看起来在抖。
 */
export function sortProcesses(list: ProcessInfo[], key: SortKey, dir: SortDir): ProcessInfo[] {
  const sign = dir === "asc" ? 1 : -1;
  return [...list].sort((a, b) => {
    const av = a[key];
    const bv = b[key];
    const aNull = av === null || av === undefined || av === "";
    const bNull = bv === null || bv === undefined || bv === "";
    if (aNull && bNull) return a.pid - b.pid;
    if (aNull) return 1; // 空值恒后
    if (bNull) return -1;
    let c: number;
    if (typeof av === "number" && typeof bv === "number") c = av - bv;
    else c = String(av).localeCompare(String(bv));
    return c !== 0 ? c * sign : a.pid - b.pid;
  });
}

/** RSS（KiB）→ 人可读。空值显示「—」，不显示 0。 */
export function formatRss(kb: number | null): string {
  if (kb === null || kb === undefined || !Number.isFinite(kb) || kb < 0) return "—";
  if (kb < 1024) return `${kb} KiB`;
  const mb = kb / 1024;
  if (mb < 1024) return `${mb.toFixed(1)} MiB`;
  return `${(mb / 1024).toFixed(2)} GiB`;
}

/** 百分比列的显示。空值「—」——0.0% 是观测值，「不知道」不是。 */
export function formatPct(v: number | null): string {
  return v === null || v === undefined || !Number.isFinite(v) ? "—" : `${v.toFixed(1)}%`;
}

/**
 * 该操作是否需要「重话」确认。与后端 `procs::needs_loud_confirm` 同口径：
 * PID 1 是 init/容器主进程，杀掉通常整机或整容器停摆；KILL 不可被捕获，进程
 * 来不及落盘。两者都不阻止，但都必须让用户看见后果。
 */
export function needsLoudConfirm(pid: number, signal: string): boolean {
  return pid === 1 || signal.toUpperCase() === "KILL";
}

/** 确认对话框的正文。重话情形明确写出后果，而不是笼统的「确定要终止吗」。 */
export function killConfirmText(p: ProcessInfo, signal: string): string {
  const sig = signal.toUpperCase();
  const head = `向 PID ${p.pid}（${p.user}）发送 ${sig}：\n${p.command || "（无命令行）"}`;
  if (p.pid === 1) {
    return `${head}\n\n⚠ PID 1 是该系统/容器的主进程，终止它通常会让整台机器或整个容器停止。`;
  }
  if (sig === "KILL") {
    return `${head}\n\n⚠ KILL 无法被进程捕获，未落盘的数据会丢失。可先试 TERM。`;
  }
  return head;
}
