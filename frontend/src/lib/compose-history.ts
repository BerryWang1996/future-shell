export type ComposeTarget = "current" | "all" | "group" | "pick";

export const HISTORY_LIMIT = 100;

const store = new Map<string, string[]>();
const cursors = new Map<string, number>(); // -1 = 未进入历史浏览

export function historyPush(target: string, cmd: string): void {
  const trimmed = cmd.trim();
  if (!trimmed) return;
  const list = store.get(target) ?? [];
  if (list[list.length - 1] === trimmed) {
    cursors.set(target, -1);
    return;
  }
  list.push(trimmed);
  if (list.length > HISTORY_LIMIT) list.shift();
  store.set(target, list);
  cursors.set(target, -1);
}

/** ↑：回退上一条历史（最新→最旧）；到顶保持；无历史返回 null。 */
export function historyPrev(target: string): string | null {
  const list = store.get(target);
  if (!list || list.length === 0) return null;
  let cur = cursors.get(target) ?? -1;
  if (cur === -1) cur = list.length - 1;
  else if (cur > 0) cur -= 1;
  cursors.set(target, cur);
  return list[cur] ?? null;
}

/** ↓：前进下一条历史；越过最新一条返回 null（调用方清空输入）。 */
export function historyNext(target: string): string | null {
  const list = store.get(target);
  if (!list || list.length === 0) return null;
  let cur = cursors.get(target) ?? -1;
  if (cur === -1) return null;
  cur += 1;
  if (cur >= list.length) {
    cursors.set(target, -1);
    return null;
  }
  cursors.set(target, cur);
  return list[cur] ?? null;
}

/** 测试钩子：清空全部历史。 */
export function historyClear(): void {
  store.clear();
  cursors.clear();
}
