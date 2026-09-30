<script lang="ts">
  import { useFocusTrap } from "../lib/focusTrap";

  let {
    open = false,
    rows,
    profiles = [],
    onReconnect,
    onSkipAll,
  }: {
    open: boolean;
    profiles?: Array<{ id: string; name: string; host: string; port: number; protocol?: string; serial?: { port: string } }>;
    rows: Array<{ session_key: string; profile_id: string | null; updated_at: string }>;
    onReconnect(sessionKeys: string[]): void;
    onSkipAll(): void;
  } = $props();

  let dialogEl = $state<HTMLDivElement | undefined>();
  let skipBtn = $state<HTMLButtonElement | undefined>();
  let selected = $state<Set<string>>(new Set());

  // 默认全选
  $effect(() => {
    if (open) {
      selected = new Set(rows.map((r) => r.session_key));
    }
  });

  $effect(() => {
    if (open && dialogEl) {
      const cleanup = useFocusTrap(dialogEl, { initial: skipBtn ?? null }); // 默认焦点「全部跳过」
      return cleanup;
    }
  });

  function toggleSelection(sessionKey: string) {
    if (selected.has(sessionKey)) {
      selected.delete(sessionKey);
    } else {
      selected.add(sessionKey);
    }
    selected = selected; // trigger reactivity
  }

  function handleReconnect() {
    onReconnect(Array.from(selected));
  }

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === "Escape") {
      e.preventDefault();
      onSkipAll();
    }
  }

  // 格式化 SQLite UTC 时间戳为本地化呈现
  function formatUpdatedAt(utcStr: string): string {
    try {
      // SQLite datetime('now') 格式: YYYY-MM-DD HH:MM:SS (UTC)
      const dt = new Date(/(?:Z|[+-]\d{2}:?\d{2})$/.test(utcStr) ? utcStr : utcStr.replace(" ", "T") + "Z");
      return Number.isNaN(dt.getTime()) ? utcStr : dt.toLocaleString();
    } catch {
      return utcStr;
    }
  }
</script>

{#if open}
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div class="overlay" onclick={onSkipAll} onkeydown={handleKeydown}>
    <!-- svelte-ignore a11y_interactive_supports_focus -->
    <div
      class="dialog"
      role="dialog" aria-modal="true" tabindex="-1"
      aria-labelledby="recovery-title"
      bind:this={dialogEl}
      onclick={(e) => e.stopPropagation()}
      onkeydown={(e) => { handleKeydown(e); e.stopPropagation(); }}
    >
      <h2 id="recovery-title">恢复会话</h2>
      <p class="hint">检测到未正常关闭的会话，选择需要重连的会话：</p>

      <div class="session-table">
        <div class="table-header">
          <div class="col-check">
            <input
              type="checkbox" aria-label="全选会话"
              checked={selected.size === rows.length}
              indeterminate={selected.size > 0 && selected.size < rows.length}
              onchange={() => {
                if (selected.size === rows.length) {
                  selected = new Set();
                } else {
                  selected = new Set(rows.map((r) => r.session_key));
                }
              }}
            />
          </div>
          <div class="col-name">会话名</div>
          <div class="col-host">连接目标</div>
          <div class="col-time">上次活跃</div>
        </div>
        {#each rows as row}
          {@const profile = profiles.find(p => p.id === row.profile_id)}
          <div class="table-row">
            <div class="col-check">
              <input
                type="checkbox" aria-label={`恢复 ${profile?.name ?? "未命名会话"}`}
                checked={selected.has(row.session_key)}
                onchange={() => toggleSelection(row.session_key)}
              />
            </div>
            <div class="col-name">{profile?.name ?? "原会话配置不可用"}</div>
            <div class="col-host">{profile ? (profile.protocol === "serial" ? profile.serial?.port ?? "串口" : `${profile.host}:${profile.port}`) : "跳过此项或重新新建连接"}</div>
            <div class="col-time">{formatUpdatedAt(row.updated_at)}</div>
          </div>
        {/each}
      </div>

      <div class="actions">
        <button
          class="primary"
          disabled={selected.size === 0}
          onclick={handleReconnect}
        >
          重连所选 ({selected.size})
        </button>
        <button bind:this={skipBtn} onclick={onSkipAll}>全部跳过</button>
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
    min-width: min(600px, 92vw);
    max-width: min(800px, 92vw);
    max-height: 92vh;
    overflow-y: auto;
    padding: 20px;
  }
  h2 {
    margin: 0 0 8px;
    font-size: 16px;
    color: var(--fs-fg-primary);
  }
  .hint {
    margin: 0 0 16px;
    font-size: 13px;
    color: var(--fs-fg-secondary);
  }
  .session-table {
    border: 1px solid var(--fs-border);
    border-radius: 4px;
    overflow: hidden;
    margin-bottom: 16px;
    max-height: 400px;
    overflow-y: auto;
  }
  .table-header,
  .table-row {
    display: grid;
    grid-template-columns: 40px 2fr 2fr 1.5fr;
    align-items: center;
    padding: 8px 12px;
    gap: 12px;
    font-size: 13px;
  }
  .table-header {
    background: var(--fs-bg-panel);
    border-bottom: 1px solid var(--fs-border);
    font-weight: 600;
    color: var(--fs-fg-primary);
  }
  .table-row {
    border-bottom: 1px solid var(--fs-border);
    color: var(--fs-fg-secondary);
  }
  .table-row:last-child {
    border-bottom: none;
  }
  .table-row:hover {
    background: var(--fs-bg-hover);
  }
  .col-check {
    display: flex;
    align-items: center;
    justify-content: center;
  }
  .col-name {
    font-family: monospace;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .col-host,
  .col-time {
    font-size: 12px;
    color: var(--fs-fg-tertiary);
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
  button:hover:not(:disabled) {
    background: var(--fs-bg-hover);
  }
  button:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }
  button.primary {
    background: var(--fs-accent);
    border-color: var(--fs-accent);
    color: white;
  }
  button.primary:hover:not(:disabled) {
    opacity: 0.9;
  }
</style>
