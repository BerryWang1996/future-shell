import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { render, screen, fireEvent, waitFor, cleanup } from "@testing-library/svelte";

/**
 * 审计链校验对话框（M3 出口第 7 项「快照校验命令可用」的界面那一半）。
 *
 * 这份测试钉的不是按钮点了有没有反应，而是三件事：
 *
 *   1. **链断了要按断的样子显示**，且断因那句话（追查方向）原样上屏——
 *      那是给事后调查的第一条线索，不是给用户的安慰。
 *   2. **导出的披露常驻在对话框里**：forensics::export 的文档把「导出前 UI 上
 *      明确提示包里含命令文本」写成调用方的义务。披露段落消失是可能的回归
 *      （重构时被当成装饰性文案删掉），而它是义务不是装饰。
 *   3. 校验取证包不接触数据库——命令收 JSON 文本，路径是谁选的、文件从哪来
 *      都不出前端。
 */

const calls: { cmd: string; args?: unknown }[] = [];
let verifyReply: unknown = null;
let quickReply: unknown = null;
let exportReply: unknown = null;
let bundleReply: unknown = null;
let throwOn: string | null = null;

vi.mock("../lib/ipc", () => ({
  invoke: async <T>(cmd: string, args?: unknown): Promise<T> => {
    calls.push({ cmd, args });
    if (throwOn === cmd) throw new Error(`${cmd} 失败`);
    if (cmd === "audit_verify") return verifyReply as T;
    if (cmd === "audit_verify_quick") return quickReply as T;
    if (cmd === "audit_export") return exportReply as T;
    if (cmd === "audit_verify_bundle") return bundleReply as T;
    return undefined as T;
  },
}));

vi.mock("../lib/toast", () => ({
  toast: { info: vi.fn(), warn: vi.fn(), error: vi.fn() },
}));

import AuditDialog from "./AuditDialog.svelte";

beforeEach(() => {
  calls.length = 0;
  verifyReply = { ok: true, rows: 12, at_id: null, why: null, message: "审计链完整，共 12 条记录。" };
  quickReply = {
    ok: true,
    full_coverage: false,
    at_id: null,
    message:
      "校验通过，重算了 3 条记录。覆盖范围：仅第 9 条之后（2026-08-26T12:00:00Z 的快照）。" +
      "**第 9 条及之前的 9 条这次未重算**——快照与审计表同库同权限，它证明不了那一段。" +
      "需要完整证明请跑全表校验。",
  };
  exportReply = "{}";
  bundleReply = { usable: true, message: "取证包校验通过，共 5 条审计记录，哈希链完整。" };
  throwOn = null;
});
afterEach(cleanup);

describe("本机链状态", () => {
  it("打开即校验；完整态显示行数", async () => {
    render(AuditDialog, { props: { open: true } });
    await waitFor(() => expect(screen.getByTestId("audit-chain-ok")).toBeTruthy());
    expect(screen.getByTestId("audit-chain-ok").textContent).toContain("12");
    expect(calls.filter((c) => c.cmd === "audit_verify")).toHaveLength(1);
  });

  it("断裂态标红、role=alert，且断因那句话原样上屏", async () => {
    verifyReply = {
      ok: false,
      rows: 0,
      at_id: 7,
      why: "link_mismatch",
      message: "审计链在第 7 条记录处断裂。这一行的 prev_hash 与上一行接不上：有行被删掉或插入过。查「谁动过这张表」。",
    };
    render(AuditDialog, { props: { open: true } });
    const el = await waitFor(() => screen.getByTestId("audit-chain-broken"));
    // 追查方向（「查谁动过这张表」）是后端 message 的一部分，必须到达屏幕，
    // 不能被前端截成一句「校验失败」。
    expect(el.textContent).toContain("第 7 条");
    expect(el.textContent).toContain("谁动过这张表");
    expect(el.getAttribute("role")).toBe("alert");
    // 完整态的绿色标记不能同时出现
    expect(screen.queryByTestId("audit-chain-ok")).toBeNull();
  });

  it("「重新校验」真的再发一次请求（上一次的绿不能带过夜）", async () => {
    render(AuditDialog, { props: { open: true } });
    await waitFor(() => expect(screen.getByTestId("audit-chain-ok")).toBeTruthy());
    await fireEvent.click(screen.getByTestId("audit-recheck"));
    await waitFor(() =>
      expect(calls.filter((c) => c.cmd === "audit_verify")).toHaveLength(2),
    );
  });

  it("校验命令本身失败 ≠ 链断了——错误走 toast，不显示断链态", async () => {
    throwOn = "audit_verify";
    render(AuditDialog, { props: { open: true } });
    await waitFor(() => expect(calls.some((c) => c.cmd === "audit_verify")).toBe(true));
    await new Promise((r) => setTimeout(r, 10));
    // 「库读不了」与「审计被篡改」是两个完全不同严重度的事件；
    // 把前者显示成后者会触发一次没有发生过的安全调查。
    expect(screen.queryByTestId("audit-chain-broken")).toBeNull();
    expect(screen.queryByTestId("audit-chain-ok")).toBeNull();
  });
});

describe("取证包导出（披露是义务，不是装饰）", () => {
  it("披露段常驻：包含命令文本、会离开本机", async () => {
    render(AuditDialog, { props: { open: true } });
    await waitFor(() => expect(screen.getByTestId("audit-export-warning")).toBeTruthy());
    const t = screen.getByTestId("audit-export-warning").textContent ?? "";
    expect(t).toContain("命令文本");
    expect(t).toContain("离开本机");
    // 按钮文案本身也携带披露——点下去的那一刻提示还在眼前
    expect(screen.getByTestId("audit-export").textContent).toContain("命令文本");
  });

  it("点导出 → audit_export 一次；命令失败时 toast 而不是静默", async () => {
    render(AuditDialog, { props: { open: true } });
    await waitFor(() => expect(screen.getByTestId("audit-export")).toBeTruthy());
    await fireEvent.click(screen.getByTestId("audit-export"));
    await waitFor(() => expect(calls.some((c) => c.cmd === "audit_export")).toBe(true));
    expect(calls.filter((c) => c.cmd === "audit_export")).toHaveLength(1);

    cleanup();
    calls.length = 0;
    throwOn = "audit_export";
    render(AuditDialog, { props: { open: true } });
    await waitFor(() => expect(screen.getByTestId("audit-export")).toBeTruthy());
    await fireEvent.click(screen.getByTestId("audit-export"));
    await waitFor(() => expect(calls.some((c) => c.cmd === "audit_export")).toBe(true));
  });
});

describe("校验一份取证包", () => {
  /** jsdom 里造一个 File 并走真实的 onchange 路径（走 input.files 的那条逻辑）。
   *  jsdom 没有 DataTransfer，而 handler 只读 `.files?.[0]`——给个带 [0] 的
   *  数组就够了，不需要真 FileList。 */
  async function pickFile(name: string, content: string): Promise<void> {
    const file = new File([content], name, { type: "application/json" });
    const input = screen.getByTestId("audit-pick-label").querySelector("input")!;
    Object.defineProperty(input, "files", { value: [file], configurable: true });
    await fireEvent.change(input);
  }

  it("选文件 → 读文本 → invoke audit_verify_bundle（收 JSON 文本，不是路径）", async () => {
    render(AuditDialog, { props: { open: true } });
    await waitFor(() => expect(screen.getByTestId("audit-pick-label")).toBeTruthy());
    await pickFile("audit-bundle.json", '{"version":1}');
    await waitFor(() => expect(screen.getByTestId("audit-bundle-result")).toBeTruthy());
    const c = calls.find((x) => x.cmd === "audit_verify_bundle")!;
    expect(c.args).toMatchObject({ content: '{"version":1}' });
    expect(screen.getByTestId("audit-bundle-result").textContent).toContain("校验通过");
  });

  it("包被篡改 → 不可用态 + role=alert", async () => {
    bundleReply = { usable: false, message: "第 3 条记录的内容与它的哈希不符：这一行被改过。" };
    render(AuditDialog, { props: { open: true } });
    await waitFor(() => expect(screen.getByTestId("audit-pick-label")).toBeTruthy());
    await pickFile("bad.json", "{}");
    const el = await waitFor(() => screen.getByTestId("audit-bundle-result"));
    expect(el.getAttribute("role")).toBe("alert");
    expect(el.textContent).toContain("被改过");
  });

  it("同一个文件连选两次要真的各验一次（input.value 复位）", async () => {
    render(AuditDialog, { props: { open: true } });
    await waitFor(() => expect(screen.getByTestId("audit-pick-label")).toBeTruthy());
    await pickFile("a.json", "{}");
    await waitFor(() =>
      expect(calls.filter((c) => c.cmd === "audit_verify_bundle")).toHaveLength(1),
    );
    await pickFile("a.json", "{}");
    await waitFor(() =>
      expect(calls.filter((c) => c.cmd === "audit_verify_bundle")).toHaveLength(2),
    );
    // 不复位 value 的话第二次 selectionchange 不触发——「再验一次」是最正常的意图
    //（第一次验完用户去改了文件内容，同一文件名再选，内容已经不同）。
  });
});

describe("关着的时候", () => {
  it("不渲染、不发任何请求（对话框不预热）", async () => {
    render(AuditDialog, { props: { open: false } });
    await new Promise((r) => setTimeout(r, 20));
    expect(calls).toEqual([]);
    expect(screen.queryByTestId("audit-dialog")).toBeNull();
  });
});

describe("增量校验", () => {
  it("打开对话框跑的是全表校验，不是增量——正确的默认不让位于快的默认", async () => {
    render(AuditDialog, { props: { open: true } });
    await waitFor(() => expect(screen.getByTestId("audit-chain-ok")).toBeTruthy());
    expect(calls.filter((c) => c.cmd === "audit_verify")).toHaveLength(1);
    expect(calls.filter((c) => c.cmd === "audit_verify_quick")).toHaveLength(0);
    // 增量结果块此时不该存在——没点过就不该有结论。
    expect(screen.queryByTestId("audit-quick-result")).toBeNull();
  });

  it("「通过但没查全」显示 ⚠ 而不是 ✓", async () => {
    // 这一条是本组的重心。一个绿勾会被读成「审计链完整」，
    // 而增量校验只说了「查过的那一段没问题」——没查的那一段这次什么也没说。
    render(AuditDialog, { props: { open: true } });
    await waitFor(() => expect(screen.getByTestId("audit-quick")).toBeTruthy());
    await fireEvent.click(screen.getByTestId("audit-quick"));
    await waitFor(() => expect(screen.getByTestId("audit-quick-result")).toBeTruthy());
    const el = screen.getByTestId("audit-quick-result");
    expect(el.textContent).toContain("⚠");
    expect(el.textContent).not.toContain("✓");
    // 覆盖范围声明必须原样上屏，不被本层重写或截断。
    expect(el.textContent).toContain("未重算");
    expect(el.textContent).toContain("全表校验");
  });

  it("全覆盖的增量结果（库里还没有快照）才给 ✓", async () => {
    // 反向对照：没有快照时增量退化成全表，那时 ⚠ 是误报。
    quickReply = {
      ok: true,
      full_coverage: true,
      at_id: null,
      message: "校验通过，重算了 12 条记录。覆盖范围：全表。",
    };
    render(AuditDialog, { props: { open: true } });
    await waitFor(() => expect(screen.getByTestId("audit-quick")).toBeTruthy());
    await fireEvent.click(screen.getByTestId("audit-quick"));
    await waitFor(() => expect(screen.getByTestId("audit-quick-result")).toBeTruthy());
    const el = screen.getByTestId("audit-quick-result");
    expect(el.textContent).toContain("✓");
    expect(el.textContent).not.toContain("⚠");
  });

  it("增量查出断链显示 ✗ 并上 alert", async () => {
    quickReply = {
      ok: false,
      full_coverage: false,
      at_id: 14,
      message: "哈希链在第 14 条处断开（ContentAltered）。覆盖范围：仅第 9 条之后……",
    };
    render(AuditDialog, { props: { open: true } });
    await waitFor(() => expect(screen.getByTestId("audit-quick")).toBeTruthy());
    await fireEvent.click(screen.getByTestId("audit-quick"));
    await waitFor(() => expect(screen.getByTestId("audit-quick-result")).toBeTruthy());
    const el = screen.getByTestId("audit-quick-result");
    expect(el.textContent).toContain("✗");
    expect(el.getAttribute("role")).toBe("alert");
  });

  it("全表与增量的结论各占一块，不共用显示位", async () => {
    // 两句结论的分量不同。叠在同一个元素上迟早会被读成同一件事——
    // 组件里它们是两个 $state，这里钉的是那个决定在 DOM 上真的成立。
    render(AuditDialog, { props: { open: true } });
    await waitFor(() => expect(screen.getByTestId("audit-chain-ok")).toBeTruthy());
    await fireEvent.click(screen.getByTestId("audit-quick"));
    await waitFor(() => expect(screen.getByTestId("audit-quick-result")).toBeTruthy());
    // 增量跑完之后，全表那条结论仍在原处——没被覆盖掉。
    expect(screen.getByTestId("audit-chain-ok").textContent).toContain("12");
    expect(screen.getByTestId("audit-quick-result").textContent).toContain("未重算");
  });

  it("重新打开对话框会清掉上一次的增量结论", async () => {
    // 链的状态只能代表「此刻」，上一次的结论不能带过来——全表那条已有此约束，
    // 增量这条同理，否则关掉再开会看到一句过期的「通过」。
    const { unmount } = render(AuditDialog, { props: { open: true } });
    await waitFor(() => expect(screen.getByTestId("audit-quick")).toBeTruthy());
    await fireEvent.click(screen.getByTestId("audit-quick"));
    await waitFor(() => expect(screen.getByTestId("audit-quick-result")).toBeTruthy());
    unmount();

    render(AuditDialog, { props: { open: true } });
    await waitFor(() => expect(screen.getByTestId("audit-chain-ok")).toBeTruthy());
    expect(screen.queryByTestId("audit-quick-result")).toBeNull();
  });
});
