/**
 * file-clipboard.ts — 远端文件的剪切 / 复制剪贴板（M7.3）。
 *
 * # 为什么剪贴板要记住 sessionId
 *
 * 范围裁定是「**先只做同主机远端↔远端**」。在 A 机剪切、切到 B 机粘贴，用户预期的是跨机搬运，
 * 而我们此刻做不到——那要走「下载到本机再上传」，是一次可能持续几分钟的传输，语义、进度与
 * 失败恢复都和「粘贴」完全不同。所以粘贴按钮在**别的会话上必须是不可用的，并说清为什么**；
 * 让它可点然后报一句「失败」，用户会以为程序坏了。
 *
 * # 为什么剪切不立刻移动
 *
 * 桌面文件管理器的「剪切」只是标记，真正的移动发生在粘贴那一刻。立刻移动的话，用户剪切之后
 * 改主意不粘贴，文件已经不在原处了——而他没有做过任何「移动」的动作。
 */
import { writable } from "svelte/store";

export interface FileClip {
  /** 源所在的会话。跨会话粘贴不做（见模块头注）。 */
  sessionId: string;
  /** 源的完整远端路径。 */
  paths: string[];
  /** true = 剪切（粘贴时移动），false = 复制。 */
  cut: boolean;
}

export const fileClip = writable<FileClip | null>(null);

/** 记一次剪切/复制。空选择不写剪贴板——那会把上一次的内容悄悄清掉。 */
export function setClip(sessionId: string, paths: string[], cut: boolean): void {
  if (paths.length === 0) return;
  fileClip.set({ sessionId, paths: [...paths], cut });
}

export function clearClip(): void {
  fileClip.set(null);
}

/** 粘贴按钮为什么不可用；返回 null = 可用。 */
export function pasteBlockedReason(clip: FileClip | null, sessionId: string | null): string | null {
  if (!clip || clip.paths.length === 0) return "剪贴板是空的";
  if (!sessionId) return "没有活动会话";
  if (clip.sessionId !== sessionId) {
    return "剪贴板里的文件属于另一个会话——跨主机的剪切/复制还没做（那是一次真实传输，不是粘贴）";
  }
  return null;
}

/** 剪贴板摘要（按钮 title 用）。 */
export function clipSummary(clip: FileClip | null): string {
  if (!clip || clip.paths.length === 0) return "剪贴板是空的";
  const verb = clip.cut ? "剪切" : "复制";
  const first = clip.paths[0].split("/").pop() ?? clip.paths[0];
  return clip.paths.length === 1
    ? `已${verb}：${first}`
    : `已${verb} ${clip.paths.length} 项，第一项：${first}`;
}

/** 与 Rust `ConflictPolicy` 对齐。 */
export type ConflictPolicy = "overwrite" | "keep_both" | "skip";

export interface PasteOp {
  src: string;
  dst: string;
  dst_name: string;
  renamed: boolean;
  cut: boolean;
  /** 这一条会毁掉一个已存在的目标（只有用户选了「覆盖」才为真）。 */
  overwrite: boolean;
}
export interface PastePlan {
  ops: PasteOp[];
  skipped: string[];
  same_path: string[];
}
export interface PasteOutcome {
  name: string;
  ok: boolean;
  error: string;
  renamed: boolean;
}
export interface PasteResult {
  done: PasteOutcome[];
  skipped: string[];
  same_path: string[];
}

/**
 * 一次粘贴之后该对用户说什么。
 *
 * 三类信息各自成句，**绝不合并**：成功几条、失败哪几条为什么、跳过/改名/同路径各是哪些。
 * 只报一句「粘贴完成」会让部分失败被静默吞掉，而文件操作不可撤销——用户必须知道现在是什么状态。
 * 返回 `{ level, lines }`，`level` 决定 toast 的档位。
 */
export function summarizePaste(r: PasteResult): { level: "info" | "warn" | "error"; lines: string[] } {
  const okCount = r.done.filter((d) => d.ok).length;
  const failed = r.done.filter((d) => !d.ok);
  const renamed = r.done.filter((d) => d.ok && d.renamed);
  const lines: string[] = [];
  if (okCount > 0) lines.push(`成功 ${okCount} 项`);
  for (const f of failed) lines.push(`失败：${f.name}——${f.error || "远端没有给出原因"}`);
  if (renamed.length > 0) {
    lines.push(`因重名改存为：${renamed.map((d) => d.name).join("、")}`);
  }
  if (r.skipped.length > 0) lines.push(`已跳过（目标已存在）：${r.skipped.join("、")}`);
  if (r.same_path.length > 0) {
    lines.push(`源与目标是同一个位置，未处理：${r.same_path.join("、")}`);
  }
  if (lines.length === 0) lines.push("没有需要处理的条目");
  const level = failed.length > 0 ? "error" : r.skipped.length + r.same_path.length > 0 ? "warn" : "info";
  return { level, lines };
}

/** 这个名字看起来像不像 shell 脚本（双击要走「先看原文」那条路）。 */
export function looksLikeScript(name: string): boolean {
  return /\.(sh|bash|zsh|ksh)$/i.test(name);
}
