import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { render, screen, fireEvent, waitFor, cleanup } from "@testing-library/svelte";

const calls: string[] = [];
let reply: unknown = { keys: [], agent_error: null, vault_locked: false };
let shouldThrow: string | null = null;

vi.mock("../lib/ipc", () => ({
  invoke: async <T>(cmd: string): Promise<T> => {
    calls.push(cmd);
    if (shouldThrow) throw new Error(shouldThrow);
    return reply as T;
  },
}));

import KeyManagerDialog from "./KeyManagerDialog.svelte";

const key = (over: Record<string, unknown> = {}) => ({
  source: "vault",
  id: 1,
  label: "我的密钥",
  fingerprint: "SHA256:AAAA",
  algorithm: "ssh-ed25519",
  comment: "me@box",
  status: "ok",
  ...over,
});

beforeEach(() => {
  calls.length = 0;
  shouldThrow = null;
  reply = { keys: [], agent_error: null, vault_locked: false };
});
afterEach(() => cleanup());

async function open() {
  render(KeyManagerDialog, { props: { open: true } });
  await waitFor(() => expect(calls).toContain("key_list"));
}

describe("密钥管理器：只显示可公开信息", () => {
  it("打开即拉取一次", async () => {
    await open();
    expect(calls).toEqual(["key_list"]);
  });

  it("列出指纹、类型、来源", async () => {
    reply = {
      keys: [key(), key({ source: "agent", id: null, label: null, fingerprint: "SHA256:BBBB" })],
      agent_error: null,
      vault_locked: false,
    };
    await open();
    await waitFor(() => expect(screen.getAllByTestId("key-row").length).toBe(2));
    expect(screen.getAllByTestId("key-fingerprint")[0].textContent).toContain("SHA256:AAAA");
    // 来源必须显示：Vault 里的删得掉，agent 里的要去 `ssh-add -d`。
    // 不分来源的话，用户会找一个删不掉的条目的删除按钮。
    expect(screen.getAllByTestId("key-source").map((e) => e.textContent)).toEqual(["Vault", "Agent"]);
  });

  it("界面上明说只有指纹/类型/注释", async () => {
    await open();
    const t = screen.getByTestId("key-manager-scope").textContent ?? "";
    expect(t).toContain("指纹");
    expect(t).toContain("私钥");
  });

  /**
   * 「已加密」是**正常状态**，不是失败。
   *
   * 画成红色失败会让用户去「修」一件没坏的事；而条目本身必须留在列表里——
   * 不显示的话，他会以为这把密钥不见了。
   */
  it("加密的密钥仍在列表里，且说清是「已加密」而不是报错", async () => {
    reply = {
      keys: [key({ fingerprint: null, algorithm: null, comment: null, status: "encrypted", label: "加密的" })],
      agent_error: null,
      vault_locked: false,
    };
    await open();
    await waitFor(() => expect(screen.getByTestId("key-row")).toBeTruthy());
    expect(screen.getByTestId("key-fingerprint").textContent).toContain("已加密");
    // 不是错误区
    expect(screen.queryByTestId("key-manager-error")).toBeNull();
  });
});

describe("两种「没有密钥」要分得开", () => {
  /**
   * 空列表会被读成「agent 在跑但里面没有密钥」，那要去 `ssh-add`；
   * 而 agent 根本没在跑要去启动它。两件完全不同的事。
   */
  it("agent 不可用时说出原因，而不是显示成空列表", async () => {
    reply = { keys: [], agent_error: "未检测到可用的 SSH agent", vault_locked: false };
    await open();
    await waitFor(() => expect(screen.getByTestId("key-manager-agent-error")).toBeTruthy());
    expect(screen.getByTestId("key-manager-agent-error").textContent).toContain("未检测到");
  });

  it("agent 正常时不显示那条警告", async () => {
    // 反向对照：上一条若因那块无条件渲染而通过，这一条会红。
    reply = { keys: [key()], agent_error: null, vault_locked: false };
    await open();
    await waitFor(() => expect(screen.getByTestId("key-row")).toBeTruthy());
    expect(screen.queryByTestId("key-manager-agent-error")).toBeNull();
  });

  /**
   * 一个锁着的 Vault 与一个空 Vault 在列表上长得一样，而用户会以为自己的密钥丢了。
   */
  it("Vault 锁着时说出来", async () => {
    reply = { keys: [], agent_error: null, vault_locked: true };
    await open();
    await waitFor(() => expect(screen.getByTestId("key-manager-vault-locked")).toBeTruthy());
  });

  it("Vault 解锁时不显示那条", async () => {
    reply = { keys: [key()], agent_error: null, vault_locked: false };
    await open();
    await waitFor(() => expect(screen.getByTestId("key-row")).toBeTruthy());
    expect(screen.queryByTestId("key-manager-vault-locked")).toBeNull();
  });

  it("两边都空且都正常时，说「没有找到任何密钥」", async () => {
    await open();
    await waitFor(() => expect(screen.getByTestId("key-manager-empty")).toBeTruthy());
  });
});

describe("agent 开关的指路", () => {
  /**
   * 这里**不提供**全局开关：Profile 上已经有 `auth.allow_agent`，
   * 再加一个全局的会制造一个没人答得上来的问题——全局关、这个 Profile 开，用不用？
   * 但用户打开「代理管理器」时确实会找那个开关，所以要指路。
   */
  it("说清开关在每个连接自己的属性里", async () => {
    await open();
    const t = screen.getByTestId("key-manager-agent-hint").textContent ?? "";
    expect(t).toContain("每个连接");
    expect(t).toContain("SSH Agent");
  });

  it("本对话框里没有任何开关控件", async () => {
    reply = { keys: [key()], agent_error: null, vault_locked: false };
    await open();
    await waitFor(() => expect(screen.getByTestId("key-row")).toBeTruthy());
    const dlg = screen.getByTestId("key-manager");
    expect(dlg.querySelectorAll('input[type="checkbox"]').length).toBe(0);
    expect(dlg.querySelectorAll("select").length).toBe(0);
  });
});

describe("刷新与关闭", () => {
  /**
   * 每次打开都重新取：agent 里的内容会在对话框关着的时候变（用户去别处 `ssh-add` 了），
   * 而这张表的用途正是「核对现在有哪些」。
   */
  it("重开会再拉一次，不吃缓存", async () => {
    const { rerender } = render(KeyManagerDialog, { props: { open: true } });
    await waitFor(() => expect(calls.length).toBe(1));
    await rerender({ open: false });
    await rerender({ open: true });
    await waitFor(() => expect(calls.length).toBe(2));
  });

  it("刷新按钮再拉一次", async () => {
    await open();
    await fireEvent.click(screen.getByTestId("key-manager-refresh"));
    await waitFor(() => expect(calls.length).toBe(2));
  });

  it("拉取失败时把错误显示出来，不是显示成空列表", async () => {
    shouldThrow = "vault 未解锁";
    render(KeyManagerDialog, { props: { open: true } });
    await waitFor(() => expect(screen.getByTestId("key-manager-error")).toBeTruthy());
    expect(screen.getByTestId("key-manager-error").textContent).toContain("vault 未解锁");
    expect(screen.queryByTestId("key-manager-empty")).toBeNull();
  });

  it("Esc 与关闭按钮都能关", async () => {
    for (const how of ["esc", "button"] as const) {
      let closed = false;
      render(KeyManagerDialog, { props: { open: true, onClose: () => (closed = true) } });
      await waitFor(() => expect(screen.getByTestId("key-manager")).toBeTruthy());
      if (how === "esc") {
        await fireEvent.keyDown(screen.getByTestId("key-manager"), { key: "Escape" });
      } else {
        await fireEvent.click(screen.getByTestId("key-manager-close"));
      }
      expect(closed, how).toBe(true);
      cleanup();
    }
  });
});
