<script lang="ts">
  import type { TermController } from "../lib/term";

  let { controller }: { controller: TermController } = $props();

  let open = $state(false);
  let query = $state("");
  let inputEl: HTMLInputElement | undefined = $state();
  /** S274（复审 #13）：无命中短暂警示——findNext/Previous 返回 false 时输入框加 .miss 样式 1.2s。
   *  规格未要求，属基础观感补齐：否则用户无从分辨查询词拼错还是确无匹配。 */
  let miss = $state(false);
  let missTimer: ReturnType<typeof setTimeout> | undefined;
  function flagMiss(): void {
    miss = true;
    clearTimeout(missTimer);
    missTimer = setTimeout(() => (miss = false), 1200);
  }
  function find(dir: "next" | "prev"): void {
    if (!query) return;
    const hit = dir === "next" ? controller.findNext(query) : controller.findPrevious(query);
    if (!hit) flagMiss();
  }

  /** 打开浮层并聚焦输入框（TerminalPane 经 onReady 转发；App 级 Ctrl+F 经 Task 17 shortcuts 派发器，R3） */
  export function openSearch(): void {
    open = true;
    requestAnimationFrame(() => { inputEl?.focus(); inputEl?.select(); });
  }

  /** F3「查找下一个」（R57）：沿用当前查询词续搜；查询词为空则打开浮层录入。
   *  TerminalPane 经 onReady 转发为 `api.findNext()`，由 Task 20 Step 4 ④ 的 edit.findNext 分支调用（UI §4/§8）。 */
  export function findNextAgain(): void {
    if (!query) { openSearch(); return; }
    if (!open) open = true;
    controller.findNext(query);
  }

  function close(): void {
    open = false;
    controller.clearSearch();
    controller.focus(); // 关闭还焦终端
  }

  function onKeydown(e: KeyboardEvent): void {
    if (e.key === "Enter" && !e.isComposing) {
      e.preventDefault();
      e.stopPropagation(); // S272/#21：勿冒泡至窗口级（共存粘贴气泡时一次 Enter 只翻匹配，不结算气泡）
      find(e.shiftKey ? "prev" : "next");
    } else if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation(); // 同上：与右键菜单共存时一次 Esc 只关浮层
      close();
    }
  }
</script>

{#if open}
  <div class="search-overlay" role="search">
    <input
      bind:this={inputEl}
      bind:value={query}
      onkeydown={onKeydown}
      class:miss
      placeholder="搜索（Enter 下一个 / Shift+Enter 上一个 / Esc 关闭）"
      aria-label="终端内搜索"
      data-testid="search-input"
    />
    <button onclick={() => find("prev")} title="上一个（Shift+Enter）">↑</button>
    <button onclick={() => find("next")} title="下一个（Enter）">↓</button>
    <button onclick={close} title="关闭（Esc）">✕</button>
  </div>
{/if}

<style>
  /* 绝对定位浮于终端右上：锚定 `.terminal-pane{position:relative}`（Step 5）与 Task 20 `.terminal{position:relative}` 容器 */
  .search-overlay { position: absolute; top: 8px; right: 16px; z-index: 10; display: flex; gap: 4px; padding: 4px 6px; background: var(--fs-bg-elevated); border: 1px solid var(--fs-border-strong); border-radius: var(--fs-radius); box-shadow: var(--fs-shadow); }
  .search-overlay input { width: 240px; background: var(--fs-bg-input); color: var(--fs-fg-primary); border: 1px solid var(--fs-border); border-radius: 3px; padding: 2px 6px; font: inherit; }
  /* S274：无命中警示（--fs-danger 令牌，1.2s 后自动撤除） */
  .search-overlay input.miss { border-color: var(--fs-danger); outline: 1px solid var(--fs-danger); }
</style>
