import { describe, it, expect, beforeEach, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/svelte";
import { tick } from "svelte";

const calls: { cmd: string; args: any }[] = [];
vi.mock("../lib/ipc", () => ({
  invoke: async (cmd: string, args?: any) => {
    calls.push({ cmd, args });
    return undefined;
  },
}));

/** 事件名 → 处理器：测试直接投递 `hostkey:prompt` 把对话框打开。 */
const handlers = new Map<string, (e: any) => void>();
vi.mock("@tauri-apps/api/event", () => ({
  listen: (name: string, cb: (e: any) => void) => {
    handlers.set(name, cb);
    return Promise.resolve(() => handlers.delete(name));
  },
}));

import HostKeyDialog from "./HostKeyDialog.svelte";

const PROMPT = {
  session_id: "s1",
  promptId: "p1",
  host: "example.com",
  port: 22,
  key_type: "ssh-ed25519",
  fingerprint: "SHA256:aaaa",
  kind: "tofu" as const,
};

/** 挂载并投递一次提问；listen 在 $effect 里注册，需让出微任务后处理器才就位。 */
async function openDialog(payload: Record<string, unknown> = {}) {
  render(HostKeyDialog);
  await tick();
  const h = handlers.get("hostkey:prompt");
  if (!h) throw new Error("组件未订阅 hostkey:prompt");
  h({ payload: { ...PROMPT, ...payload } });
  await tick();
}

describe("HostKeyDialog 裁决取值契约", () => {
  beforeEach(() => {
    calls.length = 0;
    handlers.clear();
  });

  // 这条钉的是一个静默把安全裁决反转的缺陷：主按钮此前发 `accept_persist`，
  // 而后端 `parse_hostkey_choice` 认的是 `accept_record`，且当时还有 `_ => Refuse` 兜底——
  // 于是每一次首连 TOFU 的「接受并记录」都被翻译成**拒绝**，两侧都不报错。
  // 断言写死字面量而非引用常量：契约的两端本就分处 TS 与 Rust，测试必须替人盯住这个字面量。
  it("「接受并记录」发 accept_record（不是 accept_persist）", async () => {
    await openDialog();
    await fireEvent.click(screen.getByTestId("hk-persist"));
    expect(calls).toEqual([
      { cmd: "hostkey_decide", args: { sessionId: "s1", promptId: "p1", choice: "accept_record" } },
    ]);
  });

  it("「仅本次接受」发 accept_once", async () => {
    await openDialog();
    await fireEvent.click(screen.getByTestId("hk-once"));
    expect(calls[0].args.choice).toBe("accept_once");
  });

  it("「拒绝」发 refuse（不是 reject）", async () => {
    await openDialog();
    await fireEvent.click(screen.getByTestId("hk-reject"));
    expect(calls[0].args.choice).toBe("refuse");
  });

  it("Esc 关闭 = 拒绝，同样走契约取值 refuse", async () => {
    await openDialog();
    await fireEvent.keyDown(screen.getByTestId("hostkey-dialog"), { key: "Escape" });
    expect(calls[0].args.choice).toBe("refuse");
  });

  // 三个取值一次性对照契约白名单：任何一侧再漂一个词都会在这里断。
  it("发出的取值必在后端契约白名单内", async () => {
    const contract = new Set(["accept_record", "accept_once", "refuse"]);
    for (const id of ["hk-persist", "hk-once", "hk-reject"]) {
      calls.length = 0;
      handlers.clear();
      await openDialog();
      await fireEvent.click(screen.getByTestId(id));
      expect(contract.has(calls[0].args.choice)).toBe(true);
    }
  });

  it("裁决后对话框关闭（连点不会二次投递）", async () => {
    await openDialog();
    await fireEvent.click(screen.getByTestId("hk-reject"));
    expect(screen.queryByTestId("hostkey-dialog")).toBeNull();
    expect(calls).toHaveLength(1);
  });
});

/**
 * 「确认框调包」回归测试（审计 P1：前端单槽 prompt 覆盖）。
 *
 * 修复前，`hostkey:prompt` 每来一条就整片覆写组件状态。三个按钮的文字、位置、样式逐字相同，
 * 只有主机与指纹那两行变了——用户正要点「接受并记录」的那一下，落到的是**另一台主机**的确认上；
 * 被换掉的那一枚无人应答，一直挂到后端 120 s 超时（表现为连接莫名卡住）。
 * 并发不是假想：两个会话各自跑重连循环、一条跳板链上多跳先后要确认，都会几乎同时发出提问。
 */
const PROMPT_B = {
  session_id: "s2",
  promptId: "p2",
  host: "other.example.net",
  port: 2222,
  key_type: "ssh-rsa",
  fingerprint: "SHA256:bbbb",
  kind: "changed" as const,
  old_fingerprint: "SHA256:cccc",
};

describe("HostKeyDialog 并发提问排队", () => {
  beforeEach(() => {
    calls.length = 0;
    handlers.clear();
  });

  async function emit(payload: Record<string, unknown>) {
    const h = handlers.get("hostkey:prompt");
    if (!h) throw new Error("组件未订阅 hostkey:prompt");
    h({ payload });
    await tick();
  }

  it("后到的提问不替换正在显示的一条", async () => {
    await openDialog(); // PROMPT（example.com:22, tofu）
    await emit(PROMPT_B);
    // 主机行仍是第一条的。若这里读到 other.example.net，说明用户面前的框已被调包。
    expect(screen.getByTestId("hk-host").textContent).toBe("example.com:22");
    // 第一条是 tofu，不该出现密钥变更告警——身份换了而告警块跟着换，是同一个缺陷的另一面
    expect(screen.queryByText("警告：主机密钥已变更")).toBeNull();
  });

  it("裁决当前这一条后下一条就位，两条各自带自己的 promptId", async () => {
    await openDialog();
    await emit(PROMPT_B);
    await fireEvent.click(screen.getByTestId("hk-persist"));
    expect(calls[0].args).toEqual({ sessionId: "s1", promptId: "p1", choice: "accept_record" });

    expect(screen.getByTestId("hk-host").textContent).toBe("other.example.net:2222");
    await fireEvent.click(screen.getByTestId("hk-reject"));
    expect(calls[1].args).toEqual({ sessionId: "s2", promptId: "p2", choice: "refuse" });
  });

  it("排队时显示还有几个待确认", async () => {
    await openDialog();
    expect(screen.queryByTestId("hk-queued")).toBeNull();
    await emit(PROMPT_B);
    expect(screen.getByTestId("hk-queued").textContent).toContain("还有 1 个");
  });

  // 人已离开机器：整队按拒绝收尾。只收当前一条的话，排在后面的会一直挂到后端超时，
  // 解锁后还会弹出一串来历不明的高危确认框。
  it("vault 锁定 → 整队按 refuse 投递", async () => {
    const { component } = render(HostKeyDialog) as any;
    await tick();
    await emit(PROMPT);
    await emit(PROMPT_B);

    component.dismissForVaultLock();
    await tick();

    expect(screen.queryByTestId("hostkey-dialog")).toBeNull();
    expect(calls.map((c) => [c.args.promptId, c.args.choice])).toEqual([
      ["p1", "refuse"],
      ["p2", "refuse"],
    ]);
  });
});
