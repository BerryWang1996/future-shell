import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/svelte";
import type { ConnectFailure, Profile } from "../lib/types";

/** key_list 桩回复，由用例设定。null = 让 invoke 抛（面板要能在拿不到列表时照常工作）。 */
let keyListReply: { agent_error: string | null; vault_locked: boolean } | null = {
  agent_error: null,
  vault_locked: false,
};
const invoked: string[] = [];
vi.mock("../lib/ipc", () => ({
  invoke: async <T>(cmd: string): Promise<T> => {
    invoked.push(cmd);
    if (cmd === "key_list") {
      if (!keyListReply) throw new Error("key_list 不可用");
      return keyListReply as T;
    }
    throw new Error(`未桩化的命令：${cmd}`);
  },
}));

import ConnectFailurePanel from "./ConnectFailurePanel.svelte";

/* ------------------------------------------------------------------------------
 * 连接失败面板（路线图 4c，2026-09-01）：失败当刻**带出路**。
 *
 * 用户三次撞到「只弹提示、不给入口」。这里钉的不是好不好看，是三条纪律：
 *   ① 每颗按钮点一次只重试一次、在途禁用（publickey 每失败一次吃一次 MaxAuthTries）；
 *   ② 有跳板时禁掉改档案的一键动作（错误里不带「失败在第几跳」）；
 *   ③ remaining 为空时**不说**「服务器只接受 X」（那时这句是编的）。
 * ---------------------------------------------------------------------------- */

const PROFILE: Profile = {
  id: "p-1",
  name: "prod",
  group_path: null,
  host: "10.0.0.9",
  port: 2222,
  username: "root",
  auth: { vault_record: null, allow_agent: false },
  jump: [],
};

const PUBKEY_ONLY: ConnectFailure = {
  summary: 'auth failed; tried: []; server allows: ["publickey"]; notes: ["只收密钥…"]',
  category: "auth",
  tried: [],
  remaining: ["publickey"],
  notes: ["本连接未配置任何可用的认证凭据，而这台服务器只接受密钥认证（publickey）", "agent 管道：\\\\.\\pipe\\openssh-ssh-agent 不存在"],
  host: "10.0.0.9",
  port: 2222,
  username: "root",
  fingerprint: "SHA256:abc",
  has_jump: false,
};

beforeEach(() => {
  invoked.length = 0;
  keyListReply = { agent_error: null, vault_locked: false };
});

/** 让 onMount 里的 key_list 落地。 */
async function flush(): Promise<void> {
  await new Promise((r) => setTimeout(r, 0));
}

describe("ConnectFailurePanel", () => {
  it("顶部固定显示 user@host:port 与指纹——防在 A 机面板上给 B 机挑私钥", () => {
    render(ConnectFailurePanel, { props: { failure: PUBKEY_ONLY, profile: PROFILE } });
    expect(screen.getByTestId("cf-identity").textContent).toBe("root@10.0.0.9:2222");
    expect(screen.getByTestId("cf-fingerprint").textContent).toBe("SHA256:abc");
  });

  it("说清服务器只接受什么（对端通告原话）+ 一次都没发起", () => {
    render(ConnectFailurePanel, { props: { failure: PUBKEY_ONLY, profile: PROFILE } });
    const fact = screen.getByTestId("cf-server-accepts").textContent ?? "";
    expect(fact).toContain("publickey");
    expect(fact).toContain("一次认证都没有发起");
    // 主指引 = notes 第一句；其余折叠（agent 管道路径那种本地细节不摊在主文案里）
    expect(screen.getByTestId("cf-headline").textContent).toContain("只接受密钥认证");
    expect(screen.getByTestId("cf-more").textContent).toContain("agent 管道");
  });

  it("remaining 为空时不说「服务器只接受 X」——那句是编的", () => {
    const f: ConnectFailure = { ...PUBKEY_ONLY, category: "connect", remaining: [], notes: [], summary: "connect: refused" };
    render(ConnectFailurePanel, { props: { failure: f, profile: PROFILE, errorText: "connect: refused" } });
    expect(screen.queryByTestId("cf-server-accepts")).toBeNull();
    expect(screen.getByTestId("cf-headline").textContent).toContain("connect: refused");
    // 非认证类：没有 agent/配置以外的认证按钮误导用户；重试仍在
    expect(screen.queryByTestId("cf-agent")).toBeNull();
    expect(screen.getByTestId("cf-retry")).toBeTruthy();
  });

  it("publickey-only 时挂「用本地 SSH Agent 重试」，点一次只调一次，在途禁用", async () => {
    let calls = 0;
    let release: () => void = () => {};
    const onEnableAgent = () => {
      calls++;
      return new Promise<void>((r) => { release = r; });
    };
    render(ConnectFailurePanel, { props: { failure: PUBKEY_ONLY, profile: PROFILE, onEnableAgent } });
    await flush();
    const btn = screen.getByTestId("cf-agent") as HTMLButtonElement;
    expect(btn.disabled).toBe(false);
    await fireEvent.click(btn);
    await fireEvent.click(btn); // 连点：第二下必须被在途禁用吃掉
    await fireEvent.click(screen.getByTestId("cf-retry")); // 别的按钮同样禁用
    expect(calls, "一次点击 = 一次尝试，连点不得叠加（MaxAuthTries）").toBe(1);
    expect(btn.disabled).toBe(true);
    release();
  });

  it("agent 不可用时按钮禁用并把原因写进 title——不给一颗点了没变化的按钮", async () => {
    keyListReply = { agent_error: "本机没有运行 ssh-agent（openssh-ssh-agent 管道不存在）", vault_locked: false };
    render(ConnectFailurePanel, { props: { failure: PUBKEY_ONLY, profile: PROFILE } });
    await waitFor(() => expect((screen.getByTestId("cf-agent") as HTMLButtonElement).disabled).toBe(true));
    expect(screen.getByTestId("cf-agent").getAttribute("title")).toContain("ssh-agent");
  });

  it("档案已开 agent → 不再挂「用 Agent 重试」（点了等于没点）", async () => {
    const p: Profile = { ...PROFILE, auth: { vault_record: null, allow_agent: true } };
    render(ConnectFailurePanel, { props: { failure: PUBKEY_ONLY, profile: p } });
    await flush();
    expect(screen.queryByTestId("cf-agent")).toBeNull();
  });

  it("有跳板：一键改档案的动作禁用并解释，「配置认证…」与「重试」保留", async () => {
    render(ConnectFailurePanel, { props: { failure: { ...PUBKEY_ONLY, has_jump: true }, profile: PROFILE } });
    await flush();
    expect((screen.getByTestId("cf-agent") as HTMLButtonElement).disabled).toBe(true);
    expect(screen.getByTestId("cf-jump-hint")).toBeTruthy();
    expect((screen.getByTestId("cf-configure") as HTMLButtonElement).disabled).toBe(false);
    expect((screen.getByTestId("cf-retry") as HTMLButtonElement).disabled).toBe(false);
  });

  it("服务器收口令时重试按钮改叫「重试并输入口令」（重连会弹框问）", () => {
    const f: ConnectFailure = { ...PUBKEY_ONLY, remaining: ["publickey", "password"] };
    render(ConnectFailurePanel, { props: { failure: f, profile: PROFILE } });
    expect(screen.getByTestId("cf-retry").textContent).toContain("输入口令");
  });

  it("保险库锁着 → 说明 + 「解锁保险库」按钮", async () => {
    keyListReply = { agent_error: null, vault_locked: true };
    const onUnlockVault = vi.fn();
    render(ConnectFailurePanel, { props: { failure: PUBKEY_ONLY, profile: PROFILE, onUnlockVault } });
    await waitFor(() => expect(screen.getByTestId("cf-unlock")).toBeTruthy());
    expect(screen.getByTestId("cf-vault-locked")).toBeTruthy();
    await fireEvent.click(screen.getByTestId("cf-unlock"));
    expect(onUnlockVault).toHaveBeenCalledTimes(1);
  });

  it("「配置认证…」与「完整错误」各调对应回调", async () => {
    const onConfigureAuth = vi.fn();
    const onDetail = vi.fn();
    render(ConnectFailurePanel, { props: { failure: PUBKEY_ONLY, profile: PROFILE, onConfigureAuth, onDetail } });
    await fireEvent.click(screen.getByTestId("cf-configure"));
    await fireEvent.click(screen.getByTestId("cf-detail"));
    expect(onConfigureAuth).toHaveBeenCalledTimes(1);
    expect(onDetail).toHaveBeenCalledTimes(1);
  });

  it("老式字符串失败（无 failure）：退化成文案 + 重试 + 配置，不崩", async () => {
    render(ConnectFailurePanel, { props: { failure: null, errorText: "no route to host", profile: PROFILE } });
    await flush();
    expect(screen.getByTestId("cf-headline").textContent).toContain("no route to host");
    expect(screen.getByTestId("cf-retry")).toBeTruthy();
    expect(screen.queryByTestId("cf-server-accepts")).toBeNull();
    // 非认证类不问 key_list
    expect(invoked).not.toContain("key_list");
  });

  it("档案已删（反查不到）：只渲染文案，不渲染点了没反应的按钮", () => {
    render(ConnectFailurePanel, { props: { failure: PUBKEY_ONLY, profile: null } });
    expect(screen.queryByTestId("cf-retry")).toBeNull();
    expect(screen.queryByTestId("cf-configure")).toBeNull();
    expect(screen.queryByTestId("cf-agent")).toBeNull();
    expect(screen.getByTestId("cf-detail")).toBeTruthy();
  });

  it("key_list 拿不到时面板照常渲染（agent 按钮仍可点，只是没有禁用理由）", async () => {
    keyListReply = null;
    render(ConnectFailurePanel, { props: { failure: PUBKEY_ONLY, profile: PROFILE } });
    await flush();
    expect((screen.getByTestId("cf-agent") as HTMLButtonElement).disabled).toBe(false);
  });
});
