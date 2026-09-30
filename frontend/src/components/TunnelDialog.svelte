<script lang="ts">
  /**
   * 隧道管理器（M4a：SSH 隧道/端口转发管理器 UI；路线图 §M4）。
   *
   * 只做**本地转发**（`-L`），与后端 fs_sshengine::tunnel 同口径——远程转发（`-R`）
   * 与动态转发（`-D`）未做，故界面上不放这两个选项：给一个选不了的选项比不给更糟。
   *
   * 「隧道随会话关闭而停止」写在界面上而不是只写在代码注释里：用户建了一条
   * 到生产库的隧道，需要知道它不会在下次启动后自动回来。
   */
  import { useFocusTrap } from "../lib/focusTrap";
  import { invoke } from "../lib/ipc";
  import { toast } from "../lib/toast";

  interface TunnelInfo {
    id: string;
    local_port: number;
    local_addr: string;
    remote_host: string;
    remote_port: number;
    bind_all: boolean;
    accepted: number;
  }

  let {
    open = false,
    sessionId = null,
    onClose,
  }: {
    open?: boolean;
    /** 活动会话；null 时禁用新建（隧道是会话级设施） */
    sessionId?: string | null;
    onClose(): void;
  } = $props();

  let list = $state<TunnelInfo[]>([]);
  let localPort = $state("");
  let remoteHost = $state("");
  let remotePort = $state("");
  let bindAll = $state(false);
  let busy = $state(false);

  /** 列表刷新：打开时与每次增删后拉一次。轮询刷 accepted 计数交给用户手动
   *  「刷新」——后台定时 invoke 只为一个计数不值当（隧道页多是短暂停留）。 */
  async function refresh() {
    if (!sessionId) {
      list = [];
      return;
    }
    try {
      list = await invoke<TunnelInfo[]>("tunnel_list", { sessionId });
    } catch (e) {
      toast.error(`读取隧道列表失败：${e}`);
    }
  }

  $effect(() => {
    if (open) void refresh();
  });

  async function start() {
    if (!sessionId || busy) return;
    const lp = Number(localPort);
    const rp = Number(remotePort);
    // 前端只挡「明显填错」——权威校验在 Rust（validate_spec），错误文案直出
    if (!Number.isInteger(lp) || lp < 1 || lp > 65535) {
      toast.warn("本地端口须是 1–65535 的整数");
      return;
    }
    if (!Number.isInteger(rp) || rp < 1 || rp > 65535) {
      toast.warn("远端端口须是 1–65535 的整数");
      return;
    }
    if (!remoteHost.trim()) {
      toast.warn("远端主机不能为空");
      return;
    }
    busy = true;
    try {
      await invoke("tunnel_start", {
        sessionId,
        spec: {
          // id 用「本地端口 + 时间」而非随机 UUID：出问题时日志里的 id 能一眼
          // 对上用户说的「18080 那条」
          id: `L${lp}-${Date.now().toString(36)}`,
          local_port: lp,
          remote_host: remoteHost.trim(),
          remote_port: rp,
          bind_all: bindAll,
        },
      });
      toast.info(`隧道已启动：127.0.0.1:${lp} → ${remoteHost.trim()}:${rp}`);
      localPort = "";
      remoteHost = "";
      remotePort = "";
      bindAll = false;
      await refresh();
    } catch (e) {
      toast.error(`隧道启动失败：${e}`);
    } finally {
      busy = false;
    }
  }

  async function stop(id: string) {
    if (!sessionId) return;
    try {
      await invoke("tunnel_stop", { sessionId, tunnelId: id });
      await refresh();
    } catch (e) {
      toast.error(`停止隧道失败：${e}`);
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
      class="dialog" role="dialog" aria-modal="true" aria-label="隧道管理器" tabindex="-1"
      bind:this={dialogEl}
      data-testid="tunnel-dialog"
      onclick={(e) => e.stopPropagation()}
      onkeydown={(e) => { if (e.key === "Escape") onClose(); }}
    >
      <h3>SSH 隧道（本地端口转发）</h3>
      {#if !sessionId}
        <p class="warn" data-testid="tunnel-nosession">请先连接并选中一个 SSH 会话，再打开隧道管理器。</p>
      {:else}
        <div class="form">
          <label>本地端口 <input data-testid="tn-local" inputmode="numeric" bind:value={localPort} placeholder="18080" /></label>
          <span class="arrow">→</span>
          <label>远端主机 <input data-testid="tn-host" bind:value={remoteHost} placeholder="localhost（在服务端解析）" /></label>
          <label>远端端口 <input data-testid="tn-remote" inputmode="numeric" bind:value={remotePort} placeholder="5432" /></label>
          <label class="chk" title="绑 0.0.0.0 会让同网段任何人都能用这条隧道">
            <input type="checkbox" bind:checked={bindAll} data-testid="tn-bindall" /> 允许外部访问
          </label>
          <button data-testid="tn-start" disabled={busy} onclick={start}>{busy ? "启动中…" : "启动隧道"}</button>
        </div>
        {#if bindAll}
          <p class="warn" data-testid="tn-bindall-warn">
            已勾选「允许外部访问」：隧道将绑定 0.0.0.0，<b>同网段任何人</b>都能经它访问 {remoteHost || "远端"}。
          </p>
        {/if}
        <p class="hint">隧道随会话关闭而停止，不会自动恢复。远程转发（-R）与动态转发（-D）暂未支持。</p>
        <table class="tbl">
          <thead><tr><th>本地</th><th>远端</th><th>累计连接数</th><th></th></tr></thead>
          <tbody>
            {#each list as t (t.id)}
              <tr data-testid="tn-row">
                <td class="mono">{t.local_addr}{#if t.bind_all}<span class="badge" title="绑定 0.0.0.0">外部可达</span>{/if}</td>
                <td class="mono">{t.remote_host}:{t.remote_port}</td>
                <td>{t.accepted}</td>
                <td><button data-testid="tn-stop" onclick={() => stop(t.id)}>停止</button></td>
              </tr>
            {:else}
              <tr><td colspan="4" class="empty">尚无隧道</td></tr>
            {/each}
          </tbody>
        </table>
      {/if}
      <footer>
        {#if sessionId}<button data-testid="tn-refresh" onclick={refresh}>刷新</button>{/if}
        <button onclick={onClose}>关闭</button>
      </footer>
    </div>
  </div>
{/if}

<style>
  .overlay { position: fixed; inset: 0; background: rgba(0,0,0,.5); display: grid; place-items: center; z-index: 60; }
  .dialog { width: 640px; max-width: 94vw; max-height: 80vh; overflow-y: auto; padding: 16px 20px; background: var(--fs-bg-elevated); border: 1px solid var(--fs-border); border-radius: 6px; color: var(--fs-fg-primary); font-size: 13px; }
  h3 { margin: 0 0 12px; font-size: 14px; }
  .form { display: flex; align-items: flex-end; gap: 8px; flex-wrap: wrap; }
  .form label { display: flex; flex-direction: column; gap: 2px; font-size: 12px; color: var(--fs-fg-secondary); }
  .form input:not([type="checkbox"]) { width: 120px; background: var(--fs-bg-input); color: var(--fs-fg-primary); border: 1px solid var(--fs-border); border-radius: 3px; padding: 3px 6px; font: inherit; }
  .form .chk { flex-direction: row; align-items: center; gap: 4px; }
  .arrow { color: var(--fs-fg-secondary); padding-bottom: 4px; }
  .form button { padding: 4px 14px; border: none; border-radius: 4px; background: var(--fs-accent); color: var(--fs-accent-fg); cursor: pointer; }
  .hint { color: var(--fs-fg-secondary); font-size: 12px; margin: 8px 0; }
  .warn { color: var(--fs-danger); font-size: 12px; margin: 8px 0; }
  .tbl { width: 100%; border-collapse: collapse; font-size: 12px; }
  .tbl th { text-align: left; color: var(--fs-fg-secondary); font-weight: normal; border-bottom: 1px solid var(--fs-border); padding: 3px 4px; }
  .tbl td { padding: 4px; border-bottom: 1px solid var(--fs-border); }
  .mono { font-family: var(--fs-mono, monospace); }
  .badge { margin-left: 6px; padding: 0 4px; border-radius: 3px; background: var(--fs-danger); color: #fff; font-size: 10px; }
  .empty { color: var(--fs-fg-secondary); text-align: center; }
  .tbl button { background: var(--fs-bg-panel); border: 1px solid var(--fs-border); color: var(--fs-fg-primary); border-radius: 3px; padding: 1px 10px; cursor: pointer; }
  footer { display: flex; justify-content: flex-end; gap: 8px; margin-top: 12px; }
  footer button { padding: 4px 14px; border: 1px solid var(--fs-border); background: var(--fs-bg-panel); color: var(--fs-fg-primary); border-radius: 4px; cursor: pointer; }
</style>
