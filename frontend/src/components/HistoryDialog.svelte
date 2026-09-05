<script lang="ts">
  /**
   * 历史命令面板（M4a）：检索本地历史库 + 一键重发到当前会话。
   *
   * 两个数据源在同一张表里，靠 `source` 列区分，**UI 必须如实标注**：
   * - `sent`（「发出」）= 本程序亲手发出的字节，重发是字节精确的；
   * - `grid`（「屏幕」）= 从终端回滚里启发式提取，可能带提示符残渣。
   *
   * 混在一起不标注的后果是真事故：用户以为在重发一条命令，实际发出去的是一段被
   * 误切的输出。所以 `grid` 条目带明显标记，且不参与「回车直接重发」这条快路径。
   */
  import { invoke } from "../lib/ipc";
  import { toast } from "../lib/toast";
  import { useFocusTrap } from "../lib/focusTrap";
  import ConfirmDialog from "./ConfirmDialog.svelte";
  import { formatUsedAt, type HistoryEntry } from "../lib/history";

  let {
    open = false,
    sessionId = null,
    /** 当前会话的主机名；非空且「仅本机」勾选时只检索该主机的历史 */
    host = "",
    onClose,
    onSend,
  }: {
    open?: boolean;
    sessionId?: string | null;
    host?: string;
    onClose(): void;
    /** 重发。由 App 统一走 term_input（这里不直接发字节，避免两条发送路径分叉） */
    onSend(command: string): void;
  } = $props();

  let query = $state("");
  let onlyThisHost = $state(false);
  let rows = $state<HistoryEntry[]>([]);
  let loading = $state(false);
  let err = $state("");
  let dialogEl = $state<HTMLDivElement | undefined>();
  let inputEl = $state<HTMLInputElement | undefined>();
  let confirmClear = $state(false);
  /** 键盘选中项下标（-1 = 未选） */
  let cursor = $state(-1);

  async function search() {
    loading = true;
    err = "";
    try {
      rows = await invoke<HistoryEntry[]>("history_search", {
        query,
        host: onlyThisHost && host ? host : null,
      });
      cursor = rows.length > 0 ? 0 : -1;
    } catch (e) {
      err = String(e);
      rows = [];
    } finally {
      loading = false;
    }
  }

  // 打开即检索一次；改查询/改范围也重查。
  // 不做防抖：检索走的是本地 SQLite 的一次 LIKE，几千条量级是微秒级；加防抖只会
  // 让「边打边看」有滞后感。
  $effect(() => {
    if (!open) return;
    void search();
  });

  $effect(() => {
    if (!open || !dialogEl) return;
    return useFocusTrap(dialogEl, { initial: inputEl ?? null });
  });

  async function scanGrid() {
    if (!sessionId) {
      toast.warn("没有活动会话，无法从屏幕提取");
      return;
    }
    try {
      const n = await invoke<number>("history_scan_grid", { sessionId, maxLines: 5000 });
      toast.info(n > 0 ? `已从屏幕提取 ${n} 条候选命令` : "屏幕上没有可提取的命令行");
      await search();
    } catch (e) {
      toast.error(`提取失败：${e}`);
    }
  }

  function send(e: HistoryEntry) {
    if (!sessionId) {
      toast.warn("没有活动会话，命令未发送");
      return;
    }
    onSend(e.command);
    onClose();
  }

  async function remove(e: HistoryEntry) {
    try {
      await invoke("history_delete", { id: e.id });
      rows = rows.filter((r) => r.id !== e.id);
    } catch (err2) {
      toast.error(`删除失败：${err2}`);
    }
  }

  async function clearAll() {
    confirmClear = false;
    try {
      await invoke("history_clear");
      rows = [];
      toast.info("命令历史已清空");
    } catch (e) {
      toast.error(`清空失败：${e}`);
    }
  }

  function onKey(e: KeyboardEvent) {
    if (e.key === "Escape") {
      onClose();
      return;
    }
    if (e.key === "ArrowDown") {
      e.preventDefault();
      cursor = Math.min(cursor + 1, rows.length - 1);
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      cursor = Math.max(cursor - 1, 0);
    } else if (e.key === "Enter" && !e.isComposing && cursor >= 0 && rows[cursor]) {
      e.preventDefault();
      const row = rows[cursor];
      // 屏幕提取的条目不走「回车直接发」：它可能带提示符残渣，必须让人看着点一下
      if (row.source === "grid") {
        toast.warn("该条来自屏幕提取，可能不准，请确认后点「重发」");
        return;
      }
      send(row);
    }
  }
</script>

{#if open}
  <div class="overlay" role="presentation" onclick={onClose}>
    <div
      class="dialog" role="dialog" aria-modal="true" aria-label="历史命令" tabindex="-1"
      data-testid="history-dialog" bind:this={dialogEl}
      onclick={(e) => e.stopPropagation()} onkeydown={onKey}
    >
      <header>
        <h3>历史命令</h3>
        <button class="x" onclick={onClose} data-testid="hist-close" aria-label="关闭">×</button>
      </header>

      <div class="bar">
        <input
          bind:this={inputEl} bind:value={query} type="search"
          placeholder="搜索命令（留空看全部，按最近使用排序）"
          data-testid="hist-query" aria-label="搜索历史命令"
        />
        <label title={host ? `只看 ${host}` : "当前无会话主机"}>
          <input type="checkbox" bind:checked={onlyThisHost} disabled={!host} data-testid="hist-only-host" />
          仅本机
        </label>
        <button onclick={() => void scanGrid()} data-testid="hist-scan" title="从当前终端回滚里启发式提取候选命令">
          从屏幕提取
        </button>
        <button onclick={() => (confirmClear = true)} data-testid="hist-clear" title="命令行里可能含口令/令牌">
          清空
        </button>
      </div>

      {#if err}
        <p class="err" role="alert" data-testid="hist-error">{err}</p>
      {:else if loading && rows.length === 0}
        <p class="empty">检索中…</p>
      {:else if rows.length === 0}
        <p class="empty" data-testid="hist-empty">
          {query.trim() ? "没有匹配的历史命令" : "历史还是空的——发过的命令会自动记下来，也可以点「从屏幕提取」"}
        </p>
      {:else}
        <ul data-testid="hist-list">
          {#each rows as r, i (r.id)}
            <li class:sel={i === cursor}>
              <button class="row" onclick={() => send(r)} data-testid="hist-send-{r.id}" title="重发到当前会话">
                <code>{r.command}</code>
                <span class="meta">
                  <span class="src" class:approx={r.source === "grid"}
                        title={r.source === "grid" ? "从屏幕启发式提取，可能带提示符残渣" : "本程序亲手发出，字节精确"}>
                    {r.source === "grid" ? "屏幕" : "发出"}
                  </span>
                  {#if r.host}<span class="host">{r.host}</span>{/if}
                  <span class="cnt">×{r.use_count}</span>
                  <span class="when">{formatUsedAt(r.used_at)}</span>
                </span>
              </button>
              <button class="del" onclick={() => void remove(r)} data-testid="hist-del-{r.id}" aria-label="删除该条">🗑</button>
            </li>
          {/each}
        </ul>
      {/if}
      <p class="hint">↑↓ 选择，回车重发（「屏幕」来源需手动点击确认）。共 {rows.length} 条。</p>
    </div>
  </div>
{/if}

<ConfirmDialog
  open={confirmClear}
  title="清空命令历史"
  message={"将删除全部历史命令记录，不可撤销。\n\n命令行里可能含一次性口令、令牌或私有主机名——清空也是一次隐私清理。"}
  confirmText="清空"
  danger
  onConfirm={() => void clearAll()}
  onCancel={() => (confirmClear = false)}
/>

<style>
  .overlay { position: fixed; inset: 0; background: rgba(0,0,0,.5); display: grid; place-items: center; z-index: 60; }
  .dialog { width: 720px; max-width: 94vw; max-height: 80vh; display: flex; flex-direction: column;
            padding: 14px 18px; background: var(--fs-bg-elevated); border: 1px solid var(--fs-border);
            border-radius: 6px; color: var(--fs-fg-primary); font-size: 13px; }
  header { display: flex; align-items: center; justify-content: space-between; margin-bottom: 8px; }
  h3 { margin: 0; font-size: 14px; }
  .x { border: none; background: none; color: var(--fs-fg-secondary); font-size: 16px; cursor: pointer; }
  .bar { display: flex; align-items: center; gap: 8px; margin-bottom: 8px; }
  .bar input[type="search"] { flex: 1; background: var(--fs-bg-input); color: var(--fs-fg-primary);
                              border: 1px solid var(--fs-border); border-radius: 4px; padding: 4px 8px; font: inherit; }
  .bar label { display: inline-flex; align-items: center; gap: 4px; color: var(--fs-fg-secondary); white-space: nowrap; }
  .bar button { padding: 4px 10px; border: 1px solid var(--fs-border); background: var(--fs-bg-panel);
                color: var(--fs-fg-primary); border-radius: 4px; cursor: pointer; white-space: nowrap; }
  .empty, .err { color: var(--fs-fg-secondary); margin: 8px 0; }
  .err { color: var(--fs-danger); }
  ul { list-style: none; margin: 0; padding: 0; overflow-y: auto; flex: 1; }
  li { display: flex; align-items: stretch; gap: 4px; border-bottom: 1px solid var(--fs-border); }
  li.sel { background: var(--fs-bg-hover); }
  button.row { flex: 1; display: flex; flex-direction: column; gap: 2px; align-items: flex-start;
               background: none; border: 0; color: var(--fs-fg-primary); text-align: left;
               padding: 5px 4px; cursor: pointer; font: inherit; min-width: 0; }
  button.row:hover { background: var(--fs-bg-hover); }
  code { font-family: var(--fs-font-mono, monospace); white-space: pre-wrap; word-break: break-all; }
  .meta { display: flex; gap: 8px; color: var(--fs-fg-secondary); font-size: 11px; }
  .src { border: 1px solid var(--fs-border); border-radius: 3px; padding: 0 4px; }
  .src.approx { color: var(--fs-warn); border-color: currentColor; }
  .del { border: 0; background: none; color: var(--fs-fg-secondary); cursor: pointer; padding: 0 6px; }
  .del:hover { color: var(--fs-danger); }
  .hint { margin: 6px 0 0; color: var(--fs-fg-secondary); font-size: 11px; }
</style>
