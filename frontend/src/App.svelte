<script lang="ts">
  import { onDestroy, onMount, untrack } from "svelte";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import MenuBar from "./components/MenuBar.svelte";
  import ToolBar from "./components/ToolBar.svelte";
  import ComposeBar from "./components/ComposeBar.svelte";
  import QuickBar from "./components/QuickBar.svelte";
  import QuickCommandsDialog from "./components/QuickCommandsDialog.svelte";
  import TunnelDialog from "./components/TunnelDialog.svelte";
  import ConnectionDetailDialog from "./components/ConnectionDetailDialog.svelte";
  import TextInputDialog from "./components/TextInputDialog.svelte";
  import StatusBar from "./components/StatusBar.svelte";
  import SettingsDialog from "./components/SettingsDialog.svelte";
  import TransferQueueDrawer from "./components/TransferQueueDrawer.svelte";
  import CloseConfirmDialog from "./components/CloseConfirmDialog.svelte";
  import DeleteConfirmDialog from "./components/DeleteConfirmDialog.svelte";
  import ForeignImportDialog from "./components/ForeignImportDialog.svelte";
  import KeyManagerDialog from "./components/KeyManagerDialog.svelte";
  import AiPanel from "./components/AiPanel.svelte";
  import AuditDialog from "./components/AuditDialog.svelte";
  import AgentPanel from "./components/AgentPanel.svelte";
  import SessionRecoveryDialog from "./components/SessionRecoveryDialog.svelte";
  import Sidebar from "./components/Sidebar.svelte";
  import TabBar from "./components/TabBar.svelte";
  import TerminalPane from "./components/TerminalPane.svelte";
  import RdpPane from "./components/RdpPane.svelte";
  import RdpCertDialog from "./components/RdpCertDialog.svelte";
  import MonitorPanel from "./components/MonitorPanel.svelte";
  import ZmodemBar from "./components/ZmodemBar.svelte";
  import HistoryDialog from "./components/HistoryDialog.svelte";
  import ScheduleDialog from "./components/ScheduleDialog.svelte";
  import ReplayDialog from "./components/ReplayDialog.svelte";
  import ConfirmDialog from "./components/ConfirmDialog.svelte";
  import { sendThenRecord } from "./lib/history";
  import { shouldAllowNativeContextMenu } from "./lib/context-menu";
  import { scheduleRunTick, shouldNotify, type ScheduleRunEvent } from "./lib/schedule";
  import {
    IDLE as ZMODEM_IDLE,
    reduce as reduceZmodem,
    type TransferView as ZmodemView,
    type ZmodemProgress,
  } from "./lib/zmodem";
  import ProfileDialog from "./components/ProfileDialog.svelte";
  import VaultDialog from "./components/VaultDialog.svelte";
  import ErrorDetailDialog from "./components/ErrorDetailDialog.svelte"; // 长错误看全（2026-09-01）
  import ConnectFailurePanel from "./components/ConnectFailurePanel.svelte"; // 连接失败带出路（路线图 4c，2026-09-01）
  import VaultManagerDialog from "./components/VaultManagerDialog.svelte";
  import AuthPromptDialog from "./components/AuthPromptDialog.svelte";
  import HostKeyDialog from "./components/HostKeyDialog.svelte";
  import McpConfirmDialog from "./components/McpConfirmDialog.svelte";
  import ActionConfirmDialog from "./components/ActionConfirmDialog.svelte"; // 危险动作统一确认（M7.2）
  import Toast from "./components/Toast.svelte";
  import type { Profile, ProfileListResult } from "./lib/types";
  import { ping, invoke, listen, openExternal, reportFrontendError, settingGet, settingSet, type UnlistenFn } from "./lib/ipc";
  import { untilUnmount } from "./lib/lifecycle";
  import { exportAllProfiles, exportOneProfile, importProfilesFromFile } from "./lib/profile-io";
  import { arrangeWindows, closeViewWindowFor, detachTab, openWindow } from "./lib/window-actions";
  import { broadcastEmptyHint, broadcastTargetIds, routeBroadcastKeystroke, toggleLiveBroadcast }
    from "./lib/broadcast-actions";
  import { toast } from "./lib/toast";
  import { restoreAction } from "./lib/session-restore";
  import { isPendingSessionId, openSession } from "./lib/open-session"; // 连接态三处一致：占位标签 → 转正（本文件无组件测试，故把可测部分整体外移，同 restoreAction 先例）
  import { statusTransition, type SessionStatusPayload } from "./lib/session-status";
  import { createCloseGate, decideClose } from "./lib/window-close"; // ×/Alt+F4 与菜单退出共用同一条确认路径（同上：可测部分外移）
  import { tabs, activeTabId, activateMruNext, activateMruPrev, removeTab, setTabStatus, setTabError } from "./lib/tabs"; // keyboardMode/errorText 取活动标签（F28/F29，per-session）；F25：窗口级仲裁读活动会话键盘模式（UI §2.12）；onReconnect 经 session_reconnect（F25⑤）。R107：本模块**只此一条** import，下方与 Task 20 一律在本句上补名，不得再写第二条 `from "./lib/tabs"`（同模块重复 import 致绑定重复声明，svelte-check 报错）
  import { composeTarget, liveBroadcast, pickedSessions, resolveTargetSessions, sendToSessions } from "./lib/broadcast"; // M4a 广播双通道（S305–S308）：目标解析与扇出在 lib，本文件只装配
  import {
    parseQuickCommands,
    quickCommandFromAi,
    type QuickCommand,
  } from "./lib/quick-commands"; // M4a 快速命令集/片段库；M4b AI 生成片段入库
  import { addFolder, hasProfilesUnder, normalizeFolderPath, parseFolders, removeFolder, renameFolder } from "./lib/folders"; // M4a 手动文件夹（S326）
  import {
    hostLights,
    startHostProbeLoop,
    type HostProbeLoop,
    HOST_PROBE_INTERVAL_DEFAULT_SECONDS,
  } from "./lib/host-lights"; // M4a 主机状态灯轮询（S311）
  import type { ComposeTarget } from "./lib/compose-history";
  import { get } from "svelte/store";
  import { tabsToClose } from "./lib/tab-close";
  import { initTransferStore, activeTransfers } from "./lib/transfers";
  import { resolveVaultToggle } from "./lib/vault";
  import EncodingDialog from "./components/EncodingDialog.svelte";
  import SerialBaudDialog from "./components/SerialBaudDialog.svelte"; // M7.4 改波特率（不重开端口） // M7.4 终端编码切换（不重连）
  import VWindowLayer from "./components/VWindowLayer.svelte"; // M7.3 虚拟窗口画布（铺在终端区之上）
  import { closeVWindowsForSession, openVWindow } from "./lib/vwindow";
  import { dropFrameChannel } from "./lib/rdp-frames"; // 4d：RDP 会话关闭时清帧通道
  import { dropForSession, queueForSession, takeForSession } from "./lib/pending-commands"; // M7.3 前台运行远端脚本：命令排队等新标签的终端就绪
  import { encodeB64, type TermController } from "./lib/term"; // 组合命令栏经 term_input 下发，与 TerminalPane 共用同一 UTF-8→base64 编码
  import { initTheme, setTheme } from "./lib/theme/store";
  import { initLocale } from "./lib/i18n";
  import {
    actionForKey, arbitrate, initKeyboardMode, isAlwaysLocal, keyboardModeDefault, toggleKeyboardMode,
  } from "./lib/shortcuts";
  import {
    composeVisible, initLayout, monitorOpen, persistSidebarWidth,
    quickbarVisible, setSidebarWidth, sidebarVisible, sidebarWidth, statusVisible, toggle, toolbarVisible,
    effectivePresentation, initResponsive, narrowScreen, setSidebarDock, sidebarDock, sidebarOverlayOpen, toggleSidebarFor,
  } from "./lib/layout";
  // S244：计划代码块此处另有 `import { clipboardWrite, clipboardRead } from "./lib/term";`——lib/term 属 Task 18 产物（镜像残留），
  // 本 Task 主体零使用（仅 Task 20 Step 4 ④ 消费），先行引入将致 npm run check 解析失败，故省略；Task 20 装配时随 ④ 一并补入。

  type SessionStatus = "connecting" | "connected" | "disconnected" | "error";

  let settingsOpen = $state(false);
  // M4b：从 Xshell / FinalShell / iTerm2 导入。菜单「文件 → 导入(Xshell/FinalShell 格式)」的落点。
  let foreignImportOpen = $state(false);
  // M4b：密钥/代理管理器（工具菜单）
  let keyManagerOpen = $state(false);
  // M3：审计链校验（工具菜单）
  let auditDialogOpen = $state(false);
  // M3：Agent 面板（工具菜单）
  let agentPanelOpen = $state(false);

  /* ── M2 AI 面板 ────────────────────────────────────────────────────────── */

  let aiPanelOpen = $state(false);

  /* ── RDP（阶段 1）────────────────────────────────────────────────────── */
  /** rdp_connect 带回的桌面尺寸（键 = sessionId）：RdpPane 的位图分辨率。 */
  let rdpSizes = $state<Map<string, { w: number; h: number }>>(new Map());
  function rdpSizeOf(id: string): { w: number; h: number } | undefined {
    return rdpSizes.get(id);
  }
  /** 活动标签是 RDP 时 AI 入口应呈禁用态（整个 AI 面板建立在「有文本终端可读写」上）。 */
  const activeTabIsRdp = $derived(
    $activeTabId ? ($tabs.find((t) => t.id === $activeTabId)?.kind === "rdp") : false,
  );
  /**
   * 选区右键「AI 解读」要解读的那段文本。
   *
   * 走 prop 交给面板、而不是 App 自己调 `ai_explain`：结果要显示在面板里，
   * 由 App 调命令再把结果传进去，等于把面板的状态拆成两处——
   * 而那两处迟早会出现「面板显示的是上一次的结果」。
   */
  let aiPendingSelection = $state<{ text: string; seq: number } | null>(null);
  let aiSelectionSeq = 0;
  /** 预填进「提问」框的问题（监控诊断那一路）。带 seq，同 aiPendingSelection。 */
  let aiPrefillPrompt = $state<{ text: string; seq: number } | null>(null);
  /**
   * 「填进命令栏」的载荷。
   *
   * 带 `seq`：同一条命令用户可能连点两次「填进命令栏」（第一次填完自己改坏了想重来），
   * 只给字符串的话第二次值没变、组合栏那边的 `$effect` 不重跑，看上去就是按钮坏了。
   */
  let composeFill = $state<{ text: string; seq: number } | null>(null);
  let composeFillSeq = 0;

  /**
   * 屏幕上下文取值口。
   *
   * 由 App 注入而不是让面板自己去拿：只有 App 握着 `termApis` 表，知道哪个标签是活动的。
   * 返回 `null` 表示「没有活动终端」——那与「终端是空屏」不同，后者该发一个空上下文，
   * 前者根本没有上下文可发，`fs_ai` 那边据此完全不带屏幕段。
   */
  function aiScreenContext(): string | null {
    return activeTermApi()?.controller()?.visibleText() ?? null;
  }

  /**
   * 监控 × AI 联动（M4b）：异常指标一键诊断。
   *
   * 走的是 NL→命令那条路（`ai_suggest_command`），不是解读那条。
   * 理由是出口原文要求「走 fs_policy 同闸门」——而解读不产出命令、
   * 也就没有闸门可过。诊断的产出本来就是「跑哪条命令去看」，
   * 那条命令要过和别处完全相同的裁决。
   */
  function diagnoseFromMonitor(prompt: string): void {
    aiPendingSelection = null;
    aiPrefillPrompt = { text: prompt, seq: ++aiSelectionSeq };
    aiPanelOpen = true;
  }

  /**
   * AI 建议 → 片段库（M4b 出口第 2 项后半）。
   *
   * 裁决在 `quickCommandFromAi` 里**再核一遍**，不信任「按钮没禁用所以能存」：
   * disabled 是外观，而这条命令会以一个可一键下发的按钮的形式长期留在库里。
   *
   * 四种拒绝各说各的原因。一句笼统的「保存失败」会让用户重试——
   * 而重试对这四种里的任何一种都不会成功。
   */
  async function saveAiSnippet(command: string, decision: string): Promise<void> {
    const r = quickCommandFromAi(command, decision, quickCommands);
    if (!r.ok) {
      const why = {
        empty: "命令是空的",
        "too-long": "命令超过 2048 字符——截断会得到另一条命令，所以不截",
        denied: "当前档位不允许执行这条命令，也就不该把它存成一键按钮",
        duplicate: "库里已有逐字相同的一条",
      }[r.reason];
      toast.warn(`没存进片段库：${why}`);
      return;
    }
    if (!await saveQuickCommands([...quickCommands, r.item])) return;
    toast.info(`已存进片段库的「AI」分类：${r.item.name}`);
  }

  /** AI 建议 → 组合命令栏。**只填不发**：替用户按回车是这个功能里最不该有的一步。 */
  function fillCompose(cmd: string): void {
    // 组合栏可能是隐藏的（§2.6 可切换）。不先打开的话，命令填进了一个看不见的框，
    // 用户只会看到面板关掉、什么都没发生。
    composeVisible.set(true);
    composeFill = { text: cmd, seq: ++composeFillSeq };
    aiPanelOpen = false;
  }

  /**
   * 侧栏最终的呈现形态（M4b 第 16、18 项）：停靠偏好 + 屏宽仲裁的结果。
   *
   * 窄屏一律覆盖，但**不改写** `sidebarDock`——用户的偏好留着，窗口拉宽后自动回到
   * 他选的那一侧。把窄屏写回偏好会让「拉窄一次就永久变成浮动」，而他没做过这个选择。
   */
  const sidebarPresentation = $derived(effectivePresentation($sidebarDock, $narrowScreen));
  /** 打开选项页时的目标页签（菜单「高亮关键字…」等直达入口用；关闭即复位）。 */
  let settingsTab = $state<string | null>(null);
  /** M4a 隧道管理器对话框（隧道是会话级设施，故只在有活动会话时能新建）。 */
  let tunnelDialogOpen = $state(false);
  /** M4a 连接详情弹层（状态栏状态灯点击；UI 规格 §2.7）。 */
  let connDetailOpen = $state(false);
  /** M4a 历史命令面板（工具菜单 / Ctrl+Shift+H）。 */
  let historyOpen = $state(false);
  /** M4a 计划任务面板（工具菜单）。 */
  let scheduleOpen = $state(false);
  /** M4a 会话回放面板（工具菜单）。 */
  let replayOpen = $state(false);

  /**
   * M4a 会话录屏：标签右键的「录制/停止录制」按当前状态切换。
   *
   * 开录前确认一次「含输出明文」——录屏把命令与回显原样落盘，与 sessionlog
   * 同一隐私口径；用户按下录制就是给了 consent，停/再录不再问。
   */
  let recordConfirmFor: string | null = $state(null);
  async function toggleRecording(sessionId: string) {
    try {
      const st = await invoke<{ recording: boolean }>("recording_status", { sessionId });
      if (st.recording) {
        await invoke("recording_stop", { sessionId });
        toast.info("录制已结束，可在「工具 → 会话回放」中打开");
      } else {
        recordConfirmFor = sessionId; // 确认后真正开录（见 ConfirmDialog）
      }
    } catch (e) {
      toast.error(`录制切换失败：${e}`);
    }
  }
  async function startRecordingConfirmed() {
    const id = recordConfirmFor;
    recordConfirmFor = null;
    if (!id) return;
    try {
      const path = await invoke<string>("recording_start", { sessionId: id });
      toast.info(`录制中：${path}`);
    } catch (e) {
      toast.error(`开录失败：${e}`);
    }
  }
  /**
   * M4a 终端内传输（rz/sz）的每会话传输态，按 session_id 归档。
   *
   * 按会话存而不是只留一份「当前」：传输是长活儿，用户会在传输期间切到别的标签
   * 干活。只留一份的话切回来进度就没了，看起来像传输被中断。
   */
  let zmodemViews = $state<Record<string, ZmodemView>>({});
  /** M4a 手动新建的文件夹（settings 键 sidebar.folders）。空文件夹靠它存在——
   *  MVP 期分组纯由 group_path 派生，建完立刻消失。 */
  let manualFolders = $state<string[]>([]);

  async function loadFolders(): Promise<void> {
    manualFolders = parseFolders(await settingGet<string | null>("sidebar.folders", null));
  }

  async function saveFolders(next: string[]): Promise<void> {
    manualFolders = next;
    try {
      if (await settingSet("sidebar.folders", next) === false) return;
    } catch (e) {
      toast.error(`文件夹保存失败（本次会话内生效，重启后回退）：${e}`);
    }
  }

  /** 新建文件夹的输入态（非 null = 模态开着）。
   *  用自建模态而非原生 `prompt()`：后者同步阻塞整个 webview（期间终端输出只进
   *  队列不渲染）、在 jsdom 下返回 undefined 使该分支不可测、且无焦点陷阱与主题——
   *  与 DeleteConfirmDialog 换掉原生 confirm() 是同一组理由。 */
  let newFolderOpen = $state(false);

  /** 菜单「新建文件夹」：开输入模态（路径用 / 分层建多层）。 */
  function newFolder(): void {
    newFolderOpen = true;
  }

  async function confirmNewFolder(raw: string): Promise<void> {
    newFolderOpen = false;
    const { list, error } = addFolder(manualFolders, raw);
    if (error) {
      toast.warn(error);
      return;
    }
    await saveFolders(list);
  }

  /** 删除手动文件夹：**不动任何会话**（删目录不该连带删连接配置）。
   *  若路径下仍有会话，节点会继续由派生路径撑着——如实告知，别让用户以为没删掉。 */
  async function deleteFolder(path: string): Promise<void> {
    const next = removeFolder(manualFolders, path);
    await saveFolders(next);
    if (hasProfilesUnder(path, profiles.map((p) => p.group_path))) {
      toast.info(`「${path}」下仍有会话，目录仍会显示（删除文件夹不会删除其中的连接）`);
    }
  }

  /** 重命名手动文件夹的输入态（非 null = 模态开着，值是被改名的原路径）。 */
  let renameFolderFrom = $state<string | null>(null);

  /**
   * 重命名手动文件夹（M1 出口原文「新建 / 重命名 / 删除」的那一半，2026-08-22 补）。
   *
   * 与删除的关键差别：删除刻意不动会话，而重命名必须动——group_path 是会话归属的
   * 唯一依据，只改清单不改会话，那些会话会指向一个已不存在的名字，于是树里由派生
   * 路径撑出一个「本该被改名的旧目录」，用户看到的是「改了名字反而多出一个」。
   *
   * 落库顺序：先改会话再存清单。反过来的话，若中途失败，清单已是新名而会话还在旧名下，
   * 那批会话立刻成为孤儿；先改会话则最坏是「会话已迁、清单没存住」，重启后清单回退，
   * 而派生路径会把新目录撑出来——仍然自洽。
   */
  async function renameFolderTo(raw: string) {
    const from = renameFolderFrom;
    renameFolderFrom = null;
    if (!from) return;
    const { list, moves, error } = renameFolder(
      manualFolders,
      from,
      raw,
      profiles.map((p) => p.group_path),
    );
    if (error) {
      toast.warn(error);
      return;
    }

    // 先迁会话：逐条改 group_path 并存库
    const moveMap = new Map(moves.map((m) => [m.from, m.to]));
    let moved = 0;
    for (const p of profiles) {
      const gp = (p.group_path ?? "").trim();
      const to = moveMap.get(gp);
      if (!to) continue;
      try {
        await invoke("profile_save", { profile: { ...p, group_path: to } });
        moved += 1;
      } catch (e) {
        // 部分失败要如实说：已迁的留在新名下、没迁的还在旧名下，两个目录都会显示；
        // 沉默会让用户以为「改名只生效了一半」是界面 bug。
        toast.error(`「${p.name}」的目录归属未能更新：${e}`);
      }
    }
    await saveFolders(list);
    if (moved > 0) await loadProfiles();
  }

  let status = $state("starting…");
  let sidebarDrag = $state(false);

  // Task 20: Profile 和 Vault 对话框状态
  let profileDialogOpen = $state(false);
  // 类型从 `any` 收紧到 `Profile | null`：`any` 会把 types.ts 的整套线格式约束就地作废，
  // 下面 profiles_list 的返回形状变更之所以能在无人察觉的情况下把侧栏打空，正是因为这里是 any。
  let profileDialogInitial = $state<Profile | null>(null);
  /** ProfileDialog 打开时直达的页；只有失败面板的「配置认证…」设它，其它入口一律 null（常规页）。 */
  let profileDialogTab = $state<"auth" | null>(null);
  let vaultDialogOpen = $state(false);
  /** 完整错误对话框：状态栏那条被截断的报错点开后落这里。 */
  let errorDetailOpen = $state(false);
  let errorDetailText = $state("");
  let vaultDialogMode = $state<"setup" | "unlock">("unlock");
  let vaultManagerOpen = $state(false);

  // Task 20: profiles 列表和 session 状态映射
  let profiles = $state<Profile[]>([]);
  /** `profiles_list` 报回的**读不出来的行**（Rust `ProfileListResult.badRows`）。
   *  必须一路带到 Sidebar 呈现：丢弃它等于把「我那条生产机配置凭空消失了」变成无声故障。 */
  let profileBadRows = $state<ProfileListResult["badRows"]>([]);
  /**
   * 侧栏状态灯数据源：**由 tabs store 派生**，不再是一份平行维护的 $state。
   *
   * 原实现是 `$state<Map>` + `sessionStates.set(profile.id, {status:"connecting"})`，
   * 而全仓只有 openSession 那一处写入方：三个事件桥（status/disconnected/closed）只更新
   * tabs、从不更新这张表，于是侧栏灯**四态里只有 connecting 一态可达**——连上不转绿、
   * 断线不转灰、重连失败不转红，一路蓝闪到标签被关掉为止。
   * 平行的第二份状态迟早会与第一份分叉；这里让它没有分叉的余地：唯一写入方是 tabs。
   * 同一 Profile 开多个标签时按最后一个赢（与原 Map 按 profileId 覆盖的行为一致）。
   */
  const sessionStates = $derived(
    new Map<string, { status: SessionStatus; sessionId: string }>(
      $tabs.map((t) => [t.profileId, { status: t.status as SessionStatus, sessionId: t.id }]),
    ),
  );

  // ── M4a 主机状态灯轮询 ────────────────────────────────────────────────────
  // 只探测**未连接**档案（已连接的由 sessionStates 呈现——连接是最强可达证明）。
  // 间隔可配（settings 键 sidebar.hostProbeSeconds，默认 30s）；失败静默降级
  // （host-lights 内 console.warn，UI 不弹框）。
  let hostProbeSeconds = $state(HOST_PROBE_INTERVAL_DEFAULT_SECONDS);
  void settingGet<number>("sidebar.hostProbeSeconds", HOST_PROBE_INTERVAL_DEFAULT_SECONDS).then((v) => {
    if (Number.isFinite(v) && v >= 0) hostProbeSeconds = v;
  });
  /** 当前轮询器句柄（普通变量、不进响应图）：下面第二个 effect 用它在档案列表变化时补探一轮。 */
  let hostProbeLoop: HostProbeLoop | null = null;
  $effect(() => {
    if (hostProbeSeconds <= 0) return; // 0 = 关闭轮询（设置页可选）
    // 本 effect 只依赖 hostProbeSeconds：targets 里的 profiles / sessionStates 在微任务里才被读到
    //（lib/host-lights.ts 的 A1 说明），所以会话开关不会再重建定时器。
    const stop = startHostProbeLoop(
      () =>
        profiles
          .filter((p) => !sessionStates.has(p.id))
          .map((p) => ({ profileId: p.id, host: p.host, port: p.port })),
      hostProbeSeconds,
    );
    hostProbeLoop = stop;
    return () => {
      stop();
      hostProbeLoop = null;
    };
  });
  // 档案列表一变（首次载入 / 新建 / 删除 / 导入）立刻补探一轮，而不是等下一个 30s 刻——
  // 否则启动后半分钟侧栏一颗灯都没有（2026-09-02 新包真机实测 25 秒仍无灯）。
  // 刻意只依赖 profiles、不依赖 sessionStates：会话开关引发的全量重探正是 A1 要去掉的流量尖峰。
  $effect(() => {
    void profiles;
    hostProbeLoop?.poke();
  });

  /** 认证 / 主机密钥两个对话框的实例引用：仅用于 `vault:locked` 时调其 dismissForVaultLock()。
   *  两者自行 listen 后端事件、自行回传 promptId，除此之外不需要任何 props。 */
  let authDlg = $state<AuthPromptDialog | undefined>();
  let hostKeyDlg = $state<HostKeyDialog | undefined>();

  // Task 22 Step 4: 关闭确认与启动恢复对话框状态
  let closeConfirmState = $state<{ scope: "window" | "tab" | "tabs"; sessions: string[]; queued: number; running: number; onConfirm: () => void } | null>(null);
  /**
   * 删除连接的确认态（形状照抄上面的 closeConfirmState：状态里带 onConfirm 闭包，挂载点只负责调它）。
   *
   * 此前这里走的是原生 `confirm()`，而 DeleteConfirmDialog.svelte 写好了却全仓无人引用。
   * 换掉它不是为了统一观感，是因为原生确认框在这条**不可撤销**的路径上默认方向是反的：
   * 原生 confirm 的默认按钮是「确定」，回车即删；本组件的焦点陷阱把初始焦点给「取消」
   * （见其内部注释「破坏性操作不默认选中」）。用户按惯性敲回车时，两者的后果正好相反。
   * 另有两条：`confirm()` 同步阻塞整个 webview（期间终端输出事件只进队列不渲染），
   * 且在 jsdom 下返回 undefined —— 组件测试里这条删除分支永远走不到，等于不可测。
   */
  let deleteConfirmState = $state<{ name: string; onConfirm: () => void } | null>(null);
  let restoreDialogOpen = $state(false);
  let unclosedRows = $state<Array<{ session_key: string; profile_id: string | null; updated_at: string }>>([]);

  // —— StatusBar 动态段数据源（R4 对偶；接线归属见注；占位初值，Task 20 Step 4 ② 整体替换接线） ——
  // activeStatus/activeHost：由 lib/tabs.ts 活动标签 status/host 派生（Task 20 组 H：Tab 接口扩 status/host + 三事件桥）
  // activeRows/activeCols：由 TerminalPane resize 回调上报 store（Task 18 组 G）
  // vaultLocked：Task 20 组 H 解锁/锁定（vault_status/vault_lock）后翻转；默认 true=锁定
  let activeStatus = $state<SessionStatus | null>(null);
  let activeHost = $state<string | null>(null);
  let activeRows = $state(24);
  let activeCols = $state(80);
  /** 活动会话的终端编码（M7.4）。由后端回报的规范名，不是用户填的标签。 */
  let activeEncoding = $state("UTF-8");
  let encodingDialogOpen = $state(false);
  /** 活动串口会话的参数（M7.4）。非串口标签为 null——状态栏据此决定主机段可不可点。 */
  let activeSerial = $state<{ port: string; baud: number; summary: string } | null>(null);
  let serialBaudOpen = $state(false);
  let vaultLocked = $state(true);
  // Caps/Num 锁键态：窗口键事件经 getModifierState 刷新（UI 规格 §2.7）
  let capsOn = $state(false);
  let numOn = $state(false);

  /**
   * 每个终端标签的能力句柄，由 TerminalPane 构建完成时经 onReady 回调登记（键 = sessionId = tab.id）。
   *
   * 这张表是「菜单/工具栏/快捷键 → 当前活动终端」的**唯一**通路。此前它整个不存在：
   * TerminalPane 一直在调 onReady，而 App 的挂载点从没传过这个 prop，于是下列入口全部
   * 点了无事、按了无声——菜单「编辑」整组（复制/粘贴/复制为纯文本/全选/查找/查找下一个/
   * 清除屏幕/清除滚动缓冲）、「查看→字体放大/缩小」、工具栏上三个未置灰的按钮
   * （复制/粘贴/查找）、以及 Ctrl+F、F3、Ctrl+L、终端未获焦时的 Ctrl+= / Ctrl+- / Ctrl+0。
   * 其中 F3 与 Ctrl+Tab 在全仓找不出第二个触发点，即功能整体不可达。
   *
   * 用普通 Map 而非 $state：只在事件回调里读，不参与渲染；做成响应式反而会让每次登记都
   * 触发一轮无谓的重渲。
   */
  type TermApi = {
    openSearch(): void;
    findNext(): void;
    copySelection(): void;
    pasteFromClipboard(): void;
    controller(): TermController | undefined;
  };
  const termApis = new Map<string, TermApi>();
  /** 派发目标恒为**活动**标签：菜单是全局的，而终端是每标签一个 */
  const activeTermApi = (): TermApi | undefined => ($activeTabId ? termApis.get($activeTabId) : undefined);

  // 标签关掉后清掉它的句柄。不清不会立刻出错（查表恒按 activeTabId，闭标签查不到），
  // 但 controller() 指着已 dispose 的终端，且 replaceTabId 会让 id 变动——留着就是一张
  // 只增不减、还可能被复用 id 命中的陈旧表。
  $effect(() => {
    const live = new Set($tabs.map((t) => t.id));
    for (const id of termApis.keys()) if (!live.has(id)) termApis.delete(id);
  });

  // 派生状态：从活动标签同步 status 和 host
  $effect(() => {
    const activeTab = $tabs.find(t => t.id === $activeTabId);
    if (activeTab) {
      activeStatus = activeTab.status as SessionStatus;
      activeHost = activeTab.host;
    } else {
      activeStatus = null;
      activeHost = null;
    }
    // 编码随活动标签走：每条会话各有各的（一个连 GBK 板子、一个连 UTF-8 服务器是常态）。
    // 取不到就回落 UTF-8 并且**不报错**——占位标签/RDP 标签本来就没有终端管道。
    const id = activeTab?.id ?? null;
    void untrack(() =>
      (async () => {
        if (!id) { activeEncoding = "UTF-8"; return; }
        try {
          activeEncoding = await invoke<string>("term_encoding", { sessionId: id });
        } catch {
          activeEncoding = "UTF-8";
        }
        // 串口参数：只有串口标签才有。取不到就当不是串口——占位标签与刚断开的会话
        // 都会走到这里，报错没有意义。
        if (activeTab?.kind === "serial") {
          try {
            activeSerial = await invoke<{ port: string; baud: number; summary: string }>(
              "serial_params",
              { sessionId: id },
            );
          } catch {
            activeSerial = null;
          }
        } else {
          activeSerial = null;
        }
      })(),
    );
  });

  /**
   * 应用级订阅的解绑函数集（P1-11）：会话三事件桥 + 传输 store。
   * 组件卸载后仍在册的监听器会继续改 tabs store（HMR / 多窗口场景下叠加注册），必须在 onDestroy 全部解绑。
   */
  let appUnlisteners: Array<() => void> = [];

  /** listen 返回 Promise<UnlistenFn>，直接把 Promise 塞进解绑表会在 onDestroy 里「调用一个 Promise」而静默失败；
   *  故一律经此登记。实现走 lib/ipc 的 untilUnmount（路线图 4c A2）：解绑函数**同步**入表，
   *  onDestroy 快于 listen 落地时也会在落地当刻就地解绑——不再靠一个 appMounted 标志与 then 回调赛跑。 */
  function trackUnlisten(p: Promise<UnlistenFn>): void {
    appUnlisteners.push(untilUnmount(p));
  }

  /**
   * 会话状态桥（P1-11）：此前 App 只在 openSession 里建了 connecting 标签，却无人订阅后端的会话事件——
   * 标签永远停在「连接中」，远端正常 exit 后标签也不消失。三事件与 app 层 emit 处逐字对偶：
   * - session:status{session_id, message, state?} → 仅当 state="connected"（重连成功）才迁移；
   *   其余是连接过程进度文案，只 toast 不迁移（原实现无条件判 connected，见 session-status.ts）
   * - session:disconnected{session_id, reason, attempt, gave_up?} → disconnected；
   *   gave_up=true 是重连循环的终态（达上限或 profile 取不到），此时置 error 并写文案，
   *   否则标签停在灰点而 banner 已不再刷新，用户看不出「已经不再重试了」
   * - session:closed{session_id, reason}      → 移除标签（user_closed / remote_exit 两来源）
   * 注意载荷键是 snake_case 的 session_id（app 层 serde_json::json! 手写键，非 camelCase 转换路径）。
   */
  function bridgeSessionEvents(): void {
    trackUnlisten(listen<SessionStatusPayload>("session:status", (e) => {
      const { session_id, message } = e.payload;
      // 只有后端显式标了 state:"connected" 才是状态迁移；其余是连接过程的进度文案，
      // 拿它们判「已连接」会在重连期把一个还没连上（甚至刚被拒连）的标签点绿。详见 session-status.ts。
      const next = statusTransition(e.payload);
      if (next) setTabStatus(session_id, next);
      if (message) toast.info(message);
    }));
    trackUnlisten(listen<{ session_id: string; gave_up?: boolean }>("session:disconnected", (e) => {
      const { session_id, gave_up } = e.payload;
      if (gave_up) setTabError(session_id, "重连失败，已停止自动重连");
      else setTabStatus(session_id, "disconnected");
    }));
    trackUnlisten(listen<{ session_id: string }>("session:closed", (e) => {
      // 侧栏灯随 tabs 派生（见 sessionStates），移除标签即同时熄灯——不再有第二处要清的表。
      removeTab(e.payload.session_id);
      // 排队中的脚本命令同理：会话在终端建起来之前就没了，那条命令再没有归属，
      // 留着会在下一个同 id 的会话上突然执行（M7.3）。
      dropForSession(e.payload.session_id);
      // RDP 帧通道同理（4d）：会话没了，通道与它缓存的最后一帧都没有归属了。
      dropFrameChannel(e.payload.session_id);
      // 虚拟窗口同理：会话没了，窗口里那个文件面板只会对着死会话刷错误（M7.3）。
      closeVWindowsForSession(e.payload.session_id);
      // 会话没了，它的传输态也没了：留着会让新开的同名标签一上来就显示上一次的
      // 传输结果条（后端拦截器已随会话作废，前端这条纯粹是幽灵）。
      delete zmodemViews[e.payload.session_id];
      zmodemViews = zmodemViews;
    }));
    // M4a 计划任务：执行结果。**只对 failed 弹提示**——ok 会被每分钟一条的任务刷爆，
    // skipped 最常见的原因是「那台机器没连着」而那是产品声明过的边界，反复提醒是噪音；
    // 只有 failed 是用户不看提示就永远不会知道的（见 lib/schedule.ts 的 shouldNotify）。
    trackUnlisten(listen<ScheduleRunEvent>("schedule:run", (e) => {
      const p = e.payload;
      if (shouldNotify(p.outcome)) {
        toast.error(`计划任务「${p.name}」执行失败：${p.detail || `exit ${p.exitCode ?? "?"}`}`);
      }
      // 面板订阅此计数刷新（列表的「上次触发」与执行记录都变了）
      scheduleRunTick.update((n) => n + 1);
    }));
    // M4a 终端内传输（rz/sz）：按会话归约进度事件。载荷键是 camelCase 的 sessionId
    // （后端 emit_progress 单点发出，键集恒定）——与上面三条 snake_case 的不同源。
    trackUnlisten(listen<ZmodemProgress>("zmodem:progress", (e) => {
      const id = e.payload.sessionId;
      zmodemViews[id] = reduceZmodem(zmodemViews[id] ?? ZMODEM_IDLE, e.payload);
      zmodemViews = zmodemViews;
    }));
  }

  /**
   * Vault 自动锁定桥（审计 P1-16 的前端半边）。
   * 后端 `spawn_auto_lock_watcher` 闲置超时后把 Store 置空并 emit `vault:locked{reason, idleMinutes}`。
   * 前端不接这条事件的后果不是「少个提示」：状态栏锁标会一直显示「已解锁」，用户照常去开连接，
   * 收到的却是一串「vault 未解锁」的失败，且完全无从判断发生了什么。
   * 三件事必须同时做——翻锁标、提示原因、把两个可能开着的凭据类对话框收掉
   * （人已离开机器，屏幕上不能留一个等着被随手点「接受」的主机密钥确认框）。
   */
  function bridgeVaultEvents(): void {
    trackUnlisten(listen<{ reason: string; idleMinutes: number }>("vault:locked", (e) => {
      vaultLocked = true;
      authDlg?.dismissForVaultLock();
      hostKeyDlg?.dismissForVaultLock();
      const { reason, idleMinutes } = e.payload;
      // 其他来源（将来的手动/策略锁定）按通用文案兜底，不假装知道分钟数——
      // idleMinutes 只对闲置超时有意义。
      if (reason === "auto_lock") toast.warn(`保险库已闲置 ${idleMinutes} 分钟，已自动锁定`);
      // restored：恢复备份之后后端把内存里那份 Store 丢掉了（它是恢复前的快照）。
      // 走通用文案会说成「保险库已锁定」，读起来像是出了故障——而这其实是用户刚做完的事
      // 的正常后果，该说的是「接下来要用新库的密码重新解锁」。
      else if (reason === "restored") toast.warn("保险库已从备份恢复，请用该备份的应用密码重新解锁");
      else toast.warn("保险库已锁定");
    }));
  }

  /**
   * 加载连接列表（审计 P2：`profiles_list` 从裸数组改为 `{ profiles, badRows }`）。
   * 五个刷新点全部收敛到这一个函数，否则每新增一处调用就多一次「忘了取 .profiles」的机会——
   * 而这类错误在 `profiles` 声明为 any[] 时编译期毫无信号，运行期表现为侧栏整体空白。
   * 坏行诊断同步落到 `profileBadRows`：读不出来的行必须在 UI 上可见，见 Sidebar 的 badRows 呈现。
   */
  /**
   * 失败面板的「重试」：撤掉这个失败占位标签，再拨一次。**一次点击 = 一次拨号**，
   * 不循环——publickey 每失败一次照样吃堡垒机的 MaxAuthTries。
   * 先把 id/profile 读进局部量再动手：removeTab 会销毁当前 each 节点。
   */
  async function retryFailedTab(tabId: string, p: Profile | null): Promise<void> {
    if (!p) return;
    removeTab(tabId);
    await openSession(p);
  }

  /**
   * 失败面板的「用本地 SSH Agent 重试」：在档案上勾 allow_agent 并**立刻**重拨一次。
   * 走 profile_save 而不是自己拼 SQL：id 冲突、pins 剥离等不变量都在那条路上。
   * 保存失败就不重拨（toast 说清）——不能让用户以为「已经开了 agent」。
   */
  async function enableAgentAndRetry(tabId: string, p: Profile | null): Promise<void> {
    if (!p) return;
    const updated: Profile = { ...p, auth: { ...(p.auth ?? { vault_record: null }), allow_agent: true } };
    try {
      await invoke("profile_save", { id: p.id, profile: updated });
    } catch (e) {
      toast.error(`开启 Agent 认证失败：${e}`);
      return;
    }
    await loadProfiles();
    removeTab(tabId);
    await openSession(updated);
  }

  /**
   * 把这个会话排队的命令发进它的终端（M7.3）。
   *
   * 调用点有两个，且**必须都在**：`onReady`（终端刚建好）与 `runScriptInNewTab` 里拿到 id 之后。
   * 谁先到不确定——`openSession` resolve 与 Svelte 渲染 TerminalPane 都在微任务里排队。
   * `takeForSession` 取走即清，所以两处都调也只会发一次；只留一处则有一半概率命令永远不发。
   */
  async function flushPendingCommands(sessionId: string): Promise<void> {
    for (const cmd of takeForSession(sessionId)) {
      try {
        await invoke("term_input", { sessionId, dataB64: encodeB64(new TextEncoder().encode(cmd + "\r")) });
      } catch (e) {
        // 会话在这半秒里没了：说出来。静默丢弃的话用户看到的是一个空白新标签，
        // 而他刚刚确认过「前台运行这个脚本」。
        toast.error(`脚本命令没能送进新标签：${e}`);
      }
    }
  }

  /**
   * 文件视图双击 `.sh` → 前台运行（M7.3 出口标准②：「前台输出进新终端标签」）。
   *
   * 为什么开**新**标签而不是往当前标签里打：当前标签正开着文件视图，它下面那个终端可能正跑着
   * 别的东西（vim、tail -f、另一个交互程序）。把一条 `sh /path` 塞进去轻则打断，重则被那个
   * 交互程序当成它自己的输入吃掉——脚本不会运行，而用户以为它运行了。
   */
  async function runScriptInNewTab(command: string, fromSessionId: string): Promise<void> {
    const tab = get(tabs).find((t) => t.id === fromSessionId);
    const prof = tab ? profiles.find((x) => x.id === tab.profileId) : undefined;
    if (!prof) {
      toast.error("找不到这个标签对应的连接档案，无法开新标签运行脚本");
      return;
    }
    const id = await openSession(prof);
    if (!id) return; // openSession 失败时已落错误标签 + toast，这里再说一遍是重复噪音
    queueForSession(id, command);
    await flushPendingCommands(id);
  }

  /**
   * 开虚拟窗口那一刻的画布尺寸估值。
   *
   * 精确尺寸只有画布自己知道（它上面有标签栏、下面有状态栏、右边可能开着 AI 面板），
   * 而画布挂载后 ResizeObserver 会立刻 `reflowVWindows` 把所有窗口重新夹一遍——所以这里
   * 给一个保守估值就够，不必去 DOM 里量。估小不估大：估大了新窗口会先闪到画布外一帧。
   */
  function vwindowViewport(): { width: number; height: number } {
    const el = document.querySelector('[data-testid="vwindow-layer"]');
    if (el) return { width: el.clientWidth, height: el.clientHeight };
    return { width: Math.max(0, window.innerWidth - 260), height: Math.max(0, window.innerHeight - 160) };
  }

  async function loadProfiles(): Promise<void> {
    try {
      const r = await invoke<ProfileListResult>("profiles_list");
      profiles = r.profiles ?? [];
      profileBadRows = r.badRows ?? [];
    } catch (e) {
      // 整表读失败（库损坏/无权限）与「读到了但缺几行」是两回事，后端也分两条路返回：
      // 这里是前者，列表根本没拿到，必须报错而不是把界面留成一个空列表。
      console.error("加载 profiles 失败:", e);
      toast.error(`连接列表加载失败：${e}`);
    }
  }

  // ── M4a：快速命令集/片段库 ────────────────────────────────────────────────

  let quickCommands = $state<QuickCommand[]>([]);
  let quickDialogOpen = $state(false);

  async function loadQuickCommands(): Promise<void> {
    // 读侧兜底：库被写脏不炸（parseQuickCommands 烂值回空库），便利功能不为坏数据全不可用
    quickCommands = parseQuickCommands(await settingGet<string | null>("quick.commands", null));
  }

  async function saveQuickCommands(list: QuickCommand[]): Promise<boolean> {
    try {
      if (await settingSet("quick.commands", JSON.stringify(list)) === false) return false;
      quickCommands = list;
      return true;
    } catch (e) {
      // 写失败（越过后端形状闸/库故障）：本地态已更新但库没动，刷新后回旧库。
      // 提示必须说清这个分裂，否则用户以为已保存。
      toast.error(`快速命令未保存，请重试：${e}`);
      return false;
    }
  }

  /** 快速命令下发（M4a）：发当前活动会话（Xshell 同款语义；广播走组合栏/📢）。 */
  async function sendQuickCommand(command: string) {
    const sessionId = $activeTabId;
    if (!sessionId) {
      toast.warn("没有活动会话，命令未发送");
      return;
    }
    try {
      // 经 sendThenRecord 走「先发后记」：次序反了历史里会出现一条其实从未发出
      // 的命令（lib/history.ts 有该次序的单测；直接内联写就没法钉住它）。
      await sendThenRecord(
        command,
        sessionId,
        (cmd) => invoke("term_input", { sessionId, dataB64: encodeB64(new TextEncoder().encode(cmd + "\r")) }),
        recordHistory,
      );
      // 执行侧审计，与组合命令栏同一口径：先发后记，失败不阻断。
      void invoke("audit_command_sent", { sessionId, command }).catch((e) =>
        console.error("命令审计写入失败（发送本身已成功）", e),
      );
    } catch (e) {
      // P2-20 口径：失败不回显命令内容
      toast.error(`快速命令发送失败：${e}`);
    }
  }

  /**
   * 记一条历史命令（M4a）。
   *
   * 只在**发送成功之后**记：发失败的命令记进历史会让人以为跑过了。
   *
   * 记录失败**不打扰用户**（只落一条 console 供上报通道抓）：历史是增强，
   * 为它弹一个错误提示会把「命令发出去了」这件正事的反馈盖掉。
   */
  function recordHistory(command: string, sessionId: string | null): void {
    void invoke("history_record", { command, sessionId }).catch((e) => {
      console.warn("history_record 失败（不影响命令发送）", e);
    });
  }

  /**
   * 启动步骤的隔离壳（路线图 4c 架构缺陷 A3「启动期静默失败」，2026-09-02）。
   *
   * onMount 此前是一串裸 await：排在最前的 `await listen("rdp:status")` 若因 IPC 未就绪而
   * reject，整个 async 函数在那里终止——后面的 loadProfiles / vault_status / 会话恢复一个都
   * 不跑，用户看到的是空侧栏 + 锁着的库，而 rejection 落在无人 catch 的 Promise 上，**没有任何
   * 报错**。`void initTheme()` 这类 fire-and-forget 同理：失败即无声。
   *
   * 每一步各自兜住：失败上报后端日志（`log_frontend_error`，可查），下一步照常。
   * 步骤之间没有真正的顺序依赖——各自读写各自的 store，谁失败都不该拖垮别人。
   * 判据：lib/lifecycle-guards.test.ts 守「onMount 顶层没有裸 await / void」。
   */
  async function boot(name: string, step: () => Promise<unknown> | unknown): Promise<void> {
    try {
      await step();
    } catch (e) {
      reportFrontendError(`startup:${name}`, e);
      console.error(`启动步骤 ${name} 失败：`, e);
    }
  }

  onMount(async () => {
    void boot("initTheme", initTheme);
    // RDP 状态播报（固定事件名，sessionId 在载荷里）：连接安全层/远端断开原因等
    // 一句一行给用户看见。RdpCertDialog 自带 rdp:cert 的监听，这里不重复。
    // 经 untilUnmount 同步入表（4c A2），不再 await——它此前是整条启动链上第一个
    // 能让后面全部不跑的 reject 点。
    appUnlisteners.push(
      untilUnmount(listen<{ sessionId: string; message: string }>("rdp:status", (e) => {
        if (e.payload.message) toast.info(e.payload.message);
      })),
    );
    // 语言：与主题同批载入。放在这里而不是 SettingsDialog 的 onMount，
    // 是因为菜单栏在设置对话框被打开之前就已经渲染了——晚一步就会先闪一次中文。
    void boot("initLocale", initLocale);
    void boot("initLayout", initLayout);
    // 窄屏覆盖层（M4b 第 18 项）：返回解绑函数，随 onDestroy 一并释放。
    // 不解绑的话，每次组件重挂载都留一个持有旧 store 的 matchMedia 监听。
    appUnlisteners.push(initResponsive());
    void boot("initKeyboardMode", initKeyboardMode);
    appUnlisteners.push(initTransferStore()); // 返回解绑函数，随 onDestroy 一并释放
    await boot("bridgeSessionEvents", bridgeSessionEvents);
    await boot("bridgeVaultEvents", bridgeVaultEvents);
    await boot("bridgeWindowClose", bridgeWindowClose);
    // 此处原有一行 `void emit("app:ready")`，注释标着「Task 24 stdout 行协议」。
    // 计划里的另一半（`lib.rs` 侧 `app.listen("app:ready")` 后打印 `FUTURE_SHELL_READY_MS`）
    // 从未落笔——全仓 Rust 侧零 `listen`（审计：事件契约门禁）。一条无人接收的事件不构成协议，
    // 留着只会让人以为「前端就绪时刻」是有测量的。scripts/coldstart.ps1 实际抓的是后端 setup()
    // 里的 stderr 标记，那是**后端**就绪；两者的差值就是前端首帧耗时，目前无人测量——
    // crates/itest/tests/perf.rs 的文件头已把这个口径缺口写明，此处不再用一行死代码假装它存在。
    await boot("ping", async () => { status = await ping().catch(() => "ipc 未就绪"); });

    // Task 20: 加载 profiles
    await boot("loadProfiles", loadProfiles);

    // M4a：快速命令集/片段库（settings 键 quick.commands，后端白名单闸同形状）
    void boot("loadQuickCommands", loadQuickCommands);
    void boot("loadFolders", loadFolders); // M4a 手动文件夹

    // Task 20: 检查 vault 状态
    // 契约：vault_status 的 Rust 签名是 `-> Result<bool, String>`（已解锁 = true），不是对象。
    // 原先按 `{ locked }` 取值恒得 undefined → vaultLocked 变 undefined，状态栏锁标与菜单分流双双失灵。
    await boot("vault", async () => {
      // 先顺手试一次静默解锁（2026-08-31）：没设应用口令的库主密钥就在系统
      // keyring 里，开就是了——用户不该每次启动都去点那个「应用密码（留空）」
      // 的框。设过口令的库后端会原样不动（这里拿到 false，仍走手动路径）。
      //
      // 由前端**显式调用**而不是只靠后端 setup 里那个 spawn：spawn 与本函数
      // 谁先跑没有保证，而 vault_status 的结果直接决定状态栏锁标与菜单分流——
      // 竞态的表现是「有时候启动完还显示锁着，点一下才好」，那种偶发最难查。
      // 命令本身可重入（已解锁时直接返回 true）。
      await invoke<boolean>("vault_auto_unlock").catch(() => false);
      vaultLocked = !(await invoke<boolean>("vault_status"));
    });

    // Task 22 Step 4: 启动恢复流程
    //
    // 审计 P2「设置项只写不读」：`ui.restoreUnclosed` 此前**全仓只有 SettingsDialog 那一处写入**，
    // 没有任何读取方——设置页上那个「启动时询问恢复未关闭的会话」的勾选框取消掉也照样弹框。
    // 与 P1-16 的 `vault.autoLockMinutes` 同类：界面承诺了一个它并不执行的行为。
    //
    // 三条走向的裁决在 lib/session-restore.ts（纯函数，逐格有测试）；这里只负责执行。
    await boot("sessions_unclosed", async () => {
      const askOnStart = await settingGet<boolean>("ui.restoreUnclosed", true);
      const result = await invoke<Array<{ session_key: string; profile_id: string | null; updated_at: string }>>("sessions_unclosed");
      switch (restoreAction(askOnStart, result.length)) {
        case "ask":
          unclosedRows = result;
          restoreDialogOpen = true;
          break;
        case "discard":
          void invoke("sessions_discard_unclosed").catch(() => {});
          break;
        case "none":
          break;
      }
    });
  });

  // P1-11：onMount 为 async 函数，其返回值不被 Svelte 当作清理函数，故解绑一律走 onDestroy
  onDestroy(() => {
    const pending = appUnlisteners;
    appUnlisteners = [];
    for (const un of pending) un();
  });

  /** 菜单/工具栏动作分发（MVP 可用项；会话类动作由 Task 20 接线）。 */
  async function onAction(id: string) {
    if (id === "app.exit") {
      await requestCloseWindow();
      return;
    }
    if (id === "view.fullscreen") {
      const w = getCurrentWindow();
      const fs = await w.isFullscreen();
      await w.setFullscreen(!fs);
      return;
    }
    if (id === "tools.settings") { settingsOpen = true; return; }
    if (id === "tools.broadcast") { toggleLiveBroadcast(profiles); return; }
    if (id === "tools.quickCommands") { toggle("quickbar"); return; }
    // 菜单「快速命令集编辑器…」直开管理器；工具栏 ⚡ 与菜单「显示/隐藏快速命令条」
    // 切换按钮条。两个入口两件事，标签与行为必须对得上——同一 id 干两件事
    // 的写法迟早让某一处的文案说谎。
    if (id === "tools.quickCommandsEdit") { quickDialogOpen = true; return; }
    // M4a 高亮关键字：规则编辑器在选项页「终端外观」内，故此入口开选项页并切到该页
    // （不另做一个第二处编辑器——两处编辑同一份规则表迟早显示不一致）。
    if (id === "tools.highlight") { settingsOpen = true; settingsTab = "terminal"; return; }
    // 配色编辑器（M4b 第 4 项）与高亮规则同处「终端外观」页，走同一条直达路径
    if (id === "tools.schemeEditor") { settingsOpen = true; settingsTab = "terminal"; return; }
    if (id === "tools.tunnels") { tunnelDialogOpen = true; return; }
    if (id === "tools.history") { historyOpen = true; return; }
    if (id === "tools.schedule") { scheduleOpen = true; return; }
    if (id === "tools.replay") { replayOpen = true; return; }
    if (id === "folder.new") { newFolder(); return; }
    if (id.startsWith("theme.")) { await setTheme(id.slice("theme.".length)); return; }
    // overlay 形态下这个动作的含义是「展开/收起浮层」而非翻 sidebarVisible（路线图 4c，见 toggleSidebarFor）
    if (id === "view.sidebar") { toggleSidebarFor(sidebarPresentation); return; }
    // 停靠侧（M4b 第 16 项）。父项 view.sidebarDock 自身无动作，只展开子菜单。
    if (id.startsWith("sidebarDock.")) {
      const next = id.slice("sidebarDock.".length);
      if (next === "left" || next === "right" || next === "float") void setSidebarDock(next);
      return;
    }
    if (id === "view.monitor") { toggle("monitor"); return; }
    if (id === "view.compose") { toggle("compose"); return; }
    if (id === "view.status") { toggle("status"); return; }
    if (id === "view.toolbar") { toggle("toolbar"); return; }
    // ——终端类动作：一律派发到活动标签的终端（termApis，见其声明处的失效清单）——
    // 无活动终端时静默返回：菜单项在 §2.1 里是常驻可点的，没有会话可作用于时不该弹错误提示。
    if (id === "edit.find" || id === "edit.findNext") {
      const api = activeTermApi();
      if (!api) return;
      if (id === "edit.find") api.openSearch();
      else api.findNext(); // R57：F3 续搜，查询词存于浮层内部
      return;
    }
    // 复制为纯文本 = 复制：xterm 的 getSelection() 取的是**已渲染的字符网格**，本就不含 ANSI 序列
    // （序列在解析阶段就被消费成属性了，不进缓冲区）。两项行为一致是终端模型决定的，不是偷懒；
    // 保留两个菜单项是因为 §2.1 列了它，且 Xshell 用户按名字找得到。若哪天引入富文本复制
    // （HTML/RTF），分岔点在这里：edit.copy 带样式，edit.copyPlain 保持现状。
    if (id === "edit.copy" || id === "edit.copyPlain") { activeTermApi()?.copySelection(); return; }
    if (id === "edit.paste") { activeTermApi()?.pasteFromClipboard(); return; } // 经 pasteFlow 多行确认，与右键「粘贴」同一条路
    if (id === "edit.selectAll") { activeTermApi()?.controller()?.selectAll(); return; }
    if (id === "edit.clearScrollback") { activeTermApi()?.controller()?.clearScrollback(); return; }
    if (id === "edit.clearScreen") {
      // §2.7 明写「截获后触发的应用动作可经 term_input 下发等效字节，如 Ctrl+L→edit.clearScreen 发送 \x0c」。
      // 走会话而不是本地 clear：清屏是 shell 的事，本地清了提示符不会重绘，屏上会留一片空白。
      if (!$activeTabId) return;
      try {
        await invoke("term_input", { sessionId: $activeTabId, dataB64: encodeB64(new TextEncoder().encode("\x0c")) });
      } catch (e) {
        toast.error(`清屏失败：${e}`); // 口径同 sendCompose：term_input 会在会话已终止时 reject，不接就是一条无人看见的 unhandled rejection
      }
      return;
    }
    if (id === "view.fontGrow" || id === "view.fontShrink" || id === "view.fontReset") {
      // per-session 字号（§3.3）：clamp 8–32 在 controller 内部。终端**获焦**时这三个键由
      // term.ts 的按键钩子就地消费（不到这里）；本分支承接的是菜单点击与终端失焦时的按键。
      const ctrl = activeTermApi()?.controller();
      if (!ctrl) return;
      if (id === "view.fontReset") ctrl.resetFontSize();
      else ctrl.zoomFont(id === "view.fontGrow" ? 1 : -1);
      return;
    }
    // 标签 MRU 切换（Ctrl+Tab / Ctrl+Shift+Tab，§4）：tabs.ts 早有原语，只是从没人调
    if (id === "tab.mruNext") { activateMruNext(); return; }
    if (id === "tab.mruPrev") { activateMruPrev(); return; }
    // 导入/导出：与侧栏 ⤵ / ⤴ 两个按钮共用同一实现（见 importProfilesFromFile / exportAllProfiles）
    if (id === "import.json") { importProfilesFromFile(loadProfiles); return; }
    if (id === "import.xshell") { foreignImportOpen = true; return; }
    if (id === "tools.keyManager") { keyManagerOpen = true; return; }
    // M2 AI 面板。这里**不查**「配没配好模型源」——没配好时面板自己显示配置向导，
    // 那正是出口第 7 项要的行为。在这里拦一道会让用户点了没反应，等于把
    // 「怎么配」这条路堵死。
    if (id === "tools.auditVerify") { auditDialogOpen = true; return; }
    if (id === "tools.agent") { agentPanelOpen = true; return; }
    if (id === "tools.ai") {
      // RDP 会话没有文本终端可读写：AI 整套（提问/解读/建议）对它无意义。
      // 显式 toast 而不是静默无效——静默会让用户以为是 AI 坏了（裁定：显式禁用）。
      if (activeTabIsRdp) {
        toast.info("AI 功能不支持 RDP 会话（没有文本终端可读写）");
        return;
      }
      // 从快捷键/菜单进来的是「提问」那一路，不带选区。清掉上一次解读的选区，
      // 否则面板一开就重跑上次那段解读——用户按 Ctrl+Shift+A 是想问新问题。
      aiPendingSelection = null;
      aiPanelOpen = true;
      return;
    }
    // 窗口菜单（M4b 多窗口）
    if (id === "window.new") { void openWindow(null, null); return; }
    // 截图（M4b）：两条落地路径各一个菜单项
    if (id === "edit.screenshotCopy") { void takeScreenshot("clipboard"); return; }
    if (id === "edit.screenshotSave") { void takeScreenshot("file"); return; }
    if (id === "window.closeAll") { closeAllTabs(); return; }
    if (id === "window.cascade") { void arrangeWindows("cascade"); return; }
    if (id === "window.tileH") { void arrangeWindows("tile-horizontal"); return; }
    if (id === "window.tileV") { void arrangeWindows("tile-vertical"); return; }
    if (id === "export") { await exportAllProfiles(); return; }
    // Alt+P 会话属性（§4 白名单）：打开活动标签所属 profile 的编辑框，等价于侧栏右键「属性」。
    // 无档案会话（临时连接，profileId 为空）无属性可编辑，静默返回。
    if (id === "session.properties") {
      const tab = $tabs.find((t) => t.id === $activeTabId);
      const p = tab ? profiles.find((x) => x.id === tab.profileId) : undefined;
      if (!p) return;
      profileDialogInitial = p;
      profileDialogOpen = true;
      return;
    }
    if (id === "help.docs" || id === "help.keys") {
      // 仓库地址取自本仓 `git remote get-url origin`，不是随手编的：原值 `your-org/futureshell`
      // 是脚手架占位符，该 org 不存在，于是「帮助 → 文档 / 快捷键」两项**必然**打开 404——
      // 与审计反复点名的那一类「界面承诺了一个它并不执行的行为」同构，只是后果轻。
      // 路径用 /tree/main/<仓内实际路径>：GitHub 上 `/<repo>/docs` 这种写法本身也是 404，
      // 目录必须经 tree 引用。两个目标都对应仓内真实存在的文件/目录（docs/、ui-design 规格 §4 快捷键）。
      const REPO = "https://github.com/BerryWang1996/future-shell";
      const urls: Record<string, string> = {
        "help.docs": `${REPO}/tree/main/docs`,
        "help.keys": `${REPO}/blob/main/docs/design/ui.md`,
      };
      if (urls[id]) await openExternal(urls[id]);
      return;
    }
    if (id === "help.logs") {
      await invoke("reveal_log_dir");
      return;
    }
    // M4a「打开会话目录」：**会话转录**的落盘目录（与 help.logs 的应用日志目录
    // 是两处——用户点这里要看的是自己的会话转录，不是程序的排障日志）。
    // 录屏在另一个目录（data_dir/recordings，不可配），走下面那一项——
    // 这段注释曾写「转录/录屏」而实现只指转录，录屏那一半没有任何入口。
    if (id === "session.openDir") {
      try {
        await invoke("reveal_session_log_dir");
      } catch (e) {
        toast.error(`打开会话日志目录失败：`);
      }
      return;
    }
    if (id === "session.openRecordings") {
      try {
        await invoke("reveal_recordings_dir");
      } catch (e) {
        toast.error(`打开录屏目录失败：`);
      }
      return;
    }
    if (id === "help.about") {
      // 轻量 About 模态 - 暂时用 alert，后续可以改成对话框
      alert("FutureShell MVP\n基于 Tauri + Svelte + Rust\n© 2026");
      return;
    }
    if (id === "tools.vaultToggle") {
      // 三分而非二分：已解锁 → 锁上；未解锁且库文件在 → unlock；未解锁且库文件不在 → setup。
      //
      // 原实现把第三种情形挂在 `catch` 上，可 `vault_status` 的 Rust 签名是
      // `Ok(state.vault.lock().await.is_some())`——锁一把 Mutex 读个 Option，**没有任何**
      // 返回 Err 的路径。于是 setup 分支从来不会被走到：全新安装的用户只能看见「解锁 Vault」，
      // 而输入任何密码都会走进后端的 `unlock_with_passphrase(&data_dir, ..)` 去打开一个
      // 根本不存在的 vault.json，必然失败。表现是「Vault 在 UI 上无法初始化」，整条凭据链
      // （ProfileDialog 的凭据选取器、vault_put_secret、认证时按需取密）在新机器上全不可达。
      // `vault_has_file` 这个命令本就是为这一分流写的（见 vault_cmd.rs 文档注），此前零调用。
      try {
        const next = await resolveVaultToggle();
        if (next === "lock") {
          await invoke("vault_lock");
          vaultLocked = true;
          return;
        }
        vaultDialogMode = next; // "unlock" | "setup"
        vaultDialogOpen = true;
      } catch (e) {
        console.error("Vault 状态查询失败:", e);
        toast.error(`Vault 状态查询失败：${e}`);
      }
      return;
    }
    if (id === "tools.vaultManage") {
      // 刻意**不**要求先解锁：这个对话框里的「恢复备份」正是给「库打不开了」准备的。
      // 需要解锁的那两栏（记录、应用密码）由组件自己按 locked 分流显示。
      vaultManagerOpen = true;
      return;
    }
    // Task 20 实际动作
    if (id === "session.new") {
      profileDialogOpen = true;
      profileDialogInitial = null;
      return;
    }
    if (id === "session.closeActive") {
      if ($activeTabId) await requestCloseTab($activeTabId);
      return;
    }
    // session.closeActive 一律经 requestCloseTab 统一门控（Ctrl+W / 中键 / × 同路）。
    //
    // 承接台账已作废，不再保留：S262 当时用它记录「这几项由后续 Task 接线」，而 Task 18/20 收尾时
    // 接线并没有发生——台账留在原地，menus.test.ts 的 S262 守卫按设计只查「零记录」、明写不区分
    // 实体与台账，于是 11 个 enabled 菜单项、3 个未置灰的工具栏按钮、8 个快捷键**全绿着**静默失效，
    // 其中 F3 与 Ctrl+Tab 全仓无第二触发点＝功能整体不可达。
    //
    // 教训不是「台账写得不够细」，是**用文字承诺代替代码、再让守卫接受文字**这件事本身：
    // 承诺方与验收方成了同一份注释。现在每一项都有实体分支，另有 lib/action-wiring.test.ts
    // 只认剥掉注释后的 `"<id>"` 字面量、并同时覆盖菜单/工具栏/快捷键三个触发面；
    // 豁免必须逐条进 EXEMPT 表并写明理由。
  }

  /**
   * 窗口级唯一键派发（UI 规格 §2.7 仲裁 + §4 固定集，R3 裁决）。
   *
   * 两条入口通向同一个 onAction：
   * 1. 终端**未**获焦（菜单/侧栏/对话框焦点，或无标签）→ 本函数；
   * 2. 终端获焦 → term.ts 的 attachCustomKeyEventHandler 内先按 arbitrate({terminalFocused:true})
   *    仲裁，判 local 的键经 TerminalPane 的 onLocalAction 回投到这里的 onAction（R58），
   *    并在回投时 stopPropagation，避免同一次按键被本函数二次执行。
   *    该 prop 此前也从没传过：term.ts 里那条 `if (action.kind === "action" && d.onLocalAction)`
   *    分支在生产中恒不成立，只有单测覆盖过。缺它不至于丢动作（不回投时不拦冒泡，键仍会
   *    冒到本函数），但白名单键在终端内的「本地截获」路径始终是断的。
   */
  function onWindowKeydown(e: KeyboardEvent) {
    capsOn = e.getModifierState("CapsLock");
    numOn = e.getModifierState("NumLock");
    const target = e.target as HTMLElement | null;
    const inField = !!target && /^(INPUT|SELECT|TEXTAREA)$/.test(target.tagName);
    // 输入框内保留原生输入，仅白名单键（§2.7 恒本地）照旧截获
    if (inField && !isAlwaysLocal(e)) return;
    if (arbitrate(e, { terminalFocused: false, mode: (($tabs.find((t) => t.id === $activeTabId))?.keyboardMode) ?? "remote" }) === "passthrough") return;
    const action = actionForKey(e);
    if (action.kind === "passthrough") return;
    e.preventDefault();
    if (action.kind === "toggle-mode") { if ($activeTabId) toggleKeyboardMode($activeTabId); return; }
    void onAction(action.id);
  }

  /**
   * 全局右键兜底：不让 WebView 自己的菜单冒出来。
   *
   * # 为什么需要一个兜底
   *
   * 右键此前只在**少数几个组件**上被显式处理（侧边栏条目、标签、工具栏、
   * 终端、RDP 画布），其余一律漏给 WebView——设置面板、传输列表、AI 面板、
   * SFTP 面板、以及所有空白区域右键，弹的都是「返回/重新加载/查看源代码/
   * 检查」这类浏览器菜单。用户看到的是「这软件是个网页」，而其中「重新加载」
   * 被误点还会把整个会话界面重置。
   *
   * # 唯一的放行：文本输入框
   *
   * `input` / `textarea` / `contenteditable` 里的原生右键菜单是**真有用**的
   * （剪切/复制/粘贴/全选，以及输入法与拼写建议的入口），自己重造一套只会
   * 更差。这与 `onWindowKeydown` 里 `inField` 的口径一致——同一条「输入框
   * 里让原生行为生效」的原则，两处必须一样，否则用户会觉得规则随机。
   *
   * # 为什么不是在 Tauri 侧全局关掉
   *
   * WebView2 有 `AreDefaultContextMenusEnabled`，但那是**一刀切**：连输入框
   * 的原生菜单一起没了，且 macOS/Linux 的 webview 各有各的开关（三套平台
   * 代码）。在事件层做，语义精确且零平台代码。
   *
   * 组件自己的右键处理（终端粘贴、标签配色菜单…）都在冒泡路径上更早的位置
   * 调了 `preventDefault`，本函数看到的是**没人认领**的那些——所以两者不冲突：
   * 已认领的这里再 preventDefault 一次也无副作用（幂等）。
   */
  function onWindowContextMenu(e: MouseEvent) {
    const target = e.target as HTMLElement | null;
    if (!target) return;
    if (shouldAllowNativeContextMenu(target)) return; // 输入框：原生菜单有用
    e.preventDefault();
  }

  /** Task 20: 关闭会话 */
  async function closeSession(sessionId: string) {
    // 占位标签后端并无对应会话（见 lib/open-session.ts）：session_close 对未知 id 虽然返回 Ok，
    // 但它会顺带广播一条 session:closed 并去 journal 里改一行不存在的记录，白跑一趟 IPC。
    // 若此刻拨号仍在途中，openSession 会在 resolve 后发现占位标签已不在，自己把真会话关掉。
    if (isPendingSessionId(sessionId)) {
      removeTab(sessionId);
      return;
    }
    try {
      await invoke("session_close", { sessionId });
      removeTab(sessionId); // 侧栏灯随 tabs 派生，无第二处状态要清
      closeViewWindowFor(sessionId);
    } catch (e) {
      console.error("关闭会话失败:", e);
    }
  }

  /** 窗口关闭闸门：菜单「文件 → 退出」与标题栏 ×/Alt+F4 共用同一条确认路径。见 lib/window-close.ts。 */
  const closeGate = createCloseGate(
    () => getCurrentWindow().close(),
    () => requestCloseWindow(),
  );

  /** 订阅 `tauri://close-requested`：不订阅则 ×/Alt+F4 绕过下面这整段门控（审计：事件契约门禁）。 */
  function bridgeWindowClose(): void {
    trackUnlisten(
      getCurrentWindow().onCloseRequested(async (event) => {
        // 闸门返回的布尔在此处吞掉：Tauri 只看 `event.isPreventDefault()`，处理器签名要求
        // `void | Promise<void>`。是否拦截由闸门内部**同步**调用的 preventDefault 决定。
        await closeGate.onCloseRequested(() => event.preventDefault());
      }),
    );
  }

  /** Task 22 Step 4: 窗口关闭确认门控 */
  async function requestCloseWindow() {
    // 判据统一走 decideClose：此处曾用 tabs.length，于是满屏断开态的残标签也照样弹框，
    // 违反出口的「全空闲直接关闭」。
    const { prompt, sessions, queued, running } = decideClose($tabs, activeTransfers());
    if (!prompt) {
      // 经闸门关闭而非直接 close()：× 触发的那一次已被 preventDefault 拦下，
      // 这里直接 close() 会再次落到 onCloseRequested 上被再拦一次——无会话时窗口关不掉。
      await closeGate.closeConfirmed();
      return;
    }
    closeConfirmState = {
      scope: "window",
      sessions,
      queued,
      running,
      onConfirm: async () => {
        closeConfirmState = null;
        // 关闭窗口不能依赖会话拆除完成：session_close_all 若抛错或挂起（异常会话态），
        // 窗口也必须关得掉。给拆除一个上限，超时/失败都放行关闭——宁可下回启动多一条
        // 「未关闭会话」恢复提示，也不能让用户关不掉窗口。
        await Promise.race([
          invoke("session_close_all").catch((e) => console.error("关闭会话失败：", e)),
          new Promise((resolve) => setTimeout(resolve, 3000)),
        ]);
        await closeGate.closeConfirmed();
      },
    };
  }

  /** Task 22 Step 4: 标签关闭确认门控（Ctrl+W/中键/× 统一入口） */
  async function requestCloseTab(tabId: string) {
    // 判据统一走 decideClose：此处曾**只**数传输队列，于是一条正跑着 vim 的活连接，
    // 只要没有传输任务，Ctrl+W 就静默断开——出口原文点名要「含活动会话…弹模态」。
    const tab = $tabs.find((t) => t.id === tabId);
    const { prompt, sessions, queued, running } = decideClose(tab ? [tab] : [], activeTransfers(tabId));
    if (prompt) {
      closeConfirmState = {
        scope: "tab",
        sessions,
        queued,
        running,
        onConfirm: async () => {
          closeConfirmState = null;
          await closeSession(tabId);
        },
      };
    } else {
      await closeSession(tabId);
    }
  }


  /**
   * 关闭一批标签（M4b「关闭全部标签」「关闭其他标签」）。
   *
   * 走**同一个** `decideClose` 与**同一个**确认模态——出口原文点名要「触发同一关闭确认」。
   * 另起一套判据的话，「关一个标签会问、关十个反而不问」这种事迟早会出现，
   * 而它出现时没有任何东西会提示。
   *
   * 传输计数用**不带参数**的 `activeTransfers()`（全局在途量），不是逐标签累加：
   * 传输队列本来就是全局的，逐标签累加会把同一件传输数很多次。
   */
  async function requestCloseTabs(tabIds: string[], scope: "tabs"): Promise<void> {
    const targets = $tabs.filter((t) => tabIds.includes(t.id));
    if (targets.length === 0) return;
    const { prompt, sessions, queued, running } = decideClose(targets, activeTransfers());
    const doClose = async () => {
      // 逐个关。并发关会让后端在同一批 session map 上竞争，而收益是零——
      // 用户关的是几个标签，不是几千个。
      for (const t of targets) await closeSession(t.id);
    };
    if (!prompt) {
      await doClose();
      return;
    }
    closeConfirmState = {
      scope,
      sessions,
      queued,
      running,
      onConfirm: async () => {
        closeConfirmState = null;
        await doClose();
      },
    };
  }

  /** 「关闭全部标签」。 */
  function closeAllTabs(): void {
    void requestCloseTabs(tabsToClose($tabs, { mode: "all" }), "tabs");
  }

  /**
   * 「关闭其他标签」——留下 `keepId`，关掉其余。
   *
   * 留下的那个是**右键点中的那个**，不是当前活动的那个：用户在一个非活动标签上
   * 右键选「关闭其他」，他要留的显然是手指底下这个。按活动标签留的话，
   * 他会眼睁睁看着自己刚点的那个被关掉。
   */
  function closeOtherTabs(keepId: string): void {
    void requestCloseTabs(tabsToClose($tabs, { mode: "others", keepId }), "tabs");
  }

  /**
   * Task 22 Step 4 / P1-14：勾选的未关闭会话逐条重连。
   * 原实现只 console.info 打印「恢复会话: <key>」——对话框点「重连」后什么都不会发生，
   * 用户看到的是弹窗消失、终端空空如也，属功能缺失而非未接线。
   *
   * 恢复 = 按 profile_id 重新 session_open（旧 session_id 是进程内句柄，重启后已无意义，
   * 后端也不支持接管旧连接；屏幕内容同样不恢复——远端是新 shell）。两类无法恢复的行显式提示，不静默跳过：
   * - profile_id 为 null：日志行落库时就没记连接配置（早于 Task 20 的旧库行）；
   * - profile_id 指向的配置已被删除：列表里查无此 id。
   * 收尾统一 sessions_discard_unclosed：本次已就每一行做出裁决（选中=重连、未选中=放弃），
   * 旧行不清则下次启动仍会弹同一批，且新开的会话会另立新行，日志越积越多。
   */
  async function onRestoreSessions(sessionKeys: string[]) {
    restoreDialogOpen = false;
    const rows = unclosedRows;
    unclosedRows = [];
    let restored = 0;
    for (const key of sessionKeys) {
      const row = rows.find((r) => r.session_key === key);
      if (!row) continue;
      if (!row.profile_id) {
        toast.warn("该会话缺少连接配置，无法自动恢复");
        continue;
      }
      const profile = profiles.find((p) => p.id === row.profile_id);
      if (!profile) {
        toast.warn("该会话对应的连接配置已删除，无法自动恢复");
        continue;
      }
      if (await openSession(profile)) restored += 1;
    }
    if (restored > 0) toast.info(`已发起 ${restored} 个会话的恢复连接`);
    void invoke("sessions_discard_unclosed").catch(() => {});
  }

  /**
   * 组合命令栏发送（P2-20 + M4a 广播通道①）：原实现是 `console.info(...)` 打印
   * 命令原文（功能不存在 + 泄露面），后改为只发当前标签；M4a 实装四态目标。
   *
   * P2-20 纪律不变：失败只报错误本身、任何路径都不回显命令内容——组合栏的
   * 典型用途恰恰是批量下发含口令、token、私钥路径的命令。
   */
  async function sendCompose(target: ComposeTarget, text: string, suffix: string) {
    const ids = broadcastTargetIds(target, profiles);
    if (ids.length === 0) {
      toast.warn(broadcastEmptyHint(target));
      return;
    }
    // UTF-8 → 二进制串 → base64：直接 btoa(中文) 会抛 InvalidCharacterError（btoa 只吃 Latin-1）
    const dataB64 = encodeB64(new TextEncoder().encode(text + suffix));
    const { sent, failed } = await sendToSessions(ids, dataB64, invoke);
    // 至少发成一个才记历史；广播时记在**第一个成功**的会话名下（主机维度的历史
    // 只能落在一台机器上，而多目标广播本身在历史里的价值是「这条命令我发过」）。
    if (sent.length > 0) {
      recordHistory(text, sent[0] ?? null);
      // 执行侧审计（M3 出口 7 的 ②）：AI 建议那条记的是「发起了、未执行」
      // （RequestOnly），真正跑没跑由这一条回答。**只在真发出去之后记**——
      // 记在发送之前会把一次失败的发送也算成执行过。
      // 广播时记在第一个成功的会话名下，与历史同一口径。
      void invoke("audit_command_sent", { sessionId: sent[0] ?? null, command: text }).catch(
        (e) => console.error("命令审计写入失败（发送本身已成功）", e),
      );
    }
    if (sent.length > 0 && failed.length === 0 && sent.length > 1) {
      toast.info(`已发送到 ${sent.length} 个会话`);
    }
    if (failed.length > 0) {
      // 目标数与失败数并列：5 发 2 败时用户要的是「哪 3 个成了」，不是一句失败
      toast.error(`广播发送：${sent.length} 成功 / ${failed.length} 失败（${failed.map((f) => f.sessionId.slice(0, 8)).join("、")}）`);
    }
  }

  /** 解析广播目标为已连接会话集合（S305 的装配点：tabs/profiles 喂纯函数）。 */
  /* 广播动作（broadcastTargetIds / broadcastEmptyHint / routeBroadcastKeystroke /
   * toggleLiveBroadcast）已搬到 lib/broadcast-actions.ts（2026-09-03 拆分）。
   * 它们唯一没法从 store 拿的是 profiles（组件状态），故逐个收它作参数。 */

  function onDiscardSessions() {
    restoreDialogOpen = false;
    void invoke("sessions_discard_unclosed").catch(() => {});
    unclosedRows = [];
  }

  /**
   * 连接配置导出的落盘路径（全量导出与右键「导出此连接」共用）。
   * 抽出来是为了让两条入口的行为不可能分叉——文件名规则、Blob 类型、URL 回收、成功提示
   * 各写一遍迟早跑偏（右键导出此前根本没有实现，属于第二种分叉：一条入口压根不存在）。
   * `revokeObjectURL` 必须在 click 之后调，且不能提前到同步栈之外：Blob URL 一旦回收，
   * 尚未取走内容的下载会静默失败。
   */
  /**
   * 截当前终端一屏（M4b）。
   *
   * 一定要有反馈：截图是「按一下、屏幕上什么都不变」的操作，没有 toast 的话
   * 用户会以为没生效而连按好几次——存盘路径下那就是好几个文件。
   */
  async function takeScreenshot(sink: "clipboard" | "file"): Promise<void> {
    const ctrl = activeTermApi()?.controller();
    if (!ctrl) {
      toast.warn("没有活动终端");
      return;
    }
    const title = get(tabs).find((t) => t.id === get(activeTabId))?.title;
    try {
      const name = await ctrl.screenshot(sink, title);
      toast.info(name ? `已保存 ${name}` : "截图已复制到剪贴板");
    } catch (e) {
      toast.error(`截图失败：${e}`);
    }
  }

  /* ── M4b 多窗口 ──────────────────────────────────────────────────────────────
   * openWindow / closeViewWindowFor / detachTab / arrangeWindows 已搬到 lib/window-actions.ts
   * （2026-09-03 拆分）：它们只用 IPC + toast + tabs store，不碰本组件状态。
   * ────────────────────────────────────────────────────────────────────────── */

  /* 档案导入/导出（todayStamp / saveJsonFile / importProfilesFromFile / exportAllProfiles /
   * exportOneProfile）已搬到 lib/profile-io.ts（2026-09-03 拆分）。导入成功后要重载列表，
   * 故 importProfilesFromFile 收一个 reload 回调——那是它与本组件之间仅剩的耦合。 */

  function onGripPointerDown(e: PointerEvent) {
    sidebarDrag = true;
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
  }
  function onGripPointerMove(e: PointerEvent) {
    if (sidebarDrag) setSidebarWidth(e.clientX);
  }
  function onGripPointerUp() {
    if (sidebarDrag) {
      sidebarDrag = false;
      persistSidebarWidth();
    }
  }
</script>

<svelte:window onkeydown={onWindowKeydown} oncontextmenu={onWindowContextMenu} />

<div class="app">
  <MenuBar {onAction} />
  {#if $toolbarVisible}<ToolBar {onAction} pressed={(id) => id === "tools.broadcast" && $liveBroadcast}
    overlaySidebar={sidebarPresentation === "overlay"} overlayOpen={$sidebarOverlayOpen} />{/if}
  <div class="body">
    {#if $sidebarVisible}
      <aside
        class="sidebar"
        class:dock-right={sidebarPresentation === "right"}
        class:overlay={sidebarPresentation === "overlay"}
        class:overlay-open={sidebarPresentation === "overlay" && $sidebarOverlayOpen}
        style:width="{$sidebarWidth}px"
        style:--fs-sidebar-w="{$sidebarWidth}px"
        data-testid="sidebar"
        data-presentation={sidebarPresentation}
      >
        <Sidebar
          {profiles}
          badRows={profileBadRows}
          {sessionStates}
          hostLights={$hostLights}
          {manualFolders}
          onDeleteFolder={(path) => void deleteFolder(path)}
          onRenameFolder={(path) => (renameFolderFrom = path)}
          onOpen={openSession}
          onNew={() => { profileDialogOpen = true; profileDialogInitial = null; }}
          onEdit={(p) => { profileDialogOpen = true; profileDialogInitial = p; }}
          onDelete={(p) => {
            deleteConfirmState = {
              name: p.name,
              onConfirm: async () => {
                deleteConfirmState = null;
                try {
                  await invoke("profile_delete", { id: p.id });
                  await loadProfiles();
                  toast.info(`已删除连接 ${p.name}`);
                } catch (e) {
                  toast.error(`删除失败: ${e}`);
                }
              },
            };
          }}
          onImport={() => importProfilesFromFile(loadProfiles)}
          onExport={exportAllProfiles}
          onExportOne={(p) => void exportOneProfile(p)}
          onSaved={async () => { await loadProfiles(); }}
        />
      </aside>
      {#if sidebarPresentation !== "overlay"}
      <div
        class="sidebar-grip"
        class:dock-right={sidebarPresentation === "right"}
        role="separator"
        aria-orientation="vertical"
        onpointerdown={onGripPointerDown}
        onpointermove={onGripPointerMove}
        onpointerup={onGripPointerUp}
      ></div>
      {/if}
      {#if sidebarPresentation === "overlay" && $sidebarOverlayOpen}
        <!-- 点击外部收起（出口原文点名）。用一个透明遮罩而不是在 .main 上挂
             click：后者会把「点终端里的某个位置」也算成「点了外部」，
             于是用户想在终端里放光标却先收起了侧栏，还得再点一次。 -->
        <div
          class="sidebar-scrim"
          data-testid="sidebar-scrim"
          role="presentation"
          onclick={() => sidebarOverlayOpen.set(false)}
        ></div>
      {/if}
    {/if}
    <div class="main">
      <!-- Task 20：标签 MDI（铃铛角标读 lib/tabs store，Task 18 契约） -->
      <TabBar
        onClose={requestCloseTab}
        onRecord={(id) => void toggleRecording(id)}
        onDetach={detachTab}
        onCloseOthers={closeOtherTabs}
      />
      <div class="terminal" data-testid="terminal-area">
        <!-- Task 18/20：TerminalPane 标签页堆叠 -->
        {#each $tabs as tab (tab.id)}
          <div class="term-wrapper" class:active={tab.id === $activeTabId}>
            {#if isPendingSessionId(tab.id)}
              <!-- 占位标签后端还没有（error 态则是永远不会有）对应会话，不能挂真终端：
                   term_input/term_resize/term_ack 对未知 session_id 一律 Err("no session")，
                   而 TerminalPane 里这三处都是 `void invoke(...)`，挂上去就是每建一个占位标签
                   甩一串没人接的 Promise 拒绝，且用户对着一个黑框敲的字会被静默丢弃。 -->
              {#if tab.status === "error"}
                {@const failedProfile = profiles.find((x) => x.id === tab.profileId) ?? null}
                <!-- 失败当刻给出路（路线图 4c，2026-09-01）：此前这里只有一行被截断的文案。
                     回调全部走既有原语——removeTab + openSession / profile_save / ProfileDialog /
                     tools.vaultToggle——面板自己不发 IPC（key_list 除外，只读）。 -->
                <ConnectFailurePanel
                  failure={tab.failure ?? null}
                  errorText={tab.errorText ?? "连接失败"}
                  profile={failedProfile}
                  onRetry={() => retryFailedTab(tab.id, failedProfile)}
                  onEnableAgent={() => enableAgentAndRetry(tab.id, failedProfile)}
                  onConfigureAuth={() => { if (failedProfile) { profileDialogInitial = failedProfile; profileDialogTab = "auth"; profileDialogOpen = true; } }}
                  onUnlockVault={() => void onAction("tools.vaultToggle")}
                  onDetail={() => { errorDetailText = tab.errorText ?? ""; errorDetailOpen = true; }}
                />
              {:else}
                <p class="placeholder" data-testid="pending-pane">正在连接 {tab.host}…</p>
              {/if}
            {:else if tab.kind === "rdp"}
              <!-- RDP 会话：画布而不是终端。占位期（connecting）同样不挂真组件——
                   rdp:frame 事件对未知会话无害，但画布尺寸要等 Connected 才有。 -->
              {#if tab.status === "connected"}
                <RdpPane sessionId={tab.id} />
              {:else}
                <p class="placeholder" data-testid="pending-pane">
                  {tab.status === "error" ? (tab.errorText ?? "连接失败") : `正在连接 ${tab.host}…`}
                </p>
              {/if}
            {:else}
              {@const prof = profiles.find((x) => x.id === tab.profileId)}
              <TerminalPane
                sessionId={tab.id}
                onResized={(rows, cols) => { activeRows = rows; activeCols = cols; }}
                onReady={(api) => { termApis.set(tab.id, api); void flushPendingCommands(tab.id); }}
                onRunInNewTab={(cmd) => void runScriptInNewTab(cmd, tab.id)}
                onFloatFiles={() =>
                  openVWindow(
                    { kind: "sftp", sessionId: tab.id, title: `文件 — ${tab.title}` },
                    vwindowViewport(),
                  )}
                onExplainSelection={(sel) => {
                  // seq 每次递增：同一段输出再解读一次也要真的重跑，
                  // 用户的意图是「再问一遍」而不是「看上次的结果」。
                  aiPendingSelection = { text: sel, seq: ++aiSelectionSeq };
                  aiPanelOpen = true;
                }}
                onLocalAction={(actionId) => void onAction(actionId)}
                onBroadcastData={(b64) => routeBroadcastKeystroke(b64, profiles)}
                hasFiles={tab.kind !== "serial"}
                sftpLocalDir={prof?.sftp?.local_dir ?? null}
                sftpRemoteDir={prof?.sftp?.remote_dir ?? null}
                scrollbackLines={prof?.term?.scrollback_lines ?? null}
              />
            {/if}
          </div>
        {/each}
        {#if $tabs.length === 0}
          <div class="welcome" data-testid="status">
            <h1>开始连接</h1>
            <p>双击侧栏中的会话即可连接；首次使用可新建 SSH、远程桌面或串口会话。</p>
            <button class="welcome-new" onclick={() => void onAction("session.new")}>新建会话 <kbd>Ctrl+N</kbd></button>
            <button onclick={() => void onAction("import.json")}>导入会话配置…</button>
            <small>FutureShell — {status === "pong" ? "就绪" : status}</small>
          </div>
        {/if}
        <!-- M7.3 虚拟窗口：铺在终端区之上、状态栏之下。画布自己量尺寸并夹紧所有窗口，
             所以「拖不出可视区」这件事不依赖这里传什么。 -->
        <VWindowLayer
          onRunInNewTab={(cmd) => void runScriptInNewTab(cmd, $activeTabId ?? "")}
          dirsFor={(sid) => {
            const t = get(tabs).find((x) => x.id === sid);
            const p = t ? profiles.find((x) => x.id === t.profileId) : undefined;
            return { local: p?.sftp?.local_dir ?? null, remote: p?.sftp?.remote_dir ?? null };
          }}
        />
      </div>
      {#if $monitorOpen}
        <div class="monitor" data-testid="monitor"><MonitorPanel sessionId={$activeTabId} onDiagnose={diagnoseFromMonitor} /></div>
      {/if}
      <TransferQueueDrawer />
    </div>
  </div>
  {#if $composeVisible}
    <ComposeBar
      onSend={(target, text, suffix) => void sendCompose(target, text, suffix)}
      openSessions={$tabs.map((t) => ({ id: t.id, title: t.title }))}
      fill={composeFill}
    />
  {/if}
  {#if $quickbarVisible}
    <QuickBar items={quickCommands} onSend={(cmd) => void sendQuickCommand(cmd)} onManage={() => (quickDialogOpen = true)} />
  {/if}
  <!-- 终端内传输条（M4a）：只显示当前标签的传输态，其余会话的进度在各自标签下留存 -->
  {#if $activeTabId}
    <ZmodemBar sessionId={$activeTabId} view={zmodemViews[$activeTabId] ?? ZMODEM_IDLE} />
  {/if}
  {#if $statusVisible}
    <!-- R4 全参挂载：status/host 经 tabs store（Task 20 组 H）、rows/cols 经 TerminalPane resize 上报（Task 18 组 G） -->
    <StatusBar
      status={activeStatus}
      host={activeSerial ? `${activeSerial.port} · ${activeSerial.summary}` : activeHost}
      rows={activeRows}
      cols={activeCols}
      vaultLocked={vaultLocked}
      keyboardMode={($tabs.find((t) => t.id === $activeTabId))?.keyboardMode ?? "remote"}
      caps={capsOn}
      num={numOn}
      errorText={($tabs.find((t) => t.id === $activeTabId))?.errorText}
      onErrorClick={() => {
        const t = $tabs.find((x) => x.id === $activeTabId);
        if (t?.errorText) { errorDetailText = t.errorText; errorDetailOpen = true; }
      }}
      encoding={activeEncoding}
      onHostClick={activeSerial && activeStatus === "connected" ? () => (serialBaudOpen = true) : null}
      onEncodingClick={activeStatus === "connected" && $activeTabId ? () => (encodingDialogOpen = true) : null}
      onVaultClick={() => void onAction("tools.vaultToggle")}
      onKeyboardModeClick={() => { const id = $activeTabId; if (id) toggleKeyboardMode(id); }}
      onReconnect={() => { if (activeStatus === "disconnected" && $activeTabId) void invoke("session_reconnect", { sessionId: $activeTabId }).catch(() => {}); }}
      onStatusClick={$activeTabId ? () => (connDetailOpen = true) : null}
    />
  {/if}
  <SettingsDialog open={settingsOpen} initialTab={settingsTab} onClose={() => { settingsOpen = false; settingsTab = null; }} />
  <TunnelDialog open={tunnelDialogOpen} sessionId={$activeTabId} onClose={() => (tunnelDialogOpen = false)} />
  <ScheduleDialog open={scheduleOpen} profiles={profiles} onClose={() => (scheduleOpen = false)} />
  <ReplayDialog open={replayOpen} onClose={() => (replayOpen = false)} />
  <!-- 开录确认：录屏含输出明文（命令与回显原样落盘），与 sessionlog 同一隐私口径 -->
  <ConfirmDialog
    open={recordConfirmFor !== null}
    title="开始录制"
    message={'录屏将把该会话的输出原样保存到磁盘（含命令、参数与回显的明文，颜色码保留）。\n\n可在「工具 → 会话回放」中随时回放。'}
    confirmText="开始录制"
    danger
    onConfirm={() => void startRecordingConfirmed()}
    onCancel={() => (recordConfirmFor = null)}
  />
  <HistoryDialog
    open={historyOpen}
    sessionId={$activeTabId}
    host={activeHost ?? ""}
    onClose={() => (historyOpen = false)}
    onSend={(cmd) => void sendQuickCommand(cmd)}
  />
  <ConnectionDetailDialog open={connDetailOpen} sessionId={$activeTabId} onClose={() => (connDetailOpen = false)} />
  <TextInputDialog
    open={newFolderOpen}
    title="新建文件夹"
    label="路径（用 / 分层，如 工作/生产）"
    placeholder="工作/生产"
    validate={(v) => (normalizeFolderPath(v) ? undefined : (v.trim() ? "名称不合法（过深、过长或含控制字符）" : "请输入文件夹名"))}
    onConfirm={(v) => void confirmNewFolder(v)}
    onCancel={() => (newFolderOpen = false)}
  />
  <!-- 重命名文件夹：initial 填原名，用户在原名上改（多数改名只动最后一段）。
       与新建共用 TextInputDialog——同一类输入不该有两套模态。 -->
  <TextInputDialog
    open={renameFolderFrom !== null}
    title="重命名文件夹"
    label="新路径（用 / 分层；其下的连接会一并跟随）"
    initial={renameFolderFrom ?? ""}
    placeholder="工作/生产"
    validate={(v) => (normalizeFolderPath(v) ? undefined : (v.trim() ? "名称不合法（过深、过长或含控制字符）" : "请输入文件夹名"))}
    onConfirm={(v) => void renameFolderTo(v)}
    onCancel={() => (renameFolderFrom = null)}
  />
  <QuickCommandsDialog
    open={quickDialogOpen}
    items={quickCommands}
    onSave={saveQuickCommands}
    onClose={() => (quickDialogOpen = false)}
  />
  <CloseConfirmDialog
    open={!!closeConfirmState}
    scope={closeConfirmState?.scope ?? "window"}
    sessions={closeConfirmState?.sessions ?? []}
    queued={closeConfirmState?.queued ?? 0}
    running={closeConfirmState?.running ?? 0}
    onConfirm={() => closeConfirmState?.onConfirm()}
    onCancel={() => (closeConfirmState = null)}
  />
  <DeleteConfirmDialog
    open={!!deleteConfirmState}
    name={deleteConfirmState?.name ?? ""}
    onConfirm={() => deleteConfirmState?.onConfirm()}
    onCancel={() => (deleteConfirmState = null)}
  />
  <KeyManagerDialog open={keyManagerOpen} onClose={() => (keyManagerOpen = false)} />
  <AuditDialog open={auditDialogOpen} onClose={() => (auditDialogOpen = false)} />
  <AgentPanel open={agentPanelOpen} onClose={() => (agentPanelOpen = false)} sessionId={$activeTabId} />
  <AiPanel
    open={aiPanelOpen}
    onClose={() => (aiPanelOpen = false)}
    sessionId={$activeTabId}
    getScreen={aiScreenContext}
    onUseCommand={fillCompose}
    pendingSelection={aiPendingSelection}
    prefillPrompt={aiPrefillPrompt}
    onSaveSnippet={(cmd, decision) => void saveAiSnippet(cmd, decision)}
  />
  <ForeignImportDialog
    open={foreignImportOpen}
    onClose={() => (foreignImportOpen = false)}
    onImported={() => { void loadProfiles(); }}
  />
  <SessionRecoveryDialog
    open={restoreDialogOpen}
    rows={unclosedRows}
    {profiles}
    onReconnect={onRestoreSessions}
    onSkipAll={onDiscardSessions}
  />
  <ProfileDialog
    open={profileDialogOpen}
    initial={profileDialogInitial}
    initialTab={profileDialogTab}
    onClose={() => { profileDialogOpen = false; profileDialogTab = null; }}
    onSaved={async () => { profileDialogOpen = false; profileDialogTab = null; await loadProfiles(); }}
  />
  <ErrorDetailDialog
    open={errorDetailOpen}
    text={errorDetailText}
    onClose={() => (errorDetailOpen = false)}
  />
  <VaultDialog
    open={vaultDialogOpen}
    mode={vaultDialogMode}
    onClose={() => (vaultDialogOpen = false)}
    onUnlocked={async () => { vaultDialogOpen = false; vaultLocked = !(await invoke<boolean>("vault_status")); }}
  />
  <!-- 保险库管理（审计2 #27/#28）：删除记录、设置/修改应用密码、备份与恢复。
       `locked` 要传进去——「恢复」在锁定状态下也必须可用（需要恢复的时刻正是打不开库的时刻），
       而记录与改密两栏必须先解锁。恢复成功后后端会把 Store 丢掉并广播 vault:locked，
       这里的 onChanged 负责把本地那份解锁态重新取一遍，避免 UI 还显示着「已解锁」。 -->
  <VaultManagerDialog
    open={vaultManagerOpen}
    locked={vaultLocked}
    onUnlock={() => { vaultManagerOpen = false; void onAction("tools.vaultToggle"); }}
    onClose={() => (vaultManagerOpen = false)}
    onChanged={async () => { vaultLocked = !(await invoke<boolean>("vault_status")); }}
  />
  <!-- 认证 / 主机密钥确认：两者自订 `auth:prompt`、`hostkey:prompt` 并自带 promptId 回传，
       故除 bind:this（供 vault:locked 时收窗）外无任何 props。此前它们在整个应用里**没有任何挂载点**，
       后端发出的提问无人消费 —— 表现为连接卡在「连接中」直到后端超时，用户看不到任何弹窗。 -->
  <AuthPromptDialog bind:this={authDlg} />
  <HostKeyDialog bind:this={hostKeyDlg} />
  <!-- MCP 确认框：§4.5 阻塞式人机确认回路的前端落点。自监听 mcp:confirm，
       与具体面板无关（外部客户端的调用随时可能到）。 -->
  <McpConfirmDialog />
  <!-- RDP 证书确认（自监听 rdp:cert，同 McpConfirmDialog 模式） -->
  <RdpCertDialog />
  <!-- 危险动作统一确认（M7.2）：单实例，由 lib/confirm-gate.ts 的 pendingConfirm 驱动。
       挂在 Toast 之前 = z-index 之下，确认框上的 toast 仍可见。 -->
  <ActionConfirmDialog />
  <!-- M7.4 终端编码：切换只作用于这一条会话，立即生效不重连（档案里另存才长期生效）。 -->
  <EncodingDialog
    open={encodingDialogOpen}
    sessionId={$activeTabId}
    current={activeEncoding}
    onChanged={(name) => { activeEncoding = name; encodingDialogOpen = false; toast.info(`终端编码已切到 ${name}`); }}
    onCancel={() => (encodingDialogOpen = false)}
  />
  <!-- M7.4 串口波特率：立即生效、不重开端口（重开会抖 DTR，很多板子的 DTR 接在复位脚上）。 -->
  <SerialBaudDialog
    open={serialBaudOpen}
    sessionId={$activeTabId}
    port={activeSerial?.port ?? ""}
    current={activeSerial?.summary ?? ""}
    onChanged={(summary) => {
      if (activeSerial) activeSerial = { ...activeSerial, summary };
      serialBaudOpen = false;
      toast.info(`串口参数：${summary}`);
    }}
    onCancel={() => (serialBaudOpen = false)}
  />
  <Toast />
</div>

<style>
  .welcome { margin: auto; max-width: 520px; padding: 24px; color: var(--fs-fg-secondary); text-align: center; }
  .welcome h1 { font-size: 24px; color: var(--fs-fg-primary); }
  .welcome p { line-height: 1.7; }
  .welcome button { padding: 10px 16px; margin: 6px; background: var(--fs-bg-panel); color: var(--fs-fg-primary); border: 1px solid var(--fs-border); border-radius: 6px; cursor: pointer; }
  .welcome .welcome-new { background: var(--fs-accent); color: var(--fs-accent-fg); }
  .welcome small { display: block; margin-top: 18px; }
  .app { height: 100vh; display: flex; flex-direction: column; background: var(--fs-bg-app); color: var(--fs-fg-primary); }
  /* position: relative 是覆盖形态的前提：侧栏 absolute 定位要以 .body 为基准，
     不加的话它会逃到最外层，盖住菜单栏与工具栏。 */
  .body { flex: 1; display: flex; min-height: 0; position: relative; }
  .sidebar { flex: none; display: flex; flex-direction: column; background: var(--fs-bg-panel); border-right: 1px solid var(--fs-border); overflow: hidden; }
  /* 已删：`.sidebar-header` 与 `.sidebar .placeholder`。侧栏在 Task 20 组件化成 `<Sidebar />`
     之后，App 这一侧的 `.sidebar` 里只剩那个组件，两条规则再没有可命中的宿主元素，
     且 Sidebar 内部也不存在同名节点——不是「暂时没用上」，是**永远不会**用上。 */
  .sidebar-grip { flex: none; width: 4px; cursor: col-resize; background: transparent; }
  /* ── 侧栏停靠 / 浮动 / 窄屏覆盖层（M4b 第 16、18 项）──────────────────────
   *
   * 三种形态共用同一个 DOM，只换 CSS：换 DOM 会让 <Sidebar> 组件被销毁重建，
   * 于是每次切换停靠侧都会丢掉它的滚动位置与展开状态——而用户切停靠侧的时候
   * 恰恰是在整理界面，不是在重置它。
   */

  /* 右停靠：flex order 换位。.main 是 order:1，侧栏推到它后面。 */
  .sidebar.dock-right { order: 2; border-right: 0; border-left: 1px solid var(--fs-border); }
  .sidebar-grip.dock-right { order: 2; }

  /* 覆盖层：绝对定位、浮在主区之上、带投影（出口原文点名这三件事）。
     默认**收起**（translateX 移出视野）而不是 display:none —— 后者切换时没有过渡，
     而侧栏是个宽 240px 的面板，瞬间出现会让人以为界面跳了一下。 */
  .sidebar.overlay {
    position: absolute;
    top: 0;
    bottom: 0;
    /* 收起 = 整个侧栏移到 .body 左缘之外。用 left 而**不是** transform: translateX(-100%)：
       transform 会让它成为后代 position: fixed 元素的包含块——侧栏里的右键菜单与遮罩
       （Sidebar.svelte 的 .ctxmenu / .menu-overlay）都是 fixed 定位、按视口坐标摆放，被圈进这个
       240px 宽、overflow: hidden 的盒子后只剩一条几像素的竖边可见（2026-09-02 响应式核查在
       800×600 上抓到，见路线图 4c 清单第 1 条）。宽度是运行期值（160–480，可拖），所以偏移量
       走行内自定义属性 --fs-sidebar-w，与 style:width 同源。 */
    left: calc(-1 * var(--fs-sidebar-w, 240px));
    z-index: 40;
    box-shadow: var(--fs-shadow, 0 2px 16px rgba(0, 0, 0, 0.4));
    /* 收起时同时 visibility: hidden：屏外的侧栏不该还能被 Tab 走到、也不该还接收点击；
       切换延后到滑动结束（120ms），展开时立即可见。prefers-reduced-motion 下全局规则会把
       transition 清零，两处一起失效，语义不变。 */
    visibility: hidden;
    transition: left 120ms ease-out, right 120ms ease-out, visibility 0s linear 120ms;
  }
  .sidebar.overlay.overlay-open {
    left: 0;
    visibility: visible;
    transition: left 120ms ease-out, right 120ms ease-out, visibility 0s;
  }
  /* 右停靠偏好 + 覆盖形态：从右边滑出，与他选的那一侧一致。
     .dock-right 的 left: auto 必须写在两条里（收起/展开），否则展开态会被上面的 left: 0 拉成通栏。 */
  .sidebar.overlay.dock-right { left: auto; right: calc(-1 * var(--fs-sidebar-w, 240px)); }
  .sidebar.overlay.dock-right.overlay-open { left: auto; right: 0; }

  /* 遮罩：接住「点击外部收起」。z-index 在侧栏之下、主区之上。
     不给底色——一个把终端压暗的遮罩会让人以为界面被禁用了，而这里只是侧栏开着。 */
  .sidebar-scrim { position: absolute; inset: 0; z-index: 39; background: transparent; }

  /* 覆盖形态下 .body 必须是定位上下文，否则 absolute 会逃到最外层。 */

  .sidebar-grip:hover { background: var(--fs-accent-dim); }
  /* order: 1 是右停靠的支点：侧栏用 order:2 推到它后面。
     不显式写的话 .main 是默认 order:0，与侧栏并列，换位就失效了。 */
  .main { flex: 1; display: flex; flex-direction: column; min-width: 0; order: 1; }
  /* 已迁走：`.tabbar` 的高度/底色/底边框现在定义在 TabBar.svelte 自己的 `.tabbar` 规则上
     （样式作用域不跨组件，写在这里从组件化起就没生效过）。 */
  .terminal { flex: 1; min-height: 0; background: var(--fs-term-bg); position: relative; }
  .term-wrapper { position: absolute; inset: 0; display: none; }
  .term-wrapper.active { display: block; }
  .terminal .placeholder { color: var(--fs-fg-secondary); font-size: 12.5px; position: absolute; inset: 0; display: grid; place-items: center; }
  .monitor { flex: none; height: 150px; background: var(--fs-bg-panel); border-top: 1px solid var(--fs-border); }
</style>
