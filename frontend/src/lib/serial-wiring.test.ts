import { beforeEach, describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";
import APP_SRC from "../App.svelte?raw";
import PROFILE_SRC from "../components/ProfileDialog.svelte?raw";
import TERMPANE_SRC from "../components/TerminalPane.svelte?raw";

const invokeMock = vi.hoisted(() => vi.fn(async (_cmd?: string, _args?: unknown): Promise<unknown> => undefined));
vi.mock("./ipc", () => ({ invoke: invokeMock }));
vi.mock("./toast", () => ({ toast: { info: vi.fn(), error: vi.fn(), warn: vi.fn() } }));

import { openSession, resetPendingSeq } from "./open-session";
import { resetTabStore, tabs } from "./tabs";
import type { Profile } from "./types";

/**
 * 串口会话的接线（M7.4 出口标准③「串口会话与 SSH 会话在标签/状态栏/历史等公共设施上
 * 行为一致」）。
 *
 * 「一致」的实现方式不是「照着做一遍」，而是**走同一条路**：同一个 `openSession`、同一批
 * 标签原语、同一套 `session:*` 事件、同一条 `term_input`。所以这里钉的是「串口没有另起一套」，
 * 而不是「串口那一套也对」。
 */
const SERIAL: Profile = {
  id: "p-serial",
  name: "板子",
  group_path: null,
  host: "",
  port: 22,
  username: "",
  protocol: "serial",
  serial: { port: "COM3", baud: 115200, data_bits: "eight", parity: "none", stop_bits: "one", flow: "none" },
} as Profile;

const SSH: Profile = {
  id: "p-ssh",
  name: "服务器",
  group_path: null,
  host: "10.0.0.1",
  port: 22,
  username: "root",
  protocol: "ssh",
} as Profile;

describe("openSession 对串口的分流", () => {
  beforeEach(() => {
    resetTabStore();
    resetPendingSeq();
    invokeMock.mockReset();
    invokeMock.mockImplementation(async () => "s-real");
  });

  it("串口走 serial_open，SSH 走 session_open（两条打开路径不混）", async () => {
    await openSession(SERIAL);
    expect(invokeMock).toHaveBeenCalledWith("serial_open", { profileId: "p-serial", rows: 24, cols: 80 });
    expect(invokeMock).not.toHaveBeenCalledWith("session_open", expect.anything());
    invokeMock.mockClear();
    await openSession(SSH);
    expect(invokeMock).toHaveBeenCalledWith("session_open", { profileId: "p-ssh" });
  });

  it("标签用端口名而不是 user@host——串口没有用户名，硬套会渲染成「@」开头的空串", async () => {
    await openSession(SERIAL);
    const t = get(tabs)[0];
    expect(t.title).toBe("COM3");
    expect(t.host).toContain("COM3");
    expect(t.host).toContain("115200");
    expect(t.kind).toBe("serial");
  });

  it("串口标签也是 connecting → connected 原地转正（与 SSH 同一条时序）", async () => {
    const p = openSession(SERIAL);
    expect(get(tabs)[0].status).toBe("connecting");
    await p;
    expect(get(tabs)[0].id).toBe("s-real");
    expect(get(tabs)[0].status).toBe("connected");
  });

  it("打开失败照旧落错误标签（不是撤掉标签，那会让失败只剩一条 8 秒的 toast）", async () => {
    invokeMock.mockImplementation(async () => {
      throw new Error("打不开 COM3：没有权限，或端口正被另一个程序占着");
    });
    await expect(openSession(SERIAL)).resolves.toBeNull();
    const t = get(tabs)[0];
    expect(t.status).toBe("error");
    expect(t.errorText).toContain("COM3");
  });
});

describe("串口在公共设施上没有另起一套（源码级）", () => {
  it("终端输入照旧走 term_input——没有 serial_input 这种平行命令", () => {
    expect(APP_SRC).not.toMatch(/invoke\(\s*"serial_input"/);
    expect(TERMPANE_SRC).not.toMatch(/invoke\(\s*"serial_input"/);
  });

  it("串口标签不挂文件视图（SFTP 在串口上恒报 no session）", () => {
    expect(APP_SRC).toContain('hasFiles={tab.kind !== "serial"}');
    expect(TERMPANE_SRC).toMatch(/\{#if hasFiles\}[\s\S]{0,200}文件<\/button>/);
  });

  it("状态栏的主机段在串口会话上显示端口 + 参数摘要，并可点改波特率", () => {
    expect(APP_SRC).toMatch(/host=\{activeSerial \?[\s\S]{0,80}activeSerial\.summary/);
    expect(APP_SRC).toMatch(/onHostClick=\{activeSerial &&[\s\S]{0,80}serialBaudOpen = true/);
  });

  it("会话属性里串口档案不显示认证/跳板/主机密钥/SFTP 页签", () => {
    expect(PROFILE_SRC).toMatch(/SERIAL_HIDDEN[^\n]*=[^\n]*"auth"[^\n]*"jump"[^\n]*"hostkey"[^\n]*"sftp"/);
    expect(PROFILE_SRC).toContain("{#each visibleTabs as t (t.id)}");
  });

  it("串口端口是可输入的（枚举列表是便利不是白名单——容器里的伪终端不会被枚举到）", () => {
    expect(PROFILE_SRC).toMatch(/<input list="pf-serial-ports"/);
  });
});
