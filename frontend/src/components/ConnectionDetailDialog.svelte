<script lang="ts">
  /**
   * 连接详情弹层（M4a；UI 规格 §2.7「点击状态栏状态灯弹出：认证方式、密钥指纹、延时」）。
   *
   * 三个字段各有各的用途，故各自有独立的「测不到」表示，不混成一句「获取失败」：
   * - 认证方式：知道自己是靠密钥还是口令进去的（排查「为什么这台要我输密码」）；
   * - 密钥指纹：**核对 MITM** 的唯一手段，必须能一眼比对、能复制；
   * - 往返延时：判断卡不卡。测不到时显示「—」而不是 0（0 ms 会被读成极快）。
   */
  import { useFocusTrap } from "../lib/focusTrap";
  import { invoke } from "../lib/ipc";
  import { clipboardWrite } from "../lib/term";
  import { toast } from "../lib/toast";

  interface ConnectionDetail {
    host: string;
    port: number;
    username: string;
    auth_method: string;
    key_type: string;
    fingerprint_sha256: string;
    latency_ms: number | null;
  }

  let {
    open = false,
    sessionId = null,
    onClose,
  }: { open?: boolean; sessionId?: string | null; onClose(): void } = $props();

  let detail = $state<ConnectionDetail | null>(null);
  let err = $state("");
  let loading = $state(false);

  async function load() {
    if (!sessionId) {
      detail = null;
      err = "";
      return;
    }
    loading = true;
    try {
      detail = await invoke<ConnectionDetail>("session_connection_detail", { sessionId });
      err = "";
    } catch (e) {
      err = String(e);
    } finally {
      loading = false;
    }
  }

  $effect(() => {
    if (open) void load();
  });

  async function copyFingerprint() {
    if (!detail?.fingerprint_sha256) return;
    try {
      await clipboardWrite(detail.fingerprint_sha256);
      toast.info("指纹已复制");
    } catch (e) {
      toast.error(`复制失败：${e}`);
    }
  }
  let dialogEl = $state<HTMLDivElement | null>(null);
  $effect(() => {
    if (open && dialogEl) return useFocusTrap(dialogEl, { initial: dialogEl.querySelector<HTMLElement>("input, footer button:not([disabled])") });
  });
</script>

{#if open}
  <div class="overlay" role="presentation" onclick={onClose}>
    <div
      class="pop" role="dialog" aria-modal="true" aria-label="连接详情" tabindex="-1"
      bind:this={dialogEl}
      data-testid="conn-detail"
      onclick={(e) => e.stopPropagation()}
      onkeydown={(e) => { if (e.key === "Escape") onClose(); }}
    >
      <h3>连接详情</h3>
      {#if !sessionId}
        <p class="dim">请先连接并选中一个 SSH 会话，再查看连接详情。</p>
      {:else if err}
        <p class="err" role="alert" data-testid="cd-err">读取失败：{err}</p>
      {:else if !detail}
        <p class="dim">{loading ? "读取中…" : "—"}</p>
      {:else}
        <dl>
          <dt>主机</dt>
          <dd class="mono">{detail.username}@{detail.host}:{detail.port}</dd>
          <dt>认证方式</dt>
          <dd data-testid="cd-auth">{detail.auth_method || "—"}</dd>
          <dt>主机密钥</dt>
          <dd class="mono">{detail.key_type || "—"}</dd>
          <dt>指纹</dt>
          <dd class="fp">
            <span class="mono" data-testid="cd-fp">{detail.fingerprint_sha256 || "—"}</span>
            {#if detail.fingerprint_sha256}
              <button class="copy" data-testid="cd-copy" onclick={copyFingerprint}>复制</button>
            {/if}
          </dd>
          <dt>往返延时</dt>
          <dd data-testid="cd-latency">
            {detail.latency_ms == null ? "—" : `${detail.latency_ms} ms`}
            <span class="dim small">（含一次远端命令往返，非纯网络时延）</span>
          </dd>
        </dl>
      {/if}
      <footer>
        <button data-testid="cd-refresh" onclick={load} disabled={!sessionId || loading}>刷新</button>
        <button onclick={onClose}>关闭</button>
      </footer>
    </div>
  </div>
{/if}

<style>
  .overlay { position: fixed; inset: 0; background: rgba(0,0,0,.4); display: grid; place-items: center; z-index: 60; }
  .pop { width: 460px; max-width: 92vw; max-height: 92vh; overflow-y: auto; padding: 14px 18px; background: var(--fs-bg-elevated); border: 1px solid var(--fs-border); border-radius: 6px; color: var(--fs-fg-primary); font-size: 13px; }
  h3 { margin: 0 0 10px; font-size: 14px; }
  dl { display: grid; grid-template-columns: auto 1fr; gap: 6px 12px; margin: 0; }
  dt { color: var(--fs-fg-secondary); }
  dd { margin: 0; overflow-wrap: anywhere; }
  .mono { font-family: var(--fs-mono, monospace); }
  .fp { display: flex; align-items: center; gap: 8px; }
  .copy { background: var(--fs-bg-panel); border: 1px solid var(--fs-border); color: var(--fs-fg-primary); border-radius: 3px; padding: 1px 8px; cursor: pointer; font-size: 12px; flex: none; }
  .dim { color: var(--fs-fg-secondary); }
  .small { font-size: 11px; }
  .err { color: var(--fs-danger); }
  footer { display: flex; justify-content: flex-end; gap: 8px; margin-top: 12px; }
  footer button { padding: 4px 14px; border: 1px solid var(--fs-border); background: var(--fs-bg-panel); color: var(--fs-fg-primary); border-radius: 4px; cursor: pointer; }
</style>
