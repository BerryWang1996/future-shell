import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { render, screen, fireEvent, waitFor, cleanup } from "@testing-library/svelte";
import { readFileSync } from "node:fs";
import { join } from "node:path";

/**
 * AI 助手面板（M2 出口第 7、12、14 项）。
 *
 * 这份测试的重心不在「点了按钮有没有反应」，而在三条**行为契约**：
 *
 *   1. 未配置 ≠ 出错（第 7 项）。没配模型源时面板要给「怎么配」，不是一句错误、
 *      更不是编一份假数据糊过去。
 *   2. 危险度裁决来自后端，面板只显示（第 12 项的安全前提）。面板不许有任何
 *      「自己看看这条命令危不危险」的代码路径——那等于把裁决交给最不该做裁决的一层。
 *   3. 总开关关着时功能不可用（第 14 项）。
 */

const calls: { cmd: string; args: unknown }[] = [];
let statusReply: unknown = null;
let suggestReply: unknown = null;
let explainReply: unknown = null;
let throwOn: string | null = null;

vi.mock("../lib/ipc", () => ({
  invoke: async <T>(cmd: string, args?: unknown): Promise<T> => {
    calls.push({ cmd, args });
    if (throwOn === cmd) throw new Error(`${cmd} 炸了`);
    if (cmd === "ai_status") return statusReply as T;
    if (cmd === "ai_suggest_command") return suggestReply as T;
    if (cmd === "ai_explain") return explainReply as T;
    return undefined as T;
  },
}));

import AiPanel from "./AiPanel.svelte";

const status = (over: Record<string, unknown> = {}) => ({
  configured: true,
  active: { kind: "ollama", base_url: "http://127.0.0.1:11434", model: "llama3", allow_screen_context: false },
  entries: [
    {
      id: "abc123",
      kind: "ollama",
      base_url: "http://127.0.0.1:11434",
      model: "llama3",
      has_key: false,
      allow_screen_context: false,
      problem: null,
    },
  ],
  mode: "with_confirm",
  ...over,
});

const suggestion = (over: Record<string, unknown> = {}) => ({
  command: "ss -ltnp | grep 8080",
  explanation: "列出监听中的 TCP 端口，筛出 8080。",
  tier: "read_only",
  reasons: [],
  decision: "auto",
  included_screen: false,
  redactions: [],
  ...over,
});

beforeEach(() => {
  calls.length = 0;
  statusReply = status();
  suggestReply = suggestion();
  explainReply = { markdown: "这段是 **权限不足**。", included_screen: false, redactions: [] };
  throwOn = null;
});
afterEach(cleanup);

describe("未配置态不是错误态（出口第 7 项）", () => {
  it("没有任何模型源时显示配置向导，不显示错误、也不显示假数据", async () => {
    statusReply = status({ configured: false, active: null, entries: [] });
    render(AiPanel, { props: { open: true } });
    await waitFor(() => expect(screen.getByTestId("ai-panel")).toBeTruthy());

    // 落在**配置页**（而不是一个能打字、一按就报错的输入框）。
    // 断言的是 ai-config-intro 而不是 ask 页的 ai-not-configured：
    // 未配置时 refresh 会把页签直接切到「配置」，所以 ask 页那句提示此刻不在 DOM 里。
    // 出口原文要的正是「显示配置向导」——向导在配置页上。
    await waitFor(() => expect(screen.getByTestId("ai-config-intro")).toBeTruthy());
    expect(screen.getByTestId("ai-tab-ask").hasAttribute("disabled")).toBe(true);

    // 关键的反向断言：不许出现任何**看起来像结果**的东西。
    // 「不降级为假数据」是出口原文，而假数据最可能的形态就是一条示例命令
    // ——用户照着一条编出来的命令敲下去，后果由他承担。
    expect(screen.queryByTestId("ai-suggestion")).toBeNull();
    expect(screen.queryByTestId("ai-explanation")).toBeNull();
    // 也没有报错：没配置是待办，不是故障
    expect(screen.queryByTestId("ai-error")).toBeNull();
  });

  it("打开面板只发一次 ai_status（出口第 16 项「≤200ms」的前提）", async () => {
    render(AiPanel, { props: { open: true } });
    await waitFor(() => expect(calls.length).toBeGreaterThan(0));
    // 每多一次 IPC 往返就多一次调度延迟。一次带回全部四样（配没配好/有哪些/哪条在用/什么档位）。
    expect(calls.filter((c) => c.cmd === "ai_status")).toHaveLength(1);
  });

  it("关着的时候一次 IPC 都不发（面板不预热）", async () => {
    render(AiPanel, { props: { open: false } });
    await new Promise((r) => setTimeout(r, 20));
    expect(calls).toEqual([]);
  });
});

describe("裁决来自后端，面板只显示（出口第 12 项的安全前提）", () => {
  it("四档裁决各显示各的文案——strong-confirm 不得折成 confirm", async () => {
    // 这一条钉的是「四档在界面上真的是四种」。曾经的写法是
    // `decision === "deny" ? … : "执行前会问你一次"`，于是 strong-confirm
    // 与 confirm 在界面上一模一样——一条 `rm -rf /` 与一条 `apt install`
    // 给用户看的是同一句话。
    const seen = new Set<string>();
    for (const [decision, expected] of [
      ["auto", "只读命令，可直接执行"],
      ["confirm", "执行前会问你一次"],
      ["strong-confirm", "危险操作，执行前需要强确认"],
      ["deny", "当前档位不允许执行这条命令"],
    ] as const) {
      cleanup();
      suggestReply = suggestion({ decision });
      render(AiPanel, { props: { open: true } });
      await waitFor(() => expect(screen.getByTestId("ai-prompt")).toBeTruthy());
      await fireEvent.input(screen.getByTestId("ai-prompt"), { target: { value: "找出占用 8080 的进程" } });
      await fireEvent.click(screen.getByTestId("ai-ask"));
      await waitFor(() => expect(screen.getByTestId("ai-verdict")).toBeTruthy());
      const text = screen.getByTestId("ai-verdict").textContent ?? "";
      expect(text, decision).toContain(expected);
      expect(seen.has(text), `${decision} 的文案与前一档重复了`).toBe(false);
      seen.add(text);
    }
    expect(seen.size).toBe(4);
  });

  it("deny 档不给「填进命令栏」——按钮禁用，且点了也不回投", async () => {
    suggestReply = suggestion({ decision: "deny", tier: "dangerous", command: "rm -rf /" });
    const used: string[] = [];
    render(AiPanel, { props: { open: true, onUseCommand: (c: string) => used.push(c) } });
    await waitFor(() => expect(screen.getByTestId("ai-prompt")).toBeTruthy());
    await fireEvent.input(screen.getByTestId("ai-prompt"), { target: { value: "清空磁盘" } });
    await fireEvent.click(screen.getByTestId("ai-ask"));
    await waitFor(() => expect(screen.getByTestId("ai-use")).toBeTruthy());

    expect(screen.getByTestId("ai-use").hasAttribute("disabled")).toBe(true);
    // disabled 是**外观**：键盘、自动化、以及将来某次重构都可能绕过它。
    // 所以点一次，确认真的没有回投——挡这一层的是 useSuggestion 里的判断。
    await fireEvent.click(screen.getByTestId("ai-use"));
    expect(used).toEqual([]);
  });

  it("允许执行时「填进命令栏」回投原文，一字不改", async () => {
    const cmd = "ss -ltnp | grep ':8080'";
    suggestReply = suggestion({ command: cmd, decision: "confirm" });
    const used: string[] = [];
    render(AiPanel, { props: { open: true, onUseCommand: (c: string) => used.push(c) } });
    await waitFor(() => expect(screen.getByTestId("ai-prompt")).toBeTruthy());
    await fireEvent.input(screen.getByTestId("ai-prompt"), { target: { value: "看端口" } });
    await fireEvent.click(screen.getByTestId("ai-ask"));
    await waitFor(() => expect(screen.getByTestId("ai-use")).toBeTruthy());
    await fireEvent.click(screen.getByTestId("ai-use"));
    // 引号、管道、空格都不许被「整理」——用户拿到的必须是后端裁决过的**那一条**
    expect(used).toEqual([cmd]);
  });

  it("面板源码里没有任何自行判定危险度的路径", () => {
    // 结构性断言，不是行为断言：只要面板里出现「按命令文本猜危险度」的代码，
    // 上面那些行为断言全都还是绿的——它们喂的是后端给的 decision。
    // 而真正的风险是有人图省事在前端加一句 `if (cmd.includes("rm -rf"))`：
    // 那一刻裁决就有了第二个来源，两个来源迟早不一致，而不一致的方向
    // 一定是「前端说安全、后端说危险」被前端赢了。
    const src = readFileSync(join(__dirname, "AiPanel.svelte"), "utf8")
      .replace(/<!--[\s\S]*?-->/g, "")
      .replace(/\/\*[\s\S]*?\*\//g, "")
      .replace(/(^|[^:])\/\/.*$/gm, "$1");
    for (const banned of ["rm -rf", "dd if=", "mkfs", "shutdown", "DANGEROUS", "isDangerous", "classifyCommand"]) {
      expect(src.includes(banned), `面板源码里出现了 ${banned}——危险度判定只能在后端`).toBe(false);
    }
    // 正向：decision/tier 是**读**进来的，不是算出来的
    expect(src).toContain("suggestion.decision");
  });
});

describe("总开关（出口第 14 项）", () => {
  it("mode=disabled 时说清楚是关着的，并指出去哪儿开", async () => {
    statusReply = status({ mode: "disabled" });
    render(AiPanel, { props: { open: true } });
    await waitFor(() => expect(screen.getByTestId("ai-disabled")).toBeTruthy());
    const t = screen.getByTestId("ai-disabled").textContent ?? "";
    // 「不可用」必须连着「怎么变可用」。只说不可用的提示会让用户以为程序坏了。
    expect(t).toContain("设置");
  });

  // 出口原文是「入口全部**不可用**并呈禁用态」——只有一句提示文案不算。
  // 交叉审计用变异实证过这个洞：把两个控件上的 `|| status.mode === "disabled"`
  // 去掉，全仓 1158 条前端测试全绿。文案与禁用态是两件事，各钉各的。
  it("mode=disabled 时输入框与提问按钮真的处于禁用态（不只是有句提示）", async () => {
    statusReply = status({ mode: "disabled" });
    render(AiPanel, { props: { open: true } });
    await waitFor(() => expect(screen.getByTestId("ai-disabled")).toBeTruthy());
    expect(
      (screen.getByTestId("ai-prompt") as HTMLTextAreaElement).disabled,
      "输入框必须禁用",
    ).toBe(true);
    expect(
      (screen.getByTestId("ai-ask") as HTMLButtonElement).disabled,
      "提问按钮必须禁用",
    ).toBe(true);
  });

  // 反向对照：开着的时候这两个控件是能用的。只有上一条的话，把它们写成
  // 恒 disabled 也能过——而那会让功能在任何档位下都用不了。
  it("mode=with_confirm 且已配置时两个控件可用（反向对照）", async () => {
    statusReply = status({ mode: "with_confirm" });
    render(AiPanel, { props: { open: true } });
    await waitFor(() => expect(screen.getByTestId("ai-prompt")).toBeTruthy());
    expect((screen.getByTestId("ai-prompt") as HTMLTextAreaElement).disabled).toBe(false);
    // ai-ask 还受「输入非空」约束，填一下再看
    await fireEvent.input(screen.getByTestId("ai-prompt"), { target: { value: "看看磁盘" } });
    expect((screen.getByTestId("ai-ask") as HTMLButtonElement).disabled).toBe(false);
  });
});

describe("选区解读（出口第 12 项）", () => {
  it("pendingSelection 进来即跑 ai_explain，选区原文照传", async () => {
    const sel = "Permission denied (publickey).";
    render(AiPanel, { props: { open: true, pendingSelection: { text: sel, seq: 1 }, sessionId: "s1" } });
    await waitFor(() => expect(calls.some((c) => c.cmd === "ai_explain")).toBe(true));
    const c = calls.find((x) => x.cmd === "ai_explain")!;
    expect(c.args).toMatchObject({ selection: sel, sessionId: "s1" });
    await waitFor(() => expect(screen.getByTestId("ai-explanation")).toBeTruthy());
  });

  it("同一段选区再解读一次要真的重跑（seq 递增，不看文本是否变了）", async () => {
    const sel = "connection reset by peer";
    const { rerender } = render(AiPanel, { props: { open: true, pendingSelection: { text: sel, seq: 1 } } });
    await waitFor(() => expect(calls.filter((c) => c.cmd === "ai_explain")).toHaveLength(1));
    // 内容一模一样，只有 seq 变了。「再问一遍」是完全正常的意图——
    // 第一次的回答没说清，用户会再点一次。只比文本的话这里是 1，按钮看起来就是坏的。
    await rerender({ open: true, pendingSelection: { text: sel, seq: 2 } });
    await waitFor(() => expect(calls.filter((c) => c.cmd === "ai_explain")).toHaveLength(2));
  });

  it("seq 没变则不重跑（重渲不该白发一次请求）", async () => {
    const { rerender } = render(AiPanel, { props: { open: true, pendingSelection: { text: "x", seq: 7 } } });
    await waitFor(() => expect(calls.filter((c) => c.cmd === "ai_explain")).toHaveLength(1));
    await rerender({ open: true, pendingSelection: { text: "x", seq: 7 }, sessionId: "s9" });
    await new Promise((r) => setTimeout(r, 20));
    expect(calls.filter((c) => c.cmd === "ai_explain")).toHaveLength(1);
  });
});

describe("失败路径", () => {
  it("ai_suggest_command 抛错时显示错误，不留一个转圈的按钮", async () => {
    throwOn = "ai_suggest_command";
    render(AiPanel, { props: { open: true } });
    await waitFor(() => expect(screen.getByTestId("ai-prompt")).toBeTruthy());
    await fireEvent.input(screen.getByTestId("ai-prompt"), { target: { value: "随便问问" } });
    await fireEvent.click(screen.getByTestId("ai-ask"));
    await waitFor(() => expect(screen.getByTestId("ai-error")).toBeTruthy());
    // busy 必须落回来：finally 里没归位的话，用户只能关掉面板重开
    expect(screen.getByTestId("ai-ask").hasAttribute("disabled")).toBe(false);
  });

  it("存模型源之后把 key 从内存里清掉", async () => {
    statusReply = status({ configured: false, entries: [] });
    render(AiPanel, { props: { open: true } });
    await waitFor(() => expect(screen.getByTestId("ai-form-key")).toBeTruthy());
    const keyInput = screen.getByTestId("ai-form-key") as HTMLInputElement;
    await fireEvent.input(screen.getByTestId("ai-form-url"), { target: { value: "https://api.example.com" } });
    await fireEvent.input(screen.getByTestId("ai-form-model"), { target: { value: "gpt-x" } });
    await fireEvent.input(keyInput, { target: { value: "sk-secret-value" } });
    await fireEvent.click(screen.getByTestId("ai-form-save"));
    // 存完立刻清空：它已经在 Vault 里了，留在这个 DOM 节点上只是多一处泄漏面
    //（截图、录屏、以及「把界面截给同事看」都会带走它）。
    await waitFor(() => expect(keyInput.value).toBe(""));
    const saved = calls.find((c) => c.cmd === "ai_provider_save");
    expect(saved).toBeDefined();
    expect(saved!.args).toMatchObject({ apiKey: "sk-secret-value" });
  });
});

describe("打开时延（出口第 16 项：≤200ms）", () => {
  /**
   * 这一条量的是**我们自己的那部分**：从 render 到面板可交互（输入框拿到手）。
   *
   * 它不量真机上的 200ms，jsdom 里量不出那个数——没有 GPU 合成、没有真实的
   * IPC 往返、字体也不排版。真机那一路是人工闸门（发布前的手动核对项）。
   *
   * 那这条断言的价值在哪？在于**它能捕捉到唯一一类会真的把 200ms 吃掉的回归**：
   * 打开路径上多出一次串行等待。面板现在是「一次 ai_status 带回全部」，
   * 若有人把它拆成「先取 entries、再取 mode、再取 active」，jsdom 里的耗时会
   * 从个位数毫秒跳到三次微任务链——而更要紧的是下面那条并发断言会直接红。
   *
   * 所以门槛定得宽（150ms），真正把守的是它旁边那条「只发一次 IPC，且不串行」。
   * 一个量不出真值的性能断言，若门槛卡得死紧，只会在别人的机器上随机红，
   * 然后被人调宽、再调宽，最后没人看它。
   */
  it("render → 输入框可交互，jsdom 内耗时远低于预算", async () => {
    const t0 = performance.now();
    render(AiPanel, { props: { open: true } });
    await waitFor(() => expect(screen.getByTestId("ai-prompt")).toBeTruthy());
    const ms = performance.now() - t0;
    expect(ms, `打开面板耗时 ${ms.toFixed(1)}ms`).toBeLessThan(150);
  });

  it("打开路径上没有串行 IPC 链：全程只有一次调用", async () => {
    // 真正把守 200ms 的是这条。第一条量的是绝对耗时（在 CI 机器上会抖），
    // 这条量的是**结构**：串行的第二次 IPC 一出现就红，而那才是时延回归的来源。
    render(AiPanel, { props: { open: true } });
    await waitFor(() => expect(screen.getByTestId("ai-prompt")).toBeTruthy());
    // 面板已可交互——此刻若还有别的请求在路上，说明有一段是并发发出的（可接受）
    // 或有一段还没发（那就是串行，且用户已经能打字了却还在等）。
    expect(calls.map((c) => c.cmd)).toEqual(["ai_status"]);
  });
});

describe("存为片段（M4b 出口第 2 项后半）", () => {
  const ask = async (decision: string, command = "ss -ltnp") => {
    suggestReply = suggestion({ decision, command });
    const saved: [string, string][] = [];
    render(AiPanel, {
      props: { open: true, onSaveSnippet: (c: string, d: string) => saved.push([c, d]) },
    });
    await waitFor(() => expect(screen.getByTestId("ai-prompt")).toBeTruthy());
    await fireEvent.input(screen.getByTestId("ai-prompt"), { target: { value: "看端口" } });
    await fireEvent.click(screen.getByTestId("ai-ask"));
    await waitFor(() => expect(screen.getByTestId("ai-save-snippet")).toBeTruthy());
    return saved;
  };

  it("允许执行时把命令与裁决**两样**都交出去", async () => {
    // 传裁决是为了让入库那一步能自己再核一遍（白名单判定），
    // 而不是信任「按钮没禁用所以能存」。
    const saved = await ask("confirm");
    await fireEvent.click(screen.getByTestId("ai-save-snippet"));
    expect(saved).toEqual([["ss -ltnp", "confirm"]]);
  });

  it("deny 档：按钮禁用，且点了也不回投", async () => {
    // 片段库是一键下发的地方，而档位是会变的——今天被拒的一条存进去，
    // 切档之后就成了一个点两下就能跑的按钮。
    const saved = await ask("deny", "rm -rf /var/lib/x");
    expect(screen.getByTestId("ai-save-snippet").hasAttribute("disabled")).toBe(true);
    await fireEvent.click(screen.getByTestId("ai-save-snippet"));
    expect(saved).toEqual([]);
  });

  it("没有 onSaveSnippet 回调时按钮不出现", async () => {
    suggestReply = suggestion({ decision: "auto" });
    render(AiPanel, { props: { open: true } });
    await waitFor(() => expect(screen.getByTestId("ai-prompt")).toBeTruthy());
    await fireEvent.input(screen.getByTestId("ai-prompt"), { target: { value: "x" } });
    await fireEvent.click(screen.getByTestId("ai-ask"));
    await waitFor(() => expect(screen.getByTestId("ai-use")).toBeTruthy());
    expect(screen.queryByTestId("ai-save-snippet")).toBeNull();
  });
});

describe("监控诊断预填（M4b 联动的面板一侧）", () => {
  it("prefillPrompt 进来即填进提问框并立刻发问", async () => {
    // 用户点的是「AI 诊断」，那个动作的语义就是「现在去诊断」。
    // 让他再按一次「问」是多一步，而那一步没给他任何新的决定权。
    render(AiPanel, {
      props: { open: true, prefillPrompt: { text: "web-01 负载异常：…", seq: 1 } },
    });
    await waitFor(() => expect(calls.some((c) => c.cmd === "ai_suggest_command")).toBe(true));
    const c = calls.find((x) => x.cmd === "ai_suggest_command")!;
    expect(c.args).toMatchObject({ prompt: "web-01 负载异常：…" });
    await waitFor(() => expect(screen.getByTestId("ai-verdict")).toBeTruthy());
  });

  it("未配置模型源时**不发**——那时该看到的是配置向导", async () => {
    // 发一次注定失败的请求只会在向导上面糊一条错误，让「先去配」更难看清。
    statusReply = status({ configured: false, entries: [] });
    render(AiPanel, {
      props: { open: true, prefillPrompt: { text: "诊断一下", seq: 1 } },
    });
    await waitFor(() => expect(screen.getByTestId("ai-config-intro")).toBeTruthy());
    await new Promise((r) => setTimeout(r, 20));
    expect(calls.filter((c) => c.cmd === "ai_suggest_command")).toEqual([]);
  });

  it("同一组异常再点一次诊断要真的重问（seq 递增）", async () => {
    const { rerender } = render(AiPanel, {
      props: { open: true, prefillPrompt: { text: "同一段", seq: 1 } },
    });
    await waitFor(() => expect(calls.filter((c) => c.cmd === "ai_suggest_command")).toHaveLength(1));
    await rerender({ open: true, prefillPrompt: { text: "同一段", seq: 2 } });
    await waitFor(() => expect(calls.filter((c) => c.cmd === "ai_suggest_command")).toHaveLength(2));
  });
});
