<script lang="ts">
  /* ── 保险库管理（审计2 #27/#28 的 UI 一半，发布门槛第 5 条）──────────────────────
   *
   * 这个对话框补的不是「几个按钮」，而是一个凭据管理器缺了就不成立的四件事：
   *   删除 —— 存错一条私钥、某台服务器下线，密文就永远躺在 vault.json 里拿不掉；
   *   改密 —— 应用密码泄露之后没有轮换路径，只能连库一起弃掉；
   *   备份 —— 主密钥在 OS 凭据库里，机器一坏就是全部凭据一起没；
   *   恢复 —— 备份没有恢复入口，等于没有备份。
   *
   * `Store::delete` 与 `Store::change_passphrase` 在 core 里早就写好了，只是从未接到
   * IPC 上——**没有注册就等于没有功能**，这是本次要消灭的那类「实现了但用户到不了」。
   *
   * 「恢复」一栏刻意在**锁定状态下也可用**：需要恢复的时刻，恰恰就是库打不开的时刻。
   * 把它锁在解锁之后，等于把这条路挡在它自己要解决的那个问题后面。
   * ────────────────────────────────────────────────────────────────────────────── */
  import { invoke } from "../lib/ipc";
  import { useFocusTrap } from "../lib/focusTrap";

  let {
    open = false,
    locked = true,
    onClose = () => {},
    onChanged = () => {},
    onUnlock,
  }: {
    open?: boolean;
    /** 由 App 持有的解锁态。记录与应用密码两栏需要它，备份/恢复不需要。 */
    locked?: boolean;
    onClose?(): void;
    /** 库内容或解锁态被本对话框改动过（删除/改密/恢复），供 App 刷新自己的状态。 */
    onChanged?(): void;
    onUnlock?(): void;
  } = $props();

  type SecretRow = { id: number; kind: string; label: string };
  type BackupRow = { name: string; bytes: number; modified_unix: number };

  let records = $state<SecretRow[]>([]);
  let backups = $state<BackupRow[]>([]);
  let backupDir = $state("");
  let hasPass = $state(false);
  let err = $state("");
  let ok = $state("");

  /** 正在等待二次确认的记录 id（行内两步删除，避免再套一层模态与焦点陷阱）。 */
  let confirmDelete = $state<number | null>(null);

  let curPass = $state("");
  let newPass = $state("");
  let newPass2 = $state("");

  /** 正在等待口令的备份名；null = 没有恢复流程在进行中。 */
  let restoreTarget = $state<string | null>(null);
  let restorePass = $state("");

  let dialogEl: HTMLDivElement | undefined = $state();
  let closeBtn: HTMLButtonElement | undefined = $state();

  $effect(() => {
    // 与 VaultDialog 同口径（审计2 #30）：**关闭时也清**。这里的 curPass/newPass/restorePass
    // 都是用户刚敲进去的应用密码，而本组件挂在 App 顶层、活到进程结束。
    // 能做到的是尽早丢掉引用（JS 字符串不可变，无法擦除）；真正的清零在 Rust 侧。
    resetTransient();
    if (!open) {
      records = [];
      backups = [];
      return;
    }
    void refresh();
  });

  // 焦点陷阱单独一个 effect：`dialogEl` 由 bind:this 在挂载后才赋值，写在上面那个 effect 里会让
  // 它整体再跑一遍，于是每次打开都白取一轮数据。分开之后取数只依赖 open/locked。
  $effect(() => {
    if (!open || !dialogEl) return;
    return useFocusTrap(dialogEl, { initial: closeBtn ?? null });
  });

  function resetTransient() {
    curPass = "";
    newPass = "";
    newPass2 = "";
    restorePass = "";
    restoreTarget = null;
    confirmDelete = null;
    err = "";
    ok = "";
  }

  async function refresh() {
    // 同步读一次再进 await：这样 `locked` 是上面那个 effect 的真依赖，本对话框开着的时候
    // 从别处解锁/自动锁定也会重取。await 之后再读就不在依赖收集范围内了。
    const isLocked = locked;
    // 备份那一栏在锁定状态下也要能用，所以它与记录栏分开取、各自失败各自算。
    try {
      backupDir = await invoke<string>("vault_backup_dir");
      backups = await invoke<BackupRow[]>("vault_list_backups");
    } catch (e) {
      err = `读取备份列表失败：${e}`;
    }
    if (isLocked) {
      records = [];
      hasPass = false;
      return;
    }
    try {
      records = await invoke<SecretRow[]>("vault_list_secrets");
      hasPass = await invoke<boolean>("vault_has_passphrase");
    } catch (e) {
      err = `读取保险库失败：${e}`;
    }
  }

  const KIND_LABEL: Record<string, string> = { password: "密码", private_key: "私钥", api_key: "API Key" };
  /** 未知类别原样回显，绝不回落成某个具体用途（与 ProfileDialog 同口径，审计2 #20）。 */
  const kindLabel = (k: string) => KIND_LABEL[k] ?? k;

  function fmtBytes(n: number): string {
    return n < 1024 ? `${n} B` : n < 1024 * 1024 ? `${(n / 1024).toFixed(1)} KB` : `${(n / 1048576).toFixed(1)} MB`;
  }
  function fmtTime(unix: number): string {
    return unix ? new Date(unix * 1000).toLocaleString() : "时间未知";
  }

  async function doDelete(id: number) {
    err = ""; ok = "";
    try {
      await invoke("vault_delete_secret", { recordId: id });
      confirmDelete = null;
      ok = `已删除记录 #${id}`;
      await refresh();
      onChanged();
    } catch (e) {
      err = `删除失败：${e}`;
    }
  }

  async function doCopy(id: number) {
    err = ""; ok = "";
    try {
      await invoke("vault_copy_to_clipboard", { recordId: id });
      ok = `记录 #${id} 已复制到剪贴板（会按设置自动清除）`;
    } catch (e) {
      err = `复制失败：${e}`;
    }
  }

  async function doChangePassphrase() {
    err = ""; ok = "";
    if (!newPass) { err = "新的应用密码不能为空"; return; }
    if (newPass !== newPass2) { err = "两次输入的新密码不一致"; return; }
    try {
      // 库里已有口令却不给旧口令时由后端判（`Store::change_passphrase` 返回 Locked）：
      // 前端再判一次就是第二处口径，而两处口径迟早分叉。
      await invoke("vault_change_passphrase", { current: curPass || null, newPassphrase: newPass });
      curPass = ""; newPass = ""; newPass2 = "";
      ok = hasPass ? "应用密码已更新" : "应用密码已设置：现在可以导出可离机恢复的备份了";
      await refresh();
      onChanged();
    } catch (e) {
      err = `修改应用密码失败：${e}`;
    }
  }

  async function doBackup() {
    err = ""; ok = "";
    try {
      const name = await invoke<string>("vault_create_backup");
      ok = `已导出备份 ${name}`;
      await refresh();
    } catch (e) {
      err = `${e}`;
    }
  }

  async function doRestore() {
    if (!restoreTarget) return;
    err = ""; ok = "";
    try {
      const displaced = await invoke<string | null>("vault_restore_backup", {
        name: restoreTarget,
        passphrase: restorePass,
      });
      restoreTarget = null;
      restorePass = "";
      // 恢复成功 = 内存里的解锁状态已作废（后端把 Store 丢了并广播 vault:locked）。
      // 说清「原有的库留档在哪儿」，否则用户恢复错了就没有退路可走。
      ok = displaced
        ? `已从备份恢复。原有保险库已留档为 ${displaced}，请重新解锁。`
        : "已从备份恢复，请重新解锁。";
      await refresh();
      onChanged();
    } catch (e) {
      err = `恢复失败：${e}`;
    }
  }
</script>

{#if open}
  <div class="overlay" role="presentation">
    <!-- tabindex="-1"：role="dialog" 属交互角色须可聚焦；取 -1 是让容器只能被脚本聚焦、不进 Tab 序列 -->
    <div class="dialog" role="dialog" aria-modal="true" tabindex="-1" aria-label="保险库管理"
         data-testid="vault-mgr" bind:this={dialogEl}
         onkeydown={(e) => { if (e.key === "Escape") onClose(); }}>
      <header>
        <h2>保险库管理</h2>
        <button bind:this={closeBtn} onclick={onClose} data-testid="vm-close">关闭</button>
      </header>

      {#if err}<p class="err" role="alert" data-testid="vm-err">{err}</p>{/if}
      {#if ok}<p class="ok" role="status" data-testid="vm-ok">{ok}</p>{/if}

      <section>
        <h3>凭据记录</h3>
        {#if locked}
          <p class="hint" data-testid="vm-records-locked">保险库已锁定。解锁后才能查看、复制或删除记录。</p>
          {#if onUnlock}<button onclick={onUnlock}>解锁保险库…</button>{/if}
        {:else if records.length === 0}
          <p class="hint">还没有任何凭据记录。</p>
        {:else}
          <ul class="records" data-testid="vm-records">
            {#each records as r (r.id)}
              <li data-testid={`vm-record-${r.id}`}>
                <span class="label">#{r.id} · {kindLabel(r.kind)} · {r.label}</span>
                {#if confirmDelete === r.id}
                  <!-- 行内二次确认：删除不可撤销，而这里删掉的可能是唯一一份私钥。
                       不另开模态是为了不和本对话框的焦点陷阱互相抢。 -->
                  <span class="confirm" data-testid={`vm-del-confirm-row-${r.id}`}>
                    删除后无法恢复（除非有备份），确定？
                    <button class="danger" onclick={() => void doDelete(r.id)} data-testid={`vm-del-yes-${r.id}`}>确认删除</button>
                    <button onclick={() => (confirmDelete = null)} data-testid={`vm-del-no-${r.id}`}>取消</button>
                  </span>
                {:else}
                  <button onclick={() => void doCopy(r.id)} data-testid={`vm-copy-${r.id}`}>复制</button>
                  <button onclick={() => (confirmDelete = r.id)} data-testid={`vm-del-${r.id}`}>删除…</button>
                {/if}
              </li>
            {/each}
          </ul>
        {/if}
      </section>

      <section>
        <h3>应用密码</h3>
        {#if locked}
          <p class="hint">保险库已锁定，解锁后可设置或修改应用密码。</p>
        {:else}
          <p class="hint" data-testid="vm-pass-state">
            {#if hasPass}
              已设置。修改需要提供当前密码。
            {:else}
              <strong>尚未设置。</strong>主密钥目前只存在于本机系统 keyring：凭据管理器一旦被清理、
              系统重装或更换电脑，库中全部凭据将永久无法恢复，且<strong>导不出可离机恢复的备份</strong>。
            {/if}
          </p>
          {#if hasPass}
            <label>当前密码 <input type="password" bind:value={curPass} data-testid="vm-pass-current" /></label>
          {/if}
          <label>新密码 <input type="password" bind:value={newPass} data-testid="vm-pass-new" /></label>
          <label>再次输入 <input type="password" bind:value={newPass2} data-testid="vm-pass-new2" /></label>
          <button class="primary" onclick={() => void doChangePassphrase()} data-testid="vm-pass-submit">
            {hasPass ? "修改应用密码" : "设置应用密码"}
          </button>
        {/if}
      </section>

      <section>
        <h3>备份与恢复</h3>
        <p class="hint">
          备份目录：<code data-testid="vm-backup-dir">{backupDir || "（未知）"}</code>
          <br />备份里的主密钥用应用密码封装，所以<strong>未设应用密码时导不出备份</strong>——
          换机器恢复靠的就是这把密码。
        </p>
        <button onclick={() => void doBackup()} disabled={locked} data-testid="vm-backup-create">立即导出备份</button>
        {#if backups.length === 0}
          <p class="hint" data-testid="vm-backups-empty">备份目录里还没有备份。</p>
        {:else}
          <ul class="records" data-testid="vm-backups">
            {#each backups as b (b.name)}
              <li data-testid={`vm-backup-${b.name}`}>
                <span class="label">{b.name} · {fmtBytes(b.bytes)} · {fmtTime(b.modified_unix)}</span>
                <button onclick={() => { restoreTarget = b.name; restorePass = ""; err = ""; ok = ""; }}
                        data-testid={`vm-restore-${b.name}`}>恢复…</button>
              </li>
            {/each}
          </ul>
        {/if}
        {#if restoreTarget}
          <!-- 恢复要用那份备份自己的应用密码，不是本机当前的：备份可能来自另一台机器，
               它自带一把不同的主密钥。后端会先把备份整份验通（一个字节都不写）才动现有库。 -->
          <div class="restore" data-testid="vm-restore-form">
            <p class="hint">
              将用 <strong>{restoreTarget}</strong> 顶替当前保险库。请输入<strong>导出这份备份时</strong>的应用密码。
              <br />现有保险库会先留档为 <code>vault.json.replaced-…</code>；口令不对或备份损坏时，
              现有保险库一个字节都不会被改动。
            </p>
            <label>该备份的应用密码 <input type="password" bind:value={restorePass} data-testid="vm-restore-pass" /></label>
            <div class="row">
              <button class="danger" onclick={() => void doRestore()} data-testid="vm-restore-confirm">确认恢复</button>
              <button onclick={() => { restoreTarget = null; restorePass = ""; }} data-testid="vm-restore-cancel">取消</button>
            </div>
          </div>
        {/if}
      </section>
    </div>
  </div>
{/if}

<style>
  .overlay { position: fixed; inset: 0; background: rgba(0,0,0,.5); display: grid; place-items: center; z-index: 60; }
  .dialog { width: 560px; max-width: 94vw; max-height: 86vh; overflow: auto; padding: 14px 18px; background: var(--fs-bg-elevated); border: 1px solid var(--fs-border); border-radius: 6px; color: var(--fs-fg-primary); display: flex; flex-direction: column; gap: 12px; }
  header { display: flex; align-items: center; justify-content: space-between; }
  h2 { margin: 0; font-size: 14px; }
  h3 { margin: 0 0 6px; font-size: 12.5px; color: var(--fs-fg-secondary); border-bottom: 1px solid var(--fs-border); padding-bottom: 4px; }
  section { display: flex; flex-direction: column; gap: 6px; }
  label { display: flex; flex-direction: column; gap: 4px; font-size: 12.5px; color: var(--fs-fg-secondary); }
  input { padding: 5px 8px; background: var(--fs-bg-input); border: 1px solid var(--fs-border); border-radius: 4px; color: var(--fs-fg-primary); }
  .hint { font-size: 12px; color: var(--fs-fg-secondary); margin: 0; line-height: 1.5; }
  .err { color: var(--fs-danger); font-size: 12px; margin: 0; }
  .ok { color: var(--fs-ok); font-size: 12px; margin: 0; }
  code { font-size: 11.5px; word-break: break-all; }
  ul.records { list-style: none; margin: 0; padding: 0; display: flex; flex-direction: column; gap: 4px; }
  ul.records li { display: flex; align-items: center; gap: 6px; font-size: 12.5px; }
  ul.records .label { flex: 1; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .confirm { display: flex; align-items: center; gap: 6px; font-size: 12px; color: var(--fs-danger); }
  .restore { border: 1px solid var(--fs-warn); border-radius: 4px; padding: 8px 10px; display: flex; flex-direction: column; gap: 6px; }
  .row { display: flex; gap: 8px; }
  button { padding: 4px 10px; border: 1px solid var(--fs-border); background: var(--fs-bg-panel); color: var(--fs-fg-primary); border-radius: 4px; cursor: pointer; font-size: 12px; }
  button.primary { background: var(--fs-accent); color: var(--fs-accent-fg); border-color: var(--fs-accent); }
  button.danger { background: var(--fs-danger); border-color: var(--fs-danger); color: #fff; }
  button:disabled { opacity: .5; cursor: not-allowed; }
</style>
