import { describe, expect, it, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/svelte";
import SessionRecoveryDialog from "./SessionRecoveryDialog.svelte";
describe("恢复会话的识别与取消", () => {
  it("显示会话名称和目标，内部编号不充当名称", () => {
    render(SessionRecoveryDialog, { open: true, rows: [{ session_key: "internal-uuid", profile_id: "p1", updated_at: "2026-09-05 03:00:00" }], profiles: [{ id: "p1", name: "生产服务器", host: "prod.example", port: 22 }], onReconnect: vi.fn(), onSkipAll: vi.fn() });
    expect(screen.getByText("生产服务器")).toBeTruthy();
    expect(screen.getByText("prod.example:22")).toBeTruthy();
    expect(screen.queryByText("internal-uuid")).toBeNull();
  });
  it("对话框内按 Escape 可以全部跳过", async () => {
    const onSkipAll = vi.fn();
    render(SessionRecoveryDialog, { open: true, rows: [], onReconnect: vi.fn(), onSkipAll });
    await fireEvent.keyDown(screen.getByRole("button", { name: "全部跳过" }), { key: "Escape" });
    expect(onSkipAll).toHaveBeenCalledOnce();
  });
});
