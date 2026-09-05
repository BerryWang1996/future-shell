import { describe, it, expect, beforeEach, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/svelte";
import { tick } from "svelte";

/**
 * 「口令串台」回归测试（审计 P1：前端单槽 prompt 覆盖）。
 *
 * 修复前，`auth:prompt` 每来一条就整片覆写组件状态。用户正在给 A 输密码时 B 的提问到达，
 * 框里的 sessionId/promptId 已经悄悄换成 B、输入框被清空——而框上**不显示任何主机身份**，
 * 两边的提示语又通常都是服务端给的同一句 `Password:`。用户把 A 的口令打完回车，
 * 这份明文就投给了 B；A 那一枚无人应答，挂到后端 120 s 超时。
 *
 * 所以这里钉的是两件互补的事：
 * ① 已显示的提问只由用户的提交/取消推进，后到的一律排队；
 * ② 框上始终标明**是谁在问**，且身份取自本地配置而非对端自报的字符串。
 */

const calls: { cmd: string; args: any }[] = [];
vi.mock("../lib/ipc", () => ({
  invoke: async (cmd: string, args?: any) => {
    calls.push({ cmd, args });
    return undefined;
  },
}));

/** 事件名 → 处理器：测试直接投递 `auth:prompt`。 */
const handlers = new Map<string, (e: any) => void>();
vi.mock("@tauri-apps/api/event", () => ({
  listen: (name: string, cb: (e: any) => void) => {
    handlers.set(name, cb);
    return Promise.resolve(() => handlers.delete(name));
  },
}));

import AuthPromptDialog from "./AuthPromptDialog.svelte";

const A = {
  session_id: "aaaaaaaa-1111-1111-1111-111111111111",
  promptId: "p-A",
  target: "root@alpha:22",
  prompts: [{ text: "Password:", echo: false }],
};
/** 与 A 逐字相同的提示语——正是这一点让「被换掉」在界面上无法察觉。 */
const B = {
  session_id: "bbbbbbbb-2222-2222-2222-222222222222",
  promptId: "p-B",
  target: "ops@beta:2222",
  prompts: [{ text: "Password:", echo: false }],
};

/** 挂载；listen 在 $effect 里注册，需让出微任务后处理器才就位。 */
async function mount() {
  render(AuthPromptDialog);
  await tick();
}

async function emit(payload: Record<string, unknown>) {
  const h = handlers.get("auth:prompt");
  if (!h) throw new Error("组件未订阅 auth:prompt");
  h({ payload });
  await tick();
}

/** 往当前可见的第一个输入框里打字。 */
async function type(text: string) {
  await fireEvent.input(screen.getByTestId("auth-input-0"), { target: { value: text } });
}

describe("AuthPromptDialog · 并发提问排队", () => {
  beforeEach(() => {
    calls.length = 0;
    handlers.clear();
  });

  // ① 覆盖本身。断言身份行**与**已输入的明文都不动：只断身份行的话，
  //    一个「换身份但保留输入」的错误实现照样能过——那种实现更危险。
  it("后到的提问不替换正在显示的一条，也不动已输入的内容", async () => {
    await mount();
    await emit(A);
    await type("secret-for-alpha");
    await emit(B);

    expect(screen.getByTestId("auth-target").textContent).toContain("root@alpha:22");
    expect((screen.getByTestId("auth-input-0") as HTMLInputElement).value).toBe("secret-for-alpha");
  });

  // ② 串台的落点：应答必须回到**当时正显示的那一条**的 (sessionId, promptId) 上。
  it("提交把明文投给当前这一条，而不是后到的那条", async () => {
    await mount();
    await emit(A);
    await type("secret-for-alpha");
    await emit(B);
    await fireEvent.click(screen.getByTestId("auth-submit"));

    expect(calls).toEqual([
      {
        cmd: "auth_respond",
        args: {
          sessionId: A.session_id,
          promptId: "p-A",
          responses: ["secret-for-alpha"],
        },
      },
    ]);
  });

  // ③ 换到下一条时必须重建输入槽：上一条的明文残留在框里，用户很可能直接回车把它投给下一台机器。
  it("答完一条后下一条就位，且输入框不残留上一条的明文", async () => {
    await mount();
    await emit(A);
    await type("secret-for-alpha");
    await emit(B);
    await fireEvent.click(screen.getByTestId("auth-submit"));

    expect(screen.getByTestId("auth-target").textContent).toContain("ops@beta:2222");
    expect((screen.getByTestId("auth-input-0") as HTMLInputElement).value).toBe("");
  });

  // ④ 身份行本身。没有它，①②③ 修好了用户依然分不清自己在给谁输口令。
  it("框上标明是谁在问（取自本地配置的 user@host:port）", async () => {
    await mount();
    await emit(A);
    expect(screen.getByTestId("auth-target").textContent).toContain("root@alpha:22");
  });

  // ⑤ target 缺省（旧后端）时退回会话短号：短号至少能把两个并发会话区分开。
  //    绝不能退回服务端自报的 name —— 那串字是对端说的，冒充成身份正是钓鱼要的效果。
  it("target 缺省 → 退回会话短号，不采信服务端自报的 name", async () => {
    await mount();
    await emit({ ...A, target: undefined, name: "Production Gateway" });
    const line = screen.getByTestId("auth-target").textContent ?? "";
    expect(line).toContain(A.session_id.slice(0, 8));
    expect(line).not.toContain("Production Gateway");
  });

  // ⑥ 队列深度可见：否则答完一个又冒出一个几乎一样的框，用户第一反应是「刚才那下没点上」。
  it("排队时显示还有几个待应答", async () => {
    await mount();
    await emit(A);
    expect(screen.queryByTestId("auth-queued")).toBeNull();
    await emit(B);
    expect(screen.getByTestId("auth-queued").textContent).toContain("还有 1 个");
  });

  // ⑦ 人已离开机器：整队收掉，且**不替用户投递任何应答**（后端对未决 prompt 自带超时，
  //    比在这里猜一个答案安全）。只收当前一条的话，解锁后会弹出一串来历不明的框。
  it("vault 锁定 → 整队清空，不投递任何应答", async () => {
    const { component } = render(AuthPromptDialog) as any;
    await tick();
    await emit(A);
    await type("secret-for-alpha");
    await emit(B);

    component.dismissForVaultLock();
    await tick();

    expect(screen.queryByTestId("auth-dialog")).toBeNull();
    expect(calls).toEqual([]);
  });

  // 2026-09-02：取消的语义从「只关框」改为「立刻回空应答」。此前靠后端 120 秒超时
  // 收尾，用户取消后标签还「正在连接」转两分钟（真机日志两次都是整 120 秒）。
  // 空应答 = 用户放弃：后端取 first() 得空串，引擎不会拿空口令去撞服务器。
  it("取消：给**被取消的那条**立刻投空应答；下一条随即就位且不被代答", async () => {
    await mount();
    await emit(A);
    await emit(B);
    await fireEvent.click(screen.getByText("取消"));
    await tick();

    expect(calls).toEqual([
      { cmd: "auth_respond", args: { sessionId: A.session_id, promptId: "p-A", responses: [""] } },
    ]);
    // B 仍在等真人：不得被顺手代答
    expect(screen.getByTestId("auth-target").textContent).toContain("ops@beta:2222");
  });

  it("取消的空应答按 prompts 条数对齐（多提示的 kbd 回合每一格都是空串）", async () => {
    await mount();
    await emit({ ...A, promptId: "p-multi", prompts: [{ text: "Password:", echo: false }, { text: "OTP:", echo: true }] });
    await fireEvent.click(screen.getByText("取消"));
    await tick();
    expect(calls[0]?.args).toEqual({ sessionId: A.session_id, promptId: "p-multi", responses: ["", ""] });
  });
});

/**
 * 连接时输口令（Task 47）：档案没配口令时后端发 `auth:prompt{kind:"password"}`，
 * 前端多一个「记住（存入 Vault）」勾选；勾选提交后额外调 `profile_store_password`。
 * kbd-interactive（kind 缺省）不得出现该勾选——服务端问的可能是 OTP/新密码，存下来是错的。
 */
describe("AuthPromptDialog · 连接时输口令（Task 47）", () => {
  beforeEach(() => {
    calls.length = 0;
    handlers.clear();
  });

  const PASSWORD = {
    session_id: "cccccccc-3333-3333-3333-333333333333",
    promptId: "p-C",
    authKind: "password",
    target: "root@10.0.0.1:22",
    name: "口令认证",
    instruction: "该连接未配置口令，请输入登录口令",
    prompts: [{ text: "密码", echo: false }],
    profileId: "f6b11258-dd1c-494b-8d09-d8bca5011818",
  };

  it("kind=password 显示「记住」勾选；kind 缺省（kbd）不显示", async () => {
    await mount();
    await emit(PASSWORD);
    expect(screen.queryByTestId("auth-remember")).not.toBeNull();

    await fireEvent.click(screen.getByText("取消"));
    await emit({ ...A }); // kbd：无 kind 字段
    expect(screen.queryByTestId("auth-remember")).toBeNull();
  });

  it("勾选「记住」提交 → 先 auth_respond，再 profile_store_password（secretB64）", async () => {
    await mount();
    await emit(PASSWORD);
    await type("hunter2");
    await fireEvent.click(screen.getByTestId("auth-remember"));
    await fireEvent.click(screen.getByTestId("auth-submit"));

    expect(calls).toEqual([
      {
        cmd: "auth_respond",
        args: {
          sessionId: PASSWORD.session_id,
          promptId: "p-C",
          responses: ["hunter2"],
        },
      },
      {
        cmd: "profile_store_password",
        args: {
          profileId: "f6b11258-dd1c-494b-8d09-d8bca5011818",
          secretB64: "aHVudGVyMg==", // btoa("hunter2")
        },
      },
    ]);
  });

  it("不勾选「记住」→ 只 auth_respond，不存 Vault", async () => {
    await mount();
    await emit(PASSWORD);
    await type("hunter2");
    await fireEvent.click(screen.getByTestId("auth-submit"));

    expect(calls).toEqual([
      {
        cmd: "auth_respond",
        args: {
          sessionId: PASSWORD.session_id,
          promptId: "p-C",
          responses: ["hunter2"],
        },
      },
    ]);
  });
});

/**
 * kbd-interactive **多提示**（交叉审计 2026-08-25）。
 *
 * 此前全部用例的 `prompts` 都只有一枚——`{#each cur.prompts as p, i}` 在 i≥1 的
 * 分支从没被执行过。而 kbd-interactive 恰恰是**多提示**的主场：服务端可以先问
 * `Password:` 再问 `OTP:`（两步），或一次性问「旧密码 + 新密码 + 确认新密码」。
 * 若 `responses` 与输入框的对齐在第二格上断了（串格、漏格、类型错），用户输入的
 * OTP 就投不到它该去的那一格——这类错误在单提示用例里**永远显不了形**。
 */
describe("AuthPromptDialog · kbd-interactive 多提示", () => {
  beforeEach(() => {
    calls.length = 0;
    handlers.clear();
  });

  /** 一次性两问：口令（隐藏）+ OTP（隐藏）。 */
  const OTP = {
    session_id: "dddddddd-4444-4444-4444-444444444444",
    promptId: "p-otp",
    target: "root@gateway:22",
    prompts: [
      { text: "Password:", echo: false },
      { text: "OTP:", echo: false },
    ],
  };

  it("两枚提示各渲染一个输入框，且应答按格归位、顺序不乱", async () => {
    await mount();
    await emit(OTP);

    // 两个输入框都在（这是 i≥1 分支真的被渲染的直接证据）
    expect(screen.getByTestId("auth-input-0")).toBeTruthy();
    expect(screen.getByTestId("auth-input-1")).toBeTruthy();

    await fireEvent.input(screen.getByTestId("auth-input-0"), { target: { value: "pw-1" } });
    await fireEvent.input(screen.getByTestId("auth-input-1"), { target: { value: "246810" } });
    await fireEvent.click(screen.getByTestId("auth-submit"));

    expect(calls).toEqual([
      {
        cmd: "auth_respond",
        args: {
          sessionId: OTP.session_id,
          promptId: "p-otp",
          // 顺序必须与提示一一对应：第 0 格是口令，第 1 格是 OTP
          responses: ["pw-1", "246810"],
        },
      },
    ]);
  });

  it("三枚提示（旧密码/新密码/确认）同样逐格归位", async () => {
    await mount();
    await emit({
      ...OTP,
      prompts: [
        { text: "旧密码:", echo: false },
        { text: "新密码:", echo: false },
        { text: "确认新密码:", echo: false },
      ],
    });
    await fireEvent.input(screen.getByTestId("auth-input-0"), { target: { value: "old" } });
    await fireEvent.input(screen.getByTestId("auth-input-1"), { target: { value: "new" } });
    await fireEvent.input(screen.getByTestId("auth-input-2"), { target: { value: "new" } });
    await fireEvent.click(screen.getByTestId("auth-submit"));

    expect(calls[0].args.responses).toEqual(["old", "new", "new"]);
  });

  it("echo 决定输入框类型：隐藏回显 ⇒ password，可见 ⇒ text", async () => {
    await mount();
    await emit({
      ...OTP,
      prompts: [
        { text: "口令:", echo: false },
        { text: "用户名:", echo: true }, // 可见的提示（有些服务端先问用户名）
      ],
    });
    expect((screen.getByTestId("auth-input-0") as HTMLInputElement).type).toBe("password");
    expect((screen.getByTestId("auth-input-1") as HTMLInputElement).type).toBe("text");
  });

  // 反向对照：只填第二格、不填第一格，第一格投出去的就是空串而非第二格的内容——
  // 证明两格没有串。否则一个「所有输入都写进 responses[0]」的错误实现，在
  // 「两格都填」的用例里照样能得到两个非空值而蒙混过关。
  it("只填第二格 → 第一格应答为空串，两格互不串", async () => {
    await mount();
    await emit(OTP);
    await fireEvent.input(screen.getByTestId("auth-input-1"), { target: { value: "only-otp" } });
    await fireEvent.click(screen.getByTestId("auth-submit"));

    expect(calls[0].args.responses).toEqual(["", "only-otp"]);
  });
});
