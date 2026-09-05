<script lang="ts">
  /**
   * 双击远端 `.sh` 之前的那一步（M7.3 出口标准②）。
   *
   * 出口原文：「双击 `.sh` 前展示**脚本原文**并让用户选前台/后台，前台输出进新终端标签，
   * 后台明确告知『关掉本程序它仍在跑』」。用户 2026-08-28 原话还加了一条：
   * **不得替用户默认**——所以两个单选**都不预选**，「运行」在选定之前一直是禁用的。
   *
   * # 与 M7.2 统一确认口径的关系
   *
   * 这就是 `script.run` 那一类的确认框，不是另一套：它读同一个豁免集、写同一条审计
   * （`audit_dangerous_action`）。豁免掉的只是**确认语气与勾选框**——前台/后台的选择照旧要做，
   * 因为那是**参数**不是确认，替他选一个等于替他决定「这个脚本会不会在你关掉程序之后继续跑」。
   *
   * # 原文被截断时必须说
   *
   * 用户是拿这段原文来决定要不要执行的。「你看到的不是全部」这件事静默处理，
   * 等于让他在不完整的信息上做一个不可撤销的决定。
   */
  import { useFocusTrap } from "../lib/focusTrap";
  import { invoke } from "../lib/ipc";
  import { loadSuppressed, saveSuppressed } from "../lib/confirm-gate";
  import { withSuppressed } from "../lib/action-confirm";

  let {
    open = false,
    /** 远端脚本完整路径。 */
    path = "",
    sessionId = null,
    /** 用户选定后的回调；`mode` 是他当次选的那一档。 */
    onRun = (_mode: "foreground" | "background") => {},
    onCancel = () => {},
  }: {
    open?: boolean;
    path?: string;
    sessionId?: string | null;
    onRun?(mode: "foreground" | "background"): void;
    onCancel?(): void;
  } = $props();

  let dialogEl = $state<HTMLDivElement | undefined>();
  let cancelBtn = $state<HTMLButtonElement | undefined>();
  /** null = 还没选（出口原文：不得替用户默认）。 */
  let mode = $state<"foreground" | "background" | null>(null);
  let text = $state<string | null>(null);
  let truncated = $state(false);
  let totalBytes = $state(0);
  let err = $state("");
  let suppress = $state(false);
  /** 这一类是否已被豁免确认（豁免只影响确认语气，不影响前台/后台的选择）。 */
  let alreadySuppressed = $state(false);

  $effect(() => {
    if (!open) return;
    // 每次打开都从零开始：上一个脚本选过「后台」不代表这一个也要后台。
    mode = null;
    text = null;
    err = "";
    suppress = false;
    truncated = false;
    void (async () => {
      alreadySuppressed = (await loadSuppressed()).includes("script.run");
      try {
        const r = await invoke<{ text: string; truncated: boolean; total_bytes: number }>(
          "sftp_read_text",
          { sessionId, path },
        );
        text = r.text;
        truncated = r.truncated;
        totalBytes = r.total_bytes;
      } catch (e) {
        err = `读不到脚本原文：${e}`;
        text = "";
      }
    })();
  });

  $effect(() => {
    if (!open || !dialogEl || !cancelBtn) return;
    return useFocusTrap(dialogEl, { initial: cancelBtn });
  });

  async function run() {
    if (!mode) return;
    if (!alreadySuppressed && suppress) {
      await saveSuppressed(withSuppressed(await loadSuppressed(), "script.run", true));
    }
    onRun(mode);
  }

  const fmtBytes = (n: number) => (n < 1024 ? `${n} B` : n < 1024 * 1024 ? `${Math.round(n / 1024)} KiB` : `${(n / 1048576).toFixed(1)} MiB`);
</script>

{#if open}
  <div class="overlay" role="presentation">
    <div
      class="dialog" role="dialog" aria-modal="true" aria-label="运行远端脚本" tabindex="-1"
      bind:this={dialogEl} data-testid="script-run"
      onkeydown={(e) => { if (e.key === "Escape") onCancel(); }}
    >
      <h2>运行远端脚本</h2>
      <p class="path" data-testid="script-run-path">{path}</p>

      {#if err}
        <p class="err" role="alert" data-testid="script-run-error">{err}</p>
      {/if}
      <p class="lead">脚本原文：</p>
      <pre class="code" data-testid="script-run-text">{text ?? "读取中…"}</pre>
      {#if truncated}
        <p class="warn" data-testid="script-run-truncated">
          原文超过预览上限，以上不是全部内容（文件共 {fmtBytes(totalBytes)}）。要看全请先下载。
        </p>
      {/if}

      <fieldset class="mode">
        <legend>怎么运行（必须选一个）</legend>
        <label>
          <input type="radio" name="script-mode" value="foreground" data-testid="script-run-fg"
                 checked={mode === "foreground"} onchange={() => (mode = "foreground")} />
          前台：新开一个终端标签，输出直接看得到；关掉标签即中断
        </label>
        <label>
          <input type="radio" name="script-mode" value="background" data-testid="script-run-bg"
                 checked={mode === "background"} onchange={() => (mode = "background")} />
          后台：脱离本次连接运行（nohup），输出写到远端当前目录的 nohup.out
        </label>
        {#if mode === "background"}
          <!-- 出口原文点名要「明确告知」。这句话是后台与前台唯一的实质差别，也是唯一会留下
               「没人管的远端常驻进程」的那一档，必须在按下运行之前看到。 -->
          <p class="warn" data-testid="script-run-bg-warning">
            ⚠ 关掉本程序它仍在跑。不会自己停止的脚本会变成一个没人管的远端常驻进程，
            之后只能靠「监控 → 进程」页签找到并终止它。
          </p>
        {/if}
      </fieldset>

      {#if !alreadySuppressed}
        <label class="suppress">
          <input type="checkbox" bind:checked={suppress} data-testid="script-run-suppress" />
          以后不再对「运行远端脚本」显示此确认（前台/后台仍会问）
        </label>
      {/if}

      <footer>
        <button bind:this={cancelBtn} onclick={onCancel} data-testid="script-run-cancel">取消</button>
        <button class="primary danger" disabled={!mode || text === null}
                onclick={() => void run()} data-testid="script-run-ok">运行</button>
      </footer>
    </div>
  </div>
{/if}

<style>
  .overlay { position: fixed; inset: 0; background: rgba(0,0,0,.5); display: grid; place-items: center; z-index: 66; }
  .dialog {
    width: 620px; max-width: 94vw; max-height: 92vh; overflow-y: auto;
    padding: 16px 20px; background: var(--fs-bg-elevated);
    border: 1px solid var(--fs-border-strong); border-radius: var(--fs-radius);
    box-shadow: var(--fs-shadow); color: var(--fs-fg-primary);
    display: flex; flex-direction: column; gap: 10px;
  }
  .dialog h2 { margin: 0; font-size: 14px; }
  .path { margin: 0; font-family: var(--fs-font-mono, monospace); font-size: 12px; color: var(--fs-fg-secondary); overflow-wrap: anywhere; }
  .lead { margin: 0; font-size: 12px; color: var(--fs-fg-secondary); }
  .code {
    margin: 0; padding: 8px 10px; max-height: 34vh; overflow-y: auto;
    background: var(--fs-bg-app); border: 1px solid var(--fs-border); border-radius: var(--fs-radius);
    font-family: var(--fs-font-mono, monospace); font-size: 12px; line-height: 1.5;
    white-space: pre-wrap; overflow-wrap: anywhere; user-select: text;
  }
  .warn { margin: 0; font-size: 12px; color: var(--fs-warn); white-space: pre-wrap; }
  .err { margin: 0; font-size: 12px; color: var(--fs-danger); }
  .mode { border: 1px solid var(--fs-border); border-radius: var(--fs-radius); padding: 8px 10px; display: flex; flex-direction: column; gap: 6px; }
  .mode legend { font-size: 12px; color: var(--fs-fg-secondary); padding: 0 4px; }
  .mode label { display: flex; align-items: flex-start; gap: 6px; font-size: 12.5px; cursor: pointer; }
  .suppress { display: flex; align-items: center; gap: 6px; font-size: 12.5px; cursor: pointer; }
  footer { display: flex; justify-content: flex-end; gap: 8px; }
  footer button { padding: 5px 14px; border: 1px solid var(--fs-border); background: var(--fs-bg-panel); color: var(--fs-fg-primary); border-radius: var(--fs-radius); cursor: pointer; }
  footer button.primary { background: var(--fs-accent); color: var(--fs-accent-fg); border-color: var(--fs-accent); }
  footer button.danger { background: var(--fs-danger); border-color: var(--fs-danger); color: #fff; }
  footer button:disabled { background: var(--fs-bg-panel); color: var(--fs-fg-disabled); border-color: var(--fs-border); cursor: not-allowed; }
  /* 高对比度（房规见 styles.css）：主/危险按钮的强调全靠底色，强制颜色下与取消按钮无从分辨。 */
  @media (forced-colors: active) {
    footer button.primary, footer button.danger { outline: 2px solid Highlight; outline-offset: -2px; }
  }
</style>
