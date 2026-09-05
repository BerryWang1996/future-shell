<script lang="ts">
  import { invoke } from "../lib/ipc";
  import { untilUnmount } from "../lib/lifecycle";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { useFocusTrap } from "../lib/focusTrap";
  import { toast } from "../lib/toast";
  import type { HostKeyPromptPayload } from "../lib/types";

  /**
   * 一条待确认的提问。字段全部来自事件载荷，组件不做任何解析。
   * `promptId` 是后端待决表 `(session_id, prompt_id)` 的后半截，对前端只是不透明令牌。
   */
  type Pending = {
    sessionId: string;
    promptId: string | null;
    host: string;
    port: number;
    keyType: string;
    fingerprint: string;
    kind: "tofu" | "changed";
    oldFingerprint: string | null;
  };

  /**
   * 待确认队列（审计 P1「前端单槽 prompt 覆盖」）。
   *
   * 原实现是**单槽**：每来一条 `hostkey:prompt` 就把 sessionId/host/fingerprint… 逐个覆写。
   * 后端那侧早已按 `(session_id, prompt_id)` 支持多枚并存（见 events.rs 的 PendingPrompts），
   * 并发也确实会发生——两个会话各自跑重连循环（session_cmd.rs 的 `reconnect_loop`）、
   * 一条跳板链上多跳先后要确认，都会在几乎同一时刻发出两条提问。
   *
   * 单槽下的后果不是「少弹一个框」，而是**确认框被调包**：三个按钮的文字、位置、样式逐字相同，
   * 只有指纹那一行变了。用户正要点「接受并记录」的那一下，落到的是另一台主机的确认上；
   * 而被换掉的那一枚无人应答，一直挂到后端 120 s 超时（表现为连接莫名卡住）。
   *
   * 队列的不变量：**已显示的提问只由用户的裁决推进，任何后到的事件都不得替换它**。
   *
   * 残留风险（有意保留、不再加计时器）：答完一条后下一条立即就位，理论上一次双击的第二下
   * 会落到新框上。但这与修复前有本质区别——替换只发生在用户自己的点击**之内**，
   * 而不是任意时刻凭空发生，窗口从「无界」收敛到「一次点击的间隔」。
   */
  let queue = $state<Pending[]>([]);
  const cur = $derived(queue.length ? queue[0] : null);

  // bind:this 的目标必须是 $state：否则赋值不进入响应式图，下面的 $effect 不会因绑定完成而重跑，
  // 焦点陷阱拿到的是 undefined —— 主机密钥变更这种高危确认框却不抢焦点，等于诱导误操作。
  let dialogEl = $state<HTMLDivElement | undefined>();
  let rejectBtn = $state<HTMLButtonElement | undefined>();
  let acceptBtn = $state<HTMLButtonElement | undefined>();

  let unlisten: UnlistenFn | null = null;
  $effect(() => {
    unlisten = untilUnmount(listen<HostKeyPromptPayload>("hostkey:prompt", (e) => {
      // 一律追加到队尾。这里**没有**「若当前空闲则直接显示」的分支：显示与否完全由
      // `cur` 派生，入队逻辑因此不可能写出「顺手覆盖当前项」的变体。
      queue = [
        ...queue,
        {
          sessionId: e.payload.session_id,
          promptId: e.payload.promptId ?? null,
          host: e.payload.host,
          port: e.payload.port,
          keyType: e.payload.key_type,
          fingerprint: e.payload.fingerprint,
          kind: e.payload.kind,
          oldFingerprint: e.payload.old_fingerprint ?? null,
        },
      ];
    }));
    return () => { unlisten?.(); };
  });

  $effect(() => {
    const c = cur;
    // 三个 ref 齐了才装陷阱：三次 bind:this 赋值同属一次刷新，齐读可避免装了又拆的抖动
    // （拆除会把焦点归还背景元素）。
    if (!c || !dialogEl || !rejectBtn || !acceptBtn) return;
    // 读 promptId 把「换到下一条提问」也纳入依赖：内容换了焦点却仍停在上一条的按钮上，
    // 等于把用户的回车留在了错误的答案上。
    void c.promptId;
    // 密钥变更（changed）默认落在「拒绝」上：默认焦点即默认答案，高危分支不能默认接受。
    return useFocusTrap(dialogEl, { initial: c.kind === "changed" ? rejectBtn : acceptBtn });
  });

  /**
   * 裁决取值必须与后端 `parse_hostkey_choice`（app/src/commands/auth_cmd.rs）逐字一致：
   * `accept_record` / `accept_once` / `refuse`（计划 §Task 17 契约）。
   *
   * 此处曾发 `accept_persist` 与 `reject`，与后端认的 `accept_record` / `refuse` 各差一个词。
   * 后端当时还留着 `_ => Refuse` 的 catch-all，于是**每一次首连 TOFU 的「接受并记录」都被
   * 静默翻译成拒绝**：前端没报错、后端没打日志、两侧类型各自自洽，唯一的线索是两个字符串
   * 字面量对不上，而它们分处 TypeScript 与 Rust，谁也看不见谁。
   * 后端现已改为契约外取值即报错，故本联合类型是硬约束而非风格偏好——改动务必同步两侧。
   */
  type HostKeyChoice = "accept_record" | "accept_once" | "refuse";

  /** 把裁决投给**指定那一枚**提问。参数取的是出队时抓住的快照，与队列后续变化无关。 */
  async function send(p: Pending, choice: HostKeyChoice) {
    try {
      await invoke("hostkey_decide", { sessionId: p.sessionId, promptId: p.promptId, choice });
    } catch (e) {
      console.error("hostkey_decide 投递失败", e);
      toast.error("主机密钥裁决提交失败，连接可能已中断");
    }
  }

  function decide(choice: HostKeyChoice) {
    const head = queue[0];
    if (!head) return;
    // 先出队再投递：出队是同步的，能挡住 await 期间的连点（第二次投递会因待决 prompt
    // 已被取走而报错，还可能把「拒绝」盖成「接受」的错觉留给用户）。
    queue = queue.slice(1);
    void send(head, choice);
  }

  /**
   * Vault 自动锁定（`vault:locked`）时由 App.svelte 调用：等价于用户按下「拒绝」。
   * 主机密钥确认是安全裁决，人已离开机器就不能把一个待接受的高危弹窗留在屏幕上；
   * 与后端超时同向（超时也是 refusing），只是即时生效而非等满超时。
   *
   * **整队**拒绝而非只拒当前这一枚：排在后面的提问同样是「人不在机器前」时收到的，
   * 留着它们只会在解锁后弹出一串来历不明的确认框，且各自把连接挂到超时。
   */
  export function dismissForVaultLock() {
    const all = queue;
    queue = [];
    for (const p of all) void send(p, "refuse");
  }
</script>

{#if cur}
  <div class="overlay" role="presentation">
    <!-- tabindex="-1"：role="dialog" 需可聚焦（a11y_interactive_supports_focus），
         负值使其可被 focus() 定位但不占 Tab 序列。 -->
    <div class="dialog" role="dialog" aria-modal="true" aria-label="主机密钥确认" tabindex="-1"
         data-testid="hostkey-dialog" bind:this={dialogEl}
         onkeydown={(e) => { if (e.key === "Escape") decide("refuse"); }}>
      <h2>
        主机密钥确认
        <!-- 队列深度必须可见：否则用户答完一个又冒出一个一模一样的框，第一反应是
             「刚才那下没点上」，于是照着惯性再点一次——正是最不该发生的那种点击。 -->
        {#if queue.length > 1}
          <span class="queued" data-testid="hk-queued">还有 {queue.length - 1} 个待确认</span>
        {/if}
      </h2>
      {#if cur.kind === "changed"}
        <div class="warn" role="alert">
          <strong>警告：主机密钥已变更</strong>
          <p>这可能意味着服务器重新生成了密钥，也可能是中间人攻击。请谨慎确认。</p>
        </div>
        <dl>
          <dt>主机</dt><dd data-testid="hk-host">{cur.host}:{cur.port}</dd>
          <dt>密钥类型</dt><dd>{cur.keyType}</dd>
          <dt>旧密钥指纹</dt><dd><code>{cur.oldFingerprint}</code></dd>
          <dt>新密钥指纹</dt><dd><code class="new">{cur.fingerprint}</code></dd>
        </dl>
      {:else}
        <dl>
          <dt>主机</dt><dd data-testid="hk-host">{cur.host}:{cur.port}</dd>
          <dt>密钥类型</dt><dd>{cur.keyType}</dd>
          <dt>指纹 (SHA256)</dt><dd><code>{cur.fingerprint}</code></dd>
        </dl>
      {/if}
      <p class="hint">请通过服务器控制台或管理员核对 SHA256 指纹。尚未确认来源时请选择「拒绝」；「接受并记录」会保存信任，供下次连接使用。</p>
      <footer>
        <button class="danger" bind:this={rejectBtn} onclick={() => decide("refuse")} data-testid="hk-reject">拒绝</button>
        <button onclick={() => decide("accept_once")} data-testid="hk-once">仅本次接受</button>
        <button class="primary" bind:this={acceptBtn} onclick={() => decide("accept_record")} data-testid="hk-persist">接受并记录</button>
      </footer>
    </div>
  </div>
{/if}

<style>
  .overlay { position: fixed; inset: 0; background: rgba(0,0,0,.5); display: grid; place-items: center; z-index: 60; }
  .dialog { width: 480px; max-width: 92vw; max-height: 92vh; overflow-y: auto; padding: 16px 20px; background: var(--fs-bg-elevated); border: 1px solid var(--fs-border); border-radius: 6px; color: var(--fs-fg-primary); display: flex; flex-direction: column; gap: 12px; }
  .dialog h2 { margin: 0; font-size: 14px; display: flex; align-items: baseline; gap: 8px; }
  .queued { font-size: 11.5px; font-weight: normal; color: var(--fs-fg-secondary); }
  .warn { background: rgba(220, 38, 38, 0.1); border: 1px solid var(--fs-danger); border-radius: 4px; padding: 10px; }
  .warn strong { color: var(--fs-danger); display: block; margin-bottom: 4px; }
  .warn p { margin: 0; font-size: 12px; color: var(--fs-fg-secondary); }
  dl { display: grid; grid-template-columns: auto 1fr; gap: 6px 12px; margin: 0; font-size: 12.5px; }
  dt { color: var(--fs-fg-secondary); text-align: right; }
  dd { margin: 0; }
  code { font-family: monospace; font-size: 11.5px; background: var(--fs-bg-input); padding: 2px 4px; border-radius: 2px; }
  code.new { color: var(--fs-danger); }
  footer { display: flex; justify-content: flex-end; gap: 8px; }
  footer button { padding: 5px 14px; border: 1px solid var(--fs-border); background: var(--fs-bg-panel); color: var(--fs-fg-primary); border-radius: 4px; cursor: pointer; }
  footer button.primary { background: var(--fs-accent); color: var(--fs-accent-fg); border-color: var(--fs-accent); }
  footer button.danger { background: var(--fs-danger); border-color: var(--fs-danger); color: #fff; }
</style>
