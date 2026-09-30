<script lang="ts">
  /**
   * 文件属性对话框（M4a，Xftp 对标「属性」）。
   *
   * 呈现口径的三条铁律，都是「不替用户解析」的同一条原则：
   * ① 软链显示**链自身**的元数据（lstat），另开一栏显示它指向哪里、目标是什么样；
   *    把目标属性冒充成链的属性，用户据此做的删/传/覆盖决定全是错的。
   * ② 服务端没回的字段（mode/uid/gid 在部分 SFTP 实现与 Windows 远端上就是没有）
   *    显示「—」，不编默认值。看到 `0644` 的用户会以为那是真实权限。
   * ③ 断链（目标不可达）是**状态**不是错误：链在、目标读不到，就照实说「目标不可访问」，
   *    不弹错误框、不把整个面板变红。
   */
  import { useFocusTrap } from "../lib/focusTrap";
  import { formatMode, formatOwner, typeLabel, type EntryDetail } from "../lib/fileprops";

  let {
    open = false,
    detail = null,
    err = "",
    onClose,
  }: {
    open?: boolean;
    detail?: EntryDetail | null;
    err?: string;
    onClose(): void;
  } = $props();

  let dialogEl = $state<HTMLDivElement | undefined>();
  $effect(() => {
    if (!open || !dialogEl) return;
    return useFocusTrap(dialogEl, { initial: null });
  });

  function fmtTime(sec: number): string {
    if (!Number.isFinite(sec) || sec <= 0) return "—";
    return new Date(sec * 1000).toLocaleString();
  }
  function fmtSize(n: number): string {
    if (!Number.isFinite(n)) return "—";
    return `${n.toLocaleString()} 字节`;
  }
</script>

{#if open}
  <div class="overlay" role="presentation" onclick={onClose}>
    <div
      class="dialog" role="dialog" aria-modal="true" aria-label="文件属性" tabindex="-1"
      data-testid="props-dialog" bind:this={dialogEl}
      onclick={(e) => e.stopPropagation()}
      onkeydown={(e) => { if (e.key === "Escape") onClose(); }}
    >
      <header>
        <h3>属性</h3>
        <button class="x" onclick={onClose} data-testid="props-close" aria-label="关闭">×</button>
      </header>

      {#if err}
        <p class="err" role="alert" data-testid="props-error">{err}</p>
      {:else if detail}
        <dl data-testid="props-body">
          <dt>路径</dt><dd class="path" data-testid="props-path">{detail.path}</dd>
          <dt>类型</dt><dd data-testid="props-type">{typeLabel(detail.meta.file_type)}</dd>
          <dt>大小</dt><dd data-testid="props-size">{fmtSize(detail.meta.size)}</dd>
          <dt>修改时间</dt><dd data-testid="props-mtime">{fmtTime(detail.meta.mtime)}</dd>
          <dt>权限</dt><dd data-testid="props-mode">{formatMode(detail.meta.mode)}</dd>
          <dt>属主</dt><dd data-testid="props-owner">{formatOwner(detail.meta.uid, detail.meta.gid)}</dd>
        </dl>

        {#if detail.meta.file_type === "symlink"}
          <!-- 链的两半分开陈述：指向何处（readlink 原文，相对/绝对照原样）与目标是什么样。 -->
          <div class="link" data-testid="props-link">
            <div class="link-row">
              <span class="k">指向</span>
              <code data-testid="props-link-target">{detail.link_target ?? "（无法读取链目标）"}</code>
            </div>
            {#if detail.target_meta}
              <dl class="target">
                <dt>目标类型</dt><dd data-testid="props-target-type">{typeLabel(detail.target_meta.file_type)}</dd>
                <dt>目标大小</dt><dd data-testid="props-target-size">{fmtSize(detail.target_meta.size)}</dd>
                <dt>目标修改时间</dt><dd>{fmtTime(detail.target_meta.mtime)}</dd>
                <dt>目标权限</dt><dd>{formatMode(detail.target_meta.mode)}</dd>
              </dl>
            {:else}
              <p class="warn" role="note" data-testid="props-broken">
                目标不可访问（断链、权限不足或跨越了服务端的可见范围）。链本身仍然存在，
                上表显示的是<strong>链自身</strong>的属性。
              </p>
            {/if}
          </div>
        {/if}
      {:else}
        <p class="placeholder" data-testid="props-loading">读取属性…</p>
      {/if}

      <footer><button onclick={onClose} data-testid="props-ok">关闭</button></footer>
    </div>
  </div>
{/if}

<style>
  .overlay { position: fixed; inset: 0; background: rgba(0,0,0,.5); display: grid; place-items: center; z-index: 60; }
  .dialog { width: 520px; max-width: 92vw; max-height: 86vh; overflow-y: auto; padding: 14px 18px;
            background: var(--fs-bg-elevated); border: 1px solid var(--fs-border); border-radius: 6px;
            color: var(--fs-fg-primary); font-size: 13px; }
  header { display: flex; align-items: center; justify-content: space-between; }
  h3 { margin: 0 0 6px; font-size: 14px; }
  .x { border: none; background: none; color: var(--fs-fg-secondary); font-size: 16px; cursor: pointer; }
  dl { display: grid; grid-template-columns: max-content 1fr; gap: 3px 10px; margin: 6px 0; }
  dt { color: var(--fs-fg-secondary); }
  dd { margin: 0; word-break: break-all; }
  dd.path { font-family: var(--fs-font-mono, monospace); font-size: 12px; }
  .link { border-top: 1px solid var(--fs-border); margin-top: 8px; padding-top: 8px; }
  .link-row { display: flex; gap: 8px; align-items: baseline; }
  .link-row .k { color: var(--fs-fg-secondary); }
  .link code { word-break: break-all; }
  dl.target { margin-top: 6px; }
  .warn { color: var(--fs-warn); font-size: 12px; margin: 6px 0 0; }
  .err { color: var(--fs-danger); margin: 4px 0; }
  .placeholder { color: var(--fs-fg-secondary); }
  footer { display: flex; justify-content: flex-end; margin-top: 10px; }
  footer button { padding: 4px 14px; border: 1px solid var(--fs-border); background: var(--fs-bg-panel);
                  color: var(--fs-fg-primary); border-radius: 4px; cursor: pointer; }
</style>
