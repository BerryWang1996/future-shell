<script lang="ts">
  /**
   * 审计链校验（M3 出口第 7 项「快照校验命令可用」的界面那一半）。
   *
   * 三个动作对应三个后端命令（audit_cmd.rs）：
   * ① 打开即校验本机审计链（verify_chain：逐行重算 hash，报第一处断点）；
   * ② 导出取证包（forensics::export——**含命令文本**，见下面的导出提示）；
   * ③ 校验一份取证包文件（forensics::verify_json——不接触数据库，
   *    这是它作为「证据」的关键：不依赖本机这台可能正是案发现场的库）。
   *
   * # 校验失败不是「程序坏了」
   *
   * 链断了意味着 audit 表被改过（字段被改 / 行被删插）。断因的两句话
   * （content_altered / link_mismatch）各自指向不同的追查方向——那不是给
   * 用户的安慰，是给事后调查的第一条线索，故原样显示后端的 message。
   */
  import { useFocusTrap } from "../lib/focusTrap";
  import { invoke } from "../lib/ipc";
  import { toast } from "../lib/toast";

  let {
    open = false,
    onClose = () => {},
  }: {
    open?: boolean;
    onClose?(): void;
  } = $props();

  interface ChainReport {
    ok: boolean;
    rows: number;
    at_id: number | null;
    why: string | null;
    message: string;
  }
  interface BundleReport {
    usable: boolean;
    message: string;
  }
  /**
   * `audit_verify_quick` 的返回。**与 ChainReport 是两个类型，不共用一个变量。**
   *
   * 增量校验只重算了最近一次快照之后的行；快照与审计表同库同权限，证明不了
   * 它之前那一段。把两种结果塞进同一个 `chain` 变量，就是「增量被印成全量」
   * 这件事在 UI 上的发生方式——所以它们各有各的状态、各有各的显示块。
   */
  interface QuickReport {
    ok: boolean;
    full_coverage: boolean;
    at_id: number | null;
    message: string;
  }

  let dialogEl = $state<HTMLDivElement | undefined>();
  let chain = $state<ChainReport | null>(null);
  let quick = $state<QuickReport | null>(null);
  let busy = $state(false);
  let bundleResult = $state<BundleReport | null>(null);
  let fileInput = $state<HTMLInputElement | undefined>();

  $effect(() => {
    if (!open || !dialogEl) return;
    return useFocusTrap(dialogEl, { initial: dialogEl });
  });

  /** 每次打开都重验——链的状态只能代表「此刻」，上一次的绿不能带过来。 */
  $effect(() => {
    if (!open) return;
    chain = null;
    quick = null;
    bundleResult = null;
    void verify();
  });

  async function verify(): Promise<void> {
    busy = true;
    try {
      chain = await invoke<ChainReport>("audit_verify");
    } catch (e) {
      // 校验命令本身失败（库读不了）与「链断了」是两回事——后者在 chain.ok=false 里。
      toast.error(`审计校验失败：${e}`);
    } finally {
      busy = false;
    }
  }

  /**
   * 增量校验。**打开对话框时跑的仍是全表校验**——正确的默认不该让位于快的默认。
   * 这一条是给「表大到全表校验要等」的场合准备的显式选项，且它的结果自带
   * 覆盖范围声明（后端 message 无条件带，本层原样显示、不重写）。
   */
  async function quickVerify(): Promise<void> {
    busy = true;
    try {
      quick = await invoke<QuickReport>("audit_verify_quick");
    } catch (e) {
      toast.error(`增量校验失败：${e}`);
    } finally {
      busy = false;
    }
  }

  /**
   * 增量结果的图标。**`ok && !full_coverage` 给 ⚠ 而不是 ✓。**
   *
   * 「查过的那一段没问题」与「审计链完整」是两句不同的话。一个绿勾会被读成后者，
   * 而没查的那一段这次什么也没说。图标是这个区别在 UI 上唯一一眼能看见的地方。
   */
  function quickIcon(q: QuickReport): string {
    if (!q.ok) return "✗";
    return q.full_coverage ? "✓" : "⚠";
  }

  /**
   * 导出取证包。**不在点击后再弹一层确认**：forensics::export 的文档要求
   * 「导出前 UI 上明确提示包里含命令文本」——这里用按钮文案本身携带那句披露
   * （「导出取证包（含已执行的命令文本）」），加上对话框里常驻的警示段。
   * 两次确认的对话框教人快点按「确定」，常驻披露反而更难被略过。
   */
  async function exportBundle(): Promise<void> {
    busy = true;
    try {
      const json = await invoke<string>("audit_export");
      const url = URL.createObjectURL(new Blob([json], { type: "application/json" }));
      const a = document.createElement("a");
      a.href = url;
      a.download = `audit-bundle-${new Date().toISOString().slice(0, 10)}.json`;
      a.click();
      URL.revokeObjectURL(url);
      toast.info("取证包已导出");
    } catch (e) {
      toast.error(`导出失败：${e}`);
    } finally {
      busy = false;
    }
  }

  /** 校验一份取证包文件。读文件是前端的事（<input type="file">），命令只收 JSON 文本。 */
  async function onPickFile(e: Event): Promise<void> {
    const f = (e.currentTarget as HTMLInputElement).files?.[0];
    if (!f) return;
    busy = true;
    try {
      const text = await f.text();
      bundleResult = await invoke<BundleReport>("audit_verify_bundle", { content: text });
    } catch (err) {
      bundleResult = { usable: false, message: String(err) };
    } finally {
      busy = false;
      // 允许连选同一个文件再验一次：不重置 value 的话第二次 selectionchange 不触发。
      if (fileInput) fileInput.value = "";
    }
  }

</script>

{#if open}
  <div class="overlay" role="presentation" onclick={onClose}>
    <div
      class="panel"
      role="dialog"
      aria-modal="true"
      aria-label="审计链校验"
      tabindex="-1"
      data-testid="audit-dialog"
      bind:this={dialogEl}
      onclick={(e) => e.stopPropagation()}
      onkeydown={(e) => { if (e.key === "Escape") onClose(); }}
    >
      <header>
        <h2>审计链校验</h2>
        <button data-testid="audit-close" onclick={onClose} aria-label="关闭">✕</button>
      </header>

      <!-- 本机链状态。三条互斥态：未出结果 / 完整 / 断裂——「正在查」与「查不了」不共用一个态。 -->
      <section data-testid="audit-chain">
        <h3>本机审计链</h3>
        {#if busy && !chain}
          <p class="hint">校验中…</p>
        {:else if chain}
          {#if chain.ok}
            <p class="ok" data-testid="audit-chain-ok">✓ {chain.message}</p>
          {:else}
            <!-- 断裂态显式标红并复述断因：那句话是给事后调查的第一条线索，不是安慰。 -->
            <p class="bad" data-testid="audit-chain-broken" role="alert">✗ {chain.message}</p>
          {/if}
          <div class="row">
            <button data-testid="audit-recheck" disabled={busy} onclick={() => void verify()}>
              重新校验（全表）
            </button>
            <button data-testid="audit-quick" disabled={busy} onclick={() => void quickVerify()}>
              增量校验（自上次快照起）
            </button>
          </div>
        {:else}
          <p class="hint">尚未校验</p>
        {/if}

        <!-- 增量结果自成一块，不与全表结果共用显示位：两句结论的分量不同，
             叠在同一行上迟早会被读成同一件事。 -->
        {#if quick}
          <p
            class={quick.ok && quick.full_coverage ? "ok" : quick.ok ? "warn-line" : "bad"}
            data-testid="audit-quick-result"
            role={!quick.ok ? "alert" : undefined}
          >
            {quickIcon(quick)} {quick.message}
          </p>
        {/if}
      </section>

      <section>
        <h3>取证包</h3>
        <!-- 常驻披露（forensics::export 文档要求的「导出前明确提示」）：
             包里含命令文本，且这份包会离开本机。脱敏是形状匹配，
             挡不住一段看起来像散文的机密。 -->
        <p class="hint warn" data-testid="audit-export-warning">
          取证包包含<strong>已执行的命令文本</strong>，自动脱敏可能遗漏敏感内容。
          导出文件会保存在本机；分享后内容将离开本机，请在发送前核对。
        </p>
        <div class="row">
          <button data-testid="audit-export" disabled={busy} onclick={() => void exportBundle()}>
            导出取证包（含已执行的命令文本）
          </button>
          <!-- label 而不是隐藏 input + 按钮：屏幕阅读器要能报出「选一个文件来校验」。 -->
          <label class="pick" data-testid="audit-pick-label">
            <input
              type="file"
              accept="application/json,.json"
              bind:this={fileInput}
              disabled={busy}
              onchange={(e) => void onPickFile(e)}
            />
            校验一份取证包文件…
          </label>
        </div>
        {#if bundleResult}
          <p
            class={bundleResult.usable ? "ok" : "bad"}
            data-testid="audit-bundle-result"
            role={!bundleResult.usable ? "alert" : undefined}
          >
            {bundleResult.usable ? "✓" : "✗"} {bundleResult.message}
          </p>
        {/if}
      </section>
    </div>
  </div>
{/if}

<style>
  .overlay {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.45);
    display: grid;
    place-items: center;
    z-index: 60;
  }
  .panel {
    width: min(560px, 92vw);
    max-height: 84vh;
    overflow: auto;
    background: var(--fs-bg-elevated, var(--fs-bg-panel));
    color: var(--fs-fg-primary);
    border: 1px solid var(--fs-border);
    border-radius: var(--fs-radius-lg, 8px);
    box-shadow: 0 8px 32px rgba(0, 0, 0, 0.5);
    padding: 16px 20px 20px;
  }
  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 8px;
  }
  h2 {
    font-size: 15px;
    margin: 0;
  }
  h3 {
    font-size: 13px;
    margin: 14px 0 6px;
    color: var(--fs-fg-secondary);
  }
  section button,
  .pick {
    padding: 5px 12px;
    border: 1px solid var(--fs-border);
    background: var(--fs-bg-input);
    color: var(--fs-fg-primary);
    border-radius: var(--fs-radius);
    cursor: pointer;
    font: inherit;
    font-size: 12px;
  }
  section button:hover:not(:disabled),
  .pick:hover {
    background: var(--fs-bg-hover);
  }
  section button:disabled {
    color: var(--fs-fg-disabled);
    cursor: default;
  }
  .row {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
    align-items: center;
  }
  .pick input {
    display: none;
  }
  .hint {
    font-size: 12px;
    color: var(--fs-fg-secondary);
    margin: 6px 0;
  }
  .hint.warn {
    color: var(--fs-warn, #d89614);
    border: 1px solid var(--fs-warn, #d89614);
    border-radius: var(--fs-radius);
    padding: 8px;
  }
  .ok {
    color: var(--fs-ok, #3fb950);
    font-size: 13px;
    margin: 6px 0;
  }
  .bad {
    color: var(--fs-danger, #d32029);
    font-size: 13px;
    margin: 6px 0;
  }
  /* 「通过但没查全」既不是绿也不是红——它是一句需要读完的话。 */
  .warn-line {
    color: var(--fs-warn, #d89614);
    font-size: 13px;
    margin: 6px 0;
  }
  header button {
    border: none;
    background: none;
    color: var(--fs-fg-secondary);
    cursor: pointer;
    font-size: 14px;
  }
</style>
