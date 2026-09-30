<script lang="ts">
  import { useFocusTrap } from "../lib/focusTrap";

  let {
    open = false,
    name = "",
    what = "连接",
    onConfirm = () => {},
    onCancel = () => {},
  }: {
    open?: boolean;
    name?: string;
    /** 被删对象的名词。默认「连接」= 目前唯一的消费方（App.svelte 侧栏右键「删除…」）。
     *  原文案硬写「会话」，而本组件删的是连接档案——文案与实际后果不符的确认框比没有确认框更坏：
     *  用户按对了自己以为的那件事，删掉的是另一件。新增消费方必须显式传 what。 */
    what?: string;
    onConfirm?(): void;
    onCancel?(): void;
  } = $props();

  // bind:this 的目标必须是 $state：否则赋值不进入响应式图，下面的 $effect 不会因绑定完成而重跑，
  // 焦点陷阱拿到的是 undefined —— 删除确认框不抢焦点时回车会落在背景元素上。
  let dialogEl = $state<HTMLDivElement | undefined>();
  let cancelBtn = $state<HTMLButtonElement | undefined>();

  $effect(() => {
    // 两个 ref 齐了才装陷阱，避免装了又拆的抖动；默认焦点给「取消」（破坏性操作不默认选中）。
    if (!open || !dialogEl || !cancelBtn) return;
    return useFocusTrap(dialogEl, { initial: cancelBtn });
  });
</script>

{#if open}
  <div class="overlay" role="presentation" onclick={onCancel}>
    <!-- tabindex="-1"：role="dialog" 需可聚焦（a11y_interactive_supports_focus），
         负值使其可被 focus() 定位但不占 Tab 序列。 -->
    <div class="dialog" role="dialog" aria-modal="true" aria-label="删除确认" tabindex="-1"
         data-testid="delete-confirm-dialog" bind:this={dialogEl}
         onclick={(e) => e.stopPropagation()}
         onkeydown={(e) => { if (e.key === "Escape") onCancel(); }}>
      <p data-testid="delete-msg">确认删除{what}「{name}」？该操作不可撤销。</p>
      <footer>
        <button class="danger" onclick={onConfirm} data-testid="delete-ok">删除</button>
        <button bind:this={cancelBtn} onclick={onCancel} data-testid="delete-cancel">取消</button>
      </footer>
    </div>
  </div>
{/if}

<style>
  .overlay { position: fixed; inset: 0; background: rgba(0,0,0,.5); display: grid; place-items: center; z-index: 60; }
  .dialog { width: 380px; max-width: 92vw; max-height: 92vh; overflow-y: auto; padding: 16px 20px; background: var(--fs-bg-elevated); border: 1px solid var(--fs-border); border-radius: 6px; color: var(--fs-fg-primary); }
  .dialog p { margin: 0 0 12px; font-size: 13px; }
  footer { display: flex; justify-content: flex-end; gap: 8px; }
  footer button { padding: 5px 14px; border: 1px solid var(--fs-border); background: var(--fs-bg-panel); color: var(--fs-fg-primary); border-radius: 4px; cursor: pointer; }
  footer button.danger { background: var(--fs-danger); border-color: var(--fs-danger); color: #fff; }
</style>
