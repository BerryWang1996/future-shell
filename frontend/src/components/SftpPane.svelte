<script lang="ts">
  import { useFocusTrap } from "../lib/focusTrap";
  import { invoke } from "../lib/ipc";
  import { toast } from "../lib/toast"; // 粘贴/脚本的结果走 toast：它们是「刚才那一下做了什么」，不是 pane 的持续状态
  import PasteConflictDialog from "./PasteConflictDialog.svelte";
  import ScriptRunDialog from "./ScriptRunDialog.svelte";
  import {
    clearClip, clipSummary, fileClip, looksLikeScript, pasteBlockedReason, setClip, summarizePaste,
    type ConflictPolicy, type PastePlan, type PasteResult,
  } from "../lib/file-clipboard"; // 远端剪切/复制/粘贴（M7.3）
  import FilePropsDialog from "./FilePropsDialog.svelte";
  import ConfirmDialog from "./ConfirmDialog.svelte";
  import { validateSymlink, type EntryDetail } from "../lib/fileprops";
  import {
    POLL_MS,
    conflictMessage,
    goneMessage,
    needsConfirm,
    shouldAutoUpload,
    type EditDecision,
  } from "../lib/editor";
  let {
    sessionId,
    localDir = null,
    remoteDir = null,
    /** 前台运行远端脚本：交给 App 开一个新终端标签并把命令送进去（M7.3 出口标准②）。 */
    onRunInNewTab = (_cmd: string) => {},
    /** 本面板此刻是不是已经装在一个虚拟窗口里（装着就不再显示「浮动窗口」按钮——
     *  再点一次只会把同一个窗口置顶，那个按钮在窗口里是个纯噪音）。 */
    floating = false,
    /** 请 App 把文件视图弹成一个虚拟窗口（M7.3 出口标准③）。 */
    onFloat = null,
  }: {
    sessionId: string;
    localDir?: string | null;
    remoteDir?: string | null;
    onRunInNewTab?(command: string): void;
    floating?: boolean;
    onFloat?: (() => void) | null;
  } = $props();
  // 审计2 #18：档案 SFTP 默认目录在此生效（旧版是死配置——可编辑可保存，面板却恒从
  // 本地家目录与远端 `.` 起步）。档案列表在启动时异步加载、可能晚于面板挂载，故 prop
  // 到达后仍要跟随；用户手动导航一次后以用户为准，不再回拽。
  // 初始值不含 prop：直接引用会只捕获挂载时的那次取值（编译器 state_referenced_locally
  // 警告），晚到的档案值交给下方 dirs-follow $effect 统一处理——它声明在 refresh 的
  // $effect 之前，同一次 flush 内先把路径拨正、refresh 拿到的就是档案目录。
  let localPath = $state(homeDir());
  let remotePath = $state(".");
  let dirsTaken = $state({ local: false, remote: false });
  let localEntries = $state<any[]>([]);
  let remoteEntries = $state<any[]>([]);
  // 审计2 #38：后端列表带条目上限（引擎 LIST_ENTRY_CAP），截断必须如实呈现——
  // 「列表不完整」不得伪装成「目录里就这么多内容」（找文件找不到时用户要知道
  // 该去更深的子目录，而不是以为文件不存在）。
  let localTruncated = $state(false);
  let remoteTruncated = $state(false);
  // UI 规格 §1.4 的 M1 承诺增量（Round 6 i=25 回灌）：工具行/面包屑/多选/隐藏文件/排序/拖拽传输/错误与空态/分隔条比例记忆
  let localStack = $state<string[]>([]);              // 返回栈（导航前压入旧路径）
  let remoteStack = $state<string[]>([]);
  let selectedLocal = $state(new Set<string>());      // Ctrl 加选 / Shift 区间 / 单击替换
  let selectedRemote = $state(new Set<string>());
  let lastClicked: { local?: string; remote?: string } = {};
  let showHidden = $state(false);                     // settings 键 sftp.showHiddenFiles（默认 false，经 Task 20 SettingsRepo）
  let sortKey = $state<"name" | "size" | "mtime">("name");
  let sortAsc = $state(true);
  // 视图模式与图标大小（Windows 资源管理器式的「查看」切换）：details = 列式详情，icons = 图标网格
  let viewMode = $state<"details" | "icons">("details");
  let iconSize = $state<"small" | "large">("large");
  let loading = $state(false);
  let paneError = $state("");                         // pane 级错误条（口径同 ProfileDialog 的 formError/.err）
  let splitRatio = $state(Number(localStorage.getItem("sftp.splitRatio")) || 50); // 分隔条比例记忆（UI 规格 §1.4）
  let containerEl: HTMLElement | undefined = $state();

  // ── M4a 排除过滤器：一个串，同时管两栏的呈现（后端 fs_sshengine::filter 是唯一匹配器）
  let excludeText = $state(localStorage.getItem("sftp.exclude") ?? "");
  /** 空串一律传 undefined：让后端走「无过滤器」快路，也避免把 "" 当成一条模式。 */
  const excludeArg = $derived(excludeText.trim() === "" ? undefined : excludeText);

  // ── M4a 同步浏览（Xftp「同步浏览」对标）：一侧进入子目录时另一侧进同名子目录。
  //
  // 语义刻意保守——只同步**用户主动的导航动作**（双击进目录 / 上级 / 面包屑 / 返回），
  // 且只在两侧当前目录「看起来对得上」时同步：另一侧没有同名子目录就**不动**并提示，
  // 而不是把它导航到一个不存在的路径然后弹「列出失败」。独立开关，默认关。
  let syncBrowse = $state(localStorage.getItem("sftp.syncBrowse") === "1");
  let syncNote = $state(""); // 同步未生效时的就地提示（不是错误，不占 paneError）

  /* ── 远端剪切/复制/粘贴 + 脚本运行（M7.3）──────────────────────────────
   * 范围裁定「先只做同主机远端↔远端」：跨会话粘贴的按钮是**禁用**的并说清为什么，
   * 而不是可点然后报一句失败（见 lib/file-clipboard.ts 的模块头注）。 */
  let pasteConflict = $state<{ names: string[]; srcs: string[]; cut: boolean } | null>(null);
  let scriptRun = $state<{ path: string } | null>(null);
  const clipBlocked = $derived(pasteBlockedReason($fileClip, sessionId));

  function remotePathOf(name: string): string {
    return joinPath(remotePath, name);
  }

  function cutOrCopySelected(cut: boolean) {
    const names = [...selectedRemote];
    if (names.length === 0) {
      toast.warn("先选中要操作的远端条目");
      return;
    }
    setClip(sessionId, names.map(remotePathOf), cut);
    toast.info(`${cut ? "已剪切" : "已复制"} ${names.length} 项，切到目标目录后点「粘贴」`);
  }

  /** 执行一份已经算好的计划，并把逐条结果说清楚。 */
  async function runPlan(plan: PastePlan, cut: boolean) {
    try {
      const r = await invoke<PasteResult>("fileops_paste", { sessionId, plan });
      const { level, lines } = summarizePaste(r);
      toast[level](lines.join("；"));
      // 剪切成功之后源已经不在了，剪贴板必须清——留着会让用户再粘一次而全部失败
      if (cut && r.done.some((d) => d.ok)) clearClip();
      await refresh();
    } catch (e) {
      toast.error(`粘贴失败：${e}`);
    }
  }

  async function paste() {
    const clip = $fileClip;
    if (!clip || clipBlocked) {
      if (clipBlocked) toast.warn(clipBlocked);
      return;
    }
    try {
      // 先用 skip 探一次：它不改任何东西，而 skipped 恰好就是冲突的名字。
      const probe = await invoke<PastePlan>("fileops_plan", {
        sessionId, srcs: clip.paths, dstDir: remotePath, policy: "skip", cut: clip.cut,
      });
      if (probe.skipped.length > 0) {
        pasteConflict = { names: probe.skipped, srcs: clip.paths, cut: clip.cut };
        return;
      }
      await runPlan(probe, clip.cut);
    } catch (e) {
      toast.error(`粘贴失败：${e}`);
    }
  }

  async function pasteWithPolicy(policy: ConflictPolicy) {
    const c = pasteConflict;
    pasteConflict = null;
    if (!c) return;
    try {
      const plan = await invoke<PastePlan>("fileops_plan", {
        sessionId, srcs: c.srcs, dstDir: remotePath, policy, cut: c.cut,
      });
      await runPlan(plan, c.cut);
    } catch (e) {
      toast.error(`粘贴失败：${e}`);
    }
  }

  /** 远端双击：目录进去；`.sh` 走「先看原文再选前台/后台」；其余按既有行为（无动作）。 */
  function remoteDblClick(e: { name: string; is_dir: boolean }) {
    if (e.is_dir) { enterDir("remote", e.name); return; }
    if (looksLikeScript(e.name)) scriptRun = { path: remotePathOf(e.name) };
  }

  /** 用户在 ScriptRunDialog 里选定之后。审计两条路都写（脚本 = 双击即以远端身份执行任意代码）。 */
  async function runScript(mode: "foreground" | "background") {
    const target = scriptRun;
    scriptRun = null;
    if (!target) return;
    try {
      if (mode === "background") {
        const cmd = await invoke<string>("fileops_run_script_background", { sessionId, path: target.path });
        void invoke("audit_dangerous_action", {
          kind: "script.run", command: cmd, sessionId, approved: true, auto: false,
        }).catch(() => {});
        toast.warn(`已在后台启动：${target.path}。关掉本程序它仍在跑；输出写在远端当前目录的 nohup.out。`);
      } else {
        const cmd = await invoke<string>("fileops_foreground_command", { path: target.path });
        void invoke("audit_dangerous_action", {
          kind: "script.run", command: cmd, sessionId, approved: true, auto: false,
        }).catch(() => {});
        onRunInNewTab(cmd);
      }
    } catch (e) {
      toast.error(`运行失败：${e}`);
    }
  }

  function homeDir(): string { return ""; /* 经 local_list 的默认值由 Rust 侧 dirs::home_dir 提供 */ }
  function joinPath(a: string, b: string): string {
    if (!a) return b;
    return a.replace(/[\\/]+$/, "") + "/" + b; // 去尾分隔符，避免双斜杠
  }
  function crumbs(p: string): string[] { return p.split(/[\\/]/).filter(Boolean); }
  function crumbPath(p: string, i: number): string {
    const joined = crumbs(p).slice(0, i + 1).join("/");
    return p.startsWith("/") ? "/" + joined : joined; // 绝对路径补首斜杠；盘符（C:）直接拼接即可
  }
  function upOf(p: string): string {
    const t = p.replace(/[\\/]+$/, "");
    const i = t.lastIndexOf("/");
    if (i <= 0) return t.startsWith("/") ? "/" : (t === "." ? "." : ""); // 本地根（家目录，空串）/ 远程根（"."）不再上行
    return t.slice(0, i);
  }
  function navLocal(p: string) { localStack.push(localPath); localPath = p; dirsTaken.local = true; selectedLocal = new Set(); }
  function navRemote(p: string) { remoteStack.push(remotePath); remotePath = p; dirsTaken.remote = true; selectedRemote = new Set(); }
  function backLocal() { const p = localStack.pop(); if (p !== undefined) { localPath = p; selectedLocal = new Set(); } }
  function backRemote() { const p = remoteStack.pop(); if (p !== undefined) { remotePath = p; selectedRemote = new Set(); } }

  /**
   * M4a 同步浏览：一侧进入子目录 `name` 时，另一侧进同名子目录。
   *
   * 三条约束都是为了「不把用户导航到一个不存在的地方」：
   * ① 只在另一侧**当前列表里确实有**这个同名目录时才动。列表是刚刷出来的、且已按
   *    过滤器过滤——所以「过滤器隐藏掉的目录」也不会被同步进入（界面上看不见的东西
   *    不该成为导航目标）。
   * ② 对不上就**原地不动 + 就地提示**，不是错误：另一侧没有同名目录是常态
   *    （本地是 Windows 目录树、远端是 Linux），弹错误框会让人以为操作失败了。
   * ③ 上级同步只在两侧都还能上行时做——一侧已到根就不动它（否则「上一级」会在
   *    两栏之间产生持续错位）。
   */
  function syncEnter(side: "local" | "remote", name: string) {
    if (!syncBrowse) return;
    syncNote = "";
    if (side === "local") {
      const hit = remoteEntries.some((e) => e.is_dir && e.name === name);
      if (!hit) { syncNote = `远端当前目录下没有「${name}」，同步浏览未跟随`; return; }
      navRemote(joinPath(remotePath, name));
    } else {
      const hit = localEntries.some((e) => e.is_dir && e.name === name);
      if (!hit) { syncNote = `本地当前目录下没有「${name}」，同步浏览未跟随`; return; }
      navLocal(joinPath(localPath, name));
    }
  }

  /** 上级的同步：两侧都能上行才动（见 syncEnter 的第 ③ 条）。 */
  function syncUp(side: "local" | "remote") {
    if (!syncBrowse) return;
    syncNote = "";
    if (side === "local") {
      const up = upOf(remotePath);
      if (up === remotePath) { syncNote = "远端已在根目录，同步浏览未跟随"; return; }
      navRemote(up);
    } else {
      const up = upOf(localPath);
      if (up === localPath) { syncNote = "本地已在根目录，同步浏览未跟随"; return; }
      navLocal(up);
    }
  }

  /** 进入子目录的统一入口（双击/键盘 Enter 都走这里，同步逻辑因此只有一处）。 */
  function enterDir(side: "local" | "remote", name: string) {
    if (side === "local") navLocal(joinPath(localPath, name));
    else navRemote(joinPath(remotePath, name));
    syncEnter(side, name);
  }
  /** 上级的统一入口。 */
  function goUp(side: "local" | "remote") {
    if (side === "local") navLocal(upOf(localPath));
    else navRemote(upOf(remotePath));
    syncUp(side);
  }

  /**
   * 列两栏。
   *
   * 四个参数不是装饰：`$effect` 只跟踪**第一个 `await` 之前**同步读到的状态。原来的
   * `refresh()` 在第一个 await **之后**才读 `remotePath`/`sessionId`，于是那个
   * `$effect(() => void refresh())` 的依赖只有 `localPath` 与 `excludeArg`——
   * **远端换目录从来不会重新列表**：双击进入子目录/点上级/点面包屑之后路径变了、面包屑
   * 变了，列表却还是上一个目录的内容，用户看到的就是「点了没反应」。
   * （本地那栏一直是好的，因为 `localPath` 恰好读在 await 之前；档案目录晚到那条也一直
   * 是绿的，因为它同时改了两栏的路径——这就是它躲过既有测试的原因。）
   *
   * 把这四个值提到调用点当实参，依赖就落在同步段里。直接调用处（刷新按钮、各操作之后）
   * 不传参、默认取当前值，行为不变。
   */
  async function refresh(lp = localPath, rp = remotePath, ex = excludeArg, sid = sessionId) {
    loading = true; paneError = "";
    try {
      // 审计2 #38：载荷是 { entries, truncated } 对象（后端上限闸的如实上报面），
      // 泛型实参免 unknown→any[] 类型错（O11）
      const local = await invoke<{ entries: any[]; truncated: boolean }>("local_list", { path: lp, exclude: ex }); // 空路径 = Rust 侧默认用户目录（local_list 根约束）
      const remote = await invoke<{ entries: any[]; truncated: boolean }>("sftp_list", { sessionId: sid, path: rp, exclude: ex });
      localEntries = local.entries;
      remoteEntries = remote.entries;
      localTruncated = local.truncated;
      remoteTruncated = remote.truncated;
    } catch (e) {
      paneError = `列出失败：${e}`; // 权限拒绝/会话断开等显式呈现，禁止静默 unhandled rejection
    } finally {
      loading = false;
    }
  }
  $effect(() => {
    // 档案目录到达（异步加载/恢复会话）→ 用户尚未动过该栏时跟随；动过则以用户为准。
    if (!dirsTaken.local && localDir) localPath = localDir;
    if (!dirsTaken.remote && remoteDir) remotePath = remoteDir;
  });
  // 实参必须写全：少写一个，那一路的导航就又变成「点了没反应」（见 refresh 的文档）。
  $effect(() => { void refresh(localPath, remotePath, excludeArg, sessionId); });
  $effect(() => stopPolling); // 卸载即停轮询：对着已销毁的会话发 IPC 只会刷错误
  // 挂载时从后端**恢复**编辑列表。面板会随标签切换卸载/重挂，而编辑会话活在后端——
  // 不恢复的话那些文件的回传就此失联：用户在编辑器里存了盘，界面这边毫无反应，
  // 而后端还记着它们。这也是 editor_list 存在的理由。
  $effect(() => {
    void (async () => {
      try {
        const items = await invoke<{ session_id: string; remote: string }[]>("editor_list");
        const mine = items.filter((i) => i.session_id === sessionId).map((i) => i.remote);
        if (mine.length > 0) { editing = mine; startPolling(); }
      } catch { /* 拿不到就当没有：这是恢复，不是必须成功的一步 */ }
    })();
  });
  $effect(() => { localStorage.setItem("sftp.exclude", excludeText); });
  $effect(() => { localStorage.setItem("sftp.syncBrowse", syncBrowse ? "1" : "0"); });

  function toggleSort(k: "name" | "size" | "mtime") {
    if (sortKey === k) { sortAsc = !sortAsc; } else { sortKey = k; sortAsc = true; }
  }

  /** 人类可读尺寸：后端给的是裸字节数，直接印 `1024` 会让「文件显示」看起来像错了。 */
  function formatSize(n: number): string {
    if (!Number.isFinite(n) || n < 0) return "";
    if (n < 1024) return `${n} B`;
    const units = ["KB", "MB", "GB", "TB"];
    let v = n;
    let i = -1;
    do { v /= 1024; i += 1; } while (v >= 1024 && i < units.length - 1);
    return `${v >= 100 ? v.toFixed(0) : v.toFixed(1)} ${units[i]}`;
  }
  /** 修改时间：unix 秒 → `YYYY-MM-DD HH:mm`。 */
  function formatMtime(sec: number): string {
    if (!Number.isFinite(sec) || sec <= 0) return "";
    const d = new Date(sec * 1000);
    const p = (x: number) => String(x).padStart(2, "0");
    return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`;
  }

  /**
   * 传输引擎的临时件后缀（与 `crates/sshengine/src/transfer.rs` 的 `PART_SUFFIX` 逐字一致）。
   *
   * 由来：原子提交方案下，一切写入先落 `<目标名>.fspart`，全部字节到位后才 rename 覆盖最终目标
   * （同目录 rename 才具备原子性，故临时件就躺在用户正在浏览的这个目录里）。语义因此是
   * **「一次尚未完成的传输」**，而不是文件：
   * - 传输进行中 → 它正在被写，长度还在涨；
   * - 传输中断/失败 → 它留在原地，正是断点续传的进度凭据（引擎按其实际长度重算续传起点）。
   *
   * 所以两侧列表都不能把它当普通文件列出来。放任不管有两种具体的坏结果：
   * ① 用户当成垃圾文件删掉 —— 删的是别人正在写的目标，或者是自己那条大文件传输的全部进度；
   * ② 用户双击/点「下载」把它取回来 —— 拿到的是个残缺文件，且文件名带着 `.fspart` 后缀，
   *    看上去却和正常下载没有任何区别。
   * 这里把它们从主列表里摘出去，另折叠成一条「N 项未完成传输」，既不误导也不隐瞒。
   */
  const PART_SUFFIX = ".fspart";
  /** 目录不参与判定：`.fspart` 只可能是引擎创建的普通文件，同名目录属用户自造，不该被吞掉。 */
  function isPartial(e: any): boolean {
    return !e.is_dir && String(e.name).endsWith(PART_SUFFIX);
  }
  /** 摘出未完成传输项（保持后端返回的原序，此处只做筛选不排序：条目少且无排序诉求）。 */
  function partials(entries: any[]): any[] {
    return entries.filter(isPartial);
  }

  function view(entries: any[]): any[] {
    // 先摘临时件再过隐藏文件：`foo.bin.fspart` 不以 `.` 开头，「显示隐藏文件」开关对它无效，
    // 不能指望那个开关顺带把它挡住。
    const listed = entries.filter((e) => !isPartial(e));
    const visible = showHidden ? listed : listed.filter((e) => !e.name.startsWith("."));
    const dir = sortAsc ? 1 : -1;
    return [...visible].sort((a, b) =>
      Number(b.is_dir) - Number(a.is_dir) || // 目录优先：不随升降序翻转（在 Rust 侧目录优先序基础上叠加）
      (sortKey === "size"
        ? dir * Math.sign((a.size ?? 0) - (b.size ?? 0))
        : sortKey === "mtime"
          ? dir * Math.sign((a.mtime ?? 0) - (b.mtime ?? 0))
          : dir * String(a.name).localeCompare(String(b.name))));
  }
  let shownLocal = $derived(view(localEntries));
  let shownRemote = $derived(view(remoteEntries));
  // 未完成传输：与 shownLocal/shownRemote 互补（同一份 entries 的两个不相交子集），
  // 选择/拖拽/下载一律只认 shown*，故临时件天然不可被选中、不可被误传。
  let partialLocal = $derived(partials(localEntries));
  let partialRemote = $derived(partials(remoteEntries));

  // ev 放宽到 KeyboardEvent：两类事件都带 shiftKey/ctrlKey/metaKey，
  // 让空格键走与单击完全相同的选择分支，避免键鼠两套选择语义。
  function select(side: "local" | "remote", name: string, ev: MouseEvent | KeyboardEvent) {
    const set = side === "local" ? selectedLocal : selectedRemote;
    const order = (side === "local" ? shownLocal : shownRemote).map((x: any) => x.name as string);
    if (ev.shiftKey && lastClicked[side]) {
      const a = order.indexOf(lastClicked[side]!);
      const b = order.indexOf(name);
      if (a >= 0 && b >= 0) {
        for (const n of order.slice(Math.min(a, b), Math.max(a, b) + 1)) set.add(n); // Shift 区间选
      } else { set.add(name); }
    } else if (ev.ctrlKey || ev.metaKey) {
      if (set.has(name)) { set.delete(name); } else { set.add(name); } // Ctrl 加选/减选
    } else {
      set.clear();
      set.add(name); // 普通单击替换选择
    }
    lastClicked[side] = name;
    if (side === "local") { selectedLocal = new Set(set); } else { selectedRemote = new Set(set); } // 重新赋值触发响应
  }

  async function upload(name: string) {
    paneError = "";
    try {
      await invoke("transfer_submit", {
        sessionId, direction: "up",
        local: joinPath(localPath, name), remote: joinPath(remotePath, name), resumeOffset: 0,
      });
    } catch (e) { paneError = `上传提交失败：${e}`; }
  }
  async function download(name: string) {
    paneError = "";
    try {
      await invoke("transfer_submit", {
        sessionId, direction: "down",
        local: "", // 下载目的由 Rust 侧 per-profile 沙箱决定（spec §3.3），前端不可指定
        remote: joinPath(remotePath, name), resumeOffset: 0,
      });
    } catch (e) { paneError = `下载提交失败：${e}`; }
  }
  // 批量上传/下载 = 循环单文件 transfer_submit；多文件拖拽与「全部取消」随 M4a（路线图 §2.2 增量，见 Step 4 抽屉豁免注）
  // 「上传/下载选中」与拖拽走**同一条**入队路径：否则「拖进去能传目录、按按钮不能」
  // 这种差异会让用户以为目录传输时好时坏。
  async function uploadSelected() { await enqueueMany("local", [...selectedLocal]); }
  async function downloadSelected() { await enqueueMany("remote", [...selectedRemote]); }

  /**
   * 本地新建目录（M4b 第 15 项）。
   *
   * 本地栏**没有删除**，这个缺席是刻意的——评估结论见
   * 路线图 §6.3（`docs/roadmap.md`）：删除是三个操作里
   * 唯一不可逆的，而本地这一侧用户是在「用自己的电脑」，那上面有回收站、我们这里没有。
   */
  async function mkdirLocal() {
    const name = window.prompt("新建目录名");
    if (!name?.trim()) return;
    paneError = "";
    try {
      await invoke("local_mkdir", { parent: localPath, name: name.trim() });
      await refresh();
    } catch (e) { paneError = `新建目录失败：${e}`; }
  }

  /** 本地重命名（单项，同目录内）。目标已存在时后端拒绝——不问「要不要覆盖」。 */
  async function renameLocal() {
    const names = [...selectedLocal];
    if (names.length !== 1) { paneError = "重命名请仅选中一个本地项"; return; }
    const from = names[0];
    const to = window.prompt("重命名为", from);
    if (!to?.trim() || to.trim() === from) return;
    paneError = "";
    try {
      await invoke("local_rename", { parent: localPath, from, to: to.trim() });
      selectedLocal = new Set();
      await refresh();
    } catch (e) { paneError = `重命名失败：${e}`; }
  }

  async function mkdirRemote() {
    const name = window.prompt("新建目录名");
    if (!name || !name.trim()) return;
    paneError = "";
    try {
      await invoke("sftp_mkdir", { sessionId, path: joinPath(remotePath, name.trim()) });
      await refresh();
    } catch (e) { paneError = `新建目录失败：${e}`; }
  }
  async function removeSelected() {
    const names = [...selectedRemote];
    if (names.length === 0) return;
    const preview = names.slice(0, 3).join(", ") + (names.length > 3 ? ` 等 ${names.length} 项` : "");
    // 审计2 #17：目录删除走引擎守卫递归（连同内容删除）。确认文案必须把这个事实说清楚——
    // 「删除」对目录而言意味着「删除里面的一切」，藏着不说等于替用户按下一个他不知道的确认。
    const hasDir = names.some((n) => shownRemote.some((e) => e.name === n && e.is_dir));
    const text =
      `删除选中的远程项：${preview}？` +
      (hasDir ? "选中项含目录，目录将连同内容一并删除。" : "") +
      "此操作不可撤销。";
    if (!window.confirm(text)) return; // 破坏性操作二次确认
    paneError = "";
    for (const n of names) {
      try {
        await invoke("sftp_remove", { sessionId, path: joinPath(remotePath, n) });
      } catch (e) { paneError = `删除失败：${e}`; }
    }
    selectedRemote = new Set();
    await refresh();
  }
  async function renameSelected() {
    const names = [...selectedRemote];
    if (names.length !== 1) { paneError = "重命名请仅选中一个远程项"; return; }
    const from = names[0];
    const to = window.prompt("重命名为", from);
    if (!to || !to.trim() || to.trim() === from) return;
    paneError = "";
    try {
      await invoke("sftp_rename", { sessionId, from: joinPath(remotePath, from), to: joinPath(remotePath, to.trim()) });
      await refresh();
    } catch (e) { paneError = `重命名失败：${e}`; }
  }

  // ── M4a 属性对话框 / 新建软链 ────────────────────────────────────────────
  let propsOpen = $state(false);
  let propsDetail = $state<EntryDetail | null>(null);
  let propsErr = $state("");
  let symlinkOpen = $state(false);
  let symlinkBusy = $state(false);
  let symlinkError = $state("");
  let symlinkEl = $state<HTMLDivElement | null>(null);
  $effect(() => {
    if (symlinkOpen && symlinkEl) {
      symlinkError = "";
      return useFocusTrap(symlinkEl, { initial: symlinkEl.querySelector("input") });
    }
  });
  let symlinkName = $state("");
  let symlinkTarget = $state("");

  /**
   * 打开属性。两侧走各自的命令（远端 lstat 经 SFTP、本地 symlink_metadata），
   * 但**呈现同一个结构**——两套字段名迟早分叉，而分叉的那侧会把「未知」显示成 0。
   *
   * 失败不弹 toast 而是把错误显示在对话框里：用户点的是「这一项的属性」，
   * 错误就该出现在他打开的那扇窗上（例如「文件已被删除」）。
   */
  async function openProps(side: "local" | "remote", name: string) {
    propsDetail = null;
    propsErr = "";
    propsOpen = true;
    // 命令名写成**字面量**的两条分支，而不是拼一个变量传进 invoke：
    // 仓库的 IPC 契约守卫（lib/ipc-contract.test.ts）扫的是字面量，动态名会让
    // 「这条命令有没有接上界面」这个检查失效——它当场抓到了这一点。
    try {
      propsDetail =
        side === "remote"
          ? await invoke<EntryDetail>("sftp_stat_entry", { sessionId, path: joinPath(remotePath, name) })
          : await invoke<EntryDetail>("local_stat_entry", { path: joinPath(localPath, name) });
    } catch (e) {
      propsErr = `读取属性失败：${e}`;
    }
  }

  /**
   * 属性入口的**唯一**判据：恰好选中一项时才有「这一项」。
   *
   * 单源很要紧——最初这条规则写了两份（模板 `disabled={size !== 1}` 与本函数的
   * `size === 1`）。两份判据里第二份是不可观测的：按钮禁用挡住了点击，所以把本函数
   * 放宽成 `>= 1` 时全部测试照旧全绿（变异验证当场抓到）。现在 disabled 直接取
   * 下面两个 $derived，函数与界面共用同一个结论。
   */
  function propsTargetOf(sel: Set<string>): string | null {
    return sel.size === 1 ? [...sel][0] : null;
  }
  const remotePropsTarget = $derived(propsTargetOf(selectedRemote));
  const localPropsTarget = $derived(propsTargetOf(selectedLocal));

  /**
   * 新建软链：链名在**当前远端目录**下创建，目标原样下发（相对链是常态，不做解析）。
   *
   * 校验在 lib/fileprops.validateSymlink——链名不许带路径分隔符：面板的语义是
   * 「在**当前目录**建一个链」，允许 `a/b` 就等于允许往别处写，而那个「别处」
   * 用户在界面上看不见。
   */
  async function createSymlink(name: string, target: string) {
    if (symlinkBusy) return;
    const bad = validateSymlink(name, target);
    if (bad) { paneError = bad; return; }
    paneError = "";
    symlinkBusy = true;
    symlinkError = "";
    try {
      await invoke("sftp_symlink", {
        sessionId,
        target: target.trim(),
        linkPath: joinPath(remotePath, name.trim()),
      });
      symlinkOpen = false;
      await refresh();
    } catch (e) {
      // 服务端不支持（Windows OpenSSH 等）原样透传：替它编一句「不支持」会掩盖真实回复码
      symlinkError = `新建软链失败：${e}`;
    } finally { symlinkBusy = false; }
  }

  // ── 拖拽传输（M1 单文件基础 → M4a 多选 + 文件夹 + 入队中取消）
  //
  // 拖起来的那一项若在当前选中集里，就把**整个选中集**都传（资源管理器口径：
  // 选中 5 个再拖任意一个 = 传 5 个）；不在选中集里则只传它自己——那是「我就要这一个」。
  //
  // 目录展开在后端（sftp_walk / local_walk：截断/深度/条数三条上限拒绝而非截断，
  // 软链跳过并如实上报）。前端只负责建目录 + 逐个入队，并在过程中给一条可取消的进度。
  let enqueueing = $state<{ total: number; done: number; cancelled: boolean } | null>(null);

  /** 拖拽发起时决定实际要传的名字集合（见上方口径）。 */
  function dragNames(side: "local" | "remote", dragged: string): string[] {
    const sel = side === "local" ? selectedLocal : selectedRemote;
    return sel.has(dragged) ? [...sel] : [dragged];
  }

  /** 入队中取消：只停「还没提交的」，已提交的作业由传输抽屉各自取消（那是它的职责）。 */
  function cancelEnqueue() {
    if (enqueueing) enqueueing = { ...enqueueing, cancelled: true };
  }

  /**
   * 把一批名字（文件或目录）入队。目录走后端递归枚举，逐文件提交；
   * 上传方向先在远端把子目录建出来（SFTP 写入不会自动建父目录）。
   */
  async function enqueueMany(side: "local" | "remote", names: string[]) {
    if (names.length === 0) return;
    paneError = "";
    const entries = side === "local" ? localEntries : remoteEntries;
    const skippedLinks: string[] = [];
    // 先展开成「待提交的具体文件」清单，再逐个提交：这样进度的分母是真实的文件数，
    // 而不是「N 个顶层项」——拖一个目录时那个分母毫无意义。
    type Item = { local: string; remote: string };
    const items: Item[] = [];
    const mkdirs: string[] = [];
    type Walk = { files: string[]; dirs: string[]; skipped_links: string[] };
    try {
      for (const name of names) {
        const isDir = entries.some((e) => e.name === name && e.is_dir);
        if (!isDir) {
          items.push({ local: joinPath(localPath, name), remote: joinPath(remotePath, name) });
          continue;
        }
        const walk =
          side === "local"
            ? await invoke<Walk>("local_walk", { path: joinPath(localPath, name), exclude: excludeArg })
            : await invoke<Walk>("sftp_walk", { sessionId, path: joinPath(remotePath, name), exclude: excludeArg });
        skippedLinks.push(...walk.skipped_links.map((p) => `${name}/${p}`));
        // 上传方向要先建远端目录（含空目录：拖的是「这个目录」，结构缺一块不算传完）
        if (side === "local") {
          mkdirs.push(
            joinPath(remotePath, name),
            ...walk.dirs.map((d) => joinPath(remotePath, `${name}/${d}`)),
          );
        }
        for (const rel of walk.files) {
          items.push({
            local: joinPath(localPath, `${name}/${rel}`),
            remote: joinPath(remotePath, `${name}/${rel}`),
          });
        }
      }
    } catch (e) {
      // 枚举失败（截断/超限/权限）一律不入队：宁可一件不传，也不要传一半却说传完了
      paneError = `展开目录失败，未入队任何作业：${e}`;
      return;
    }

    enqueueing = { total: items.length, done: 0, cancelled: false };
    try {
      // 建目录（父目录必先出现：mkdirs 的顺序来自枚举）。已存在的 mkdir 会报错，
      // 这里忽略——「目录已存在」对「把文件传进去」这个目标不是失败。
      for (const d of mkdirs) {
        if (enqueueing?.cancelled) break;
        try { await invoke("sftp_mkdir", { sessionId, path: d }); } catch { /* 已存在即可 */ }
      }
      for (const it of items) {
        if (enqueueing?.cancelled) break;
        if (side === "local") {
          await invoke("transfer_submit", {
            sessionId, direction: "up", local: it.local, remote: it.remote, resumeOffset: 0,
          });
        } else {
          await invoke("transfer_submit", {
            // 下载目的由 Rust 侧沙箱决定（spec §3.3），前端不可指定
            sessionId, direction: "down", local: "", remote: it.remote, resumeOffset: 0,
          });
        }
        enqueueing = { ...enqueueing!, done: enqueueing!.done + 1 };
      }
      const cancelled = enqueueing?.cancelled === true;
      const done = enqueueing?.done ?? 0;
      if (cancelled) {
        // 取消是用户的意思，不是错误——但必须说清「已经入队的那些仍在传」，
        // 否则用户以为按了取消就什么都没发生。
        syncNote = `已停止入队：${done}/${items.length} 件已提交（这些仍在传输队列里，可在抽屉中取消）`;
      } else if (skippedLinks.length > 0) {
        // 静默跳过等于谎报完整性：软链没传这件事必须出现在界面上
        syncNote =
          `已入队 ${done} 件；跳过 ${skippedLinks.length} 个符号链接（不跟随，避免把目录外的内容传走）：` +
          skippedLinks.slice(0, 3).join(", ") + (skippedLinks.length > 3 ? " 等" : "");
      }
    } catch (e) {
      paneError = `入队失败（已提交 ${enqueueing?.done ?? 0} 件）：${e}`;
    } finally {
      enqueueing = null;
    }
  }

  // ── M4a 外部编辑器关联：远端文件下到暂存区、交给系统关联程序、存盘后回传。
  //
  // 判定（要不要回传、是不是冲突）全在后端 editor_check（引擎 fs_sshengine::editsync）。
  // 这里只做两件事：按 POLL_MS 问一次，以及把「冲突」交给用户拍板——静默覆盖会抹掉
  // 别人在这期间做的修改，而那是不可逆的。
  let editing = $state<string[]>([]); // 正在编辑的远端路径
  let editConflict = $state<{ remote: string; message: string } | null>(null);
  let pollTimer: ReturnType<typeof setInterval> | null = null;

  async function openInEditor(name: string) {
    paneError = "";
    const remote = joinPath(remotePath, name);
    try {
      const local = await invoke<string>("editor_open", { sessionId, remote });
      if (!editing.includes(remote)) editing = [...editing, remote];
      // 暂存路径要说出来：连接断掉时后端会保留这个文件（未存盘的改动不代为丢弃），
      // 用户得知道去哪儿找。
      syncNote = `已在本地编辑器打开：${local}（存盘后自动回传）`;
      startPolling();
    } catch (e) {
      paneError = `打开编辑器失败：${e}`;
    }
  }

  function startPolling() {
    if (pollTimer !== null) return;
    pollTimer = setInterval(() => void pollEdits(), POLL_MS);
  }
  function stopPolling() {
    if (pollTimer !== null) { clearInterval(pollTimer); pollTimer = null; }
  }

  async function pollEdits() {
    if (editing.length === 0) { stopPolling(); return; }
    // 冲突对话框开着时不再问：否则每 2 秒重复弹同一件事，而用户正在读它
    if (editConflict) return;
    for (const remote of editing) {
      let d: EditDecision;
      try {
        d = await invoke<EditDecision>("editor_check", { sessionId, remote });
      } catch (e) {
        // 会话断了/文件不在编辑中：停止跟踪这一个，不影响其余
        paneError = `检查编辑状态失败：${e}`;
        editing = editing.filter((r) => r !== remote);
        continue;
      }
      if (shouldAutoUpload(d)) {
        try {
          await invoke("editor_upload", { sessionId, remote });
          syncNote = `已回传：${remote}`;
          await refresh();
        } catch (e) {
          paneError = `回传失败：${e}`;
        }
      } else if (needsConfirm(d)) {
        editConflict = { remote, message: conflictMessage(remote, d) };
        return; // 一次只问一件
      } else if (d.kind === "remote_gone") {
        editConflict = { remote, message: goneMessage(remote) };
        return;
      }
    }
  }

  /** 冲突/远端消失的确认：用户明确要覆盖时才回传。 */
  async function confirmEditUpload() {
    const c = editConflict;
    editConflict = null;
    if (!c) return;
    try {
      await invoke("editor_upload", { sessionId, remote: c.remote });
      syncNote = `已按你的确认覆盖回传：${c.remote}`;
      await refresh();
    } catch (e) {
      paneError = `回传失败：${e}`;
    }
  }

  /** 放弃这次回传 = 停止编辑该文件（继续跟踪只会每 2 秒再问一次同一件事）。 */
  async function declineEditUpload() {
    const c = editConflict;
    editConflict = null;
    if (!c) return;
    await stopEditing(c.remote);
  }

  async function stopEditing(remote: string) {
    editing = editing.filter((r) => r !== remote);
    if (editing.length === 0) stopPolling();
    try {
      await invoke("editor_close", { sessionId, remote });
    } catch (e) {
      paneError = `停止编辑失败：${e}`;
    }
  }
  function onDropLocal(ev: DragEvent) {
    ev.preventDefault();
    const side = ev.dataTransfer?.getData("side");
    const name = ev.dataTransfer?.getData("name");
    if (name && side === "remote") void enqueueMany("remote", dragNames("remote", name)); // 远程→本地
  }
  function onDropRemote(ev: DragEvent) {
    ev.preventDefault();
    const side = ev.dataTransfer?.getData("side");
    const name = ev.dataTransfer?.getData("name");
    if (name && side === "local") void enqueueMany("local", dragNames("local", name)); // 本地→远程
  }

  // 列表行键盘等价（a11y：行是 role="option"，必须能纯键盘操作）：
  // Enter = 双击语义（目录则进入），Space = 单击语义（走同一 select，故 Ctrl/Shift 组合键同样生效）。
  function onRowKey(side: "local" | "remote", ev: KeyboardEvent, entry: any) {
    if (ev.key === "Enter") {
      if (!entry.is_dir) return;
      ev.preventDefault();
      // enterDir 是进目录的唯一入口（双击与 Enter 共用）：同步浏览的判断因此只有一处
      enterDir(side, entry.name);
    } else if (ev.key === " ") {
      ev.preventDefault(); // 否则空格会滚动列表容器
      select(side, entry.name, ev);
    }
  }

  // 分隔条拖拽：改左栏 flex-basis 百分比（20%–80% 钳制），松开持久化（UI 规格 §1.4 比例记忆）
  function startSplit(ev: MouseEvent) {
    ev.preventDefault();
    const el = containerEl;
    if (!el) return;
    const rect = el.getBoundingClientRect();
    const move = (m: MouseEvent) => {
      splitRatio = Math.min(80, Math.max(20, ((m.clientX - rect.left) / rect.width) * 100));
    };
    const stop = () => {
      window.removeEventListener("mousemove", move);
      window.removeEventListener("mouseup", stop);
      localStorage.setItem("sftp.splitRatio", String(splitRatio));
    };
    window.addEventListener("mousemove", move);
    window.addEventListener("mouseup", stop);
  }
</script>

<!--
  「未完成传输」折叠区：`.fspart` 临时件在界面上的唯一露面处（来源与语义见 script 内 PART_SUFFIX 注）。
  用原生 <details>：默认折叠（不打扰正常浏览）、点击展开、键盘可达，且不必自建展开态与 a11y 处理。
  条目显示的是目标文件名（去掉后缀）与已落盘字节数——用户关心的是「哪个文件没传完、传了多少」，
  而不是那个临时文件名本身。这里刻意不提供任何删除/下载入口：临时件是引擎的续传凭据，
  在 UI 上给出操作等于鼓励用户去破坏它。
-->
{#snippet partialsBlock(items: any[])}
  {#if items.length > 0}
    <details class="partials">
      <summary>{items.length} 项未完成传输</summary>
      <ul>
        {#each items as e (e.name)}
          <li>
            <span class="fname">{e.name.slice(0, -PART_SUFFIX.length)}</span>
            <span class="size">已落盘 {e.size}</span>
          </li>
        {/each}
      </ul>
    </details>
  {/if}
{/snippet}

<div class="wrap">
  {#if paneError}
    <div class="pane-error" role="alert">{paneError} <button title="关闭" onclick={() => (paneError = "")}>✕</button></div>
  {/if}
  <div class="sftp" bind:this={containerEl}>
    <!-- 两栏都是拖放靶区：ARIA 没有「drop target」角色，这里靠 aria-label 让 <section> 取得隐含的 region 地标角色，
         从而满足「带 dragover/drop 的元素必须有 role」的约束；显式写 role="region" 反而是冗余角色（a11y_no_redundant_roles） -->
    <section aria-label="本地文件" style="flex-basis: {splitRatio}%"
             ondragover={(ev) => ev.preventDefault()} ondrop={onDropLocal}>
      <header>本地 <input bind:value={localPath} oninput={() => (dirsTaken.local = true)} /></header>
      <nav class="crumbs" aria-label="本地路径面包屑">
        <button class="crumb" title="用户目录" onclick={() => navLocal("")}>~</button>
        {#each crumbs(localPath) as c, i}
          <span>/</span><button class="crumb" onclick={() => navLocal(crumbPath(localPath, i))}>{c}</button>
        {/each}
      </nav>
      <div class="toolbar">
        <button title="返回" disabled={localStack.length === 0} onclick={backLocal}>←</button>
        <button title="上级" data-testid="local-up" onclick={() => goUp("local")}>⤴</button>
        <button title="刷新" onclick={() => void refresh()}>⟳</button>
        <button disabled={selectedLocal.size === 0} onclick={() => void uploadSelected()}>↑ 上传选中 ({selectedLocal.size})</button>
        <button title="新建目录" data-testid="local-mkdir" onclick={() => void mkdirLocal()}>新建目录</button>
        <button title="重命名（单项）" data-testid="local-rename" disabled={selectedLocal.size !== 1}
                onclick={() => void renameLocal()}>重命名</button>
        <button title="属性（单项）" data-testid="local-props" disabled={localPropsTarget === null}
                onclick={() => { if (localPropsTarget) void openProps("local", localPropsTarget); }}>属性</button>
        <label class="hidden-toggle"><input type="checkbox" bind:checked={showHidden} /> 显示隐藏文件</label>
        <!-- M4a 排除过滤器：作用于两栏（后端 fs_sshengine::filter 是唯一匹配器）。
             语法只支持 * 与 ?、; 分隔、以 / 结尾表示只匹配目录——刻意小，因为这个框
             决定了哪些文件不会被传，用户必须能准确预测它的行为。 -->
        <label class="filter-box" title="排除模式：; 分隔，支持 * 与 ?，以 / 结尾只匹配目录（例：*.tmp;node_modules/）。作用于两栏，并在目录递归传输时跳过匹配项">
          排除
          <input bind:value={excludeText} data-testid="sftp-exclude" placeholder="*.tmp;*.log;node_modules/" />
        </label>
        <label class="hidden-toggle" title="一侧进入子目录时，另一侧进入同名子目录；对不上则原地不动并提示">
          <input type="checkbox" bind:checked={syncBrowse} data-testid="sftp-sync" /> 同步浏览
        </label>
      </div>
      <div class="listhead">
        <button class="sort name" onclick={() => toggleSort("name")}>名称 {sortKey === "name" ? (sortAsc ? "▲" : "▼") : ""}</button>
        <button class="sort size" onclick={() => toggleSort("size")}>大小 {sortKey === "size" ? (sortAsc ? "▲" : "▼") : ""}</button>
        <button class="sort mtime" onclick={() => toggleSort("mtime")}>修改时间 {sortKey === "mtime" ? (sortAsc ? "▲" : "▼") : ""}</button>
        <span class="viewtoggle">
          <button class:on={viewMode === "details"} onclick={() => (viewMode = "details")} title="详细信息">≡</button>
          <button class:on={viewMode === "icons"} onclick={() => (viewMode = "icons")} title="图标">▦</button>
          {#if viewMode === "icons"}
            <button class:on={iconSize === "large"} onclick={() => (iconSize = "large")} title="大图标">大</button>
            <button class:on={iconSize === "small"} onclick={() => (iconSize = "small")} title="小图标">小</button>
          {/if}
        </span>
      </div>
      {#if localTruncated}
        <p class="truncation" role="note">条目超过上限，列表不完整——未显示的项目仍存在于目录中</p>
      {/if}
      {#if loading}
        <p class="placeholder">加载中…</p>
      {:else if shownLocal.length === 0}
        <p class="placeholder">{localEntries.length ? "没有可显示的文件。请检查排除规则或开启显示隐藏文件。" : "此目录为空，可上传或新建文件。"}</p>
      {:else if viewMode === "icons"}
        <ul class="icons {iconSize}" role="listbox" aria-multiselectable="true" aria-label="本地文件列表">{#each shownLocal as e (e.name)}
          <li draggable class:tile-selected={selectedLocal.has(e.name)} class:tile={true}
              role="option" tabindex="0" aria-selected={selectedLocal.has(e.name)}
              title={e.name}
              onclick={(ev) => select("local", e.name, ev)}
              onkeydown={(ev) => onRowKey("local", ev, e)}
              ondblclick={() => e.is_dir && enterDir("local", e.name)}
              ondragstart={(ev) => { ev.dataTransfer?.setData("side", "local"); ev.dataTransfer?.setData("name", e.name); }}>
            <span class="tile-icon">{e.is_dir ? "📁" : e.is_symlink ? "🔗" : "📄"}</span>
            <span class="tile-name">{e.name}</span>
          </li>
        {/each}</ul>
      {:else}
        <!-- listbox/option：文件行本就是可多选列表项，用这组角色既拿到正确语义，
             也让 li 从「非交互元素挂了鼠标事件」的告警里彻底脱身（不再需要 svelte-ignore） -->
        <ul role="listbox" aria-multiselectable="true" aria-label="本地文件列表">{#each shownLocal as e (e.name)}
          <li draggable class:selected={selectedLocal.has(e.name)}
              role="option" tabindex="0" aria-selected={selectedLocal.has(e.name)}
              onclick={(ev) => select("local", e.name, ev)}
              onkeydown={(ev) => onRowKey("local", ev, e)}
              ondblclick={() => e.is_dir && enterDir("local", e.name)}
              ondragstart={(ev) => { ev.dataTransfer?.setData("side", "local"); ev.dataTransfer?.setData("name", e.name); }}>
            <span class="fname">{e.is_dir ? "📁 " : e.is_symlink ? "🔗 " : ""}{e.name}{e.is_dir ? "/" : ""}</span>
            <span class="size">{e.is_dir ? "" : formatSize(e.size)}</span>
            <span class="mtime">{formatMtime(e.mtime)}</span>
          </li>
        {/each}</ul>
      {/if}
      {@render partialsBlock(partialLocal)}
    </section>
    <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
    <div class="splitter" role="separator" aria-orientation="vertical" onmousedown={startSplit}></div>
    <section class="right" aria-label="远程文件"
             ondragover={(ev) => ev.preventDefault()} ondrop={onDropRemote}>
      <header>远程 <input bind:value={remotePath} oninput={() => (dirsTaken.remote = true)} /></header>
      <nav class="crumbs" aria-label="远程路径面包屑">
        <button class="crumb" title="相对根" onclick={() => navRemote(".")}>.</button>
        {#each crumbs(remotePath) as c, i}
          <span>/</span><button class="crumb" onclick={() => navRemote(crumbPath(remotePath, i))}>{c}</button>
        {/each}
      </nav>
      <div class="toolbar">
        {#if !floating && onFloat}
          <!-- 浮动出去之后终端与文件能同屏看，这正是「文件管理器桌面化」要的工作流。 -->
          <button title="在浮动窗口中打开文件视图" data-testid="remote-float" onclick={onFloat}>⧉ 浮动</button>
        {/if}
        <button title="返回" disabled={remoteStack.length === 0} onclick={backRemote}>←</button>
        <button title="上级" data-testid="remote-up" onclick={() => goUp("remote")}>⤴</button>
        <button title="刷新" onclick={() => void refresh()}>⟳</button>
        <button title="新建目录" onclick={() => void mkdirRemote()}>新建目录</button>
        <button title="删除选中（二次确认）" disabled={selectedRemote.size === 0} onclick={() => void removeSelected()}>删除</button>
        <button title="重命名（单项）" disabled={selectedRemote.size !== 1} onclick={() => void renameSelected()}>重命名</button>
        <button title="新建软链" data-testid="remote-symlink"
                onclick={() => { symlinkName = ""; symlinkTarget = remotePropsTarget ?? ""; symlinkOpen = true; }}>新建软链</button>
        <button title="属性（单项）" data-testid="remote-props" disabled={remotePropsTarget === null}
                onclick={() => { if (remotePropsTarget) void openProps("remote", remotePropsTarget); }}>属性</button>
        <!-- 远端剪切/复制/粘贴（M7.3）。粘贴在跨会话时禁用并把原因写进 title——
             可点然后报一句失败会让用户以为程序坏了。 -->
        <button title="剪切选中（粘贴时移动；此刻不动任何文件）" data-testid="remote-cut"
                disabled={selectedRemote.size === 0} onclick={() => cutOrCopySelected(true)}>剪切</button>
        <button title="复制选中" data-testid="remote-copy"
                disabled={selectedRemote.size === 0} onclick={() => cutOrCopySelected(false)}>复制</button>
        <button title={clipBlocked ?? clipSummary($fileClip)} data-testid="remote-paste"
                disabled={clipBlocked !== null} onclick={() => void paste()}>粘贴</button>
        <button disabled={selectedRemote.size === 0} onclick={() => void downloadSelected()}>↓ 下载选中 ({selectedRemote.size})</button>
      </div>
      <div class="listhead">
        <button class="sort name" onclick={() => toggleSort("name")}>名称 {sortKey === "name" ? (sortAsc ? "▲" : "▼") : ""}</button>
        <span class="col perms" title="文件权限（读/写/执行）">权限</span>
        <button class="sort size" onclick={() => toggleSort("size")}>大小 {sortKey === "size" ? (sortAsc ? "▲" : "▼") : ""}</button>
        <button class="sort mtime" onclick={() => toggleSort("mtime")}>修改时间 {sortKey === "mtime" ? (sortAsc ? "▲" : "▼") : ""}</button>
        <span class="viewtoggle">
          <button class:on={viewMode === "details"} onclick={() => (viewMode = "details")} title="详细信息">≡</button>
          <button class:on={viewMode === "icons"} onclick={() => (viewMode = "icons")} title="图标">▦</button>
          {#if viewMode === "icons"}
            <button class:on={iconSize === "large"} onclick={() => (iconSize = "large")} title="大图标">大</button>
            <button class:on={iconSize === "small"} onclick={() => (iconSize = "small")} title="小图标">小</button>
          {/if}
        </span>
      </div>
      {#if editing.length > 0}
        <p class="editing" role="status" data-testid="sftp-editing">
          正在编辑 {editing.length} 个文件（存盘后自动回传）：
          {#each editing as r (r)}
            <button data-testid="sftp-edit-stop" title="停止编辑 {r}" onclick={() => void stopEditing(r)}>{r} ×</button>
          {/each}
        </p>
      {/if}
      {#if enqueueing}
        <p class="enqueue" role="status" data-testid="sftp-enqueue">
          正在入队 {enqueueing.done}/{enqueueing.total}
          <button data-testid="sftp-enqueue-cancel" onclick={cancelEnqueue}>停止入队</button>
        </p>
      {/if}
      {#if syncNote}
        <p class="sync-note" role="status" data-testid="sftp-sync-note">{syncNote}</p>
      {/if}
      {#if remoteTruncated}
        <p class="truncation" role="note">条目超过上限，列表不完整——未显示的项目仍存在于目录中</p>
      {/if}
      {#if loading}
        <p class="placeholder">加载中…</p>
      {:else if shownRemote.length === 0}
        <p class="placeholder">{remoteEntries.length ? "没有可显示的文件。请检查排除规则或开启显示隐藏文件。" : "此目录为空，可上传或新建文件。"}</p>
      {:else if viewMode === "icons"}
        <ul class="icons {iconSize}" role="listbox" aria-multiselectable="true" aria-label="远程文件列表">{#each shownRemote as e (e.name)}
          <li draggable class:tile={true} class:tile-selected={selectedRemote.has(e.name)}
              role="option" tabindex="0" aria-selected={selectedRemote.has(e.name)}
              title={e.name}
              onclick={(ev) => select("remote", e.name, ev)}
              onkeydown={(ev) => onRowKey("remote", ev, e)}
              ondblclick={() => remoteDblClick(e)}
              ondragstart={(ev) => { ev.dataTransfer?.setData("side", "remote"); ev.dataTransfer?.setData("name", e.name); }}>
            <span class="tile-icon">{e.is_dir ? "📁" : e.is_symlink ? "🔗" : "📄"}</span>
            <span class="tile-name">{e.name}</span>
          </li>
        {/each}</ul>
      {:else}
        <ul role="listbox" aria-multiselectable="true" aria-label="远程文件列表">{#each shownRemote as e (e.name)}
          <li draggable class:selected={selectedRemote.has(e.name)}
              role="option" tabindex="0" aria-selected={selectedRemote.has(e.name)}
              onclick={(ev) => select("remote", e.name, ev)}
              onkeydown={(ev) => onRowKey("remote", ev, e)}
              ondblclick={() => remoteDblClick(e)}
              ondragstart={(ev) => { ev.dataTransfer?.setData("side", "remote"); ev.dataTransfer?.setData("name", e.name); }}>
            <span class="fname" title={e.is_symlink ? "符号链接——目标见「属性」" : e.name}>{e.is_dir ? "📁 " : e.is_symlink ? "🔗 " : ""}{e.name}{e.is_dir ? "/" : ""}</span>
            <span class="perms" title={e.perms ?? "权限未知"}>{e.is_dir ? "d" : e.is_symlink ? "l" : "-"}{e.perms ?? "---------"}</span>
            <span class="size">{e.is_dir ? "" : formatSize(e.size)}</span>
            <span class="mtime">{formatMtime(e.mtime)}</span>
            <button title="下载" onclick={() => void download(e.name)}>↓</button>
            <button title="属性" data-testid="row-props-{e.name}" onclick={() => void openProps("remote", e.name)}>ⓘ</button>
            {#if !e.is_dir && !e.is_symlink}
              <button title="用本地关联程序编辑（存盘后自动回传）" data-testid="row-edit-{e.name}"
                      onclick={() => void openInEditor(e.name)}>✎</button>
            {/if}
          </li>
        {/each}</ul>
      {/if}
      {@render partialsBlock(partialRemote)}
    </section>
  </div>
</div>

<ConfirmDialog
  open={editConflict !== null}
  title="回传确认"
  message={editConflict?.message ?? ""}
  confirmText="覆盖回传"
  danger
  onConfirm={() => void confirmEditUpload()}
  onCancel={() => void declineEditUpload()}
/>

<FilePropsDialog open={propsOpen} detail={propsDetail} err={propsErr} onClose={() => (propsOpen = false)} />

{#if symlinkOpen}
  <!-- 两个输入框，故不复用 TextInputDialog（它是单行输入模态）。语义与它一致：
       Esc/取消不做任何事，确定走同一条校验（lib/fileprops.validateSymlink）。 -->
  <div class="overlay" role="presentation" onclick={() => (symlinkOpen = false)}>
    <div class="sym-dialog" role="dialog" aria-modal="true" aria-label="新建软链" tabindex="-1"
         bind:this={symlinkEl} data-testid="symlink-dialog"
         onclick={(e) => e.stopPropagation()}
         onkeydown={(e) => { if (e.key === "Escape") symlinkOpen = false; }}>
      <h3>新建软链</h3>
      {#if symlinkError}<p class="sym-err" role="alert">{symlinkError}</p>{/if}
      <label>链接名（在当前远端目录创建）
        <input bind:value={symlinkName} data-testid="symlink-name" placeholder="link-name" />
      </label>
      <label>链接目标（远端路径；相对路径以当前远端目录为起点）
        <input bind:value={symlinkTarget} data-testid="symlink-target" placeholder="../shared/file.bin" />
      </label>
      {#if validateSymlink(symlinkName, symlinkTarget)}
        <p class="sym-err" role="alert" data-testid="symlink-err">{validateSymlink(symlinkName, symlinkTarget)}</p>
      {/if}
      <div class="sym-actions">
        <button data-testid="symlink-ok" disabled={symlinkBusy || !!validateSymlink(symlinkName, symlinkTarget)}
                onclick={() => void createSymlink(symlinkName, symlinkTarget)}>创建</button>
        <button data-testid="symlink-cancel" onclick={() => (symlinkOpen = false)}>取消</button>
      </div>
    </div>
  </div>
{/if}

<!-- 粘贴冲突（M7.3 出口标准①）：三档无默认，各自说清后果。 -->
<PasteConflictDialog
  open={pasteConflict !== null}
  names={pasteConflict?.names ?? []}
  dstDir={remotePath}
  onPick={(pol) => void pasteWithPolicy(pol)}
  onCancel={() => (pasteConflict = null)}
/>
<!-- 双击 .sh：先看原文，再选前台/后台（M7.3 出口标准②）。 -->
<ScriptRunDialog
  open={scriptRun !== null}
  path={scriptRun?.path ?? ""}
  {sessionId}
  onRun={(mode) => void runScript(mode)}
  onCancel={() => (scriptRun = null)}
/>

<style>
  .overlay { position: fixed; inset: 0; background: rgba(0,0,0,.5); display: grid; place-items: center; z-index: 60; }
  .sym-dialog { width: 420px; max-width: 92vw; max-height: 92vh; overflow-y: auto; padding: 14px 18px; background: var(--fs-bg-elevated);
                border: 1px solid var(--fs-border); border-radius: 6px; color: var(--fs-fg-primary); font-size: 13px; }
  .sym-dialog h3 { margin: 0 0 8px; font-size: 14px; }
  .sym-dialog label { display: block; margin-bottom: 8px; color: var(--fs-fg-secondary); }
  .sym-dialog input { width: 100%; margin-top: 3px; background: var(--fs-bg-input); color: var(--fs-fg-primary);
                      border: 1px solid var(--fs-border); border-radius: 4px; padding: 3px 6px; }
  .sym-err { color: var(--fs-danger); font-size: 12px; margin: 0 0 8px; }
  .sym-actions { display: flex; gap: 8px; justify-content: flex-end; }
  .sym-actions button { padding: 4px 12px; border: 1px solid var(--fs-border); background: var(--fs-bg-panel);
                        color: var(--fs-fg-primary); border-radius: 4px; cursor: pointer; }
  .wrap { display: flex; flex-direction: column; flex: 1; min-height: 0; height: 100%; }
  .pane-error { color: var(--fs-danger); background: var(--fs-bg-elevated); font-size: 12px; padding: 2px 8px; } /* 口径同 ProfileDialog .err */
  .pane-error button { background: none; border: 0; color: inherit; cursor: pointer; }
  .sftp { display: flex; flex: 1; min-height: 0; }
  section { overflow-y: auto; border-right: 1px solid var(--fs-bg-panel); min-width: 0; }
  section.right { flex: 1; border-right: 0; }
  .splitter { flex: none; width: 4px; cursor: col-resize; background: var(--fs-border-strong); }
  .splitter:hover { background: var(--fs-accent); }
  .filter-box { display: inline-flex; align-items: center; gap: 4px; color: var(--fs-fg-secondary); font-size: 12px; }
  .filter-box input { width: 150px; background: var(--fs-bg-input); color: var(--fs-fg-primary);
                      border: 1px solid var(--fs-border); border-radius: 3px; padding: 1px 4px; }
  .sync-note { color: var(--fs-fg-secondary); background: var(--fs-bg-panel); font-size: 12px; margin: 0; padding: 2px 8px; }
  .enqueue { color: var(--fs-fg-primary); background: var(--fs-bg-panel); font-size: 12px; margin: 0; padding: 2px 8px;
             display: flex; gap: 8px; align-items: center; }
  .editing { color: var(--fs-fg-secondary); background: var(--fs-bg-panel); font-size: 12px; margin: 0;
             padding: 2px 8px; display: flex; gap: 6px; align-items: center; flex-wrap: wrap; }
  .editing button { background: none; border: 1px solid var(--fs-border); border-radius: 3px;
                    color: inherit; cursor: pointer; font-size: 11px; padding: 0 6px; max-width: 320px;
                    overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .enqueue button { background: none; border: 1px solid var(--fs-border); border-radius: 3px;
                    color: inherit; cursor: pointer; font-size: 11px; padding: 0 6px; }
  header { display: flex; gap: 4px; padding: 2px 6px; }
  header input { flex: 1; }
  .crumbs { display: flex; flex-wrap: wrap; gap: 2px; padding: 0 6px; font-size: 12px; }
  .crumb { background: none; border: 0; padding: 0; color: var(--fs-accent); cursor: pointer; }
  .toolbar { display: flex; flex-wrap: wrap; gap: 4px; align-items: center; padding: 2px 6px; font-size: 12px; }
  .toolbar button { background: none; border: 1px solid var(--fs-border); color: var(--fs-fg-primary); cursor: pointer; }
  .toolbar button:disabled { opacity: 0.4; cursor: default; }
  .hidden-toggle { display: inline-flex; gap: 4px; align-items: center; }
  .listhead { display: flex; gap: 8px; align-items: center; padding: 2px 8px; border-bottom: 1px solid var(--fs-bg-panel); font-size: 12px; }
  .sort { background: none; border: 0; padding: 0; color: var(--fs-accent); cursor: pointer; text-align: left; }
  .sort.name { flex: 1; }
  .sort.size { width: 72px; }
  .sort.mtime { width: 128px; }
  .viewtoggle { display: inline-flex; gap: 2px; margin-left: auto; }
  .viewtoggle button { background: none; border: 0; padding: 1px 5px; color: var(--fs-fg-secondary); cursor: pointer; font-size: 12px; border-radius: 3px; }
  .viewtoggle button.on { background: var(--fs-bg-elevated); color: var(--fs-fg-primary); }
  ul { list-style: none; padding: 0; margin: 0; font-size: 12px; }
  li { display: flex; gap: 8px; align-items: center; padding: 2px 8px; cursor: default; }
  li:hover { background: var(--fs-bg-hover); }
  li.selected { background: var(--fs-selection); }
  li .fname { flex: 1; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  li .perms { width: 92px; color: var(--fs-fg-secondary); flex: none; }
  .col.perms { width: 92px; color: var(--fs-fg-secondary); flex: none; }
  li .size { width: 72px; text-align: right; color: var(--fs-fg-secondary); flex: none; }
  li .mtime { width: 128px; color: var(--fs-fg-secondary); flex: none; }
  /* 图标视图：网格铺排，大小可切（大/小两档，对标 Windows 资源管理器的查看切换） */
  ul.icons { display: grid; grid-template-columns: repeat(auto-fill, minmax(96px, 1fr)); gap: 6px; padding: 8px; }
  ul.icons.large { grid-template-columns: repeat(auto-fill, minmax(112px, 1fr)); }
  ul.icons.small { grid-template-columns: repeat(auto-fill, minmax(72px, 1fr)); }
  li.tile { flex-direction: column; gap: 2px; padding: 6px 4px; border-radius: 4px; text-align: center; }
  li.tile-selected { background: var(--fs-selection); }
  li.tile .tile-icon { font-size: 22px; }
  ul.icons.large li.tile .tile-icon { font-size: 30px; }
  ul.icons.small li.tile .tile-icon { font-size: 16px; }
  li.tile .tile-name { font-size: 11.5px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; max-width: 100%; }
  .placeholder { color: var(--fs-fg-secondary); font-size: 12px; padding: 8px; }
  .truncation { background: var(--fs-warn-dim); color: var(--fs-fg-primary); font-size: 12px; padding: 2px 8px; margin: 0; } /* 2026-08-31：幻影令牌 --fs-warning + 状态色作整块背景（对比度不足）一并修 */
  .partials { border-top: 1px dashed var(--fs-border); margin-top: 4px; font-size: 12px; }
  .partials summary { padding: 3px 8px; color: var(--fs-fg-secondary); cursor: pointer; }
</style>
