/**
 * 多会话广播（M4a：广播发送双通道，路线图 §M4 / UI 规格 §2.6）。
 *
 * 两个通道共享同一套「目标模型」：
 * - ① 组合命令栏：一次性把整条命令发给目标集合（ComposeBar 的目标选择器实装）；
 * - ② 实时键入广播：开关开启期间，活动终端的**每一次键入**（含交互式命令的
 *   中途输入）同步到目标集合（ToolBar 📢 开关，Xshell「Send Input to Multiple
 *   Sessions」对标）。
 *
 * 本模块只放**纯逻辑与共享状态**，不放 UI。App.svelte 负责把 tabs/profiles
 * 喂进 resolveTargetSessions、把结果送进 sendToSessions——组件装配层保持薄，
 * 判据全部落在可单测的纯函数上（App 没有组件测试，见 window-close.ts 的先例注）。
 *
 * 字节级一致是硬契约（路线图 M4a 出口标准「各会话收到字节级一致」）：实时广播
 * 转发的是**已编码的 b64 原文**，绝不解码重编——中途任何转码都是不一致的来源。
 */
import { writable } from "svelte/store";
import type { ComposeTarget } from "./compose-history";

/** resolveTargetSessions 需要的标签最小形状（解耦 Tab 全量类型，测试可裸造）。 */
export interface BroadcastTabInfo {
  /** 会话 id（= Tab.id = term_input 的 sessionId） */
  id: string;
  /** 所属 Profile id（分组解析用） */
  profileId: string;
  /** TabStatus："connecting" | "connected" | "disconnected" | "error" */
  status: string;
}

/** 分组解析需要的 Profile 最小形状。 */
export interface BroadcastProfileInfo {
  id: string;
  group_path: string | null;
}

/** 共享目标选择：组合栏与实时广播同源（S306）。 */
export const composeTarget = writable<ComposeTarget>("current");
/** 手选目标集合（session id）。仅 target === "pick" 时生效；其余模式忽略。 */
export const pickedSessions = writable<string[]>([]);
/** 实时键入广播开关（②通道的「独立开关」）。 */
export const liveBroadcast = writable(false);

/** 测试隔离：组件测试间复位三个 store（生产代码不得调用）。 */
export function resetBroadcastStoresForTest(): void {
  composeTarget.set("current");
  pickedSessions.set([]);
  liveBroadcast.set(false);
}

/**
 * 解析目标 → 会话 id 列表（纯函数，S305 判据载体）。
 *
 * 语义（逐条对齐 UI 文案，改一处必须同步另一处）：
 * - **只发给已连接会话**（status === "connected"）：connecting 还没有 PTY 可写，
 *   disconnected/error 的管道已拆（term_input 会 Err("no session")），发给它们
 *   只会制造失败噪声。
 * - current：[活动会话]（活动会话未连接时为空——调用方提示）。
 * - all：全部已连接会话，按标签栏顺序。
 * - group：活动会话所属 Profile 的 group_path **非空**时，取同组的全部已连接
 *   会话（含自身）；group_path 为空 = 无分组即无同伴，退化为 [活动会话]。
 *   「退化」而非「空集」：用户在无分组下选「当前分组」的意图最可能是「就发这
 *   一个」，弹空集警告反而是打断。
 * - pick：手选集合 ∩ 当前已连接会话（按手选顺序、去重）——已断线的选中项静默
 *   剔除，恢复连接后自然回来（集合本身不动）。
 */
export function resolveTargetSessions(
  target: ComposeTarget,
  tabs: BroadcastTabInfo[],
  profiles: BroadcastProfileInfo[],
  activeId: string | null,
  picked: string[],
): string[] {
  const connected = tabs.filter((t) => t.status === "connected");
  switch (target) {
    case "current":
      return activeId && connected.some((t) => t.id === activeId) ? [activeId] : [];
    case "all":
      return connected.map((t) => t.id);
    case "group": {
      const active = tabs.find((t) => t.id === activeId);
      if (!active) return [];
      const group = profiles.find((p) => p.id === active.profileId)?.group_path?.trim() ?? "";
      if (!group) {
        return connected.some((t) => t.id === active.id) ? [active.id] : [];
      }
      const sameGroup = connected.filter((t) =>
        profiles.find((p) => p.id === t.profileId)?.group_path?.trim() === group,
      );
      return sameGroup.map((t) => t.id);
    }
    case "pick": {
      const alive = new Set(connected.map((t) => t.id));
      const seen = new Set<string>();
      const out: string[] = [];
      for (const id of picked) {
        if (alive.has(id) && !seen.has(id)) {
          seen.add(id);
          out.push(id);
        }
      }
      return out;
    }
  }
}

/** invoke 的最小形状（依赖注入，测试用 mock 断言字节级一致）。 */
export type TermInputFn = (
  cmd: "term_input",
  args: { sessionId: string; dataB64: string },
) => Promise<unknown>;

export interface BroadcastSendResult {
  /** 成功送达的会话 id（保序）。 */
  sent: string[];
  /** 失败的会话与错误（不回显载荷内容——组合栏的典型载荷是含口令的命令，P2-20 口径）。 */
  failed: { sessionId: string; error: string }[];
}

/**
 * 把**已编码**输入发给多个会话（S305：字节级一致的载体——逐会话独立 invoke，
 * 载荷原样透传；单会话失败不阻断其余（广播的要点恰是「一个目标死了其余照发」）。
 */
export async function sendToSessions(
  sessionIds: string[],
  dataB64: string,
  invokeFn: TermInputFn,
): Promise<BroadcastSendResult> {
  const sent: string[] = [];
  const failed: { sessionId: string; error: string }[] = [];
  // 顺序发而非 Promise.all：会话间无依赖，但顺序发让「发到第几个断了」在日志里
  // 有确定形状；N 很小（打开的标签数），并发收益可忽略。
  for (const sessionId of sessionIds) {
    try {
      await invokeFn("term_input", { sessionId, dataB64 });
      sent.push(sessionId);
    } catch (e) {
      failed.push({ sessionId, error: String(e) });
    }
  }
  return { sent, failed };
}
