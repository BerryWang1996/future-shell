<script lang="ts">
  /**
   * 计划任务面板（M4a）。
   *
   * ## 界面上必须说清的一件事
   *
   * 计划任务**只在该连接有活动会话时执行**。到点了而那台机器没连着，就记一条
   * 「未执行」并写明原因，不会自动拨号——自动拨号要凭据，而凭据在保险库里、保险库
   * 有闲置自动锁定，凌晨三点大概率是锁着的。要让它不锁就得让用户为了一条定时任务
   * 永久关掉自动锁定，那是替用户做了一个安全决策（详见 app/src/scheduler.rs 模块头）。
   *
   * 所以面板顶部常驻一行说明。这不是免责声明，是产品边界——用户据此决定要不要把
   * 一台机器的会话一直开着。
   *
   * ## cron 不做「人话化」
   *
   * 带步长与列表的表达式的自然语言描述既长又容易写错，而错误的描述比没有描述更坏
   * （用户会信它）。取而代之的是**预览接下来三次触发**——那是不会说错的，而且校验与
   * 预览共用后端那一份 cron 实现（前端另写一份就是两套规则，迟早「界面说合法、后端
   * 说不合法」）。
   */
  import { invoke } from "../lib/ipc";
  import { toast } from "../lib/toast";
  import { useFocusTrap } from "../lib/focusTrap";
  import ConfirmDialog from "./ConfirmDialog.svelte";
  import {
    catchupLabel,
    formatAtOffset,
    formatLastFire,
    formatOffset,
    localTzOffsetMinutes,
    outcomeKind,
    outcomeLabel,
    scheduleSummary,
    type CronPreview,
    type ScheduledRun,
    scheduleRunTick,
    type ScheduledTask,
  } from "../lib/schedule";
  import type { Profile } from "../lib/types";

  let {
    open = false,
    profiles = [] as Profile[],
    onClose,
  }: { open?: boolean; profiles?: Profile[]; onClose(): void } = $props();

  let tasks = $state<ScheduledTask[]>([]);
  let err = $state("");
  let dialogEl = $state<HTMLDivElement | undefined>();

  // ── 编辑表单 ──────────────────────────────────────────────────────────────
  /** null = 未在编辑；0 = 新建；>0 = 编辑该 id */
  let editing = $state<number | null>(null);
  let fName = $state("");
  let fCron = $state("30 3 * * *");
  let fCommand = $state("");
  let fProfile = $state("");
  let fCatchup = $state<"skip" | "once">("skip");
  let fOffset = $state(localTzOffsetMinutes());
  /** 跟随系统时区（含夏令时）：开启后固定偏移只作展示兜底，判定用系统当前值 */
  let fFollowDst = $state(false);
  let preview = $state<CronPreview | null>(null);

  // ── 执行记录 ──────────────────────────────────────────────────────────────
  let runsFor = $state<number | null>(null);
  let runs = $state<ScheduledRun[]>([]);

  let pendingDelete = $state<ScheduledTask | null>(null);

  async function load() {
    try {
      tasks = await invoke<ScheduledTask[]>("schedule_list");
      err = "";
    } catch (e) {
      err = String(e);
    }
  }

  $effect(() => {
    if (!open) return;
    // 订阅执行计数：任务跑过之后「上次触发」与执行记录都变了，重取全量而不是
    // 把单条事件拼进现有列表（拼接会在面板关着的那段时间漏掉若干条）
    const tick = $scheduleRunTick;
    void tick;
    void load();
    if (runsFor !== null) {
      const t = tasks.find((x) => x.id === runsFor);
      if (t) void showRuns(t);
    }
  });

  $effect(() => {
    if (!open || !dialogEl) return;
    return useFocusTrap(dialogEl, { initial: null });
  });

  // cron 预览：随表达式与偏移变化重取。校验也在这里——**后端那一份实现**说了算。
  $effect(() => {
    if (editing === null) {
      preview = null;
      return;
    }
    const cron = fCron;
    const off = fFollowDst ? localTzOffsetMinutes() : fOffset;
    void (async () => {
      try {
        const p = await invoke<CronPreview>("schedule_preview_cron", {
          cron,
          tzOffsetMinutes: off,
        });
        // 表达式在 await 期间又被改过：落后的应答不得覆盖当前的预览
        if (fCron === cron && fOffset === off) preview = p;
      } catch (e) {
        if (fCron === cron) preview = { valid: false, error: String(e), nextFires: [], note: null };
      }
    })();
  });

  function startCreate() {
    editing = 0;
    fName = "";
    fCron = "30 3 * * *";
    fCommand = "";
    fProfile = profiles[0]?.id ?? "";
    fCatchup = "skip";
    fOffset = localTzOffsetMinutes();
    fFollowDst = false;
  }

  function startEdit(t: ScheduledTask) {
    editing = t.id;
    fName = t.name;
    fCron = t.cron;
    fCommand = t.command;
    fProfile = t.profile_id;
    fCatchup = t.catchup === "once" ? "once" : "skip";
    fOffset = t.tz_offset_minutes;
    fFollowDst = t.tz_follows_dst;
  }

  const canSave = $derived(
    !!fName.trim() && !!fCommand.trim() && !!fProfile && preview?.valid === true,
  );

  let saving = $state(false);
  async function save() {
    if (saving || !canSave) return;
    saving = true;
    const form = {
      name: fName,
      cron: fCron,
      command: fCommand,
      profileId: fProfile,
      catchup: fCatchup,
      tzOffsetMinutes: fOffset,
      tzFollowsDst: fFollowDst,
    };
    try {
      if (editing === 0) {
        await invoke("schedule_create", { form });
        toast.info("计划任务已创建");
      } else if (editing !== null) {
        await invoke("schedule_update", { id: editing, form });
        toast.info("计划任务已保存");
      }
      editing = null;
      await load();
    } catch (e) {
      toast.error(`保存失败：${e}`);
    } finally { saving = false; }
  }

  async function toggle(t: ScheduledTask) {
    try {
      await invoke("schedule_set_enabled", { id: t.id, enabled: !t.enabled });
      await load();
    } catch (e) {
      toast.error(`切换失败：${e}`);
    }
  }

  async function remove(t: ScheduledTask) {
    pendingDelete = null;
    try {
      await invoke("schedule_delete", { id: t.id });
      if (runsFor === t.id) runsFor = null;
      await load();
    } catch (e) {
      toast.error(`删除失败：${e}`);
    }
  }

  async function runNow(t: ScheduledTask) {
    try {
      await invoke("schedule_run_now", { id: t.id });
      toast.info(`已请求立即执行「${t.name}」`);
      // 执行是异步的；记录面板开着就刷一下
      if (runsFor === t.id) await showRuns(t);
    } catch (e) {
      toast.error(`执行失败：${e}`);
    }
  }

  async function showRuns(t: ScheduledTask) {
    runsFor = t.id;
    try {
      runs = await invoke<ScheduledRun[]>("schedule_runs", { taskId: t.id });
    } catch (e) {
      runs = [];
      toast.error(`读取执行记录失败：${e}`);
    }
  }

  const profileName = (id: string): string =>
    profiles.find((p) => p.id === id)?.name ?? `（已删除的连接 ${id.slice(0, 8)}）`;
  const shownTask = $derived(tasks.find((t) => t.id === runsFor) ?? null);
</script>

{#if open}
  <div class="overlay" role="presentation" onclick={onClose}>
    <div
      class="dialog" role="dialog" aria-modal="true" aria-label="计划任务" tabindex="-1"
      data-testid="schedule-dialog" bind:this={dialogEl}
      onclick={(e) => e.stopPropagation()}
      onkeydown={(e) => { if (e.key === "Escape") onClose(); }}
    >
      <header>
        <h3>计划任务</h3>
        <button class="x" onclick={onClose} data-testid="sched-close" aria-label="关闭">×</button>
      </header>

      <!-- 产品边界，常驻显示（不是免责声明：用户据此决定要不要把会话一直开着） -->
      <p class="scope" data-testid="sched-scope">
        计划任务只在该连接<strong>有活动会话</strong>时执行。到点而未连接时会记一条「未执行」
        并写明原因——本程序不会为了跑任务自动拨号（那需要保险库在无人时保持解锁）。
      </p>

      {#if err}<p class="err" role="alert" data-testid="sched-error">{err}</p>{/if}

      <div class="bar">
        <button onclick={startCreate} data-testid="sched-new" disabled={profiles.length === 0}
                title={profiles.length === 0 ? "先创建一个连接" : "新建计划任务"}>新建任务</button>
      </div>

      {#if tasks.length === 0}
        <p class="empty" data-testid="sched-empty">还没有计划任务。</p>
      {:else}
        <table data-testid="sched-table">
          <thead>
            <tr><th>启用</th><th>名称</th><th>计划</th><th>目标</th><th>补跑</th><th>上次触发</th><th></th></tr>
          </thead>
          <tbody>
            {#each tasks as t (t.id)}
              <tr class:off={!t.enabled}>
                <td>
                  <input type="checkbox" checked={t.enabled} data-testid="sched-toggle-{t.id}"
                         onchange={() => void toggle(t)} aria-label="启用 {t.name}" />
                </td>
                <td class="name" title={t.command}>{t.name}</td>
                <td class="cron" title={t.tz_follows_dst ? "跟随系统时区（含夏令时）" : "固定偏移（不跟随夏令时）"}>
                  <code>{scheduleSummary(t)}</code>{#if t.tz_follows_dst}<span class="tag">跟随系统时区</span>{/if}
                </td>
                <td>{profileName(t.profile_id)}</td>
                <td title={catchupLabel(t.catchup)}>{t.catchup === "once" ? "补一次" : "跳过"}</td>
                <td>{formatLastFire(t.last_fire_minute, t.tz_offset_minutes)}</td>
                <td class="ops">
                  <button onclick={() => startEdit(t)} data-testid="sched-edit-{t.id}">编辑</button>
                  <button onclick={() => void runNow(t)} data-testid="sched-run-{t.id}"
                          title="立刻执行一次，不影响定时节奏">立即执行</button>
                  <button onclick={() => void showRuns(t)} data-testid="sched-runs-{t.id}">记录</button>
                  <button class="del" onclick={() => (pendingDelete = t)} data-testid="sched-del-{t.id}">删除</button>
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      {/if}

      {#if editing !== null}
        <fieldset data-testid="sched-form">
          <legend>{editing === 0 ? "新建任务" : "编辑任务"}</legend>
          <label>名称
            <input bind:value={fName} data-testid="sched-f-name" placeholder="每夜备份" />
          </label>
          <label>目标连接
            <select bind:value={fProfile} data-testid="sched-f-profile">
              {#each profiles as p (p.id)}<option value={p.id}>{p.name}（{p.host}）</option>{/each}
            </select>
          </label>
          <label>命令
            <input bind:value={fCommand} data-testid="sched-f-command" placeholder="/opt/backup.sh" />
          </label>
          {#if profiles.length === 0}<p class="err">请先新建连接，再回来选择计划任务的目标。</p>{/if}
          <div class="ops" aria-label="常用计划">
            <span>常用计划：</span>
            <button type="button" class="mini" onclick={() => (fCron = "0 * * * *")}>每小时</button>
            <button type="button" class="mini" onclick={() => (fCron = "0 9 * * *")}>每天 09:00</button>
            <button type="button" class="mini" onclick={() => (fCron = "0 9 * * 1-5")}>工作日 09:00</button>
          </div>
          <label>cron（分 时 日 月 周）
            <input bind:value={fCron} data-testid="sched-f-cron" spellcheck="false" />
          </label>
          <label title="开启后按系统当前时区（含夏令时变化）判定；关闭则用下面的固定偏移——固定偏移不跟随夏令时，DST 地区每年会差一小时">
            <input type="checkbox" bind:checked={fFollowDst} data-testid="sched-f-follow-dst" />
            跟随系统时区（含夏令时）
          </label>
          <label>时区偏移（分钟，东为正）
            <input type="number" bind:value={fOffset} data-testid="sched-f-offset" step="15"
                   disabled={fFollowDst}
                   title={fFollowDst ? "跟随系统时区时由系统决定，此值仅作展示兜底" : ""} />
            <span class="hint">{formatOffset(fFollowDst ? localTzOffsetMinutes() : fOffset)}</span>
            <button type="button" class="mini" data-testid="sched-f-offset-now"
                    disabled={fFollowDst}
                    onclick={() => (fOffset = localTzOffsetMinutes())}>用本机当前时区</button>
          </label>
          <label>错过时
            <select bind:value={fCatchup} data-testid="sched-f-catchup">
              <option value="skip">跳过（默认）</option>
              <option value="once">补跑一次</option>
            </select>
          </label>

          <!-- 预览：cron 是否合法、接下来三次什么时候跑。校验以后端为准。 -->
          <div class="preview" data-testid="sched-preview">
            {#if !preview}
              <span class="hint">校验中…</span>
            {:else if !preview.valid}
              <span class="err" role="alert" data-testid="sched-cron-error">{preview.error}</span>
            {:else if preview.note}
              <!-- 空的触发列表是有信息的：它意味着「一年内不会触发」，必须说出来 -->
              <span class="warn" data-testid="sched-cron-note">{preview.note}</span>
            {:else}
              <span class="ok" data-testid="sched-next-fires">
                接下来：{preview.nextFires.map((s) => formatAtOffset(s, fOffset)).join("、")}
                （{formatOffset(fOffset)}）
              </span>
            {/if}
          </div>

          <footer>
            <button disabled={saving || !canSave} onclick={() => void save()} data-testid="sched-save">{saving ? "保存中…" : "保存"}</button>
            <button onclick={() => (editing = null)} data-testid="sched-cancel">取消</button>
          </footer>
        </fieldset>
      {/if}

      {#if runsFor !== null}
        <fieldset data-testid="sched-runs">
          <legend>执行记录{shownTask ? `：${shownTask.name}` : ""}</legend>
          {#if runs.length === 0}
            <p class="empty" data-testid="sched-runs-empty">还没有执行记录。</p>
          {:else}
            <ul>
              {#each runs as r (r.id)}
                <li>
                  <span class="oc {outcomeKind(r.outcome)}">{outcomeLabel(r.outcome)}</span>
                  <span class="when">{formatAtOffset(r.fired_at, shownTask?.tz_offset_minutes ?? 0)}</span>
                  {#if r.catchup}<span class="tag">补跑</span>{/if}
                  {#if r.exit_code !== null}<span class="code">exit {r.exit_code}</span>{/if}
                  {#if r.detail}<span class="detail" title={r.detail}>{r.detail}</span>{/if}
                </li>
              {/each}
            </ul>
          {/if}
          <footer><button onclick={() => (runsFor = null)} data-testid="sched-runs-close">收起</button></footer>
        </fieldset>
      {/if}
    </div>
  </div>
{/if}

<ConfirmDialog
  open={pendingDelete !== null}
  title="删除计划任务"
  message={pendingDelete
    ? `将删除「${pendingDelete.name}」及其全部执行记录，不可撤销。\n\n计划：${pendingDelete.cron}\n命令：${pendingDelete.command}`
    : ""}
  confirmText="删除"
  danger
  onConfirm={() => { if (pendingDelete) void remove(pendingDelete); }}
  onCancel={() => (pendingDelete = null)}
/>

<style>
  .overlay { position: fixed; inset: 0; background: rgba(0,0,0,.5); display: grid; place-items: center; z-index: 60; }
  .dialog { width: 860px; max-width: 95vw; max-height: 86vh; overflow-y: auto;
            padding: 14px 18px; background: var(--fs-bg-elevated); border: 1px solid var(--fs-border);
            border-radius: 6px; color: var(--fs-fg-primary); font-size: 13px; }
  header { display: flex; align-items: center; justify-content: space-between; }
  h3 { margin: 0; font-size: 14px; }
  .x { border: none; background: none; color: var(--fs-fg-secondary); font-size: 16px; cursor: pointer; }
  .scope { margin: 6px 0 10px; padding: 6px 8px; font-size: 12px; color: var(--fs-fg-secondary);
           background: var(--fs-bg-panel); border-left: 3px solid var(--fs-accent); border-radius: 3px; }
  .bar { margin-bottom: 8px; }
  .bar button, footer button { padding: 4px 12px; border: 1px solid var(--fs-border);
          background: var(--fs-bg-panel); color: var(--fs-fg-primary); border-radius: 4px; cursor: pointer; }
  .bar button:disabled, footer button:disabled { opacity: .5; cursor: default; }
  .empty { color: var(--fs-fg-secondary); }
  .err { color: var(--fs-danger); }
  .warn { color: var(--fs-warn); }
  .ok { color: var(--fs-fg-secondary); }
  table { width: 100%; border-collapse: collapse; }
  th, td { text-align: left; padding: 3px 6px 3px 0; }
  th { color: var(--fs-fg-secondary); font-weight: 500; font-size: 11px; }
  tr.off { opacity: .55; }
  td.name { max-width: 180px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  td.cron code { font-family: var(--fs-font-mono, monospace); }
  td.ops { white-space: nowrap; }
  td.ops button { padding: 0 6px; margin-left: 2px; font-size: 11px; border: 1px solid var(--fs-border);
                  background: none; color: var(--fs-fg-primary); border-radius: 3px; cursor: pointer; }
  td.ops button.del { color: var(--fs-danger); }
  fieldset { margin: 10px 0 0; border: 1px solid var(--fs-border); border-radius: 4px; padding: 8px 12px; }
  legend { color: var(--fs-fg-secondary); font-size: 12px; padding: 0 4px; }
  fieldset label { display: flex; align-items: center; gap: 8px; margin: 4px 0; color: var(--fs-fg-secondary); }
  /* 文本框都没写 type（默认 text），故只需 :not([type])；写 input[type="text"] 是个
     未命中的选择器，svelte-check 会如实报 unused。 */
  fieldset input:not([type]), fieldset select { flex: 1; min-width: 0;
          background: var(--fs-bg-input); color: var(--fs-fg-primary); border: 1px solid var(--fs-border);
          border-radius: 4px; padding: 3px 6px; font: inherit; }
  /* select 单独补右内距给下拉箭头让位（2026-09-01；input 不该跟着空一块） */
  fieldset select { padding-right: 22px; }
  fieldset input[type="number"] { width: 90px; background: var(--fs-bg-input); color: var(--fs-fg-primary);
          border: 1px solid var(--fs-border); border-radius: 4px; padding: 3px 6px; font: inherit; }
  .hint { color: var(--fs-fg-secondary); font-size: 12px; }
  button.mini { padding: 1px 8px; font-size: 11px; border: 1px solid var(--fs-border);
                background: none; color: var(--fs-fg-primary); border-radius: 3px; cursor: pointer; }
  .preview { margin: 6px 0; font-size: 12px; min-height: 1.4em; }
  fieldset footer { display: flex; justify-content: flex-end; gap: 8px; margin-top: 8px; }
  fieldset ul { list-style: none; margin: 0; padding: 0; max-height: 200px; overflow-y: auto; }
  fieldset li { display: flex; gap: 8px; align-items: baseline; font-size: 12px; padding: 1px 0; }
  .oc { min-width: 3.2em; }
  .oc.ok { color: var(--fs-success, #4caf50); }
  .oc.warn { color: var(--fs-warn); }
  .oc.error { color: var(--fs-danger); }
  .when { color: var(--fs-fg-secondary); font-variant-numeric: tabular-nums; }
  .tag { border: 1px solid var(--fs-border); border-radius: 3px; padding: 0 4px; font-size: 11px; }
  .code { color: var(--fs-fg-secondary); }
  .detail { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
</style>
