/**
 * 外部编辑器关联的前端侧（M4a）。
 *
 * 判定在后端（fs_sshengine::editsync）——这里只有「怎么呈现结论」和「隔多久问一次」，
 * 刻意不复制任何判断逻辑：判定决定要不要覆盖远端文件，两份实现迟早分叉，而分叉的
 * 那一侧会静默抹掉别人的修改。
 */

/** 后端 editor_check 的结论（serde tag = "kind"，snake_case）。 */
export type EditDecision =
  | { kind: "no_local_change" }
  | { kind: "upload" }
  | { kind: "remote_gone" }
  | {
      kind: "conflict";
      remote_at_open: { size: number; mtime: number };
      remote_now: { size: number; mtime: number };
    };

export interface EditItem {
  session_id: string;
  remote: string;
  local: string;
}

/**
 * 轮询间隔。2 秒是「存盘后很快就回传」与「不要每秒往远端发一次 lstat」之间的折中：
 * 编辑一个配置文件的会话通常同时开着 1–3 个文件，2 秒即每文件每分钟 30 次 lstat，
 * 对服务端可以忽略，对用户也感觉不到延迟。
 *
 * 不做「立刻检查」的按钮以外的加速：更快的轮询会在编辑器「先截断再写」的那一瞬间
 * 抓到一个空文件（size=0、mtime 新），从而把一次正常保存判成内容清空。
 */
export const POLL_MS = 2000;

/** 结论 → 是否需要弹确认。只有冲突需要人来定。 */
export function needsConfirm(d: EditDecision): boolean {
  return d.kind === "conflict";
}

/** 结论 → 是否应当自动回传（无冲突的保存）。 */
export function shouldAutoUpload(d: EditDecision): boolean {
  return d.kind === "upload";
}

/**
 * 冲突文案。必须把**两个时刻**都说出来：用户要判断「远端那份是谁改的、是不是我自己
 * 刚才用别的方式传上去的」，只说「文件已变更」他没法决定。
 */
export function conflictMessage(remote: string, d: EditDecision): string {
  if (d.kind !== "conflict") return "";
  const at = (s: number) => (s > 0 ? new Date(s * 1000).toLocaleString() : "未知时间");
  return (
    `远端文件「${remote}」在你编辑期间被改动过。\n\n` +
    `打开时：${d.remote_at_open.size} 字节，${at(d.remote_at_open.mtime)}\n` +
    `现在：${d.remote_now.size} 字节，${at(d.remote_now.mtime)}\n\n` +
    `继续回传会用你的本地版本**覆盖**远端的改动，且不可撤销。`
  );
}

/** 远端文件消失的文案（同样不代替用户决定：可能正是他自己删的）。 */
export function goneMessage(remote: string): string {
  return (
    `远端文件「${remote}」已不存在（被删除或移走）。\n\n` +
    `继续回传会**重新创建**它。若这个删除是有意的，请改为「停止编辑」。`
  );
}
