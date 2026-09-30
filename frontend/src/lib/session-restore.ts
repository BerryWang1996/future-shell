/**
 * 启动时对「未关闭会话」的处置裁决（UI 规格 §2.12「启动时询问恢复未关闭的会话」）。
 *
 * 抽成纯函数是为了让 `ui.restoreUnclosed` 这个开关**有测试**：它的三种走向都在
 * App.svelte 的 onMount 里，而 onMount 还夹着 vault_status、事件订阅等一串副作用，
 * 在组件层面复现「关掉开关且有 3 行遗留」这一格要连带搭一套 IPC 桩。裁决本身只有
 * 两个输入，单独拎出来后一格一格钉得住。
 */
export type RestoreAction =
  /** 弹恢复框，让用户逐行裁决。 */
  | "ask"
  /** 不问，直接把遗留行全部弃掉。 */
  | "discard"
  /** 无事可做（没有遗留行）。 */
  | "none";

/**
 * @param askOnStart `ui.restoreUnclosed` 的当前值（缺省 true）。
 * @param unclosedCount `sessions_unclosed` 返回的行数。
 *
 * 关掉开关时返回 `discard` 而非 `none`：用户说「别问我」等价于对每一行都作出了
 * 「放弃」的裁决（与恢复框里「全部放弃」同一个 IPC）。若只是跳过弹框，unclosed 表
 * 就只进不出——用户日后重新打开这个开关，迎面是一堆几个月前的陈年会话。
 */
export function restoreAction(askOnStart: boolean, unclosedCount: number): RestoreAction {
  if (unclosedCount <= 0) return "none";
  return askOnStart ? "ask" : "discard";
}
