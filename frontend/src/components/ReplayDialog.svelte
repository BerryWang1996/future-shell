<script lang="ts">
  /**
   * 会话回放面板（M4a）：列出 recordings 目录、选一个在 xterm 里按原始时序回放。
   *
   * 回放语义：终端状态是累积的，「快进/回退」一律 = 从头无延迟重喂到目标处
   * （见 lib/record.ts 的 countUpTo）——没有「倒放」这种东西，也不假装有。
   * 变速只缩放延迟，不丢事件；max 速度即「全部立即倒出」。
   */
  import { onDestroy } from "svelte";
  import { invoke } from "../lib/ipc";
  import { toast } from "../lib/toast";
  import { useFocusTrap } from "../lib/focusTrap";
  import ConfirmDialog from "./ConfirmDialog.svelte";
  import {
    countUpTo,
    delays,
    durationSecs,
    formatBytes,
    formatDuration,
    shouldWarnPrivacy,
    type CastContent,
    type CastEvent,
    type CastFile,
  } from "../lib/record";
  import { Terminal } from "@xterm/xterm";
  import "@xterm/xterm/css/xterm.css";

  let { open = false, onClose }: { open?: boolean; onClose(): void } = $props();

  let files = $state<CastFile[]>([]);
  let err = $state("");
  let selected = $state<CastFile | null>(null);
  let content = $state<CastContent | null>(null);
  let playing = $state(false);
  let speed = $state(1);
  /** 当前已应用到终端的事件下标 */
  let cursor = $state(0);
  let dialogEl = $state<HTMLDivElement | undefined>();
  let termHost = $state<HTMLDivElement | undefined>();
  let term: Terminal | null = null;
  let timer: ReturnType<typeof setTimeout> | null = null;
  let confirmPrivacy = $state(false);
  let pendingFile = $state<CastFile | null>(null);

  const events = $derived(content?.events ?? []);
  const total = $derived(durationSecs(events));
  const posSecs = $derived(
    events.length > 0 && cursor > 0 ? events[Math.min(cursor, events.length) - 1].t : 0,
  );

  async function load() {
    try {
      files = await invoke<CastFile[]>("recordings_list");
      err = "";
    } catch (e) {
      err = String(e);
      files = [];
    }
  }

  $effect(() => {
    if (!open) return;
    void load();
    stop();
    selected = null;
    content = null;
  });

  $effect(() => {
    if (!open || !dialogEl) return;
    return useFocusTrap(dialogEl, { initial: null });
  });

  // xterm 实例随选中文件创建；尺寸事件（r）在喂事件时按头/标记复原
  $effect(() => {
    const host = termHost;
    const c = content;
    if (!host || !c) return;
    term?.dispose();
    term = new Terminal({
      cols: c.header.width,
      rows: c.header.height,
      disableStdin: true, // 回放终端不接受输入
      convertEol: false,
      scrollback: 5000,
      fontSize: 12,
    });
    term.open(host);
    cursor = 0;
    return () => {
      term?.dispose();
      term = null;
    };
  });

  function applyEvent(e: CastEvent) {
    if (!term) return;
    if (e.kind === "o") {
      term.write(e.data);
    } else if (e.kind === "r") {
      const m = e.data.match(/^(\d+)x(\d+)$/);
      if (m) term.resize(Number(m[1]), Number(m[2]));
    }
  }

  function stop() {
    if (timer !== null) {
      clearTimeout(timer);
      timer = null;
    }
    playing = false;
  }

  function scheduleNext() {
    if (timer !== null) clearTimeout(timer);
    if (cursor >= events.length) {
      playing = false;
      return;
    }
    const d = delays(events)[cursor] ?? 0;
    timer = setTimeout(() => {
      applyEvent(events[cursor]);
      cursor += 1;
      scheduleNext();
    }, Math.max(0, d / speed));
  }

  function play() {
    if (events.length === 0) return;
    playing = true;
    if (cursor >= events.length) {
      // 播完了再按播放 = 从头再来（终端重置）
      term?.reset();
      cursor = 0;
    }
    scheduleNext();
  }

  /** 拖动到某秒：从 0 无延迟重喂到该处（快进与回退同一条路） */
  function scrub(secs: number) {
    stop();
    term?.reset();
    const upto = countUpTo(events, secs);
    for (let i = 0; i < upto; i++) applyEvent(events[i]);
    cursor = upto;
  }

  function pick(f: CastFile) {
    if (shouldWarnPrivacy(f.bytes)) {
      pendingFile = f;
      confirmPrivacy = true;
      return;
    }
    void openFile(f);
  }

  async function openFile(f: CastFile) {
    pendingFile = null;
    confirmPrivacy = false;
    try {
      content = await invoke<CastContent>("recording_read_events", { path: f.path });
      selected = f;
    } catch (e) {
      toast.error(`打开录屏失败：${e}`);
    }
  }

  onDestroy(stop);
</script>

{#if open}
  <div class="overlay" role="presentation" onclick={onClose}>
    <div
      class="dialog" role="dialog" aria-modal="true" aria-label="会话回放" tabindex="-1"
      data-testid="replay-dialog" bind:this={dialogEl}
      onclick={(e) => e.stopPropagation()}
      onkeydown={(e) => { if (e.key === "Escape") onClose(); }}
    >
      <header>
        <h3>会话回放</h3>
        <button class="x" onclick={onClose} data-testid="replay-close" aria-label="关闭">×</button>
      </header>

      {#if err}<p class="err" role="alert" data-testid="replay-error">{err}</p>{/if}

      <div class="body">
        <ul class="list" data-testid="replay-list">
          {#if files.length === 0}
            <li class="empty">还没有录屏文件。在会话标签上右键 →「开始录制」。</li>
          {:else}
            {#each files as f (f.path)}
              <li>
                <button class:sel={selected?.path === f.path} onclick={() => pick(f)}
                        data-testid="replay-pick" title={f.path}>
                  <span class="name">{f.name}</span>
                  <span class="meta">{formatBytes(f.bytes)} · {new Date(f.modified * 1000).toLocaleString()}</span>
                </button>
              </li>
            {/each}
          {/if}
        </ul>

        <div class="player">
          {#if content}
            <div class="term" bind:this={termHost} data-testid="replay-term"></div>
            <div class="controls">
              <button onclick={playing ? stop : play} data-testid="replay-play">
                {playing ? "⏸ 暂停" : cursor >= events.length ? "↻ 重放" : "▶ 播放"}
              </button>
              <label>速度
                <select bind:value={speed} data-testid="replay-speed">
                  <option value={0.5}>0.5×</option>
                  <option value={1}>1×</option>
                  <option value={2}>2×</option>
                  <option value={4}>4×</option>
                  <option value={9999}>最大</option>
                </select>
              </label>
              <input
                type="range" min="0" max={Math.max(total, 0.001)} step="0.1"
                value={posSecs} data-testid="replay-seek"
                oninput={(e) => scrub(Number((e.target as HTMLInputElement).value))}
                aria-label="回放进度"
              />
              <span class="time" data-testid="replay-time">
                {formatDuration(posSecs)} / {formatDuration(total)}
              </span>
            </div>
            <p class="hint">
              回放忠实重现原始时序与颜色；拖动进度会从头快速重放到该处（终端状态是累积的，
              没有真正的「倒放」）。
            </p>
          {:else}
            <p class="empty" data-testid="replay-empty">从左侧选择一个录屏文件。</p>
          {/if}
        </div>
      </div>
    </div>
  </div>
{/if}

<ConfirmDialog
  open={confirmPrivacy}
  title="打开录屏"
  message={"录屏文件包含会话输出的明文（命令、参数与回显，颜色码原样保留）。\n\n" +
    (pendingFile ? `文件：${pendingFile.name}（${formatBytes(pendingFile.bytes)}）\n\n` : "") +
    "如这台机器共用，请注意播放时屏幕上的内容。"}
  confirmText="打开"
  onConfirm={() => { if (pendingFile) void openFile(pendingFile); }}
  onCancel={() => { confirmPrivacy = false; pendingFile = null; }}
/>

<style>
  .overlay { position: fixed; inset: 0; background: rgba(0,0,0,.5); display: grid; place-items: center; z-index: 60; }
  .dialog { width: 900px; max-width: 95vw; height: 600px; max-height: 88vh; display: flex; flex-direction: column;
            padding: 14px 18px; background: var(--fs-bg-elevated); border: 1px solid var(--fs-border);
            border-radius: 6px; color: var(--fs-fg-primary); font-size: 13px; }
  header { display: flex; align-items: center; justify-content: space-between; margin-bottom: 8px; }
  h3 { margin: 0; font-size: 14px; }
  .x { border: none; background: none; color: var(--fs-fg-secondary); font-size: 16px; cursor: pointer; }
  .body { flex: 1; display: flex; gap: 12px; min-height: 0; }
  .list { list-style: none; margin: 0; padding: 0; width: 280px; overflow-y: auto; }
  .list button { width: 100%; text-align: left; background: none; border: 1px solid transparent;
                 border-radius: 4px; padding: 5px 8px; cursor: pointer; color: var(--fs-fg-primary);
                 display: flex; flex-direction: column; gap: 2px; font: inherit; }
  .list button:hover { background: var(--fs-bg-hover); }
  .list button.sel { border-color: var(--fs-accent); }
  .name { word-break: break-all; }
  .meta { color: var(--fs-fg-secondary); font-size: 11px; }
  .player { flex: 1; display: flex; flex-direction: column; min-width: 0; }
  .term { flex: 1; min-height: 0; background: #000; border-radius: 4px; padding: 4px; overflow: hidden; }
  .controls { display: flex; align-items: center; gap: 10px; margin-top: 8px; }
  .controls button { padding: 4px 12px; border: 1px solid var(--fs-border); background: var(--fs-bg-panel);
                     color: var(--fs-fg-primary); border-radius: 4px; cursor: pointer; }
  .controls label { display: inline-flex; align-items: center; gap: 4px; color: var(--fs-fg-secondary); font-size: 12px; }
  .controls select { background: var(--fs-bg-input); color: var(--fs-fg-primary);
                     border: 1px solid var(--fs-border); border-radius: 4px; }
  .controls select { padding-right: 22px; } /* 箭头让位（2026-09-01） */
  .controls input[type="range"] { flex: 1; }
  .time { font-variant-numeric: tabular-nums; color: var(--fs-fg-secondary); white-space: nowrap; }
  .empty, .hint { color: var(--fs-fg-secondary); }
  .hint { font-size: 11px; margin: 6px 0 0; }
  .err { color: var(--fs-danger); margin: 0 0 6px; }
</style>
