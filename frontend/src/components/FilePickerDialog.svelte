<script lang="ts">
  /**
   * 本地文件/目录选择器（通用模态）。
   *
   * 2026-08-26 从 `ZmodemBar.svelte` 里那个内嵌选择器重做而来。用户实测报的三个
   * 问题都是那一版的：**进了文件夹回不去**（没有上级、没有面包屑）、
   * **不想选了关不掉**、**只能单选**。
   *
   * # 为什么自建而不是引 `tauri-plugin-dialog`（沿用旧版的判断）
   *
   * ① webview 里 `<input type="file">` 拿不到**真实路径**（浏览器刻意隐藏），
   *    而后端需要路径才能读文件；
   * ② 原生对话框能选到浏览根之外，选完后端照样拒——用户看到的是一个莫名其妙的
   *    「path escapes browse root」，而他明明是在程序给的对话框里选的；
   * ③ 供应链卫生是未决审计项，一次 UX 修复不该顺手加一个新依赖。
   *
   * 选择器与后端闸口同源（都走 `local_list`），用户看不到的目录也就选不出来。
   *
   * # 本组件**绝不**自己决定要不要打开
   *
   * `open` 是纯受控 prop，组件内部只会通过 `onCancel` 请求关闭，不会自行置位。
   * 旧版的「关不掉」正是因为调用方写了 `if (needFile && !pickerOpen) pickerOpen = true`
   * ——而 `needFile` 在远端 rz 等待期间恒为真，于是用户刚关掉、effect 重跑、
   * 立刻又开。修法在调用方（边沿触发），但**组件这一侧必须保证自己不掺和**，
   * 否则同样的 bug 会换个地方长出来。
   */
  import { invoke } from "../lib/ipc";
  import { useFocusTrap } from "../lib/focusTrap";
  import {
    EMPTY_SELECTION,
    joinPath,
    parentOf,
    reduceSelection,
    selectedBytes,
    sortEntries,
    splitBreadcrumbs,
    type PickerEntry,
    type Selection,
  } from "../lib/file-picker";
  import { formatBytes } from "../lib/zmodem";

  let {
    open = false,
    title = "选择文件",
    /** `files` = 选一个或多个文件；`directory` = 选当前所在目录 */
    mode = "files",
    multi = true,
    confirmLabel = "",
    /** 顶部常驻说明行（如「收到的文件将保存到…」）；空串不渲染 */
    note = "",
    onConfirm,
    onCancel,
  }: {
    open?: boolean;
    title?: string;
    mode?: "files" | "directory";
    multi?: boolean;
    confirmLabel?: string;
    note?: string;
    onConfirm(paths: string[]): void;
    onCancel(): void;
  } = $props();

  let dialogEl = $state<HTMLDivElement | undefined>();
  let cwd = $state("");
  let entries = $state<PickerEntry[]>([]);
  let loading = $state(false);
  let listError = $state("");
  let truncated = $state(false);
  let sel = $state<Selection>(EMPTY_SELECTION);

  const sorted = $derived(sortEntries(entries));
  /** 可参与多选的条目（目录不参与——点目录是「进去」不是「选中」）。 */
  const selectableNames = $derived(sorted.filter((e) => !e.is_dir).map((e) => e.name));
  const crumbs = $derived(splitBreadcrumbs(cwd));
  const parent = $derived(parentOf(cwd));
  const chosenBytes = $derived(selectedBytes(entries, sel.names));
  const canConfirm = $derived(mode === "directory" || sel.names.size > 0);

  // 打开时回到根并清空选择。**不保留上次位置**：这个模态被不同场景复用
  //（上传选文件 / 下载选目录），留着上次的目录会让用户在一个与当前任务无关的
  // 地方开始，而那比多点两下更让人困惑。
  $effect(() => {
    if (open) {
      cwd = "";
      sel = EMPTY_SELECTION;
      void load("");
    }
  });

  $effect(() => {
    if (!open || !dialogEl) return;
    return useFocusTrap(dialogEl, { initial: dialogEl });
  });

  async function load(path: string) {
    loading = true;
    listError = "";
    try {
      const r = await invoke<{ entries: PickerEntry[]; truncated: boolean }>("local_list", {
        path,
      });
      entries = r.entries;
      truncated = r.truncated;
      cwd = path;
      // 换目录必须清空选择：跨目录的多选在确认时会拼出错误的路径
      //（我们只记名字，路径靠当前 cwd 拼）。
      sel = EMPTY_SELECTION;
    } catch (e) {
      // 列不出来要说出来：静默空列表会被读作「这个目录是空的」。
      listError = String(e);
      entries = [];
      truncated = false;
    } finally {
      loading = false;
    }
  }

  function goUp() {
    if (parent === null) return;
    void load(parent);
  }

  function onRowClick(e: PickerEntry, ev: MouseEvent) {
    if (e.is_dir) {
      void load(joinPath(cwd, e.name));
      return;
    }
    if (mode !== "files") return;
    if (!multi) {
      sel = { names: new Set([e.name]), anchor: e.name };
      return;
    }
    sel = reduceSelection(sel, e.name, { ctrl: ev.ctrlKey || ev.metaKey, shift: ev.shiftKey }, selectableNames);
  }

  function toggleCheck(name: string) {
    sel = reduceSelection(sel, name, { ctrl: true, shift: false }, selectableNames);
  }

  function confirm() {
    if (!canConfirm) return;
    if (mode === "directory") {
      onConfirm([cwd]);
      return;
    }
    // 按**显示顺序**输出，而不是 Set 的插入顺序：用户看到的顺序就是传输顺序，
    // 多文件上传时进度条上的「第 i/N 个」才对得上他的预期。
    onConfirm(selectableNames.filter((n) => sel.names.has(n)).map((n) => joinPath(cwd, n)));
  }

  const defaultConfirm = $derived(
    confirmLabel || (mode === "directory" ? "选择当前目录" : "上传选中项"),
  );
</script>

{#if open}
  <div class="overlay" role="presentation" onclick={onCancel}>
    <div
      class="dialog"
      role="dialog"
      aria-modal="true"
      aria-label={title}
      tabindex="-1"
      data-testid="file-picker"
      bind:this={dialogEl}
      onclick={(e) => e.stopPropagation()}
      onkeydown={(e) => {
        if (e.key === "Escape") onCancel();
      }}
    >
      <header>
        <h3>{title}</h3>
        <button data-testid="fp-close" onclick={onCancel} aria-label="关闭">✕</button>
      </header>

      {#if note}
        <p class="note" data-testid="fp-note">{note}</p>
      {/if}

      <!-- 导航行：上级 + 面包屑。两者都能回上层，是刻意的冗余——
           用户报的原始问题就是「进了文件夹回不去」。 -->
      <div class="nav">
        <button
          data-testid="fp-up"
          disabled={parent === null}
          title={parent === null ? "已在主目录" : "上一级"}
          onclick={goUp}>⬆ 上级</button
        >
        <nav class="crumbs" data-testid="fp-crumbs" aria-label="路径">
          {#each crumbs as c, i (c.path)}
            {#if i > 0}<span class="sep">/</span>{/if}
            <button
              class="crumb"
              disabled={c.path === cwd}
              onclick={() => void load(c.path)}>{c.label}</button
            >
          {/each}
        </nav>
      </div>

      {#if listError}
        <p class="err" role="alert" data-testid="fp-error">{listError}</p>
        <button onclick={() => void load(cwd)}>重试读取目录</button>
      {:else if loading}
        <p class="hint" data-testid="fp-loading">读取中…</p>
      {:else}
        {#if truncated}
          <p class="hint warn" data-testid="fp-truncated">
            此目录条目过多，列表已截断——未显示的文件同样选不到。
          </p>
        {/if}
        <ul data-testid="fp-entries">
          {#if parent !== null}
            <!-- 列表首行的 `..`：与「上级」按钮同一个动作。放两处是因为用户的手
                 可能在列表上，让他不必移到工具栏去。 -->
            <li>
              <button class="row dir" data-testid="fp-dotdot" onclick={goUp}>📁 ..</button>
            </li>
          {/if}
          {#each sorted as e (e.name)}
            <li>
              <button
                class="row"
                class:dir={e.is_dir}
                class:on={sel.names.has(e.name)}
                data-testid={e.is_dir ? "fp-dir" : "fp-file"}
                onclick={(ev) => onRowClick(e, ev)}
              >
                {#if mode === "files" && multi && !e.is_dir}
                  <!-- checkbox 只做视觉与无障碍指示；点击由整行处理器统一收口，
                       否则 Ctrl/Shift 的语义会在 checkbox 与行之间分叉。 -->
                  <span
                    class="box"
                    role="checkbox"
                    tabindex="-1"
                    aria-checked={sel.names.has(e.name)}
                    onclick={(ev) => {
                      ev.stopPropagation();
                      toggleCheck(e.name);
                    }}
                    onkeydown={() => {}}>{sel.names.has(e.name) ? "☑" : "☐"}</span
                  >
                {/if}
                <span class="icon">{e.is_dir ? "📁" : "📄"}</span>
                <span class="name">{e.name}</span>
                {#if !e.is_dir}<span class="size">{formatBytes(e.size)}</span>{/if}
              </button>
            </li>
          {/each}
          {#if sorted.length === 0}
            <li class="empty" data-testid="fp-empty">（空目录）</li>
          {/if}
        </ul>
      {/if}

      <footer>
        <span class="tally" data-testid="fp-tally">
          {#if mode === "directory"}
            将使用：{cwd || "主目录"}
          {:else}
            已选 {sel.names.size} 个{#if chosenBytes > 0}（共 {formatBytes(chosenBytes)}）{/if}
          {/if}
        </span>
        <button data-testid="fp-cancel" onclick={onCancel}>取消</button>
        <button
          class="primary"
          data-testid="fp-confirm"
          disabled={!canConfirm}
          onclick={confirm}>{defaultConfirm}</button
        >
      </footer>
    </div>
  </div>
{/if}

<style>
  .overlay { position: fixed; inset: 0; background: rgba(0,0,0,.5); display: grid; place-items: center; z-index: 60; }
  .dialog { width: 560px; max-width: 94vw; height: 72vh; max-height: 92vh; display: flex; flex-direction: column;
            padding: 14px 18px 16px; background: var(--fs-bg-elevated); border: 1px solid var(--fs-border);
            border-radius: 6px; color: var(--fs-fg-primary); font-size: 13px; }
  header { display: flex; align-items: center; justify-content: space-between; }
  h3 { margin: 0 0 6px; font-size: 14px; }
  header button { border: none; background: none; color: var(--fs-fg-secondary); cursor: pointer; font-size: 14px; }
  .note { margin: 0 0 8px; color: var(--fs-fg-secondary); font-size: 12px; word-break: break-all; }
  .nav { display: flex; align-items: center; gap: 8px; margin-bottom: 8px; flex-wrap: wrap; }
  .nav > button { padding: 3px 10px; border: 1px solid var(--fs-border); background: var(--fs-bg-panel);
                  color: var(--fs-fg-primary); border-radius: 4px; cursor: pointer; font-size: 12px; }
  .nav > button:disabled { color: var(--fs-fg-disabled); cursor: default; }
  .crumbs { display: flex; align-items: center; gap: 2px; flex-wrap: wrap; min-width: 0; }
  .crumb { border: none; background: none; color: var(--fs-accent); cursor: pointer; font: inherit;
           font-size: 12px; padding: 2px 4px; border-radius: 3px; }
  .crumb:disabled { color: var(--fs-fg-primary); cursor: default; font-weight: 600; }
  .crumb:hover:not(:disabled) { background: var(--fs-bg-hover); }
  .sep { color: var(--fs-fg-secondary); font-size: 12px; }
  .hint { margin: 4px 0; color: var(--fs-fg-secondary); font-size: 12px; }
  .hint.warn { color: var(--fs-warn, #d89614); }
  .err { color: var(--fs-danger); margin: 4px 0; }
  ul { list-style: none; margin: 0; padding: 0; overflow: auto; flex: 1;
       border: 1px solid var(--fs-border); border-radius: 4px; }
  li.empty { padding: 8px; color: var(--fs-fg-secondary); }
  .row { display: flex; align-items: center; gap: 8px; width: 100%; text-align: left;
         padding: 4px 8px; border: 0; background: none; color: var(--fs-fg-primary);
         cursor: pointer; font: inherit; }
  .row:hover { background: var(--fs-bg-hover); }
  .row.on { background: var(--fs-bg-selected, var(--fs-bg-hover)); }
  .row.dir { color: var(--fs-accent); }
  .name { flex: 1; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .size { color: var(--fs-fg-secondary); font-size: 12px; font-variant-numeric: tabular-nums; }
  .box { width: 1em; }
  footer { display: flex; align-items: center; gap: 8px; margin-top: 10px; }
  .tally { flex: 1; color: var(--fs-fg-secondary); font-size: 12px;
           overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  footer button { padding: 5px 14px; border: 1px solid var(--fs-border); background: var(--fs-bg-panel);
                  color: var(--fs-fg-primary); border-radius: 4px; cursor: pointer; }
  footer button.primary { background: var(--fs-accent); border-color: var(--fs-accent); color: #fff; }
  footer button:disabled { opacity: .5; cursor: default; }
</style>
