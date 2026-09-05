<script lang="ts">
  /**
   * RDP 桌面画布（阶段 1）。
   *
   * 三件事：**帧绘制**（rdp:frame 事件 → base64 → putImageData）、
   * **输入捕获**（鼠标坐标换算 + KeyboardEvent.code → PS/2 扫描码）、
   * **尺寸跟随**（容器变化 → canvas CSS 尺寸，位图尺寸保持远端桌面分辨率）。
   *
   * # 为什么用扫描码而不是字符
   *
   * RDP 传 PS/2 Set 1 扫描码；浏览器的 KeyboardEvent.code 是**与布局无关的
   * 物理键位**——两者一一对应。这样远端的键盘布局/输入法才是生效的那一个：
   * 用户在远端开了中文输入法，按下的就是他自己布局下的那个键。
   * 传字符（UnicodeKeyboardEvent）会把布局钉死在本地。
   *
   * # 缩放
   *
   * 位图 = 远端分辨率（事件里的 w/h）；CSS 把它等比缩放进容器。
   * 鼠标坐标按「位图尺寸 / 显示尺寸」反算——否则在缩小的画布上点击
   * 会偏到右下方（分母用了位图宽）。
   *
   * # 阶段 1 的明知之选
   *
   * base64 over 事件：单帧几十 KB 可接受，1080p 全屏一帧 ≈11MB 是天花板
   * （后端 rdp.rs 模块头记录了同一笔账）。阶段 2 换 ipc::Channel raw。
   */
  import { onMount, onDestroy } from "svelte";
  import { invoke, listen, reportFrontendError } from "../lib/ipc";
  import { untilUnmount } from "../lib/lifecycle";
  import { RdpAudioPlayer } from "../lib/rdp-audio";
  import { claimFramePainter, createFrameChannel, decodeFrame, dropFrameChannel, hasFrameChannel, rebindFrameChannel, releaseFramePainter } from "../lib/rdp-frames";
  import { replaceTabId, setTabStatus } from "../lib/tabs";
  import FilePickerDialog from "./FilePickerDialog.svelte";
  import { confirmAction } from "../lib/confirm-gate";
  import { toast } from "../lib/toast";

  let { sessionId }: { sessionId: string } = $props();

  let canvas = $state<HTMLCanvasElement | undefined>();
  let ctx: CanvasRenderingContext2D | null = null;
  /** 位图分辨率（rdp:connected 事件定格；canvas 的 width/height 属性） */
  let bmW = $state(1280);
  let bmH = $state(800);
  let unlistenFrame: (() => void) | null = null;
  let unlistenClosed: (() => void) | null = null;
  let unlistenConnected: (() => void) | null = null;
  /** 音频（阶段 3）：播放在前端（Web Audio），见 lib/rdp-audio.ts 的模块头。 */
  const audio = new RdpAudioPlayer();
  let unlistenAudioFmt: (() => void) | null = null;
  let unlistenAudio: (() => void) | null = null;
  let unlistenAudioClose: (() => void) | null = null;
  /** 意外断线（非远端优雅收尾）：显示重连入口。 */
  let closedUnexpectedly = $state(false);
  let reconnecting = $state(false);
  // ── 共享目录（RDPDR 批）──────────────────────────────────────────
  let sharePickerOpen = $state(false);
  /** 已挂的槽位（rdp_share_status / rdp:share 事件维护）。 */
  let shares = $state<{ device: number; mounted: boolean; readonly: boolean }[]>([]);

  async function refreshShares(): Promise<void> {
    try {
      const r = await invoke<{ device: number; mounted: boolean; readonly: boolean }[]>(
        "rdp_share_status",
        { sessionId },
      );
      // Array.isArray 兜底：后端形状异常（旧版本/桩返回 undefined）时退空表，
      // 而不是让模板对着 undefined 取 .length——那会把整个画布炸掉。
      shares = Array.isArray(r) ? r : [];
    } catch {
      shares = [];
    }
  }

  /**
   * 挂载三步：选目录 → 确认闸（fs.share）→ rdp_share_mount。
   *
   * 只读开关放在**选目录之前**（挂在选择器旁边）：确认框里写「远端将能读写此目录」
   * 还是「只能读取」，取决于用户此刻的选择——顺序反了就要二次确认，而那一次
   * 用户多半直接点确定。
   */
  let wantReadonly = $state(true);

  async function doMount(dir: string, readonly: boolean): Promise<void> {
    const slot = shares.length + 1; // 1..=4
    const ok = await confirmAction({
      kind: "fs.share",
      title: readonly ? "共享目录（只读）" : "共享目录（可写）",
      command: `远端桌面将${readonly ? "只读" : "读写"}本机目录：${dir}`,
      note: readonly
        ? "远端可以浏览和读取这个目录的内容，不能修改。"
        : "远端桌面里的任何程序都可以读取、修改、删除这个目录里的文件。每个文件的打开与写入都会写审计。",
      danger: !readonly,
      sessionId,
    });
    if (!ok) return;
    try {
      await invoke("rdp_share_mount", { sessionId, device: slot, dir, readonly });
      await refreshShares();
    } catch (e) {
      toast.error(`挂载失败：${e}`);
    }
  }

  async function doUnmount(device: number): Promise<void> {
    try {
      await invoke("rdp_share_unmount", { sessionId, device });
      await refreshShares();
    } catch (e) {
      toast.error(`卸载失败：${e}`);
    }
  }

  /** raw 载荷（lib/rdp-frames.ts 的通道送来的）→ 与事件路径同一个 paint 核心。 */
  function paintRaw(buf: ArrayBuffer): void {
    const f = decodeFrame(buf);
    paintBytes({ x: f.x, y: f.y, w: f.w, h: f.h, rgba: f.rgba });
  }

  onMount(async () => {
    // 4d raw 帧通道：openSession 在 connect 前就建好了通道（lib/rdp-frames.ts 的表），
    // 这里认领它。认领前到达的帧被表缓存着——先补画那一块，再等新帧。
    // 没有通道（MCP 开的会话 / 前端被降级到事件路径）时落回事件监听，两条路径
    // 不会并存：通道在表里，后端就一定在往它发（两端的对齐见 rdp.rs emit_frame）。
    if (hasFrameChannel(sessionId)) {
      const missed = claimFramePainter(sessionId, (buf) => {
        try {
          paintRaw(buf);
        } catch (err) {
          reportFrontendError("rdp:paint", err);
          void invoke("rdp_request_full_frame", { sessionId }).catch(() => {});
        }
        void invoke("rdp_frame_ack", { sessionId }).catch(() => {});
      });
      if (missed) {
        try {
          paintRaw(missed);
        } catch { /* 缓存帧画不上就等整屏：这里没有可报告的「刚发生」上下文 */ }
        // 缓存的那一帧当初没经过 painter（当时还没有画布），ack 一次也没发过。
        // helper 侧是在途配额计数：漏这一次，配额永久少一格；漏两次画面冻死
        // 而鼠标键盘还通（与事件路径同一笔账，见上方 painter 的注释）。
        void invoke("rdp_frame_ack", { sessionId }).catch(() => {});
      }
    }
    unlistenFrame = hasFrameChannel(sessionId)
      ? null
      : untilUnmount(listen<{ x: number; y: number; w: number; h: number; rgbaB64: string }>(
      `rdp:frame:${sessionId}`,
      (e) => {
        // **回执必须无条件发出**——这是本组件最容易写错、错了最难查的一处。
        //
        // helper 那侧是配额计数（在途上限 2）：漏一次回执，配额就少一个；
        // 漏两次，flush 永远在第一行 return —— 画面**永久冻死，而鼠标键盘
        // 还通**，日志里一个字都没有。paint 里 atob / new ImageData /
        // putImageData 任何一处抛异常（载荷损坏、尺寸不符、内存不足）
        // 都会跳过 ack，所以 try/catch 是承重的，不是防御性编程。
        //
        // 但只补 ack 不够：帧是**增量**的，画失败的那块像素没有任何机制会
        // 自愈（协议里也没有「重发这一块」）。所以失败时另外索要一次整屏。
        try {
          paint(e.payload);
        } catch (err) {
          // 与终端解码失败同款处理（见 lib/term.ts 的 onDecodeError）：
          // 静默吞掉是 2026-08-19 渲染帧丢失事故的根因，不重蹈。
          reportFrontendError("rdp:paint", err);
          void invoke("rdp_request_full_frame", { sessionId }).catch(() => {});
        }
        void invoke("rdp_frame_ack", { sessionId }).catch(() => {});
      },
    ));
    unlistenConnected = untilUnmount(listen<{ width: number; height: number }>(
      `rdp:connected:${sessionId}`,
      (e) => {
        bmW = e.payload.width;
        bmH = e.payload.height;
        ctx = canvas?.getContext("2d") ?? null;
        // 改 canvas 的 width/height 会**清空**画布，而帧是增量的——
        // 不索要整屏，用户会盯着一块空白等到远端自己重绘那一片为止。
        // （重激活改分辨率时这条事件会再来一次，正是需要它的时刻。）
        void invoke("rdp_request_full_frame", { sessionId }).catch(() => {});
      },
    ));
    unlistenClosed = untilUnmount(listen<{ sessionId: string; graceful: boolean }>(
      `rdp:closed:${sessionId}`,
      (e) => {
        // graceful = 远端主动收尾（用户注销/被踢），**不**提示重连——
        // 用户就是要退出，再弹一个「重新连接？」是添乱。
        // 非 graceful = 通道断了（网络/helper 挂了），才给重连入口。
        closedUnexpectedly = !e.payload.graceful;
      },
    ));
    unlistenAudioFmt = untilUnmount(listen<{ sampleRate: number; channels: number; bitsPerSample: number }>(
      `rdp:audio-format:${sessionId}`,
      (e) => audio.setFormat(e.payload),
    ));
    unlistenAudio = untilUnmount(listen<{ timestampMs: number; pcmB64: string }>(
      `rdp:audio:${sessionId}`,
      (e) => {
        const bin = atob(e.payload.pcmB64);
        const bytes = new Uint8Array(bin.length);
        for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
        audio.push(bytes);
      },
    ));
    unlistenAudioClose = untilUnmount(listen(`rdp:audio-close:${sessionId}`, () => audio.close()));
    // RDPDR：挂载确认（helper 宣告成功）后刷新状态。
    untilUnmount(listen(`rdp:share:${sessionId}`, () => void refreshShares()));
    void refreshShares();
    ctx = canvas?.getContext("2d") ?? null;
    observeResize();
  });
  onDestroy(() => {
    releaseFramePainter(sessionId); // 只解绑回调：会话还在，切回来还要用这条通道
    unlistenFrame?.();
    unlistenClosed?.();
    unlistenConnected?.();
    unlistenAudioFmt?.();
    unlistenAudio?.();
    unlistenAudioClose?.();
    audio.close(); // 释放 AudioContext（不关会一直占着音频设备）
    resizeObserver?.disconnect();
    if (resizeTimer !== null) clearTimeout(resizeTimer);
  });

  /* ── 动态分辨率（阶段 2）────────────────────────────────────────────────
   *
   * 容器尺寸变化 → 防抖 250ms → rdp_resize。防抖是必须的而不是优化：
   * 拖窗口一次会产生几十次 resize 事件，每次都发一条 MonitorLayout PDU
   * 会让远端疯狂重协商分辨率（画面反复黑屏重建）。
   *
   * 尺寸取**容器**而不是 canvas：canvas 的位图尺寸是远端分辨率，
   * 拿它当目标尺寸算就永远等于当前值、永远不会触发变化。
   */
  let resizeObserver: ResizeObserver | null = null;
  let resizeTimer: ReturnType<typeof setTimeout> | null = null;
  let wrapEl = $state<HTMLDivElement | undefined>();
  /** 已请求过的目标尺寸：相同尺寸不重复发（拖回原大小很常见）。 */
  let lastRequested = { w: 0, h: 0 };

  function observeResize(): void {
    if (!wrapEl || typeof ResizeObserver === "undefined") return;
    resizeObserver = new ResizeObserver((entries) => {
      const box = entries[0]?.contentRect;
      if (!box) return;
      const w = Math.round(box.width);
      const h = Math.round(box.height);
      if (w < 200 || h < 200) return; // 协议下限，后端还会再钳一次
      if (resizeTimer !== null) clearTimeout(resizeTimer);
      resizeTimer = setTimeout(() => {
        if (w === lastRequested.w && h === lastRequested.h) return;
        lastRequested = { w, h };
        void invoke("rdp_resize", { sessionId, width: w, height: h }).catch(() => {
          /* 会话已关：忽略（下一次连接是全新会话） */
        });
      }, 250);
    });
    resizeObserver.observe(wrapEl);
  }

  /* ── 断线重连（阶段 2）──────────────────────────────────────────────────
   *
   * **重建式**，不是续上原连接——后端 rdp.rs 的重连段注释写明了它与
   * RDP 自动重连 cookie 的语义差异（IronRDP 0.10 未接通 cookie 那条路）。
   * 由用户点按触发而不是自动重试：每次重连都要过一次认证，自动重试等于
   * 拿口令反复撞服务器（Windows 默认 5 次失败锁 30 分钟）。
   */
  let reconnectError = $state("");

  async function reconnect(): Promise<void> {
    if (reconnecting) return;
    reconnecting = true;
    reconnectError = "";
    // 重连是**新会话新 id**（后端重建式）：帧通道必须重建（旧通道闭包绑着本组件
    // 的 ctx），标签必须**原地转正**到新 id——此前返回值被丢弃，标签一直指着死掉的
    // 旧 id：画布冻着、关标签也关不掉新会话。占位键先顶着，拿到新 id 立刻 rebind。
    const chanKey = `pending-rdp-reconnect-${sessionId}`;
    try {
      const r = await invoke<{ ok: boolean; session_id: string; width: number; height: number; message: string }>(
        "rdp_reconnect",
        { sessionId, frameChannel: createFrameChannel(chanKey) },
      );
      if (r.ok && r.session_id) {
        rebindFrameChannel(chanKey, r.session_id);
        replaceTabId(sessionId, r.session_id);
        setTabStatus(r.session_id, "connected");
        // 本组件随旧标签一起被替换，无需再清状态
      } else {
        dropFrameChannel(chanKey);
        reconnecting = false;
        reconnectError = r.message || "重连失败";
      }
    } catch (e) {
      dropFrameChannel(chanKey);
      reconnecting = false;
      // 失败原因由后端给人话（认证/网络/证书各不相同），原样呈现
      reconnectError = String(e);
    }
  }

  /* ── 剪贴板同步（阶段 2，CLIPRDR）──────────────────────────────────────
   *
   * 触发点是**画布获得焦点**——那正是「用户开始在远端干活」的时刻，
   * 也正是 mstsc 的对齐时机。定时轮询剪贴板是另一种做法，但它要么太慢
   * （错过用户刚复制的内容）、要么太勤（每秒读一次系统剪贴板，在 X11 上
   * 会把 selection owner 抢来抢去）。
   *
   * 本机内容由**前端**读：webview 的 clipboard.readText() 需要用户手势
   * 授权，而 focus 事件正处在手势上下文里。读失败（未授权/无内容）时
   * 本方向静默跳过——反方向（远端→本机）是后端事件驱动的，不受影响。
   */
  async function syncClipboard(): Promise<void> {
    // 本机剪贴板由**后端**读（arboard），前端不碰 navigator.clipboard：
    // WebView2 对 readText() 每次都弹「此页面想读取剪贴板」的授权框——
    // 用户在远程桌面里看到浏览器权限弹窗，既出戏又挡屏幕。理由详见
    // rdp_cmd.rs 的 rdp_clipboard_sync 文档。
    await invoke("rdp_clipboard_sync", { sessionId }).catch(() => {});
    // 反方向不必在这里做：远端一复制就会通告，后端收到即取并写进系统剪贴板。
  }

  /** 绘制核心：两条投递路径（事件+base64 / channel+raw）汇到这同一个函数——
   *  拆开后任何一条路径的像素处理漂移都会立刻表现为另一条路径的回归。 */
  function paintBytes(p: { x: number; y: number; w: number; h: number; rgba: Uint8Array }): void {
    const c = ctx;
    if (!c || p.w === 0 || p.h === 0) return;
    // ImageData 要的是 ClampedArray：字节相同，只是视图语义（自动截断到 0–255）。
    // 类型注：TS 的 Uint8ClampedArray<ArrayBufferLike> 与 DOM lib 期望的
    // ImageDataArray（限 ArrayBuffer）在「buffer 到底是不是共享的」上谈不拢，
    // 我们的数据源（DataView/Channel 的 ArrayBuffer）永远是独占的——断言即可。
    const view = new Uint8ClampedArray(p.rgba.buffer, p.rgba.byteOffset, p.rgba.byteLength) as Uint8ClampedArray<ArrayBuffer>;
    const img = new ImageData(view, p.w, p.h);
    c.putImageData(img, p.x, p.y);
  }

  /** 事件路径（回落用）：base64 → 字节 → 核心。 */
  function paint(p: { x: number; y: number; w: number; h: number; rgbaB64: string }): void {
    const bin = atob(p.rgbaB64);
    const bytes = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
    paintBytes({ x: p.x, y: p.y, w: p.w, h: p.h, rgba: bytes });
  }

  // ── 鼠标 ────────────────────────────────────────────────────────────
  /** 显示坐标 → 远端位图坐标（等比缩放反算） */
  function toRemote(e: MouseEvent): { x: number; y: number } {
    const c = canvas;
    if (!c) return { x: 0, y: 0 };
    const r = c.getBoundingClientRect();
    const sx = r.width === 0 ? 1 : c.width / r.width;
    const sy = r.height === 0 ? 1 : c.height / r.height;
    return {
      x: Math.max(0, Math.min(c.width - 1, Math.round((e.clientX - r.left) * sx))),
      y: Math.max(0, Math.min(c.height - 1, Math.round((e.clientY - r.top) * sy))),
    };
  }

  function sendMouse(kind: "move" | "down" | "up", e: MouseEvent): void {
    const { x, y } = toRemote(e);
    const button =
      e.button === 2 ? "right" : e.button === 1 ? "middle" : "left";
    const payload =
      kind === "move"
        ? { kind: "mouse_move" as const, x, y }
        : {
            kind: "mouse_button" as const,
            button,
            down: kind === "down",
            x,
            y,
          };
    void invoke("rdp_input", { sessionId, event: payload }).catch(() => {
      /* 会话关闭后的尾巴事件：丢弃（下一次连接是全新会话） */
    });
  }

  function onWheel(e: WheelEvent): void {
    e.preventDefault();
    const { x, y } = toRemote(e as unknown as MouseEvent);
    const vertical = !(e.shiftKey && Math.abs(e.deltaX) > Math.abs(e.deltaY));
    const delta = vertical ? -Math.sign(e.deltaY) : Math.sign(e.deltaX);
    void invoke("rdp_input", {
      sessionId,
      event: { kind: "mouse_scroll", vertical, delta, x, y },
    }).catch(() => {});
  }

  // ── 键盘 ────────────────────────────────────────────────────────────
  /** KeyboardEvent.code（物理键位）→ PS/2 Set 1 (extended, scancode)。 */
  function scancodeOf(code: string): { scancode: number; extended: boolean } | null {
    // extended 族（0xE0 前缀）：方向键 / 右侧修饰 / 六键区 / 等
    const ext: Record<string, number> = {
      ArrowUp: 0x48, ArrowDown: 0x50, ArrowLeft: 0x4b, ArrowRight: 0x4d,
      Home: 0x47, End: 0x4f, PageUp: 0x49, PageDown: 0x51,
      Insert: 0x52, Delete: 0x53,
      ControlRight: 0x1d, AltRight: 0x38, NumpadEnter: 0x1c,
      // NumpadDivide 是扩展键（0xE0 35，与主区 Slash 的裸 0x35 区分）；
      // **NumpadMultiply(0x37) 与 NumpadSubtract(0x4A) 不是**——它们是裸码。
      // 曾经把这两个放在本表里，发出去成了 E0 37（= PrintScreen）与
      // E0 4A（无定义），于是小键盘的 * 变截屏、- 完全不工作。
      // 对表：上游 web-client 的 scancodes.ts（0x0037/0x004A 在基本表，
      // 0xE035/0xE037 在扩展表）。
      NumpadDivide: 0x35,
      // Win 键**必须带扩展位**（0xE0 5B / 0xE0 5C）：不带的话远端收到的是
      // 一个无定义的裸 0x5B，开始菜单不会弹——这是 Win 键「按了没反应」的成因。
      MetaLeft: 0x5b, MetaRight: 0x5c,
      // PrintScreen 是 0xE0 37（裸 0x37 是小键盘 *，正是上面那处的错源）。
      PrintScreen: 0x37,
      ContextMenu: 0x5d, WakeUp: 0xe3,
    };
    if (code in ext) return { scancode: ext[code], extended: true };
    // 主区：1..9/a../z 与常见控制键。数字排与字母排的 code 名即物理名
    const base: Record<string, number> = {
      Backquote: 0x29, Digit1: 0x02, Digit2: 0x03, Digit3: 0x04, Digit4: 0x05,
      Digit5: 0x06, Digit6: 0x07, Digit7: 0x08, Digit8: 0x09, Digit9: 0x0a,
      Digit0: 0x0b, Minus: 0x0c, Equal: 0x0d, Backspace: 0x0e, Tab: 0x0f,
      KeyQ: 0x10, KeyW: 0x11, KeyE: 0x12, KeyR: 0x13, KeyT: 0x14, KeyY: 0x15,
      KeyU: 0x16, KeyI: 0x17, KeyO: 0x18, KeyP: 0x19, BracketLeft: 0x1a,
      BracketRight: 0x1b, Backslash: 0x2b, CapsLock: 0x3a,
      KeyA: 0x1e, KeyS: 0x1f, KeyD: 0x20, KeyF: 0x21, KeyG: 0x22, KeyH: 0x23,
      KeyJ: 0x24, KeyK: 0x25, KeyL: 0x26, Semicolon: 0x27, Quote: 0x28,
      Enter: 0x1c, ShiftLeft: 0x2a, IntlBackslash: 0x56,
      KeyZ: 0x2c, KeyX: 0x2d, KeyC: 0x2e, KeyV: 0x2f, KeyB: 0x30, KeyN: 0x31,
      KeyM: 0x32, Comma: 0x33, Period: 0x34, Slash: 0x35, ShiftRight: 0x36,
      ControlLeft: 0x1d, AltLeft: 0x38, Space: 0x39,
      Escape: 0x01, F1: 0x3b, F2: 0x3c, F3: 0x3d, F4: 0x3e, F5: 0x3f,
      F6: 0x40, F7: 0x41, F8: 0x42, F9: 0x43, F10: 0x44, F11: 0x57, F12: 0x58,
      ScrollLock: 0x46,
      // NumLock 传裸 0x45。上游 web-client 把 0xE045 标为 NumLock、0x0045 标为
      // Pause，与这里相反——**刻意不跟**：Windows 侧的历史约定是 NumLock 发
      // 0x45（E1 1D 45 才是 Pause 的三字节序列，不是 E0 45）。这一条置信度
      // 低于其余四处，真机核验前不动（见 V15 手测清单）。
      NumLock: 0x45,
      // 小键盘的 * - 是裸码（不带 0xE0）：见上面 ext 表的记述。
      NumpadMultiply: 0x37, NumpadSubtract: 0x4a,
      Numpad0: 0x52, Numpad1: 0x4f, Numpad2: 0x50, Numpad3: 0x51,
      Numpad4: 0x4b, Numpad5: 0x4c, Numpad6: 0x4d, Numpad7: 0x47,
      Numpad8: 0x48, Numpad9: 0x49, NumpadAdd: 0x4e, NumpadDecimal: 0x53,
    };
    if (code in base) {
      // 右 Alt/右 Ctrl 在主区表里但属 extended——单列在 ext 表优先命中，
      // 这里只剩真正的左/主区键。右 Alt 走 ext 已命中，到不了这里。
      return { scancode: base[code], extended: false };
    }
    return null;
  }

  /**
   * 失焦/移出画布：让远端把所有按下态放掉。
   *
   * 按下与松开是两条独立事件，而**失焦之后松开事件不会再来**：用户按住
   * Alt 摁 Tab 切走，远端就一直认为 Alt 按着——切回来每个字母都成了
   * Alt+ 组合键；拖拽中切走则是左键永远按着。mstsc 在这里的行为一致。
   *
   * 同时挂 focusout 与 mouseleave：前者管键盘（切窗口/切标签），
   * 后者管鼠标（拖出画布再松手，那次 mouseup 落在画布外，我们收不到）。
   */
  function releaseAllKeys(): void {
    void invoke("rdp_input", {
      sessionId,
      event: { kind: "release_all" },
    }).catch(() => {});
  }

  function onKey(e: KeyboardEvent, down: boolean): void {
    // 本地共享按钮和文件选择器的输入不能发送给远程桌面。
    if (e.target !== wrapEl && e.target !== canvas) return;
    if (e.ctrlKey && e.altKey && e.key === "Home") {
      e.preventDefault(); e.stopPropagation();
      if (down) { releaseAllKeys(); wrapEl?.querySelector<HTMLButtonElement>(".shares button")?.focus(); }
      return;
    }
    // 浏览器快捷键（Ctrl+W 关标签等）留给本地，不透传——透传了会在远端
    // 触发同名动作，本地却又拦不住，行为不可预期。
    if (e.ctrlKey && ["w", "t", "n"].includes(e.key.toLowerCase())) return;
    if (e.metaKey) return;
    const sc = scancodeOf(e.code);
    if (!sc) return;
    e.preventDefault();
    void invoke("rdp_input", {
      sessionId,
      event: {
        kind: "key",
        scancode: sc.scancode,
        extended: sc.extended,
        down,
      },
    }).catch(() => {});
  }
</script>

<!-- a11y：role="application" 的 div 承载键鼠是 RDP 画布的本意（屏幕内容
     无障碍树里没有意义，键盘事件必须整体捕获）。svelte 的两条告警按全局
     处理惯例用 role+tabindex 表达意图，svelte-check 只看 ERROR 门禁。 -->
<!-- 右键菜单拦在容器而不是画布上：画布与窗口宽高比不一致时（动态分辨率
     生效前、或远端拒绝该尺寸时），画布周围有一圈黑边——右键黑边曾弹出
     浏览器的右键菜单，用户在「远程桌面」里看到的是 WebView 的菜单而不是
     远端的，既出戏也丢事件。挂在容器上，黑边与断线覆盖层一并覆盖。 -->
<div
  class="rdp-wrap"
  tabindex="0"
  role="application"
  aria-label="远程桌面（Ctrl+Alt+Home 返回本地共享控件）"
  data-testid="rdp-wrap"
  bind:this={wrapEl}
  onfocusin={(e) => { if (e.target === wrapEl || e.target === canvas) void syncClipboard(); }}
  onkeydown={(e) => onKey(e, true)}
  onkeyup={(e) => onKey(e, false)}
  onfocusout={releaseAllKeys}
  onmouseleave={releaseAllKeys}
  oncontextmenu={(e) => e.preventDefault()}
>
  <canvas
    bind:this={canvas}
    width={bmW}
    height={bmH}
    data-testid="rdp-canvas"
    onmousemove={(e) => sendMouse("move", e)}
    onmousedown={(e) => {
      if (e.button === 0 || e.button === 1 || e.button === 2) sendMouse("down", e);
    }}
    onmouseup={(e) => {
      if (e.button === 0 || e.button === 1 || e.button === 2) sendMouse("up", e);
    }}
    onwheel={onWheel}
  ></canvas>

  <!-- 共享目录条（RDPDR）：浮动右上角。已挂的盘逐个列出（点 × 卸载），
       「共享目录…」开选择器。共享面是 RDP 的真安全面，入口必须常驻可见——
       藏进菜单的共享等于用户忘了自己共享过什么。 -->
  <div class="shares" data-testid="rdp-shares" title="在远程桌面中按 Ctrl+Alt+Home 可回到本地共享控件">
    {#each shares as sh (sh.device)}
      <span class="chip" data-testid="rdp-share-chip-{sh.device}">
        盘{sh.device} {sh.readonly ? "（只读）" : "（可写）"}
        <button
          title="停止共享"
          aria-label="停止共享盘{sh.device}"
          data-testid="rdp-share-unmount-{sh.device}"
          onclick={() => void doUnmount(sh.device)}
        >×</button>
      </span>
    {/each}
    {#if shares.length < 4}
      <label class="ro" title="勾上后远端只能读取，不能修改">
        <input type="checkbox" bind:checked={wantReadonly} data-testid="rdp-share-readonly" /> 只读
      </label>
      <button data-testid="rdp-share-add" onclick={() => (sharePickerOpen = true)}>共享目录…</button>
    {/if}
  </div>

  {#if sharePickerOpen}
    <FilePickerDialog
      open={sharePickerOpen}
      title={wantReadonly ? "选择要共享的目录（只读）" : "选择要共享的目录（远端可写）"}
      mode="directory"
      confirmLabel="共享这个目录"
      onConfirm={(paths) => {
        sharePickerOpen = false;
        if (paths[0] !== undefined) void doMount(paths[0], wantReadonly);
      }}
      onCancel={() => (sharePickerOpen = false)}
    />
  {/if}

  <!-- 意外断线覆盖层：盖在画面上而不是替换它——最后一帧留着，
       用户能看见断线前的状态（对着一个纯黑框，「刚才干到哪了」全靠记）。 -->
  {#if closedUnexpectedly}
    <div class="disconnected" data-testid="rdp-disconnected">
      <p class="msg">连接已断开</p>
      {#if reconnectError}
        <p class="err" data-testid="rdp-reconnect-error">{reconnectError}</p>
      {/if}
      <button
        data-testid="rdp-reconnect"
        disabled={reconnecting}
        onclick={() => void reconnect()}
      >
        {reconnecting ? "重新连接中…" : "重新连接"}
      </button>
      <p class="hint">
        重连会重新登录一次。桌面内容能否恢复取决于服务器的会话保留设置。
      </p>
    </div>
  {/if}
</div>

<style>
  .shares {
    position: absolute; top: 6px; right: 6px; z-index: 5;
    display: flex; align-items: center; gap: 6px;
    padding: 3px 6px; border-radius: var(--fs-radius);
    background: var(--fs-bg-elevated); border: 1px solid var(--fs-border);
    font-size: 11.5px; color: var(--fs-fg-primary);
  }
  .shares .chip {
    display: inline-flex; align-items: center; gap: 4px;
    padding: 1px 4px 1px 8px; border-radius: var(--fs-radius);
    background: var(--fs-bg-panel); border: 1px solid var(--fs-border);
  }
  .shares .chip button {
    background: none; border: 0; color: var(--fs-fg-secondary);
    cursor: pointer; font-size: 12px; line-height: 1;
  }
  .shares .chip button:hover { color: var(--fs-danger); }
  .shares .ro { display: inline-flex; align-items: center; gap: 3px; color: var(--fs-fg-secondary); }
  .shares button[data-testid="rdp-share-add"] {
    background: var(--fs-bg-panel); color: var(--fs-fg-primary);
    border: 1px solid var(--fs-border); border-radius: var(--fs-radius);
    padding: 2px 8px; cursor: pointer; font-size: 11.5px;
  }
  .rdp-wrap {
    position: relative; /* 断线覆盖层的定位基准 */
    width: 100%;
    height: 100%;
    display: grid;
    place-items: center;
    background: #000;
    outline: none;
    overflow: hidden;
  }
  .disconnected {
    position: absolute;
    inset: 0;
    display: grid;
    place-content: center;
    justify-items: center;
    gap: 8px;
    background: rgba(0, 0, 0, 0.72);
    color: var(--fs-fg-primary, #eee);
    padding: 24px;
    text-align: center;
  }
  .disconnected .msg { margin: 0; font-size: 15px; }
  .disconnected .err { margin: 0; font-size: 12px; color: var(--fs-danger, #e05252); max-width: 40ch; }
  .disconnected .hint { margin: 0; font-size: 12px; color: var(--fs-fg-secondary, #aaa); max-width: 40ch; }
  .disconnected button {
    padding: 6px 18px;
    border: 1px solid var(--fs-border, #555);
    background: var(--fs-accent, #6ea8fe);
    color: #fff;
    border-radius: 4px;
    cursor: pointer;
  }
  .disconnected button:disabled { opacity: .6; cursor: default; }

  canvas {
    max-width: 100%;
    max-height: 100%;
    /* 等比缩放：位图保持远端分辨率，显示尺寸跟随容器 */
    object-fit: contain;
  }
</style>
