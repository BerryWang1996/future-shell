<script lang="ts">
  /**
   * 粘贴时目标已存在，问用户怎么办（M7.3 出口标准①的「冲突」那一档）。
   *
   * 三档**无默认**：替用户默认「覆盖」会让一次误粘贴毁掉原文件，默认「跳过」又会让他以为
   * 复制成功了。三个按钮并列，各自说清后果，没有一个被预选或被样式暗示成「推荐」。
   *
   * 「对本次剩余全部采用」是一次性的（只作用于这一批），不写进设置——粘贴的冲突处置与
   * 「以后不再显示某类确认」不是一回事：前者取决于这一批文件是什么，后者是长期偏好。
   */
  import { useFocusTrap } from "../lib/focusTrap";
  import type { ConflictPolicy } from "../lib/file-clipboard";

  let {
    open = false,
    /** 冲突的名字（目标目录里已存在的那些）。 */
    names = [] as string[],
    dstDir = "",
    onPick = (_p: ConflictPolicy) => {},
    onCancel = () => {},
  }: {
    open?: boolean;
    names?: string[];
    dstDir?: string;
    onPick?(policy: ConflictPolicy): void;
    onCancel?(): void;
  } = $props();

  let dialogEl = $state<HTMLDivElement | undefined>();
  let cancelBtn = $state<HTMLButtonElement | undefined>();

  $effect(() => {
    if (!open || !dialogEl || !cancelBtn) return;
    return useFocusTrap(dialogEl, { initial: cancelBtn });
  });
</script>

{#if open}
  <div class="overlay" role="presentation">
    <div
      class="dialog" role="dialog" aria-modal="true" aria-label="目标已存在" tabindex="-1"
      bind:this={dialogEl} data-testid="paste-conflict"
      onkeydown={(e) => { if (e.key === "Escape") onCancel(); }}
    >
      <h2>目标已存在</h2>
      <p class="lead">
        <code>{dstDir}</code> 里已经有 {names.length} 个同名条目：
      </p>
      <ul class="names" data-testid="paste-conflict-names">
        {#each names.slice(0, 20) as n (n)}<li>{n}</li>{/each}
      </ul>
      {#if names.length > 20}
        <p class="lead">…以及另外 {names.length - 20} 个</p>
      {/if}
      <p class="lead">这一批冲突的条目怎么处理？</p>
      <div class="choices">
        <button data-testid="paste-overwrite" onclick={() => onPick("overwrite")}>
          <strong>覆盖</strong><span>用新的替换掉已有的，原文件不可恢复</span>
        </button>
        <button data-testid="paste-keep-both" onclick={() => onPick("keep_both")}>
          <strong>保留两者</strong><span>新的自动改名（如 <code>a (2).txt</code>），已有的不动</span>
        </button>
        <button data-testid="paste-skip" onclick={() => onPick("skip")}>
          <strong>跳过</strong><span>冲突的这些不粘贴，其余照常</span>
        </button>
      </div>
      <footer>
        <button bind:this={cancelBtn} onclick={onCancel} data-testid="paste-conflict-cancel">
          取消整次粘贴
        </button>
      </footer>
    </div>
  </div>
{/if}

<style>
  .overlay { position: fixed; inset: 0; background: rgba(0,0,0,.5); display: grid; place-items: center; z-index: 66; }
  .dialog {
    width: 520px; max-width: 92vw; max-height: 92vh; overflow-y: auto;
    padding: 16px 20px; background: var(--fs-bg-elevated);
    border: 1px solid var(--fs-border-strong); border-radius: var(--fs-radius);
    box-shadow: var(--fs-shadow); color: var(--fs-fg-primary);
    display: flex; flex-direction: column; gap: 10px;
  }
  .dialog h2 { margin: 0; font-size: 14px; }
  .lead { margin: 0; font-size: 12.5px; color: var(--fs-fg-secondary); }
  .lead code { font-family: var(--fs-font-mono, monospace); color: var(--fs-fg-primary); }
  .names { margin: 0; padding-left: 18px; max-height: 26vh; overflow-y: auto; font-size: 12px; font-family: var(--fs-font-mono, monospace); }
  .choices { display: flex; flex-direction: column; gap: 6px; }
  /* 三档并列、样式一致：把其中一个做成主按钮等于替用户推荐一个不可撤销的选择。 */
  .choices button {
    display: flex; flex-direction: column; align-items: flex-start; gap: 2px;
    padding: 8px 10px; text-align: left; cursor: pointer;
    background: var(--fs-bg-panel); color: var(--fs-fg-primary);
    border: 1px solid var(--fs-border); border-radius: var(--fs-radius);
  }
  .choices button:hover { background: var(--fs-bg-hover); }
  .choices strong { font-size: 12.5px; }
  .choices span { font-size: 11.5px; color: var(--fs-fg-secondary); }
  footer { display: flex; justify-content: flex-end; }
  footer button { padding: 5px 14px; border: 1px solid var(--fs-border); background: var(--fs-bg-panel); color: var(--fs-fg-primary); border-radius: var(--fs-radius); cursor: pointer; }
</style>
