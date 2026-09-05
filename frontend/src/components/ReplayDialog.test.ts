/**
 * 会话回放面板测试（M4a）。xterm 以 mock 注入（jsdom 无真实 canvas），
 * 钉的是面板自己的行为：列表、隐私确认、播放调度（事件顺序与延迟）、
 * 拖动 = 从头快喂。
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/svelte";
import ReplayDialog from "./ReplayDialog.svelte";
import type { CastFile } from "../lib/record";

// Terminal mock：记录 write/reset/resize 的调用，供断言
const written: string[] = [];
const resizes: [number, number][] = [];
const TerminalMock = vi.hoisted(() => {
  return class {
    static lastInstance: any = null;
    wrote: string[] = written;
    constructor(_opts: unknown) {
      (TerminalMock as any).lastInstance = this;
    }
    open() {}
    write(d: string) { written.push(d); }
    reset() { written.length = 0; }
    resize(c: number, r: number) { resizes.push([c, r]); }
    dispose() {}
  };
});
vi.mock("@xterm/xterm", () => ({ Terminal: TerminalMock }));
vi.mock("@xterm/xterm/css/xterm.css", () => ({}));

const invokeMock = vi.hoisted(() =>
  vi.fn(async (_cmd: string, _args?: Record<string, unknown>): Promise<unknown> => undefined),
);
vi.mock("../lib/ipc", () => ({ invoke: invokeMock }));
const toastMock = vi.hoisted(() => ({
  info: vi.fn(), warn: vi.fn(), error: vi.fn(), push: vi.fn(), dismiss: vi.fn(),
  subscribe: vi.fn(() => () => {}),
}));
vi.mock("../lib/toast", () => ({ toast: toastMock }));

const FILES: CastFile[] = [
  { name: "web-20260821-030000.cast", path: "/data/recordings/web-x.cast", bytes: 4096, modified: 1_787_302_940 },
  { name: "tiny.cast", path: "/data/recordings/tiny.cast", bytes: 10, modified: 1_787_300_000 },
];

const CONTENT = {
  header: { version: 2, width: 80, height: 24, timestamp: 1_787_302_940 },
  events: [
    { t: 0, kind: "o", data: "hello\r\n" },
    { t: 1, kind: "o", data: "world\r\n" },
    { t: 2, kind: "r", data: "100x30" },
    { t: 2.5, kind: "o", data: "resized" },
  ],
};

beforeEach(() => {
  cleanup();
  vi.clearAllMocks();
  written.length = 0;
  resizes.length = 0;
  invokeMock.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
    if (cmd === "recordings_list") return FILES;
    if (cmd === "recording_read_events") {
      const p = String(args?.path ?? "");
      if (p.includes("tiny")) return { header: CONTENT.header, events: [] };
      return CONTENT;
    }
    return undefined;
  });
});

describe("ReplayDialog", () => {
  it("列出录屏文件；空目录给出引导文案", async () => {
    invokeMock.mockImplementation(async (cmd: string) =>
      cmd === "recordings_list" ? [] : undefined);
    render(ReplayDialog, { props: { open: true, onClose: vi.fn() } });
    await screen.findByTestId("replay-list");
    expect(screen.getByText(/还没有录屏文件/)).toBeTruthy();
  });

  it("大于 1KiB 的文件先弹隐私确认，小文件直接打开", async () => {
    render(ReplayDialog, { props: { open: true, onClose: vi.fn() } });
    const picks = await screen.findAllByTestId("replay-pick");
    // 小文件直接开
    await fireEvent.click(picks[1]);
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith(
      "recording_read_events", { path: FILES[1].path }));
    // 大文件先确认
    await fireEvent.click(picks[0]);
    await screen.findByTestId("confirm-dialog");
    expect(screen.getByTestId("confirm-msg").textContent).toContain("明文");
    await fireEvent.click(screen.getByTestId("confirm-ok"));
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith(
      "recording_read_events", { path: FILES[0].path }));
    await screen.findByTestId("replay-term");
  });

  it("播放按延迟顺序写入事件（o 写入、r 触发 resize）", async () => {
    vi.useFakeTimers();
    try {
      render(ReplayDialog, { props: { open: true, onClose: vi.fn() } });
      const picks = await screen.findAllByTestId("replay-pick");
      await fireEvent.click(picks[0]); // 大文件 → 确认
      await screen.findByTestId("confirm-dialog");
      await fireEvent.click(screen.getByTestId("confirm-ok"));
      await screen.findByTestId("replay-term");
      // 未按播放前不得有输出
      await waitFor(() => expect(written.length).toBe(0));

      await fireEvent.click(screen.getByTestId("replay-play"));
      // t=0 立即写入
      await vi.advanceTimersByTimeAsync(0);
      expect(written.join("")).toContain("hello");
      // t=1 第二条（默认 1× 速）
      await vi.advanceTimersByTimeAsync(1000);
      expect(written.join("")).toContain("world");
      expect(written.join("")).not.toContain("resized");
      // t=2.5 最后一条
      await vi.advanceTimersByTimeAsync(1500);
      expect(written.join("")).toContain("resized");
      // r 事件须触发 resize（尺寸在回放里还原，否则宽高停在头的初始值）
      expect(resizes, "r=100x30 事件须 resize(100,30)").toContainEqual([100, 30]);
      // 播完按钮变「重放」
      expect(screen.getByTestId("replay-play").textContent).toContain("重放");
    } finally {
      vi.useRealTimers();
    }
  });

  it("拖动进度 = 终端重置后从头无延迟重喂到该处", async () => {
    vi.useFakeTimers();
    try {
      render(ReplayDialog, { props: { open: true, onClose: vi.fn() } });
      const picks = await screen.findAllByTestId("replay-pick");
      await fireEvent.click(picks[0]);
      await screen.findByTestId("confirm-dialog");
      await fireEvent.click(screen.getByTestId("confirm-ok"));
      await screen.findByTestId("replay-term");

      // 先播放一段：屏幕上已有 hello
      await fireEvent.click(screen.getByTestId("replay-play"));
      await vi.advanceTimersByTimeAsync(0);
      expect(written.join("")).toContain("hello");

      // 拖到 2 秒：reset 后从头喂到 2s——hello 只出现**一次**（不 reset 会叠成两份，
      // 且旧 ANSI 状态残留会让回退后的画面与真实历史不一致）
      const seek = screen.getByTestId("replay-seek") as HTMLInputElement;
      await fireEvent.input(seek, { target: { value: "2" } });
      const joined = written.join("");
      expect(joined.match(/hello/g)?.length, "重喂前必须 reset，hello 只得一份").toBe(1);
      expect(joined).toContain("world");
      // 2.5s 的事件在 2s 处还没发生
      expect(joined).not.toContain("resized");
    } finally {
      vi.useRealTimers();
    }
  });

  it("读取失败原样报错（坏文件说坏在哪）", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "recordings_list") return FILES;
      if (cmd === "recording_read_events") throw new Error("第 3 个事件解析失败：expected value");
      return undefined;
    });
    render(ReplayDialog, { props: { open: true, onClose: vi.fn() } });
    const picks = await screen.findAllByTestId("replay-pick");
    await fireEvent.click(picks[1]);
    await waitFor(() => expect(toastMock.error).toHaveBeenCalled());
    expect(String(toastMock.error.mock.calls[0][0])).toContain("第 3 个事件");
  });
});
