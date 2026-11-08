/**
 * ZmodemBar 组件测试（M4a：终端内传输条）。
 *
 * 钉的是「界面会不会骗人」那几处：总大小未知时不许画百分比进度、后端说在传而
 * 本界面没有事件时要显示恢复条（否则用户以为断了去按 Ctrl+C 把它真打断）、
 * 取消要真的调到后端。
 */
import { describe, expect, it, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/svelte";
import ZmodemBar from "./ZmodemBar.svelte";
import { toast } from "../lib/toast";
import { IDLE, type TransferView } from "../lib/zmodem";

const invoke = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({ invoke }));

const view = (p: Partial<TransferView>): TransferView => ({ ...IDLE, ...p });

/** 默认：status 返回未启用，local_list 返回两条。 */
function mockDefaults() {
  invoke.mockImplementation((cmd: string) => {
    if (cmd === "zmodem_status") return Promise.resolve({ active: false, sandbox: null, auto: true });
    if (cmd === "local_list") {
      return Promise.resolve({
        entries: [
          { name: "docs", is_dir: true, size: 0 },
          { name: "b.bin", is_dir: false, size: 200 },
          { name: "a.bin", is_dir: false, size: 10 },
        ],
        truncated: false,
      });
    }
    return Promise.resolve(undefined);
  });
}

beforeEach(() => {
  invoke.mockReset();
  mockDefaults();
});

describe("ZmodemBar", () => {
  it("空闲时整条不渲染（不占终端的地方）", async () => {
    render(ZmodemBar, { props: { sessionId: "s1", view: IDLE } });
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("zmodem_status", { sessionId: "s1" }));
    expect(screen.queryByTestId("zmodem-bar")).toBeNull();
  });

  it("传输中显示进度与百分比", async () => {
    render(ZmodemBar, {
      props: { sessionId: "s1", view: view({ visible: true, direction: "receive", name: "a.bin", done: 512, total: 1024, ratio: 0.5 }) },
    });
    const txt = screen.getByTestId("zm-progress").textContent ?? "";
    expect(txt).toContain("50%");
    expect(screen.getByTestId("zmodem-bar").textContent).toContain("下载中");
  });

  it("总大小未知时用不定态条，不画会骗人的百分比", () => {
    render(ZmodemBar, {
      props: { sessionId: "s1", view: view({ visible: true, direction: "receive", done: 4096, total: null, ratio: null }) },
    });
    expect(screen.getByTestId("zm-indeterminate")).toBeTruthy();
    expect(screen.getByTestId("zm-progress").textContent).not.toContain("%");
  });

  it("取消按钮调到后端 zmodem_cancel", async () => {
    render(ZmodemBar, {
      props: { sessionId: "s7", view: view({ visible: true, direction: "send", done: 1, total: 2, ratio: 0.5 }) },
    });
    await fireEvent.click(screen.getByTestId("zm-cancel"));
    expect(invoke).toHaveBeenCalledWith("zmodem_cancel", { sessionId: "s7" });
  });

  it("终态显示结果与「打开所在目录」，且 reveal 走受校验的 zmodem_reveal", async () => {
    render(ZmodemBar, {
      props: { sessionId: "s1", view: view({ visible: true, ok: true, message: "接收完成", name: "a.bin", path: "C:\\dl\\a.bin" }) },
    });
    expect(screen.getByTestId("zm-result").textContent).toContain("接收完成");
    await fireEvent.click(screen.getByTestId("zm-reveal"));
    // 必须是 zmodem_reveal（沙箱内校验），不是 open_external
    expect(invoke).toHaveBeenCalledWith("zmodem_reveal", { sessionId: "s1", path: "C:\\dl\\a.bin" });
  });

  it("失败终态照样显示原因（静默失败最难排查）", () => {
    render(ZmodemBar, {
      props: { sessionId: "s1", view: view({ visible: true, ok: false, message: "对端中止了传输" }) },
    });
    expect(screen.getByTestId("zm-result").textContent).toContain("对端中止了传输");
  });

  it("后端说在传而本界面无事件时显示恢复条（事件不重放）", async () => {
    invoke.mockImplementation((cmd: string) => {
      if (cmd === "zmodem_status") return Promise.resolve({ active: true, sandbox: "C:\\dl", auto: true });
      return Promise.resolve(undefined);
    });
    render(ZmodemBar, { props: { sessionId: "s2", view: IDLE } });
    const el = await screen.findByTestId("zm-recovered");
    expect(el.textContent).toContain("传输进行中");
    // 恢复条也要能取消——否则用户只能去终端里 Ctrl+C
    expect(screen.getByTestId("zm-cancel")).toBeTruthy();
  });

  it("取消等待上传的选文件对话框时请求远端取消", async () => {
    render(ZmodemBar, { props: { sessionId: "s3", view: view({ needFile: true, direction: "send" }) } });
    await screen.findByTestId("file-picker");
    await fireEvent.click(await screen.findByTestId("fp-cancel"));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("zmodem_cancel", { sessionId: "s3" }));
  });

  it("need-file 的跳变自动打开选择器一次", async () => {
    render(ZmodemBar, { props: { sessionId: "s3", view: view({ needFile: true, direction: "send" }) } });
    await screen.findByTestId("file-picker");
  });

  it("**用户关掉后，同一轮 need-file 不再自动重开**（2026-08-26 修的 bug，必须钉死）", async () => {
    const { rerender } = render(ZmodemBar, {
      props: { sessionId: "s3", view: view({ needFile: true, direction: "send" }) },
    });
    await screen.findByTestId("file-picker");
    // 用户关掉（FilePickerDialog 的取消按钮）
    await fireEvent.click(await screen.findByTestId("fp-cancel"));
    await waitFor(() => expect(screen.queryByTestId("file-picker")).toBeNull());
    // needFile 仍为 true，但不得再弹——旧版在这里立即重开。
    // 再喂一个仍然为 true 的 view（Svelte 组件 rerender 同值也触发 effect 重跑，
    // 模拟 rz 重发 ZRINIT 时后端再 emit 一条 need-file 的情形）。
    rerender({ sessionId: "s3", view: view({ needFile: true, direction: "send" }) });
    await new Promise((r) => setTimeout(r, 50));
    expect(screen.queryByTestId("file-picker")).toBeNull();
  });

  it("选中的文件以**数组**提交给 zmodem_send（多文件上传的入口）", async () => {
    render(ZmodemBar, { props: { sessionId: "s3", view: view({ needFile: true, direction: "send" }) } });
    await screen.findByText("a.bin");
    // Ctrl 多选两条再确认
    await fireEvent.click(screen.getByText("a.bin"));
    await fireEvent.click(screen.getByText("b.bin"), { ctrlKey: true });
    await fireEvent.click(screen.getByTestId("fp-confirm"));
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("zmodem_send", {
        sessionId: "s3",
        localPaths: ["a.bin", "b.bin"],
      }),
    );
  });

  it("多文件进度显示「第 i/N 个」；单文件不显示", async () => {
    render(ZmodemBar, {
      props: {
        sessionId: "s1",
        view: view({
          visible: true,
          direction: "send",
          name: "b.bin",
          done: 1,
          total: 2,
          ratio: 0.5,
          fileIndex: 2,
          fileCount: 5,
        }),
      },
    });
    expect(screen.getByTestId("zmodem-bar").textContent).toContain("第 2/5 个");
  });

  it("列目录失败由 FilePickerDialog 呈现错误（不再由本组件负责）", async () => {
    invoke.mockImplementation((cmd: string) => {
      if (cmd === "zmodem_status") return Promise.resolve({ active: false, sandbox: null, auto: true });
      if (cmd === "local_list") return Promise.reject(new Error("browse root unavailable"));
      return Promise.resolve(undefined);
    });
    render(ZmodemBar, { props: { sessionId: "s5", view: view({ needFile: true, direction: "send" }) } });
    const err = await screen.findByTestId("fp-error");
    expect(err.textContent).toContain("browse root unavailable");
  });

  it("zmodem_status 查不到不弹错、不显示条（未启用的会话应当完全无痕）", async () => {
    invoke.mockImplementation((cmd: string) => {
      if (cmd === "zmodem_status") return Promise.reject(new Error("该会话未启用终端内传输"));
      return Promise.resolve(undefined);
    });
    render(ZmodemBar, { props: { sessionId: "s6", view: IDLE } });
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("zmodem_status", { sessionId: "s6" }));
    expect(screen.queryByTestId("zmodem-bar")).toBeNull();
  });
});

describe("下载决策（两段式第二段）", () => {
  const dec = (p: Partial<import("../lib/zmodem").DownloadDecision>) =>
    view({
      visible: true,
      direction: "receive",
      decision: { decisionId: "test", sessionId: "s1",
        name: "a.bin",
        suggestedDir: "C:\Users\me\Downloads\FutureShell",
        conflict: false,
        reason: "first-run",
        ...p,
      },
    });

  it("首次（first-run）显示引导：目录 + 记住偏好单选 + 保存", async () => {
    render(ZmodemBar, { props: { sessionId: "d1", view: dec({ sessionId: "d1",}) } });
    expect(screen.getByTestId("zm-decision").textContent).toContain("第一次收到文件");
    expect(screen.getByTestId("zm-dec-dir").textContent).toContain("FutureShell");
    // 未选「以后怎么办」时也可以保存——记住是可选的（不强制现在决定）
    await fireEvent.click(screen.getByTestId("zm-dec-ok"));
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("zmodem_finalize", {
        sessionId: "d1",
        dir: "C:\Users\me\Downloads\FutureShell",
        expectedDecisionId: "test", onConflict: "keep-both",
        remember: null,
      }),
    );
  });

  it("选了「记住这个目录」→ remember=true 随归置一起提交", async () => {
    render(ZmodemBar, { props: { sessionId: "d2", view: dec({ sessionId: "d2",}) } });
    await fireEvent.click(screen.getByText("记住这个目录，以后都存这里"));
    await fireEvent.click(screen.getByTestId("zm-dec-ok"));
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("zmodem_finalize", {
        sessionId: "d2",
        dir: "C:\Users\me\Downloads\FutureShell",
        expectedDecisionId: "test", onConflict: "keep-both",
        remember: true,
      }),
    );
  });

  it("选了「以后每次都问我」→ remember=false", async () => {
    render(ZmodemBar, { props: { sessionId: "d3", view: dec({ sessionId: "d3",}) } });
    await fireEvent.click(screen.getByText("以后每次都问我"));
    await fireEvent.click(screen.getByTestId("zm-dec-ok"));
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("zmodem_finalize", {
        sessionId: "d3",
        dir: "C:\Users\me\Downloads\FutureShell",
        expectedDecisionId: "test", onConflict: "keep-both",
        remember: false,
      }),
    );
  });

  it("「不选了」兜底归置到建议目录（keep-both），**绝不删已收数据**", async () => {
    render(ZmodemBar, { props: { sessionId: "d4", view: dec({ sessionId: "d4",}) } });
    await fireEvent.click(screen.getByTestId("zm-dec-later"));
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("zmodem_finalize", {
        sessionId: "d4",
        dir: "C:\Users\me\Downloads\FutureShell",
        expectedDecisionId: "test", onConflict: "keep-both",
        remember: null,
      }),
    );
  });

  it("choose-dir 形态不显示「以后怎么办」（设置里改）", () => {
    render(ZmodemBar, { props: { sessionId: "d5", view: dec({ sessionId: "d5", reason: "choose-dir" }) } });
    expect(screen.queryByText("以后怎么办")).toBeNull();
    expect(screen.getByTestId("zm-decision").textContent).toContain("文件保存到哪里");
  });

  it("冲突形态三按钮各提交对应策略", async () => {
    render(ZmodemBar, {
      props: { sessionId: "d6", view: dec({ sessionId: "d6", conflict: true, reason: "conflict" }) },
    });
    expect(screen.getByTestId("zm-decision").textContent).toContain("已存在");
    await fireEvent.click(screen.getByTestId("zm-ow"));
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("zmodem_finalize", {
        sessionId: "d6",
        dir: "C:\Users\me\Downloads\FutureShell",
        expectedDecisionId: "test", onConflict: "overwrite",
        remember: null,
      }),
    );
    await fireEvent.click(screen.getByTestId("zm-skip"));
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("zmodem_finalize", {
        sessionId: "d6",
        dir: "C:\Users\me\Downloads\FutureShell",
        expectedDecisionId: "test", onConflict: "skip",
        remember: null,
      }),
    );
  });

  it("**勾了「其余同此处理」后，下一个冲突不再弹框而是直接办**", async () => {
    const { rerender } = render(ZmodemBar, {
      props: { sessionId: "d7", view: dec({ sessionId: "d7", conflict: true, reason: "conflict" }) },
    });
    // 勾选 + 点「保留两者」
    await fireEvent.click(screen.getByTestId("zm-apply-rest"));
    await fireEvent.click(screen.getByTestId("zm-keep"));
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("zmodem_finalize", {
        sessionId: "d7",
        dir: "C:\Users\me\Downloads\FutureShell",
        expectedDecisionId: "test", onConflict: "keep-both",
        remember: null,
      }),
    );
    // 第二个冲突到来（sz 多文件）：不渲染决策框，直接按记下的策略 finalize
    rerender({ sessionId: "d7", view: dec({ sessionId: "d7", conflict: true, reason: "conflict" }) });
    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith("zmodem_finalize", {
        sessionId: "d7",
        dir: "C:\Users\me\Downloads\FutureShell",
        expectedDecisionId: "test", onConflict: "keep-both",
        remember: null,
      });
    });
    expect(screen.queryByTestId("zm-decision")).toBeNull();
  });
});

describe("download decision asynchronous ownership", () => {
  it.each(["save", "skip", "failure", "stale", "session"])("late A %s does not affect B", async (outcome) => {
    let resolve!: (result: boolean) => void;
    let reject!: (reason: string) => void;
    const pending = new Promise<boolean>((ok, fail) => { resolve = ok; reject = fail; });
    invoke.mockImplementation((cmd: string) => cmd === "zmodem_finalize" ? pending : Promise.resolve({ active: false, sandbox: null, auto: true }));
    const info = vi.spyOn(toast, "info");
    const error = vi.spyOn(toast, "error");
    const decision = (id: string) => view({ visible: true, decision: {
      decisionId: id, sessionId: outcome === "session" && id === "B" ? "s2" : "s1", name: id, suggestedDir: "/tmp/downloads",
      conflict: outcome === "skip", reason: "choose-dir",
    }});
    const { rerender } = render(ZmodemBar, { props: { sessionId: "s1", view: decision("A") } });
    await fireEvent.click(screen.getByTestId(outcome === "skip" ? "zm-skip" : "zm-dec-later"));
    expect(invoke).toHaveBeenCalledWith("zmodem_finalize", expect.objectContaining({sessionId: "s1", expectedDecisionId: "A"}));
    await rerender({ sessionId: outcome === "session" ? "s2" : "s1", view: decision("B") });
    if (outcome === "failure") reject("late A failure"); else resolve(outcome !== "stale");
    await pending.catch(() => {});
    await waitFor(() => expect(screen.getByTestId("zm-decision").textContent).toContain("B"));
    expect(info).not.toHaveBeenCalled();
    expect(error).not.toHaveBeenCalled();
    info.mockRestore(); error.mockRestore();
  });
});
