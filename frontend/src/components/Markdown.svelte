<script lang="ts">
  /**
   * 渲染 Markdown token 树（M2 出口第 12 项）。
   *
   * **这个组件里没有 `{@html}`。** 那是它存在的全部理由——
   * 文本来自远端模型，而「没有脚本执行路径」在这里不是靠净化器保证的，
   * 是结构上不可能：所有文字都走 Svelte 的文本插值，它会转义一切。
   *
   * 一段 `<img src=x onerror=alert(1)>` 在这里的结局是被当成普通文字显示出来。
   */
  import { parseMarkdown, type Block } from "../lib/markdown-lite";

  let { source = "" }: { source?: string } = $props();
  const blocks = $derived<Block[]>(parseMarkdown(source));
</script>

<div class="md" data-testid="markdown">
  {#each blocks as b, bi (bi)}
    {#if b.kind === "p"}
      <p>
        {#each b.spans as s, si (si)}
          {#if s.kind === "code"}<code>{s.text}</code>{:else if s.kind === "strong"}<strong>{s.text}</strong>{:else}{s.text}{/if}
        {/each}
      </p>
    {:else if b.kind === "h"}
      <!-- 标题级别做成 class 而不是动态标签名：<svelte:element> 能做，但那让
           「渲染出什么标签」变成一个运行期决定的值，而这个组件的全部卖点是
           结构固定。三个分支写死更长，却让「不可能渲染出 <script>」一眼可验。 -->
      <p class="h h{b.level}">
        {#each b.spans as s, si (si)}
          {#if s.kind === "code"}<code>{s.text}</code>{:else if s.kind === "strong"}<strong>{s.text}</strong>{:else}{s.text}{/if}
        {/each}
      </p>
    {:else if b.kind === "ul"}
      <ul>
        {#each b.items as item, ii (ii)}
          <li>
            {#each item as s, si (si)}
              {#if s.kind === "code"}<code>{s.text}</code>{:else if s.kind === "strong"}<strong>{s.text}</strong>{:else}{s.text}{/if}
            {/each}
          </li>
        {/each}
      </ul>
    {:else if b.kind === "ol"}
      <ol>
        {#each b.items as item, ii (ii)}
          <li>
            {#each item as s, si (si)}
              {#if s.kind === "code"}<code>{s.text}</code>{:else if s.kind === "strong"}<strong>{s.text}</strong>{:else}{s.text}{/if}
            {/each}
          </li>
        {/each}
      </ol>
    {:else if b.kind === "pre"}
      <pre data-testid="md-pre" data-lang={b.lang}><code>{b.text}</code></pre>
    {/if}
  {/each}
</div>

<style>
  .md { font-size: 12.5px; line-height: 1.65; color: var(--fs-fg-primary); }
  .md p { margin: 6px 0; }
  .md .h { font-weight: 600; margin: 10px 0 4px; }
  .md .h1 { font-size: 14px; }
  .md .h2 { font-size: 13px; }
  .md .h3 { font-size: 12.5px; color: var(--fs-fg-secondary); }
  .md ul, .md ol { margin: 6px 0; padding-left: 20px; }
  .md li { margin: 2px 0; }
  .md code { font-family: var(--fs-font-mono, monospace); font-size: 11.5px; background: var(--fs-bg-panel); padding: 1px 4px; border-radius: 3px; }
  /* 代码块可横向滚动而不撑破面板：解读里常有长命令，而 AI 面板是个窄栏。 */
  .md pre { margin: 8px 0; padding: 8px 10px; background: var(--fs-bg-panel); border: 1px solid var(--fs-border); border-radius: 4px; white-space: pre-wrap; overflow-wrap: anywhere; } /* 2026-08-31 换行代替横向滚动 */
  .md pre code { background: none; padding: 0; white-space: pre; }
</style>
