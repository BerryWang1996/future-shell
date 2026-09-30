import { beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/svelte";
let deliver: ((e: { payload: unknown }) => void) | undefined;
const { invoke } = vi.hoisted(() => ({ invoke: vi.fn().mockResolvedValue(true) }));
vi.mock("../lib/ipc", () => ({ invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen: async (_: string, cb: typeof deliver) => { deliver = cb; return () => {}; } }));
import McpConfirmDialog from "./McpConfirmDialog.svelte";
const request = (id: number, strong = false) => ({ request_id: id, tool: "command.run", caller: "测试客户端", tier: strong ? "dangerous" : "write", strong, display: "测试操作 " + id, details: [], session_id: null });
beforeEach(() => { invoke.mockClear(); deliver = undefined; });
describe("MCP 确认交互", () => {
  it("默认聚焦拒绝，Escape 拒绝请求", async () => {
    render(McpConfirmDialog);
    await waitFor(() => expect(deliver).toBeDefined());
    deliver!({ payload: request(1) });
    const reject = await screen.findByTestId("mcp-reject");
    expect(document.activeElement).toBe(reject);
    await fireEvent.keyDown(reject, { key: "Escape" });
    expect(invoke).toHaveBeenCalledWith("mcp_confirm_answer", { requestId: 1, approved: false });
  });
  it("并发请求逐个呈现，不覆盖上一条；强确认输入不会沿用", async () => {
    render(McpConfirmDialog);
    await waitFor(() => expect(deliver).toBeDefined());
    deliver!({ payload: request(1, true) });
    deliver!({ payload: request(2, true) });
    const input = await screen.findByTestId("mcp-strong-input");
    expect(screen.getByTestId("mcp-display").textContent).toBe("测试操作 1");
    expect((screen.getByTestId("mcp-approve") as HTMLButtonElement).disabled).toBe(true);
    await fireEvent.input(input, { target: { value: "确认" } });
    await fireEvent.click(screen.getByTestId("mcp-approve"));
    await waitFor(() => expect(screen.getByTestId("mcp-display").textContent).toBe("测试操作 2"));
    expect((screen.getByTestId("mcp-strong-input") as HTMLInputElement).value).toBe("");
    expect((screen.getByTestId("mcp-approve") as HTMLButtonElement).disabled).toBe(true);
  });
});
