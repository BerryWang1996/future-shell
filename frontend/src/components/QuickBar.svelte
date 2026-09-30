<script lang="ts">
  /**
   * 快速命令按钮条（M4a，路线图 §M4 / UI 规格 §2.2 ⚡）。
   *
   * 按分类分段渲染片段库；点击即经 onSend 下发（App 决定发给谁——当前实现发
   * 活动会话，与 Xshell 快速命令一致）。带占位符的片段先弹参数填写行，**填完
   * 才发**：占位符不填就发等于把 `systemctl restart {{svc}}` 原文敲进 shell。
   */
  import { groupByCategory, parsePlaceholders, type QuickCommand } from "../lib/quick-commands";

  let {
    items = [],
    onSend,
    onManage,
  }: {
    items: QuickCommand[];
    /** 参数已替换完成的最终命令（App 负责编码与下发） */
    onSend(command: string): void;
    onManage?(): void;
  } = $props();

  const groups = $derived(groupByCategory(items));

  /** 待填参数的片段与其参数值。非 null 即渲染参数行。 */
  let pending = $state<{ snippet: QuickCommand; names: string[]; values: Record<string, string> } | null>(null);

  function clickSnippet(s: QuickCommand) {
    const names = parsePlaceholders(s.command);
    if (names.length === 0) {
      onSend(s.command);
      return;
    }
    pending = { snippet: s, names, values: {} };
  }

  function sendPending() {
    const p = pending;
    if (!p) return;
    // fillPlaceholders 对未填的占位符原样保留（S309）——这里在发送前拦一道：
    // 有空参数就不发，提示填全。静默发 {{svc}} 字面量比不发送危险。
    const missing = p.names.some((n) => !(p.values[n] ?? "").trim());
    if (missing) {
      // 不 toast：参数行就在眼前，输入框标红比全局 toast 更指向现场
      return;
    }
    onSend(cmd(p));
    pending = null;
  }

  /** 即时预览替换结果（参数行的「将发送：」文案来源，所见即所发）。 */
  function cmd(p: { snippet: QuickCommand; values: Record<string, string> }): string {
    return p.snippet.command.replace(/\{\{([^{}]+)\}\}/g, (whole, name: string) => {
      const key = name.trim();
      return key in p.values ? p.values[key] : whole;
    });
  }
</script>

<div class="quickbar" role="region" aria-label="快速命令" data-testid="quickbar">
  {#each groups as g (g.category || "__none__")}
    {#if g.category}<span class="qcat" title={g.category}>{g.category}</span>{/if}
    {#each g.items as s (s.id)}
      <button
        class="qbtn"
        class:active={pending?.snippet.id === s.id}
        title={s.command}
        onclick={() => clickSnippet(s)}
      >{s.name}</button>
    {/each}
    <span class="qsep" aria-hidden="true"></span>
  {/each}
  {#if items.length === 0}
    <span class="qempty">快速命令集为空</span>
  {/if}
  {#if onManage}
    <button class="qmanage" onclick={() => { pending = null; onManage(); }} title="管理快速命令集">⚙ 管理</button>
  {/if}

  {#if pending}
    {@const p = pending}
    <div class="params" data-testid="quick-params">
      <span class="ptitle">「{p.snippet.name}」参数：</span>
      {#each p.names as n (n)}
        <label class="pfield">
          <span class="pname">{n}</span>
          <input
            class="pinput"
            class:missing={!(p.values[n] ?? "").trim()}
            data-param={n}
            value={p.values[n] ?? ""}
            oninput={(e) => { p.values[n] = e.currentTarget.value; }}
            onkeydown={(e) => {
              if (e.key === "Enter" && !e.isComposing) { e.preventDefault(); sendPending(); }
              if (e.key === "Escape") { e.preventDefault(); pending = null; }
            }}
          />
        </label>
      {/each}
      <span class="ppreview" title={cmd(p)}>将发送：{cmd(p)}</span>
      <button class="psend" class:disabled={p.names.some((n) => !(p.values[n] ?? "").trim())} onclick={sendPending}>发送 ⏎</button>
      <button class="pcancel" onclick={() => (pending = null)}>取消</button>
    </div>
  {/if}
</div>

<style>
  .quickbar { display: flex; align-items: center; flex-wrap: wrap; gap: 4px; padding: 4px 8px; background: var(--fs-bg-panel); border-top: 1px solid var(--fs-border); flex: none; position: relative; }
  .qcat { color: var(--fs-fg-secondary); font-size: 12px; padding: 0 2px; max-width: 96px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .qbtn { background: var(--fs-bg-input); color: var(--fs-fg-primary); border: 1px solid var(--fs-border); border-radius: var(--fs-radius); padding: 2px 10px; cursor: pointer; font: inherit; font-size: 12px; white-space: nowrap; }
  .qbtn:hover { background: var(--fs-bg-hover); }
  .qbtn.active { border-color: var(--fs-accent); }
  .qsep { width: 1px; height: 16px; background: var(--fs-border); margin: 0 4px; }
  .qsep:last-of-type { display: none; }
  .qempty { color: var(--fs-fg-secondary); font-size: 12px; }
  .qmanage { margin-left: auto; background: none; border: none; color: var(--fs-fg-secondary); cursor: pointer; font: inherit; font-size: 12px; }
  .qmanage:hover { color: var(--fs-fg-primary); }
  /* 参数行：占位符填写（Enter 发送 / Esc 取消 / 空参数标红不发） */
  .params { display: flex; align-items: center; flex-wrap: wrap; gap: 6px; width: 100%; padding-top: 4px; border-top: 1px dashed var(--fs-border); }
  .ptitle, .ppreview { color: var(--fs-fg-secondary); font-size: 12px; }
  .ppreview { max-width: 360px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-family: var(--fs-mono, monospace); }
  .pfield { display: flex; align-items: center; gap: 4px; }
  .pname { color: var(--fs-fg-primary); font-size: 12px; }
  .pinput { width: 110px; background: var(--fs-bg-input); color: var(--fs-fg-primary); border: 1px solid var(--fs-border); border-radius: 3px; padding: 2px 6px; font: inherit; font-size: 12px; }
  .pinput.missing { border-color: var(--fs-danger, #e5484d); }
  .psend { background: var(--fs-accent); color: var(--fs-accent-fg); border: none; border-radius: var(--fs-radius); padding: 2px 10px; cursor: pointer; font: inherit; font-size: 12px; }
  .psend.disabled { opacity: 0.5; cursor: default; }
  .pcancel { background: none; border: none; color: var(--fs-fg-secondary); cursor: pointer; font: inherit; font-size: 12px; }
</style>
