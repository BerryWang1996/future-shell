<script lang="ts">
  import { invoke } from "../lib/ipc";
  import { untilUnmount } from "../lib/lifecycle";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { useFocusTrap } from "../lib/focusTrap";
  import { toast } from "../lib/toast";
  import type { AuthPromptPayload } from "../lib/types";

  /**
   * 一条待应答的提问。`target` 是后端按连接配置拼的 `user@host:port`（events.rs `target_label`），
   * `name`/`instruction` 则是**服务端自报**的字符串——两者的可信度天差地别，故分开呈现。
   */
  type Pending = {
    sessionId: string;
    promptId: string | null;
    target: string | null;
    name: string | null;
    instruction: string | null;
    prompts: { text: string; echo: boolean }[];
    kind: "kbd" | "password";
    profileId: string | null;
  };

  /**
   * 待应答队列（审计 P1「前端单槽 prompt 覆盖」）。
   *
   * 原实现是单槽：每来一条 `auth:prompt` 就覆写 sessionId/promptId/prompts，并把 `responses`
   * 重置成空串。后端那侧早已按 `(session_id, prompt_id)` 支持多枚并存，而并发是常态——
   * 两个会话各自跑重连循环（session_cmd.rs 的 `reconnect_loop`）就会同时要口令。
   *
   * 覆盖的后果是**把口令输给了另一台机器**：用户正在给 A 输密码，B 的提问到达，
   * 框里的 sessionId/promptId 已经换成 B，输入框被清空；由于框上原先**不显示任何主机身份**，
   * 而两边的提示语通常都是服务端给的同一句 `Password:`，用户看不出发生过任何事，
   * 接着把 A 的口令打完、回车——这份明文就投给了 B。被换掉的 A 则无人应答，挂到 120 s 超时。
   *
   * 因此这里同时修两件事：① 队列化，已显示的提问只由用户的提交/取消推进；
   * ② 框上标明是谁在问，且身份取自**本地配置**而非对端字符串。
   */
  let queue = $state<Pending[]>([]);
  const cur = $derived(queue.length ? queue[0] : null);
  /** 当前这一条的明文应答。与 `cur` 一一对应，切换提问时必须重建（见 `advance`）。 */
  let responses = $state<string[]>([]);
  /** 「记住此密码（存入 Vault）」勾选：只在 kind==="password" 时渲染，随提问切换清零。 */
  let remember = $state(false);

  // bind:this 的目标必须是 $state：否则赋值不进入响应式图，下面的 $effect 不会因绑定完成而重跑，
  // 焦点陷阱拿到的永远是 undefined（表现为对话框弹出后焦点仍留在背景，Tab 也不圈闭）。
  let dialogEl = $state<HTMLDivElement | undefined>();
  let firstInput = $state<HTMLInputElement | undefined>();

  let unlisten: UnlistenFn | null = null;
  $effect(() => {
    unlisten = untilUnmount(listen<AuthPromptPayload>("auth:prompt", (e) => {
      const p: Pending = {
        sessionId: e.payload.session_id,
        promptId: e.payload.promptId ?? null,
        target: e.payload.target ?? null,
        name: e.payload.name ?? null,
        instruction: e.payload.instruction ?? null,
        prompts: e.payload.prompts,
        kind: e.payload.authKind === "password" ? "password" : "kbd",
        profileId: e.payload.profileId ?? null,
      };
      // 一律追加到队尾；当且仅当队列此前为空，这一条才顺带成为当前项并铺开输入槽。
      const wasIdle = queue.length === 0;
      queue = [...queue, p];
      if (wasIdle) {
        responses = p.prompts.map(() => "");
        remember = false;
      }
    }));
    return () => { unlisten?.(); };
  });

  $effect(() => {
    const c = cur;
    if (!c || !dialogEl) return;
    // 读 promptId 把「换到下一条提问」也纳入依赖：内容换了焦点却仍停在上一条的输入框上，
    // 等于让用户对着新问题继续打旧答案。
    void c.promptId;
    // prompts 为空（russh 的纯提示回合）时没有输入框可聚焦，退回对话框本体：
    // 它带 tabindex="-1"，可编程聚焦但不进入 Tab 序列。
    return useFocusTrap(dialogEl, { initial: firstInput ?? dialogEl });
  });

  /**
   * 出队并切到下一条。**先把明文抹掉再换队列**：`responses` 与当前项一一对应，
   * 若留着旧数组等下一条的 `prompts` 渲染出来，中间这一帧里 `bind:value` 会把上一条的
   * 口令显示在下一条的输入框里——那正是本次要根除的「串台」。
   */
  function advance() {
    responses = [];
    remember = false;
    queue = queue.slice(1);
    const next = queue[0];
    if (next) responses = next.prompts.map(() => "");
  }

  /** UTF-8 字符串 → base64：与 ProfileDialog 的 utf8ToB64 同口径（vault_put 收 secretB64）。 */
  function utf8ToB64(s: string): string {
    return btoa(new TextEncoder().encode(s).reduce((a, b) => a + String.fromCharCode(b), ""));
  }

  async function submit() {
    const head = queue[0];
    if (!head) return;
    // 先取参再出队：出队是同步的，能挡住 await 期间的重复提交（回车连击会让第二次投递
    // 因待决 prompt 已被取走而报错）。
    // $state.snapshot：responses 是状态代理，直接跨 IPC 传给外部序列化器是官方点名的坑；
    // 顺带脱钩，advance() 之后的清空不会回头影响这次在途调用。
    const snapshot = $state.snapshot(responses);
    const args = {
      sessionId: head.sessionId,
      promptId: head.promptId,
      responses: snapshot,
    };
    // 「记住」：只对客户端主动发起的口令询问生效，且只在用户勾选时；先应答再落库——
    // 连接不该为「顺便存个密码」多等一次 vault 往返，存失败也不阻断登录。
    const wantRemember = head.kind === "password" && remember && head.profileId;
    const password = snapshot[0] ?? "";
    advance();
    try {
      await invoke("auth_respond", args);
      if (wantRemember && password) {
        try {
          await invoke("profile_store_password", {
            profileId: head.profileId,
            secretB64: utf8ToB64(password),
          });
          toast.info("已记住此密码（存入 Vault）");
        } catch (e) {
          // 存不进去（Vault 未解锁/写库失败）不影响本次登录，但必须让用户知道「没记住」，
          // 否则下次连接又得重输、而用户以为已经存了。
          console.error("profile_store_password 失败", e);
          toast.warn("本次登录不受影响，但密码未能存入 Vault（Vault 未解锁？）");
        }
      }
    } catch (e) {
      // 应答没投到位时连接会一直挂到后端超时，静默吞掉等于让用户对着无响应的界面干等。
      console.error("auth_respond 投递失败", e);
      toast.error("认证应答提交失败，连接可能已中断");
    }
  }

  /**
   * 取消 = 立刻回一个**空应答**（2026-09-02 真机核实后改）。
   *
   * 此前只关框、不告诉后端，靠后端对未决 prompt 的 120 秒超时收尾。后果在日志里
   * 一目了然：用户 8 月 31 日那次 09:44:57 拨号 → 09:46:57 失败，探针复现也是
   * 06:00:30 → 06:02:30——**取消之后标签还「正在连接」转整整两分钟**，失败面板
   * 两分钟后才出来。用户以为程序卡了。
   *
   * 空应答不是「替用户猜答案」：后端 `password_prompt` 对空 responses 取 `first()`
   * 得空串，引擎 `if !pw.is_empty()` 直接跳过，**不会拿空口令去撞服务器**
   * （那是 RDP 侧 cancelled_prompt 判据钉过的同一条语义）。它就是「用户放弃」
   * 的准确表达，只是把 120 秒的等待变成即时。
   *
   * `dismissForVaultLock` 刻意**不**这么做（见其注释）：那是「人不在机器前」，
   * 让超时兜底更稳；这里是人亲手点的取消，两者语义不同。
   */
  function cancel() {
    const head = queue[0];
    advance();
    if (!head) return;
    void invoke("auth_respond", {
      sessionId: head.sessionId,
      promptId: head.promptId,
      responses: head.prompts.map(() => ""),
    }).catch((e) => console.error("取消应答投递失败（后端将按超时收尾）", e));
  }

  /**
   * Vault 自动锁定（`vault:locked`）时由 App.svelte 调用。
   * 语义与「离开机器」一致：把已输入但未提交的口令抹掉并关窗，不替用户向后端投递任何应答——
   * 后端对未决 prompt 自带超时（超时按空应答处理），比在这里猜一个答案安全。
   * **整队**清空：排在后面的提问同样是人不在机器前时收到的，留着只会在解锁后弹出一串。
   */
  export function dismissForVaultLock() {
    responses = [];
    queue = [];
  }
</script>

{#if cur}
  <div class="overlay" role="presentation">
    <!-- tabindex="-1"：role="dialog" 需可聚焦（a11y_interactive_supports_focus），
         负值使其可被 focus() 定位但不占 Tab 序列，不干扰焦点陷阱的圈闭顺序。 -->
    <div class="dialog" role="dialog" aria-modal="true" aria-label="认证" tabindex="-1"
         data-testid="auth-dialog" bind:this={dialogEl}
         onkeydown={(e) => { if (e.key === "Escape") cancel(); }}>
      <!-- 身份行排在服务端文案之前，且始终存在：口令要输给谁，是这个框里唯一必须先读的信息。
           target 缺省（旧后端）时退回会话短号——短号至少能把两个并发会话区分开，
           而把服务端自报的 name 当身份用是不行的：那串字是对端说的。 -->
      <p class="target" data-testid="auth-target">
        {cur.target ?? `会话 ${cur.sessionId.slice(0, 8)}`} 要求认证
        {#if queue.length > 1}
          <span class="queued" data-testid="auth-queued">还有 {queue.length - 1} 个待应答</span>
        {/if}
      </p>
      {#if cur.name}<h2>{cur.name}</h2>{/if}
      {#if cur.instruction}<p class="instruction">{cur.instruction}</p>{/if}
      <form onsubmit={(e) => { e.preventDefault(); void submit(); }}>
        {#each cur.prompts as p, i (i)}
          <label>
            {p.text}
            {#if i === 0}
              <input
                type={p.echo ? "text" : "password"}
                bind:value={responses[i]}
                bind:this={firstInput}
                data-testid="auth-input-{i}"
              />
            {:else}
              <input
                type={p.echo ? "text" : "password"}
                bind:value={responses[i]}
                data-testid="auth-input-{i}"
              />
            {/if}
          </label>
        {/each}
        {#if cur.kind === "password"}
          <!-- 连接时输口令（Task 47）：只有客户端主动要口令时才有「记住」一说。
               kbd-interactive 是服务端在问（可能问 OTP/新密码），存下来是错的。 -->
          <label class="remember">
            <input type="checkbox" bind:checked={remember} data-testid="auth-remember" />
            记住此密码（存入 Vault）
          </label>
        {/if}
        <footer>
          <button type="button" onclick={cancel}>取消</button>
          <button type="submit" class="primary" data-testid="auth-submit">提交</button>
        </footer>
      </form>
    </div>
  </div>
{/if}

<style>
  .overlay { position: fixed; inset: 0; background: rgba(0,0,0,.5); display: grid; place-items: center; z-index: 60; }
  .dialog { width: 420px; max-width: 92vw; max-height: 92vh; overflow-y: auto; padding: 16px 20px; background: var(--fs-bg-elevated); border: 1px solid var(--fs-border); border-radius: 6px; color: var(--fs-fg-primary); display: flex; flex-direction: column; gap: 10px; }
  .dialog h2 { margin: 0; font-size: 14px; }
  .target { margin: 0; font-size: 12.5px; font-weight: 600; display: flex; align-items: baseline; gap: 8px; }
  .queued { font-size: 11.5px; font-weight: normal; color: var(--fs-fg-secondary); }
  .instruction { margin: 0; font-size: 12.5px; color: var(--fs-fg-secondary); white-space: pre-wrap; }
  form { display: flex; flex-direction: column; gap: 10px; }
  label { display: flex; flex-direction: column; gap: 4px; font-size: 12.5px; color: var(--fs-fg-secondary); }
  label.remember { flex-direction: row; align-items: center; gap: 8px; font-size: 12.5px; color: var(--fs-fg-primary); }
  input { padding: 5px 8px; background: var(--fs-bg-input); border: 1px solid var(--fs-border); border-radius: 4px; color: var(--fs-fg-primary); }
  footer { display: flex; justify-content: flex-end; gap: 8px; }
  footer button { padding: 5px 14px; border: 1px solid var(--fs-border); background: var(--fs-bg-panel); color: var(--fs-fg-primary); border-radius: 4px; cursor: pointer; }
  footer button.primary { background: var(--fs-accent); color: var(--fs-accent-fg); border-color: var(--fs-accent); }
</style>
