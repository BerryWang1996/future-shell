<script lang="ts">
  /**
   * 高亮关键字规则编辑器（M4a，UI 规格 §2.11）。
   *
   * 逐条列出规则：名称/匹配式/类型（字面量·正则）/颜色/提醒/启用。正则**即时编译**
   * 并把错因就地显示——不能让用户保存一条永远不生效的规则却毫无提示（compileRules
   * 对坏规则整条禁用，UI 不标错的话那条规则就成了沉默的谜）。
   *
   * 快照式提交（同 QuickCommandsDialog / KeymapEditor 口径）：编辑副本上改，
   * 确定才整体回写。
   */
  import {
    compileRules,
    newRuleId,
    type HighlightRule,
  } from "../lib/highlights";

  let {
    rules = [],
    onSave,
  }: {
    rules?: HighlightRule[];
    onSave(next: HighlightRule[]): void;
  } = $props();

  let draft = $state<HighlightRule[]>([]);
  $effect(() => {
    // 外部是权威（刚从库里来）；同 KeymapEditor 的重同步理据
    draft = rules.map((r) => ({ ...r }));
  });

  /** 编译结果与草稿逐条对应，用于就地显示正则错因。 */
  const compiled = $derived(compileRules(draft));

  function add() {
    draft = [
      ...draft,
      {
        id: newRuleId(),
        name: "",
        pattern: "",
        kind: "literal",
        color: "#e5c07b",
        alert: false,
        enabled: true,
      },
    ];
  }

  function remove(id: string) {
    draft = draft.filter((r) => r.id !== id);
  }
</script>

<div class="hl" data-testid="highlight-editor">
  <div class="ops">
    <button data-testid="hl-add" onclick={add}>＋ 新增规则</button>
    <button data-testid="hl-save" onclick={() => onSave(draft.map((r) => ({ ...r })))}>保存规则</button>
    <span class="count">共 {draft.length} 条</span>
  </div>
  {#if draft.length === 0}
    <p class="hint">尚无规则。新增后，会话输出里命中的文本会按规则着色；勾了「提醒」的规则命中时标签标签会显示提醒。</p>
  {/if}
  {#each draft as r, i (r.id)}
    <div class="row" data-testid="hl-row">
      <input class="nm" placeholder="名称（可空）" maxlength="64" bind:value={r.name} data-testid="hl-name" />
      <select bind:value={r.kind} data-testid="hl-kind" title="字面量 = 子串匹配（大小写不敏感）；正则 = JS 正则">
        <option value="literal">字面量</option>
        <option value="regex">正则</option>
      </select>
      <input
        class="pat"
        placeholder={r.kind === "regex" ? "如 \\bERROR\\b|FATAL" : "如 ERROR"}
        maxlength="200"
        bind:value={r.pattern}
        data-testid="hl-pattern"
      />
      <input class="col" type="color" bind:value={r.color} data-testid="hl-color" title="命中底色" />
      <label class="chk" title="命中时联动标签角标（与 bell 同一通道）">
        <input type="checkbox" bind:checked={r.alert} data-testid="hl-alert" /> 提醒
      </label>
      <label class="chk" title="关闭而不删除">
        <input type="checkbox" bind:checked={r.enabled} data-testid="hl-enabled" /> 启用
      </label>
      <button class="del" data-testid="hl-del" title="删除" onclick={() => remove(r.id)}>✕</button>
      {#if compiled[i]?.error}
        <p class="err" role="alert" data-testid="hl-error">{compiled[i].error}</p>
      {/if}
    </div>
  {/each}
</div>

<style>
  .hl { display: flex; flex-direction: column; gap: 6px; }
  .ops { display: flex; align-items: center; gap: 8px; }
  .ops button { padding: 3px 10px; border: 1px solid var(--fs-border); background: var(--fs-bg-panel); color: var(--fs-fg-primary); border-radius: 4px; cursor: pointer; font: inherit; font-size: 12px; }
  .count, .hint { color: var(--fs-fg-secondary); font-size: 12px; margin: 0; }
  .row { display: flex; align-items: center; gap: 6px; flex-wrap: wrap; padding: 4px; border: 1px solid var(--fs-border); border-radius: 4px; }
  .nm, .pat { background: var(--fs-bg-input); color: var(--fs-fg-primary); border: 1px solid var(--fs-border); border-radius: 3px; padding: 2px 6px; font: inherit; font-size: 12px; }
  .nm { width: 110px; }
  .pat { flex: 1; min-width: 140px; font-family: var(--fs-mono, monospace); }
  .col { width: 34px; height: 22px; padding: 0; border: 1px solid var(--fs-border); background: none; }
  .chk { display: inline-flex; align-items: center; gap: 3px; color: var(--fs-fg-secondary); font-size: 12px; white-space: nowrap; }
  .del { background: none; border: none; color: var(--fs-fg-secondary); cursor: pointer; }
  .row select { background: var(--fs-bg-input); color: var(--fs-fg-primary); border: 1px solid var(--fs-border); border-radius: 3px; padding: 2px 22px 2px 4px; font: inherit; font-size: 12px; } /* 右内距给箭头让位 */
  .err { width: 100%; margin: 2px 0 0; color: var(--fs-danger); font-size: 12px; }
</style>
