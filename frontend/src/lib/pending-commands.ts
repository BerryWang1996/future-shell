/**
 * pending-commands.ts — 「等这个会话连上之后，把这条命令送进它的终端」（M7.3）。
 *
 * # 为什么需要它
 *
 * 「双击 `.sh` → 前台运行 → 输出进**新终端标签**」（M7.3 出口标准②）意味着：先开一个新会话，
 * 再往里发命令。而 `openSession` 返回时会话还在拨号——那一刻 `term_input` 会被后端以
 * 「no session」拒掉，用户看到的是一个空白的新标签，什么都没发生。
 *
 * 所以命令要**排队**，由 App 在两处取走：`openSession` resolve 之后、以及 TerminalPane 的
 * `onReady`（终端真正建好、开始收 `term:data` 的那一刻）。两处都要，因为谁先到不确定；
 * `takeForSession` 取走即清保证只发一次。
 *
 * 排空点**不是** `session:status` → connected：首次连接时后端那几条 connected 事件全部发生在
 * 标签存在之前（见 lib/session-status.ts），事件桥根本收不到——只在重连成功时才会到。
 * 队列放在模块级而不是组件里：发起方（SftpPane，嵌在旧标签的 TerminalPane 里）与
 * 消费方（App）之间没有任何父子关系，靠 props 传递要穿三层。
 *
 * # 一次性
 *
 * 取走即清。会话可能因为断线重连再次进入 connected——那时把命令再跑一遍是**执行了两次**，
 * 而用户只双击过一次。
 */

const pending = new Map<string, string[]>();

/** 排一条命令，等这个会话连上时发。 */
export function queueForSession(sessionId: string, command: string): void {
  const list = pending.get(sessionId) ?? [];
  list.push(command);
  pending.set(sessionId, list);
}

/** 取走这个会话排队的全部命令（取走即清；没有则空数组）。 */
export function takeForSession(sessionId: string): string[] {
  const list = pending.get(sessionId) ?? [];
  pending.delete(sessionId);
  return list;
}

/** 会话没连上就被关掉了：丢掉它的队列，别留到下一个同 id 的会话上。 */
export function dropForSession(sessionId: string): void {
  pending.delete(sessionId);
}

/** 仅测试用。 */
export function resetPendingCommandsForTest(): void {
  pending.clear();
}

/** 仅测试/诊断用：还排着几条。 */
export function pendingCount(sessionId: string): number {
  return pending.get(sessionId)?.length ?? 0;
}
