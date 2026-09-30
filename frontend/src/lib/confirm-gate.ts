/**
 * confirm-gate.ts — 危险动作确认的**闸门**（路线图 M7.2，2026-09-03）。
 *
 * 一个入口：`confirmAction(req)` → `Promise<boolean>`。调用方只管「批准了没有」，
 * 至于弹不弹框、勾没勾过「以后不再显示」、审计怎么写，全在这里收口。
 *
 * # 四条出口标准分别落在哪
 *
 * ① **命令全文摊给用户看**：`req.command` 原样进对话框的 `<pre>`（ActionConfirmDialog），
 *    不截断、不摘要。「确定吗？」式的确认框等于没有确认——用户不知道自己在批准什么。
 * ② **按动作类别记忆**：见 action-confirm.ts 的 `SUPPRESS_KEY`，粒度是 `DangerousAction`。
 * ③ **设置页可撤销**：SettingsDialog「安全与 Vault」页读写同一个键。
 * ④ **无论是否显示确认框，审计照写**：本模块在**每一条**路径上都发 `audit_dangerous_action`——
 *    弹框批准、弹框拒绝、以及因为勾过而没弹框的那一条。豁免掉的是确认框，不是审计。
 *
 * # 审计的 verdict 怎么选
 *
 * 用 `Approved` / `Rejected`，**不**用 `AutoRun`。`AutoRun` 在 audit_repo 里的文档是
 * 「只读，自动放行」——把一次预授权的重启记成「只读自动放行」会误导事后追查的人。
 * 免确认这件事记在 action 文本的前缀上（`[免确认]` vs `[确认]`），可 grep、语义不走样。
 */
import { writable } from "svelte/store";
import { invoke, settingGet, settingSet } from "./ipc";
import { reportFrontendError } from "./ipc";
import { SUPPRESS_KEY, needsConfirm, parseSuppressed, withSuppressed, type DangerousAction } from "./action-confirm";

export interface ConfirmRequest {
  kind: DangerousAction;
  /** 对话框标题，如「重启服务」。 */
  title: string;
  /** **将要执行的命令全文**（出口标准①）。原样展示，不截断。 */
  command: string;
  /** 后果说明。独立成段，用来讲「KILL 无法被捕获」这类不可逆代价。 */
  note?: string;
  /** 确认按钮按危险色渲染。 */
  danger?: boolean;
  /** 审计要记到哪个会话上。 */
  sessionId?: string | null;
  /** 确认按钮文案（缺省「确定」）。 */
  confirmText?: string;
}

/** 正在等用户回答的那一条（null = 没有）。ActionConfirmDialog 订阅它。 */
export interface PendingConfirm extends ConfirmRequest {
  /** 用户按了确认/取消时由对话框调用；`suppress` = 他勾了「以后不再显示」。 */
  answer(approved: boolean, suppress: boolean): void;
}
export const pendingConfirm = writable<PendingConfirm | null>(null);

/** 读当前豁免集（脏值回落空集 = 全部都要确认，见 parseSuppressed 的失败方向说明）。 */
export async function loadSuppressed(): Promise<DangerousAction[]> {
  return parseSuppressed(await settingGet<unknown>(SUPPRESS_KEY, []));
}

/** 写豁免集（设置页的撤销与确认框的勾选共用）。 */
export async function saveSuppressed(list: readonly DangerousAction[]): Promise<boolean> {
  return await settingSet(SUPPRESS_KEY, [...list]) !== false;
}

/**
 * 审计一条危险动作。**恒不抛**——把一次成功的操作因为审计写失败报成失败，用户会重做一次，
 * 而那才是真的多执行了一次（与 audit_command_sent 同口径）。
 */
async function audit(req: ConfirmRequest, approved: boolean, auto: boolean): Promise<void> {
  try {
    await invoke("audit_dangerous_action", {
      kind: req.kind,
      command: req.command,
      sessionId: req.sessionId ?? null,
      approved,
      auto,
    });
  } catch (e) {
    reportFrontendError("audit_dangerous_action", e);
  }
}

/**
 * 危险动作的统一确认。返回 true = 可以执行。
 *
 * 同一时刻只允许一条待确认：第二条来时**直接拒绝**而不是排队或覆盖。覆盖会让用户看到的框
 * 换了内容却以为还是刚才那件事；排队则会在他刚按完一次确认之后立刻弹出第二个框，很容易连按。
 */
export async function confirmAction(req: ConfirmRequest): Promise<boolean> {
  const suppressed = await loadSuppressed();
  if (!needsConfirm(req.kind, suppressed)) {
    await audit(req, true, true); // 出口标准④：没弹框也照写
    return true;
  }

  let busy = false;
  pendingConfirm.update((cur) => {
    busy = cur !== null;
    return cur;
  });
  if (busy) {
    await audit(req, false, false);
    return false;
  }

  const { approved, suppress } = await new Promise<{ approved: boolean; suppress: boolean }>((resolve) => {
    pendingConfirm.set({
      ...req,
      answer(a: boolean, s: boolean) {
        pendingConfirm.set(null);
        resolve({ approved: a, suppress: s });
      },
    });
  });

  // 勾选只在**批准**时生效：在取消里勾「以后不再显示」是自相矛盾的（下次直接放行一件他刚拒绝的事）。
  if (approved && suppress) {
    await saveSuppressed(withSuppressed(suppressed, req.kind, true));
  }
  await audit(req, approved, false);
  return approved;
}

/** 仅测试用：清空待确认。 */
export function resetConfirmGateForTest(): void {
  pendingConfirm.set(null);
}
