<script lang="ts">
  /**
   * 终端内传输条（M4a：rz/sz 的进度、取消、选文件）。
   *
   * 三种形态共用一条：
   * ① 远端在跑 `rz`（phase=need-file）→ 显示「选择本地文件」；
   * ② 传输中 → 进度 + 取消；
   * ③ 终态 → 结果文案 + 「打开所在目录」。
   *
   * ## 2026-08-26 重设计：选文件改走 FilePickerDialog
   *
   * 旧版把一个内嵌选择器画在这里，用户实测报了三件事：进了文件夹回不去、
   * 不想选了关不掉、只能单选。「关不掉」的根因在本组件：
   *
   *     $effect(() => { if (view.needFile && !pickerOpen) pickerOpen = true; ... });
   *
   * `needFile` 在远端 rz 等待期间**恒为 true**（rz 每十秒重发 ZRINIT），用户刚把
   * pickerOpen 设回 false，effect 重跑、条件又成立、弹窗立刻重开——关闭按钮
   * 形同虚设。
   *
   * 现在的修法是**边沿触发**：只在 false→true 的跳变瞬间开一次。加上条上常驻的
   * 「选择本地文件…」按钮（手动重开）与「取消」（通知远端退出），三件合起来
   * 才算修完：能关、关了不弹回、想继续有入口。
   */
  import { invoke } from "@tauri-apps/api/core";
  import { IDLE, formatBytes, progressText, type TransferView } from "../lib/zmodem";
  import { toast } from "../lib/toast";
  import FilePickerDialog from "./FilePickerDialog.svelte";

  let { sessionId = "", view = IDLE }: { sessionId?: string; view?: TransferView } = $props();

  let pickerOpen = $state(false);
  /** 目录选择器（下载决策的「更改目录」用；与上传选择器是两个实例两种模式）。 */
  let dirPickerOpen = $state(false);
  /** 决策框里当前选定的目录。`null` = 未换过（用后端建议）；`""` = 用户主目录
   *  （目录选择器以相对家目录的路径回报，根就是空串——**与「未选」不是一个值**）。 */
  let decisionDir = $state<string | null>(null);
  /** 首次引导的「记住选择」：null=未选（不让确认），"fixed"/"ask"。 */
  let rememberChoice = $state<"fixed" | "ask" | null>(null);
  /**
   * 会话内的「对本次剩余文件采用相同处理」。
   *
   * 后端不知道 sz 一共要传几个文件（协议不预告总数），所以「剩余」只能由
   * 前端会话级记忆实现：勾了之后，本会话后续的 conflict 不再弹框、直接按
   * 同样策略 finalize；done/failed 时复位。
   */
  let applyPolicy = $state<"overwrite" | "keep-both" | "skip" | null>(null);
  /** 冲突框的「对本次其余文件采用相同处理」勾选（布尔，与 applyPolicy 分开：
   *  勾选状态在按钮点击之前就存在，而策略值取自**被点的那颗按钮**）。 */
  let applyToRest = $state(false);
  /**
   * 后端自报的传输态（`zmodem_status`）。
   *
   * 需要它是因为事件**没有重放**：切到别的标签再切回来、或者界面重挂载之后，
   * 之前那些 progress 事件已经过去了。只靠事件的话，一个正在跑的传输在界面上
   * 会彻底消失——用户以为传输断了，去按 Ctrl+C，真把它打断。
   */
  let backendActive = $state(false);
  let sandbox = $state<string | null>(null);

  /** 实际归置用的目录：换过用换的，没换用后端建议。 */
  function effDir(): string {
    return decisionDir ?? view.decision?.suggestedDir ?? "";
  }

  /** 决策框里显示的目录文本（主目录的空串形态换成人话）。 */
  function dirLabel(): string {
    const d = effDir();
    return d === "" ? "（主目录）" : d;
  }

  // 决策一到：冲突且用户已选「其余同此处理」→ 不弹框直接办；
  // 问去向（first-run/choose-dir）→ 复位记忆单选（目录保持 null=跟随建议）。
  $effect(() => {
    const d = view.decision;
    if (!d) return;
    if (d.conflict && applyPolicy) {
      void finalize(effDir(), applyPolicy, null);
      return;
    }
    rememberChoice = null;
  });

  // 会话终态复位「其余同此处理」：下次传输重新问（与后端 session 目录的
  // 生命周期对齐——临时决定不越会话存活）。
  $effect(() => {
    if (view.ok !== null || view.needFile) applyPolicy = null;
  });

  // need-file 的**边沿触发**：只在 false→true 的跳变瞬间开一次选择器。
  // 远端 rz 在计时等待，自动打开是对的（多一次点击就多一次超时风险），
  // 但「条件为真就开着」不对——见组件头那段：那正是关不掉的原因。
  let lastNeedFile = false;
  $effect(() => {
    const now = view.needFile;
    if (now && !lastNeedFile) pickerOpen = true;
    lastNeedFile = now;
  });

  // 会话切换即回查一次后端传输态（事件不重放，见 backendActive）
  $effect(() => {
    const id = sessionId;
    if (!id) {
      backendActive = false;
      sandbox = null;
      return;
    }
    void (async () => {
      try {
        const s = await invoke<{ active: boolean; sandbox: string | null; auto: boolean }>(
          "zmodem_status",
          { sessionId: id },
        );
        // 会话可能在 await 期间又被切走，落后的应答不得覆盖当前会话的状态
        if (sessionId !== id) return;
        backendActive = s.active;
        sandbox = s.sandbox;
      } catch {
        // 查不到就按未启用处理：这条只影响「要不要显示恢复条」，不该弹错提示
        if (sessionId === id) {
          backendActive = false;
          sandbox = null;
        }
      }
    })();
  });

  async function pick(paths: string[]) {
    pickerOpen = false;
    if (paths.length === 0) return;
    try {
      await invoke("zmodem_send", { sessionId, localPaths: paths });
    } catch (e) {
      toast.error(`上传失败：${e}`);
    }
  }

  /** 归置（两段式第二段）。`remember`：null=不动设置，"fixed"/"ask"=记住去向偏好。 */
  async function finalize(
    dir: string,
    onConflict: "overwrite" | "keep-both" | "skip",
    remember: "fixed" | "ask" | null,
  ) {
    try {
      await invoke("zmodem_finalize", {
        sessionId,
        dir,
        onConflict,
        remember: remember === null ? null : remember === "fixed",
      });
      if (onConflict === "skip") toast.info("已跳过该文件");
    } catch (e) {
      toast.error(`保存失败：${e}`);
    }
  }

  /** 冲突框的一次裁决：勾了「其余同此处理」就把**这颗按钮的策略**记住，
   *  本会话后续冲突不再弹框（见 applyPolicy）。 */
  function settleConflict(policy: "overwrite" | "keep-both" | "skip") {
    if (applyToRest) applyPolicy = policy;
    void finalize(effDir(), policy, null);
  }

  /** 决策框的「不选了」：按建议目录 + 保留两者兜底归置。**绝不丢已收数据**——
   *  远端已经把文件发完了，删掉等于让用户白传一遍。 */
  async function dismissDecision() {
    const d = view.decision;
    if (!d) return;
    await finalize(effDir(), "keep-both", null);
    toast.info(`已保存到 ${dirLabel()}`);
  }

  async function cancel() {
    try {
      await invoke("zmodem_cancel", { sessionId });
    } catch (e) {
      toast.error(`取消失败：${e}`);
    }
  }

  async function reveal() {
    if (!view.path) return;
    try {
      await invoke("zmodem_reveal", { sessionId, path: view.path });
    } catch (e) {
      toast.error(`打开失败：${e}`);
    }
  }

  const label = $derived(
    view.direction === "send" ? "上传中" : view.direction === "receive" ? "下载中" : "传输",
  );
  /** 多文件上传时显示「第 i/N 个」；单文件或未知时不显示。 */
  const tally = $derived(
    view.fileCount !== null && view.fileCount > 1 && view.fileIndex !== null
      ? `（第 ${view.fileIndex}/${view.fileCount} 个）`
      : "",
  );
</script>

{#if view.visible || view.needFile || backendActive}
  <div class="bar" data-testid="zmodem-bar" role="status">
    {#if !view.visible && !view.needFile && backendActive}
      <!-- 后端说在传，但本界面没有这次传输的事件（切标签/重挂载后事件已过去）。
           不画进度条：字节数无从得知，画一个 0% 比不画更误导。 -->
      <span class="msg" data-testid="zm-recovered">传输进行中…</span>
      <div class="track indeterminate"></div>
      <button data-testid="zm-cancel" onclick={() => void cancel()}>取消</button>
    {:else if view.needFile}
      <span class="msg">远端正在等待文件（rz）</span>
      <button data-testid="zm-pick" onclick={() => (pickerOpen = true)}>选择本地文件…</button>
      <button data-testid="zm-cancel-need" onclick={() => void cancel()}>取消</button>
    {:else if view.ok === null}
      <span class="msg">{label} {view.name}{tally}</span>
      {#if view.ratio === null}
        <!-- 总大小未知：显示不定态条纹，不画一个会骗人的百分比进度 -->
        <div class="track indeterminate" data-testid="zm-indeterminate"></div>
      {:else}
        <div class="track"><div class="fill" style="width: {view.ratio * 100}%"></div></div>
      {/if}
      <span class="num" data-testid="zm-progress">{progressText(view)}</span>
      <button data-testid="zm-cancel" onclick={() => void cancel()}>取消</button>
    {:else}
      <span class="msg" class:err={!view.ok} data-testid="zm-result">
        {view.ok ? "✓" : "✕"} {view.message || (view.ok ? "传输完成" : "传输失败")}
        {#if view.name}— {view.name}{/if}
      </span>
      {#if view.path}
        <button data-testid="zm-reveal" onclick={() => void reveal()}>打开所在目录</button>
      {/if}
    {/if}
  </div>
{/if}

{#if pickerOpen}
  <FilePickerDialog
    open={pickerOpen}
    title="选择要上传的文件"
    mode="files"
    multi={true}
    note={sandbox ? `收到的文件保存到：${sandbox}` : ""}
    onConfirm={(paths) => void pick(paths)}
    onCancel={() => (pickerOpen = false)}
  />
{/if}

{#if dirPickerOpen}
  <FilePickerDialog
    open={dirPickerOpen}
    title="选择保存位置"
    mode="directory"
    confirmLabel="用这个目录"
    onConfirm={(paths) => {
      dirPickerOpen = false;
      if (paths[0] !== undefined) decisionDir = paths[0];
    }}
    onCancel={() => (dirPickerOpen = false)}
  />
{/if}

<!-- 下载决策框（两段式第二段）。三种形态：
     first-run（首次引导：选目录 + 记住偏好）、choose-dir（每次询问模式）、
     conflict（覆盖/保留两者/跳过）。「不选了」一律兜底归置到建议目录，
     绝不删已收数据。 -->
{#if view.decision && !(view.decision.conflict && applyPolicy)}
  <div class="overlay" role="presentation">
    <div class="dpanel" role="dialog" aria-modal="true" aria-label="保存下载文件" tabindex="-1"
         data-testid="zm-decision"
         onkeydown={(e) => { if (e.key === "Escape") void dismissDecision(); }}>
      {#if view.decision.conflict}
        <h3>同名文件已存在</h3>
        <p class="line">
          <strong>{view.decision.name}</strong> 已存在于
          <code>{dirLabel()}</code>
        </p>
        <div class="btns">
          <button class="danger" data-testid="zm-ow"
                  onclick={() => settleConflict("overwrite")}>
            覆盖
          </button>
          <button data-testid="zm-keep"
                  onclick={() => settleConflict("keep-both")}>
            保留两者
          </button>
          <button data-testid="zm-skip"
                  onclick={() => settleConflict("skip")}>
            跳过
          </button>
        </div>
        <label class="rest">
          <input type="checkbox" data-testid="zm-apply-rest" bind:checked={applyToRest} />
          对本次传输的其余同名文件采用相同处理
        </label>
        <p class="hint">「跳过」会丢弃本次收到的这个文件（远端已发完，不会重传）。</p>
      {:else}
        <h3>{view.decision.reason === "first-run" ? "第一次收到文件——保存到哪里？" : "文件保存到哪里？"}</h3>
        <p class="line">
          <strong>{view.decision.name}</strong>（{formatBytes(view.total ?? 0)}）
        </p>
        <p class="dirline">
          <code data-testid="zm-dec-dir">{dirLabel()}</code>
          <button data-testid="zm-dec-change" onclick={() => (dirPickerOpen = true)}>更改…</button>
        </p>
        {#if view.decision.reason === "first-run"}
          <fieldset class="remember">
            <legend>以后怎么办</legend>
            <label><input type="radio" name="zm-remember" value="fixed" checked={rememberChoice === "fixed"}
                          onchange={() => (rememberChoice = "fixed")} />记住这个目录，以后都存这里</label>
            <label><input type="radio" name="zm-remember" value="ask" checked={rememberChoice === "ask"}
                          onchange={() => (rememberChoice = "ask")} />以后每次都问我</label>
          </fieldset>
        {/if}
        <div class="btns">
          <button data-testid="zm-dec-later" onclick={() => void dismissDecision()}>不选了（存到上面的目录）</button>
          <button class="primary" data-testid="zm-dec-ok"
                  onclick={() => void finalize(effDir(), "keep-both", rememberChoice)}>
            保存到这里
          </button>
        </div>
      {/if}
    </div>
  </div>
{/if}

<style>
  .bar { display: flex; align-items: center; gap: 10px; padding: 4px 10px; font-size: 12px;
         background: var(--fs-bg-panel); border-top: 1px solid var(--fs-border); color: var(--fs-fg-primary); }
  .msg { white-space: nowrap; overflow: hidden; text-overflow: ellipsis; max-width: 40%; }
  .msg.err { color: var(--fs-danger); }
  .num { font-variant-numeric: tabular-nums; white-space: nowrap; }
  .track { flex: 1; height: 6px; background: var(--fs-bg-input); border-radius: 3px; overflow: hidden; }
  .fill { height: 100%; background: var(--fs-accent); transition: width .15s linear; }
  .indeterminate { background: repeating-linear-gradient(90deg, var(--fs-bg-input) 0 8px, var(--fs-accent) 8px 16px); opacity: .6; }
  .bar button { padding: 2px 10px; border: 1px solid var(--fs-border); background: var(--fs-bg-elevated);
                color: var(--fs-fg-primary); border-radius: 4px; cursor: pointer; font-size: 12px; }
  .overlay { position: fixed; inset: 0; background: rgba(0,0,0,.5); display: grid; place-items: center; z-index: 60; }
  .dpanel { width: 460px; max-width: 92vw; max-height: 92vh; overflow-y: auto; padding: 16px 20px; background: var(--fs-bg-elevated);
            border: 1px solid var(--fs-border); border-radius: 6px; color: var(--fs-fg-primary); font-size: 13px; }
  .dpanel h3 { margin: 0 0 10px; font-size: 14px; }
  .line { margin: 0 0 8px; }
  .dirline { display: flex; align-items: center; gap: 8px; margin: 0 0 10px; }
  .dirline code { flex: 1; word-break: break-all; color: var(--fs-accent); font-size: 12px; }
  .dirline button, .btns button { padding: 4px 12px; border: 1px solid var(--fs-border); background: var(--fs-bg-panel);
                                  color: var(--fs-fg-primary); border-radius: 4px; cursor: pointer; }
  .btns { display: flex; gap: 8px; margin-top: 10px; flex-wrap: wrap; }
  .btns .primary { background: var(--fs-accent); border-color: var(--fs-accent); color: #fff; }
  .btns .danger { color: var(--fs-danger); border-color: var(--fs-danger); }
  .btns button:disabled { opacity: .5; cursor: default; }
  .remember { border: 1px solid var(--fs-border); border-radius: 4px; padding: 8px 10px; margin: 0 0 6px; }
  .remember legend { font-size: 12px; color: var(--fs-fg-secondary); padding: 0 4px; }
  .remember label { display: block; margin: 4px 0; cursor: pointer; }
  .rest { display: flex; align-items: center; gap: 6px; margin: 10px 0 0; cursor: pointer; }
  .hint { color: var(--fs-fg-secondary); font-size: 12px; margin: 8px 0 0; }
</style>

