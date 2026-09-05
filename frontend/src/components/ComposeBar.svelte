<script lang="ts">
  import { useFocusTrap } from "../lib/focusTrap";
  import { historyNext, historyPrev, historyPush, type ComposeTarget } from "../lib/compose-history";
  import { composeTarget, pickedSessions } from "../lib/broadcast";

  let {
    /** 发送命令；target 为广播目标（M4a：四态实装）；suffix 为附加换行（"\r"|"\n"|"\r\n"|""） */
    onSend,
    onSent,
    /** 手选目标可勾的已开会话（M4a）：[{id, title}]，App 从 tabs 供给 */
    openSessions = [],
    /**
     * 外部填入（M2「AI 建议 → 填进命令栏」）。
     *
     * 带 `seq` 而不是只给一个字符串：同一条命令用户可能连点两次「填进命令栏」，
     * 纯字符串的话第二次值没变、`$effect` 不会重跑，看上去就是按钮坏了。
     * `seq` 由调用方每次递增，于是「填了一次」这件事本身是可观测的。
     *
     * **只填不发**。发送要用户自己按回车——AI 给的命令替用户按下回车，
     * 是这个功能里最不该出现的一步。
     */
    fill = null,
  }: {
    onSend(target: ComposeTarget, text: string, suffix: string): void;
    /** 发送后回调（用于把焦点交还终端，§2.6） */
    onSent?(): void;
    openSessions: { id: string; title: string }[];
    fill?: { text: string; seq: number } | null;
  } = $props();

  // 目标选择放共享 store（S306）：组合栏与实时键入广播（ToolBar 📢）必须同源——
  // 两处各持一份状态，「实时的目标」与「组合栏的目标」迟早分叉，而广播发错
  // 目标集合的后果（把命令发进生产机组）比不发严重。
  const target = $derived($composeTarget);
  let text = $state("");
  let suffix = $state("\r");
  let showPicker = $state(false);
  let input: HTMLInputElement;

  /**
   * 接住外部填入。
   *
   * 记 seq 而不是记文本：记文本就退化成「值变了才生效」，那正是 seq 要解决的问题。
   * 填完把焦点放到输入框并选中全文——用户下一步要么直接回车，要么改两个字再回车，
   * 两种都从「光标在框里」开始。选中全文是为了后者：AI 给的命令常常只有一处要改，
   * 但也常常整条都不想要。
   */
  let lastFillSeq = $state(-1);
  $effect(() => {
    if (!fill || fill.seq === lastFillSeq) return;
    lastFillSeq = fill.seq;
    text = fill.text;
    input?.focus();
    input?.select();
  });

  function send(withSuffix: boolean) {
    const cmd = text;
    if (!cmd.trim()) return;
    historyPush(target, cmd);
    onSend(target, cmd, withSuffix ? suffix : "");
    text = "";
    onSent?.();
  }

  function onKeydown(e: KeyboardEvent) {
    if (e.key === "Enter" && !e.isComposing) {
      e.preventDefault();
      send(!e.ctrlKey); // Ctrl+Enter 不附加换行（§2.6）
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      const prev = historyPrev(target);
      if (prev !== null) text = prev;
    } else if (e.key === "ArrowDown") {
      e.preventDefault();
      text = historyNext(target) ?? "";
    }
  }

  /** 目标切换出手选面板即收起（S307）：面板只属于 pick 态，悬空开着会让人
   * 误以为「全部/分组」也受勾选影响——那是最危险的一种歧义。 */
  function onTargetChange(e: Event) {
    const v = (e.currentTarget as HTMLSelectElement).value as ComposeTarget;
    composeTarget.set(v);
    if (v !== "pick") showPicker = false;
  }

  function togglePicked(id: string, checked: boolean) {
    pickedSessions.update((list) =>
      checked ? [...list, id] : list.filter((x) => x !== id),
    );
  }
  let pickerEl = $state<HTMLDivElement | null>(null);
  $effect(() => {
    if (showPicker && pickerEl) return useFocusTrap(pickerEl, { initial: pickerEl.querySelector<HTMLElement>("input, button") });
  });
</script>

<div class="compose" role="region" aria-label="组合命令栏" data-testid="composebar">
  <div class="targetbox">
    <select
      class="target"
      aria-label="发送目标"
      value={target}
      onchange={onTargetChange}
      title="发送目标：当前 / 全部已连接 / 当前分组 / 手选"
    >
      <option value="current">目标: 当前会话</option>
      <option value="all">全部会话</option>
      <option value="group">当前分组</option>
      <option value="pick">手选…</option>
    </select>
    {#if target === "pick"}
      <button
        class="pickerbtn"
        aria-expanded={showPicker}
        aria-label="手选目标会话"
        title="勾选要发送到的会话"
        onclick={() => (showPicker = !showPicker)}
      >▾</button>
      {#if showPicker}
        <!-- 点击面板外收起：stopPropagation 防止面板内点击把自己关掉 -->
        <div class="scrim" onclick={() => (showPicker = false)} aria-hidden="true"></div>
        <div bind:this={pickerEl} class="picker" role="dialog" aria-modal="true" tabindex="-1" onkeydown={(e) => { if (e.key === "Escape") { e.stopPropagation(); showPicker = false; } }} aria-label="选择目标会话" data-testid="pick-panel">
          {#if openSessions.length === 0}
            <div class="empty">请先连接会话，再选择命令发送目标。</div>
          {:else}
            {#each openSessions as s (s.id)}
              <label class="pickrow">
                <input
                  type="checkbox"
                  checked={$pickedSessions.includes(s.id)}
                  onchange={(e) => togglePicked(s.id, e.currentTarget.checked)}
                />
                <span class="picktitle">{s.title}</span>
              </label>
            {/each}
            <div class="pickops">
              <button
                type="button"
                onclick={() => pickedSessions.set(openSessions.map((s) => s.id))}
              >全选</button>
              <button type="button" onclick={() => pickedSessions.set([])}>清空</button>
              <span class="pickcount">已选 {$pickedSessions.length}</span>
            </div>
          {/if}
          <button type="button" onclick={() => (showPicker = false)}>完成选择</button>
        </div>
      {/if}
    {/if}
  </div>
  <input
    bind:this={input}
    class="cmd"
    type="text"
    placeholder="输入命令…（↑↓ 历史，Enter 发送）"
    bind:value={text}
    onkeydown={onKeydown}
    spellcheck="false"
  />
  <select class="suffix" aria-label="发送后缀" bind:value={suffix}>
    <option value={"\r"}>CR</option>
    <option value={"\n"}>LF</option>
    <option value={"\r\n"}>CRLF</option>
    <option value={""}>无</option>
  </select>
  <button class="send" disabled={!text.trim()} onclick={() => send(true)}>发送 ⏎</button>
</div>

<style>
  .compose { display: flex; align-items: center; gap: 6px; padding: 6px 8px; background: var(--fs-bg-panel); border-top: 1px solid var(--fs-border); flex: none; }
  .target, .suffix { background: var(--fs-bg-input); color: var(--fs-fg-primary); border: 1px solid var(--fs-border); border-radius: var(--fs-radius); padding: 3px 22px 3px 6px; font: inherit; flex: none; } /* 右内距 22px：给全局自绘的下拉箭头让位（2026-09-01） */
  .targetbox { position: relative; display: flex; align-items: center; flex: none; }
  .pickerbtn { border: 1px solid var(--fs-border); border-left: none; border-radius: 0 var(--fs-radius) var(--fs-radius) 0; background: var(--fs-bg-input); color: var(--fs-fg-secondary); cursor: pointer; padding: 3px 5px; font-size: 10px; }
  .cmd { flex: 1; min-width: 0; background: var(--fs-bg-input); color: var(--fs-fg-primary); border: 1px solid var(--fs-border); border-radius: var(--fs-radius); padding: 4px 8px; font: inherit; }
  .send { flex: none; padding: 4px 12px; border: none; border-radius: var(--fs-radius); background: var(--fs-accent); color: var(--fs-accent-fg); cursor: pointer; }
  .send:hover { background: var(--fs-accent-hover); }
  /* 手选面板：覆盖层收点击外，面板本体浮在命令栏上方（下方是终端区，向下展开会盖住输入行） */
  .scrim { position: fixed; inset: 0; z-index: 40; }
  .picker { position: absolute; bottom: calc(100% + 4px); left: 0; z-index: 41; min-width: 220px; max-height: 260px; overflow-y: auto; background: var(--fs-bg-panel); border: 1px solid var(--fs-border); border-radius: var(--fs-radius); box-shadow: 0 4px 16px rgb(0 0 0 / 35%); padding: 6px; }
  .pickrow { display: flex; align-items: center; gap: 8px; padding: 3px 4px; border-radius: 3px; cursor: pointer; white-space: nowrap; }
  .pickrow:hover { background: var(--fs-bg-hover); }
  .picktitle { color: var(--fs-fg-primary); overflow: hidden; text-overflow: ellipsis; max-width: 200px; }
  .pickops { display: flex; align-items: center; gap: 8px; margin-top: 4px; padding-top: 4px; border-top: 1px solid var(--fs-border); }
  .pickops button { background: var(--fs-bg-input); color: var(--fs-fg-secondary); border: 1px solid var(--fs-border); border-radius: 3px; padding: 1px 8px; cursor: pointer; font: inherit; font-size: 12px; }
  .pickcount { margin-left: auto; color: var(--fs-fg-secondary); font-size: 12px; }
  .empty { color: var(--fs-fg-secondary); padding: 6px 4px; }
</style>
