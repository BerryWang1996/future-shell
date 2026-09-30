/**
 * action-confirm.ts — 危险动作确认的**纯逻辑层**（路线图 M7.2「统一确认口径」，2026-09-03）。
 *
 * # 为什么是「按动作类别」而不是一个全局开关
 *
 * 出口原文：「一次勾选放行全部动作等于把闸拆了」。用户在「重启一个测试服务」上勾了「以后不再
 * 显示」，绝不意味着他愿意让「双击运行远端脚本」也一路放行——那是两件风险完全不同的事。
 * 所以记忆的粒度是 `DangerousAction`，且设置页要能逐条撤销（勾了之后必须能反悔）。
 *
 * # 为什么不复用 `fs_policy::gate::decide`
 *
 * 那条闸的入参是 `AiMode`（AI 执行档位）。套到用户自己点的按钮上会得到「用户把 AI 关掉 →
 * 自己的模板也跑不了」——这是路线图 M7.2 现状段里写明的、不能直接复用的理由。
 * 用户主动发起的动作与 AI 代为发起的动作是两条独立的闸。
 */

/**
 * 会走确认的动作类别。**新增一档就要同时补 `ACTION_LABEL`**（下方有守卫钉着）——
 * 少一条 label，设置页里那条豁免就会显示成一个用户看不懂的内部串，撤销时不知道自己在撤什么。
 */
export type DangerousAction =
  | "process.kill"
  | "service.stop"
  | "service.restart"
  | "script.run"
  | "fs.delete"
  /** RDPDR：把本机目录共享给远端桌面（RDPDR 批）。 */
  | "fs.share";

/** 类别 → 用户看得懂的名字（设置页的豁免列表、确认框的勾选项都用它）。 */
export const ACTION_LABEL: Record<DangerousAction, string> = {
  "process.kill": "终止远端进程",
  "service.stop": "停止服务",
  "service.restart": "重启服务",
  "script.run": "运行远端脚本",
  "fs.delete": "删除远端文件",
  "fs.share": "共享本机目录给远程桌面",
};

export const ALL_ACTIONS = Object.keys(ACTION_LABEL) as DangerousAction[];

/** 设置键：已勾选「以后不再显示」的类别。 */
export const SUPPRESS_KEY = "confirm.suppressed";

/** 类别名（未知串原样回显，不冒充成别的动作——猜错了用户撤销的就是另一件事）。 */
export function actionLabel(kind: string): string {
  return Object.prototype.hasOwnProperty.call(ACTION_LABEL, kind)
    ? ACTION_LABEL[kind as DangerousAction]
    : kind;
}

/**
 * 解析持久值。**任何脏值一律回落空集**（= 全部都要确认）。
 *
 * 失败方向是刻意的：这一栏一旦读错方向，用户会在**没有确认框**的情况下重启生产服务。
 * 读不懂就当没勾过——最坏后果是多按一次确认，而反过来的最坏后果没有上限。
 */
export function parseSuppressed(raw: unknown): DangerousAction[] {
  if (!Array.isArray(raw)) return [];
  const out: DangerousAction[] = [];
  for (const v of raw) {
    if (typeof v === "string" && Object.prototype.hasOwnProperty.call(ACTION_LABEL, v) && !out.includes(v as DangerousAction)) {
      out.push(v as DangerousAction);
    }
  }
  return out;
}

/** 这一类现在要不要弹确认框。 */
export function needsConfirm(kind: DangerousAction, suppressed: readonly DangerousAction[]): boolean {
  return !suppressed.includes(kind);
}

/** 增/删一条豁免，返回新数组（不改原数组；顺序按 ALL_ACTIONS 归一，免得库里存出随机顺序）。 */
export function withSuppressed(
  suppressed: readonly DangerousAction[],
  kind: DangerousAction,
  on: boolean,
): DangerousAction[] {
  const set = new Set(suppressed);
  if (on) set.add(kind);
  else set.delete(kind);
  return ALL_ACTIONS.filter((a) => set.has(a));
}
