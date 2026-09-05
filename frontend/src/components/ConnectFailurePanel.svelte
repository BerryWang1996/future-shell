<script lang="ts">
  /**
   * ConnectFailurePanel.svelte — 连接失败时**带出路**的面板（路线图 4c，2026-09-01）。
   *
   * # 它替代的是什么
   *
   * 此前占位标签在 error 态只渲染一行被截断的错误文案，用户能做的事只有：读一句
   * 「请在会话属性 → 认证页…」的长文，然后自己去翻菜单。用户在 2026-08-31/09-01
   * 三次撞到同一处。这个面板把三件事放到失败当刻：
   *   ① 说清**服务器只接受什么**（对端通告原话，不猜）；
   *   ② 说清**我们手里有什么 / 为什么一次都没发起**（引擎的 notes 原文，中文）；
   *   ③ 给出能点的路：重试 / 用本地 SSH Agent 重试 / 配置认证… / 解锁保险库 / 完整错误。
   *
   * # 三条纪律（每条都有判据）
   *
   * · **每颗按钮点一次只重试一次，在途禁用，绝不自动循环**。publickey 每失败一次
   *   照样吃堡垒机的 MaxAuthTries（常配 2–3 次）；一个「自动重试」按钮能在用户
   *   眨眼之间把账户锁 30 分钟。
   * · **有跳板时禁掉改档案的一键动作**（用 Agent 重试）。错误里不带「失败在第几跳」，
   *   在目标档案上开 agent 可能修的不是坏掉的那一跳；「配置认证…」与「重试」保留。
   * · **`remaining` 为空时不说「服务器只接受 X」**。空 = 服务器没通告任何方法，或失败
   *   根本不在认证阶段（超时/拒连/主机密钥）——那时这句是编出来的。
   *
   * # 顶部固定显示 user@host:port + 指纹
   *
   * 防「在 A 机的失败面板上给 B 机挑私钥」：多标签同时失败时，面板与标签的绑定
   * 只靠视觉，身份必须写在面板自己身上。
   *
   * # Agent / 保险库 按钮的可用性来自 `key_list`
   *
   * 「用 Agent 重试」在 agent 不可用时必须**禁用并说原因**，否则它是一颗纯表演的
   * 按钮：点了、重连、再次失败、再回到这里。`key_list` 的 `agent_error` 与
   * `vault_locked` 正是为「不静默」设计的（见 key_cmd.rs 的字段文档），这里复用。
   */
  import { onMount } from "svelte";
  import { invoke } from "../lib/ipc";
  import type { ConnectFailure, Profile } from "../lib/types";

  let {
    failure = null,
    errorText = "",
    profile = null,
    onRetry = async () => {},
    onEnableAgent = async () => {},
    onConfigureAuth = () => {},
    onUnlockVault = () => {},
    onDetail = () => {},
  }: {
    failure?: ConnectFailure | null;
    errorText?: string;
    profile?: Profile | null;
    onRetry?: () => Promise<void> | void;
    onEnableAgent?: () => Promise<void> | void;
    onConfigureAuth?: () => void;
    onUnlockVault?: () => void;
    onDetail?: () => void;
  } = $props();

  /** 与 key_cmd.rs 的 KeyListing 同形（只取本面板用到的两栏）。 */
  type KeyListing = { agent_error: string | null; vault_locked: boolean };
  let listing = $state<KeyListing | null>(null);
  /** 任一动作在途：全部按钮禁用（一次点击 = 一次尝试）。 */
  let busy = $state(false);

  const isAuth = $derived(failure?.category === "auth");
  const remaining = $derived(failure?.remaining ?? []);
  const acceptsPublicKey = $derived(remaining.includes("publickey"));
  const acceptsPassword = $derived(remaining.includes("password") || remaining.includes("keyboard-interactive"));
  const agentAlreadyOn = $derived(profile?.auth?.allow_agent === true);
  const hasJump = $derived(failure?.has_jump === true);
  /** 主指引：引擎 notes 的第一句（已是中文、已可行动）。其余折叠。 */
  const headline = $derived(failure?.notes?.[0] ?? null);
  const moreNotes = $derived((failure?.notes ?? []).slice(1));

  onMount(async () => {
    // 只在认证类失败时问 key_list：其余类别没有 agent/保险库按钮，问了也不用。
    if (!isAuth) return;
    try {
      listing = await invoke<KeyListing>("key_list");
    } catch {
      // 拿不到就按「未知」处理：agent 按钮仍可点（不替用户下结论），只是没有禁用理由。
      listing = null;
    }
  });

  /** 一次点击一次尝试；在途禁用；结束后本面板通常已被卸载（标签被撤），不必复位。 */
  async function run(action: () => Promise<void> | void): Promise<void> {
    if (busy) return;
    busy = true;
    try {
      await action();
    } finally {
      busy = false;
    }
  }

  const identity = $derived(
    failure ? `${failure.username}@${failure.host}:${failure.port}` : (profile ? `${profile.username}@${profile.host}:${profile.port}` : ""),
  );
</script>

<div class="panel" data-testid="connect-failure-panel" role="region" aria-label="连接失败">
  <div class="head">
    <span class="dot" aria-hidden="true">●</span>
    <strong>连接失败</strong>
    {#if identity}
      <code class="who" data-testid="cf-identity">{identity}</code>
    {/if}
    {#if failure?.fingerprint}
      <code class="fp" title="主机密钥指纹（SHA256）" data-testid="cf-fingerprint">{failure.fingerprint}</code>
    {/if}
  </div>

  {#if isAuth && remaining.length > 0}
    <!-- 对端通告原话：不猜、不补。remaining 已在后端过滤掉本地合成的 agent。 -->
    <p class="fact" data-testid="cf-server-accepts">
      服务器只接受：<strong>{remaining.join(" / ")}</strong>
      {#if failure && failure.tried.length > 0}
        <span class="dim">（已尝试：{failure.tried.join(" / ")}，均未通过）</span>
      {:else}
        <span class="dim">（一次认证都没有发起）</span>
      {/if}
    </p>
  {/if}

  {#if headline}
    <p class="headline" data-testid="cf-headline">{headline}</p>
  {:else}
    <p class="headline" data-testid="cf-headline">{errorText || "连接失败"}</p>
  {/if}

  {#if hasJump}
    <p class="dim" data-testid="cf-jump-hint">
      本连接经过跳板，失败可能发生在跳板某一跳——一键改目标档案的动作已禁用，请到「配置认证…」逐跳核对。
    </p>
  {/if}

  {#if listing?.vault_locked}
    <p class="dim" data-testid="cf-vault-locked">保险库锁着：档案里绑定的凭据本次取不到。</p>
  {/if}

  <div class="actions">
    {#if profile}
      <button type="button" class="primary" data-testid="cf-retry" disabled={busy}
              title={isAuth && acceptsPassword ? "重连；服务器收口令时会弹框问你本次的口令" : "重新拨号一次"}
              onclick={() => void run(onRetry)}>
        {isAuth && acceptsPassword ? "重试并输入口令" : "重试连接"}
      </button>
    {/if}

    {#if isAuth && acceptsPublicKey && profile && !agentAlreadyOn}
      <!-- 禁用要说原因（title）：一颗点了没变化的按钮比没有按钮更糟。 -->
      <button type="button" data-testid="cf-agent" disabled={busy || hasJump || !!listing?.agent_error}
              title={hasJump
                ? "有跳板：失败可能在别的一跳，请用「配置认证…」逐跳设置"
                : (listing?.agent_error ?? "在档案上勾选「使用本地 SSH Agent 认证」并立刻重连（需已 ssh-add 加载密钥）")}
              onclick={() => void run(onEnableAgent)}>
        用本地 SSH Agent 重试
      </button>
    {/if}

    {#if profile}
      <button type="button" data-testid="cf-configure" disabled={busy}
              title="打开会话属性并直接落在「认证」页"
              onclick={onConfigureAuth}>
        配置认证…
      </button>
    {/if}

    {#if listing?.vault_locked}
      <button type="button" data-testid="cf-unlock" disabled={busy} onclick={onUnlockVault}>解锁保险库</button>
    {/if}

    <button type="button" class="link" data-testid="cf-detail" onclick={onDetail}>完整错误</button>
  </div>

  {#if moreNotes.length > 0}
    <details class="more" data-testid="cf-more">
      <summary>更多诊断（{moreNotes.length}）</summary>
      <ul>
        {#each moreNotes as n}<li>{n}</li>{/each}
      </ul>
    </details>
  {/if}
</div>

<style>
  .panel {
    position: absolute; inset: 0;
    display: flex; flex-direction: column; justify-content: center; align-items: center;
    gap: 10px; padding: 24px;
    color: var(--fs-fg-primary); font-size: 12.5px;
    text-align: center;
  }
  .head { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; justify-content: center; }
  .dot { color: var(--fs-danger); }
  .who, .fp { font-family: var(--fs-font-mono, monospace); font-size: 11.5px; color: var(--fs-fg-secondary); }
  .fp { opacity: .85; }
  .fact { margin: 0; }
  .headline { margin: 0; max-width: 640px; line-height: 1.55; color: var(--fs-fg-primary); overflow-wrap: anywhere; }
  .dim { color: var(--fs-fg-secondary); font-size: 11.5px; margin: 0; max-width: 640px; }
  .actions { display: flex; gap: 8px; flex-wrap: wrap; justify-content: center; margin-top: 4px; }
  .actions button {
    background: var(--fs-bg-elevated); color: var(--fs-fg-primary);
    border: 1px solid var(--fs-border); border-radius: var(--fs-radius);
    padding: 4px 12px; cursor: pointer; font: inherit;
  }
  .actions button:hover:not(:disabled) { background: var(--fs-bg-hover); }
  .actions button:disabled { color: var(--fs-fg-disabled); cursor: not-allowed; }
  .actions .primary { background: var(--fs-accent); color: var(--fs-accent-fg); border-color: var(--fs-accent); }
  .actions .primary:hover:not(:disabled) { background: var(--fs-accent-hover); }
  .actions .link { background: none; border: none; color: var(--fs-accent); text-decoration: underline; padding: 4px 6px; }
  .more { color: var(--fs-fg-secondary); font-size: 11.5px; max-width: 640px; text-align: left; }
  .more ul { margin: 6px 0 0; padding-left: 18px; }
  .more li { overflow-wrap: anywhere; }
</style>
