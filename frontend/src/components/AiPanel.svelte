<script lang="ts">
  /**
   * AI 助手面板（M2 出口第 7、8、12、14 项）。
   *
   * # 未配置态不是错误态
   *
   * 出口原文：「未配置任何模型源时 AI 面板显示配置向导，**不崩溃、不降级为假数据**」。
   * 所以这里有两个界面而不是一个界面加一句错误：没配好的时候，用户要的是
   * 「怎么配」，不是「出错了」。
   *
   * # 打开时只发一次 IPC
   *
   * 出口有「面板打开 ≤200ms」这一条。每多一次 IPC 往返就多一次调度延迟，
   * 所以 `ai_status` 一次把「配没配好、有哪些条目、哪条在生效、什么档位」全带回来。
   */
  import { onMount } from "svelte";
  import { useFocusTrap } from "../lib/focusTrap";
  import { invoke } from "../lib/ipc";
  import Markdown from "./Markdown.svelte";

  let {
    open = false,
    onClose = () => {},
    /** 活动会话 id（上下文用）。没有会话时也能用——那时上下文里没有主机信息。 */
    sessionId = null,
    /** 取当前终端可见内容（屏幕上下文）。由 App 注入，因为只有它拿得到活动终端。 */
    getScreen = () => null,
    /** 把命令填进组合命令栏（不直接执行——执行要过确认闸门）。 */
    onUseCommand = () => {},
    /**
     * 面板打开时要解读的选区（选区右键「AI 解读」进来的那一路）。
     *
     * 带 `seq` 而不是只给字符串：用户选中同一段输出、解读完关掉面板、再解读一次，
     * 纯字符串的话值没变、下面的 `$effect` 不重跑，面板打开是空白的。
     * 而「再解读一遍」是个完全正常的意图（第一次的回答没说清）。
     */
    pendingSelection = null,
    /**
     * 预填进提问框并**立刻发问**的问题（监控 × AI 联动那一路）。
     *
     * 与 `pendingSelection` 分成两个 prop 而不是合成一个：两者进的是不同的流程
     * ——选区走 `ai_explain`（解读，不产出命令），监控诊断走 `ai_suggest_command`
     * （要过 fs_policy 闸门）。合成一个就得在里面带一个「哪种」的判别字段，
     * 而那个字段一旦传错，一条本该过闸门的命令就会走成解读。
     *
     * 同样带 `seq`：同一台机器的同一组异常，用户可能连点两次诊断。
     */
    prefillPrompt = null,
    /**
     * 「存为片段」（M4b 出口第 2 项后半：AI 生成片段入库可用）。
     *
     * 传命令与裁决**两样**：入库那一步要自己再核一遍裁决
     * （`quickCommandFromAi` 是白名单判定），而不是信任「按钮没禁用所以能存」。
     * 无回调 = 按钮不出现。
     */
    onSaveSnippet = null,
  }: {
    open?: boolean;
    onClose?(): void;
    sessionId?: string | null;
    getScreen?(): string | null;
    onUseCommand?(cmd: string): void;
    pendingSelection?: { text: string; seq: number } | null;
    prefillPrompt?: { text: string; seq: number } | null;
    onSaveSnippet?: ((command: string, decision: string) => void) | null;
  } = $props();

  interface ProviderSummary {
    id: string;
    kind: string;
    base_url: string;
    model: string;
    has_key: boolean;
    allow_screen_context: boolean;
    problem: string | null;
  }
  interface AiStatus {
    configured: boolean;
    active: { kind: string; base_url: string; model: string; allow_screen_context: boolean } | null;
    entries: ProviderSummary[];
    mode: string;
  }
  interface Suggestion {
    command: string;
    explanation: string;
    tier: string;
    reasons: string[];
    decision: string;
    included_screen: boolean;
    redactions: [string, number][];
  }
  interface Explanation {
    markdown: string;
    included_screen: boolean;
    redactions: [string, number][];
  }

  let dialogEl = $state<HTMLDivElement | undefined>();
  let promptEl = $state<HTMLTextAreaElement | undefined>();
  let status = $state<AiStatus | null>(null);
  let tab = $state<"ask" | "config">("ask");
  let prompt = $state("");
  let busy = $state(false);
  let error = $state("");
  let suggestion = $state<Suggestion | null>(null);
  let explanation = $state<Explanation | null>(null);

  // 配置表单
  let formKind = $state("ollama");
  let formBaseUrl = $state("");
  let formModel = $state("");
  let formKey = $state("");
  let formAllowScreen = $state(false);
  let saving = $state(false);

  $effect(() => {
    if (!open || !dialogEl) return;
    return useFocusTrap(dialogEl, { initial: promptEl ?? dialogEl });
  });

  /** 每次打开都重取状态：用户可能刚在设置里改了档位。 */
  $effect(() => {
    if (!open) return;
    void refresh();
  });

  /**
   * 选区右键「AI 解读」进来的那一路：面板打开时直接跑解读。
   *
   * 用 `pendingSelection` 而不是让 App 直接调命令，是因为结果要显示在面板里——
   * 让 App 调命令再把结果传进来，等于把这个面板的状态拆成两处。
   */
  /**
   * 监控诊断进来的那一路：填进提问框并立刻发问。
   *
   * **立刻发问**而不是只填进去等用户按按钮：用户点的是「AI 诊断」，
   * 那个动作的语义就是「现在去诊断」。让他再按一次「问」是多一步，
   * 而那一步没有给他任何新的决定权——问题是我们拼的，他也改不了什么。
   */
  let lastPrefillSeq = $state(-1);
  $effect(() => {
    if (!open || !prefillPrompt || prefillPrompt.seq === lastPrefillSeq) return;
    // `status` 还没到就**什么都不做，也不标记 seq**——等它到了这个 effect 会重跑
    // （它追踪 status）。
    //
    // 这一行是测试逮出来的一个真 bug：原先直接 `if (status?.configured) void ask()`,
    // 而面板刚打开时 `ai_status` 还在路上、`status` 是 null，于是「不发问」这个分支
    // 被走了，同时 seq 已经被标记成已处理——status 到达后 effect 重跑却提前 return。
    // 用户点「AI 诊断」看到的是面板打开、什么都没发生。
    //
    // 「还不知道配没配好」与「知道没配好」是两件事，而只有后者才是一个决定。
    if (!status) return;
    lastPrefillSeq = prefillPrompt.seq;
    // 问题**照样填进去**：用户配完模型源切到提问页，那个问题还在，不用回监控页重点一次。
    prompt = prefillPrompt.text;
    // 没配模型源时不发、也**不抢页签**。`refresh()` 已经把页签切到「配置」了
    // （出口第 7 项：未配置时显示配置向导），这里再切回 ask 会把向导盖掉，
    // 用户看到的是一句「还没配置模型源」而不是怎么配——那正是第 7 项要避免的。
    if (!status.configured) return;
    tab = "ask";
    void ask();
  });

  let lastHandledSeq = $state(-1);
  $effect(() => {
    if (!open || !pendingSelection || pendingSelection.seq === lastHandledSeq) return;
    // 与 prefillPrompt 那条同理：status 未到 ⇒ 什么都不做也不标记。
    // 并且**没配模型源时不发** `ai_explain`——出口第 7 项要的是「显示配置向导」，
    // 而不是「向导上面糊一条注定的错误」。
    if (!status) return;
    lastHandledSeq = pendingSelection.seq;
    if (!status.configured) return;
    void runExplain(pendingSelection.text);
  });

  async function refresh(): Promise<void> {
    try {
      status = await invoke<AiStatus>("ai_status");
      // 没配好就直接落在配置页——让用户先看到「怎么配」，而不是一个能打字
      // 但一按就报错的输入框。
      if (!status.configured) tab = "config";
    } catch (e) {
      error = String(e);
    }
  }

  async function ask(): Promise<void> {
    if (!prompt.trim() || busy) return;
    busy = true;
    error = "";
    suggestion = null;
    explanation = null;
    try {
      suggestion = await invoke<Suggestion>("ai_suggest_command", {
        prompt: prompt.trim(),
        screen: getScreen(),
        sessionId,
      });
    } catch (e) {
      error = String(e);
    } finally {
      busy = false;
    }
  }

  /**
   * 「填进命令栏」。
   *
   * 是个具名函数而不是模板里的箭头闭包：模板闭包里 `suggestion` 的收窄会丢
   * （它是可变 `$state`，TS 不敢假设点击那一刻它还非空）。写成函数后判空是**运行期真的判**，
   * 而不是靠一个 `!` 把编译器按下去——按下去之后，真的为 null 时就是一次运行期崩溃。
   *
   * `deny` 档也在这里再挡一次。按钮已经 `disabled`，但 disabled 是**外观**：
   * 键盘/自动化/未来某次重构都可能绕过它，而这个函数是唯一入口。
   */
  function useSuggestion(): void {
    const s = suggestion;
    if (!s || s.decision === "deny") return;
    onUseCommand(s.command);
  }

  /** 存为片段。与 useSuggestion 同理是具名函数：模板闭包里 suggestion 的收窄会丢。 */
  function saveAsSnippet(): void {
    const s = suggestion;
    if (!s || s.decision === "deny") return;
    onSaveSnippet?.(s.command, s.decision);
  }

  async function runExplain(selection: string): Promise<void> {
    busy = true;
    error = "";
    suggestion = null;
    explanation = null;
    try {
      explanation = await invoke<Explanation>("ai_explain", { selection, sessionId });
    } catch (e) {
      error = String(e);
    } finally {
      busy = false;
    }
  }

  async function saveProvider(): Promise<void> {
    if (saving) return;
    saving = true;
    error = "";
    try {
      await invoke("ai_provider_save", {
        kind: formKind,
        baseUrl: formBaseUrl.trim(),
        model: formModel.trim(),
        // 空串 = 不改 key（编辑既有条目时不必重输）；新建时空串对 Ollama 是正常的
        apiKey: formKey,
        allowScreenContext: formAllowScreen,
      });
      formKey = ""; // 存完立刻从内存里去掉——它已经在 Vault 里了
      await refresh();
      if (status?.configured) tab = "ask";
    } catch (e) {
      error = String(e);
    } finally {
      saving = false;
    }
  }

  async function removeProvider(id: string): Promise<void> {
    try {
      await invoke("ai_provider_delete", { id });
      await refresh();
    } catch (e) {
      error = String(e);
    }
  }

  /** 三档危险度的文案与配色。`deny` 走单独一支——那不是「更危险」，是「不给执行」。 */
  const TIER_TEXT: Record<string, string> = {
    read_only: "只读",
    write: "会改动东西",
    dangerous: "危险",
  };
  const DECISION_TEXT: Record<string, string> = {
    auto: "只读命令，可直接执行",
    confirm: "执行前会问你一次",
    "strong-confirm": "危险操作，执行前需要强确认",
    deny: "当前档位不允许执行这条命令",
  };
</script>

{#if open}
  <div class="overlay" role="presentation" onclick={onClose}>
    <div class="panel" role="dialog" aria-modal="true" aria-label="AI 助手" tabindex="-1"
         data-testid="ai-panel" bind:this={dialogEl}
         onclick={(e) => e.stopPropagation()}
         onkeydown={(e) => { if (e.key === "Escape") onClose(); }}>
      <header>
        <h2>AI 助手</h2>
        <!-- `<div>` 而不是 `<nav>`：`role="tablist"` 是交互角色，压在 `<nav>` 这个
             地标元素上会让读屏同时报「导航区域」和「选项卡列表」——两个都不准。 -->
        <div class="tabs" role="tablist">
          <button role="tab" aria-selected={tab === "ask"} data-testid="ai-tab-ask"
                  disabled={!status?.configured}
                  onclick={() => (tab = "ask")}>提问</button>
          <button role="tab" aria-selected={tab === "config"} data-testid="ai-tab-config"
                  onclick={() => (tab = "config")}>配置</button>
        </div>
      </header>

      {#if status && status.mode === "disabled"}
        <!-- 总开关关着时整个功能不可用，不是「点了才报错」。
             出口原文：关闭时入口全部呈禁用态。 -->
        <p class="hint warn" data-testid="ai-disabled">
          AI 执行总开关是关着的（设置 → AI）。打开它之后才能用。
        </p>
      {/if}

      {#if tab === "ask"}
        {#if !status?.configured}
          <p class="hint" data-testid="ai-not-configured">
            还没配置模型源。切到「配置」页填一个——本程序<strong>不内置任何地址</strong>，
            也不会替你选模型。
          </p>
        {:else}
          <label class="ask">
            <span class="hint">想做什么？（自然语言，比如「找出占用 8080 端口的进程」）</span>
            <textarea
              bind:this={promptEl}
              bind:value={prompt}
              data-testid="ai-prompt"
              rows="3"
              disabled={busy || status.mode === "disabled"}
              onkeydown={(e) => { if (e.key === "Enter" && !e.isComposing && (e.ctrlKey || e.metaKey)) { e.preventDefault(); void ask(); } }}
            ></textarea>
          </label>
          <div class="row">
            <button data-testid="ai-ask" disabled={busy || !prompt.trim() || status.mode === "disabled"}
                    onclick={() => void ask()}>{busy ? "思考中…" : "生成命令 (Ctrl+Enter)"}</button>
            <span class="hint">
              {#if status.active}
                {status.active.model}
                · 屏幕上下文{status.active.allow_screen_context ? "开" : "关"}
              {/if}
            </span>
          </div>
        {/if}

        {#if error}
          <p class="hint error" data-testid="ai-error">{error}</p>
        {/if}

        {#if suggestion}
          <!-- 三段：命令、解释、我们算的策略预判。
               出口原文要「解释与策略预判两段」，这里把命令单独列出是第三段——
               它要能被整串选中复制，所以用 <pre> 而不是塞在段落里。 -->
          <div class="result" data-testid="ai-suggestion">
            <pre class="cmd" data-testid="ai-command">{suggestion.command}</pre>
            <p class="why" data-testid="ai-why">{suggestion.explanation}</p>
            <div class="verdict tier-{suggestion.tier}" data-testid="ai-verdict">
              <strong>{TIER_TEXT[suggestion.tier] ?? suggestion.tier}</strong>
              — {DECISION_TEXT[suggestion.decision] ?? suggestion.decision}
              {#if suggestion.reasons.length > 0}
                <ul>
                  {#each suggestion.reasons as r, ri (ri)}<li>{r}</li>{/each}
                </ul>
              {/if}
            </div>
            <div class="row">
              <!-- 「填进命令栏」而不是「执行」：执行要过确认闸门，而那个闸门的载体
                   是组合命令栏与终端。这个按钮把命令交给用户，不替他按回车。 -->
              <button data-testid="ai-use" disabled={suggestion.decision === "deny"}
                      onclick={useSuggestion}>填进命令栏</button>
              {#if onSaveSnippet}
                <!-- 存进片段库。同样受 deny 档挡住：片段库是一键下发的地方，
                     而档位是会变的——今天被拒的一条存进去，切档之后就成了
                     一个点两下就能跑的按钮。入库那一步还会再核一次裁决。 -->
                <button
                  data-testid="ai-save-snippet"
                  disabled={suggestion.decision === "deny"}
                  title="存进快速命令集的「AI」分类，之后可在片段库里改名"
                  onclick={saveAsSnippet}
                >存为片段</button>
              {/if}
              {#if suggestion.decision === "deny"}
                <span class="hint">当前档位不允许，改档位在 设置 → AI。</span>
              {/if}
            </div>
            <p class="hint" data-testid="ai-context-note">
              这次请求{suggestion.included_screen ? "包含" : "不包含"}屏幕内容。
              {#if suggestion.redactions.length > 0}
                已脱敏 {suggestion.redactions.reduce((n, [, c]) => n + c, 0)} 处。
              {/if}
            </p>
          </div>
        {/if}

        {#if explanation}
          <div class="result" data-testid="ai-explanation">
            <Markdown source={explanation.markdown} />
            <p class="hint">
              {#if explanation.redactions.length > 0}
                已脱敏 {explanation.redactions.reduce((n, [, c]) => n + c, 0)} 处。
              {/if}
            </p>
          </div>
        {/if}
      {:else}
        <!-- ── 配置页 ── -->
        <p class="hint" data-testid="ai-config-intro">
          请按模型服务商提供的信息填写服务地址和模型名称。
          API Key 加密保存在保险库中；本机 Ollama 通常不需要 API Key。
        </p>

        {#if status && status.entries.length > 0}
          <ul class="providers" data-testid="ai-providers">
            {#each status.entries as e (e.id)}
              <li data-testid="ai-provider-row">
                <span class="pkind">{e.kind}</span>
                <span class="pmodel">{e.model || "（没填模型名）"}</span>
                <span class="purl">{e.base_url}</span>
                {#if e.problem}
                  <!-- 一份填了一半的配置必须显示成「这条还没填完」，而不是安静地
                       不生效——后者会让用户以为配好了，直到发请求时才报错。 -->
                  <span class="pproblem" data-testid="ai-provider-problem">{e.problem}</span>
                {/if}
                <button class="link-danger" data-testid={`ai-provider-delete-${e.id}`}
                        onclick={() => void removeProvider(e.id)}>删除</button>
              </li>
            {/each}
          </ul>
        {/if}

        <div class="form">
          <label>类型
            <select bind:value={formKind} data-testid="ai-form-kind">
              <option value="ollama">Ollama（本机）</option>
              <option value="openai">OpenAI 兼容</option>
              <option value="anthropic">Anthropic</option>
            </select>
          </label>
          <label>服务地址
            <input bind:value={formBaseUrl} data-testid="ai-form-url"
                   placeholder={formKind === "ollama" ? "http://localhost:11434" : "https://…"} />
          </label>
          <label>模型名
            <input bind:value={formModel} data-testid="ai-form-model" placeholder="填写服务商提供的模型 ID" />
          </label>
          <label>API key{formKind === "ollama" ? "（Ollama 通常不需要）" : ""}
            <input type="password" bind:value={formKey} data-testid="ai-form-key" autocomplete="off" />
          </label>
          <label class="check">
            <input type="checkbox" bind:checked={formAllowScreen} data-testid="ai-form-screen" />
            允许把终端屏幕内容发给这个模型源
          </label>
          <p class="hint">
            这个开关<strong>按模型源</strong>而不是全局：发给本机 Ollama 与发给某家云 API
            是两个决定。默认关——忘了设置时的行为应该是「不发」。
            关着时「解读选中输出」用不了（那个功能就是要把选中的内容发出去）。
          </p>
          <div class="row">
            <button data-testid="ai-form-save" disabled={saving} onclick={() => void saveProvider()}>
              {saving ? "保存中…" : "保存"}
            </button>
          </div>
        </div>

        {#if error}
          <p class="hint error" data-testid="ai-config-error">{error}</p>
        {/if}
      {/if}

      <footer>
        <button data-testid="ai-close" onclick={onClose}>关闭</button>
      </footer>
    </div>
  </div>
{/if}

<style>
  .overlay { position: fixed; inset: 0; background: rgba(0,0,0,.5); display: grid; place-items: center; z-index: 60; }
  .panel { width: 640px; max-width: 94vw; max-height: 88vh; overflow: auto; padding: 16px 20px; background: var(--fs-bg-elevated); border: 1px solid var(--fs-border); border-radius: 6px; color: var(--fs-fg-primary); }
  header { display: flex; align-items: baseline; gap: 16px; margin-bottom: 10px; }
  h2 { margin: 0; font-size: 14px; }
  .tabs { display: flex; gap: 4px; }
  .tabs button { padding: 3px 10px; border: 1px solid var(--fs-border); background: var(--fs-bg-panel); color: var(--fs-fg-secondary); border-radius: 4px; cursor: pointer; font-size: 12px; }
  .tabs button[aria-selected="true"] { background: var(--fs-bg-app); color: var(--fs-fg-primary); }
  .tabs button:disabled { opacity: .5; cursor: default; }
  .hint { font-size: 11.5px; color: var(--fs-fg-secondary); margin: 6px 0; line-height: 1.6; }
  .hint.error { color: var(--fs-danger, #e05252); }
  .hint.warn { color: var(--fs-warn, #d29922); }
  label { display: block; font-size: 12px; margin: 8px 0; }
  label.check { display: flex; align-items: center; gap: 6px; }
  textarea, input:not([type="checkbox"]), select { width: 100%; box-sizing: border-box; margin-top: 3px; background: var(--fs-bg-panel); color: var(--fs-fg-primary); border: 1px solid var(--fs-border); border-radius: 4px; padding: 4px 6px; font-size: 12px; }
  select { padding-right: 22px; } /* 箭头让位（2026-09-01） */
  .row { display: flex; align-items: center; gap: 10px; margin: 8px 0; }
  .row button, footer button, .form button { padding: 5px 14px; border: 1px solid var(--fs-border); background: var(--fs-bg-panel); color: var(--fs-fg-primary); border-radius: 4px; cursor: pointer; font-size: 12px; }
  .row button:disabled { opacity: .5; cursor: default; }
  .result { margin: 10px 0; padding: 10px; border: 1px solid var(--fs-border); border-radius: 4px; background: var(--fs-bg-panel); }
  /* 命令要能整串选中复制——那是用户拿到它之后的第一个动作。 */
  .cmd { margin: 0 0 6px; padding: 8px 10px; background: var(--fs-bg-app); border-radius: 4px; font-family: var(--fs-font-mono, monospace); font-size: 12px; user-select: text; white-space: pre-wrap; overflow-wrap: anywhere; } /* 2026-08-31 换行代替横向滚动 */
  .why { font-size: 12px; margin: 6px 0; line-height: 1.6; }
  .verdict { font-size: 12px; padding: 6px 8px; border-radius: 4px; border-left: 3px solid var(--fs-border); }
  /* 三档配色差别要明显：分级的意义全在「一眼看出这条要不要小心」。 */
  .verdict.tier-read_only { border-left-color: var(--fs-ok, #3fb950); }
  .verdict.tier-write { border-left-color: var(--fs-warn, #d29922); }
  .verdict.tier-dangerous { border-left-color: var(--fs-danger, #e05252); }
  .verdict ul { margin: 4px 0 0; padding-left: 18px; }
  .providers { list-style: none; margin: 8px 0; padding: 0; font-size: 12px; }
  .providers li { display: flex; align-items: center; gap: 8px; padding: 4px 0; border-bottom: 1px solid var(--fs-border); }
  .pkind { color: var(--fs-fg-secondary); min-width: 70px; }
  .pmodel { font-weight: 600; }
  .purl { color: var(--fs-fg-secondary); flex: 1; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .pproblem { color: var(--fs-warn, #d29922); }
  .link-danger { background: none; border: none; color: var(--fs-danger, #e05252); cursor: pointer; font-size: 11.5px; }
  .form { margin-top: 10px; }
  footer { display: flex; justify-content: flex-end; margin-top: 14px; }
</style>
