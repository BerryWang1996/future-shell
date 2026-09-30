<script lang="ts">
  import { invoke } from "../lib/ipc";
  import { untilUnmount } from "../lib/lifecycle";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { onMount } from "svelte";
  import { useFocusTrap } from "../lib/focusTrap";
  import { toast } from "../lib/toast";

  /**
   * MCP 确认对话框（总设计 §4.5「阻塞式人机确认契约」的前端落点）。
   *
   * 外部 MCP 客户端（Claude Desktop 等）发来 ⚠ 工具调用、闸门判为需要人点头时，
   * 后端 emit `mcp:confirm` 并**阻塞等待**。这里就是那个「人点头」的地方——
   * stdio 传输没有别的人机回路，这个框不点，调用就只会等满 120 秒后超时。
   *
   * 与 Agent 确认同款手势成本分级：`write` 单次确认、`dangerous` 强确认
   *（逐字输入「确认」）。强确认是危险级唯一多出来的成本，不能被省掉。
   */

  type McpConfirm = {
    request_id: number;
    tool: string;
    caller: string;
    tier: string;
    strong: boolean;
    display: string;
    details: string[];
    session_id: string | null;
  };

  let queue = $state<McpConfirm[]>([]);
  const item = $derived(queue[0] ?? null);
  let answering = $state(false);
  /** 强确认的二次输入（逐字打「确认」）。 */
  let strongInput = $state("");
  $effect(() => { if (item) strongInput = ""; });
  let dialogEl = $state<HTMLDivElement | null>(null);
  $effect(() => {
    if (item && dialogEl) return useFocusTrap(dialogEl, { initial: dialogEl.querySelector<HTMLElement>('[data-testid="mcp-reject"]') });
  });

  onMount(() => {
    const uns: UnlistenFn[] = [];
    void (async () => {
      uns.push(
        untilUnmount(listen<McpConfirm>("mcp:confirm", (e) => {
          if (!queue.some(it => it.request_id === e.payload.request_id)) queue = [...queue, e.payload];
        })),
      );
    })();
    return () => uns.forEach((u) => u());
  });

  async function answer(approved: boolean): Promise<void> {
    const it = item;
    if (!it || answering) return;
    // 强确认要求逐字输入——这是 dangerous 与 write 的唯一手势差别。
    if (approved && it.strong && strongInput.trim() !== "确认") {
      toast.warn("危险操作：请逐字输入「确认」两个字");
      return;
    }
    answering = true;
    try {
    const ok = await invoke<boolean>("mcp_confirm_answer", {
      requestId: it.request_id,
      approved,
    });
    // 答到一枚过期票据（后端已超时撤回）时给用户一声，别让他以为点了没反应。
    if (!ok) toast.info("这条确认已经过期了（超时或调用已取消）");
    } catch (e) { toast.error(`未能发送确认结果，操作将等待超时取消：${e}`); }
    finally { queue = queue.filter(p => p.request_id !== it.request_id); answering = false; }
  }
</script>

{#if item}
  <div class="overlay" role="presentation">
    <div class="confirm" role="alertdialog" aria-modal="true" aria-label="MCP 操作确认"
         bind:this={dialogEl} tabindex="-1"
         onkeydown={(e) => { if (e.key === "Escape") { e.stopPropagation(); void answer(false); } }}
         data-testid="mcp-confirm">
      <p class="who">
        外部客户端 <b data-testid="mcp-caller">{item.caller}</b> 请求执行
        {#if item.session_id}<span class="sess">（会话 {item.session_id.slice(0, 8)}）</span>{/if}
      </p>
      {#if queue.length > 1}<p class="who">还有 {queue.length - 1} 个请求等待确认，将逐个显示。</p>{/if}
      <p class="tool" data-testid="mcp-tool">{item.tool}</p>
      <p class="display" data-testid="mcp-display">{item.display}</p>
      {#if item.details.length}
        <ul class="details" data-testid="mcp-details">
          {#each item.details as d}<li>{d}</li>{/each}
        </ul>
      {/if}
      {#if item.strong}
        <label class="strong">
          危险操作：请逐字输入「确认」以批准
          <input data-testid="mcp-strong-input" bind:value={strongInput} placeholder="确认" />
        </label>
      {/if}
      <footer>
        <button type="button" disabled={answering} data-testid="mcp-reject" onclick={() => void answer(false)}>拒绝</button>
        <button type="button" class="primary" disabled={answering || (item.strong && strongInput.trim() !== "确认")} data-testid="mcp-approve" onclick={() => void answer(true)}>
          {answering ? "发送中…" : item.strong ? "确认执行" : "允许"}
        </button>
      </footer>
    </div>
  </div>
{/if}

<style>
  .overlay { position: fixed; inset: 0; background: rgba(0,0,0,.5); display: grid; place-items: center; z-index: 70; }
  .confirm { width: 460px; max-width: 92vw; max-height: 92vh; overflow-y: auto; padding: 16px 20px; background: var(--fs-bg-elevated); border: 1px solid var(--fs-border); border-radius: 6px; color: var(--fs-fg-primary); display: flex; flex-direction: column; gap: 10px; }
  .who { margin: 0; font-size: 12.5px; color: var(--fs-fg-secondary); }
  .who b { color: var(--fs-fg-primary); }
  .sess { color: var(--fs-fg-secondary); }
  .tool { margin: 0; font-size: 13px; font-weight: 600; }
  .display { margin: 0; font-size: 12.5px; font-family: var(--fs-mono, monospace); background: var(--fs-bg-input); padding: 6px 8px; border-radius: 4px; white-space: pre-wrap; word-break: break-all; }
  .details { margin: 0; padding-left: 18px; font-size: 12px; color: var(--fs-fg-secondary); display: flex; flex-direction: column; gap: 3px; }
  .strong { display: flex; flex-direction: column; gap: 4px; font-size: 12.5px; color: var(--fs-fg-primary); }
  .strong input { padding: 5px 8px; background: var(--fs-bg-input); border: 1px solid var(--fs-border); border-radius: 4px; color: var(--fs-fg-primary); }
  footer { display: flex; justify-content: flex-end; gap: 8px; }
  footer button { padding: 5px 14px; border: 1px solid var(--fs-border); background: var(--fs-bg-panel); color: var(--fs-fg-primary); border-radius: 4px; cursor: pointer; }
  footer button.primary { background: var(--fs-accent); color: var(--fs-accent-fg); border-color: var(--fs-accent); }
</style>
