import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/svelte";
import StatusBar from "./StatusBar.svelte";
import APP_SOURCE from "../App.svelte?raw";

/**
 * 状态栏（M1 出口两条的呈现面，此前**整个组件零测试**）：
 *
 * - 「键盘模式三平台可达：状态栏键盘模式段点击（**三平台主入口**）与 Scroll Lock 行为等价；
 *   macOS 无 Scroll Lock 键盘（含 Magic Keyboard）下切换路径经点击可达」——
 *   这条出口标准的全部意义就在于「不靠 Scroll Lock 也能切」。而 macOS 上没有那颗键，
 *   点击这条路径断了就等于该平台功能缺失，且**在 Windows 上永远测不出来**。
 * - 「连接态三处一致（UI 规格 §5）」的第三处（状态栏文本/状态灯）。前两处
 *   （标签、侧栏）分别由 TabBar.test.ts 与 host-lights.test.ts 覆盖，
 *   状态栏这一处此前没有载体。
 *
 * 键盘模式的**语义**（Scroll Lock 走 toggle-mode 分支、per-session 存储）在
 * lib/keymap.test.ts 与 lib/tabs.test.ts；这里钉的是「另一条入口真的存在且接得上」。
 */
describe("StatusBar（连接态第三处 + 键盘模式点击入口）", () => {
  const props = (over: Record<string, unknown> = {}) => ({
    status: "connected" as const,
    host: "web-01:22",
    vaultLocked: true,
    keyboardMode: "remote" as const,
    ...over,
  });

  it("键盘模式段是可点按钮，点击回投 onKeyboardModeClick（macOS 无 Scroll Lock 的唯一出路）", async () => {
    const onKeyboardModeClick = vi.fn();
    render(StatusBar, { props: props({ onKeyboardModeClick }) });
    const seg = screen.getByTitle("切换键盘模式（本地/远程）");
    expect(seg.tagName, "必须是 button——span 上挂 onclick 键盘用户够不着").toBe("BUTTON");
    await fireEvent.click(seg);
    expect(onKeyboardModeClick).toHaveBeenCalledTimes(1);
  });

  it("键盘模式段显示当前模式（两态文案不同，否则点了看不出有没有生效）", () => {
    const { unmount } = render(StatusBar, { props: props({ keyboardMode: "remote" }) });
    expect(screen.getByTitle("切换键盘模式（本地/远程）").textContent).toContain("远程");
    unmount();
    render(StatusBar, { props: props({ keyboardMode: "local" }) });
    expect(screen.getByTitle("切换键盘模式（本地/远程）").textContent).toContain("本地");
  });

  it("四态各有不同的状态文本（UI 规格 §5 第三处）", () => {
    const want: Array<[string, string]> = [
      ["connecting", "连接中…"],
      ["connected", "已连接"],
      ["disconnected", "已断开"],
      ["error", "错误"],
    ];
    for (const [status, text] of want) {
      const { unmount } = render(StatusBar, { props: props({ status }) });
      expect(screen.getByText(new RegExp(text)), `${status} 应显示「${text}」`).toBeTruthy();
      unmount();
    }
  });

  it("无会话显示「无会话」而不是空白或上一次的状态", () => {
    render(StatusBar, { props: props({ status: null }) });
    expect(screen.getByText(/无会话/)).toBeTruthy();
  });

  it("error 态带 errorText 时显示错因摘要（§5 要求的「错误摘要」，不是干巴巴一个「错误」）", () => {
    render(StatusBar, { props: props({ status: "error", errorText: "主机密钥不匹配" }) });
    expect(screen.getByText(/错误：主机密钥不匹配/)).toBeTruthy();
  });

  it("仅 disconnected 态出现 [重连]（连接中/已连接时点它没有意义）", () => {
    for (const status of ["connecting", "connected", "error"] as const) {
      const { unmount } = render(StatusBar, { props: props({ status }) });
      expect(screen.queryByText("[重连]"), `${status} 不该有重连按钮`).toBeNull();
      unmount();
    }
    const onReconnect = vi.fn();
    render(StatusBar, { props: props({ status: "disconnected", onReconnect }) });
    expect(screen.getByText("[重连]")).toBeTruthy();
  });

  it("Vault 段两态图标不同且可点（锁/未锁是安全状态，看不出来等于没有）", async () => {
    const onVaultClick = vi.fn();
    const { unmount } = render(StatusBar, { props: props({ vaultLocked: true, onVaultClick }) });
    const locked = screen.getByTitle("点击解锁 Vault");
    await fireEvent.click(locked);
    expect(onVaultClick).toHaveBeenCalledTimes(1);
    const lockedIcon = locked.textContent?.trim();
    unmount();
    render(StatusBar, { props: props({ vaultLocked: false }) });
    const unlockedIcon = screen.getByTitle("点击锁定 Vault").textContent?.trim();
    expect(unlockedIcon).not.toBe(lockedIcon);
  });
});

/**
 * 接线断言：组件可点不等于装配层接上了。修复前 `onKeyboardModeClick` 的实现只在
 * App.svelte 里存在一行，组件侧与它之间没有任何东西证明它们对得上。
 */
describe("App.svelte 键盘模式段接线", () => {
  it("onKeyboardModeClick 落到 toggleKeyboardMode(活动会话)", () => {
    expect(
      /onKeyboardModeClick=\{[^}]*toggleKeyboardMode\(/.test(APP_SOURCE),
      "状态栏的键盘模式点击必须接到 toggleKeyboardMode——断了则 macOS 上无路可切",
    ).toBe(true);
  });

  it("切换的是 per-session 的活动会话，不是某个全局值", () => {
    // 键盘模式是 per-session（F25 / UI 规格 §2.12）：接成全局会让一个标签的切换
    // 波及其余所有标签。
    const m = APP_SOURCE.match(/onKeyboardModeClick=\{([^}]*\}[^}]*)\}/);
    expect(m?.[1] ?? "").toMatch(/activeTabId/);
  });
});

/**
 * 编码段（M7.4）。此前它是一个恒显 "UTF-8" 的只读文本，title 里写着「v1 仅 UTF-8」。
 *
 * 现在它在有活动会话时是可点的入口。两态都要钉：**无会话时不可点**——一个点了什么都不会
 * 发生的按钮比一段只读文本更糟，用户会以为程序坏了。
 */
describe("StatusBar 编码段（M7.4）", () => {
  const seg = () => document.querySelector('[data-testid="status-encoding"]') as HTMLElement;

  it("接了回调时是按钮，点它回调", async () => {
    let hits = 0;
    render(StatusBar, { encoding: "GBK", onEncodingClick: () => (hits += 1) });
    expect(seg().tagName).toBe("BUTTON");
    expect(seg().textContent).toContain("GBK");
    await fireEvent.click(seg());
    expect(hits).toBe(1);
  });

  it("没接回调（无会话）时退回只读文本，不是一个点不动的按钮", () => {
    render(StatusBar, { encoding: "UTF-8" });
    expect(seg().tagName).not.toBe("BUTTON");
  });

  it("提示语要说明切换不需要重连", () => {
    render(StatusBar, { encoding: "UTF-8", onEncodingClick: () => {} });
    expect(seg().getAttribute("title")).toContain("不需要重连");
  });
});

/**
 * 主机段（M7.4）：串口会话上它显示端口 + 参数摘要，并且可点（改波特率）。
 *
 * 与编码段同一条纪律：**没接回调时退回只读文本**——一个点了什么都不会发生的按钮
 * 比一段只读文本更糟。
 */
describe("StatusBar 主机段（M7.4 串口）", () => {
  const seg = () => document.querySelector('[data-testid="status-host"]') as HTMLElement;

  it("接了回调时是按钮，点它回调，提示语说明不重开端口", async () => {
    let hits = 0;
    render(StatusBar, { host: "COM3 · 115200 8N1", onHostClick: () => (hits += 1) });
    expect(seg().tagName).toBe("BUTTON");
    expect(seg().textContent).toContain("115200 8N1");
    expect(seg().getAttribute("title")).toContain("不重开端口");
    await fireEvent.click(seg());
    expect(hits).toBe(1);
  });

  it("SSH 会话（没接回调）时是只读文本", () => {
    render(StatusBar, { host: "10.0.0.1:22" });
    expect(seg().tagName).not.toBe("BUTTON");
    expect(seg().textContent).toContain("10.0.0.1:22");
  });
});
