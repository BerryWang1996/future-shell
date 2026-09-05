<script lang="ts">
  /**
   * 改波特率（M7.4 出口标准的「改波特率」那一档）。
   *
   * # 为什么不重开端口
   *
   * 重开会让 DTR/RTS 抖一下，而很多板子把 DTR 接在复位脚上（Arduino/ESP32 就是靠这个
   * 自动进下载模式的）——用户只是想换个速率看看，板子却重启了。所以后端走
   * `set_baud_rate` 就地改，这里把这件事在界面上也说清楚。
   *
   * # 清单是便利不是白名单
   *
   * 工控设备上 250000 / 500000 这类非标速率很常见，所以是一个可输入的数字框 +
   * 一份 datalist 建议值，而不是一个只能选的下拉。
   */
  import { useFocusTrap } from "../lib/focusTrap";
  import { invoke } from "../lib/ipc";

  let {
    open = false,
    sessionId = null,
    /** 当前参数摘要（`115200 8N1`），后端给。 */
    current = "",
    port = "",
    onChanged = (_summary: string) => {},
    onCancel = () => {},
  }: {
    open?: boolean;
    sessionId?: string | null;
    current?: string;
    port?: string;
    onChanged?(summary: string): void;
    onCancel?(): void;
  } = $props();

  let dialogEl = $state<HTMLDivElement | undefined>();
  let cancelBtn = $state<HTMLButtonElement | undefined>();
  let baud = $state<number | string>("");
  let bauds = $state<number[]>([]);
  let err = $state("");
  let busy = $state(false);

  $effect(() => {
    if (!open) return;
    err = "";
    baud = "";
    void invoke<number[]>("serial_common_bauds")
      .then((l) => (bauds = Array.isArray(l) ? l : []))
      .catch(() => (bauds = []));
  });

  $effect(() => {
    if (!open || !dialogEl || !cancelBtn) return;
    return useFocusTrap(dialogEl, { initial: cancelBtn });
  });

  async function apply() {
    const n = Number(baud);
    if (!sessionId || !Number.isFinite(n) || n <= 0) return;
    busy = true;
    err = "";
    try {
      const summary = await invoke<string>("serial_set_baud", { sessionId, baud: n });
      onChanged(summary);
    } catch (e) {
      err = `改波特率失败：${e}`;
    } finally {
      busy = false;
    }
  }
</script>

{#if open}
  <div class="overlay" role="presentation">
    <div
      class="dialog" role="dialog" aria-modal="true" aria-label="串口参数" tabindex="-1"
      bind:this={dialogEl} data-testid="serial-baud"
      onkeydown={(e) => { if (e.key === "Escape") onCancel(); }}
    >
      <h2>串口参数</h2>
      <p class="lead"><code data-testid="serial-baud-port">{port}</code> — 当前 <strong data-testid="serial-baud-current">{current}</strong></p>
      {#if err}<p class="err" role="alert" data-testid="serial-baud-error">{err}</p>{/if}
      <label>
        新波特率
        <input type="number" min="50" bind:value={baud} list="serial-baud-list" data-testid="serial-baud-input" />
      </label>
      <datalist id="serial-baud-list">
        {#each bauds as b (b)}<option value={b}></option>{/each}
      </datalist>
      <p class="note">
        立即生效，不重开端口——重开会抖一下 DTR，而很多板子的 DTR 接在复位脚上，
        那会让板子重启。数据位/校验/停止位要改的话在「会话属性」里改，需要重新连接。
      </p>
      <footer>
        <button bind:this={cancelBtn} onclick={onCancel} data-testid="serial-baud-cancel">取消</button>
        <button class="primary" disabled={!baud || busy} onclick={() => void apply()} data-testid="serial-baud-ok">
          {busy ? "切换中…" : "切换"}
        </button>
      </footer>
    </div>
  </div>
{/if}

<style>
  .overlay { position: fixed; inset: 0; background: rgba(0,0,0,.5); display: grid; place-items: center; z-index: 66; }
  .dialog {
    width: 380px; max-width: 92vw; max-height: 92vh; overflow-y: auto;
    padding: 16px 20px; background: var(--fs-bg-elevated);
    border: 1px solid var(--fs-border-strong); border-radius: var(--fs-radius);
    box-shadow: var(--fs-shadow); color: var(--fs-fg-primary);
    display: flex; flex-direction: column; gap: 10px;
  }
  .dialog h2 { margin: 0; font-size: 14px; }
  .lead { margin: 0; font-size: 12.5px; color: var(--fs-fg-secondary); }
  .lead code { font-family: var(--fs-font-mono, monospace); color: var(--fs-fg-primary); }
  .note { margin: 0; font-size: 11.5px; color: var(--fs-fg-secondary); }
  .err { margin: 0; font-size: 12px; color: var(--fs-danger); }
  label { display: flex; align-items: center; gap: 8px; font-size: 12.5px; }
  label input { flex: 1; }
  footer { display: flex; justify-content: flex-end; gap: 8px; }
  footer button { padding: 5px 14px; border: 1px solid var(--fs-border); background: var(--fs-bg-panel); color: var(--fs-fg-primary); border-radius: var(--fs-radius); cursor: pointer; }
  footer button.primary { background: var(--fs-accent); color: var(--fs-accent-fg); border-color: var(--fs-accent); }
  footer button:disabled { background: var(--fs-bg-panel); color: var(--fs-fg-disabled); border-color: var(--fs-border); cursor: not-allowed; }
  @media (forced-colors: active) {
    footer button.primary { outline: 2px solid Highlight; outline-offset: -2px; }
  }
</style>
