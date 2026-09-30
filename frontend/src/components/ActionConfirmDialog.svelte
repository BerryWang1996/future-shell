<script lang="ts">
  /**
   * 危险动作确认框（路线图 M7.2「统一确认口径」，2026-09-03）。
   *
   * 与既有 `ConfirmDialog` 的分工：那个是通用「标题 + 一段文字」的模态，本组件多两件事——
   * ① **命令全文**独立成块（等宽、可选中、可滚动、按字符换行），出口原文要的就是这个：
   *    「不是『确定吗？』」。用户批准的是这一串字符，那它就必须原样在他眼前。
   * ② **「以后不再显示」按动作类别**记忆，勾选项上写着类别名——「以后不再显示」四个字
   *    单独出现时用户无从知道自己在放行多大一片。
   *
   * 单实例挂在 App 顶层，由 `lib/confirm-gate.ts` 的 `pendingConfirm` store 驱动。
   */
  import { useFocusTrap } from "../lib/focusTrap";
  import { actionLabel } from "../lib/action-confirm";
  import { pendingConfirm } from "../lib/confirm-gate";

  let dialogEl = $state<HTMLDivElement | undefined>();
  let cancelBtn = $state<HTMLButtonElement | undefined>();
  let suppress = $state(false);

  const req = $derived($pendingConfirm);

  $effect(() => {
    // 每次换一条待确认都把勾选复位：上一条勾过不代表这一条也要勾——那是两个类别。
    void req;
    suppress = false;
  });

  $effect(() => {
    // 默认焦点给「取消」：可能有破坏性后果的操作不该让回车直接落在确认上（同 ConfirmDialog）。
    if (!req || !dialogEl || !cancelBtn) return;
    return useFocusTrap(dialogEl, { initial: cancelBtn });
  });

  function done(approved: boolean) {
    req?.answer(approved, approved && suppress);
  }
</script>

{#if req}
  <div class="overlay" role="presentation">
    <div
      class="dialog"
      role="dialog"
      aria-modal="true"
      aria-label={req.title}
      tabindex="-1"
      bind:this={dialogEl}
      data-testid="action-confirm"
      onkeydown={(e) => { if (e.key === "Escape") done(false); }}
    >
      <h2>{req.title}</h2>
      <!-- 出口标准①：命令全文。`pre-wrap` + `anywhere` 保证长命令换行而不是出横向滚动条
           （全局裁定：只允许纵向滚动）；user-select 让用户能把它复制走对照。 -->
      <p class="lead">将要执行：</p>
      <pre class="cmd" data-testid="action-confirm-command">{req.command}</pre>
      {#if req.note}
        <p class="note" data-testid="action-confirm-note">{req.note}</p>
      {/if}
      <label class="suppress">
        <input type="checkbox" bind:checked={suppress} data-testid="action-confirm-suppress" />
        以后不再对「{actionLabel(req.kind)}」显示此确认
      </label>
      <!-- 勾了之后能去哪儿反悔，得当场说，否则用户勾完就再也找不到那个开关（出口标准③）。 -->
      <p class="where">勾选后可在「工具 → 选项 → 安全与 Vault」里撤销。</p>
      <footer>
        <button bind:this={cancelBtn} onclick={() => done(false)} data-testid="action-confirm-cancel">取消</button>
        <button
          class="primary"
          class:danger={req.danger}
          onclick={() => done(true)}
          data-testid="action-confirm-ok">{req.confirmText ?? "确定"}</button>
      </footer>
    </div>
  </div>
{/if}

<style>
  .overlay { position: fixed; inset: 0; background: rgba(0,0,0,.5); display: grid; place-items: center; z-index: 65; }
  .dialog {
    width: 520px; max-width: 92vw; max-height: 92vh; overflow-y: auto;
    padding: 16px 20px; background: var(--fs-bg-elevated);
    border: 1px solid var(--fs-border-strong); border-radius: var(--fs-radius);
    box-shadow: var(--fs-shadow); color: var(--fs-fg-primary);
    display: flex; flex-direction: column; gap: 10px;
  }
  .dialog h2 { margin: 0; font-size: 14px; }
  .lead { margin: 0; font-size: 12px; color: var(--fs-fg-secondary); }
  .cmd {
    margin: 0; padding: 8px 10px; max-height: 40vh; overflow-y: auto;
    background: var(--fs-bg-app); border: 1px solid var(--fs-border); border-radius: var(--fs-radius);
    font-family: var(--fs-font-mono, monospace); font-size: 12px; line-height: 1.5;
    white-space: pre-wrap; overflow-wrap: anywhere; user-select: text;
  }
  .note { margin: 0; font-size: 12px; color: var(--fs-warn); white-space: pre-wrap; }
  .suppress { display: flex; align-items: center; gap: 6px; font-size: 12.5px; cursor: pointer; }
  .where { margin: 0; font-size: 11.5px; color: var(--fs-fg-secondary); }
  footer { display: flex; justify-content: flex-end; gap: 8px; }
  footer button {
    padding: 5px 14px; border: 1px solid var(--fs-border); background: var(--fs-bg-panel);
    color: var(--fs-fg-primary); border-radius: var(--fs-radius); cursor: pointer;
  }
  footer button.primary { background: var(--fs-accent); color: var(--fs-accent-fg); border-color: var(--fs-accent); }
  footer button.danger { background: var(--fs-danger); border-color: var(--fs-danger); color: #fff; }
  /* 高对比度（房规见 styles.css）：主/危险按钮的强调全靠底色，强制颜色下会与取消按钮无从分辨。 */
  @media (forced-colors: active) {
    footer button.primary, footer button.danger { outline: 2px solid Highlight; outline-offset: -2px; }
  }
</style>
