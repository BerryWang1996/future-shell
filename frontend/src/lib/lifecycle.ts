/**
 * lifecycle.ts — 把「异步落定的解绑函数」挂到组件生命周期上（路线图 4c 架构缺陷 A2，2026-09-02）。
 *
 * 单独成模块而不放进 ipc.ts：组件测试普遍整模块 mock 掉 `../lib/ipc`（invoke/listen/settingGet…），
 * 本工具若也住在那里，每一份 mock 都得再补一个 `untilUnmount`，漏一份就是一条
 * 「No "untilUnmount" export is defined on the mock」。它本身不碰 IPC（只在登记失败时借
 * `reportFrontendError` 留痕），没有理由被那些 mock 连坐。
 */
import { reportFrontendError, type UnlistenFn } from "./ipc";

/**
 * 症结：`listen()` 是异步的——先经 IPC 向后端登记，Promise 落定后才给出解绑函数。组件的清理
 * 却是同步的：`onMount` 返回的函数 / `onDestroy` / `$effect` 的 teardown 都在卸载那一刻立即执行。
 * 组件若在 Promise 落定**之前**就卸载了（标签刚开就关、对话框一闪而过、HMR 重挂），清理时手里
 * 还没有解绑函数 → 什么都没解 → 稍后 Promise 落定，一个指向已卸载组件的处理器被永久登记：
 * 闭包持着死掉的 DOM/状态，每个事件都往里写，且再没有任何东西能解它。
 * 全仓 8 个组件各写一份 `un = await listen(...)` / `.then((fn) => { unlisten = fn })`，每一份都有这条缝；
 * `getCurrentWebview().onDragDropEvent()` 同族。
 *
 * 用法：`const un = untilUnmount(listen<T>("evt", handler)); … return () => un();`
 * 返回值**同步可用**：卸载时调它——解绑函数已到手就立刻解；还没到就记下「已卸载」，
 * Promise 一落定当刻解掉，不给处理器任何存活窗口。需要等登记完成再继续的
 * （TerminalPane 先订阅再读设置）用 `await un.ready`。登记失败（IPC 不通）上报后端日志、不抛给
 * 调用方——与 `reportFrontendError` 同一口径：诊断侧支不该改变主流程。
 * 判据：untilUnmount.test.ts（行为）+ lifecycle-guards.test.ts（调用点纪律）。
 */
export interface TrackedUnlisten {
  (): void;
  /** 登记完成（或失败且已上报）后落定；恒不 reject。 */
  ready: Promise<void>;
}

export function untilUnmount(pending: Promise<UnlistenFn>): TrackedUnlisten {
  let fn: UnlistenFn | null = null;
  let disposed = false;
  const dispose = (() => {
    disposed = true;
    if (fn) {
      const f = fn;
      fn = null;
      f();
    }
  }) as TrackedUnlisten;
  dispose.ready = pending.then(
    (f) => {
      if (disposed) f(); // 卸载先于落定：落定当刻就地解绑
      else fn = f;
    },
    (e) => reportFrontendError("listen", e),
  );
  return dispose;
}
