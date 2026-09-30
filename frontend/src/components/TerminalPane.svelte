<script lang="ts">
  import { clampToViewport } from "../lib/menu-pos"; // 右键菜单窗口边缘避让（评审 P2-8）
  import { onMount } from "svelte";
  import { getCurrentWebview } from "@tauri-apps/api/webview";
  import { clipboardRead, clipboardWrite, createTerminal, encodeB64, type TermController } from "../lib/term";
  import { invoke, listen, openExternal, reportFrontendError, settingChanged, settingGet, type UnlistenFn } from "../lib/ipc";
  import { untilUnmount, type TrackedUnlisten } from "../lib/lifecycle";
  import {
    DEFAULT_SCHEME,
    getSchemeFrom,
    parseCustomSchemes,
    resolveScheme,
    type Scheme,
    type StoredCustomScheme,
  } from "../lib/term-schemes";
  import { resolveTheme, type TermThemeOverride } from "../lib/theme/store";
  import { ringBell } from "../lib/tabs";
  import { parseHighlightRules, type HighlightRule } from "../lib/highlights"; // M4a 高亮关键字（S322）
  import { statusTransition, type SessionStatusPayload } from "../lib/session-status";
  import { toast } from "../lib/toast";
  import SearchOverlay from "./SearchOverlay.svelte";
  import SftpPane from "./SftpPane.svelte";

  let {
    sessionId,
    profileSchemeId = null,
    tempSchemeId = null,
    profileThemeOverride = null,
    onResized = null,
    onReady = null,
    onLocalAction = null,
    onBroadcastData = null,
    onExplainSelection = null,
    sftpLocalDir = null,
    sftpRemoteDir = null,
    scrollbackLines = null,
    onRunInNewTab = null,
    onFloatFiles = null,
    hasFiles = true,
  }: {
    sessionId: string;
    profileSchemeId?: string | null;
    tempSchemeId?: string | null;
    /** 审计2 #18：档案 SFTP 默认目录，透传给 SftpPane（App 按 tab.profileId 查档案装配） */
    sftpLocalDir?: string | null;
    sftpRemoteDir?: string | null;
    /** 审计2 #35：档案滚动回看行数（App 按 tab.profileId 查档案装配；null = 未设置，term.ts 按默认） */
    scrollbackLines?: number | null;
    /** M7.3：文件视图里双击 `.sh` 选「前台运行」时，请 App 开一个新终端标签跑这条命令。
     *  本组件只透传不实现——开标签要用 profile 拨号，那是 App 的事。 */
    onRunInNewTab?: ((command: string) => void) | null;
    /** M7.3：把文件视图弹成虚拟窗口。同样只透传——窗口画布在 App 的终端区之上。 */
    onFloatFiles?: (() => void) | null;
    /** M7.4：这条会话有没有 SFTP。串口没有——留着「文件」页签只会让用户点进一个
     *  恒报 `no session` 的空面板。 */
    hasFiles?: boolean;
    /** Profile 级主题字段覆盖：取自 profile 记录 settings JSON（term_blob）的 `theme_override` 键，由 Task 20 装配层读取后传入（LL2b）；不动 Task 6 schema（R11） */
    profileThemeOverride?: TermThemeOverride | null;
    /** resize 上报（rows, cols）：与 term_resize IPC 并列、仅回调不重复 IPC，供状态栏 rows×cols 段（R4，Task 20 组 H ⑦/⑩ 接线） */
    onResized?: ((rows: number, cols: number) => void) | null;
    /** 终端构建完成后回调一次，暴露终端内搜索/复制粘贴/控制器供 App 级菜单与快捷键派发器消费
     *  （Task 17/20 接线，R3；findNext = F3 续搜，R57）。
     *
     *  copySelection / pasteFromClipboard 也走这里、而不是让 App 拿 controller 自己拼：
     *  复制要经 term.ts 的 clipboardWrite 统一通道（S276），粘贴要经 pasteFlow 多行确认气泡（§4），
     *  两者的落点都在本组件内（气泡状态是组件私有的）。App 侧若用 `controller().getSelection()` +
     *  自行写剪贴板，多行粘贴确认就被绕过去了——菜单「粘贴」与右键「粘贴」会是两种行为。 */
    onReady?: ((api: {
      openSearch(): void;
      findNext(): void;
      copySelection(): void;
      pasteFromClipboard(): void;
      controller(): TermController | undefined;
    }) => void) | null;
    /** R58：终端焦点下经 shortcuts.arbitrate 判为 "local" 的键，其应用动作 id 由此回投 App onAction（该键恒不发远端） */
    onLocalAction?: ((actionId: string) => void) | null;
    /** M4a 实时键入广播（S308）：本终端的每一次键入（已编码 b64 原文）在发给自身会话之后
     *  转发一份给装配层的广播路由。App 决定开着不开、发给谁——本组件不读广播状态，
     *  「无回调 = 无广播」保持哑组件。转发的是 b64 **原文**：字节级一致是 M4a 出口
     *  标准的硬契约，中途任何解码重编都是不一致的来源。 */
    onBroadcastData?: ((dataB64: string) => void) | null;
    /** M2 选区右键「AI 解读」：把选中的这段输出回投给装配层（由它打开 AI 面板并跑解读）。
     *  无回调 = 该菜单项禁用。哑组件不知道有没有配模型源，也不该知道——
     *  「AI 能不能用」是装配层的判断，这里只负责把选区交出去。 */
    onExplainSelection?: ((selection: string) => void) | null;
  } = $props();

  /** Task 21 Step 5：终端/文件分屏切换（UI 规格 §2.4），传输队列不在标签内呈现——由全局 TransferQueueDrawer 跨标签统一呈现 */
  let view = $state<"terminal" | "files">("terminal");

  /** Task 22 Step 3: 断线 banner 状态 */
  type DisconnectPayload = { session_id: string; reason: string; attempt: number; gave_up?: boolean; exit_code?: number };
  let disconnectState = $state<DisconnectPayload | null>(null);
  let reconnectCountdown = $state<number>(0);
  let countdownTimer: ReturnType<typeof setInterval> | null = null;

  function clearCountdown() {
    if (countdownTimer) {
      clearInterval(countdownTimer);
      countdownTimer = null;
    }
  }

  function startCountdown(attempt: number) {
    clearCountdown();
    if (attempt < 1) return; // attempt:0 不显示倒计时
    const delay = Math.min(2 ** Math.max(attempt - 1, 0), 30);
    reconnectCountdown = delay;
    countdownTimer = setInterval(() => {
      reconnectCountdown = Math.max(0, reconnectCountdown - 1);
      if (reconnectCountdown <= 0) clearCountdown();
    }, 1000);
  }

  let el = $state<HTMLDivElement>();
  let ctrl: TermController | undefined = $state();
  let overlay: SearchOverlay | undefined = $state();
  /**
   * 全局层设置的组件内投影（UI 规格 §3.2 三级作用域的最外层）：onMount 首读 + `settingChanged`
   * 订阅持续跟随。下面 GLOBAL_KEYS 是这层的**完整键表**，与订阅处的分发一一对应。
   *
   * 这里原先是一段留档注释，写着「无 settings 变更订阅…Task 20/22 接线全局设置广播时必须回补
   * 本组件的写入端」。两个 Task 都收尾了，回补没发生，注释留在原地——于是 7 个全局键改完对**所有
   * 已打开的终端**一律不生效，而标签恒挂载（隐藏≠销毁），陈旧值一直续到关标签。
   * 承诺与验收成了同一段注释，这是本仓第三次同款（前两次：App.svelte S262 台账、SettingsDialog:187
   * 那句写着内部键名的提示文字）。现由 settings-wiring.test.ts 从外部把守，不再自证。
   */
  let globalSchemeId = $state<string>(DEFAULT_SCHEME.id);
  /**
   * 自定义/导入的配色（settings 键 term.customSchemes，M4b）。
   *
   * 没有这一份的话，`schemeOf` 只查得到内置 12 套，用户选中 `custom:xxx` 之后
   * 会**静默回落默认色**——他刚导进来一套配色、在设置页里选上了，终端却毫无变化，
   * 而界面上没有任何东西说明为什么。
   */
  let customSchemes = $state<StoredCustomScheme[]>([]);
  /** 全局字号：settings term.fontSize（UI 规格 §3.3，默认 13） */
  let globalFontSize = $state<number>(13);
  /** 全局背景透明度：settings term.opacity（UI 规格 §2.12 为全局级、§3.2 要求即时生效） */
  let globalOpacity = $state<number>(100);
  /** 运行期临时方案（R16 收进 M1）：装配层自 tabs.ts `transientSchemes` store 传入（`$transientSchemes.get(sessionId) ?? null`），
   *  标签右键「配色方案 ▾」子菜单设置；不写库、关标签即释放（UI 规格 §1.3/§3.2）；$derived 跟踪 prop 变更，选中即时生效 */
  const tempScheme = $derived<string | null>(tempSchemeId);
  /** 会话内存态主题覆盖（R11 transient 层，R16 收进 M1 后不再恒 null）：tempSchemeId 投影为 resolveTheme 的字段级 transient 层，
   *  经 resolveTheme 的 transient 参数消费；$effect 跟踪本派生值 → 菜单选中后三级合并不重建终端即时换色 */
  const transientOverride = $derived<TermThemeOverride | null>(tempSchemeId ? { scheme: tempSchemeId } : null);

  // 终端交互四键（UI 规格 §2.8/§2.12）：onMount 读取、经 interactions 闭包注入（term.ts 每次用时调闭包读
  // 当前值，故只要这四个 $state 被写就即时生效、不重建终端）——写入端即下方 settingChanged 订阅
  let rightClick = $state<"paste" | "menu">("paste");
  let copyOnSelect = $state(false);
  let multilinePasteConfirm = $state(true);
  let ctrlVPaste = $state(true); // R101：§2.12「Ctrl+V 粘贴开关」，默认开
  /** 「记住本会话」：组件销毁即重置 = 会话级、不持久化（UI 规格 §4） */
  let pasteRemembered = $state(false);
  let rememberThis = $state(false);
  /** 多行粘贴确认气泡状态（UI 规格 §4） */
  let pasteConfirm: { lines: number } | null = $state(null);
  /** M4a 高亮关键字规则（settings 键 term.highlights；onMount 首读 + settingChanged 跟随）。 */
  let highlightRules = $state<HighlightRule[]>([]);
  let pasteResolve: ((ok: boolean) => void) | null = null;
  /** F34 粘贴确认气泡焦点契约：绑定气泡容器，弹出时自动聚焦（Enter/Escape 快捷键生效前提） */
  let pasteConfirmEl = $state<HTMLDivElement | undefined>(undefined);
  /** 右键上下文菜单坐标（UI 规格 §2.8 条目表） */
  let ctxMenu: { x: number; y: number } | null = $state(null);

  const schemeOf = (id?: string | null): Scheme | null =>
    id ? getSchemeFrom(id, customSchemes) ?? null : null;

  onMount(() => {
    let un: TrackedUnlisten | undefined;
    let unDisconnect: UnlistenFn | undefined;
    let unStatus: UnlistenFn | undefined;
    let unSetting: (() => void) | undefined;
    // S299 待就绪缓冲：`term:data` 订阅必须在**任何 await 之前**建立，否则
    // 「7 次 settingGet + createTerminal」这段（实测约 33 ms）里后端 emit 的帧
    // 全部永久丢失——后端已把它们记进 sent_frames，前端却根本没收到，于是永远
    // 不会 ack。旧 ack 口径下这等于开局就欠一笔死账；改累计确认后债有界，但
    // 「开局丢帧」本身仍是真丢数据（终端少显示一段输出），故一并修掉。
    // ctrl 未就绪时压队列，createTerminal 完成后按序回放。
    let pendingFrames: Array<{ seq: number; b64: string }> | null = [];
    // 缓冲上限：正常窗口只有几十毫秒、至多几帧。但 ctrl 若因异常始终建不起来，
    // 无上限的缓冲就是一条内存泄漏——终端不显示、内存却随远端输出一直涨。
    // 越界丢最旧的（新内容更有价值），并且只报一次，避免每帧一条日志。
    const MAX_PENDING_FRAMES = 512;
    let overflowReported = false;
    const deliver = (seq: number, b64: string) => {
      // 不用 `ctrl?.write(...)`：可选链在 ctrl 未就绪时**静默丢帧**，正是要修的形状。
      if (pendingFrames) {
        if (pendingFrames.length >= MAX_PENDING_FRAMES) {
          pendingFrames.shift();
          if (!overflowReported) {
            overflowReported = true;
            reportFrontendError(
              "term:data buffer",
              `ctrl 长时间未就绪，待回放帧超过 ${MAX_PENDING_FRAMES}，开始丢弃最旧帧`,
            );
          }
        }
        pendingFrames.push({ seq, b64 });
        return;
      }
      try {
        ctrl?.write(seq, b64);
      } catch (e) {
        reportFrontendError("term:data", e);
      }
    };
    void (async () => {
      un = untilUnmount(listen<{ seq: number; data_b64: string }>(
        `term:data:${sessionId}`,
        (e) => deliver(e.payload.seq, e.payload.data_b64),
      ));
      // 订阅**落地**之后才往下走（原先 `await listen` 的顺序语义原样保留）；解绑函数已同步在手，
      // 卸载快于落地时 untilUnmount 会在落地当刻就地解绑（路线图 4c A2）。
      await un.ready;
      globalSchemeId = await settingGet<string>("term.scheme", DEFAULT_SCHEME.id);
      customSchemes = parseCustomSchemes(await settingGet<unknown>("term.customSchemes", []));
      globalFontSize = await settingGet<number>("term.fontSize", 13);
      globalOpacity = await settingGet<number>("term.opacity", 100); // 全局背景透明度（UI 规格 §2.12，经 opts.opacity 注入 createTerminal）
      rightClick = await settingGet<"paste" | "menu">("ui.rightClick", "paste");
      copyOnSelect = await settingGet<boolean>("ui.copyOnSelect", false);
      multilinePasteConfirm = await settingGet<boolean>("ui.multilinePasteConfirm", true);
      ctrlVPaste = await settingGet<boolean>("ui.ctrlVPaste", true); // R101/§2.12
      highlightRules = parseHighlightRules(await settingGet<string | null>("term.highlights", null));
      // 全局设置变更 → 上面这批 $state（UI 规格 §3.2「即时生效」）。订阅**在首读之后**建立：
      // 反过来（先订阅）会让首读的旧值覆盖掉读取期间到达的新值，而首读本身已取到订阅前的最新持久值。
      // 处理器只赋值、不做副作用——store 保留末值，新标签订阅时会重放最后一次变更（见 ipc.ts）。
      unSetting = settingChanged.subscribe((ev) => {
        if (!ev) return;
        const num = (fallback: number): number => (Number.isFinite(Number(ev.value)) ? Number(ev.value) : fallback);
        switch (ev.key) {
          case "term.scheme": globalSchemeId = typeof ev.value === "string" ? ev.value : DEFAULT_SCHEME.id; break;
          // 编辑器/导入改了配色内容 ⇒ 已打开的终端当场换色（出口「热加载 ≤1s 生效」）。
          // 少了这一条，用户改完一套正在用的配色要重启才看得到——而配色是要反复调的东西。
          case "term.customSchemes": customSchemes = parseCustomSchemes(ev.value); break;
          case "term.fontSize": globalFontSize = num(globalFontSize); break;
          case "term.opacity": globalOpacity = num(globalOpacity); break;
          case "ui.rightClick": rightClick = ev.value === "menu" ? "menu" : "paste"; break;
          case "ui.copyOnSelect": copyOnSelect = ev.value === true; break; // 默认关，非布尔值按默认解
          case "ui.multilinePasteConfirm": multilinePasteConfirm = ev.value !== false; break; // 默认开，同上
          case "ui.ctrlVPaste": ctrlVPaste = ev.value !== false; break; // 默认开，同上
          // M4a 高亮规则：规则表改完对**所有已打开终端**即时生效（标签恒挂载，
          // 不订阅则陈旧规则一直续到关标签——正是本文件头注记的那类缺陷）
          case "term.highlights":
            highlightRules = parseHighlightRules(
              typeof ev.value === "string" ? ev.value : JSON.stringify(ev.value ?? []),
            );
            break;
        }
      });
      // 三级配色作用域（R11）：resolveTheme 字段级浅合并（transient > profile > global）——
      // global = 全局 settings term.scheme/term.fontSize；profile = profile 记录 settings JSON 的
      // `theme_override` 键（经 profileThemeOverride prop，Task 20 装配层供给，不动 Task 6 schema）；transient = 会话内存态（R16 M1 接线：tempSchemeId prop ← tabs.ts transientSchemes store）
      const resolved = resolveTheme({
        global: { scheme: globalSchemeId, fontSize: globalFontSize },
        profile: profileThemeOverride,
        transient: transientOverride,
      });
      if (!el) {
        // 早退前必须放掉缓冲：否则 pendingFrames 永远非 null，deliver 一路只入队
        // 不投递，远端输出会把它撑成一条内存泄漏（终端还什么都不显示）。
        pendingFrames = null;
        reportFrontendError("term:data", "终端容器未挂载，渲染帧无处投递");
        return;
      }
      ctrl = createTerminal(el, {
        onData: (b64) => {
          void invoke("term_input", { sessionId, dataB64: b64 });
          // S308：自身会话已发（上面这行），广播转发是**附加**副本——顺序必须
          // 自身在前：广播目标解析以「活动会话」为基准（分组模式含自身），先发
          // 自身再转发，两个通道对同一击键的到达序才与单发一致。
          onBroadcastData?.(b64);
        },
        onResize: (cols, rows) => {
          void invoke("term_resize", { sessionId, cols, rows });
          onResized?.(rows, cols); // R4：状态栏 rows×cols 段（App activeRows/activeCols → StatusBar，Task 20 组 H ⑦/⑩ 接线），仅回调不重复 IPC
        },
        // S295：回帧 seq（累计确认），不再上报字节数。
        // S300：不再 `void` 吞掉 rejection——term_ack 对未知会话返回 Err("no session")，
        // 静默吞掉它就等于把「ack 根本没入账」这条线索抹干净。
        onAck: (seq) => { invoke("term_ack", { sessionId, seq }).catch((e) => reportFrontendError("term_ack", e)); },
        onDecodeError: (seq, message) => reportFrontendError("term:data decode", `seq=${seq}: ${message}`),
        onBell: () => ringBell(sessionId), // 前端侧检测，不给 core 管道加事件（UI 规格 §5）
        // M4a 高亮关键字的提醒联动（UI 规格 §2.11）：与 bell 共用角标通道——
        // 用户要的是「有事发生了」这一个信号，两套角标只会互相盖掉。
        onHighlightAlert: () => ringBell(sessionId),
        onLink: (url) => { void openExternal(url); }, // http/https/mailto 白名单由 Rust 侧校验（UI 规格 §2.8）
        onContextMenu: (x, y) => { ctxMenu = { x, y }; },
      }, {
        sessionId, // R106/F25：per-session 键盘模式仲裁键（term.ts 内 tabKeyboardMode/toggleKeyboardMode 消费）
        scheme: resolveScheme(schemeOf(resolved.scheme), schemeOf(profileSchemeId), schemeOf(tempScheme)),
        fontSize: resolved.fontSize,
        opacity: globalOpacity,
        scrollback: scrollbackLines, // 审计2 #35：档案滚动行数贯通 xterm（term.ts 钳位兜底）
        // M4a 高亮关键字（UI 规格 §2.11）：闭包读组件 $state，设置页改完下一批
        // 输出即生效、不重建终端（与全局配色/字号同款「即时生效」口径）。
        highlights: () => highlightRules,
        interactions: () => ({ rightClick, copyOnSelect, multilinePasteConfirm, ctrlVPaste }),
        pasteRemembered: () => pasteRemembered,
        onLocalAction: onLocalAction ?? undefined, // R58：终端内仲裁出的应用动作回投装配层 onAction
        // S283（二轮复审整改）：以组件 $state 为气泡悬置的权威判定——重入时旧泡被 false 结算、新泡
        // 立即置位，pasteConfirm !== null 恒如实；优先于 term.ts 内部计数（免疫微任务时序陈旧）。
        pasteConfirmPending: () => pasteConfirm !== null,
        requestPasteConfirm: (lines) => new Promise<boolean>((resolve) => {
          // S272（复审 #1/#8）：重入守卫——气泡未作答时可经右键 paste 模式/菜单「粘贴」按钮再入多行粘贴，
          // 原单槽覆写令第一条 pasteFlow 的 Promise 永挂（该次粘贴静默丢失）。先以 false 结算旧泡
          // （旧粘贴按取消处理）再装新泡，保证同一时刻至多一个待决确认、无悬挂 Promise。
          pasteResolve?.(false);
          rememberThis = false;
          pasteResolve = resolve;
          pasteConfirm = { lines };
        }),
      });
      // ctrl 就绪：把订阅建立以来缓冲的帧按序回放，然后转入直投（S299）。
      // 置 null 先于回放：回放期间到达的新帧走 deliver 的直投支路，天然排在
      // 回放之后——若先回放再置 null，这中间到达的帧会被追加进已在遍历的数组，
      // 顺序仍对但边界依赖遍历实现，不如显式切换清楚。
      const buffered = pendingFrames ?? [];
      pendingFrames = null;
      for (const f of buffered) {
        try {
          ctrl?.write(f.seq, f.b64);
        } catch (e) {
          reportFrontendError("term:data replay", e);
        }
      }

      // Task 22 Step 3: 监听 session:disconnected 与 session:status
      unDisconnect = untilUnmount(listen<DisconnectPayload>("session:disconnected", (e) => {
        if (e.payload.session_id === sessionId) {
          disconnectState = e.payload;
          startCountdown(e.payload.attempt);
        }
      }));
      // 判据从 `message.includes("已重连")` 换成后端显式的 state 字段：重连期间 connect() 会打好几行
      // 进度文案（跳板第几跳 / 尝试哪种认证方法 / 拒连理由），它们与「已经重连上了」共用同一个事件名，
      // 靠文案子串区分等于把一句 UI 文字变成不可改的协议。见 lib/session-status.ts。
      unStatus = untilUnmount(listen<SessionStatusPayload>("session:status", (e) => {
        if (e.payload.session_id === sessionId && statusTransition(e.payload) === "connected") {
          disconnectState = null;
          clearCountdown();
        }
      }));

      onReady?.({
        openSearch: () => overlay?.openSearch(),
        findNext: () => overlay?.findNextAgain(), // R57：F3 续搜（查询词存于浮层内部）
        copySelection: () => copySelection(),
        pasteFromClipboard: () => void pasteFromClipboard(),
        controller: () => ctrl,
      });
    })();
    return () => {
      un?.();
      unDisconnect?.();
      unStatus?.();
      unSetting?.(); // 不退订则每关一个标签就留一个持有已 dispose 终端的处理器（store 订阅表只增不减）
      clearCountdown();
      pasteResolve?.(false);
      pasteResolve = null;
      ctrl?.dispose();
    }; // S272：卸载兜底结算悬挂的粘贴确认（false = 取消，终端已 dispose 不得 paste）
  });

  function answerPaste(ok: boolean): void {
    if (rememberThis) pasteRemembered = true; // 记住后本会话直通，不再弹气泡
    pasteConfirm = null;
    pasteResolve?.(ok);
    pasteResolve = null;
    ctrl?.focus(); // F34：气泡关闭后焦点归还终端（键盘契约焦点闭环）
  }

  // F34：气泡弹出即自动聚焦（tabindex="-1" 容器接收 keydown，Enter 确认 / Escape 取消）
  $effect(() => { if (pasteConfirm) pasteConfirmEl?.focus(); });

  // S276（复审 #10）：改经 term.ts 的 clipboardWrite/clipboardRead 统一通道——剪贴板实现期择一
  // （Tauri capabilities 或 arboard IPC）时一处切换全路径生效，原直连 navigator.clipboard 会绕过新通道
  function copySelection(): void {
    const sel = ctrl?.getSelection() ?? "";
    if (sel) void clipboardWrite(sel);
  }

  async function pasteFromClipboard(): Promise<void> {
    const text = await clipboardRead();
    if (text) ctrl?.pasteText(text); // 经 pasteFlow 多行确认仲裁
  }

  // S270（复审 #6，medium）：per-session 字号缩放只活在 term.options.fontSize（zoom/resetFont 读写它），
  // 不在任何被本 $effect 追踪的响应式值里。若无守卫，任一依赖变化（临时配色、profile 覆盖更新等
  // **scheme-only** 触发）都会以合并基线 setFontSize，把用户 Ctrl+滚轮/Ctrl+= 缩放静默打回——
  // 违反 §3.2「切换只影响 theme 与 drawBoldTextInBrightColors」的作用域限定与 §3.3 per-session 契约。
  let lastAppliedFontSize: number | undefined;
  // 三级配色作用域再应用（R11 + UI 规格 §3.2）：settings 变更（globalSchemeId/globalFontSize 写回）或 profile 覆盖/
  // transient 变化 → resolveTheme 字段级浅合并（transient > profile > global）→ resolveScheme 三级择一；
  // runtime options 写入即时生效不重建（与会话建立 onMount 首应用同一管线）
  $effect(() => {
    if (!ctrl) return;
    const resolved = resolveTheme({
      global: { scheme: globalSchemeId, fontSize: globalFontSize },
      profile: profileThemeOverride,
      transient: transientOverride,
    });
    // 透明度先于配色写入：两者合成同一个 theme 对象，setOpacity 等值早退，故稳态下只有 setScheme 那一次写
    ctrl.setOpacity(globalOpacity);
    ctrl.setScheme(resolveScheme(schemeOf(resolved.scheme), schemeOf(profileSchemeId), schemeOf(tempScheme)));
    if (resolved.fontSize !== lastAppliedFontSize) { // S270：仅字号实际变化才重写，scheme-only 变化不碰缩放
      lastAppliedFontSize = resolved.fontSize;
      ctrl.setFontSize(resolved.fontSize);
    }
  });

  // 从文件视图切回终端：终端 div 恒挂载但 display:none 期间尺寸可能变过（窗口缩放/面板比例），
  // 重显后 fit 一次重测尺寸——否则可能出现「内容还在但网格尺寸对不上」的错位。
  $effect(() => {
    if (view === "terminal" && ctrl) {
      requestAnimationFrame(() => ctrl?.fit());
    }
  });

  /**
   * 拖本地文件到终端 = 上传（对标 Xshell 的拖拽上传）。
   *
   * 目标目录取会话远端根（SFTP 会话的起始目录，即用户家目录）——终端 shell 的 cwd 并未被跟踪，
   * 拿它当上传目的地是不可靠的；远端根是唯一有确定语义的落点。同名已存在则二次确认覆盖。
   *
   * 路径来源是 Tauri 的 webview 拖放事件（`onDragDropEvent`），不是 HTML5 `dataTransfer`——
   * 后者只有 `File` 对象、拿不到本地绝对路径，而 `transfer_submit` 要的是本地路径。
   */
  async function uploadDropped(localPath: string) {
    const base = localPath.split(/[\\/]/).pop() || localPath;
    const remote = "./" + base;
    try {
      const list = await invoke<{ entries: { name: string }[]; truncated: boolean }>("sftp_list", {
        sessionId, path: ".",
      });
      const exists = list.entries.some((e) => e.name === base);
      if (exists && !window.confirm(`远端已存在「${base}」，是否覆盖？`)) return;
    } catch {
      // 列不到远端目录就不做重名预检，直接交给上传（引擎按目标路径处理）
    }
    try {
      await invoke("transfer_submit", {
        sessionId, direction: "up", local: localPath, remote, resumeOffset: 0,
      });
      toast.info(`已开始上传 ${base}`);
    } catch (e) {
      toast.error(`上传失败：${e}`);
    }
  }

  $effect(() => {
    // 与 listen 同一条竞态（路线图 4c A2）：onDragDropEvent 也是异步落定的解绑函数，走同一个 untilUnmount。
    const un = untilUnmount(getCurrentWebview()
      .onDragDropEvent((ev) => {
        // Tauri v2 的 DragDropEvent 是判别联合：drop 带 paths，enter/over/leave 不带。
        const p = ev.payload as { type?: string; paths?: string[] };
        if (p.type !== "drop") return;
        if (view !== "terminal") return; // 只在终端视图接受拖拽上传（文件视图走 SftpPane 自己的拖拽）
        if (!p.paths?.length) return;
        for (const path of p.paths) void uploadDropped(path);
      }));
    return () => un();
  });
</script>

<!-- S267：右键菜单的关闭改挂窗口级。原实现把 onclick 挂在 xterm 挂载 div 上，既触发 svelte-check 两条
     a11y 告警（非交互 div 带 click 却无键盘等价物、无 ARIA role），也确实漏了两条关闭路径：点击终端
     面板以外任何位置、以及 Esc，菜单都不关而是一直浮着。挂窗口级后两条路径齐备，且键盘等价物
     （Esc）正是那条 a11y 告警要的东西 ⇒ 告警随行为补齐一并消失，非压制。
     不会自关：菜单由 contextmenu 事件开，右键不产生 click；菜单项自身 click 冒泡到 window 时其
     onclick 已先置 null，重复置空无害。
     S275（复审 #23）：补 pointerdown 关闭——click 在 mouseup 后才派发，打开菜单后按住左键拖选文本
     期间菜单会一直悬浮；pointerdown 起手即关（菜单内部的 pointerdown 豁免，保住菜单项的完整
     down→up→click 生命周期）。click 路径保留为冗余无害。
     留档未修（复审 #22）：macOS 的 Ctrl+点击右键手势（button=0）会在 contextmenu 之后再派发
     一次 click，同一手势把刚开的菜单秒关；且 Ctrl+点击同是 web-links 打开手势。当前平台 Windows
     WebView2 不触发，留 Task 20 平台验收合并处理（如需即修：开菜单时记手势时间戳、window click
     对同手势免疫）。
     S272（复审 #20）：窗口级键盘兜底——粘贴气泡的 onkeydown 只挂在气泡 div 上，气泡弹出后用户点击
     终端查看粘贴落点（焦点转 xterm textarea）即「焦点漂移」，此后 Esc 不抵气泡（取消失效）、Enter
     更会被 xterm 透传为 \r 误发远端。故 pasteConfirm 非空时窗口级消费 Enter/Escape。气泡自身
     onkeydown 已 stopPropagation（见下），焦点在气泡时窗口层收不到该事件，两层互斥不双结算。
     S284（二轮复审整改，中×2 合并）：窗口级结算必须加窗格归属判别，否则两类越权：
     ① Task 20 多标签装配（隐藏标签恒挂载、N 个 svelte:window 监听并存）下，标签 B 的 Enter 会命中
        隐藏标签 A 的分支、把 A 排队的多行内容灌进 A 会话（用户全程面对 B）；
     ② 气泡悬置期打开 Settings/ComposeBar 等共存输入面，输入框内 Enter 冒泡至 window 同样误结算。
     判据用归属而非 tagName：e.target 必须在本窗格 el 内（xterm helper textarea 在 el 内，恰好保住
     S272 焦点漂移主用例；App 式 inField 守卫按 INPUT/TEXTAREA 排除会误伤该正用例）。气泡/右键菜单
     是 el 兄弟节点，但气泡焦点路径由其自身 onkeydown 承包（stopPropagation），不经此分支。
     S288 留档（三轮复审，low，不修）——该判别对 body 焦点的窄化：若用户在气泡悬置期点击应用
     非可聚焦区域（工具栏空隙/状态栏文本等），焦点落到 document.body、不在任何 el 内，此后 Enter/Esc
     不再结算气泡，S272 所修的「Esc 取消不可达」在这一支重新出现。评估为可接受取舍：① 气泡弹出即
     自动聚焦（F34），漂移的现实路径是点终端 → 落在 el 内的 xterm textarea，主用例已覆盖；② 气泡的
     粘贴/取消两个按钮始终可点，无死锁、无数据损失、无错误动作。不即修的技术原因：body 焦点无法
     归属到具体窗格——多标签恒挂载下每个窗格都会认领它，正是 S284 要消灭的越权；安全处理需要「本窗格
     是否为活动标签」的信号，而该信号归 Task 20 装配层所有（{#each $tabs} + activeTabId）。
     承接：Task 20 装配时以活动标签判别补齐此支（活动窗格才认领 body 焦点的 Enter/Esc）。 -->
<svelte:window
  onpointerdown={(e) => { if (ctxMenu && !(e.target instanceof Element && e.target.closest(".ctx-menu"))) ctxMenu = null; }}
  onclick={() => (ctxMenu = null)}
  onkeydown={(e) => {
    if (pasteConfirm && e.target instanceof Node && el?.contains(e.target)) { // S284：仅本窗格内的键事件结算本窗格气泡
      if (e.key === "Enter") { e.preventDefault(); e.stopPropagation(); answerPaste(true); return; }
      if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); answerPaste(false); return; }
    }
    if (ctxMenu && e.key === "Escape") { e.preventDefault(); ctxMenu = null; }
  }}
/>

<div class="terminal-root">
  <div class="view-switcher">
    <button class:active={view === "terminal"} onclick={() => (view = "terminal")}>终端</button>
    {#if hasFiles}
      <button class:active={view === "files"} onclick={() => (view = "files")}>文件</button>
    {/if}
  </div>

{#if disconnectState}
  <div class="disconnect-banner" class:gave-up={disconnectState.gave_up} role="alert">
    {#if disconnectState.attempt === 0}
      连接断开{disconnectState.reason === "exit_nonzero" && disconnectState.exit_code ? `（远端退出码 ${disconnectState.exit_code}）` : ""}，准备重连…
    {:else if disconnectState.gave_up}
      <span class="dot-danger" aria-hidden="true">●</span> <strong>重连失败（已达上限）</strong>
      <button onclick={() => void invoke("session_reconnect", { sessionId })}>立即重连</button>
    {:else}
      正在重连（第 {disconnectState.attempt} 次）{disconnectState.reason === "exit_nonzero" && disconnectState.exit_code ? `（远端退出码 ${disconnectState.exit_code}）` : ""}
      {#if reconnectCountdown > 0}
        — 下次尝试 {reconnectCountdown}s
      {/if}
      <button onclick={() => void invoke("session_reconnect", { sessionId })}>立即重连</button>
      <button onclick={() => void invoke("session_reconnect_stop", { sessionId })}>停止重连</button>
    {/if}
  </div>
{/if}

<!-- 终端 div 恒挂载、只切可见性：切换文件视图若把它卸载，xterm 的 DOM 与回看缓冲随之销毁，
     切回来就是一个空 div（onMount 的 createTerminal 不会再跑一次）。文件视图以 {#if} 按需挂载即可，
     它的列表状态本来就可重建。 -->
<div class="terminal-pane" class:hidden={view === "files"} bind:this={el}></div>
{#if view === "terminal" && ctrl}
  <SearchOverlay bind:this={overlay} controller={ctrl} />
{/if}
{#if view === "files"}
  <SftpPane
    {sessionId} localDir={sftpLocalDir} remoteDir={sftpRemoteDir}
    onRunInNewTab={(cmd) => onRunInNewTab?.(cmd)}
    onFloat={onFloatFiles
      ? () => {
          onFloatFiles();
          // 弹出去之后本窗格切回终端：文件视图已经在浮动窗口里了，留在文件视图上
          // 等于同一份内容并排两遍，而用户点「浮动」的意思是「我要同时看终端和文件」。
          view = "terminal";
        }
      : null}
  />
{/if}

{#if pasteConfirm}
  <!-- S272/#21：stopPropagation 保住「一次按键只结算一层」——焦点在气泡时本处理器先于窗口级兜底运行，
       若不拦冒泡，同一 Enter/Escape 会再命中 svelte:window 分支（双结算，且 Esc 会顺带关掉共存菜单） -->
  <div class="paste-confirm" role="alertdialog" aria-label="粘贴确认" tabindex="-1" bind:this={pasteConfirmEl} onkeydown={(e) => { if (e.key === "Enter") { e.preventDefault(); e.stopPropagation(); answerPaste(true); } else if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); answerPaste(false); } }}>
    <span>将向会话粘贴 {pasteConfirm.lines} 行，可能立即执行</span>
    <label><input type="checkbox" bind:checked={rememberThis} /> 记住本会话</label>
    <button class="primary" onclick={() => answerPaste(true)}>粘贴</button>
    <button onclick={() => answerPaste(false)}>取消</button>
  </div>
{/if}

{#if ctxMenu}
  <div class="ctx-menu" use:clampToViewport style="left: {ctxMenu.x}px; top: {ctxMenu.y}px" role="menu">
    <button role="menuitem" onclick={() => { ctxMenu = null; copySelection(); }}>复制</button>
    <button role="menuitem" onclick={() => { ctxMenu = null; void pasteFromClipboard(); }}>粘贴</button>
    <button role="menuitem" onclick={() => { ctxMenu = null; ctrl?.selectAll(); }}>全选</button>
    <hr />
    <button role="menuitem" onclick={() => { ctxMenu = null; overlay?.openSearch(); }}>搜索 <span class="kbd">Ctrl+F</span></button>
    <hr />
    <!-- 「AI 解读」（M2 出口第 12 项，UI 规格 §2.8）。
         选区为空时禁用而不是隐藏：这一项在菜单里的位置是固定的，
         隐藏会让菜单在有无选区时高度不同，右键两次条目跳位置。
         判空在这里做（`ctrl?.getSelection()`）而不是交给 App：
         App 收到回调时选区可能已经被右键本身清掉了。 -->
    <button
      role="menuitem"
      disabled={!onExplainSelection || !(ctrl?.getSelection() ?? "").trim()}
      title="把选中的这段输出交给 AI 解释（Ctrl+Shift+A 打开面板）"
      onclick={() => {
        const sel = ctrl?.getSelection() ?? "";
        ctxMenu = null;
        if (sel.trim()) onExplainSelection?.(sel);
      }}>AI 解读 ✨</button>
    <hr />
    <!-- .catch 不可省：会话已终止时 term_input 会 reject，裸 void 只留下一条无人看见的 unhandled rejection -->
    <button role="menuitem" onclick={() => { ctxMenu = null; void invoke("term_input", { sessionId, dataB64: encodeB64(new TextEncoder().encode("\x0c")) }).catch((e) => toast.error(`清屏失败：${e}`)); }}>清屏</button>
  </div>
{/if}
</div>

<style>
  /* 布局根：view-switcher/断线横幅是固定高，终端与文件视图吃满剩余高度。
     旧实现没有这个 flex 列容器，`.terminal-pane{height:100%}` 与上方的 view-switcher
     是平级兄弟——100% 再叠加 ~30px 的切换条，底部溢出与下方状态栏重叠（文件视图同病）。 */
  .terminal-root { height: 100%; display: flex; flex-direction: column; position: relative; }
  .view-switcher { flex: none; display: flex; gap: 4px; padding: 4px 8px; background: var(--fs-bg-panel); border-bottom: 1px solid var(--fs-border); }
  .view-switcher button { background: none; border: 0; padding: 4px 12px; color: var(--fs-fg-secondary); cursor: pointer; border-radius: var(--fs-radius); }
  .view-switcher button.active { background: var(--fs-bg-elevated); color: var(--fs-fg-primary); }
  .view-switcher button:hover:not(.active) { background: var(--fs-bg-hover); }
  /* 断线横幅（2026-08-31 评审后重做）：浅色底纹 + 状态色左边框 + 主文字。
   * 原样是「状态色作整块背景 + 主文字」，四主题实测对比度 2.11–3.22:1，
   * 全部低于 WCAG AA 的 4.5:1；且用的 --fs-warning 是不存在的令牌
   * （实际叫 --fs-warn），横幅背景整条失效。底纹令牌进对比度核验
   * （theme.test.ts 的 DIM_COMBOS）。 */
  .disconnect-banner { flex: none; background: var(--fs-warn-dim); border-left: 3px solid var(--fs-warn); color: var(--fs-fg-primary); padding: 6px 12px; font-size: 12px; display: flex; align-items: center; gap: 8px; }
  .disconnect-banner.gave-up { background: var(--fs-danger-dim); border-left-color: var(--fs-danger); }
  /* 高对比度（房规见 styles.css）：两态的底色（warn-dim / danger-dim）被抹平，只剩同宽的左条。
     文案本身已经不同（「连接已断开」vs「重连失败（已达上限）」），但左条是横幅的严重度信号，
     加粗成 double 让「已放弃」在扫一眼时就不同。
     本构件要有真实会话才挂载，动态核查（scripts/forced-colors-audit.mjs）量不到它，
     判据落在 lib/forced-colors-guards.test.ts 的结构守卫上——如实记在路线图里。 */
  @media (forced-colors: active) {
    .disconnect-banner.gave-up { border-left-width: 6px; border-left-style: double; }
  }
  /* 失败态强调：**粗体**而不是危险色文字——danger 作文字在 daylight 实测
   * 4.28:1（hc 6.37:1）不达标（theme.test.ts 有实测记录），状态色只走
   * 左边框与圆点（非文本构件 3:1）。圆点是 aria-hidden 的装饰。 */
  .disconnect-banner.gave-up strong { font-weight: 700; }
  .dot-danger { color: var(--fs-danger); }
  .disconnect-banner button { background: var(--fs-bg-elevated); border: 1px solid var(--fs-border); color: var(--fs-fg-primary); padding: 2px 8px; border-radius: var(--fs-radius); cursor: pointer; font-size: 11px; }
  .disconnect-banner button:hover { background: var(--fs-bg-hover); }
  .terminal-pane { flex: 1; min-height: 0; width: 100%; position: relative; }
  .terminal-pane.hidden { display: none; }
  .paste-confirm { position: absolute; top: 8px; left: 50%; transform: translateX(-50%); z-index: 11; display: flex; align-items: center; gap: 8px; padding: 6px 10px; background: var(--fs-bg-elevated); border: 1px solid var(--fs-border-strong); border-radius: var(--fs-radius); box-shadow: var(--fs-shadow); }
  .ctx-menu { position: fixed; z-index: 40; min-width: 140px; padding: 4px; background: var(--fs-bg-elevated); border: 1px solid var(--fs-border-strong); border-radius: var(--fs-radius); box-shadow: var(--fs-shadow); display: flex; flex-direction: column; }
  .ctx-menu button { border: none; background: none; color: var(--fs-fg-primary); text-align: left; padding: 4px 8px; border-radius: 3px; cursor: pointer; font: inherit; }
  .ctx-menu button:hover:not(:disabled) { background: var(--fs-bg-panel); }
  .ctx-menu button:disabled { opacity: 0.5; cursor: default; }
  .ctx-menu hr { border: none; border-top: 1px solid var(--fs-border); margin: 4px 0; }
  .kbd { opacity: 0.6; font-size: 0.9em; }
</style>
