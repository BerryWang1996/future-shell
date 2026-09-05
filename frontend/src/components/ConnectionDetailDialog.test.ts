/**
 * ConnectionDetailDialog 组件测试（M4a 连接详情弹层；UI 规格 §2.7）。
 *
 * 出口标准要求「三字段与当前会话实际值一致、Esc 或点击外部关闭」。后端字段的
 * 真实性由 Rust 侧保证（observed_facts 记的是 check_server_key 真看到的密钥），
 * 这里钉 UI 侧三件：字段如实呈现、**测不到时显示「—」而不是 0**、关闭路径可用。
 */
import { describe, expect, it, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/svelte";

const mocks = vi.hoisted(() => ({
  invoke: vi.fn(),
  clipboardWrite: vi.fn(async () => {}),
  toast: { info: vi.fn(), warn: vi.fn(), error: vi.fn(), success: vi.fn() },
}));
vi.mock("../lib/ipc", () => ({ invoke: mocks.invoke }));
vi.mock("../lib/term", () => ({ clipboardWrite: mocks.clipboardWrite }));
vi.mock("../lib/toast", () => ({ toast: mocks.toast }));

import ConnectionDetailDialog from "./ConnectionDetailDialog.svelte";

const detail = {
  host: "web-01",
  port: 22,
  username: "root",
  auth_method: "publickey",
  key_type: "ssh-ed25519",
  fingerprint_sha256: "SHA256:abc123def456",
  latency_ms: 42,
};

describe("ConnectionDetailDialog（连接详情）", () => {
  beforeEach(() => {
    mocks.invoke.mockReset();
    mocks.invoke.mockResolvedValue(detail);
    mocks.clipboardWrite.mockClear();
  });

  it("三字段如实呈现（认证方式/指纹/延时）", async () => {
    render(ConnectionDetailDialog, { props: { open: true, sessionId: "s1", onClose: vi.fn() } });
    await waitFor(() => expect(screen.getByTestId("cd-auth").textContent).toContain("publickey"));
    expect(screen.getByTestId("cd-fp").textContent).toContain("SHA256:abc123def456");
    expect(screen.getByTestId("cd-latency").textContent).toContain("42 ms");
  });

  it("延时测不到显示「—」而不是 0（0 ms 会被读成「极快」）", async () => {
    mocks.invoke.mockResolvedValue({ ...detail, latency_ms: null });
    render(ConnectionDetailDialog, { props: { open: true, sessionId: "s1", onClose: vi.fn() } });
    await waitFor(() => {
      const t = screen.getByTestId("cd-latency").textContent ?? "";
      expect(t).toContain("—");
      expect(t).not.toContain("0 ms");
    });
  });

  it("认证方式未知时显示「—」（不编造一个方法名）", async () => {
    mocks.invoke.mockResolvedValue({ ...detail, auth_method: "" });
    render(ConnectionDetailDialog, { props: { open: true, sessionId: "s1", onClose: vi.fn() } });
    await waitFor(() => expect(screen.getByTestId("cd-auth").textContent?.trim()).toBe("—"));
  });

  it("指纹可复制（核对 MITM 的唯一手段，必须能取出来比对）", async () => {
    render(ConnectionDetailDialog, { props: { open: true, sessionId: "s1", onClose: vi.fn() } });
    await fireEvent.click(await screen.findByTestId("cd-copy"));
    await waitFor(() => expect(mocks.clipboardWrite).toHaveBeenCalledWith("SHA256:abc123def456"));
  });

  it("读取失败就地报错，不整屏空白", async () => {
    mocks.invoke.mockRejectedValue(new Error("no session"));
    render(ConnectionDetailDialog, { props: { open: true, sessionId: "s1", onClose: vi.fn() } });
    const err = await screen.findByTestId("cd-err");
    expect(err.textContent).toContain("no session");
  });

  it("Esc 关闭（出口标准原文）", async () => {
    const onClose = vi.fn();
    render(ConnectionDetailDialog, { props: { open: true, sessionId: "s1", onClose } });
    await fireEvent.keyDown(screen.getByTestId("conn-detail"), { key: "Escape" });
    expect(onClose).toHaveBeenCalled();
  });
});
