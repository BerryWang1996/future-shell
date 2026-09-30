/**
 * broadcast-actions（2026-09-03 从 App.svelte 搬出）。
 *
 * 广播是本程序最容易造成真实损害的开关之一——开着它敲的每个字都会进所有目标机组。
 * 三条判据都围着「不要让用户以为在广播、其实没有；也不要让他以为没在广播、其实在」：
 * · 目标=当前会话时拒开而不是空开；
 * · 关闭态零开销短路，绝不发 IPC；
 * · 开启时排除自身，否则每键两份回显。
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";

const calls: { cmd: string; args?: Record<string, unknown> }[] = [];
vi.mock("./ipc", () => ({
  invoke: async (cmd: string, args?: Record<string, unknown>) => {
    calls.push({ cmd, args });
    return undefined;
  },
}));

import { resetToastsForTest, toast } from "./toast";
import { tabs, activeTabId } from "./tabs";
import { composeTarget, liveBroadcast, pickedSessions } from "./broadcast";
import { broadcastEmptyHint, broadcastTargetIds, routeBroadcastKeystroke, toggleLiveBroadcast } from "./broadcast-actions";
import type { Profile } from "./types";

const tab = (id: string, profileId = "p1", status = "connected") =>
  ({ id, title: id, bell: false, status, profileId, host: "", kind: "ssh", keyboardMode: "remote" }) as never;
const PROFILES = [
  { id: "p1", name: "a", group_path: "prod", host: "h1", port: 22, username: "root" },
  { id: "p2", name: "b", group_path: "prod", host: "h2", port: 22, username: "root" },
] as unknown as Profile[];

beforeEach(() => {
  calls.length = 0;
  resetToastsForTest(); // dismiss 分两拍（动效批），同步清不干净
  tabs.set([]);
  activeTabId.set(null);
  composeTarget.set("current");
  liveBroadcast.set(false);
  pickedSessions.set([]);
});

describe("broadcastEmptyHint：三种空因说三句不同的话", () => {
  it.each([
    ["pick", "手选"],
    ["group", "已连接"],
    ["all", "活动会话"],
  ])("target=%s 的提示带「%s」", (target, frag) => {
    expect(broadcastEmptyHint(target as never)).toContain(frag);
  });

  it("三句互不相同（同一句话等于没区分空因，用户不知道该去做什么）", () => {
    const hints = (["pick", "group", "all"] as const).map(broadcastEmptyHint);
    expect(new Set(hints).size).toBe(3);
  });
});

describe("toggleLiveBroadcast", () => {
  it("目标=当前会话时拒开：那是恒空操作，开了按钮还亮着最危险", () => {
    composeTarget.set("current");
    toggleLiveBroadcast(PROFILES);
    expect(get(liveBroadcast)).toBe(false);
    expect(get(toast).some((t) => t.level === "warn" && t.msg.includes("当前会话"))).toBe(true);
  });

  it("目标=全部时可开，并报出会同步到几个**其他**会话", () => {
    tabs.set([tab("s1"), tab("s2"), tab("s3")]);
    activeTabId.set("s1");
    composeTarget.set("all");
    toggleLiveBroadcast(PROFILES);
    expect(get(liveBroadcast)).toBe(true);
    expect(get(toast)[0].msg).toContain("2 个其他会话"); // 三个连着，排除自身
  });

  it("已开启时再点即关，且关掉这一次不看目标（否则关不掉）", () => {
    liveBroadcast.set(true);
    composeTarget.set("current");
    toggleLiveBroadcast(PROFILES);
    expect(get(liveBroadcast)).toBe(false);
    expect(get(toast)[0].msg).toContain("已关闭");
  });
});

describe("routeBroadcastKeystroke", () => {
  it("关闭态零开销短路：不 resolve、不 invoke", () => {
    liveBroadcast.set(false);
    tabs.set([tab("s1"), tab("s2")]);
    routeBroadcastKeystroke("YQ==", PROFILES);
    expect(calls).toEqual([]);
  });

  it("目标=当前会话时不广播（路由自身 = 关）", () => {
    liveBroadcast.set(true);
    composeTarget.set("current");
    tabs.set([tab("s1"), tab("s2")]);
    activeTabId.set("s1");
    routeBroadcastKeystroke("YQ==", PROFILES);
    expect(calls).toEqual([]);
  });

  it("开启时排除自身：三个会话只发两份，不然每键两份回显", async () => {
    liveBroadcast.set(true);
    composeTarget.set("all");
    tabs.set([tab("s1"), tab("s2"), tab("s3")]);
    activeTabId.set("s1");
    routeBroadcastKeystroke("YQ==", PROFILES);
    await Promise.resolve();
    await Promise.resolve();
    const ids = calls.map((c) => c.args?.sessionId);
    expect(ids).not.toContain("s1");
    expect(new Set(ids)).toEqual(new Set(["s2", "s3"]));
  });

  it("目标解析为空时不发（避免一串空 IPC）", () => {
    liveBroadcast.set(true);
    composeTarget.set("all");
    tabs.set([tab("s1")]);
    activeTabId.set("s1"); // 只有自己，排除后为空
    routeBroadcastKeystroke("YQ==", PROFILES);
    expect(calls).toEqual([]);
  });
});

describe("broadcastTargetIds 与 store 快照同源", () => {
  it("按 target 解析出的 id 来自当前 tabs / picked 快照", () => {
    tabs.set([tab("s1"), tab("s2")]);
    activeTabId.set("s1");
    pickedSessions.set(["s2"]);
    expect(broadcastTargetIds("pick", PROFILES)).toEqual(["s2"]);
    expect(new Set(broadcastTargetIds("all", PROFILES))).toEqual(new Set(["s1", "s2"]));
  });
});
