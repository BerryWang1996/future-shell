/**
 * QuickBar / QuickCommandsDialog 组件测试（M4a 出口标准「UI 增删改查测试」）。
 * 纯函数判据（占位符替换等）在 lib/quick-commands.test.ts（S309）。
 */
import { describe, expect, it, vi } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/svelte";
import QuickBar from "./QuickBar.svelte";
import QuickCommandsDialog from "./QuickCommandsDialog.svelte";

const items = [
  { id: "q1", name: "磁盘", command: "df -h", category: "诊断" },
  { id: "q2", name: "内存", command: "free -m", category: "诊断" },
  { id: "q3", name: "重启 nginx", command: "systemctl restart nginx" },
];

describe("QuickBar（按钮条）", () => {
  it("按分类分段渲染片段，点击无占位符的片段即发送命令原文", async () => {
    const onSend = vi.fn();
    render(QuickBar, { props: { items, onSend } });
    const bar = screen.getByTestId("quickbar");
    expect(bar.textContent).toContain("诊断");
    expect(bar.querySelectorAll(".qbtn").length).toBe(3);
    await fireEvent.click(screen.getByText("磁盘"));
    expect(onSend).toHaveBeenCalledWith("df -h");
  });

  it("带占位符的片段：先弹参数行，空参数不发（不静默发字面量），填全才发替换结果", async () => {
    const onSend = vi.fn();
    render(QuickBar, {
      props: { items: [{ id: "q1", name: "重启服务", command: "systemctl restart {{服务}}" }], onSend },
    });
    await fireEvent.click(screen.getByText("重启服务"));
    const params = await screen.findByTestId("quick-params");
    expect(params).toBeTruthy();
    expect(onSend).not.toHaveBeenCalled();
    // 空参数点发送：拦下（S309 的 UI 侧——静默发 {{服务}} 字面量比不发送危险）
    await fireEvent.click(screen.getByText("发送 ⏎"));
    expect(onSend).not.toHaveBeenCalled();
    // 填参后发送：替换完成
    const input = params.querySelector<HTMLInputElement>('[data-param="服务"]');
    expect(input).toBeTruthy();
    await fireEvent.input(input!, { target: { value: "nginx" } });
    await fireEvent.click(screen.getByText("发送 ⏎"));
    expect(onSend).toHaveBeenCalledWith("systemctl restart nginx");
  });

  it("参数行 Esc 取消、Enter 发送", async () => {
    const onSend = vi.fn();
    render(QuickBar, {
      props: { items: [{ id: "q1", name: "看日志", command: "tail -n {{行数}} /var/log/syslog" }], onSend },
    });
    await fireEvent.click(screen.getByText("看日志"));
    const input = screen.getByTestId("quick-params").querySelector<HTMLInputElement>('[data-param="行数"]')!;
    await fireEvent.input(input, { target: { value: "50" } });
    await fireEvent.keyDown(input, { key: "Enter" });
    expect(onSend).toHaveBeenCalledWith("tail -n 50 /var/log/syslog");
    // 再开一次，Esc 收起不发送
    await fireEvent.click(screen.getByText("看日志"));
    const input2 = screen.getByTestId("quick-params").querySelector<HTMLInputElement>('[data-param="行数"]')!;
    await fireEvent.input(input2, { target: { value: "10" } });
    await fireEvent.keyDown(input2, { key: "Escape" });
    expect(screen.queryByTestId("quick-params")).toBeNull();
    expect(onSend).toHaveBeenCalledTimes(1);
  });
});

describe("QuickCommandsDialog（片段库增删改查）", () => {
  it("新增：填名称/命令/分类后保存条目，确定回写全量列表（category 空串归一为无分类）", async () => {
    const onSave = vi.fn();
    render(QuickCommandsDialog, { props: { open: true, items: [], onSave, onClose: vi.fn() } });
    await fireEvent.click(screen.getByTestId("qc-add"));
    await fireEvent.input(screen.getByTestId("qc-name"), { target: { value: "磁盘" } });
    await fireEvent.input(screen.getByTestId("qc-category"), { target: { value: "诊断" } });
    await fireEvent.input(screen.getByTestId("qc-command"), { target: { value: "df -h" } });
    await fireEvent.click(screen.getByTestId("qc-save"));
    expect(screen.getByTestId("qc-list").textContent).toContain("磁盘");
    await fireEvent.click(screen.getByTestId("qc-commit"));
    expect(onSave).toHaveBeenCalledWith([
      { id: expect.any(String), name: "磁盘", command: "df -h", category: "诊断" },
    ]);
  });

  it("修改：编辑已有条目后确定，只改目标条目", async () => {
    const onSave = vi.fn();
    render(QuickCommandsDialog, {
      props: { open: true, items: [{ id: "q1", name: "旧名", command: "ls" }, { id: "q2", name: "别动", command: "pwd" }], onSave, onClose: vi.fn() },
    });
    const editBtns = screen.getAllByTestId("qc-edit");
    await fireEvent.click(editBtns[0]);
    await fireEvent.input(screen.getByTestId("qc-name"), { target: { value: "新名" } });
    await fireEvent.click(screen.getByTestId("qc-save"));
    await fireEvent.click(screen.getByTestId("qc-commit"));
    const saved = onSave.mock.calls[0][0] as Array<{ id: string; name: string; command: string }>;
    expect(saved).toHaveLength(2);
    expect(saved.find((x) => x.id === "q1")?.name).toBe("新名");
    expect(saved.find((x) => x.id === "q2")?.name).toBe("别动");
  });

  it("删除：确定后列表不含被删条目", async () => {
    const onSave = vi.fn();
    render(QuickCommandsDialog, {
      props: { open: true, items: [{ id: "q1", name: "a", command: "ls" }, { id: "q2", name: "b", command: "pwd" }], onSave, onClose: vi.fn() },
    });
    const delBtns = screen.getAllByTestId("qc-del");
    await fireEvent.click(delBtns[0]);
    await fireEvent.click(screen.getByTestId("qc-commit"));
    const saved = onSave.mock.calls[0][0] as Array<{ id: string }>;
    expect(saved.map((x) => x.id)).toEqual(["q2"]);
  });

  it("取消：不触发 onSave（快照式提交——关掉不落不脏库）", async () => {
    const onSave = vi.fn();
    render(QuickCommandsDialog, {
      props: { open: true, items: [{ id: "q1", name: "a", command: "ls" }], onSave, onClose: vi.fn() },
    });
    const delBtns = screen.getAllByTestId("qc-del");
    await fireEvent.click(delBtns[0]); // 删了也不落
    await fireEvent.click(screen.getByTestId("qc-close"));
    expect(onSave).not.toHaveBeenCalled();
  });

  it("允许保存空库，便于清除最后一条命令", async () => {
    const onSave = vi.fn();
    render(QuickCommandsDialog, { props: { open: true, items: [], onSave, onClose: vi.fn() } });
    const btn = screen.getByTestId("qc-commit") as HTMLButtonElement;
    expect(btn.disabled).toBe(false);
  });
});
