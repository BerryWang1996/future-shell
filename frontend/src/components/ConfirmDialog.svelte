<script lang="ts">
  /**
   * 通用确认模态（M4a：进程终止的载体，后续「重启服务」类操作可复用）。
   *
   * 为什么不复用 [`DeleteConfirmDialog`]：那个组件的文案写死为「确认删除{what}
   * 「{name}」？该操作不可撤销。」，它自己的注释也明确警告——**文案与实际后果不符
   * 的确认框比没有确认框更坏**，因为用户按对了自己以为的那件事，做成的是另一件。
   * 「终止进程」不是「删除」，硬套过去正是那条警告说的情形。
   *
   * 与原生 `confirm()` 的区别同 DeleteConfirmDialog：原生 confirm 同步阻塞整个
   * webview（期间终端输出只进队列不渲染），且在 jsdom 下返回 undefined，这条分支
   * 在组件测试里永远走不到。
   */
  import { useFocusTrap } from "../lib/focusTrap";

  let {
    open = false,
    title = "确认",
    /** 正文。允许多行（`white-space: pre-wrap`），供「后果说明」独立成段。 */
    message = "",
    confirmText = "确定",
    /** true = 确认按钮按危险色渲染 */
    danger = false,
    onConfirm,
    onCancel,
  }: {
    open?: boolean;
    title?: string;
    message?: string;
    confirmText?: string;
    danger?: boolean;
    onConfirm(): void;
    onCancel(): void;
  } = $props();

  let dialogEl = $state<HTMLDivElement | undefined>();
  let cancelBtn = $state<HTMLButtonElement | undefined>();

  $effect(() => {
    // 默认焦点给「取消」：可能有破坏性后果的操作不该让回车直接落在确认上
    if (!open || !dialogEl || !cancelBtn) return;
    return useFocusTrap(dialogEl, { initial: cancelBtn });
  });
</script>

{#if open}
  <div class="overlay" role="presentation" onclick={onCancel}>
    <div class="dialog" role="dialog" aria-modal="true" aria-label={title} tabindex="-1"
         data-testid="confirm-dialog" bind:this={dialogEl}
         onclick={(e) => e.stopPropagation()}
         onkeydown={(e) => { if (e.key === "Escape") onCancel(); }}>
      <h3>{title}</h3>
      <p data-testid="confirm-msg">{message}</p>
      <footer>
        <button class:danger onclick={onConfirm} data-testid="confirm-ok">{confirmText}</button>
        <button bind:this={cancelBtn} onclick={onCancel} data-testid="confirm-cancel">取消</button>
      </footer>
    </div>
  </div>
{/if}

<style>
  .overlay { position: fixed; inset: 0; background: rgba(0,0,0,.5); display: grid; place-items: center; z-index: 60; }
  .dialog { width: 440px; max-width: 92vw; max-height: 92vh; overflow-y: auto; padding: 16px 20px; background: var(--fs-bg-elevated); border: 1px solid var(--fs-border); border-radius: 6px; color: var(--fs-fg-primary); }
  h3 { margin: 0 0 8px; font-size: 14px; }
  .dialog p { margin: 0 0 12px; font-size: 13px; white-space: pre-wrap; word-break: break-all; }
  footer { display: flex; justify-content: flex-end; gap: 8px; }
  footer button { padding: 5px 14px; border: 1px solid var(--fs-border); background: var(--fs-bg-panel); color: var(--fs-fg-primary); border-radius: 4px; cursor: pointer; }
  footer button.danger { background: var(--fs-danger); border-color: var(--fs-danger); color: #fff; }
</style>
