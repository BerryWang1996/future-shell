<script lang="ts">
  /**
   * ErrorDetailDialog.svelte — 长错误的完整呈现（2026-09-01 用户报「看不全」）。
   *
   * # 为什么需要它
   *
   * 认证类错误天生就长：`auth failed; tried: []; server allows: ["publickey"];
   * notes: [...]` 加一段中文指引，轻松两三百字。它此前有三个落点，**没有一个
   * 能读全**：
   * · 状态栏——24px 固定高、nowrap，右侧直接被窗口裁掉；
   * · toast——8 秒后消失，且同样不换行；
   * · 标签 title——原生 tooltip 在长文本上一样会截，且没法复制。
   *
   * 而这段文本恰恰是**用户唯一能拿去搜索/求助的东西**。读不全 = 排障断路。
   *
   * # 做法
   *
   * 可选中、可换行、等宽排版 + 一键复制。不做「人话化改写」——原文里的
   * `server allows: ["publickey"]` 对懂 SSH 的人是决定性信息，改写会把它磨掉；
   * 人话化那一层在产生错误的地方做（引擎的 notes 已经是中文指引）。
   */
  import { toast } from "../lib/toast";
  import { useFocusTrap } from "../lib/focusTrap";

  let {
    open = false,
    text = "",
    title = "连接失败详情",
    onClose = () => {},
  }: { open?: boolean; text?: string; title?: string; onClose?: () => void } = $props();

  let dialogEl = $state<HTMLDivElement | undefined>();
  $effect(() => {
    if (!open || !dialogEl) return;
    return useFocusTrap(dialogEl, { initial: dialogEl });
  });

  async function copy(): Promise<void> {
    try {
      await navigator.clipboard.writeText(text);
      toast.info("已复制完整错误");
    } catch (e) {
      toast.error(`复制失败：${e}`);
    }
  }
</script>

{#if open}
  <div class="overlay" role="presentation" onclick={onClose}>
    <div
      class="dialog"
      role="dialog"
      aria-modal="true"
      aria-label={title}
      tabindex="-1"
      bind:this={dialogEl}
      data-testid="error-detail-dialog"
      onclick={(e) => e.stopPropagation()}
      onkeydown={(e) => { if (e.key === "Escape") onClose(); }}
    >
      <h2>{title}</h2>
      <!-- 可选中 + 换行 + 等宽：这段文本的用途是被读、被搜、被贴进求助帖。 -->
      <pre class="body" data-testid="error-detail-text">{text}</pre>
      <div class="actions">
        <button type="button" class="ghost" data-testid="error-detail-copy" onclick={() => void copy()}>
          复制全文
        </button>
        <button type="button" class="primary" data-testid="error-detail-close" onclick={onClose}>关闭</button>
      </div>
    </div>
  </div>
{/if}

<style>
  .overlay { position: fixed; inset: 0; background: rgba(0, 0, 0, 0.5); display: grid; place-items: center; z-index: 60; }
  .dialog {
    background: var(--fs-bg-panel);
    border: 1px solid var(--fs-border-strong);
    border-radius: var(--fs-radius-lg);
    box-shadow: var(--fs-shadow);
    padding: 16px;
    width: min(680px, 92vw);
    max-height: 80vh;
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  h2 { margin: 0; font-size: 14px; color: var(--fs-fg-primary); }
  /* pre-wrap + break：长 URL/路径也要能折，否则又出现一条读不全的行
     （全局禁横向滚动，横着溢出 = 看不见）。 */
  .body {
    margin: 0;
    flex: 1 1 auto;
    overflow-y: auto;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    font-family: var(--fs-font-mono, monospace);
    font-size: 12px;
    line-height: 1.55;
    color: var(--fs-fg-primary);
    background: var(--fs-bg-input);
    border: 1px solid var(--fs-border);
    border-radius: var(--fs-radius);
    padding: 10px 12px;
    user-select: text;
  }
  .actions { display: flex; justify-content: flex-end; gap: 8px; }
  .ghost {
    background: var(--fs-bg-elevated); color: var(--fs-fg-primary);
    border: 1px solid var(--fs-border); border-radius: var(--fs-radius);
    padding: 4px 12px; cursor: pointer; font: inherit;
  }
  .ghost:hover { background: var(--fs-bg-hover); }
  .primary {
    background: var(--fs-accent); color: var(--fs-accent-fg);
    border: 1px solid var(--fs-accent); border-radius: var(--fs-radius);
    padding: 4px 12px; cursor: pointer; font: inherit;
  }
  .primary:hover { background: var(--fs-accent-hover); }
</style>
