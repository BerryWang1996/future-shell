import type { TabStatus } from "./tabs";

/**
 * 后端 `session:status` 载荷。两个发送点：
 * - `app/src/events.rs` 的 `SessionEvents::status()` —— 连接过程逐步播报（跳板第几跳、
 *   尝试哪种认证方法、某方法未发起的理由、策略拒连的理由）。**它们全都不是状态迁移**，
 *   其中「拒绝连接 …」「尝试认证方法：…」恰恰发生在**没有连上**的时刻。
 * - `app/src/commands/session_cmd.rs` 重连成功处 —— 这一条才是迁移，故带 `state: "connected"`。
 */
export interface SessionStatusPayload {
  session_id: string;
  message?: string;
  state?: string;
}

/**
 * `session:status` → 标签状态迁移；返回 null 表示「只是一行进度文案，不迁移」。
 *
 * 缺陷本体：原桥接是 `setTabStatus(session_id, "connected")` **无条件**执行，即把这条
 * 「进度文案通道」当成了连接成功信号。首次连接期标签尚未建立，这个错误被掩盖着；
 * 一旦进入重连（标签在、id 相同），重连循环里 `connect()` 打的第一行进度
 * （「跳板 1/2：直连 …」/「尝试认证方法：password」）就会把一个正在重连的标签点绿，
 * 若这一轮随后拒连/认证失败，标签就一直停在绿点上，与 banner 里的重连倒计时自相矛盾。
 *
 * 判据用后端显式给的 `state` 而不是 `message.includes("已重连")`：文案是给人看的，
 * 随时可以改措辞、可以做 i18n，拿它当协议字段等于把 UI 文案变成不可改的接口。
 */
export function statusTransition(payload: SessionStatusPayload): TabStatus | null {
  return payload.state === "connected" ? "connected" : null;
}
