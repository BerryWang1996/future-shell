<script lang="ts">
  /**
   * 键位重绑编辑器（M4a 键盘配置文件；UI 规格 §4）。
   *
   * 逐条列出当前生效键位（默认表 ∪ 用户覆盖），支持：录键改绑 / 解绑 / 复位单条 /
   * 全部复位 / 导入导出 JSON。
   *
   * 录键（record）而非手输键位串：手输要求用户知道规范形（`Ctrl+Shift+Tab`），
   * 而他们知道的是「我按了什么」。录键把这段翻译交给程序。
   *
   * 保存口径：编辑副本上改，**确定**才整体回写（同 QuickCommandsDialog 的快照式
   * 提交理据——逐条即时写库，一次失败就让库与界面分叉）。
   */
  import { get } from "svelte/store";
  import { t } from "../lib/i18n";
  function label(id: string): string {
    const extra: Record<string, string> = { "keyboard.toggleMode": "切换本地/远程键盘", "session.closeActive": "关闭当前会话", "session.properties": "编辑当前会话属性", "view.fontReset": "恢复默认字号", "tab.mruNext": "切换到最近使用的下一个标签", "tab.mruPrev": "切换到最近使用的上一个标签" };
    const translated = get(t)("menu." + id);
    return extra[id] ?? (translated === "menu." + id ? "自定义动作（" + id + "）" : translated);
  }
  import {
    DEFAULT_BINDINGS,
    canonicalCombo,
    comboOwner,
    isBindableCombo,
    mergeBindings,
    type KeyBindings,
  } from "../lib/keymap";

  let {
    /** 当前用户覆盖表（settings 键 keyboard.bindings 的解析结果） */
    user = {},
    /** 提交：整份用户覆盖表（App 负责落库并调 setKeyBindings 即时生效） */
    onSave,
  }: {
    user?: KeyBindings;
    onSave(next: KeyBindings): void;
  } = $props();

  /** 编辑副本。
   *
   * `user` prop 变化时必须重同步：保存后 App 回写 settings 并把新表传回来，
   * 若草稿只捕获初值（`$state({...user})` 的静态快照），下次打开设置页看到的
   * 是上一轮的草稿——用户以为没保存成功。 */
  let draft = $state<KeyBindings>({});
  $effect(() => {
    // 依赖 user：外部表换了就丢弃本地草稿（外部是权威——它刚从库里来）。
    // 初值也由本 effect 供给（首帧即跑），故声明处不再读 user——在声明处读
    // 只会捕获初值快照，是 svelte 的 state_referenced_locally 告警面。
    draft = { ...user };
  });
  /** 正在录键的动作 id（null = 未录）。 */
  let recording = $state<string | null>(null);
  /** 录键即时反馈（冲突/不可绑定的原因）。 */
  let recordHint = $state("");

  /** 生效表 = 默认 ∪ 草稿覆盖。 */
  const effective = $derived(mergeBindings(draft));

  /** 动作 → 当前键位（同一动作可能被绑到多个键；列表按动作组织，取全部键位）。 */
  const rows = $derived(
    [...new Set([...Object.values(DEFAULT_BINDINGS), ...Object.values(effective)])]
      .filter((id): id is string => !!id)
      .sort()
      .map((actionId) => ({
        actionId,
        combos: Object.entries(effective)
          .filter(([, id]) => id === actionId)
          .map(([combo]) => combo),
        defaultCombos: Object.entries(DEFAULT_BINDINGS)
          .filter(([, id]) => id === actionId)
          .map(([combo]) => combo),
      })),
  );

  function startRecord(actionId: string) {
    recording = actionId;
    recordHint = "按下新的组合键…（Esc 取消）";
  }

  /** 录键：捕获 keydown，规范化后写草稿。 */
  function onRecordKey(e: KeyboardEvent) {
    if (!recording) return;
    e.preventDefault();
    e.stopPropagation();
    if (e.key === "Escape") {
      recording = null;
      recordHint = "";
      return;
    }
    // 纯修饰键按下（Ctrl 自己）不成组合——等用户按下主键
    if (["Control", "Alt", "Shift", "Meta"].includes(e.key)) return;
    const combo = canonicalCombo(e);
    if (!isBindableCombo(combo)) {
      // S314：绑了无修饰字符键，终端里就再也打不出这个字符
      recordHint = `${combo} 不可绑定：无 Ctrl/Alt 修饰的字符键会吞掉终端输入`;
      return;
    }
    const owner = comboOwner(effective, combo);
    if (owner && owner !== recording) {
      recordHint = `${combo} 原用于「${label(owner)}」，已在草稿中改绑。点击「保存键位」后生效。`;
      // 不拦：允许抢绑，但把旧主人显式解绑，避免两条绑定并存的歧义
      draft = { ...draft, [combo]: recording };
      recording = null;
      return;
    }
    draft = { ...draft, [combo]: recording };
    recording = null;
    recordHint = "";
  }

  /** 解绑一个键位（默认表里的用 null 显式覆盖；草稿新增的直接删）。 */
  function unbind(combo: string) {
    const next = { ...draft };
    if (combo in DEFAULT_BINDINGS) next[combo] = null;
    else delete next[combo];
    draft = next;
  }

  /** 单条动作复位：清掉该动作相关的全部草稿改动。 */
  function resetAction(actionId: string) {
    const next: KeyBindings = {};
    for (const [combo, id] of Object.entries(draft)) {
      const isDefaultOfThisAction = DEFAULT_BINDINGS[combo] === actionId;
      if (id === actionId || isDefaultOfThisAction) continue; // 丢弃与本动作相关的改动
      next[combo] = id;
    }
    draft = next;
  }

  function resetAll() {
    draft = {};
  }

  function exportJson() {
    const blob = new Blob([JSON.stringify(draft, null, 2)], { type: "application/json" });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = "futureshell-keymap.json";
    a.click();
    // 与连接配置导出同款：click 之后再回收，提前回收会让未取走内容的下载静默失败
    URL.revokeObjectURL(url);
  }

  async function importJson(e: Event) {
    const input = e.currentTarget as HTMLInputElement;
    const file = input.files?.[0];
    if (!file) return;
    try {
      const text = await file.text();
      const parsed = JSON.parse(text);
      if (parsed === null || typeof parsed !== "object" || Array.isArray(parsed)) {
        recordHint = "导入失败：文件不是键位表对象";
        return;
      }
      // 逐条过滤：只收「值为字符串或 null」的条目（他人配置不可尽信）
      const next: KeyBindings = {};
      for (const [k, v] of Object.entries(parsed as Record<string, unknown>)) {
        if (v === null || typeof v === "string") next[k] = v as string | null;
      }
      draft = next;
      recordHint = `已导入 ${Object.keys(next).length} 条覆盖（尚未保存）`;
    } catch (err) {
      recordHint = `导入失败：${err}`;
    } finally {
      input.value = ""; // 允许重复导入同一文件
    }
  }
</script>

<svelte:window onkeydown={recording ? onRecordKey : undefined} />

<div class="keymap" data-testid="keymap-editor">
  <div class="ops">
    <button data-testid="km-save" onclick={() => onSave({ ...draft })}>保存键位</button>
    <button data-testid="km-reset-all" onclick={resetAll}>全部复位</button>
    <button data-testid="km-export" onclick={exportJson}>导出…</button>
    <label class="importbtn">
      导入…
      <input type="file" accept="application/json,.json" data-testid="km-import" onchange={importJson} />
    </label>
  </div>
  {#if recordHint}<p class="hint" role="status" data-testid="km-hint">{recordHint}</p>{/if}
  <table class="kmtable">
    <thead><tr><th>动作</th><th>键位</th><th></th></tr></thead>
    <tbody>
      {#each rows as r (r.actionId)}
        <tr class:changed={r.combos.join() !== r.defaultCombos.join()}>
          <td class="act">{label(r.actionId)}</td>
          <td class="combos">
            {#each r.combos as c (c)}
              <span class="kbd">{c}<button class="x" title="解绑" onclick={() => unbind(c)}>✕</button></span>
            {:else}
              <span class="none">（未绑定）</span>
            {/each}
          </td>
          <td class="rowops">
            <button
              data-testid="km-record-{r.actionId}"
              class:recording={recording === r.actionId}
              onclick={() => startRecord(r.actionId)}
            >{recording === r.actionId ? "录键中…" : "改绑"}</button>
            {#if r.combos.join() !== r.defaultCombos.join()}
              <button title="复位为默认" onclick={() => resetAction(r.actionId)}>↺</button>
            {/if}
          </td>
        </tr>
      {/each}
    </tbody>
  </table>
</div>

<style>
  .keymap { display: flex; flex-direction: column; gap: 8px; min-height: 0; }
  .ops { display: flex; gap: 8px; align-items: center; flex-wrap: wrap; }
  .ops button, .importbtn { padding: 3px 10px; border: 1px solid var(--fs-border); background: var(--fs-bg-panel); color: var(--fs-fg-primary); border-radius: 4px; cursor: pointer; font: inherit; font-size: 12px; }
  .importbtn input { display: none; }
  .hint { margin: 0; color: var(--fs-fg-secondary); font-size: 12px; }
  .kmtable { width: 100%; border-collapse: collapse; font-size: 12px; }
  .kmtable th { text-align: left; color: var(--fs-fg-secondary); font-weight: normal; border-bottom: 1px solid var(--fs-border); padding: 3px 4px; }
  .kmtable td { padding: 3px 4px; border-bottom: 1px solid var(--fs-border); vertical-align: middle; }
  .kmtable tr.changed .act { color: var(--fs-accent); }
  .act { font-family: var(--fs-mono, monospace); }
  .kbd { display: inline-flex; align-items: center; gap: 3px; background: var(--fs-bg-input); border: 1px solid var(--fs-border); border-radius: 3px; padding: 0 4px; margin-right: 4px; font-family: var(--fs-mono, monospace); }
  .kbd .x { background: none; border: none; color: var(--fs-fg-secondary); cursor: pointer; padding: 0; font-size: 10px; }
  .none { color: var(--fs-fg-disabled); }
  .rowops { white-space: nowrap; }
  .rowops button { background: none; border: 1px solid var(--fs-border); border-radius: 3px; color: var(--fs-fg-secondary); cursor: pointer; padding: 1px 8px; font: inherit; font-size: 12px; }
  .rowops button.recording { border-color: var(--fs-accent); color: var(--fs-accent); }
</style>
