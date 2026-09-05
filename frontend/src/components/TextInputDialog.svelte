<script lang="ts">
  /**
   * 单行文本输入模态（M4a 新建文件夹的载体，也可复用于后续「重命名」类操作）。
   *
   * 替换原生 `prompt()`。三条理由，都是 DeleteConfirmDialog 换掉原生 `confirm()`
   * 时同样的理由：
   * ① `prompt()` **同步阻塞整个 webview**——期间终端输出事件只进队列不渲染；
   * ② jsdom 下返回 undefined，这条分支在组件测试里永远走不到，等于不可测；
   * ③ 观感与全仓其余模态不一致（无焦点陷阱、无 Esc 语义、样式不受主题控制）。
   */
  import { useFocusTrap } from "../lib/focusTrap";

  let {
    open = false,
    title = "",
    label = "",
    placeholder = "",
    initial = "",
    confirmText = "确定",
    /** 就地校验：返回错误文案则不放行（空串/undefined = 通过）。 */
    validate = (_v: string) => "" as string | undefined,
    onConfirm,
    onCancel,
  }: {
    open?: boolean;
    title?: string;
    label?: string;
    placeholder?: string;
    initial?: string;
    confirmText?: string;
    validate?: (value: string) => string | undefined;
    onConfirm(value: string): void;
    onCancel(): void;
  } = $props();

  let value = $state("");
  let dialogEl = $state<HTMLDivElement | undefined>();
  let inputEl = $state<HTMLInputElement | undefined>();

  // open 时重置为 initial：模态是复用的，留着上次的输入会让用户在「新建」时
  // 看到上一个名字（继而误以为是在改那一条）。
  $effect(() => {
    if (open) value = initial;
  });

  $effect(() => {
    if (!open || !dialogEl) return;
    return useFocusTrap(dialogEl, { initial: inputEl ?? null });
  });

  const error = $derived(open ? (validate(value) ?? "") : "");

  function confirm() {
    if (error) return;
    onConfirm(value);
  }
</script>

{#if open}
  <div class="overlay" role="presentation" onclick={onCancel}>
    <div
      class="dialog" role="dialog" aria-modal="true" aria-label={title} tabindex="-1"
      data-testid="text-input-dialog" bind:this={dialogEl}
      onclick={(e) => e.stopPropagation()}
      onkeydown={(e) => { if (e.key === "Escape") onCancel(); }}
    >
      <h3>{title}</h3>
      <label>
        {label}
        <input
          bind:this={inputEl}
          bind:value
          data-testid="ti-input"
          {placeholder}
          onkeydown={(e) => { if (e.key === "Enter" && !e.isComposing) { e.preventDefault(); confirm(); } }}
        />
      </label>
      {#if error}<p class="err" role="alert" data-testid="ti-err">{error}</p>{/if}
      <footer>
        <button data-testid="ti-ok" disabled={!!error} onclick={confirm}>{confirmText}</button>
        <button data-testid="ti-cancel" onclick={onCancel}>取消</button>
      </footer>
    </div>
  </div>
{/if}

<style>
  .overlay { position: fixed; inset: 0; background: rgba(0,0,0,.5); display: grid; place-items: center; z-index: 60; }
  .dialog { width: 420px; max-width: 92vw; max-height: 92vh; overflow-y: auto; padding: 16px 20px; background: var(--fs-bg-elevated); border: 1px solid var(--fs-border); border-radius: 6px; color: var(--fs-fg-primary); font-size: 13px; }
  h3 { margin: 0 0 12px; font-size: 14px; }
  label { display: flex; flex-direction: column; gap: 4px; color: var(--fs-fg-secondary); }
  input { background: var(--fs-bg-input); color: var(--fs-fg-primary); border: 1px solid var(--fs-border); border-radius: 4px; padding: 5px 8px; font: inherit; }
  .err { margin: 6px 0 0; color: var(--fs-danger); font-size: 12px; }
  footer { display: flex; justify-content: flex-end; gap: 8px; margin-top: 14px; }
  footer button { padding: 5px 14px; border: 1px solid var(--fs-border); background: var(--fs-bg-panel); color: var(--fs-fg-primary); border-radius: 4px; cursor: pointer; }
  footer button:disabled { opacity: .5; cursor: default; }
</style>
