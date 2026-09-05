<script lang="ts">
  import { invoke } from "../lib/ipc";
  import { useFocusTrap } from "../lib/focusTrap";

  let {
    open = false,
    mode = "unlock",
    onClose = () => {},
    onUnlocked = () => {},
  }: { open?: boolean; mode?: "setup" | "unlock"; onClose?(): void; onUnlocked?(): void } = $props();

  let pass1 = $state("");
  let pass2 = $state("");
  let err = $state("");
  let ack = $state(false);
  let shake = $state(false);
  // bind:this 目标必须声明为 $state：Svelte 5 runes 下普通 let 的赋值不进入响应图，
  // 下面的 $effect 读到的会是挂载前的 undefined，焦点陷阱静默失效（non_reactive_update）。
  let dialogEl: HTMLDivElement | undefined = $state();
  let firstInput: HTMLInputElement | undefined = $state();

  /** 首次设置时留空应用密码 = 走「仅 keyring」这条路，需要用户明确知情（审计2 #28）。 */
  const keyringOnly = $derived(mode === "setup" && !pass1 && !pass2);

  $effect(() => {
    // 审计2 #30：**关闭时也要清**。历史实现是 `if (!open) return;` 先行早退，于是
    // 「取消」「Escape」「解锁成功」之后，用户刚敲进去的应用密码原样留在组件状态里——
    // 而这个对话框挂在 App 顶层，它的组件状态实际活到整个进程结束。
    //
    // 说清楚这一步做到了什么、做不到什么：JS 的字符串不可变，没有任何办法把它在堆上擦掉。
    // 这里能做的只是**尽早丢掉引用**，让那份字符串变成可回收对象。这不等于清零
    //（真正的清零发生在 Rust 侧，见 vault_cmd 各命令入口的 Zeroizing），
    // 但「一直持有」与「立刻不持有」之间的差别是实打实的。
    pass1 = ""; pass2 = ""; err = ""; ack = false;
    if (!open || !dialogEl) return; // 关闭时 bind:this 会置回 undefined，此处兜底
    return useFocusTrap(dialogEl, { initial: firstInput ?? null });
  });

  function fail(msg: string) {
    err = msg;
    shake = true;
    setTimeout(() => (shake = false), 400);
  }

  /** 后端 `fs_vault::Error::KeyringMissing` 的 Display 前缀（crates/vault/src/error.rs）；两侧同批改。 */
  const KEYRING_MISSING = "keyring entry missing";

  /**
   * 把解锁失败翻成一句**说真话**的文案（路线图 4c，2026-09-02）。
   *
   * 此前无论什么原因都是「解锁失败（密码错误或 keyring/文件异常）」——用户唯一能做的是再试一次
   * 空口令；而对「没设应用口令 + keyring 条目已不在」的库，再试一万次也开不了：主密钥只存在于
   * 那条被清掉的凭据里，世上再无第二份。这件事要直说，并指向唯一的出路（备份）。
   * 分岔依据从库文件头读（`vault_file_has_passphrase`，不要求解锁）；读不到就退回通用文案，不猜。
   */
  async function explainUnlockFailure(e: unknown): Promise<string> {
    const raw = String(e);
    if (!raw.includes(KEYRING_MISSING)) return `解锁失败（密码错误或 keyring/文件异常）：${raw}`;
    let hasPassphrase: boolean | null = null;
    try {
      hasPassphrase = await invoke<boolean | null>("vault_file_has_passphrase");
    } catch {
      hasPassphrase = null; // 库文件头读不出：不据此下结论
    }
    if (hasPassphrase === false) {
      return "解锁失败：系统凭据库里已没有这个保险库的主密钥（换了机器、重装系统，或凭据管理器被清理），"
        + "而建库时没有设应用密码——库里的凭据无法恢复，再试空口令也不会有结果。"
        + "出路只有「工具 → 保险库管理 → 从备份恢复」（若曾导出过备份）；否则只能删除库文件后重新建库。";
    }
    if (hasPassphrase === true) {
      return "解锁失败：系统凭据库里已没有这个保险库的主密钥，留空走不通；这个库设了应用密码，请输入应用密码解锁。";
    }
    return `解锁失败：系统凭据库里已没有这个保险库的主密钥。若建库时设过应用密码请输入；没设过则只能从备份恢复。（${raw}）`;
  }

  let busy = $state(false);
  async function submit() {
    if (busy) return;
    busy = true;
    err = "";
    try {
      if (mode === "setup") {
        // 判不一致要看**两个框**，不能只看 pass1：原实现是 `pass1 && pass1 !== pass2`，
        // 于是「只在第二个框里敲了密码」时 pass1 为空 → 短路 → 静默按「不设密码」建库。
        // 用户以为自己设了应用密码，实际得到的是一份只有本机 keyring 能开的库。
        if ((pass1 || pass2) && pass1 !== pass2) { fail("两次输入的应用密码不一致"); return; }
        // 审计2 #28：不设应用密码是**不可逆**的单点依赖，必须让用户明确知情后再落库。
        // 这里不是弹一句劝告了事——`vault_init` 一旦以 null 建成，主密钥就只存在于本机
        // 凭据库，且从此**导不出可离机恢复的备份**（`export_backup` 会直接拒绝）。
        if (!pass1 && !ack) { fail("请先勾选下方的确认，或设置一个应用密码"); return; }
        await invoke("vault_init", { passphrase: pass1 || null });
      } else {
        // 留空 **不是**输入错误，而是 keyring 路径的正规入口。
        //
        // 首次设置里「应用密码」是可选项，留空即「主密钥仅存系统 keyring」——这还是推荐的
        // 默认做法（见上方 setup 提示）。后端 `vault_unlock` 正是按口令有无二分的：
        // `Some(p)` → `unlock_with_passphrase`，`None` 且库文件已存在 → `unlock_with_keyring`。
        // 此处原本先 `if (!pass1) { fail("请输入应用密码"); return; }` 拦下，等于把 `None`
        // 这一支彻底封死：按默认配置建库的用户，此后**永远解锁不了自己的 Vault**，
        // 且提示语还在催他输入一个从来就不存在的密码。
        await invoke("vault_unlock", { passphrase: pass1 || null });
      }
      onUnlocked();
      onClose();
    } catch (e) {
      fail(mode === "setup" ? `初始化失败：${e}` : await explainUnlockFailure(e));
    } finally { busy = false; }
  }
</script>

{#if open}
  <div class="overlay" role="presentation">
    <!-- tabindex="-1"：role="dialog" 属交互角色须可聚焦；取 -1 是让容器只能被脚本聚焦、不进 Tab 序列，
         也因此不会被 focusTrap 的 FOCUSABLE 选择器（排除 tabindex="-1"）收进循环名单 -->
    <div class="dialog" class:shake role="dialog" aria-modal="true" tabindex="-1"
         aria-label={mode === "setup" ? "Vault 首次设置" : "解锁 Vault"}
         data-testid="vault-dialog" bind:this={dialogEl}
         onkeydown={(e) => { if (e.key === "Escape") onClose(); }}>
      <h2>{mode === "setup" ? "Vault 首次设置" : "解锁 Vault"}</h2>
      {#if mode === "setup"}
        <p class="hint">保险库会加密保存连接密码和私钥。主密钥默认保存在本机系统凭据库。
          设置应用密码后，可以在系统凭据库不可用时解锁，并导出可在其他电脑恢复的备份。</p>
        <label>应用密码（可选） <input type="password" bind:value={pass1} bind:this={firstInput} data-testid="vault-pass1" /></label>
        <label>再次输入 <input type="password" bind:value={pass2} data-testid="vault-pass2" /></label>
        {#if keyringOnly}
          <!-- 「可选」两个字掩盖了一个不可逆的选择，这里把代价说全（审计2 #28）。
               勾选框而不是纯提示：默认路径的后果必须由用户按一下才生效。 -->
          <div class="warn" data-testid="vault-keyring-only-warning">
            <p>不设应用密码时，主密钥<strong>只存在于本机系统 keyring</strong>。这意味着：</p>
            <ul>
              <li>重装系统、更换电脑、凭据管理器被清理，库中全部凭据将<strong>永久无法恢复</strong>；</li>
              <li>无法导出可离机恢复的备份（备份里的主密钥要靠应用密码封装）。</li>
            </ul>
            <p>应用密码随时可在「工具 → 保险库管理」中补设。</p>
            <label class="check">
              <input type="checkbox" bind:checked={ack} data-testid="vault-ack" />
              我已了解上述风险，仍然只用系统 keyring
            </label>
          </div>
        {/if}
      {:else}
        <!-- 必须明说「留空」是一条正规路径：Vault 的默认形态就是无应用密码、主密钥托管在
             系统 keyring。若这里只写「应用密码」，用户会以为自己忘了一个从未设过的密码。 -->
        <!-- 文案口径（2026-09-01 用户裁定「保持现状」后改写）：
             把「应用密码不是一道硬闸」这件事说出来，而不是让界面看起来像一道真锁。
             事实：主密钥存在系统 keyring 里，解锁走 keyring 那条路时不校验应用密码
             （crates/vault/src/store.rs 的 unlock_with_keyring 不读 passphrase_phc）。
             这是刻意保留的行为——换机器/备份恢复后还开得了库靠的正是它；
             代价是它挡不住已经能操作你这台电脑的人。含糊其辞比缺口本身更糟：
             用户会以为设了密码就安全，从而把更敏感的东西放进来。 -->
        <p class="hint">
          可输入应用密码解锁，也可留空使用本机系统凭据库解锁。
        </p>
        <p class="hint">
          即使设置了应用密码，本机系统凭据库仍可直接解锁。因此应用密码不能阻止已经能操作这台电脑的人访问保险库。
        </p>
        <label>应用密码（未设置则留空） <input type="password" bind:value={pass1} bind:this={firstInput} data-testid="vault-pass" /></label>
      {/if}
      {#if err}<p class="err" role="alert" data-testid="vault-err">{err}</p>{/if}
      <footer>
        <button onclick={onClose}>取消</button>
        <button class="primary" onclick={() => void submit()} disabled={busy} data-testid="vault-submit">
          {busy ? "处理中…" : mode === "setup" ? "创建保险库" : "解锁"}
        </button>
      </footer>
    </div>
  </div>
{/if}

<style>
  .overlay { position: fixed; inset: 0; background: rgba(0,0,0,.5); display: grid; place-items: center; z-index: 60; }
  .dialog { width: 420px; max-width: 92vw; max-height: 92vh; overflow-y: auto; padding: 16px 20px; background: var(--fs-bg-elevated); border: 1px solid var(--fs-border); border-radius: 6px; color: var(--fs-fg-primary); display: flex; flex-direction: column; gap: 10px; }
  .dialog h2 { margin: 0; font-size: 14px; }
  .dialog label { display: flex; flex-direction: column; gap: 4px; font-size: 12.5px; color: var(--fs-fg-secondary); }
  .dialog input { padding: 5px 8px; background: var(--fs-bg-input); border: 1px solid var(--fs-border); border-radius: 4px; color: var(--fs-fg-primary); }
  .hint { font-size: 12px; color: var(--fs-fg-secondary); margin: 0; }
  .warn { font-size: 12px; color: var(--fs-fg-secondary); border: 1px solid var(--fs-warn); border-radius: 4px; padding: 8px 10px; display: flex; flex-direction: column; gap: 6px; }
  .warn p { margin: 0; }
  .warn ul { margin: 0; padding-left: 18px; display: flex; flex-direction: column; gap: 3px; }
  .warn .check { flex-direction: row; align-items: center; gap: 6px; color: var(--fs-fg-primary); cursor: pointer; }
  .warn .check input { width: auto; }
  .err { color: var(--fs-danger); font-size: 12px; margin: 0; }
  footer { display: flex; justify-content: flex-end; gap: 8px; }
  footer button { padding: 5px 14px; border: 1px solid var(--fs-border); background: var(--fs-bg-panel); color: var(--fs-fg-primary); border-radius: 4px; cursor: pointer; }
  footer button.primary { background: var(--fs-accent); color: var(--fs-accent-fg); border-color: var(--fs-accent); }
  .shake { animation: vault-shake .18s 2; }
  @keyframes vault-shake { 0%,100% { transform: translateX(0); } 25% { transform: translateX(-4px); } 75% { transform: translateX(4px); } }
</style>
