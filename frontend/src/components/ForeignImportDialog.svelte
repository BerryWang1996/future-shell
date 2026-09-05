<script lang="ts">
  import { useFocusTrap } from "../lib/focusTrap";
  import { invoke } from "../lib/ipc";
  import type { ForeignPreview, ForeignImportOutcome } from "../lib/foreign-import";
  import { KIND_LABEL, summarize } from "../lib/foreign-import";

  let {
    open = false,
    onClose = () => {},
    onImported = () => {},
  }: {
    open?: boolean;
    onClose?(): void;
    /** 落库成功后通知外层刷新连接列表 / 配色下拉。 */
    onImported?(outcome: ForeignImportOutcome): void;
  } = $props();

  let dialogEl = $state<HTMLDivElement | undefined>();
  let pickBtn = $state<HTMLButtonElement | undefined>();

  $effect(() => {
    if (!open || !dialogEl || !pickBtn) return;
    return useFocusTrap(dialogEl, { initial: pickBtn });
  });

  /** 已预览、等待确认的文件。可以是多个——用户一次选了一批。 */
  let previews = $state<ForeignPreview[]>([]);
  /** 预览阶段就失败的文件（认不出格式、太大）。列出来而不是静默丢弃。 */
  let failures = $state<{ filename: string; why: string }[]>([]);
  let busy = $state(false);
  let outcome = $state<ForeignImportOutcome | null>(null);

  /**
   * 前端先量一次大小。真正的闸在 Rust 侧（`MAX_IMPORT_BYTES` = 1 MiB），这里挡是为了
   * 不把一个超大文件整份读进内存、推过 IPC 再被拒——与 JSON 导入那条路径同样的理由。
   */
  const MAX_BYTES = 1024 * 1024;

  function reset(): void {
    previews = [];
    failures = [];
    outcome = null;
  }

  function pickFiles(): void {
    const input = document.createElement("input");
    input.type = "file";
    input.multiple = true;
    input.accept = ".xsh,.xcs,.itermcolors,.json";
    input.onchange = async (e) => {
      const files = Array.from((e.target as HTMLInputElement).files ?? []);
      if (files.length === 0) return;
      reset();
      busy = true;
      try {
        for (const f of files) {
          if (f.size > MAX_BYTES) {
            failures.push({ filename: f.name, why: "文件超过 1 MiB —— 配置文件不该这么大" });
            continue;
          }
          try {
            const p = await invoke<ForeignPreview>("foreign_import_preview", {
              filename: f.name,
              content: await f.text(),
            });
            previews.push(p);
          } catch (err) {
            failures.push({ filename: f.name, why: String(err) });
          }
        }
        // 直接 push 不触发 Svelte 5 的深层代理更新（数组是 $state 的顶层引用），重新赋值一次
        previews = [...previews];
        failures = [...failures];
      } finally {
        busy = false;
      }
    };
    input.click();
  }

  /**
   * 落库失败的报错人话化（2026-08-31）。
   *
   * 用户实测报出过 `serde: missing field \`id\` at line 1 column 104` 直接贴在
   * 界面上——那是 Rust 侧 serde 的原文，对用户零信息量，还会让人以为是**自己的
   * 文件**坏了（真实原因是本程序拼落库载荷时漏了字段，与用户文件无关）。
   *
   * 只对**已知会出现的技术性错误**做映射，其余原文透出：编一句笼统的
   * 「导入失败」会把真正需要排查的信息藏掉，比原文更糟。
   */
  function humanizeCommitError(raw: string): string {
    if (/missing field/.test(raw) || /^serde:/.test(raw)) {
      return `落库时本程序内部出错（不是你的文件的问题）：${raw}。请把这句报错反馈给开发者。`;
    }
    if (/UNIQUE constraint|已存在/.test(raw)) {
      return `有同名连接已存在，本次未覆盖：${raw}`;
    }
    return raw;
  }

  async function commitAll(): Promise<void> {
    busy = true;
    const total: ForeignImportOutcome = {
      profiles_added: 0,
      schemes_added: 0,
      schemes_skipped: [],
    };
    // 逐文件落库、逐文件计错（2026-08-31）：此前整个循环包在一个 try 里，
    // 任何一个文件失败都会中断其余文件、并把错误挂在写死的「（落库）」标签上——
    // 用户看到的是一个不存在的"文件名"，还不知道是哪个文件出的问题。
    for (const p of previews) {
      try {
        const r = await invoke<ForeignImportOutcome>("foreign_import_commit", { preview: p });
        total.profiles_added += r.profiles_added;
        total.schemes_added += r.schemes_added;
        total.schemes_skipped.push(...r.schemes_skipped);
      } catch (e) {
        failures = [...failures, { filename: p.filename, why: humanizeCommitError(String(e)) }];
      }
    }
    // 有成功的就报成功（并从待导列表里清掉），失败的留在「没能读取」区。
    // 全失败时不报 outcome——报一个「导入 0 条」的成功框比不报更让人困惑。
    if (total.profiles_added > 0 || total.schemes_added > 0 || total.schemes_skipped.length > 0) {
      outcome = total;
      previews = [];
      onImported(total);
    }
    busy = false;
  }

  /** 全部文件加起来有多少条没能导入的字段。 */
  const unresolvedCount = $derived(previews.reduce((n, p) => n + p.unresolved.length, 0));
</script>

{#if open}
  <div class="overlay" role="presentation" onclick={() => { reset(); onClose(); }}>
    <div class="dialog" role="dialog" aria-modal="true" aria-label="从其他软件导入" tabindex="-1"
         data-testid="foreign-import-dialog" bind:this={dialogEl}
         onclick={(e) => e.stopPropagation()}
         onkeydown={(e) => { if (e.key === "Escape") { reset(); onClose(); } }}>
      <h2>从其他软件导入</h2>
      <p class="hint" data-testid="foreign-import-formats">
        支持 Xshell 会话（.xsh）与配色（.xcs）、FinalShell 连接配置（.json）、iTerm2 配色（.itermcolors）。
        文件名不对也没关系——按内容认。
      </p>
      <!-- 这一句不是免责声明，是这个功能的定义。放在选文件之前，
           因为用户需要在导入前就知道自己之后还得填一遍密码。 -->
      <p class="hint warn" data-testid="foreign-import-no-credentials">
        <strong>密码与私钥不会被导入。</strong>
        那些字段在源文件里是加密的，且凭据本就该由你重新录一次——导进来的连接需要你补上认证方式。
      </p>

      <button bind:this={pickBtn} type="button" data-testid="foreign-import-pick"
              disabled={busy} onclick={pickFiles}>
        {busy ? "读取中…" : "选择文件…"}
      </button>

      {#if previews.length > 0}
        <div class="section" data-testid="foreign-import-preview">
          <h3>将导入</h3>
          <ul>
            {#each previews as p (p.filename)}
              <li data-testid="foreign-import-item">
                <span class="fname">{p.filename}</span>
                <span class="kind">{KIND_LABEL[p.kind] ?? p.kind}</span>
                <span class="what">{summarize(p)}</span>
              </li>
            {/each}
          </ul>
        </div>

        {#if unresolvedCount > 0}
          <details class="section" data-testid="foreign-import-unresolved">
            <!-- 折叠而不是隐藏：这份清单通常有十几条，摊开会把「导入」按钮挤到屏幕外，
                 但它必须能被看到——否则「密码没导进来」就只是一句一闪而过的提示。 -->
            <summary>有 {unresolvedCount} 个字段没能导入（点开看）</summary>
            <ul>
              {#each previews as p (p.filename)}
                {#each p.unresolved as u (p.filename + u.key)}
                  <li data-testid="foreign-unresolved-row">
                    <code>{u.key}</code> — {u.why}
                  </li>
                {/each}
              {/each}
            </ul>
          </details>
        {/if}
      {/if}

      {#if failures.length > 0}
        <div class="section error" data-testid="foreign-import-failures">
          <h3>没能读取</h3>
          <ul>
            {#each failures as f (f.filename + f.why)}
              <li><span class="fname">{f.filename}</span> — {f.why}</li>
            {/each}
          </ul>
        </div>
      {/if}

      {#if outcome}
        <div class="section" data-testid="foreign-import-outcome">
          <p>
            导入完成：连接 {outcome.profiles_added} 条、配色 {outcome.schemes_added} 套。
          </p>
          {#if outcome.schemes_skipped.length > 0}
            <p class="hint" data-testid="foreign-import-skipped">
              同名已存在、跳过的配色：{outcome.schemes_skipped.join("、")}。
              想换成新的，请先删掉旧的再导一次——直接覆盖会无声地毁掉你调过的配色。
            </p>
          {/if}
        </div>
      {/if}

      <footer>
        <button type="button" data-testid="foreign-import-commit"
                disabled={busy || previews.length === 0} onclick={commitAll}>
          导入
        </button>
        <button type="button" data-testid="foreign-import-close"
                onclick={() => { reset(); onClose(); }}>
          {outcome ? "完成" : "取消"}
        </button>
      </footer>
    </div>
  </div>
{/if}

<style>
  .overlay { position: fixed; inset: 0; background: rgba(0,0,0,.5); display: grid; place-items: center; z-index: 60; }
  .dialog { width: 560px; max-width: 94vw; max-height: 86vh; overflow: auto; padding: 16px 20px; background: var(--fs-bg-elevated); border: 1px solid var(--fs-border); border-radius: 6px; color: var(--fs-fg-primary); }
  h2 { margin: 0 0 8px; font-size: 14px; }
  h3 { margin: 0 0 6px; font-size: 12px; color: var(--fs-fg-secondary); }
  .hint { font-size: 11.5px; color: var(--fs-fg-secondary); margin: 0 0 8px; line-height: 1.6; }
  .hint.warn { color: var(--fs-warn, #d29922); }
  .section { margin: 12px 0; font-size: 12px; }
  .section.error { color: var(--fs-danger, #e05252); }
  .section ul { margin: 4px 0 0; padding-left: 18px; }
  .section li { margin: 2px 0; }
  details summary { cursor: pointer; color: var(--fs-fg-secondary); font-size: 12px; }
  .fname { font-weight: 600; }
  .kind { color: var(--fs-fg-secondary); margin: 0 6px; }
  code { font-size: 11px; }
  footer { display: flex; justify-content: flex-end; gap: 8px; margin-top: 14px; }
  footer button { padding: 5px 14px; border: 1px solid var(--fs-border); background: var(--fs-bg-panel); color: var(--fs-fg-primary); border-radius: 4px; cursor: pointer; }
  footer button:disabled { opacity: .5; cursor: default; }
</style>
