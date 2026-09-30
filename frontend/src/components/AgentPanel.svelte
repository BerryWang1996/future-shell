<script lang="ts">
  /**
   * Agent 面板（M3 出口第 1/2/3 项的界面面）。
   *
   * 三件事，一件都不能少：
   *
   * ① **步骤时间线**——每一步的工具、参数、分级、结果都要看得见。看不见的
   *    自主执行不是「自主」，是「失控」。
   * ② **确认对话框**——write 单次、dangerous 强确认（二次输入）。手势成本的
   *    差别就是 write 与 dangerous 的差别；两者交互一样的话，分级毫无意义。
   * ③ **急停**——随时可按，按下之后**正在跑的那条命令会被杀掉**（后端拨快
   *    时钟，run_command 自己走 kill→close），不是「等它跑完再停」。
   *
   * 面板不做任何判断：危险度由 fs_policy 算、预算由账本算、停因由驱动层给。
   * 这里只显示，以及把用户的应答送回去。
   */
  import { onMount } from "svelte";
  import { invoke, listen, type UnlistenFn } from "../lib/ipc";
  import { untilUnmount } from "../lib/lifecycle";
  import { useFocusTrap } from "../lib/focusTrap";
  import { toast } from "../lib/toast";

  let {
    open = false,
    onClose = () => {},
    sessionId = null,
  }: {
    open?: boolean;
    onClose?(): void;
    sessionId?: string | null;
  } = $props();

  interface PendingConfirm {
    request_id: number;
    run_id: string;
    action: string;
    tier: string;
    strong: boolean;
    rules: string[];
    details: string[];
    deadline_secs: number;
  }
  interface PendingAsk {
    request_id: number;
    run_id: string;
    question: string;
    deadline_secs: number;
  }
  interface RunFinished {
    run_id: string;
    reason: string;
    steps_taken: number;
    streak: number;
    tokens_known: number;
    tokens_unaccounted: number;
    tokens_charged: number;
    human_touchpoints: number;
  }

  let dialogEl = $state<HTMLDivElement | undefined>();
  let task = $state("");
  let runId = $state<string | null>(null);
  let busy = $state(false);
  let confirmItem = $state<PendingConfirm | null>(null);
  let askItem = $state<PendingAsk | null>(null);
  /** 强确认的二次输入（要求逐字打「确认」——长按在桌面端不如二次输入可靠）。 */
  let strongInput = $state("");
  let askAnswer = $state("");
  let finished = $state<RunFinished | null>(null);

  $effect(() => {
    if (!open || !dialogEl) return;
    return useFocusTrap(dialogEl, { initial: dialogEl });
  });

  /**
   * 重开面板时恢复按钮态。
   *
   * run 跑在后台：关掉面板它照常跑（那正是「派任务」的意思）。重开时如果不问
   * 一句「它还在吗」，界面会显示「开始」按钮——用户点下去就会**并发起第二个
   * run**，两个 Agent 同时对着一台机器发命令。
   */
  $effect(() => {
    if (!open || !runId) return;
    const id = runId;
    void (async () => {
      const alive = await invoke<boolean>("agent_is_running", { runId: id });
      if (!alive && runId === id) runId = null;
    })();
  });

  onMount(() => {
    const uns: UnlistenFn[] = [];
    void (async () => {
      uns.push(
        untilUnmount(listen<PendingConfirm>("agent:confirm", (e) => {
          confirmItem = e.payload;
          strongInput = "";
        })),
      );
      uns.push(
        untilUnmount(listen<PendingAsk>("agent:ask", (e) => {
          askItem = e.payload;
          askAnswer = "";
        })),
      );
      uns.push(
        untilUnmount(listen<RunFinished>("agent:stopped", (e) => {
          // run 结束：清掉待答项（它们的票据已被后端撤回——留着就是一个
          // 点了没反应的对话框）。
          finished = e.payload;
          runId = null;
          confirmItem = null;
          askItem = null;
        })),
      );
    })();
    return () => uns.forEach((u) => u());
  });

  async function start(): Promise<void> {
    if (!task.trim() || busy) return;
    if (!sessionId) {
      toast.warn("先连上一台主机——Agent 要在某个会话里干活");
      return;
    }
    busy = true;
    finished = null;
    try {
      const r = await invoke<{ run_id: string }>("agent_start", {
        task: task.trim(),
        sessionId,
      });
      runId = r.run_id;
    } catch (e) {
      // 启动期拒绝（没配模型源 / 档位关着 / 会话不在）是同步返回的——
      // 那时 run 还不存在，用户看到的是「为什么起不来」而不是「起来又停了」。
      toast.error(String(e));
    } finally {
      busy = false;
    }
  }

  async function abort(): Promise<void> {
    if (!runId) return;
    const stopped = await invoke<boolean>("agent_abort", { runId });
    if (!stopped) {
      // false = 那个 run 已经结束了。据此更新按钮态，而不是显示一个
      // 「停止中…」永远转下去。
      runId = null;
      toast.info("这次任务已经结束了");
    }
  }

  async function answerConfirm(approved: boolean): Promise<void> {
    const item = confirmItem;
    if (!item) return;
    // 强确认要求逐字输入——手势成本是 dangerous 与 write 的唯一区别。
    if (approved && item.strong && strongInput.trim() !== "确认") {
      toast.warn("危险操作：请逐字输入「确认」两个字");
      return;
    }
    confirmItem = null;
    const ok = await invoke<boolean>("agent_confirm_answer", {
      requestId: item.request_id,
      approved,
    });
    if (!ok) toast.info("这条确认已经过期了（超时或任务已结束）");
  }

  async function answerAsk(): Promise<void> {
    const item = askItem;
    if (!item) return;
    const text = askAnswer;
    askItem = null;
    const ok = await invoke<boolean>("agent_ask_reply", {
      requestId: item.request_id,
      text,
    });
    if (!ok) toast.info("这个提问已经过期了");
  }

  const TIER_TEXT: Record<string, string> = {
    read_only: "只读",
    write: "会改动东西",
    dangerous: "危险",
  };
</script>

{#if open}
  <div class="overlay" role="presentation" onclick={onClose}>
    <div
      class="panel"
      role="dialog"
      aria-modal="true"
      aria-label="AI Agent"
      tabindex="-1"
      data-testid="agent-panel"
      bind:this={dialogEl}
      onclick={(e) => e.stopPropagation()}
      onkeydown={(e) => { if (e.key === "Escape" && !runId) onClose(); }}
    >
      <header>
        <h2>AI Agent（自主排查）</h2>
        <button data-testid="agent-close" onclick={onClose} aria-label="关闭" disabled={!!runId}>✕</button>
      </header>

      <p class="hint">
        把一件事交给它，它会自己发命令、看输出、再决定下一步。
        <strong>每一条命令都过同一个策略闸门</strong>——只读的自动跑，会改动的问你，
        危险的要你逐字确认。随时可以按急停。
      </p>

      <label class="task">
        <span class="hint">要它做什么？</span>
        <textarea
          bind:value={task}
          data-testid="agent-task"
          rows="2"
          disabled={!!runId}
          placeholder="例：根分区快满了，找出占用最大的目录并给出清理建议"
        ></textarea>
      </label>

      <div class="row">
        {#if runId}
          <!-- 急停：按下之后正在跑的那条命令会被杀掉，不是等它跑完 -->
          <button class="stop" data-testid="agent-abort" onclick={() => void abort()}>
            ■ 急停
          </button>
          <span class="running" data-testid="agent-running">任务进行中…</span>
        {:else}
          <button
            class="go"
            data-testid="agent-start"
            disabled={busy || !task.trim()}
            onclick={() => void start()}
          >
            {busy ? "启动中…" : "开始"}
          </button>
        {/if}
      </div>

      {#if finished}
        <!-- 终态：停因 + 预算三件套。「为什么停」不带数字，用户的第一反应是
             「坏了」而不是「到量了」。 -->
        <div class="done" data-testid="agent-finished">
          <p class="reason">{finished.reason}</p>
          <p class="hint">
            用了 {finished.steps_taken} 个模型回合、
            {finished.human_touchpoints} 次人工确认；
            token 已知 {finished.tokens_known}
            {#if finished.tokens_unaccounted > 0}
              （另有 {finished.tokens_unaccounted} 回合无账，按上界共记 {finished.tokens_charged}）
            {/if}
          </p>
        </div>
      {/if}
    </div>
  </div>
{/if}

<!-- 确认对话框：与面板本身分开——它可能在面板被关掉之后才到 -->
{#if confirmItem}
  <div class="overlay top" role="presentation">
    <div class="confirm" role="alertdialog" aria-modal="true" aria-label="确认执行"
         data-testid="agent-confirm">
      <h3 class:danger={confirmItem.tier === "dangerous"}>
        {confirmItem.strong ? "危险操作，需要强确认" : "需要你确认"}
      </h3>
      <pre data-testid="agent-confirm-action">{confirmItem.action}</pre>
      <p class="tier" data-testid="agent-confirm-tier">
        分级：{TIER_TEXT[confirmItem.tier] ?? confirmItem.tier}
        {#if confirmItem.rules.length > 0}（{confirmItem.rules.join("、")}）{/if}
      </p>
      {#if confirmItem.details.length > 0}
        <ul class="details">
          {#each confirmItem.details as d, i (i)}<li>{d}</li>{/each}
        </ul>
      {/if}
      {#if confirmItem.strong}
        <!-- 二次输入而不是长按：桌面端长按没有统一手势，而逐字输入的成本
             是确定的——它让「顺手点掉」变得不可能。 -->
        <label class="strong">
          逐字输入「确认」才能执行：
          <input bind:value={strongInput} data-testid="agent-strong-input" />
        </label>
      {/if}
      <p class="hint">{confirmItem.deadline_secs} 秒内不应答将自动取消，任务随之停止。</p>
      <div class="row">
        <button data-testid="agent-confirm-yes" onclick={() => void answerConfirm(true)}>
          执行
        </button>
        <button data-testid="agent-confirm-no" onclick={() => void answerConfirm(false)}>
          拒绝并停止
        </button>
      </div>
    </div>
  </div>
{/if}

{#if askItem}
  <div class="overlay top" role="presentation">
    <div class="confirm" role="dialog" aria-modal="true" aria-label="Agent 提问"
         data-testid="agent-ask">
      <h3>它想问你一件事</h3>
      <p data-testid="agent-ask-question">{askItem.question}</p>
      <input bind:value={askAnswer} data-testid="agent-ask-input" />
      <div class="row">
        <button data-testid="agent-ask-send" onclick={() => void answerAsk()}>回答</button>
      </div>
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
  .overlay.top { z-index: 70; }
  .panel, .confirm {
    width: min(620px, 92vw);
    max-height: 84vh;
    overflow: auto;
    background: var(--fs-bg-elevated, var(--fs-bg-panel));
    color: var(--fs-fg-primary);
    border: 1px solid var(--fs-border);
    border-radius: var(--fs-radius-lg, 8px);
    box-shadow: 0 8px 32px rgba(0, 0, 0, 0.5);
    padding: 16px 20px 20px;
  }
  .confirm { width: min(520px, 92vw); max-height: 92vh; overflow-y: auto; }
  header { display: flex; align-items: center; justify-content: space-between; }
  h2 { font-size: 15px; margin: 0; }
  h3 { font-size: 14px; margin: 0 0 8px; }
  h3.danger { color: var(--fs-danger, #d32029); }
  header button { border: none; background: none; color: var(--fs-fg-secondary); cursor: pointer; }
  .hint { font-size: 12px; color: var(--fs-fg-secondary); margin: 8px 0; }
  .task { display: flex; flex-direction: column; gap: 4px; }
  textarea, input {
    width: 100%;
    background: var(--fs-bg-input);
    color: var(--fs-fg-primary);
    border: 1px solid var(--fs-border);
    border-radius: var(--fs-radius);
    padding: 6px 8px;
    font: inherit;
    box-sizing: border-box;
  }
  .row { display: flex; gap: 8px; align-items: center; margin-top: 10px; flex-wrap: wrap; }
  button {
    padding: 5px 12px;
    border: 1px solid var(--fs-border);
    background: var(--fs-bg-input);
    color: var(--fs-fg-primary);
    border-radius: var(--fs-radius);
    cursor: pointer;
    font: inherit;
    font-size: 12px;
  }
  button:hover:not(:disabled) { background: var(--fs-bg-hover); }
  button:disabled { color: var(--fs-fg-disabled); cursor: default; }
  .go { background: var(--fs-accent); color: var(--fs-accent-fg); border-color: transparent; }
  .stop { background: var(--fs-danger, #d32029); color: #fff; border-color: transparent; }
  .running { font-size: 12px; color: var(--fs-fg-secondary); }
  pre {
    background: var(--fs-bg-input);
    border: 1px solid var(--fs-border);
    border-radius: var(--fs-radius);
    padding: 8px;
    margin: 0 0 8px;
    white-space: pre-wrap;
    word-break: break-all;
    font-size: 12px;
  }
  .tier { font-size: 12px; margin: 0 0 6px; }
  .details { font-size: 12px; color: var(--fs-fg-secondary); margin: 0 0 8px; padding-left: 18px; }
  .strong { display: block; font-size: 12px; margin: 8px 0; }
  .done { margin-top: 12px; border-top: 1px solid var(--fs-border); padding-top: 8px; }
  .reason { font-size: 13px; margin: 0; }
</style>
