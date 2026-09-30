/**
 * TunnelDialog 组件测试（M4a 隧道管理器 UI）。
 *
 * 后端判据（字节双向透传、只绑回环、端口冲突立即报错）在
 * crates/sshengine/src/tunnel.rs 的 S323–S325。这里只钉 UI 独有的三件：
 * 无会话时不给新建、参数明显填错时不发 IPC、勾选「允许外部访问」必须显式警告。
 */
import { describe, expect, it, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/svelte";

// vi.mock 的工厂被提升到文件顶部，不能引用普通顶层变量（会撞
// "Cannot access before initialization"）；vi.hoisted 让替身与工厂一起提升。
const mocks = vi.hoisted(() => ({
  invoke: vi.fn(),
  toast: { info: vi.fn(), warn: vi.fn(), error: vi.fn(), success: vi.fn() },
}));
const invoke = mocks.invoke;
const toast = mocks.toast;

vi.mock("../lib/ipc", () => ({ invoke: mocks.invoke }));
vi.mock("../lib/toast", () => ({ toast: mocks.toast }));

import TunnelDialog from "./TunnelDialog.svelte";

describe("TunnelDialog（隧道管理器）", () => {
  beforeEach(() => {
    invoke.mockReset();
    invoke.mockResolvedValue([]);
    toast.warn.mockReset();
    toast.error.mockReset();
    toast.info.mockReset();
  });

  it("无活动会话时说明原因且不给新建表单（隧道是会话级设施）", async () => {
    render(TunnelDialog, { props: { open: true, sessionId: null, onClose: vi.fn() } });
    expect(screen.getByTestId("tunnel-nosession")).toBeTruthy();
    expect(screen.queryByTestId("tn-start")).toBeNull();
  });

  it("列表渲染后端返回的隧道（本地/远端/已转发计数）", async () => {
    invoke.mockResolvedValue([
      {
        id: "L18080-x",
        local_port: 18080,
        local_addr: "127.0.0.1:18080",
        remote_host: "db.internal",
        remote_port: 5432,
        bind_all: false,
        accepted: 3,
      },
    ]);
    render(TunnelDialog, { props: { open: true, sessionId: "s1", onClose: vi.fn() } });
    const row = await screen.findByTestId("tn-row");
    expect(row.textContent).toContain("127.0.0.1:18080");
    expect(row.textContent).toContain("db.internal:5432");
    expect(row.textContent).toContain("3");
  });

  it("端口填错不发 IPC（前端只挡明显错误，权威校验在 Rust）", async () => {
    render(TunnelDialog, { props: { open: true, sessionId: "s1", onClose: vi.fn() } });
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("tunnel_list", { sessionId: "s1" }));
    invoke.mockClear();
    await fireEvent.input(screen.getByTestId("tn-local"), { target: { value: "0" } });
    await fireEvent.input(screen.getByTestId("tn-host"), { target: { value: "h" } });
    await fireEvent.input(screen.getByTestId("tn-remote"), { target: { value: "22" } });
    await fireEvent.click(screen.getByTestId("tn-start"));
    expect(invoke).not.toHaveBeenCalled();
    expect(toast.warn).toHaveBeenCalled();
  });

  it("参数合法时以 spec 发 tunnel_start（字段名与 Rust TunnelSpec 逐字对应）", async () => {
    render(TunnelDialog, { props: { open: true, sessionId: "s1", onClose: vi.fn() } });
    await waitFor(() => expect(invoke).toHaveBeenCalled());
    invoke.mockClear();
    invoke.mockResolvedValue([]);
    await fireEvent.input(screen.getByTestId("tn-local"), { target: { value: "18080" } });
    await fireEvent.input(screen.getByTestId("tn-host"), { target: { value: "db.internal" } });
    await fireEvent.input(screen.getByTestId("tn-remote"), { target: { value: "5432" } });
    await fireEvent.click(screen.getByTestId("tn-start"));
    await waitFor(() => expect(invoke.mock.calls.some((c) => c[0] === "tunnel_start")).toBe(true));
    const call = invoke.mock.calls.find((c) => c[0] === "tunnel_start")!;
    expect(call[1].sessionId).toBe("s1");
    expect(call[1].spec).toMatchObject({
      local_port: 18080,
      remote_host: "db.internal",
      remote_port: 5432,
      bind_all: false,
    });
  });

  it("勾选「允许外部访问」必须显式警告后果（同网段任何人可用这条隧道）", async () => {
    render(TunnelDialog, { props: { open: true, sessionId: "s1", onClose: vi.fn() } });
    expect(screen.queryByTestId("tn-bindall-warn")).toBeNull();
    await fireEvent.click(screen.getByTestId("tn-bindall"));
    const warn = await screen.findByTestId("tn-bindall-warn");
    expect(warn.textContent).toContain("同网段");
  });

  it("停止按钮调 tunnel_stop 并带 tunnelId", async () => {
    invoke.mockResolvedValue([
      { id: "L1-x", local_port: 1, local_addr: "127.0.0.1:1", remote_host: "h", remote_port: 2, bind_all: false, accepted: 0 },
    ]);
    render(TunnelDialog, { props: { open: true, sessionId: "s1", onClose: vi.fn() } });
    await fireEvent.click(await screen.findByTestId("tn-stop"));
    await waitFor(() =>
      expect(invoke.mock.calls.some((c) => c[0] === "tunnel_stop" && c[1].tunnelId === "L1-x")).toBe(true),
    );
  });
});
