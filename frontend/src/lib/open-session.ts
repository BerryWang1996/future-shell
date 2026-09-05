import { get } from "svelte/store";
import { invoke, settingGet } from "./ipc";
import { createFrameChannel, dropFrameChannel, rebindFrameChannel } from "./rdp-frames"; // 4d：RDP 帧走 raw channel
import { addTab, replaceTabId, setTabError, setTabFailure, setTabStatus, tabs } from "./tabs";
import { toast } from "./toast";
import type { ConnectFailure, Profile } from "./types";

/**
 * 占位标签 id 前缀。占位标签是**只存在于前端**的标签：它已经出现在标签栏/侧栏/状态栏上，
 * 但后端还没有（或永远不会有）与之对应的会话，所以它的 id 必须能被一眼认出来——
 * 装配层据此决定不给它挂真终端（`term_input`/`term_resize`/`term_ack` 对未知会话一律
 * `Err("no session")`，挂上去就是每建一个占位标签甩一串未处理的 Promise 拒绝），
 * `closeSession` 据此跳过 `session_close` IPC。
 */
const PENDING_PREFIX = "pending:";

/** 进程内自增即可：占位 id 只需在「同时存在的占位标签」之间唯一，且转正后立即消失。
 *  不用 crypto.randomUUID()——jsdom 下它并不保证存在，会把本模块变成不可测。 */
let pendingSeq = 0;

/** 该 id 是占位标签（前端有、后端无）而非真实会话。 */
export function isPendingSessionId(id: string): boolean {
  return id.startsWith(PENDING_PREFIX);
}

/** 仅供测试：重置占位 id 计数器，使断言可以钉具体的 id。 */
export function resetPendingSeq(): void {
  pendingSeq = 0;
}

/**
 * 打开会话（UI 规格 §5 连接态三处一致 / phase1-acceptance.md「双击连接 → 标签蓝环 +
 * 侧栏蓝闪 + 状态栏『连接中…』同现；成功 → 三处转绿」）。
 *
 * 缺陷本体是两条，都在「先后顺序」上：
 *
 * ① 原实现 `await invoke("session_open")` **之后**才 `addTab`。而 `session_open`
 *    (session_cmd.rs:46) 要走完 TCP、banner、主机密钥校验（含弹框等用户点确认）、认证、
 *    子系统装配、journal 落库才 resolve。也就是说从双击到连上的整个窗口——不可达主机
 *    上是一整个 TCP 超时——标签栏、侧栏状态灯、状态栏三处**全是空的**，用户唯一的反馈是
 *    连接期打出来的几条 toast。规格把「消除连接期静默」写进了 `addTab` 的注释（tabs.ts:57），
 *    转正原语 `replaceTabId`（含 S264 临时配色随迁）连同它的单测一起写好了，接线从没发生：
 *    `replaceTabId` 在整个生产代码里零调用。这是本仓第四次同款——原语齐备、散文写着接线、
 *    接线不存在，而门禁只测到原语本身。
 *
 * ② 连上之后没有任何人把标签从 connecting 翻成 connected。`setTabStatus(…, "connected")`
 *    唯一的生产调用点在 `session:status` 事件桥里，而后端这条事件在**首次连接**期间全部
 *    发生在标签存在之前（见 session-status.ts），标签建起来之后就只剩重连成功那一条。
 *    于是一个连得好好的会话，标签蓝环一直转、状态栏一直写「连接中…」、侧栏灯一直蓝闪，
 *    除非它断线并自动重连成功过一次。
 *
 * 现在的次序：占位标签 → 拨号 → 转正 + 置 connected。失败则把占位标签就地标成 error 并
 * 写错误文案（标签红✕ + 状态栏 errorText 段，§5 四态里 error 这一态此前同样不可达），
 * 不撤标签：撤掉就等于让「哪一个连接失败了」只剩一条 8 秒后消失的 toast。
 */
export async function openSession(profile: Profile): Promise<string | null> {
  const pendingId = `${PENDING_PREFIX}${++pendingSeq}`;
  // 串口没有 user@host：标签与状态栏的「主机」段显示端口名 + 波特率
  //（`COM3 · 115200`）。硬套 `@` 的写法会渲染成「@」开头的空用户名，看着像坏了。
  const serial = profile.protocol === "serial";
  addTab(
    pendingId,
    serial ? (profile.serial?.port || "串口") : `${profile.username}@${profile.host}`,
    profile.id,
    "connecting",
    serial ? `${profile.serial?.port ?? ""} · ${profile.serial?.baud ?? 115200}` : `${profile.host}:${profile.port}`,
    profile.protocol === "rdp" ? "rdp" : serial ? "serial" : "ssh",
  );
  try {
    // RDP 走独立命令（rdp_connect）：连接产物（桌面尺寸/证书裁决/帧流）与
    // SSH 会话完全不同构，不共用 session_open。口令由后端从 Vault 现取
    // （前端不传也不该知道——与 SSH 同一条纪律）。
    if (profile.protocol === "rdp") {
      // 4d raw 帧通道：必须在 connect **之前**创建（它是命令参数）。键先用占位 id，
      // 拿到真 session_id 后立刻重挂——连接期间不会有帧（helper 到 Connected 才发帧），
      // 但 channel 对象是同一个，重挂只是改表键。
      // 4d 出口判据要求「同一画面下 raw 与事件两条路径可对比」（framestats）。
      // 设置键 rdp.frameTransport（"raw" 默认 | "event"）：event 时不建通道，
      // 后端自然回落 rdp:frame 事件——两条路径在同一构建里，真机 A/B 不用换版本。
      const transport = await settingGet<string>("rdp.frameTransport", "raw");
      const frameChannel = transport === "event" ? null : createFrameChannel(pendingId);
      let r: { ok: boolean; session_id: string; width: number; height: number; message: string };
      try {
        r = await invoke<{ ok: boolean; session_id: string; width: number; height: number; message: string }>(
          "rdp_connect",
          frameChannel ? { profileId: profile.id, frameChannel } : { profileId: profile.id },
        );
      } catch (e) {
        if (frameChannel) dropFrameChannel(pendingId); // 通道随失败连接一起作废：留着会挡住下一次的表键
        throw e;
      }
      if (r.ok && r.session_id && frameChannel) rebindFrameChannel(pendingId, r.session_id);
      else if (frameChannel) dropFrameChannel(pendingId);
      if (!r.ok) {
        throw new Error(r.message || "RDP 连接失败");
      }
      if (!get(tabs).some((t) => t.id === pendingId)) {
        void invoke("rdp_close", { sessionId: r.session_id }).catch(() => {});
        dropFrameChannel(r.session_id);
        return null;
      }
      replaceTabId(pendingId, r.session_id);
      setTabStatus(r.session_id, "connected");
      return r.session_id;
    }

    // 串口走自己的打开命令（参数、失败模式、重连语义都与 SSH 不同），但**打开之后**
    // 一切共用：term_input/term_ack/term_resize/session_close 在后端回落到串口注册表。
    const sessionId = serial
      ? await invoke<string>("serial_open", { profileId: profile.id, rows: 24, cols: 80 })
      : await invoke<string>("session_open", { profileId: profile.id });
    // 连接期用户是可以把占位标签关掉的（× / 中键 / Ctrl+W），而后端没有「取消拨号」的入口——
    // 拨号仍会成功。不补这一刀，注册表里就留着一个界面上再无任何入口的活会话：连着远端、
    // 占着 SFTP/传输子系统、拿着凭据，直到进程退出。这是占位标签带进来的新窗口，必须自己收。
    if (!get(tabs).some((t) => t.id === pendingId)) {
      void invoke("session_close", { sessionId }).catch(() => {});
      return null;
    }
    replaceTabId(pendingId, sessionId);
    setTabStatus(sessionId, "connected");
    return sessionId;
  } catch (e) {
    // 2026-09-01：session_open 的 Err 现在是结构化的 ConnectFailure（见 types.ts）。
    // 旧式字符串 rejection 仍然可能来（MCP 侧、装配阶段的 String、测试桩），两种都接：
    // 结构化的连 failure 一起落标签，面板据此挂按钮；字符串的只落 errorText，
    // 面板退化成「文案 + 重试」。errorText 两条路都是人能读的原文（不是 [object Object]）。
    const failure = asConnectFailure(e);
    const text = failure ? failure.summary : String(e);
    console.error("打开会话失败:", text);
    if (failure) setTabFailure(pendingId, failure);
    else setTabError(pendingId, text);
    toast.error(`打开会话失败：${text}`);
    return null;
  }
}

/** rejection 是不是结构化失败：判据只有一条——有 string 类型的 summary。导出供测试。 */
export function asConnectFailure(e: unknown): ConnectFailure | null {
  if (typeof e !== "object" || e === null) return null;
  const r = e as Record<string, unknown>;
  return typeof r.summary === "string" && typeof r.category === "string" ? (e as ConnectFailure) : null;
}
