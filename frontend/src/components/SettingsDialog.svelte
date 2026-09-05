<script lang="ts">
  import { onMount, tick } from "svelte";
  import { toast } from "../lib/toast";
  import { tabNavigation } from "../lib/tabNavigation";
  import { useFocusTrap } from "../lib/focusTrap";
  import { invoke, listen, openExternal, settingGet, settingSet as persistSetting } from "../lib/ipc";
  import { actionLabel, withSuppressed, type DangerousAction } from "../lib/action-confirm";
  import { loadSuppressed, saveSuppressed } from "../lib/confirm-gate";
  import { untilUnmount } from "../lib/lifecycle";
  import KeymapEditor from "./KeymapEditor.svelte";
  import HighlightEditor from "./HighlightEditor.svelte";
  import SchemeEditor from "./SchemeEditor.svelte";
  import FilePickerDialog from "./FilePickerDialog.svelte";
  import { downloadJson, exportFileName, serializeSchemes } from "../lib/scheme-file";
  import { parseHighlightRules, type HighlightRule } from "../lib/highlights";
  import { parseBindings, type KeyBindings } from "../lib/keymap";
  import { setKeyBindings } from "../lib/shortcuts";
  import { SUPPORTED_LOCALES, locale, setLocale, t, type Locale } from "../lib/i18n";
  import {
    DEFAULT_SCHEME,
    SCHEMES,
    customSchemeId,
    parseCustomSchemes,
    type StoredCustomScheme,
  } from "../lib/term-schemes";

  let {
    open = false,
    onClose = () => {},
    /** 打开时定位到指定页签（M4a：菜单「高亮关键字…」直达终端外观页）。 */
    initialTab = null,
  }: { open?: boolean; onClose?(): void; initialTab?: string | null } = $props();

  // S257：§7 要求「全部 role=dialog 模态」装焦点圈闭；本组件是 Task 17 唯一模态，此前未接线，
  // focusTrap.ts 在产品代码里零消费者（仅自测覆盖）。打开时聚焦首个可用页签，关闭时焦点归还触发元素。
  let dialogEl = $state<HTMLDivElement | null>(null);
  $effect(() => {
    if (!open || !dialogEl) return;
    const el = dialogEl;
    let cancelled = false;
    let cleanup: (() => void) | undefined;
    void tick().then(() => {
      if (cancelled) return;
      const initial = el.querySelector<HTMLElement>('.tabs button[aria-selected="true"]') ?? el;
      cleanup = useFocusTrap(el, { initial });
    });
    return () => { cancelled = true; cleanup?.(); };
  });

  // S246：显式类型标注替代 `as const`——const 断言下无 note 字段的成员不含该属性，联合类型访问 t.note 报 TS2339
  const TABS: readonly { id: string; label: string; enabled: boolean; note?: string }[] = [
    { id: "general", label: "通用", enabled: true },
    { id: "keyboard", label: "键盘与鼠标", enabled: true },
    { id: "terminal", label: "终端外观", enabled: true },
    { id: "security", label: "安全与 Vault", enabled: true },
    { id: "update", label: "更新", enabled: true },
    { id: "ai", label: "AI", enabled: true },
  ];
  type TabId = (typeof TABS)[number]["id"];
  let tab = $state<TabId>("general");
  // 每次打开都按 initialTab 定位：菜单项直达某页的入口若只在首次生效，第二次点
  // 就会停在上次离开的页签上——用户点「高亮关键字…」看到的是「通用」页。
  $effect(() => {
    if (open && initialTab && TABS.some((t) => t.id === initialTab && t.enabled)) tab = initialTab;
  });

  let saveState = $state("");
  let saveFailed = $state(false);
  const failedSettings = new Map<string, unknown>();
  let pendingSaves = $state(0);
  async function settingSet(key: string, value: unknown): Promise<boolean> {
    pendingSaves++;
    try {
      const ok = await persistSetting(key, value);
      if (ok === false) failedSettings.set(key, value);
      else failedSettings.delete(key);
      saveFailed = failedSettings.size > 0;
      saveState = saveFailed ? "部分更改未保存，请重试。" : "更改已保存";
      return ok !== false;
    } catch (e) {
      failedSettings.set(key, value);
      saveFailed = true;
      saveState = "更改未保存：" + e;
      return false;
    } finally { pendingSaves--; }
  }
  async function retrySettings() {
    for (const [key, value] of [...failedSettings]) {
      if (!await settingSet(key, value)) continue;
      if (key === "keyboard.bindings") { keyBindings = value as KeyBindings; setKeyBindings(keyBindings); }
      if (key === "term.highlights") highlightRules = value as HighlightRule[];
      if (key === "term.customSchemes") customSchemes = value as StoredCustomScheme[];
    }
  }
  async function copyMcp(value: string, label: string) {
    if (!value) return;
    try { await navigator.clipboard.writeText(value); toast.info(label + "已复制"); }
    catch (e) { mcpError = "复制失败，请手动选中内容复制：" + e; }
  }

  // 通用
  //
  // 语言不再是本组件的 $state：它是全局的（菜单栏也要跟着变），所以真身是 i18n 的
  // `locale` store，本页只是它的一个控件面。用局部 $state 复制一份的话，
  // 「控件显示的」与「界面实际用的」就成了两个可以走散的值。
  let languageError = $state("");

  /** 切语言：落库成功才改 store（见 setLocale 的文档注），失败要说出来。 */
  async function changeLanguage(next: string): Promise<void> {
    try {
      await setLocale(next as Locale);
      languageError = "";
    } catch (e) {
      languageError = String(e);
    }
  }

  let density = $state<"compact" | "loose">("compact");
  let restoreUnclosed = $state(true);
  // 键盘与鼠标
  let keyboardModeDefault = $state<"remote" | "local">("remote");
  let rightClick = $state<"paste" | "menu">("paste");
  let copyOnSelect = $state(false);
  let multilinePasteConfirm = $state(true);
  let ctrlVPaste = $state(true); // R101：§2.12「Ctrl+V 粘贴开关」，默认开（关则粘贴仅剩 Ctrl+Shift+V）
  // 终端外观
  let fontSize = $state(13);
  let opacity = $state(100);
  let bellMode = $state<"badge" | "badge+notify">("badge");
  /** 默认配色方案（§2.12 明列本项；§3.2 三级作用域的**全局层**）。
   *  此前本页只有一句写着内部键名的提示文字、没有控件，全仓 term.scheme 零写入方，
   *  全局层于是永久钉死在 DEFAULT_SCHEME——三级作用域实际只有两级可用。 */
  let scheme = $state(DEFAULT_SCHEME.id);

  /**
   * 导入进来的配色（settings 键 `term.customSchemes`，写入方是「文件 → 导入」）。
   *
   * 本页是它**唯一**的删除入口。导入那条路径只会往里加，若这里不给删，用户就处在
   * 「界面上能添加、不能移除」的状态——那与要求他去改数据库没有区别。
   */
  let customSchemes = $state<StoredCustomScheme[]>([]);

  /**
   * 删掉一套导入的配色。
   *
   * 若删的正是当前选中的那一套，把全局方案退回默认——否则 `term.scheme` 会指向一个
   * 不存在的 id，而消费侧（TerminalPane / 三级作用域解析）遇到未知 id 是**静默回落**默认色的：
   * 用户看到终端变了色却在设置页里找不到原因（下拉框显示的是一个已经不在列表里的值）。
   */
  async function deleteCustomScheme(name: string): Promise<void> {
    const next = customSchemes.filter((c) => c.name !== name);
    if (!await settingSet("term.customSchemes", next)) return;
    customSchemes = next;
    if (scheme === customSchemeId(name)) {
      scheme = DEFAULT_SCHEME.id;
      await settingSet("term.scheme", scheme);
    }
  }

  // ── M4b 配色编辑器 ──
  //
  // `editing === null` 表示编辑器没开；`editing.target === null` 表示在新建。
  // 用一个对象而不是两个布尔，是为了让「开着」与「在编辑哪一套」不可能各说各话。
  let editing = $state<{ target: StoredCustomScheme | null } | null>(null);

  /**
   * 保存编辑器的结果。
   *
   * 改名（编辑时把名字改了）要**同时搬走选中状态**：`term.scheme` 存的是 `custom:旧名`，
   * 不跟着改的话它就指向一个不存在的 id，而消费侧遇到未知 id 是静默回落默认色的——
   * 用户改了个名字，终端却变回默认配色，且设置页里看不出为什么。
   */
  async function saveScheme(s: StoredCustomScheme): Promise<void> {
    const oldName = editing?.target?.name ?? null;
    const next =
      oldName === null
        ? [...customSchemes, s]
        : customSchemes.map((c) => (c.name === oldName ? s : c));
    if (!await settingSet("term.customSchemes", next)) return;
    customSchemes = next;
    if (oldName !== null && oldName !== s.name && scheme === customSchemeId(oldName)) {
      scheme = customSchemeId(s.name);
      await settingSet("term.scheme", scheme);
    }
    editing = null;
  }

  /** 导出：一套或全部。走与「文件 → 导入」同一个格式，导出的文件从那里读得回来。 */
  function exportSchemes(list: StoredCustomScheme[]): void {
    if (list.length === 0) return;
    downloadJson(serializeSchemes(list), exportFileName(list, todayStamp()));
  }

  /** 导出文件名里的日期戳。同名覆盖对「昨天导的那份」是无声的破坏。 */
  function todayStamp(): string {
    return new Date().toISOString().slice(0, 10);
  }
  // 安全与 Vault
  // ── M4b 手动版本检查 ──
  //
  // 这几个状态刻意都不带「自动」语义：没有定时器、onMount 里也不检查。
  // 「不点就不联网」这条承诺靠的是**这里没有那行代码**，而不是靠一个可以被翻转的开关。
  let appVersion = $state("");
  let updateUrl = $state("");
  /**
   * AI 执行档位（settings 键 `ai.mode`，M2 出口第 14 项的「总开关」）。
   *
   * 三档而不是一个布尔：出口原文要求「关闭时 NL→命令/解读/执行入口全部不可用」，
   * 那是 `disabled`；而开着的时候还要分「只读命令才让过」与「危险命令要确认」——
   * 一个布尔表达不了后两者的差别，而那个差别正是这个功能安全与否的所在。
   *
   * 串值用**下划线**形式（`read_only` / `with_confirm`）：那是 Rust 侧
   * `AiMode::from_str_exact` 认的稳定串。曾在别处手写成连字符形式，
   * 结果是那一档在界面上选得到、后端一律解析失败回落——选项存在但永远不生效。
   */
  let aiMode = $state("with_confirm");

  // ── MCP（M3 出口 5）── 对外 stdio MCP Server 的开关与授权。默认关：总设计 §4.5
  // 「默认关闭」。工具名单逐个勾选——白名单里没有的工具，外部客户端连枚举都枚举不到。
  // 这份清单与 `crates/mcpbridge/src/tool.rs` 的 MCP_MANIFEST 同名；两边漂移时
  // 「勾了一个不存在的工具」最坏只是被后端 `Authorization::new` 归入 unknown，不生效。
  const MCP_TOOL_CHOICES = [
    "sessions.list",
    "sessions.open",
    "terminal.read",
    "terminal.send",
    "terminal.send_raw",
    "command.run",
    "sftp.list",
    "sftp.read",
    "sftp.write",
  ] as const;
  const MCP_TOOL_LABELS: Record<string, string> = {
    "sessions.list": "查看会话列表", "sessions.open": "打开连接", "terminal.read": "读取终端内容",
    "terminal.send": "发送终端文本", "terminal.send_raw": "发送原始按键数据", "command.run": "执行命令",
    "sftp.list": "查看远程目录", "sftp.read": "读取远程文件", "sftp.write": "写入远程文件",
  };
  let mcpEnabled = $state(false);
  let mcpCaller = $state("mcp");
  let mcpTools = $state<string[]>([]);
  /** MCP 区的局部错误（开关/轮换失败时显示在本区内，不弹全局 toast）。 */
  let mcpError = $state("");
  /** 对外 MCP 的实时状态（监听中/端口/连接数）。null = 还没取到。 */
  let mcpStatus = $state<{ enabled: boolean; listening: boolean; port: number; connections: number } | null>(null);
  /** 连接 token：展示给用户抄进外部客户端配置。 */
  let mcpToken = $state("");
  /** 连接指引（可粘贴的 JSON 片段 + exe 路径）。 */
  let mcpGuide = $state<{ exe: string; config: string } | null>(null);
  let unlistenMcpStatus: (() => void) | null = null;

  /** MCP 开关：经 mcp_set_enabled（写设置 + 起停监听，即时生效），
   *  而不是 settingSet——那只写设置位，开关就成了「改了等于没改」的假开关。 */
  async function setMcpEnabled(on: boolean): Promise<void> {
    mcpEnabled = on;
    try {
      mcpStatus = await invoke("mcp_set_enabled", { enabled: on });
      if (on) await loadMcpGuide(); // 起来了才有端口可言，指引此刻是准的
    } catch (e) {
      mcpEnabled = !on; // 回滚 UI 态：起停失败不能让开关显示一个不存在的状态
      mcpError = `MCP 开关失败：${e}`;
    }
  }

  async function loadMcpStatus(): Promise<void> {
    try {
      mcpStatus = await invoke("mcp_status");
    } catch { /* 状态取不到按 null 展示「未知」——比假装关着诚实 */ }
  }

  async function loadMcpGuide(): Promise<void> {
    try {
      mcpGuide = await invoke("mcp_connection_info");
      mcpToken = await invoke("mcp_token_get");
    } catch { /* 指引拿不到就不展示那一栏；token 拿不到同理 */ }
  }

  async function regenMcpToken(): Promise<void> {
    try {
      mcpToken = await invoke("mcp_token_regen");
      await loadMcpGuide(); // 指引里的 env 带着旧 token，必须一起刷新
    } catch (e) {
      mcpError = `轮换 token 失败：${e}`;
    }
  }

  /** 状态点的文案与颜色语义（三态）。 */
  function mcpStatusText(): { text: string; cls: string } {
    if (!mcpEnabled) return { text: "未启用", cls: "off" };
    if (!mcpStatus?.listening) return { text: "启动中…", cls: "wait" };
    if (mcpStatus.connections > 0)
      return { text: `已连接客户端 ×${mcpStatus.connections}（端口 ${mcpStatus.port}）`, cls: "on" };
    return { text: `监听中（端口 ${mcpStatus.port}，等待客户端）`, cls: "on" };
  }

  /** 勾选/取消一个 MCP 工具并落库。名单即授权面——没勾的对外不可枚举。 */
  function toggleMcpTool(tool: string): void {
    const has = mcpTools.includes(tool);
    mcpTools = has ? mcpTools.filter((t) => t !== tool) : [...mcpTools, tool];
    void settingSet("mcp.tools", mcpTools);
  }

  // ── 外部 MCP 挂载（M3 出口 6）── 把外部 server（stdio 子进程）挂给 Agent。
  type McpMount = { server_id: string; command: string; args: string[] };
  let mcpMounts = $state<McpMount[]>([]);

  async function saveMcpMounts(): Promise<void> {
    const names = new Set<string>();
    for (const [i, mount] of mcpMounts.entries()) {
      if (!mount.server_id.trim() || !mount.command.trim()) { mcpError = `第 ${i + 1} 个服务器：请填写标识和启动命令。`; return; }
      if (names.has(mount.server_id.trim())) { mcpError = `服务器标识「${mount.server_id.trim()}」重复，请使用不同名称。`; return; }
      names.add(mount.server_id.trim());
    }
    if (await settingSet("mcp.mounts", mcpMounts)) mcpError = "";
  }
  function addMcpMount(): void {
    mcpMounts = [...mcpMounts, { server_id: "", command: "", args: [] }];
  }
  function removeMcpMount(i: number): void {
    mcpMounts = mcpMounts.filter((_, idx) => idx !== i);
  }

  let updateChecking = $state(false);
  let updateResult = $state<{ kind: string; message: string; url?: string } | null>(null);
  let updateUrlError = $state("");

  /**
   * 存更新地址。**不能是 `void settingSet(…)`**——后端白名单会拒非 https 的地址
   * （明文 http 的清单可被中间人整份换掉，届时「打开发布页」指向的就是钓鱼页），
   * 而 `void` 把那个 rejection 吞掉之后，用户看到的是：输入框里躺着他刚填的
   * `http://…`，没有任何报错，而库里存的还是旧值——他会以为改成功了。
   */
  async function saveUpdateUrl(): Promise<void> {
    const next = updateUrl.trim();
    try {
      if (!await settingSet("update.manifestUrl", next)) { updateUrlError = "地址未保存。请使用 https:// 地址，或留空使用默认地址，然后重试。"; return; }
      updateUrlError = "";
      updateUrl = next; // 把 trim 的结果显示出来，免得输入框与库里存的不是同一个串
    } catch (e) {
      updateUrlError = String(e);
    }
  }

  /** 用户点了「检查更新」——**唯一**会发起联网的路径。 */
  async function runUpdateCheck(): Promise<void> {
    if (updateChecking) return;
    updateChecking = true;
    updateResult = null;
    try {
      await saveUpdateUrl();
      if (updateUrlError) return;
      updateResult = await invoke<{ kind: string; message: string; url?: string }>("update_check");
    } catch (e) {
      // 后端已把各种失败映射成 Failed 分支；走到这里说明是 IPC 本身出错
      updateResult = { kind: "failed", message: "检查更新失败：" + String(e) };
    } finally {
      updateChecking = false;
    }
  }

  let autoLock = $state("0");
  let hostProbe = $state("30");
  // M4a 会话纯文本日志（settings 键 session.log；默认关——明文落盘须显式选择）
  let logEnabled = $state(false);
  let logDir = $state("");
  let logTemplate = $state("{host}_{yyyy}{MM}{dd}_{HH}{mm}{ss}.log");
  let logAppend = $state(true);

  function saveSessionLog() {
    void settingSet("session.log", {
      enabled: logEnabled,
      dir: logDir,
      template: logTemplate,
      append: logAppend,
    });
  }

  /**
   * M4a 终端内传输（settings 键 term.zmodem）。
   *
   * 默认**开**——与 session.log 的默认关相反。两者默认风险不同：转录是往用户
   * 磁盘上写他没要求的东西，传输是响应用户在终端里亲手敲的 `sz`/`rz`，不响应
   * 才是意外。fallback 与 Rust `ZmodemConfig::default` 逐字一致。
   */
  let zmodemEnabled = $state(true);
  let zmodemAutoReceive = $state(true);
  /** 4d：RDP 帧投递路径（rdp.frameTransport）。raw = tauri Channel 裸字节；
   *  event = 旧的 JSON+base64 事件。默认 raw；event 留着给真机 A/B 对比
   *  （FS_RDP_FRAMESTATS=1 的判据要求两条路径在同一构建里可比）与排查。 */
  let rdpFrameTransport = $state<"raw" | "event">("raw");
  /** 下载去向（2026-08-26 重设计）：null = 首次未决策；"fixed"/"ask"。 */
  let zmodemDlMode = $state<"fixed" | "ask" | null>(null);
  let zmodemDlDir = $state("");
  /** 下载目录选择器（与上传/保存位置共用 FilePickerDialog，独立实例）。 */
  let zmodemDirPicker = $state(false);

  function saveZmodem() {
    void settingSet("term.zmodem", {
      enabled: zmodemEnabled,
      autoReceive: zmodemAutoReceive,
      download:
        zmodemDlMode === null
          ? null
          : zmodemDlMode === "fixed"
            ? { mode: "fixed", dir: zmodemDlDir }
            : { mode: "ask", dir: "" },
    });
  }

  // M4a 高亮关键字规则（settings 键 term.highlights）
  let highlightRules = $state<HighlightRule[]>([]);

  async function saveHighlights(next: HighlightRule[]) {
    if (await settingSet("term.highlights", next)) highlightRules = next;
  }

  // M4a 键盘配置文件：用户键位覆盖表（settings 键 keyboard.bindings）
  let keyBindings = $state<KeyBindings>({});

  /** 保存键位：落库 + 即时生效（setKeyBindings 换掉运行期表，无需重启）。 */
  async function saveKeyBindings(next: KeyBindings) {
    if (!await settingSet("keyboard.bindings", next)) return;
    keyBindings = next;
    setKeyBindings(next);
    // 值是对象，settingSet 内部 JSON.stringify；后端 keyboard.bindings 规则校验形状

  }
  let clipEnabled = $state(true);
  let clipSeconds = $state(30);
  let hostKeyPolicy = $state<"tofu" | "strict">("tofu");
  let sandboxRoot = $state("");
  /**
   * 传后校验（UI 规格 §1.4）。§附表把这个开关的载体钉在 TransferQueueDrawer，本页是**第二处**
   * 控件，不是搬家——搬家会违反规格钉，而只留抽屉那一处则有一个可达性缺口：
   *
   * 抽屉整体包在 `{#if $rows.size > 0}` 里，本会话做出第一件传输之前它根本不渲染；而这个键是
   * 持久化的，后端 `sftp_cmd.rs` 又是在 `transfer_submit` **当刻**读它。于是上次会话把它关掉的
   * 用户，本次会话在传第一个文件之前没有任何入口能把它打开——等抽屉冒出来，作业已经按「关」
   * 建好了，这时再勾也不影响在飞的那一件。而"本次会话的第一件传输"恰恰是最需要它的那一件。
   * 默认是开，所以这不是数据事故，是一个**关掉之后就撤不回来**的安全开关——与 P1-16
   * （autoLockMinutes 只写不读）同族：界面承诺的可控性并不成立。
   *
   * 两处控件经 `settingChanged` 总线保持同步（抽屉侧订阅），不会各显各的值。
   */
  let verifyAfterTransfer = $state(true);

  /**
   * 回读全部已持久化的设置（审计 P2「设置项只写不读」）。
   *
   * 本组件是常驻挂载（App.svelte 里无 `{#if}` 包裹，`open` 只是 prop），故 onMount 一进程只跑
   * 一次；「写了不回读」的错位因此**不在同一次会话里显形，而在下次启动时**——控件显示的是
   * `$state` 初值，库里存的是上次写进去的值，两者从此各说各话。
   *
   * 后果最重的是 `vault.autoLockMinutes`（P1-16 刚把它接上后端巡检）：重启后下拉框显示「从不」，
   * 而 Vault 实际每 30 分钟自锁一次。更糟的是**用户点不动它**——select 的值本来就是「从不」，
   * 再选一次「从不」不产生 change 事件，onchange 不跑，settingSet 不调用，库里那个 30 原封不动。
   * 用户唯一的出路是先选 5 分钟再选从不（两次值变化），而界面上没有任何东西提示要绕这一步。
   *
   * 因此这里的规矩是：**凡在本文件里 settingSet 的键，必须在这里 settingGet 回来**，
   * 且 fallback 与上面对应 `$state` 的初值逐字一致（不一致等于换了一套默认值）。
   * 这条规矩由 SettingsDialog.test.ts 的结构性用例把守，新增控件漏了回读会直接红。
   */
  /* 危险动作豁免（M7.2 出口标准③）。读写与确认闸共用同一个设置键。 */
  let suppressed = $state<DangerousAction[]>([]);
  async function refreshSuppressed(): Promise<void> {
    suppressed = await loadSuppressed();
  }
  async function revokeSuppress(k: DangerousAction): Promise<void> {
    const next = withSuppressed(suppressed, k, false);
    if (await saveSuppressed(next) !== false) suppressed = next;
  }
  async function revokeAllSuppress(): Promise<void> {
    if (await saveSuppressed([]) !== false) suppressed = [];
  }

  onMount(async () => {
    void refreshSuppressed();
    // 对外 MCP：状态与指引在打开设置时拉一次；此后靠 mcp:status 事件实时更新
    // （连接/断开由 accept 循环推送，无需轮询）。监听器随组件常驻——设置对话框
    // 单实例、随应用存活，无泄漏窗口。
    void loadMcpStatus();
    void loadMcpGuide();
    unlistenMcpStatus = untilUnmount(listen<{ enabled: boolean; listening: boolean; port: number; connections: number }>(
      "mcp:status",
      (e) => {
        mcpStatus = e.payload;
        mcpEnabled = e.payload.enabled; // 与后端开关保持一致（比如另一处改了设置）
      },
    ));
    density = await settingGet<"compact" | "loose">("ui.density", "compact");
    // 只读本地：自己的版本号、用户填过的地址。**这里不做检查**——
    // 在 onMount 里检查等于「打开设置页即联网」，那与承诺相悖。
    appVersion = await invoke<string>("app_version").catch(() => "");
    updateUrl = await settingGet<string>("update.manifestUrl", "");
    aiMode = await settingGet<string>("ai.mode", "with_confirm");
    // MCP：默认关、空名单、缺省调用方标签（与 app/src/mcp 的缺省口径一致）。
    mcpEnabled = await settingGet<boolean>("mcp.enabled", false);
    mcpCaller = await settingGet<string>("mcp.caller", "mcp");
    mcpTools = await settingGet<string[]>("mcp.tools", []);
    mcpMounts = await settingGet<McpMount[]>("mcp.mounts", []);
    restoreUnclosed = await settingGet<boolean>("ui.restoreUnclosed", true);
    fontSize = await settingGet<number>("term.fontSize", 13);
    opacity = await settingGet<number>("term.opacity", 100);
    bellMode = await settingGet<"badge" | "badge+notify">("term.bell", "badge");
    scheme = await settingGet<string>("term.scheme", DEFAULT_SCHEME.id);
    // 形状不合规的条目在这里就滤掉：这些值最终进 xterm 的 theme 对象，
    // 一条坏数据会把整个终端渲染搞崩，而那种崩法（白屏/无字）用户无从判断原因。
    customSchemes = parseCustomSchemes(await settingGet<unknown>("term.customSchemes", []));
    rightClick = await settingGet<"paste" | "menu">("ui.rightClick", "paste");
    copyOnSelect = await settingGet<boolean>("ui.copyOnSelect", false);
    multilinePasteConfirm = await settingGet<boolean>("ui.multilinePasteConfirm", true);
    ctrlVPaste = await settingGet<boolean>("ui.ctrlVPaste", true); // R101/§2.12
    keyboardModeDefault = await settingGet<"remote" | "local">("keyboard.mode", "remote");
    hostKeyPolicy = await settingGet<"tofu" | "strict">("hostkey.defaultPolicy", "tofu");
    // 字符串而非数字：<option value="0"> 的 value 是字符串，回读成 number 会让 select 匹配不上
    // 任何一项而显示空白。后端 auto_lock_minutes 两种写法都认（JSON 串与裸串）。
    autoLock = await settingGet<string>("vault.autoLockMinutes", "0");
    // M4a 主机状态灯轮询间隔（0 = 关闭；字符串口径与 autoLock 同因：<option value> 恒为字符串）
    hostProbe = String(await settingGet<number>("sidebar.hostProbeSeconds", 30));
    // fallback 与上面 $state 初值逐字一致（本文件规矩），也与 Rust SessionLogConfig::default 同口径
    const slog = await settingGet<{ enabled: boolean; dir: string; template: string; append: boolean }>(
      "session.log",
      { enabled: false, dir: "", template: "{host}_{yyyy}{MM}{dd}_{HH}{mm}{ss}.log", append: true },
    );
    logEnabled = slog.enabled;
    logDir = slog.dir;
    logTemplate = slog.template;
    logAppend = slog.append;
    // M4a 终端内传输（fallback 与上面 $state 初值、与 Rust ZmodemConfig::default 三处一致）
    const zm = await settingGet<{
      enabled: boolean;
      autoReceive: boolean;
      download?: { mode: "fixed" | "ask"; dir: string } | null;
    }>("term.zmodem", { enabled: true, autoReceive: true, download: null });
    zmodemEnabled = zm.enabled;
    zmodemAutoReceive = zm.autoReceive;
    rdpFrameTransport = (await settingGet<string>("rdp.frameTransport", "raw")) === "event" ? "event" : "raw";
    zmodemDlMode = zm.download?.mode ?? null;
    zmodemDlDir = zm.download?.dir ?? "";
    // M4a 键位表：回读后即时套用（本对话框是常驻挂载，onMount 一进程一次）
    highlightRules = parseHighlightRules(await settingGet<string | null>("term.highlights", null));
    keyBindings = parseBindings(await settingGet<string | null>("keyboard.bindings", null));
    setKeyBindings(keyBindings);
    sandboxRoot = await settingGet<string>("sftp.sandboxRoot", "");
    // fallback 必须与 TransferQueueDrawer 的 onMount 首读、以及 sftp_cmd.rs 的
    // `settings_bool(…, true)` 三处逐字一致（走散 = 界面显示的、库里存的、后端实际用的是三个值）。
    verifyAfterTransfer = await settingGet<boolean>("transfer.verifyAfterTransfer", true);
    const clip = await settingGet<{ enabled: boolean; seconds: number }>(
      "security.clipboardClear", { enabled: true, seconds: 30 });
    clipEnabled = clip.enabled;
    clipSeconds = clip.seconds;
  });

  // 剪贴板定时清除：默认开、时长可配、整项可关（总设计 §3.2 / UI 规格 §2.12）
  function saveClipboardClear() {
    void settingSet("security.clipboardClear", { enabled: clipEnabled, seconds: clipSeconds });
  }

  // 密度即时生效：--fs-density 令牌（紧凑 1.2 / 宽松 1.5，§6）
  $effect(() => {
    document.documentElement.style.setProperty("--fs-density", density === "loose" ? "1.5" : "1.2");
  });

  function applyDensity(v: "compact" | "loose") {
    density = v;
    void settingSet("ui.density", v);
  }
</script>

{#if open}
  <div class="overlay" role="presentation" onclick={onClose}>
    <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
    <div
      bind:this={dialogEl}
      class="dialog"
      role="dialog"
      aria-modal="true"
      aria-label="选项"
      tabindex="-1"
      data-testid="settings-dialog"
      onclick={(e) => e.stopPropagation()}
      onkeydown={(e) => { if (e.key === "Escape") onClose(); }}
    >
      <header>
        <h2>选项（设置）</h2>
        <button class="x" aria-label="关闭" onclick={onClose}>×</button>
      </header>
      <p class="save-status" class:error={saveFailed} role="status" aria-live="polite" data-testid="settings-save-status">
        {pendingSaves ? "正在保存…" : saveState || "更改后自动保存；键位、高亮、配色和外部 MCP 列表需点击各自的保存按钮。"}
        {#if saveFailed}<button disabled={pendingSaves > 0} onclick={() => void retrySettings()}>重试保存</button>{/if}
      </p>
      <div class="body">
        <!-- S258：<nav> 隐含 navigation role，覆写为 tablist 属 ARIA 冲突（svelte-check a11y 告警），改用 div -->
        <div class="tabs" role="tablist" aria-label="设置分类" aria-orientation="vertical" use:tabNavigation>
          {#each TABS as t (t.id)}
            <button
              role="tab"
              aria-selected={tab === t.id}
              disabled={!t.enabled}
              title={t.note ?? ""}
              onclick={() => (tab = t.id)}
            >{t.label}{#if !t.enabled}<i>（{t.note}）</i>{/if}</button>
          {/each}
        </div>
        <section class="pane">
          {#if tab === "general"}
            <!-- §2.12 原写「Phase 4b；MVP 硬编码单语言简体中文，该项禁用」。M4b 到了，故解禁。
                 选项 label 用各语言自己的名字（简体中文 / English），不随界面语言翻译：
                 一个人误把界面切成看不懂的语言之后，要能认出母语才切得回来。 -->
            <label>{$t("settings.language")}
              <select
                value={$locale}
                data-testid="set-language"
                onchange={(e) => void changeLanguage(e.currentTarget.value)}>
                {#each SUPPORTED_LOCALES as l (l.id)}
                  <option value={l.id}>{l.label}</option>
                {/each}
              </select>
            </label>
            <p class="hint" data-testid="language-scope">{$t("settings.language.hint")}</p>
            {#if languageError}
              <p class="hint error" data-testid="language-error">{languageError}</p>
            {/if}
            <label>密度
              <select value={density} onchange={(e) => applyDensity(e.currentTarget.value as "compact" | "loose")}>
                <option value="compact">紧凑（行高 1.2）</option>
                <option value="loose">宽松（行高 1.5，触屏友好）</option>
              </select>
            </label>
            <label><input type="checkbox" bind:checked={restoreUnclosed} data-testid="set-restore-unclosed"
              onchange={() => void settingSet("ui.restoreUnclosed", restoreUnclosed)} /> 启动时询问恢复未关闭的会话</label>
          {:else if tab === "keyboard"}
            <label>键盘模式默认值
              <select bind:value={keyboardModeDefault} onchange={() => void settingSet("keyboard.mode", keyboardModeDefault)}>
                <option value="remote">远程（默认：将按键发送给当前会话）</option>
                <option value="local">本地（快捷键作用于应用 UI）</option>
              </select>
            </label>
            <p class="hint">此默认值用于新建会话。当前会话可点击状态栏「键盘」切换，也可按 Scroll Lock；临时切换不会改变默认值。</p>
            <label>右键行为
              <select bind:value={rightClick} onchange={() => void settingSet("ui.rightClick", rightClick)}>
                <option value="paste">粘贴（Xshell 习惯）</option>
                <option value="menu">上下文菜单</option>
              </select>
            </label>
            <label><input type="checkbox" bind:checked={copyOnSelect}
              onchange={() => void settingSet("ui.copyOnSelect", copyOnSelect)} /> 选择即复制（默认关，防误触）</label>
            <label><input type="checkbox" bind:checked={multilinePasteConfirm}
              onchange={() => void settingSet("ui.multilinePasteConfirm", multilinePasteConfirm)} /> 多行粘贴确认</label>
            <label><input type="checkbox" bind:checked={ctrlVPaste}
              onchange={() => void settingSet("ui.ctrlVPaste", ctrlVPaste)} /> Ctrl+V 粘贴（关则粘贴仅剩 Ctrl+Shift+V，Ctrl+V 原样发会话）</label>
            <hr class="sep" />
            <p class="hint">点击「改绑」后按下新组合键，再点击「保存键位」生效。支持导入、导出 JSON 配置。</p>
            <KeymapEditor user={keyBindings} onSave={saveKeyBindings} />
          {:else if tab === "terminal"}
            <label>默认配色方案
              <select bind:value={scheme} data-testid="set-scheme" onchange={() => void settingSet("term.scheme", scheme)}>
                {#each SCHEMES as s (s.id)}<option value={s.id}>{s.name}</option>{/each}
                {#if customSchemes.length > 0}
                  <!-- 分组显示：导入来的与随程序发布的不是一类东西，混在一列里
                       用户分不清哪些是自己导的（也就分不清删哪个是安全的）。 -->
                  <optgroup label="导入的配色">
                    {#each customSchemes as c (c.name)}
                      <option value={customSchemeId(c.name)}>{c.name}</option>
                    {/each}
                  </optgroup>
                {/if}
              </select>
            </label>
            <p class="hint">这是默认配色，修改后立即生效。会话属性中单独设置的配色优先；标签右键选择的临时配色优先级最高。</p>
            {#if customSchemes.length > 0}
              <!-- 导入的配色必须能删。没有这一块的话，用户从「文件 → 导入」导进来的东西
                   在界面上是只进不出的——这与更新地址那个输入框是同一条道理：
                   一个能被添加却不能被移除的东西，等于要求用户去改数据库。 -->
              <div class="section" data-testid="custom-schemes">
                <p class="hint">自定义配色（来自「文件 → 导入」或下面的编辑器）：</p>
                <ul class="custom-list">
                  {#each customSchemes as c (c.name)}
                    <li data-testid="custom-scheme-row">
                      <span class="swatch" style:background={c.background} style:color={c.foreground}>Aa</span>
                      <span class="cname">{c.name}</span>
                      <button type="button" class="link"
                        data-testid={`edit-custom-scheme-${c.name}`}
                        onclick={() => (editing = { target: c })}>编辑</button>
                      <button type="button" class="link"
                        data-testid={`export-custom-scheme-${c.name}`}
                        onclick={() => exportSchemes([c])}>导出</button>
                      <button type="button" class="link-danger"
                        data-testid={`delete-custom-scheme-${c.name}`}
                        onclick={() => void deleteCustomScheme(c.name)}>删除</button>
                    </li>
                  {/each}
                </ul>
                <button type="button" data-testid="export-all-schemes"
                  onclick={() => exportSchemes(customSchemes)}>导出全部为 JSON</button>
              </div>
            {/if}
            <!-- 新建入口在 `{#if}` 之外：一套配色都没有时它更该出现，而不是更不该。
                 放进去的话，用户第一次要造配色时会发现没有任何按钮。 -->
            <button type="button" data-testid="new-custom-scheme"
              onclick={() => (editing = { target: null })}>新建配色…</button>
            <p class="hint">
              导出的 JSON 可以从「文件 → 导入」原样读回来，也可以直接发给别人。
              保存后已打开的终端立即换色，不需要重启。
            </p>
            <label>全局终端字号（8–32）
              <input type="number" min="8" max="32" bind:value={fontSize}
                onchange={() => void settingSet("term.fontSize", fontSize)} />
            </label>
            <label>背景透明度（{opacity}%）
              <input type="range" min="0" max="100" bind:value={opacity}
                onchange={() => void settingSet("term.opacity", opacity)} />
            </label>
            <label>bell 行为
              <select bind:value={bellMode} data-testid="set-bell" onchange={() => void settingSet("term.bell", bellMode)}>
                <option value="badge">仅标签铃铛角标</option>
                <option value="badge+notify" disabled>角标 + 系统通知（即将推出）</option>
              </select>
            </label>
            <hr class="sep" />
            <p class="hint">匹配规则的终端输出会着色；勾选「提醒」时，标签也会显示提醒。点击「保存规则」后对新输出生效。</p>
            <HighlightEditor rules={highlightRules} onSave={saveHighlights} />
          {:else if tab === "update"}
            <!-- M4b：手动版本检查。本页的措辞与后端 update_check.rs 的承诺同源——
                 「本程序不会主动联网」不是一句 UI 文案，是这个功能的定义。 -->
            <p class="hint" data-testid="update-promise">
              本程序<strong>不会</strong>在后台检查更新，也不会自动下载或替换自己。
              只有你点下面的按钮时才会联网一次；地址不填就永远不联网。
            </p>
            <label>当前版本
              <output data-testid="update-current-version">{appVersion || "…"}</output>
            </label>
            <!-- title 里不能出现裸的 `{`：Svelte 会把它当成表达式插值的开始（实测编译报
                 js_parse_error）。故这句提示改用不带大括号的说法。 -->
            <label title="留空即永不联网。填一个返回 JSON 的地址，其中要有 version 字段（形如 0.5.0）。">
              更新检查地址（留空 = 不检查）
              <input
                type="url"
                placeholder="https://…/latest.json"
                data-testid="set-update-url"
                bind:value={updateUrl}
                onchange={saveUpdateUrl} />
            </label>
            {#if updateUrlError}
              <p class="hint error" data-testid="update-url-error">{updateUrlError}</p>
            {/if}
            <button
              type="button"
              data-testid="update-check-btn"
              disabled={updateChecking}
              onclick={runUpdateCheck}>
              {updateChecking ? "检查中…" : "检查更新"}
            </button>
            {#if updateResult}
              <p class="hint" data-testid="update-result">{updateResult.message}</p>
              <!-- 取到局部常量再用：`updateResult` 是 $state，TS 不把 `updateResult.url` 的
                   真值判断收窄到闭包里（回调可能在之后才跑，那时它已可为 undefined）。 -->
              {@const releaseUrl = updateResult.kind === "available" ? updateResult.url : undefined}
              {#if releaseUrl}
                <button type="button" data-testid="update-open-page" onclick={() => void openExternal(releaseUrl)}>
                  打开发布页（在浏览器里下载）
                </button>
              {/if}
            {/if}
          {:else if tab === "ai"}
            <!-- 三个方向、三组（2026-08-28 用户反馈「分不明白」后重排）：
                 ① AI 提供方——本程序作为客户端连出去（内嵌 AI）
                 ② 对外 MCP 服务——本程序作为服务端，外部 AI 客户端连进来
                 ③ 外部 MCP 挂载——本程序作为客户端连别的 MCP 服务器
                 每组标题都写明方向：「MCP」一个词同时出现在两组里，不写方向必然混淆。 -->
            <h4 class="grp" data-testid="ai-grp-provider">① AI 提供方（本程序 → 模型服务）</h4>
            <!-- M2 出口第 14 项：AI 执行总开关。措辞与 crates/ai 的承诺同源——
                 「不内置任何地址」不是一句 UI 文案，是这个功能的定义（零遥测那条门禁
                 只允许 crates/ai 有 HTTP 客户端，且目标地址一律由用户自己填）。 -->
            <p class="hint" data-testid="ai-promise">
              打开「AI 助手」（Ctrl+Shift+A）的配置页，填写服务地址、模型名称和 API Key。
              API Key 加密保存在保险库中。
            </p>
            <label title="关 = 自然语言转命令、输出解读、执行入口全部不可用（呈禁用态）">
              AI 执行档位
              <select
                bind:value={aiMode}
                data-testid="set-ai-mode"
                onchange={() => void settingSet("ai.mode", aiMode)}>
                <option value="disabled">关闭（全部 AI 功能不可用）</option>
                <option value="read_only">只读：只允许只读命令，改动类一律不给执行</option>
                <option value="with_confirm">需确认：改动类命令执行前逐条确认</option>
              </select>
            </label>
            <p class="hint">
              程序会根据命令内容检查风险。执行前请核对目标会话和命令；模型生成的建议可能有误。
            </p>

            <!-- ② 对外 MCP：本程序作为服务端。默认关闭；外部客户端经它操作会话，
                 与内置 Agent 同一道策略闸门，改动类逐条弹确认（§4.5 阻塞式人机回路）。 -->
            <h4 class="grp" data-testid="ai-grp-server">② 对外 MCP 服务（外部 AI 客户端 → 本程序）</h4>
            <p class="hint">
              开启后，Claude Desktop 等外部 MCP 客户端可以把本程序的终端、文件能力
              当成工具调用。下面的<strong>连接指引</strong>告诉外部客户端怎么配。
            </p>
            <label title="开 = 起一个只监听本机（127.0.0.1）的服务，即时生效">
              <input type="checkbox" bind:checked={mcpEnabled} data-testid="set-mcp-enabled"
                     onchange={(e) => void setMcpEnabled((e.currentTarget as HTMLInputElement).checked)} />
              允许外部 MCP 客户端连接
            </label>
            {#if mcpError}
              <p class="err" role="alert" data-testid="mcp-error">{mcpError}</p>
            {/if}

            {#if mcpEnabled}
              <!-- 在线状态：监听端口、客户端连接数（mcp:status 事件实时推送）。 -->
              <p class="mcp-status" data-testid="mcp-status">
                <span class="dot {mcpStatusText().cls}" aria-hidden="true"></span>
                {mcpStatusText().text}
              </p>

            {/if}
            <label title="用于在审计记录和确认框中标识调用方；此标签不验证客户端身份">
              MCP 调用方标签
              <input data-testid="set-mcp-caller" value={mcpCaller}
                     onchange={(e) => { mcpCaller = (e.currentTarget as HTMLInputElement).value.trim() || "mcp"; void settingSet("mcp.caller", mcpCaller); }} />
            </label>
            <p class="hint">
              授权给外部客户端的工具。<strong>没勾选的工具，对方连枚举都枚举不到</strong>；
              改动类（写入/发送/执行）调用时仍会逐条弹确认框。
            </p>
            <div class="mcp-tools" data-testid="set-mcp-tools">
              {#each MCP_TOOL_CHOICES as tool}
                <label>
                  <input type="checkbox" checked={mcpTools.includes(tool)}
                         onchange={() => toggleMcpTool(tool)} />
                  {MCP_TOOL_LABELS[tool]}（{tool}）
                </label>
              {/each}
            </div>

            <!-- 连接指引排在本组末尾：先配好授权，再拿配置去连。
                 它此前夹在开关与授权配置之间，134px 的代码块把一组
                 劈成两半，用户读到指引就以为这组结束了（2026-08-31 修）。 -->
            {#if mcpEnabled}
              <!-- 连接指引：可直接粘贴的 JSON 配置。用户此前「不知道怎么连」的
                   根源——开关开了之后没有任何下一步的提示。 -->
              {#if mcpGuide}
                <p class="hint">
                  在外部客户端（Claude Desktop / Cursor 等）的 MCP 配置里粘贴：
                </p>
                <pre class="mcp-guide" data-testid="mcp-guide">{mcpGuide.config}</pre>
                <div class="row">
                  <button type="button" data-testid="mcp-copy-guide"
                          onclick={() => void copyMcp(mcpGuide?.config ?? "", "配置")}>复制配置</button>
                  <button type="button" data-testid="mcp-copy-token"
                          onclick={() => void copyMcp(mcpToken, "访问令牌")}>复制 token</button>
                </div>
                <p class="hint" title="token 只在本机回环上校验；与本程序同用户的恶意软件本就能读设置库——这不是它挡得住的">
                  访问令牌：<code data-testid="mcp-token">{mcpToken}</code>
                  ——不知道它的本机进程连不上。
                  <button type="button" class="link" data-testid="mcp-regen"
                          onclick={() => void regenMcpToken()}>轮换</button>
                  （轮换后旧客户端要重新配置；已连接的会话保持到断开）
                </p>
              {:else}
                <p class="hint">正在生成连接指引…</p>
              {/if}
            {/if}

            <!-- ③ 外部挂载：本程序作为客户端。 -->
            <h4 class="grp" data-testid="ai-grp-mounts">③ 外部 MCP 挂载（本程序 → 其他 MCP 服务器）</h4>
            <p class="hint">
              挂载外部 MCP 服务器（stdio 子进程），供 Agent 作为额外工具调用。外部工具
              默认按「写」档起步、执行前逐次确认。<strong>启动命令由你填写</strong>——
              本程序不内置任何服务器地址。
            </p>
            <div class="mcp-mounts" data-testid="set-mcp-mounts">
              {#each mcpMounts as mount, i}
                <div class="mount-row">
                  <label>服务器标识<input aria-label="服务器标识" placeholder="服务器标识（唯一名称）" value={mount.server_id}
                         onchange={(e) => { mount.server_id = (e.currentTarget as HTMLInputElement).value.trim(); }} /></label>
                  <label>启动命令<input aria-label="启动命令" placeholder="启动命令（如 npx）" value={mount.command}
                         onchange={(e) => { mount.command = (e.currentTarget as HTMLInputElement).value.trim(); }} /></label>
                  <textarea aria-label="启动参数（每行一个）" placeholder="每行一个参数；路径含空格时也无需加引号" rows="3" value={mount.args.join("\n")}
                         onchange={(e) => { mount.args = e.currentTarget.value.split(/\r?\n/).filter(v => v.length > 0); }}></textarea>
                  <button type="button" onclick={() => removeMcpMount(i)}>删除</button>
                </div>
              {/each}
              <button type="button" data-testid="add-mcp-mount" onclick={addMcpMount}>添加外部 MCP 服务器</button>
              <button type="button" data-testid="save-mcp-mounts" onclick={() => void saveMcpMounts()}>保存服务器列表</button>
              <p class="hint">新增、修改或删除后请保存。启动参数每行一个，路径包含空格时无需加引号。</p>
            </div>
          {:else if tab === "security"}
            <!-- 危险动作的「以后不再显示」豁免（M7.2 出口标准③：勾了之后必须能反悔）。
                 逐条列出而不是给一个「清空全部」了事：用户可能只想把「重启服务」收回来，
                 而继续免确认「终止进程」——一个总清空按钮做不到这件事。 -->
            <fieldset class="confirm-suppress">
              <legend>危险动作确认</legend>
              {#if suppressed.length === 0}
                <span class="hint" data-testid="suppress-empty">没有已免确认的动作——所有危险动作都会先问你。</span>
              {:else}
                <ul data-testid="suppress-list">
                  {#each suppressed as k (k)}
                    <li>
                      <span>{actionLabel(k)}</span>
                      <button type="button" data-testid={`suppress-revoke-${k}`}
                              onclick={() => void revokeSuppress(k)}>恢复确认</button>
                    </li>
                  {/each}
                </ul>
                <button type="button" data-testid="suppress-revoke-all"
                        onclick={() => void revokeAllSuppress()}>全部恢复确认</button>
              {/if}
            </fieldset>
            <label>Vault 自动锁定
              <select bind:value={autoLock} data-testid="set-auto-lock" onchange={() => void settingSet("vault.autoLockMinutes", autoLock)}>
                <option value="0">从不</option>
                <option value="5">5 分钟</option>
                <option value="30">30 分钟</option>
              </select>
            </label>
            <label title="M4a 主机状态灯：对未连接档案做 TCP 探测的轮询间隔（0 = 关闭轮询）">
              主机状态灯轮询间隔
              <select bind:value={hostProbe} data-testid="set-host-probe" onchange={() => void settingSet("sidebar.hostProbeSeconds", Number(hostProbe))}>
                <option value="0">关闭</option>
                <option value="10">10 秒</option>
                <option value="30">30 秒</option>
                <option value="60">1 分钟</option>
                <option value="120">2 分钟</option>
                <option value="300">5 分钟</option>
              </select>
            </label>
            <label title="复制凭据后自动清除剪贴板，可调整等待时间或关闭">
              <input type="checkbox" bind:checked={clipEnabled} onchange={saveClipboardClear} /> 剪贴板定时清除
            </label>
            {#if clipEnabled}
              <label>剪贴板清除时长
                <select bind:value={clipSeconds} onchange={saveClipboardClear}>
                  <option value={15}>15 秒</option>
                  <option value={30}>30 秒（默认）</option>
                  <option value={60}>60 秒</option>
                  <option value={300}>5 分钟</option>
                </select>
              </label>
            {/if}
            <label>主机密钥默认策略
              <select bind:value={hostKeyPolicy} onchange={() => void settingSet("hostkey.defaultPolicy", hostKeyPolicy)}>
                <option value="tofu">TOFU（首次信任）</option>
                <option value="strict">严格（拒绝未知）</option>
              </select>
            </label>
            <label title="传输后比对 SHA-256；服务器不支持时改为比对文件大小，并标注「降级」。也可在传输队列中修改。">
              <input type="checkbox" bind:checked={verifyAfterTransfer} data-testid="set-verify-after-transfer-global"
                onchange={() => void settingSet("transfer.verifyAfterTransfer", verifyAfterTransfer)} /> 传后校验
            </label>
            <label>下载沙箱根目录
              <!-- R54：本键由 Task 21 download_sandbox_for 三级回落消费（Profile.sftp.download_sandbox → 本键/<profile_id> → 下载目录/<profile_id>），占位文案与实际缺省逐字一致 -->
              <input type="text" bind:value={sandboxRoot} data-testid="set-sandbox-root"
                placeholder="缺省：下载目录/<profile_id>；Profile 级覆盖全局"
                onchange={() => void settingSet("sftp.sandboxRoot", sandboxRoot)} />
            </label>
            <hr class="sep" />
            <!-- M4a 会话纯文本日志：默认关。转录把命令与输出明文落盘，
                 必须是显式选择——故开关文案直说这件事，不用「记录会话」这种
                 听起来无害的说法。 -->
            <label title="连接建立即把会话输出转录到文件（剥除颜色/控制序列，纯文本）">
              <input type="checkbox" bind:checked={logEnabled} data-testid="set-log-enabled"
                onchange={saveSessionLog} /> 会话日志转录（<b>含命令与输出明文</b>，默认关）
            </label>
            {#if logEnabled}
              <label>日志目录
                <input type="text" bind:value={logDir} data-testid="set-log-dir"
                  placeholder="缺省：数据目录/logs/sessions"
                  onchange={saveSessionLog} />
              </label>
              <label>命名模板
                <input type="text" bind:value={logTemplate} data-testid="set-log-template"
                  placeholder={"{host}_{yyyy}{MM}{dd}_{HH}{mm}{ss}.log"}
                  onchange={saveSessionLog} />
              </label>
              <p class="hint">
                占位符：{"{host}"} {"{user}"} {"{session}"} {"{yyyy}{MM}{dd}"} {"{HH}{mm}{ss}"}（时间为 UTC）。
                文件名中的路径分隔符与非法字符会被替换为下划线。
              </p>
              <label>写入模式
                <select bind:value={logAppend} data-testid="set-log-append" onchange={saveSessionLog}>
                  <option value={true}>追加（断线重连续写同一文件）</option>
                  <option value={false}>覆盖（每次连接重写）</option>
                </select>
              </label>
            {/if}
            <hr class="sep" />
            <!-- M4a 终端内传输：默认开。与上面的日志转录相反，理由是默认风险不同
                 ——转录是往用户磁盘上写他没要求的东西，传输是响应他亲手敲的
                 `sz`/`rz`。两个开关分开给：想要「别让远端单方面往我盘上写」的
                 用户关掉自动接收即可，上传仍然可用。 -->
            <label title="终端里执行 rz/sz 时自动识别 ZMODEM 传输">
              <input type="checkbox" bind:checked={zmodemEnabled} data-testid="set-zmodem-enabled"
                onchange={saveZmodem} /> 终端内传输（rz / sz）
            </label>
            {#if zmodemEnabled}
              <label title="关掉后仍可手动上传（rz），只是不再自动开始接收">
                <input type="checkbox" bind:checked={zmodemAutoReceive} data-testid="set-zmodem-auto"
                  onchange={saveZmodem} /> 自动接收远端 <code>sz</code> 发来的文件
              </label>
              <!-- 2026-08-26 两段式下载：去向与冲突策略。首次（null）显示引导态。 -->
              <div class="zm-download">
                <span class="zm-dl-label">下载保存到</span>
                {#if zmodemDlMode === null}
                  <span class="hint" data-testid="set-zmodem-dl-unset">首次下载时询问（选过之后在这里改）</span>
                {:else if zmodemDlMode === "fixed"}
                  <code data-testid="set-zmodem-dl-dir">{zmodemDlDir || "（主目录）"}</code>
                  <button data-testid="set-zmodem-dl-change" onclick={() => (zmodemDirPicker = true)}>更改…</button>
                  <button data-testid="set-zmodem-dl-ask"
                    onclick={() => { zmodemDlMode = "ask"; saveZmodem(); }}>改为每次询问</button>
                {:else}
                  <span data-testid="set-zmodem-dl-mode">每次下载时询问</span>
                  <button data-testid="set-zmodem-dl-fixed"
                    onclick={() => { zmodemDlMode = "fixed"; saveZmodem(); }}>改为固定目录</button>
                {/if}
              </div>
              <p class="hint">
                收到文件后如果目标位置已有同名文件，会问你要覆盖、保留两者（自动改名）还是跳过。
                不支持断点续传：对端请求续传时会明确取消。
              </p>
            {/if}
            <hr class="sep" />
            <!-- 4d：RDP 帧投递路径。raw（默认）走裸字节通道；event 走 JSON+base64 事件。
                 日常用 raw；event 留着做对比（FS_RDP_FRAMESTATS=1）与排查——两条路径
                 在同一构建里，切换后下次连接生效（通道是连接时建立的）。 -->
            <label title="RDP 画面的投递方式（下次连接生效）">
              RDP 帧传输
              <select bind:value={rdpFrameTransport} data-testid="set-rdp-transport"
                onchange={() => void settingSet("rdp.frameTransport", rdpFrameTransport)}>
                <option value="raw">裸字节通道（默认，快）</option>
                <option value="event">事件 + base64（对比/排查用）</option>
              </select>
            </label>
          {/if}
        </section>
      </div>
    </div>
  </div>
{/if}

<SchemeEditor
  open={!!editing}
  initial={editing?.target ?? null}
  takenNames={customSchemes.map((c) => c.name)}
  onSave={(s) => void saveScheme(s)}
  onCancel={() => (editing = null)}
/>

{#if zmodemDirPicker}
  <FilePickerDialog
    open={zmodemDirPicker}
    title="选择下载保存目录"
    mode="directory"
    confirmLabel="用这个目录"
    onConfirm={(paths) => {
      zmodemDirPicker = false;
      if (paths[0] !== undefined) {
        zmodemDlDir = paths[0];
        saveZmodem();
      }
    }}
    onCancel={() => (zmodemDirPicker = false)}
  />
{/if}

<style>
  .zm-download { display: flex; align-items: center; gap: 8px; margin: 4px 0 0 22px; flex-wrap: wrap; }
  .zm-dl-label { color: var(--fs-fg-secondary); }
  .zm-download code { word-break: break-all; color: var(--fs-accent, #6ea8fe); font-size: 11.5px; }
  .zm-download button { padding: 2px 10px; border: 1px solid var(--fs-border); background: var(--fs-bg-panel);
                        color: var(--fs-fg-primary); border-radius: 4px; cursor: pointer; font-size: 11.5px; }

  .overlay { position: fixed; inset: 0; z-index: 50; display: grid; place-items: center; background: rgba(0, 0, 0, 0.45); }
  .dialog { width: 720px; max-width: 92vw; height: 480px; max-height: 84vh; display: flex; flex-direction: column; background: var(--fs-bg-elevated); border: 1px solid var(--fs-border-strong); border-radius: 8px; box-shadow: var(--fs-shadow); }
  header { display: flex; justify-content: space-between; align-items: center; padding: 10px 14px; border-bottom: 1px solid var(--fs-border); flex: none; }
  header h2 { font-size: 14px; font-weight: 600; color: var(--fs-fg-primary); margin: 0; }
  .x { border: none; background: none; color: var(--fs-fg-secondary); font-size: 16px; cursor: pointer; }
  .save-status { margin: 0; padding: 8px 14px; font-size: 12px; color: var(--fs-fg-secondary); }
  .save-status.error { color: var(--fs-danger); }
  .body { flex: 1; display: flex; min-height: 0; }
  .tabs { width: 190px; flex: none; padding: 8px; border-right: 1px solid var(--fs-border); display: flex; flex-direction: column; gap: 2px; }
  .tabs button { display: flex; flex-direction: column; align-items: flex-start; gap: 1px; padding: 6px 10px; border: none; border-radius: var(--fs-radius); background: none; color: var(--fs-fg-primary); text-align: left; cursor: pointer; }
  .tabs button[aria-selected="true"] { background: var(--fs-accent-dim); color: var(--fs-accent); }
  .tabs button:disabled { color: var(--fs-fg-disabled); cursor: default; }
  .tabs i { font-style: normal; font-size: 10.5px; color: var(--fs-fg-disabled); }
  .pane { flex: 1; padding: 16px; overflow-y: auto; display: flex; flex-direction: column; gap: 12px; }
  .pane label { display: flex; align-items: center; gap: 8px; font-size: 12.5px; color: var(--fs-fg-primary); }
  .pane select, .pane input[type="text"], .pane input[type="number"], .pane input[type="url"], .pane input:not([type]) { background: var(--fs-bg-input); color: var(--fs-fg-primary); border: 1px solid var(--fs-border); border-radius: var(--fs-radius); padding: 3px 6px; font: inherit; }
  .pane select { padding-right: 22px; } /* 箭头让位（2026-09-01） */
  .mount-row { display: grid; grid-template-columns: minmax(0, 1fr) minmax(0, 1fr); gap: 8px; padding: 10px; border: 1px solid var(--fs-border); border-radius: 4px; margin-bottom: 8px; }
  .mount-row label { flex-direction: column; align-items: stretch; gap: 4px; min-width: 0; }
  .mount-row label input { flex: none; max-width: 100%; }
  .mount-row input { min-width: 0; width: 100%; }
  .mount-row textarea { grid-column: 1 / -1; width: 100%; min-width: 0; resize: vertical; background: var(--fs-bg-input); color: var(--fs-fg-primary); border: 1px solid var(--fs-border); border-radius: 4px; padding: 6px; font: inherit; }
  .mount-row button { justify-self: start; }
  /* 普通按钮（评审 P2-6：此前未覆盖，呈浏览器默认样式混进主题界面）。
   * 与 .pane 内的次级按钮一致：面板底、边框、hover 提亮。 */
  .pane > button, .pane button:not([class]) { background: var(--fs-bg-elevated); color: var(--fs-fg-primary); border: 1px solid var(--fs-border); border-radius: var(--fs-radius); padding: 3px 10px; cursor: pointer; font: inherit; }
  .pane > button:hover, .pane button:not([class]):hover { background: var(--fs-bg-hover); }
  .hint { font-size: 11.5px; color: var(--fs-fg-secondary); margin: 0; }

  /* ── 排版三修（2026-08-31 真机逐页核查后）────────────────────────────
   *
   * .pane 是纵向 flex。它带来三个**看得见**的坏，三处都不是审美偏好：
   *
   * ① `.sep` 此前**根本没有定义**——四处 `<hr class="sep">` 引用了一个
   *    不存在的类，于是 hr 在 flex 列里塌成 2px 宽的一小截，界面上是一个
   *    孤零零的点（真机测量：pane 518px 宽，hr 实测 2px）。它本该是横贯
   *    整栏的分隔线，用来把「组」分开——组分不开，正是「排版乱」的观感来源。
   * ② 按钮被 align-items: stretch 拉满整栏（实测「新建配色…」496/528px、
   *    「检查更新」同样）。一个按钮横跨整个面板，看起来像横幅而不是按钮，
   *    也让它和旁边的 label 行对不齐。改成按内容宽、左对齐。
   * ③ 单行输入框（如 MCP 调用方标签）在 label 内 flex 里会被内容顶宽，
   *    与其它行的控件左边缘对不上；给个上限并统一 flex 行为。
   *
   * 为什么用 :where() 收窄优先级：让组件里已有的更具体规则（比如
   * .hl 里的按钮、.row 里的按钮）仍然赢，不需要逐个加 !important。 */
  .pane hr.sep {
    align-self: stretch;   /* ① 不再被 flex 压缩，横贯整栏 */
    width: auto;
    height: 0;
    margin: 6px 0 2px;
    border: 0;
    border-top: 1px solid var(--fs-border);
  }
  .pane > :where(button) {
    align-self: flex-start; /* ② 按内容宽、左对齐，不再横跨整栏 */
  }
  .pane label > :where(input[type="text"], input:not([type])) {
    max-width: 260px;       /* ③ 与其它行的控件宽度同量级 */
    flex: 0 1 260px;
  }
  /* 保存被后端拒了的提示：必须与普通 hint 有色差，否则「填错了」会读起来像一句说明。 */
  .hint.error { color: var(--fs-danger, #e05252); }
  .section { margin: 8px 0 4px; }
  .custom-list { list-style: none; margin: 4px 0 0; padding: 0; display: flex; flex-direction: column; gap: 4px; }
  .custom-list li { display: flex; align-items: center; gap: 8px; font-size: 12px; }
  /* 色块用配色自己的前景/背景画，用户一眼看出哪套是哪套——只列名字的话，
     从 .xcs 导进来的十几套「Solarized 变体」在界面上完全无法分辨。 */
  .swatch { display: inline-grid; place-items: center; width: 34px; height: 20px; border: 1px solid var(--fs-border); border-radius: 3px; font-size: 11px; }
  .cname { flex: 1; }
  .link { background: none; border: none; color: var(--fs-accent, #6ea8fe); cursor: pointer; font-size: 11.5px; padding: 2px 4px; }
  .link-danger { background: none; border: none; color: var(--fs-danger, #e05252); cursor: pointer; font-size: 11.5px; padding: 2px 4px; }

  /* AI 区三组标题（2026-08-28 方向化重排） */
  .grp {
    margin: 1.2em 0 0.4em;
    font-size: 0.95em;
    color: var(--fs-fg-primary);
    border-bottom: 1px solid var(--fs-border);
    padding-bottom: 0.25em;
  }
  .grp:first-of-type { margin-top: 0; }

  /* MCP 在线状态点 */
  .mcp-status { display: flex; align-items: center; gap: 0.5em; margin: 0.4em 0; }
  .dot {
    width: 10px; height: 10px; border-radius: 50%;
    background: var(--fs-fg-disabled); /* off */
    flex: none;
  }
  .dot.on { background: #3fb950; }
  .dot.wait { background: #d29922; }

  /* 连接指引代码块（2026-08-31 修压扁 + 改换行）。
   *
   * 修的 bug：它住在 .pane（纵向 flex、内容超高出纵向滚动）里，而 flex
   * 规范对 overflow 非 visible 的子项把**自动最小高度归零**——整页收紧时
   * 只有它可压，就被压成一行高（14px），用户看到的是个"空框"。
   * `flex: none` 让它按内容自然高，超出交给 .pane 的纵向滚动。
   *
   * 换行代替横向滚动（用户裁定：只允许上下滚）：长路径行 pre-wrap 折行，
   * overflow-wrap: anywhere 兜住无空格的长 token（Program Files 之类
   * 能在空格处折，但整条无空格的路径折不了）。 */
  .mcp-guide {
    font-family: var(--fs-font-mono, monospace);
    font-size: 0.82em;
    background: rgba(127, 127, 127, 0.12);
    border: 1px solid var(--fs-border);
    border-radius: 6px;
    padding: 0.6em 0.8em;
    flex: none;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    margin: 0.4em 0;
  }
  .row { display: flex; gap: 0.5em; margin: 0.3em 0; }
  button.link {
    background: none; border: none; color: var(--fs-accent);
    padding: 0; cursor: pointer; text-decoration: underline;
  }
  .confirm-suppress { border: 1px solid var(--fs-border); border-radius: var(--fs-radius); padding: 8px 10px; display: flex; flex-direction: column; gap: 6px; }
  .confirm-suppress legend { font-size: 12px; color: var(--fs-fg-secondary); padding: 0 4px; }
  .confirm-suppress ul { list-style: none; margin: 0; padding: 0; display: flex; flex-direction: column; gap: 4px; }
  .confirm-suppress li { display: flex; align-items: center; justify-content: space-between; gap: 8px; font-size: 12.5px; }
</style>
