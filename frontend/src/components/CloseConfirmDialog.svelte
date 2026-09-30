<script lang="ts">
  import { useFocusTrap } from "../lib/focusTrap";

  let {
    open,
    scope,
    sessions,
    queued,
    running,
    onConfirm,
    onCancel,
  }: {
    open: boolean;
    /**
     * 关的是什么。
     *
     * 这个 prop 曾经**声明了却从没被消费**——模板里一个字都没用到它，
     * 于是「关整个窗口」与「关一个标签」弹的是一模一样的框。危险程度差着量级的
     * 两件事长得一样，是确认框最不该有的样子：用户按下「关闭并断开」时
     * 心里想的是哪一件，只能靠他记得自己刚才点了什么。
     *
     * `tabs` = 关一批标签（「关闭全部标签」「关闭其他标签」，M4b）。
     */
    scope: "window" | "tab" | "tabs";
    sessions: string[];
    queued: number;
    running: number;
    onConfirm: () => void;
    onCancel: () => void;
  } = $props();

  /** 标题与主按钮跟着 scope 走：用户要能一眼看出自己在关什么。 */
  const title = $derived(
    scope === "window" ? "关闭窗口" : scope === "tabs" ? "关闭多个标签" : "关闭标签",
  );
  const confirmLabel = $derived(scope === "window" ? "关闭窗口并断开" : "关闭并断开");

  let dialogEl = $state<HTMLDivElement | undefined>();
  let cancelBtn = $state<HTMLButtonElement | undefined>();

  $effect(() => {
    if (open && dialogEl) {
      const cleanup = useFocusTrap(dialogEl, { initial: cancelBtn ?? null }); // 默认焦点「取消」
      return cleanup;
    }
  });

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === "Escape") {
      e.preventDefault();
      onCancel();
    }
  }
</script>

{#if open}
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div class="overlay" onclick={onCancel} onkeydown={handleKeydown}>
    <!-- svelte-ignore a11y_interactive_supports_focus -->
    <div
      class="dialog"
      role="dialog"
      aria-labelledby="close-confirm-title"
      bind:this={dialogEl}
      onclick={(e) => e.stopPropagation()}
      onkeydown={(e) => e.stopPropagation()}
    >
      <h2 id="close-confirm-title" data-testid="close-confirm-title">{title}</h2>
      <div class="content">
        <!-- 会话行只在真有活动会话时出现：判据改用 decideClose 后，「断开态残标签 + 在途传输」
             这一组合会带着空 sessions 弹框，无条件渲染就成了「将断开 0 个会话（）」。 -->
        {#if sessions.length > 0}
          <p>
            将断开 {sessions.length} 个会话{sessions.length <= 10 ? `（${sessions.join("、")}）` : `（${sessions.slice(0, 10).join("、")}…等 ${sessions.length} 个）`}
          </p>
        {/if}
        {#if queued + running > 0}
          <p>取消 {queued + running} 个传输（排队 {queued} / 进行中 {running}）</p>
        {/if}
      </div>
      <div class="actions">
        <button class="danger" data-testid="close-confirm-ok" onclick={onConfirm}>{confirmLabel}</button>
        <button bind:this={cancelBtn} data-testid="close-confirm-cancel" onclick={onCancel}>取消</button>
      </div>
    </div>
  </div>
{/if}

<style>
  .overlay {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.5);
    z-index: 100;
    display: flex;
    align-items: center;
    justify-content: center;
  }
  .dialog {
    background: var(--fs-bg-elevated);
    border: 1px solid var(--fs-border-strong);
    border-radius: var(--fs-radius);
    box-shadow: var(--fs-shadow);
    min-width: min(400px, 92vw);
    max-width: 600px;
    max-height: 92vh;
    overflow-y: auto;
    padding: 20px;
  }
  h2 {
    margin: 0 0 16px;
    font-size: 16px;
    color: var(--fs-fg-primary);
  }
  .content {
    margin-bottom: 20px;
    color: var(--fs-fg-secondary);
    font-size: 13px;
  }
  .content p {
    margin: 8px 0;
  }
  .actions {
    display: flex;
    gap: 8px;
    justify-content: flex-end;
  }
  button {
    padding: 6px 16px;
    border: 1px solid var(--fs-border);
    border-radius: var(--fs-radius);
    background: var(--fs-bg-panel);
    color: var(--fs-fg-primary);
    cursor: pointer;
    font-size: 13px;
  }
  button:hover {
    background: var(--fs-bg-hover);
  }
  button.danger {
    background: var(--fs-danger);
    border-color: var(--fs-danger);
    color: white;
  }
  button.danger:hover {
    opacity: 0.9;
  }
</style>
