/**
 * broadcast-actions.ts — 实时键入广播的**动作层**（M4a 通道②；路线图 4c「App.svelte 拆分」，
 * 2026-09-03 从 App.svelte 搬出）。
 *
 * 与 `lib/broadcast.ts` 的分工：那边是 store 与纯解析（composeTarget / liveBroadcast /
 * pickedSessions / resolveTargetSessions / sendToSessions），这边是「点了开关之后发生什么」——
 * 要读 store 快照、要发 IPC、要说人话。搬出来之后这四件事可以直接测，不必渲染整个 App。
 *
 * 唯一没法从 store 拿的是 `profiles`（它是 App 的组件状态，不是 store），故逐个函数收它作参数。
 */
import { get } from "svelte/store";
import { invoke } from "./ipc";
import { toast } from "./toast";
import { tabs, activeTabId } from "./tabs";
import { composeTarget, liveBroadcast, pickedSessions, resolveTargetSessions, sendToSessions } from "./broadcast";
import type { ComposeTarget } from "./compose-history";
import type { Profile } from "./types";

/** 目标 → 会话 id 集合（读的是当前快照）。 */
export function broadcastTargetIds(target: ComposeTarget, profiles: Profile[]): string[] {
  return resolveTargetSessions(target, get(tabs), profiles, get(activeTabId), get(pickedSessions));
}

/** 目标解析为空时的一句话人话（区分三种空因，提示才有可行动性）。 */
export function broadcastEmptyHint(target: ComposeTarget): string {
  if (target === "pick") return "手选目标为空：在「手选…」里勾选要发送到的会话";
  if (target === "group") return "当前没有已连接的会话可发送";
  return "没有已连接的活动会话，命令未发送";
}

/**
 * 活动终端的每次键入转发给广播目标（S308 装配点）。
 *
 * 关闭态零开销短路（不 resolve、不 invoke）。开启时**排除自身**：击键已经经 TerminalPane 自己的
 * term_input 发给了本会话，分组/all 模式的解析结果都含自身，不剔除就会双发（每键两份回显）。
 */
export function routeBroadcastKeystroke(dataB64: string, profiles: Profile[]): void {
  if (!get(liveBroadcast)) return;
  const target = get(composeTarget);
  if (target === "current") return; // 目标=当前会话时无广播意义（路由自身=关）
  const ids = broadcastTargetIds(target, profiles).filter((id) => id !== get(activeTabId));
  if (ids.length === 0) return;
  // 失败静默（仅控制台）：键入级失败的合理场景是某目标恰好断线——每键弹一次 toast 只会把屏幕糊满。
  // 断线标签在 TabBar 有自己的状态灯。
  void sendToSessions(ids, dataB64, invoke).then(({ failed }) => {
    for (const f of failed) console.error(`广播键入失败（${f.sessionId}）：`, f.error);
  });
}

/** ToolBar 📢 开关（M4a 通道②的独立开关）。 */
export function toggleLiveBroadcast(profiles: Profile[]): void {
  const target = get(composeTarget);
  if (!get(liveBroadcast) && target === "current") {
    // 拒开而非空开：目标=当前会话的广播是恒空操作，开了没效果还亮着按钮——
    // 用户以为在广播，实际什么都没发出去，这比不开危险。
    toast.warn("广播目标当前是「当前会话」：请先在组合命令栏选择 全部/分组/手选");
    return;
  }
  const next = !get(liveBroadcast);
  liveBroadcast.set(next);
  if (next) {
    const n = broadcastTargetIds(target, profiles).filter((id) => id !== get(activeTabId)).length;
    toast.info(`实时广播已开启：键入将同步到 ${n} 个其他会话`);
  } else {
    toast.info("实时广播已关闭");
  }
}
