<script lang="ts">
  /**
   * 密钥 / 代理管理器（M4b 出口第 13 项）。
   *
   * 只读的一份清单：**指纹 / 类型 / 注释**，没有别的。后端给的结构里根本没有能装下
   * 密钥材料的字段（`fs_sshengine::keyinfo::KeyInfo` 只有三个 String），
   * 所以这里不需要小心地「不显示私钥」——那件事写不出来。
   *
   * 「agent 传输开关」不在本对话框里：Profile 上已经有 `auth.allow_agent`，
   * 再加一个全局开关会制造一个没人答得上来的问题（全局关、这个 Profile 开，用不用？）。
   * 这里只给一句指路。
   */
  import { useFocusTrap } from "../lib/focusTrap";
  import { invoke } from "../lib/ipc";

  let { open = false, onClose = () => {} }: { open?: boolean; onClose?(): void } = $props();

  interface KeyEntry {
    source: "vault" | "agent";
    id: number | null;
    label: string | null;
    fingerprint: string | null;
    algorithm: string | null;
    comment: string | null;
    status: "ok" | "encrypted" | "not-a-key";
  }
  interface KeyListing {
    keys: KeyEntry[];
    agent_error: string | null;
    vault_locked: boolean;
  }

  let dialogEl = $state<HTMLDivElement | undefined>();
  let closeBtn = $state<HTMLButtonElement | undefined>();
  let listing = $state<KeyListing | null>(null);
  let loading = $state(false);
  let error = $state("");

  $effect(() => {
    if (!open || !dialogEl || !closeBtn) return;
    return useFocusTrap(dialogEl, { initial: closeBtn });
  });

  /**
   * 每次打开都重新取。
   *
   * agent 里的内容会在对话框关着的时候变（用户去别处 `ssh-add` 了），
   * 缓存一份等于给他看一张过期的表——而这张表的用途正是「核对现在有哪些」。
   */
  $effect(() => {
    if (!open) return;
    void load();
  });

  async function load(): Promise<void> {
    loading = true;
    error = "";
    try {
      listing = await invoke<KeyListing>("key_list");
    } catch (e) {
      error = String(e);
      listing = null;
    } finally {
      loading = false;
    }
  }

  const STATUS_TEXT: Record<KeyEntry["status"], string> = {
    ok: "",
    // 「已加密」是**正常状态**，文案与配色都要中性。画成红色失败会让用户
    // 去修一件没坏的事。
    encrypted: "已加密（要输口令才读得出指纹）",
    "not-a-key": "读不出：这条记录不是一份私钥",
  };
</script>

{#if open}
  <div class="overlay" role="presentation" onclick={onClose}>
    <div class="dialog" role="dialog" aria-modal="true" aria-label="密钥 / 代理管理器" tabindex="-1"
         data-testid="key-manager" bind:this={dialogEl}
         onclick={(e) => e.stopPropagation()}
         onkeydown={(e) => { if (e.key === "Escape") onClose(); }}>
      <h2>密钥 / 代理管理器</h2>
      <p class="hint" data-testid="key-manager-scope">
        在这里核对保险库和本地 SSH Agent 中的密钥指纹、类型与注释。
        添加私钥请打开会话属性的「认证」页；此列表不会显示私钥内容。
      </p>

      {#if loading}
        <p class="hint" data-testid="key-manager-loading">正在读取…（探测 agent 可能要一两秒）</p>
      {:else if error}
        <p class="hint error" data-testid="key-manager-error">{error}</p>
      {:else if listing}
        {#if listing.vault_locked}
          <!-- 一个锁着的 Vault 与一个空 Vault 在列表上长得一样，而用户会以为
               自己的密钥丢了。所以锁着这件事必须说出来。 -->
          <p class="hint warn" data-testid="key-manager-vault-locked">
            Vault 是锁着的，列表里不含存在 Vault 里的密钥。解锁后再打开这里。
          </p>
        {/if}
        {#if listing.agent_error}
          <!-- 同理不静默：空列表会被读成「agent 在跑但里面没有密钥」，
               那要去 ssh-add；而 agent 没在跑要去启动它。两件事。 -->
          <p class="hint warn" data-testid="key-manager-agent-error">
            SSH Agent 不可用：{listing.agent_error}
          </p>
        {/if}

        {#if listing.keys.length === 0}
          <p class="hint" data-testid="key-manager-empty">没有找到任何密钥。</p>
        {:else}
          <table data-testid="key-manager-table">
            <thead>
              <tr><th>来源</th><th>类型</th><th>指纹</th><th>注释 / 标签</th></tr>
            </thead>
            <tbody>
              {#each listing.keys as k, i (k.source + (k.id ?? "") + (k.fingerprint ?? i))}
                <tr data-testid="key-row">
                  <!-- 来源必须显示：Vault 里的删得掉，agent 里的要去 `ssh-add -d`。
                       不分来源的话，用户会找一个删不掉的条目的删除按钮。 -->
                  <td data-testid="key-source">{k.source === "vault" ? "Vault" : "Agent"}</td>
                  <td>{k.algorithm ?? "—"}</td>
                  <td class="fp" data-testid="key-fingerprint">
                    {#if k.fingerprint}{k.fingerprint}{:else}<span class="muted">{STATUS_TEXT[k.status]}</span>{/if}
                  </td>
                  <td>{k.label ?? k.comment ?? ""}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        {/if}
      {/if}

      <p class="hint" data-testid="key-manager-agent-hint">
        要不要用 SSH Agent 认证，是<strong>每个连接自己的设置</strong>：
        在连接属性的「认证」页勾选「使用本地 SSH Agent」，然后连接该主机。
      </p>

      <footer>
        <button type="button" data-testid="key-manager-refresh" disabled={loading} onclick={load}>刷新</button>
        <button bind:this={closeBtn} type="button" data-testid="key-manager-close" onclick={onClose}>关闭</button>
      </footer>
    </div>
  </div>
{/if}

<style>
  .overlay { position: fixed; inset: 0; background: rgba(0,0,0,.5); display: grid; place-items: center; z-index: 60; }
  .dialog { width: 720px; max-width: 94vw; max-height: 86vh; overflow: auto; padding: 16px 20px; background: var(--fs-bg-elevated); border: 1px solid var(--fs-border); border-radius: 6px; color: var(--fs-fg-primary); }
  h2 { margin: 0 0 8px; font-size: 14px; }
  .hint { font-size: 11.5px; color: var(--fs-fg-secondary); margin: 0 0 10px; line-height: 1.6; }
  .hint.warn { color: var(--fs-warn, #d29922); }
  .hint.error { color: var(--fs-danger, #e05252); }
  table { width: 100%; border-collapse: collapse; font-size: 12px; }
  th, td { text-align: left; padding: 4px 8px; border-bottom: 1px solid var(--fs-border); }
  th { color: var(--fs-fg-secondary); font-weight: 500; }
  /* 指纹要能整串看到、能整串选中复制——那正是用户打开这个列表要做的事。 */
  .fp { font-family: var(--fs-font-mono, monospace); font-size: 11px; word-break: break-all; user-select: text; }
  .muted { color: var(--fs-fg-secondary); font-style: italic; }
  footer { display: flex; justify-content: flex-end; gap: 8px; margin-top: 14px; }
  footer button { padding: 5px 14px; border: 1px solid var(--fs-border); background: var(--fs-bg-panel); color: var(--fs-fg-primary); border-radius: 4px; cursor: pointer; }
  footer button:disabled { opacity: .5; cursor: default; }
</style>
