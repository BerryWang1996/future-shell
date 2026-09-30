<script lang="ts">
  /**
   * 换终端编码（M7.4）。
   *
   * # 为什么这是个对话框而不是一个下拉菜单
   *
   * 换编码要说清两件事，而下拉菜单里没地方说：
   * ① **立即生效、不需要重连**——用户的默认预期是「改编码要重连」，不说他不会当场试；
   * ② 这次改的是**这一条会话**，不是档案。想让下次也这样得去连接属性里存。
   * 两件事都不说的话，用户会以为自己改坏了配置，或者反过来以为已经存下了。
   *
   * # 清单由后端给
   *
   * 能不能解出来是 Rust 侧 `StreamDecoder` 说了算（`term_encodings` IPC）。前端另写一份
   * 迟早分叉，而分叉的那一半就是「列表里有、选了却报错」。
   */
  import { useFocusTrap } from "../lib/focusTrap";
  import { invoke } from "../lib/ipc";

  let {
    open = false,
    sessionId = null,
    /** 当前编码的规范名（后端回报的那个）。 */
    current = "UTF-8",
    onChanged = (_name: string) => {},
    onCancel = () => {},
  }: {
    open?: boolean;
    sessionId?: string | null;
    current?: string;
    onChanged?(name: string): void;
    onCancel?(): void;
  } = $props();

  let dialogEl = $state<HTMLDivElement | undefined>();
  let cancelBtn = $state<HTMLButtonElement | undefined>();
  let list = $state<[string, string][]>([]);
  let picked = $state("");
  let err = $state("");
  let busy = $state(false);

  $effect(() => {
    if (!open) return;
    err = "";
    picked = "";
    void (async () => {
      try {
        list = await invoke<[string, string][]>("term_encodings");
      } catch (e) {
        err = `取不到编码清单：${e}`;
      }
    })();
  });

  $effect(() => {
    if (!open || !dialogEl || !cancelBtn) return;
    return useFocusTrap(dialogEl, { initial: cancelBtn });
  });

  async function apply() {
    if (!picked || !sessionId) return;
    busy = true;
    err = "";
    try {
      const name = await invoke<string>("term_set_encoding", { sessionId, label: picked });
      onChanged(name);
    } catch (e) {
      // 失败要留在框里说清楚：关掉之后用户只会看到编码没变而不知道为什么
      err = `切换失败：${e}`;
    } finally {
      busy = false;
    }
  }
</script>

{#if open}
  <div class="overlay" role="presentation">
    <div
      class="dialog" role="dialog" aria-modal="true" aria-label="终端编码" tabindex="-1"
      bind:this={dialogEl} data-testid="encoding-dialog"
      onkeydown={(e) => { if (e.key === "Escape") onCancel(); }}
    >
      <h2>终端编码</h2>
      <p class="lead">当前：<strong data-testid="encoding-current">{current}</strong></p>
      {#if err}<p class="err" role="alert" data-testid="encoding-error">{err}</p>{/if}
      <ul class="list">
        {#each list as [value, human] (value)}
          <li>
            <label>
              <input type="radio" name="encoding" {value} checked={picked === value}
                     data-testid="encoding-opt-{value}" onchange={() => (picked = value)} />
              {human}
            </label>
          </li>
        {/each}
      </ul>
      <p class="note">
        改动立即生效，不需要重连；只作用于这一条会话。想让下次连接也用它，
        请在「会话属性 → 终端 → 编码」里保存。
      </p>
      <footer>
        <button bind:this={cancelBtn} onclick={onCancel} data-testid="encoding-cancel">取消</button>
        <button class="primary" disabled={!picked || busy} onclick={() => void apply()} data-testid="encoding-ok">
          {busy ? "切换中…" : "切换"}
        </button>
      </footer>
    </div>
  </div>
{/if}

<style>
  .overlay { position: fixed; inset: 0; background: rgba(0,0,0,.5); display: grid; place-items: center; z-index: 66; }
  .dialog {
    width: 420px; max-width: 92vw; max-height: 92vh; overflow-y: auto;
    padding: 16px 20px; background: var(--fs-bg-elevated);
    border: 1px solid var(--fs-border-strong); border-radius: var(--fs-radius);
    box-shadow: var(--fs-shadow); color: var(--fs-fg-primary);
    display: flex; flex-direction: column; gap: 10px;
  }
  .dialog h2 { margin: 0; font-size: 14px; }
  .lead { margin: 0; font-size: 12.5px; color: var(--fs-fg-secondary); }
  .note { margin: 0; font-size: 11.5px; color: var(--fs-fg-secondary); }
  .err { margin: 0; font-size: 12px; color: var(--fs-danger); }
  .list { list-style: none; margin: 0; padding: 0; display: flex; flex-direction: column; gap: 4px; }
  .list label { display: flex; align-items: center; gap: 6px; font-size: 12.5px; cursor: pointer; }
  footer { display: flex; justify-content: flex-end; gap: 8px; }
  footer button { padding: 5px 14px; border: 1px solid var(--fs-border); background: var(--fs-bg-panel); color: var(--fs-fg-primary); border-radius: var(--fs-radius); cursor: pointer; }
  footer button.primary { background: var(--fs-accent); color: var(--fs-accent-fg); border-color: var(--fs-accent); }
  footer button:disabled { background: var(--fs-bg-panel); color: var(--fs-fg-disabled); border-color: var(--fs-border); cursor: not-allowed; }
  @media (forced-colors: active) {
    footer button.primary { outline: 2px solid Highlight; outline-offset: -2px; }
  }
</style>
