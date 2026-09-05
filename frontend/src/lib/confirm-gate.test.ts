/**
 * 危险动作确认闸（路线图 M7.2「统一确认口径」，2026-09-03）。
 *
 * 四条出口标准里有三条能在这里钉死：
 * ② 按动作类别记忆（不是一个全局开关——一次勾选放行全部动作等于把闸拆了）；
 * ③ 撤销之后重新弹框；
 * ④ **无论是否显示确认框，审计照写**——包括因为勾过而没弹框的那一条。
 * 标准①（命令全文摊给用户看）在 ActionConfirmDialog 的组件测试里。
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";

const calls: { cmd: string; args?: Record<string, unknown> }[] = [];
const settings: Record<string, unknown> = {};
let invokeThrows = false;
vi.mock("./ipc", () => ({
  invoke: async (cmd: string, args?: Record<string, unknown>) => {
    calls.push({ cmd, args });
    if (invokeThrows) throw new Error("audit backend down");
    return undefined;
  },
  settingGet: async <T>(k: string, fb: T) => (k in settings ? (settings[k] as T) : fb),
  settingSet: async (k: string, v: unknown) => {
    settings[k] = v;
  },
  reportFrontendError: vi.fn(),
}));

import { SUPPRESS_KEY } from "./action-confirm";
import {
  confirmAction,
  loadSuppressed,
  pendingConfirm,
  resetConfirmGateForTest,
  saveSuppressed,
  type ConfirmRequest,
} from "./confirm-gate";

const REQ = {
  kind: "process.kill" as const,
  title: "终止进程",
  command: "kill -KILL 4321",
  note: "KILL 无法被捕获",
  sessionId: "s1",
};

/** 发起一次确认，等对话框挂上，返回「回答」函数。 */
async function ask(req: ConfirmRequest = REQ) {
  const p = confirmAction(req);
  // pendingConfirm 在 settingGet 落定后才被设上（loadSuppressed 是 async）
  for (let i = 0; i < 10 && get(pendingConfirm) === null; i++) await Promise.resolve();
  const cur = get(pendingConfirm);
  expect(cur, "确认框没挂上").not.toBeNull();
  return { promise: p, answer: cur!.answer };
}

const audits = () => calls.filter((c) => c.cmd === "audit_dangerous_action");

beforeEach(() => {
  calls.length = 0;
  invokeThrows = false;
  for (const k of Object.keys(settings)) delete settings[k];
  resetConfirmGateForTest();
});

describe("① 弹框路径", () => {
  it("批准 → 返回 true，并把命令全文交给了对话框", async () => {
    const { promise, answer } = await ask();
    expect(get(pendingConfirm)!.command).toBe("kill -KILL 4321");
    answer(true, false);
    await expect(promise).resolves.toBe(true);
    expect(get(pendingConfirm), "回答后要收起").toBeNull();
  });

  it("取消 → 返回 false", async () => {
    const { promise, answer } = await ask();
    answer(false, false);
    await expect(promise).resolves.toBe(false);
  });

  it("同一时刻只允许一条待确认：第二条直接拒绝，不覆盖也不排队", async () => {
    const { promise, answer } = await ask();
    const second = await confirmAction({ ...REQ, command: "kill -KILL 999" });
    expect(second, "第二条应被拒绝").toBe(false);
    expect(get(pendingConfirm)!.command, "第一条不该被覆盖").toBe("kill -KILL 4321");
    answer(true, false);
    await promise;
  });
});

describe("② 按动作类别记忆", () => {
  it("勾了「以后不再显示」并批准 → 落库，同类下次不再弹框", async () => {
    const { promise, answer } = await ask();
    answer(true, true);
    await promise;
    expect(settings[SUPPRESS_KEY]).toEqual(["process.kill"]);

    calls.length = 0;
    await expect(confirmAction(REQ)).resolves.toBe(true);
    expect(get(pendingConfirm), "已豁免的类别不该再弹框").toBeNull();
  });

  it("**只豁免这一类**：别的类别照弹（一次勾选放行全部 = 把闸拆了）", async () => {
    settings[SUPPRESS_KEY] = ["process.kill"];
    const { promise, answer } = await ask({ ...REQ, kind: "service.restart", command: "systemctl restart nginx" });
    expect(get(pendingConfirm)!.kind).toBe("service.restart");
    answer(false, false);
    await promise;
  });

  it("在**取消**里勾「以后不再显示」不落库（下次直接放行一件他刚拒绝的事，自相矛盾）", async () => {
    const { promise, answer } = await ask();
    answer(false, true);
    await promise;
    expect(settings[SUPPRESS_KEY]).toBeUndefined();
  });

  it("库里是脏值 → 当没勾过（失败方向：多按一次确认 vs 无确认重启生产服务）", async () => {
    settings[SUPPRESS_KEY] = ["process.kill", 42, "不认识的类别", null];
    expect(await loadSuppressed()).toEqual(["process.kill"]);
    settings[SUPPRESS_KEY] = "process.kill"; // 不是数组
    expect(await loadSuppressed()).toEqual([]);
  });
});

describe("③ 撤销之后重新弹框", () => {
  it("设置页撤销 → 同类再次要求确认", async () => {
    settings[SUPPRESS_KEY] = ["process.kill"];
    await expect(confirmAction(REQ)).resolves.toBe(true); // 免确认
    expect(get(pendingConfirm)).toBeNull();

    await saveSuppressed([]); // 设置页的「恢复确认」
    const { promise, answer } = await ask();
    answer(true, false);
    await expect(promise).resolves.toBe(true);
  });
});

describe("④ 无论是否显示确认框，审计照写", () => {
  it("弹框批准：记 approved=true, auto=false", async () => {
    const { promise, answer } = await ask();
    answer(true, false);
    await promise;
    expect(audits()).toHaveLength(1);
    expect(audits()[0].args).toEqual({
      kind: "process.kill",
      command: "kill -KILL 4321",
      sessionId: "s1",
      approved: true,
      auto: false,
    });
  });

  it("弹框拒绝：照样写一条 approved=false（拒绝也是一次裁决）", async () => {
    const { promise, answer } = await ask();
    answer(false, false);
    await promise;
    expect(audits()[0].args!.approved).toBe(false);
    expect(audits()[0].args!.auto).toBe(false);
  });

  it("**没弹框那条也写**，且标 auto=true——豁免掉的是确认框，不是审计", async () => {
    settings[SUPPRESS_KEY] = ["process.kill"];
    await confirmAction(REQ);
    expect(audits(), "免确认路径漏了审计 = 「以后不再显示」变成了能关审计的开关").toHaveLength(1);
    expect(audits()[0].args!.auto).toBe(true);
    expect(audits()[0].args!.approved).toBe(true);
  });

  it("被并发拒掉的那一条也记（否则日志里那次点击凭空消失）", async () => {
    const { promise, answer } = await ask();
    calls.length = 0;
    await confirmAction({ ...REQ, command: "kill -KILL 999" });
    expect(audits()).toHaveLength(1);
    expect(audits()[0].args!.approved).toBe(false);
    answer(true, false);
    await promise;
  });

  it("审计写失败不影响裁决结果（否则用户会重做一次 = 真的多执行一次）", async () => {
    invokeThrows = true;
    const { promise, answer } = await ask();
    answer(true, false);
    await expect(promise).resolves.toBe(true);
  });
});
