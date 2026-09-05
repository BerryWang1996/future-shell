<script lang="ts">
  import { invoke } from "../lib/ipc";
  import type { MonitorSnapshot } from "../lib/types";
  import {
    filterProcesses,
    formatPct,
    formatRss,
    killConfirmText,
    sortProcesses,
    type ProcessInfo,
    type SortDir,
    type SortKey,
  } from "../lib/processes";
  import {
    detectAnomalies,
    diagnosePrompt,
    worstSeverity,
    type Anomaly,
  } from "../lib/monitor-anomaly";
  import { confirmAction } from "../lib/confirm-gate"; // 危险动作统一确认（M7.2）
  import {
    ACTION_VERB, UPGRADE_HINT, confirmKindFor, filterUnits, packageEmptyHint,
    serviceEmptyHint, serviceLamp,
    type PackageScan, type ServiceAction, type ServiceListResult,
  } from "../lib/sysbox"; // 系统面工具箱（M7.1）
  import { toast } from "../lib/toast";

  /**
   * 系统监控面板（M4a）。两个页签：
   *
   * - **指标**：对活动会话每 2 秒轮询 `session_monitor`，展示主机名/运行时长/CPU%/
   *   负载/内存/根分区磁盘。CPU% 靠跨轮 /proc/stat 差量（后端持前值，S310），
   *   首轮为空态「—」。
   * - **进程**（服务器进程管理）：`session_processes` 列表 + 过滤 + 排序 + 终止。
   *
   * 两个页签的轮询节奏刻意不同：指标是几行 `echo`，2 秒一次无所谓；进程是
   * `ps -e` 全表，繁忙机器上几百行、还要走一次 exec 通道（每次新开 channel），
   * 2 秒一轮属于给别人的生产机加负载。故进程页 **5 秒**一轮，且**只在该页签
   * 可见时**才轮询——切回指标页立即停。另有手动「刷新」供需要即时确认的时刻。
   *
   * 采集失败不整屏报错：面板顶部一行小字提示，上一次成功的结果继续留在原地。
   * 监控的容错是「显示旧值 + 提示」，不是「闪一下没了」。
   */
  let {
    sessionId = null,
    /**
     * 「AI 诊断」（M4b 监控 × AI 联动）。把已判定的异常交给装配层去开 AI 面板。
     *
     * 传的是**已经拼好的提问**而不是异常列表：判定与措辞都属于本模块的职责
     * （阈值依据、要求只读命令、不锚定具体命令名，见 monitor-anomaly.ts），
     * 而装配层只负责把它送进 AI 面板。让 App 自己拼提问会让措辞散在两处。
     *
     * 无回调 = 按钮不出现。本组件不知道有没有配模型源，也不该知道。
     */
    onDiagnose = null,
  }: {
    sessionId?: string | null;
    onDiagnose?: ((prompt: string) => void) | null;
  } = $props();

  let tab = $state<"metrics" | "procs" | "services" | "packages">("metrics");

  let snap = $state<MonitorSnapshot | null>(null);
  /**
   * 异常判定。**本地算**，不问模型（与危险命令的档位判定同一个原则）。
   *
   * $derived 而不是在 $effect 里算并存进 $state：判定是快照的纯函数，
   * 存一份就意味着「快照变了而判定还是旧的」这个状态是可表达的。
   */
  const anomalies = $derived<Anomaly[]>(snap ? detectAnomalies(snap) : []);
  const worst = $derived(worstSeverity(anomalies));
  /** 某个指标是否异常（模板里给对应的 .stat 挂 class）。 */
  const sevOf = (m: Anomaly["metric"]) => anomalies.find((a) => a.metric === m)?.severity;
  let err = $state("");

  let procs = $state<ProcessInfo[]>([]);
  let procErr = $state("");
  let procLoading = $state(false);
  let query = $state("");
  let sortKey = $state<SortKey>("cpu");
  let sortDir = $state<SortDir>("desc");
  let signal = $state("TERM");
  /** 待确认终止的目标；null = 无对话框 */
  /* 终止进程走统一确认闸（M7.2）：命令全文 + 按类别记忆「以后不再显示」+ 审计照写。
   * 此前是本组件自己挂一个 ConfirmDialog——那条路没有记忆、也不写审计。 */

  async function poll() {
    if (!sessionId) {
      snap = null;
      err = "";
      return;
    }
    try {
      snap = await invoke<MonitorSnapshot>("session_monitor", { sessionId });
      err = "";
    } catch (e) {
      err = String(e);
    }
  }

  async function loadProcs() {
    if (!sessionId) {
      procs = [];
      procErr = "";
      return;
    }
    procLoading = true;
    try {
      procs = await invoke<ProcessInfo[]>("session_processes", { sessionId });
      procErr = "";
    } catch (e) {
      // 后端已把「空表」与「失败」分开（procs::classify_empty），这里直接呈现原因。
      // 不清空 procs：上一次的列表比一片空白有用。
      procErr = String(e);
    } finally {
      procLoading = false;
    }
  }

  $effect(() => {
    void poll();
    const timer = setInterval(() => void poll(), 2000); // M4a 规格：2s
    return () => clearInterval(timer);
  });

  // 进程页轮询：依赖 tab 与 sessionId，切走即 clearInterval（见组件头说明）
  $effect(() => {
    if (tab !== "procs" || !sessionId) return;
    void loadProcs();
    const timer = setInterval(() => void loadProcs(), 5000);
    return () => clearInterval(timer);
  });

  const shown = $derived(sortProcesses(filterProcesses(procs, query), sortKey, sortDir));

  function toggleSort(k: SortKey) {
    if (sortKey === k) {
      sortDir = sortDir === "asc" ? "desc" : "asc";
    } else {
      sortKey = k;
      // 换列时给一个「有用」的默认方向：占用类看大的，标识类看小的
      sortDir = k === "cpu" || k === "mem" || k === "rss_kb" ? "desc" : "asc";
    }
  }

  /**
   * 终止一个进程。确认在闸里（含 KILL / PID 1 的后果说明），本函数只负责发信号与刷新。
   *
   * `signal` 进 `kind` 吗？不进——「终止远端进程」是一个类别，TERM 与 KILL 是它的两种强度。
   * 拆成两类会让用户在设置页看到两条几乎一样的豁免，而他要放行的本来就是「终止进程」这件事；
   * 强度差异由确认框正文里的后果说明承担（KILL 那句「无法被捕获」）。
   */
  async function askKill(p: ProcessInfo): Promise<void> {
    const ok = await confirmAction({
      kind: "process.kill",
      title: "终止进程",
      command: `kill -${signal} ${p.pid}`,
      note: killConfirmText(p, signal),
      danger: signal === "KILL" || p.pid === 1,
      sessionId,
      confirmText: `发送 ${signal}`,
    });
    if (ok) await doKill(p);
  }

  async function doKill(p: ProcessInfo) {
    try {
      await invoke("session_kill_process", { sessionId, pid: p.pid, signal });
      toast.info(`已向 PID ${p.pid} 发送 ${signal}`);
      // 立即重取一次：信号是异步生效的，进程可能还在（正在收尾）也可能已经没了，
      // 两种都要如实反映，而不是乐观地把行从表里抹掉。
      await loadProcs();
    } catch (e) {
      toast.error(String(e));
    }
  }

  /* ── 系统面工具箱（M7.1）──────────────────────────────────────────────
   * 服务与补丁都**按需加载**（切到页签才拉），不进 2 秒轮询：它们变化很慢，
   * 而每两秒对远端跑一次 `systemctl list-units` 是自造的负载。 */
  let services = $state<ServiceListResult | null>(null);
  let servicesErr = $state("");
  let servicesBusy = $state(false);
  let svcFilter = $state("");
  let pkgs = $state<PackageScan | null>(null);
  let pkgsErr = $state("");
  let pkgsBusy = $state(false);
  /** 展开中的单元 → 它的日志原文（null = 正在取）。 */
  let journal = $state<{ unit: string; text: string | null } | null>(null);

  async function loadServices(): Promise<void> {
    if (!sessionId) return;
    servicesBusy = true;
    servicesErr = "";
    try {
      services = await invoke<ServiceListResult>("session_services", { sessionId });
    } catch (e) {
      servicesErr = String(e);
    } finally {
      servicesBusy = false;
    }
  }

  async function loadPkgs(): Promise<void> {
    if (!sessionId) return;
    pkgsBusy = true;
    pkgsErr = "";
    try {
      pkgs = await invoke<PackageScan>("session_packages", { sessionId });
    } catch (e) {
      pkgsErr = String(e);
    } finally {
      pkgsBusy = false;
    }
  }

  /**
   * 展开一个单元：先 `systemctl status`（它带着「为什么是这个状态」的那几行），再接 journal。
   *
   * 两条一起取而不是分两个按钮：用户点开一个失败的服务时想知道的是同一件事——「它怎么了」。
   * status 给的是当前态与最近一次退出码，journal 给的是过程。少任何一半都要再点一次。
   */
  async function showDetail(unit: string): Promise<void> {
    if (journal?.unit === unit) { journal = null; return; } // 再点一次收起
    journal = { unit, text: null };
    try {
      const status = await invoke<string>("session_service_status", { sessionId, unit });
      const log = await invoke<string>("session_service_journal", { sessionId, unit, lines: 200 });
      const text = [status.trim(), log.trim()].filter(Boolean).join("\n\n──── 最近日志 ────\n\n");
      // 取回来时用户可能已经点了别的单元——只写回还在展开的那一个
      if (journal?.unit === unit) journal = { unit, text: text || "（这个单元既没有状态也没有日志）" };
    } catch (e) {
      if (journal?.unit === unit) journal = { unit, text: `取状态/日志失败：${e}` };
    }
  }

  /**
   * 启 / 停 / 重启。停止与重启走 M7.2 的确认闸（命令全文 + 按类别记忆 + 审计），
   * 启动不走——它不打断任何在用的连接，为它弹框只会训练用户闭眼点确认。
   */
  async function doServiceAction(unit: string, action: ServiceAction): Promise<void> {
    const kind = confirmKindFor(action);
    const cmd = `systemctl ${action} ${unit}`;
    if (kind) {
      const ok = await confirmAction({
        kind,
        title: `${ACTION_VERB[action]}服务`,
        command: cmd,
        note: action === "stop"
          ? "停止后该服务不再提供任何响应，直到有人重新启动它。"
          : "重启期间该服务短暂不可用；若配置有误，它可能起不来。",
        danger: true,
        sessionId,
        confirmText: ACTION_VERB[action],
      });
      if (!ok) return;
    }
    try {
      const r = await invoke<{ ok: boolean; message: string }>("session_service_action", {
        sessionId, unit, action,
      });
      if (r.ok) toast.info(`已${ACTION_VERB[action]} ${unit}`);
      else toast.error(r.message);
    } catch (e) {
      toast.error(String(e));
    }
    await loadServices();
  }

  /** 负载/内存等「有总才有占比」的辅助：总为 0 或缺省时退 0，绝不除零。 */
  function pct(used: number | null | undefined, total: number | null | undefined): number {
    if (!used || !total) return 0;
    return Math.min(100, Math.round((used / total) * 100));
  }
  function fmtNum(n: number | null | undefined): string {
    return n == null ? "—" : String(n);
  }
  const arrow = (k: SortKey): string => (sortKey === k ? (sortDir === "asc" ? " ▲" : " ▼") : "");
  const shownUnits = $derived(
    services?.kind === "units" ? filterUnits(services.units, svcFilter) : [],
  );
  /** 切到页签时按需拉一次（只在还没拉过时——切来切去不该每次都打远端）。 */
  function onTab(next: typeof tab) {
    tab = next;
    if (next === "services" && services === null && !servicesBusy) void loadServices();
    if (next === "packages" && pkgs === null && !pkgsBusy) void loadPkgs();
  }
</script>

<div class="monitor-panel" data-testid="monitor-panel">
  <nav class="tabs" aria-label="监控页签">
    <button class:active={tab === "metrics"} data-testid="mon-tab-metrics" onclick={() => onTab("metrics")}>指标</button>
    <button class:active={tab === "procs"} data-testid="mon-tab-procs" onclick={() => onTab("procs")}>进程</button>
    <button class:active={tab === "services"} data-testid="mon-tab-services" onclick={() => onTab("services")}>服务</button>
    <button class:active={tab === "packages"} data-testid="mon-tab-packages" onclick={() => onTab("packages")}>补丁</button>
  </nav>

  {#if !sessionId}
    <p class="empty">无活动会话</p>
  {:else if tab === "metrics"}
    {#if err}<p class="err" role="alert">采集失败：{err}</p>{/if}
    {#if snap}
      <div class="stats">
        <div class="stat" title="主机名">
          <span class="k">主机</span><span class="v">{snap.hostname || "—"}</span>
        </div>
        <div class="stat" title="运行时长">
          <span class="k">运行</span><span class="v">{snap.uptime || "—"}</span>
        </div>
        <div class="stat" class:warn={sevOf("cpu") === "warn"} class:critical={sevOf("cpu") === "critical"}
             data-testid="mon-stat-cpu" title="CPU 占用率（2s 采样差量；首轮无前值、非 Linux 显示「—」）">
          <span class="k">CPU</span>
          <span class="v">
            {snap.cpu_percent == null ? "—" : `${snap.cpu_percent.toFixed(1)}%`}
            {#if snap.cpu_percent != null}<span class="bar"><i style="width: {Math.round(snap.cpu_percent)}%"></i></span>{/if}
          </span>
        </div>
        <div class="stat" class:warn={sevOf("load") === "warn"} class:critical={sevOf("load") === "critical"}
             data-testid="mon-stat-load"
             title={snap.cpu_cores == null ? "负载（1/5/15 分钟）——核数采不到，本项不做异常判定" : `负载（1/5/15 分钟），${snap.cpu_cores} 核`}>
          <span class="k">负载</span>
          <span class="v">{fmtNum(snap.load_1)} / {fmtNum(snap.load_5)} / {fmtNum(snap.load_15)}</span>
        </div>
        <div class="stat" class:warn={sevOf("mem") === "warn"} class:critical={sevOf("mem") === "critical"}
             data-testid="mon-stat-mem" title="内存（已用/总，MB）">
          <span class="k">内存</span>
          <span class="v">
            {fmtNum(snap.mem_used_mb)} / {fmtNum(snap.mem_total_mb)} MB
            <span class="bar"><i style="width: {pct(snap.mem_used_mb, snap.mem_total_mb)}%"></i></span>
          </span>
        </div>
        <div class="stat" class:warn={sevOf("disk") === "warn"} class:critical={sevOf("disk") === "critical"}
             data-testid="mon-stat-disk" title="根分区磁盘（已用/总）">
          <span class="k">磁盘 /</span>
          <span class="v">{snap.disk_used || "—"} / {snap.disk_total || "—"}</span>
        </div>
      </div>

      <!-- 异常摘要 + 一键 AI 诊断（M4b「监控 × AI 联动」）。
           只在真有异常时出现：一条常驻的「一切正常」横条只占地方，
           而一个常驻的「AI 诊断」按钮会让人在没有问题的时候也去点它。 -->
      {#if anomalies.length > 0}
        <div class="anomalies" class:critical={worst === "critical"} data-testid="mon-anomalies">
          <ul>
            {#each anomalies as a (a.metric)}
              <li class={a.severity} data-testid={`mon-anomaly-${a.metric}`}>{a.detail}</li>
            {/each}
          </ul>
          {#if onDiagnose}
            <!-- 「AI 诊断」而不是「AI 修复」：产出是一条只读命令，用来定位原因。
                 它和别处一样要过 fs_policy 闸门——按钮上的措辞不是安全保证，
                 闸门才是；措辞的作用是不让用户以为点一下机器就会被改。 -->
            <button
              class="diagnose"
              data-testid="mon-diagnose"
              title="把上面这些指标交给 AI，让它给一条只读命令来定位原因（要过同一个策略闸门）"
              onclick={() => onDiagnose?.(diagnosePrompt(snap?.hostname ?? "", anomalies))}
            >AI 诊断 ✨</button>
          {/if}
        </div>
      {/if}
    {:else if !err}
      <p class="empty">采集中…</p>
    {/if}
  {:else if tab === "procs"}
    <div class="proc-bar">
      <input
        type="search" placeholder="过滤 PID / 用户 / 命令" bind:value={query}
        data-testid="proc-filter" aria-label="过滤进程"
      />
      <select bind:value={signal} data-testid="proc-signal" aria-label="终止信号" title="发送的信号">
        <option value="TERM">TERM（优雅退出）</option>
        <option value="HUP">HUP（重载配置）</option>
        <option value="INT">INT（相当于 Ctrl-C）</option>
        <option value="KILL">KILL（强杀，会丢数据）</option>
      </select>
      <button data-testid="proc-refresh" onclick={() => void loadProcs()} disabled={procLoading}>
        {procLoading ? "读取中…" : "刷新"}
      </button>
      <span class="count" data-testid="proc-count">{shown.length} / {procs.length}</span>
    </div>
    {#if procErr}<p class="err" role="alert" data-testid="proc-error">{procErr}</p>{/if}
    {#if procs.length === 0 && !procErr}
      <p class="empty">{procLoading ? "读取中…" : "无数据"}</p>
    {:else}
      <table data-testid="proc-table">
        <thead>
          <tr>
            <th><button onclick={() => toggleSort("pid")} data-testid="proc-th-pid">PID{arrow("pid")}</button></th>
            <th><button onclick={() => toggleSort("user")}>用户{arrow("user")}</button></th>
            <th><button onclick={() => toggleSort("cpu")} data-testid="proc-th-cpu">CPU{arrow("cpu")}</button></th>
            <th><button onclick={() => toggleSort("mem")}>内存{arrow("mem")}</button></th>
            <th><button onclick={() => toggleSort("rss_kb")}>RSS{arrow("rss_kb")}</button></th>
            <th>状态</th>
            <th><button onclick={() => toggleSort("command")}>命令{arrow("command")}</button></th>
            <th></th>
          </tr>
        </thead>
        <tbody>
          {#each shown as p (p.pid)}
            <tr>
              <td class="num">{p.pid}</td>
              <td>{p.user}</td>
              <td class="num">{formatPct(p.cpu)}</td>
              <td class="num">{formatPct(p.mem)}</td>
              <td class="num">{formatRss(p.rss_kb)}</td>
              <td>{p.state}</td>
              <td class="cmd" title={p.command}>{p.command}</td>
              <td>
                <button
                  class="kill" data-testid="proc-kill-{p.pid}"
                  onclick={() => void askKill(p)}
                  title="向该进程发送所选信号"
                >终止</button>
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    {/if}
  {:else if tab === "services"}
    <!-- 服务管理（M7.1）：列表 / 状态 / 启停重启 / 看日志。停止与重启走 M7.2 确认闸。 -->
    <div class="proc-bar">
      <input placeholder="筛选服务名或描述" bind:value={svcFilter} data-testid="svc-filter" />
      <button onclick={() => void loadServices()} data-testid="svc-refresh" disabled={servicesBusy}>刷新</button>
      <span class="count">{servicesBusy ? "读取中…" : `${shownUnits.length} 个`}</span>
    </div>
    {#if servicesErr}<p class="err" role="alert" data-testid="svc-error">{servicesErr}</p>{/if}
    {#if serviceEmptyHint(services, shownUnits.length)}
      <p class="placeholder" data-testid="svc-empty">{serviceEmptyHint(services, shownUnits.length)}</p>
    {/if}
    {#if shownUnits.length > 0}
      <table data-testid="svc-table">
        <thead>
          <tr><th>状态</th><th>单元</th><th>描述</th><th></th></tr>
        </thead>
        <tbody>
          {#each shownUnits as u (u.name)}
            <tr>
              <td>
                <span class="dot {serviceLamp(u.active)}" aria-hidden="true"></span>
                <span class="svc-state">{u.active}/{u.sub}</span>
              </td>
              <td class="svc-name" title={u.name}>{u.name}</td>
              <td class="cmd" title={u.description}>{u.description}</td>
              <td class="svc-actions">
                <button data-testid={`svc-start-${u.name}`} onclick={() => void doServiceAction(u.name, "start")}>启动</button>
                <button data-testid={`svc-restart-${u.name}`} onclick={() => void doServiceAction(u.name, "restart")}>重启</button>
                <button class="kill" data-testid={`svc-stop-${u.name}`} onclick={() => void doServiceAction(u.name, "stop")}>停止</button>
                <button data-testid={`svc-log-${u.name}`} onclick={() => void showDetail(u.name)}>状态/日志</button>
              </td>
            </tr>
            {#if journal?.unit === u.name}
              <tr><td colspan="4">
                <pre class="journal" data-testid="svc-journal">{journal.text ?? "读取中…"}</pre>
              </td></tr>
            {/if}
          {/each}
        </tbody>
      </table>
    {/if}
  {:else if tab === "packages"}
    <!-- 补丁盘点（M7.1）：只读。升级命令只显示，由用户自己送进终端按回车。 -->
    <div class="proc-bar">
      <button onclick={() => void loadPkgs()} data-testid="pkg-refresh" disabled={pkgsBusy}>刷新</button>
      <span class="count">
        {pkgsBusy ? "读取中…" : pkgs?.kind === "updates" ? `${pkgs.updates.length} 个可升级` : ""}
      </span>
    </div>
    {#if pkgsErr}<p class="err" role="alert" data-testid="pkg-error">{pkgsErr}</p>{/if}
    {#if packageEmptyHint(pkgs)}
      <p class="placeholder" data-testid="pkg-empty">{packageEmptyHint(pkgs)}</p>
    {/if}
    {#if pkgs?.kind === "updates" && pkgs.updates.length > 0}
      <p class="sync-note" data-testid="pkg-hint">
        请在终端执行以下命令完成升级；如需 sudo 密码，请在终端中输入。
        <code>{UPGRADE_HINT[pkgs.manager]}</code>
      </p>
      <table data-testid="pkg-table">
        <thead><tr><th>包</th><th>当前</th><th>可升级到</th></tr></thead>
        <tbody>
          {#each pkgs.updates as p (p.name)}
            <tr>
              <td class="svc-name">{p.name}</td>
              <td>{p.current ?? "—"}</td>
              <td>{p.candidate}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    {/if}
  {/if}
</div>

<!-- 确认框不在本组件里：终止进程走 lib/confirm-gate.ts 的统一闸（M7.2），
     对话框是挂在 App 顶层的单实例 ActionConfirmDialog。 -->

<style>
  .monitor-panel { height: 100%; overflow-y: auto; padding: 6px 10px; font-size: 12px; }
  .tabs { display: flex; gap: 4px; margin-bottom: 6px; }
  .tabs button { padding: 2px 10px; border: 1px solid var(--fs-border); background: var(--fs-bg-panel); color: var(--fs-fg-secondary); border-radius: 4px; cursor: pointer; font-size: 11px; }
  .tabs button.active { background: var(--fs-bg-elevated); color: var(--fs-fg-primary); border-color: var(--fs-border-strong); }
  .empty { color: var(--fs-fg-secondary); margin: 4px 0; }
  .err { color: var(--fs-danger); margin: 0 0 4px; white-space: pre-wrap; }
  .stats { display: flex; flex-wrap: wrap; gap: 8px 18px; }
  .stat { display: flex; flex-direction: column; gap: 1px; }
  /* ── 异常态（M4b 监控 × AI 联动）─────────────────────────────────────── */
  /* 左边一道竖条 + 文字变色。**不用背景色填满**：这些格子里是数字，
     填色底会压低对比度，而这几个数字恰好是此刻最需要看清的。
     两档用不同颜色而不是同色深浅——深浅在暗色主题上区分不出来。 */
  .stat.warn { border-left: 2px solid var(--fs-warn, #d89614); padding-left: 6px; }
  .stat.critical { border-left: 2px solid var(--fs-danger, #d32029); padding-left: 6px; }
  .stat.warn .v { color: var(--fs-warn, #d89614); }
  .stat.critical .v { color: var(--fs-danger, #d32029); }
  .anomalies {
    margin-top: 8px;
    padding: 8px;
    border: 1px solid var(--fs-warn, #d89614);
    border-radius: var(--fs-radius);
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 8px;
  }
  .anomalies.critical { border-color: var(--fs-danger, #d32029); }
  .anomalies ul { margin: 0; padding-left: 16px; font-size: 12px; }
  .anomalies li.warn { color: var(--fs-warn, #d89614); }
  .anomalies li.critical { color: var(--fs-danger, #d32029); }
  .diagnose {
    flex: none;
    padding: 4px 10px;
    border: 1px solid var(--fs-border);
    background: var(--fs-bg-input);
    color: var(--fs-fg-primary);
    border-radius: var(--fs-radius);
    cursor: pointer;
    font: inherit;
    font-size: 12px;
  }
  .diagnose:hover { background: var(--fs-bg-hover); }
  .k { color: var(--fs-fg-secondary); font-size: 11px; }
  .v { color: var(--fs-fg-primary); display: inline-flex; align-items: center; gap: 6px; }
  .bar { display: inline-block; width: 48px; height: 6px; background: var(--fs-bg-panel); border-radius: 3px; overflow: hidden; }
  .bar i { display: block; height: 100%; background: var(--fs-accent); }
  .proc-bar { display: flex; align-items: center; gap: 6px; margin-bottom: 4px; }
  .proc-bar input { flex: 1; min-width: 90px; background: var(--fs-bg-input); color: var(--fs-fg-primary); border: 1px solid var(--fs-border); border-radius: 4px; padding: 2px 6px; font: inherit; }
  .proc-bar select, .proc-bar button { background: var(--fs-bg-panel); color: var(--fs-fg-primary); border: 1px solid var(--fs-border); border-radius: 4px; padding: 2px 6px; font: inherit; cursor: pointer; }
  /* select 单独补右内距给箭头让位；button 不该跟着空出一块（2026-09-01） */
  .proc-bar select { padding-right: 22px; }
  .count { color: var(--fs-fg-secondary); font-size: 11px; white-space: nowrap; }
  table { width: 100%; border-collapse: collapse; }
  th, td { text-align: left; padding: 1px 6px 1px 0; white-space: nowrap; }
  th { position: sticky; top: 0; background: var(--fs-bg-panel); }
  th button { background: none; border: 0; color: var(--fs-fg-secondary); font: inherit; cursor: pointer; padding: 0; }
  td.num { text-align: right; font-variant-numeric: tabular-nums; }
  td.cmd { max-width: 32vw; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  /* 系统面工具箱（M7.1） */
  .svc-name { font-family: var(--fs-font-mono, monospace); }
  .svc-state { color: var(--fs-fg-secondary); font-size: 11px; margin-left: 4px; }
  .svc-actions { display: flex; gap: 4px; white-space: nowrap; }
  .svc-actions button { background: none; border: 1px solid var(--fs-border); color: var(--fs-fg-primary); border-radius: 3px; padding: 0 6px; cursor: pointer; font-size: 11px; }
  .svc-actions button:hover { background: var(--fs-bg-hover); }
  .journal { margin: 0; padding: 6px 8px; max-height: 220px; overflow-y: auto; background: var(--fs-bg-app); border: 1px solid var(--fs-border); border-radius: var(--fs-radius); font-family: var(--fs-font-mono, monospace); font-size: 11.5px; white-space: pre-wrap; overflow-wrap: anywhere; }
  .dot.off { background: var(--fs-fg-disabled); }
  tbody tr:hover { background: var(--fs-bg-hover); }
  button.kill { background: none; border: 1px solid var(--fs-border); color: var(--fs-danger); border-radius: 4px; padding: 0 6px; cursor: pointer; font-size: 11px; }
</style>
