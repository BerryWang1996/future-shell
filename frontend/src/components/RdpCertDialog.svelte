<script lang="ts">
  /**
   * RDP 服务器证书确认（阶段 1 的 TOFU「U」）。
   *
   * 自监听 `rdp:cert:{sessionId}`（与 McpConfirmDialog 同款「组件自己监听」模式：
   * 裁决随时可能到，与哪个面板活动无关）。首连/变更都走这里——后端 rdp.rs
   * decide_cert 只分 Accept / Ask，拒绝只能来自这里的用户。
   *
   * 指纹排版（小写十六进制冒号分隔）与 SSH 主机密钥同款——用户对比指纹用的是
   * 同一套肌肉记忆。变更情形（库里有另一把）后端已把新的落库，这里对用户
   * 忠实转述「已记录的是 A、现在出示的是 B」的语义：首见说首见，见变更说变更。
   */
  import { onMount, onDestroy } from "svelte";
  import { invoke, listen } from "../lib/ipc";
  import { untilUnmount } from "../lib/lifecycle";
  import { useFocusTrap } from "../lib/focusTrap";

  interface CertInfo {
    fingerprint: string;
    subject: string;
    issuer: string;
    notAfter: string;
  }

  let cert = $state<CertInfo | null>(null);
  let sessionId = $state("");
  let dialogEl = $state<HTMLDivElement | undefined>();
  let un: (() => void) | null = null;

  onMount(async () => {
    // 后端用固定事件名 rdp:cert（sessionId 在载荷里）：证书裁决随时可能到、
    // 与哪个面板活动无关，逐会话动态名会让「谁在听」依赖标签是否已建——
    // 恰恰不该有这种依赖。
    un = untilUnmount(listen<{ cert: CertInfo; sessionId: string }>("rdp:cert", (e) => {
      cert = e.payload.cert;
      sessionId = e.payload.sessionId;
    }));
  });
  onDestroy(() => un?.());

  $effect(() => {
    if (!cert || !dialogEl) return;
    return useFocusTrap(dialogEl, { initial: dialogEl });
  });

  async function answer(accept: boolean): Promise<void> {
    try {
      await invoke("rdp_cert_verdict", { sessionId, accept });
    } finally {
      cert = null;
    }
  }
</script>

{#if cert}
  <div class="overlay" role="presentation">
    <div
      class="panel"
      role="dialog"
      aria-modal="true"
      aria-label="RDP 服务器证书确认"
      tabindex="-1"
      data-testid="rdp-cert-dialog"
      bind:this={dialogEl}
      onclick={(e) => e.stopPropagation()}
      onkeydown={(e) => { if (e.key === "Escape") void answer(false); }}
    >
      <h2>RDP 服务器证书</h2>
      <p class="hint">第一次连这台服务器（或证书已变更）。确认指纹与你在服务器上看到的一致。</p>
      <dl>
        <dt>主题</dt><dd>{cert.subject}</dd>
        <dt>签发者</dt><dd>{cert.issuer}</dd>
        <dt>有效期至</dt><dd>{cert.notAfter}</dd>
        <dt>SHA-256 指纹</dt><dd class="fp" data-testid="rdp-cert-fp">{cert.fingerprint}</dd>
      </dl>
      <footer>
        <button data-testid="rdp-cert-reject" onclick={() => void answer(false)}>拒绝连接</button>
        <button class="primary" data-testid="rdp-cert-accept" onclick={() => void answer(true)}>信任并连接</button>
      </footer>
    </div>
  </div>
{/if}

<style>
  .overlay { position: fixed; inset: 0; background: rgba(0,0,0,.5); display: grid; place-items: center; z-index: 60; }
  .panel { width: 480px; max-width: 92vw; max-height: 92vh; overflow-y: auto; padding: 16px 20px; background: var(--fs-bg-elevated);
           border: 1px solid var(--fs-border); border-radius: 6px; color: var(--fs-fg-primary); }
  h2 { margin: 0 0 8px; font-size: 15px; }
  .hint { color: var(--fs-fg-secondary); font-size: 12px; margin: 0 0 12px; }
  dl { display: grid; grid-template-columns: auto 1fr; gap: 6px 12px; font-size: 12.5px; margin: 0; }
  dt { color: var(--fs-fg-secondary); }
  dd { margin: 0; word-break: break-all; }
  .fp { font-family: var(--fs-mono, monospace); color: var(--fs-accent); }
  footer { display: flex; justify-content: flex-end; gap: 8px; margin-top: 14px; }
  footer button { padding: 5px 14px; border: 1px solid var(--fs-border); background: var(--fs-bg-panel);
                  color: var(--fs-fg-primary); border-radius: 4px; cursor: pointer; }
  footer .primary { background: var(--fs-accent); border-color: var(--fs-accent); color: #fff; }
</style>
