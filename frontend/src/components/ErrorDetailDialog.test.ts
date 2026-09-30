import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/svelte";
import ErrorDetailDialog from "./ErrorDetailDialog.svelte";

vi.mock("../lib/toast", () => ({ toast: { info: vi.fn(), error: vi.fn() } }));

/** 长错误的完整呈现（2026-09-01 用户报「报错文本很长，无法看全」）。
 *
 * 那段文本此前三个落点都读不全：状态栏 24px + nowrap 被裁、toast 8 秒后消失、
 * 标签 title 同样截断且无法复制。而它恰恰是用户唯一能拿去搜索/求助的东西。 */
const LONG = 'auth failed; tried: []; server allows: ["publickey"]; notes: ["本连接未配置任何可用的认证凭据，而这台服务器只接受密钥认证（publickey）——存口令没有用：请在「会话属性 → 认证」页勾选「使用本地 SSH Agent 认证」（若你已用 ssh-add 加载过密钥），或选择一条「私钥」类别的凭据记录后重试"]';

describe("ErrorDetailDialog：长错误看得全、拿得走", () => {
  it("完整文本一字不改地呈现（不做人话化改写——原文里的 server allows 是决定性信息）", () => {
    render(ErrorDetailDialog, { props: { open: true, text: LONG, onClose: () => {} } });
    expect(screen.getByTestId("error-detail-text").textContent).toBe(LONG);
  });

  it("文本可换行、可选中——这两条决定了它能不能被读完与被复制", () => {
    render(ErrorDetailDialog, { props: { open: true, text: LONG, onClose: () => {} } });
    const el = screen.getByTestId("error-detail-text");
    // jsdom 不做布局，但类名与内联样式契约仍可断言：pre 元素 + 组件样式类
    expect(el.tagName).toBe("PRE");
    expect(el.className).toContain("body");
  });

  it("有一键复制（求助时要整段贴出去）", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.assign(navigator, { clipboard: { writeText } });
    render(ErrorDetailDialog, { props: { open: true, text: LONG, onClose: () => {} } });
    await fireEvent.click(screen.getByTestId("error-detail-copy"));
    await waitFor(() => expect(writeText).toHaveBeenCalledWith(LONG));
  });

  it("Esc 与关闭按钮都能退出", async () => {
    const onClose = vi.fn();
    render(ErrorDetailDialog, { props: { open: true, text: LONG, onClose } });
    await fireEvent.keyDown(screen.getByTestId("error-detail-dialog"), { key: "Escape" });
    expect(onClose).toHaveBeenCalled();
    await fireEvent.click(screen.getByTestId("error-detail-close"));
    expect(onClose).toHaveBeenCalledTimes(2);
  });

  it("open=false 时不渲染（不留一个看不见的模态吃掉点击）", () => {
    render(ErrorDetailDialog, { props: { open: false, text: LONG, onClose: () => {} } });
    expect(screen.queryByTestId("error-detail-dialog")).toBeNull();
  });
});
