/**
 * 窗口关闭闸门（审计：事件契约门禁扫出的第三处缺陷）。
 *
 * Task 22 Step 4 建了「窗口关闭确认」——统计在途传输、列出会话、确认后 `session_close_all`
 * 再关窗。但它只挂在菜单「文件 → 退出」（`onAction("app.exit")`）上。用户关窗口最常用的三条
 * 路径——标题栏 ×、Alt+F4、任务栏右键关闭——走的是操作系统的关闭请求，**完全绕过**这段门控：
 *   1. 在途传输不提示，一律静默中断；
 *   2. `session_close_all` 不执行，journal 里那批会话仍标着「打开中」，
 *      下次启动的恢复提示会把用户主动关掉的会话当成上次崩溃的残留再问一遍。
 * Tauri 为此发 `tauri://close-requested`，全仓（前端 `onCloseRequested` / Rust
 * `on_window_event`）零处理者——正是「事件发了没人接」这一类。
 *
 * 为什么闸门要独立成模块而不是写在 App.svelte 里：它是个**两态机**，而两态机的错误分支不出
 * 声音。`onCloseRequested` 的处理器若不 `preventDefault()`，Tauri 的 JS 包装器紧接着就
 * `destroy()`；而确认之后我们又必须真的关得掉——同一个 `close()` 会再次触发本处理器。
 * 少了放行位就是「点了确认，窗口不关」的死循环（确认 → close → 拦截 → 再弹确认框）；
 * 放行位置早了则等于没门控。App.svelte 没有组件测试，把这段逻辑留在里面就没有任何东西证明
 * 这两条边同时成立。
 */
export interface CloseGate {
  /**
   * `tauri://close-requested` 处理器体。返回 true = 放行（本次关闭继续），false = 已拦截并转交确认流程。
   * `preventDefault` 由调用方从事件对象上传入（本模块不依赖 Tauri 类型，测试才能不启事件系统地跑）。
   */
  onCloseRequested(preventDefault: () => void): Promise<boolean>;
  /** 用户在确认框上点了确认后调用：置放行位，再真正关窗。 */
  closeConfirmed(): Promise<void>;
}

/**
 * @param close 真正关闭窗口（`getCurrentWindow().close()`）
 * @param ask   弹出关闭确认（App.svelte 的 `requestCloseWindow`，会置 `closeConfirmState`）
 */
export function createCloseGate(close: () => Promise<void>, ask: () => void | Promise<void>): CloseGate {
  // 一次性放行位：只有 closeConfirmed() 会把它抬起来，抬起后不再落下——
  // 抬起之后紧跟的那次 close() 必须穿过去，此后进程就要退出了，没有第二次关闭要门控。
  let confirmed = false;
  return {
    async onCloseRequested(preventDefault: () => void): Promise<boolean> {
      if (confirmed) return true;
      // 必须先拦。Tauri 的包装器是 `await handler(evt); if (!evt.isPreventDefault()) destroy()`——
      // 拦截标记记在同步字段上，但若把 preventDefault 放在下面的 await 之后，任何一次
      // ask() 抛异常都会让窗口直接销毁：确认框弹不出来，反而关得更干脆。
      preventDefault();
      await ask();
      return false;
    },
    async closeConfirmed(): Promise<void> {
      confirmed = true;
      await close();
    },
  };
}

/* ------------------------------------------------------------------ *
 * 关闭确认的判据（M1 出口「关闭确认」条，2026-08-23 补齐缺失的那一半）
 * ------------------------------------------------------------------ */

/**
 * 出口原文：「关闭含**活动会话**或排队/传输的标签/窗口弹模态（列会话数+传输数），
 * 确认后断开并取消传输；**全空闲直接关闭**」。落地时两个 scope 各错一半：
 *
 * - 标签级 `requestCloseTab` **只**数传输 → 一条正跑着 vim 的活连接，只要没有传输任务，
 *   Ctrl+W 直接静默断开。这是**漏问**，也是两者中危险的那个（用户丢工作现场）。
 * - 窗口级 `requestCloseWindow` 数的是 `tabs.length` → 满屏都是断开态的残标签也照样弹框。
 *   这是**多问**，违反「全空闲直接关闭」，代价是用户被训练成闭眼点确认。
 *
 * 两处判据各写各的，正是它们能朝相反方向漂的原因；此处收敛成一个纯函数，
 * 两个 scope 同源，且可被断言。
 */
export type CloseTabStatus = "connecting" | "connected" | "disconnected" | "error";

/** 关闭判据只关心标签的这两个字段，不依赖 tabs.ts 的完整 Tab（测试才能不搭 store 地跑）。 */
export interface CloseCandidate {
  id: string;
  status: CloseTabStatus;
}

/**
 * 「活动会话」= 后端还攥着一条 SSH 连接（或正在建）的会话。
 *
 * `connecting` 算活动：拨号中的连接同样占着后端句柄，且**认证可能已过半**——静默掐掉
 * 与掐掉 `connected` 没有区别。`disconnected` / `error` 不算：连接早没了，标签只是块残骸，
 * 关它不丢任何东西。
 */
export function isLiveSession(status: CloseTabStatus): boolean {
  return status === "connecting" || status === "connected";
}

export interface CloseDecision {
  /** true = 必须弹确认框；false = 全空闲，直接关 */
  prompt: boolean;
  /** 会被断开的活动会话 id（弹框列的「会话数」= 本数组长度；残标签不计入） */
  sessions: string[];
  queued: number;
  running: number;
}

/**
 * @param candidates 本次关闭波及的标签（tab scope 传那一个，window scope 传全部）
 * @param transfers  本次关闭波及的传输计数（同上，tab scope 传该标签的）
 */
export function decideClose(
  candidates: readonly CloseCandidate[],
  transfers: { queued: number; running: number },
): CloseDecision {
  const sessions = candidates.filter((t) => isLiveSession(t.status)).map((t) => t.id);
  const queued = Math.max(0, transfers.queued);
  const running = Math.max(0, transfers.running);
  return {
    // 两个来源是「或」：只有活动会话、只有在途传输、两者都有，都要问；两者皆无才直接关。
    prompt: sessions.length > 0 || queued + running > 0,
    sessions,
    queued,
    running,
  };
}
