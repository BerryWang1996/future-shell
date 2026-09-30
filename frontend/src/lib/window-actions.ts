/**
 * window-actions.ts — 多窗口动作（M4b；路线图 4c「App.svelte 拆分」，2026-09-03 从 App.svelte 搬出）。
 *
 * 与 profile-io 同一个搬出理由：这四件事只用 IPC + toast + tabs store，不碰 App 的组件状态。
 * 窗口几何本身在 Rust 侧（`app/src/window_layout.rs`），这里只是把动作发过去并把失败说出来。
 */
import { get } from "svelte/store";
import { invoke } from "./ipc";
import { toast } from "./toast";
import { tabs } from "./tabs";

/**
 * 开一个视图窗口。
 *
 * 「视图克隆」在本架构下不需要任何同步机制：会话活在后端，输出经 `term:data:{sessionId}` 广播
 * （Tauri 的 emit 是全窗口的），输入走 `term_input(sessionId, …)` 进同一个 PTY。
 * 两个窗口看的本来就是同一份流。
 */
export async function openWindow(sessionId: string | null, title: string | null): Promise<void> {
  try {
    await invoke<string>("window_new", { sessionId, title });
  } catch (e) {
    toast.error(`新建窗口失败: ${e}`);
  }
}

/**
 * 会话关了，把它的视图窗口一起关掉。
 *
 * 不关的话会留下一个显示**已死会话**的窗口：它还在那儿、还能往里打字，但字节没有去处。
 * 用户会以为连接还在，直到发现什么都没回应——而那时他已经在一个不存在的会话里敲了一串命令。
 *
 * 发的是 sessionId 而不是窗口 label：label 怎么拼是后端的事。前端自己拼一份的话，
 * 两份规则必然走散，而走散的表现正是这个窗口永远关不掉。
 */
export function closeViewWindowFor(sessionId: string): void {
  void invoke("window_close_view", { sessionId }).catch(() => {
    /* 窗口本来就不存在是常态（大多数标签没被拖出过），不值得打扰用户 */
  });
}

/**
 * 标签拖出 → 视图窗口。标签**留在**主窗口（克隆而非搬家，见 TabBar 的 onDetach 文档注）。
 */
export function detachTab(tabId: string, title: string): void {
  // 标签 id 就是 sessionId（tabs store 的既有契约）；找不到说明标签已经被关了，
  // 这时开一个显示已死会话的窗口只会让用户困惑。
  if (!get(tabs).some((t) => t.id === tabId)) return;
  void openWindow(tabId, title);
}

/** 排列全部窗口。 */
export async function arrangeWindows(mode: string): Promise<void> {
  try {
    const r = await invoke<{ arranged: number; overflowed: number }>("window_arrange", { mode });
    if (r.overflowed > 0) {
      // 沉默地摆成一堆是最坏的结果：用户看到的会是「点了平铺，窗口还是叠着的」，
      // 而他无从知道是没生效还是屏幕放不下。
      toast.info(`屏幕放不下 ${r.arranged} 个窗口，已尽量排开（${r.overflowed} 个仍有重叠）`);
    }
  } catch (e) {
    toast.error(`排列窗口失败: ${e}`);
  }
}
