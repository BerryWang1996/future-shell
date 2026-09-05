<script lang="ts">
  /**
   * 快速命令集/片段库管理器（M4a，FinalShell 式片段库）。
   *
   * 增删改查都在本地副本上做，**确定**才整体回写（onSave 全量列表）——逐条
   * 即时写库的版本，一次失败的写会让库与界面立刻分叉；快照式提交让「库里
   * 是什么」只在一个时刻变化。App 持有 settings 键的读写。
   */
  import { useFocusTrap } from "../lib/focusTrap";
  import {
    newQuickCommandId,
    parsePlaceholders,
    type QuickCommand,
  } from "../lib/quick-commands";

  let {
    open = false,
    items = [],
    onSave,
    onClose,
  }: {
    open?: boolean;
    items?: QuickCommand[];
    /** 确认提交：全量列表（App 序列化落 settings 键 quick.commands） */
    onSave(list: QuickCommand[]): void | boolean | Promise<void | boolean>;
    onClose(): void;
  } = $props();

  /** 编辑副本：open 时从 items 重建（每次打开都从库里来，关掉不落不脏库）。 */
  let list = $state<QuickCommand[]>([]);
  /** 正在编辑的条目（新增或改）；null = 列表态。 */
  let editing = $state<QuickCommand | null>(null);
  let isNew = $state(false);
  let dialogEl = $state<HTMLDivElement | undefined>();
  let firstBtn = $state<HTMLButtonElement | undefined>();

  $effect(() => {
    if (!open || !dialogEl) return;
    // firstBtn 仅列表态存在（表单态首次渲染晚于本 effect），null 允许——focusTrap 自回落
    return useFocusTrap(dialogEl, { initial: firstBtn ?? null });
  });

  $effect(() => {
    if (open) {
      list = items.map((x) => ({ ...x }));
      editing = null;
    }
  });

  function startAdd() {
    isNew = true;
    editing = { id: newQuickCommandId(), name: "", command: "", category: "" };
  }

  function startEdit(s: QuickCommand) {
    isNew = false;
    editing = { ...s, category: s.category ?? "" };
  }

  /** 保存编辑中的条目回列表（App 层 onSave 时才落库；字段包络校验在后端闸）。 */
  function commitEdit() {
    const e = editing;
    if (!e) return;
    const name = e.name.trim();
    if (!name || !e.command.trim()) return; // 必填缺项：不关表单（按钮 disabled 兜底）
    // 编辑副本的 category 恒为 string（空串 = 无分类），落列表前归一化回 undefined
    const cat = (e.category ?? "").trim();
    const normalized: QuickCommand = {
      id: e.id,
      name,
      command: e.command,
      ...(cat ? { category: cat } : {}),
    };
    list = isNew ? [...list, normalized] : list.map((x) => (x.id === normalized.id ? normalized : x));
    editing = null;
  }

  function remove(id: string) {
    list = list.filter((x) => x.id !== id);
  }

  let saving = $state(false);
  let saveError = $state("");
  async function save() {
    if (saving) return;
    saving = true;
    saveError = "";
    try {
      if (await onSave(list.map(x => ({ ...x }))) === false) { saveError = "命令集未保存，请重试。"; return; }
      onClose();
    } catch (e) { saveError = "命令集未保存：" + e; }
    finally { saving = false; }
  }
</script>

{#if open}
  <div class="overlay" role="presentation" onclick={onClose}>
    <div
      class="dialog" role="dialog" aria-modal="true" aria-label="管理快速命令集" tabindex="-1"
      data-testid="quick-commands-dialog" bind:this={dialogEl}
      onclick={(e) => e.stopPropagation()}
      onkeydown={(e) => { if (e.key === "Escape") { editing ? (editing = null) : onClose(); } }}
    >
      <h3>快速命令集</h3>
      <p class="hint">增删改后点击「保存命令集」生效；取消会放弃本次更改。</p>
      {#if saveError}<p role="alert">{saveError}</p>{/if}

      {#if editing}
        <!-- 条目编辑表单：新增/修改共用 -->
        <div class="form" data-testid="qc-form">
          <label>名称 <input data-testid="qc-name" maxlength="64" bind:value={editing.name} placeholder="重启 nginx" /></label>
          <label>分类（可选） <input data-testid="qc-category" maxlength="64" bind:value={editing.category} placeholder="运维" /></label>
          <label>命令（支持 {"{{参数}}"} 占位符，发送时提示填写）
            <textarea data-testid="qc-command" rows="3" maxlength="2048" bind:value={editing.command} placeholder={"systemctl restart {{服务名}}"} spellcheck="false"></textarea>
          </label>
          {#if parsePlaceholders(editing.command).length > 0}
            <p class="hint">参数：{parsePlaceholders(editing.command).join("、")}</p>
          {/if}
          <div class="rowops">
            <button data-testid="qc-save" disabled={!editing.name.trim() || !editing.command.trim()} onclick={commitEdit}>保存条目</button>
            <button data-testid="qc-cancel" onclick={() => (editing = null)}>取消</button>
          </div>
        </div>
      {:else}
        <ul class="qlist" data-testid="qc-list">
          {#each list as s (s.id)}
            <li>
              <span class="lcat" title={s.category ?? ""}>{s.category ?? ""}</span>
              <span class="lname" title={s.command}>{s.name}</span>
              <span class="lcmd">{s.command}</span>
              <button data-testid="qc-edit" title="修改" aria-label={`修改 ${s.name}`} onclick={() => startEdit(s)}>✎</button>
              <button data-testid="qc-del" title="删除" aria-label={`删除 ${s.name}`} onclick={() => remove(s.id)}>✕</button>
            </li>
          {:else}
            <li class="lempty">暂无快速命令，点击「新增」创建常用命令。</li>
          {/each}
        </ul>
        <footer>
          <button bind:this={firstBtn} data-testid="qc-add" onclick={startAdd}>＋ 新增</button>
          <span class="spacer"></span>
          <button data-testid="qc-commit" disabled={saving} onclick={save}>{saving ? "保存中…" : "保存命令集"}</button>
          <button data-testid="qc-close" onclick={onClose}>取消</button>
        </footer>
      {/if}
    </div>
  </div>
{/if}

<style>
  .overlay { position: fixed; inset: 0; background: rgba(0,0,0,.5); display: grid; place-items: center; z-index: 60; }
  .dialog { width: 560px; max-width: 92vw; max-height: 80vh; display: flex; flex-direction: column; padding: 16px 20px; background: var(--fs-bg-elevated); border: 1px solid var(--fs-border); border-radius: 6px; color: var(--fs-fg-primary); font-size: 13px; }
  h3 { margin: 0 0 12px; font-size: 14px; }
  .qlist { list-style: none; margin: 0; padding: 0; overflow-y: auto; flex: 1; min-height: 80px; }
  .qlist li { display: flex; align-items: center; gap: 8px; padding: 5px 4px; border-bottom: 1px solid var(--fs-border); }
  .lcat { flex: none; width: 72px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; color: var(--fs-fg-secondary); font-size: 12px; }
  .lname { flex: none; width: 120px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .lcmd { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; color: var(--fs-fg-secondary); font-family: var(--fs-mono, monospace); font-size: 12px; }
  .qlist button { flex: none; background: none; border: none; color: var(--fs-fg-secondary); cursor: pointer; padding: 0 3px; }
  .qlist button:hover { color: var(--fs-fg-primary); }
  .lempty { color: var(--fs-fg-secondary); justify-content: center; }
  .form { display: flex; flex-direction: column; gap: 8px; }
  .form label { display: flex; flex-direction: column; gap: 3px; }
  .form input, .form textarea { background: var(--fs-bg-input); color: var(--fs-fg-primary); border: 1px solid var(--fs-border); border-radius: 4px; padding: 4px 8px; font: inherit; }
  .form textarea { font-family: var(--fs-mono, monospace); resize: vertical; }
  .hint { margin: 0; color: var(--fs-fg-secondary); font-size: 12px; }
  .rowops { display: flex; gap: 8px; }
  footer { display: flex; gap: 8px; margin-top: 12px; }
  .spacer { flex: 1; }
  footer button { padding: 5px 14px; border: 1px solid var(--fs-border); background: var(--fs-bg-panel); color: var(--fs-fg-primary); border-radius: 4px; cursor: pointer; }
  .rowops button:disabled { opacity: 0.5; cursor: default; }
  .rowops button { padding: 4px 12px; border: 1px solid var(--fs-border); background: var(--fs-bg-panel); color: var(--fs-fg-primary); border-radius: 4px; cursor: pointer; }
</style>
