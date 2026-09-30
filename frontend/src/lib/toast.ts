import { writable } from "svelte/store";
import { prefersReducedMotion } from "./motion";

export type ToastLevel = "info" | "warn" | "error";
export interface ToastItem {
  id: number;
  level: ToastLevel;
  msg: string;
  /** 退场中：已被 dismiss，等退场动效（Toast.svelte 的 .toast.leaving）走完这一拍再从列表删。 */
  leaving?: boolean;
}

/** 自动消失时长 info 3s / warn 5s / error 8s；同时最多 4 条堆叠，超限挤出最旧（UI 规格 §8 Toast 行） */
const DWELL_MS: Record<ToastLevel, number> = { info: 3000, warn: 5000, error: 8000 };
const MAX_STACKED = 4;
/**
 * 退场动效时长（路线图 4c 动效补齐，2026-09-02）：与 Toast.svelte 的 `.toast.leaving`
 * 过渡（--fs-motion-base 140ms）同拍，多留 20ms 余量让过渡确实走完再删节点。
 * `prefers-reduced-motion` 下不等这一拍——CSS 侧的过渡已被全局规则清零，再等就是白等。
 */
export const TOAST_EXIT_MS = 160;

const store = writable<ToastItem[]>([]);
let nextId = 1;

export const toast = {
  subscribe: store.subscribe,
  /** 推一条 toast，返回 id（供提前 dismiss） */
  push(level: ToastLevel, msg: string): number {
    const id = nextId++;
    store.update((list) => [...list.slice(-(MAX_STACKED - 1)), { id, level, msg }]);
    setTimeout(() => toast.dismiss(id), DWELL_MS[level]);
    return id;
  },
  /**
   * 关掉一条：先标 `leaving`（组件据此播退场动效），一拍之后再从列表删。
   * 不存在的 id、已在退场的 id 一律无操作——后者防止「自动消失 + 用户点 ×」撞在一起时排两个删除。
   */
  dismiss(id: number): void {
    let marked = false;
    store.update((list) =>
      list.map((t) => {
        if (t.id !== id || t.leaving) return t;
        marked = true;
        return { ...t, leaving: true };
      }),
    );
    if (!marked) return;
    const remove = () => store.update((list) => list.filter((t) => t.id !== id));
    if (prefersReducedMotion()) remove();
    else setTimeout(remove, TOAST_EXIT_MS);
  },
  /** 便捷方法 */
  info(msg: string): number { return this.push("info", msg); },
  warn(msg: string): number { return this.push("warn", msg); },
  error(msg: string): number { return this.push("error", msg); },
};

/** 仅测试用：清空列表（不走退场，不排计时器）。 */
export function resetToastsForTest(): void {
  store.set([]);
}
