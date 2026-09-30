/**
 * M4a 广播双通道判据（路线图出口标准）：
 *   「组合栏 5 会话同发一致性测试（各会话收到字节级一致）」
 *   「实时输入广播：一端键入 ≥5 会话字节级一致（含交互式命令中途输入用例）」
 *
 * 全部落在 lib 纯函数上（S305–S306）：App.svelte 没有组件测试（先例见
 * window-close.ts 头注），装配层一旦吸收判据，判据就死了。
 */
import { describe, expect, it, vi } from "vitest";
import {
  resolveTargetSessions,
  sendToSessions,
  type BroadcastProfileInfo,
  type BroadcastTabInfo,
  type TermInputFn,
} from "./broadcast";

/** 5 会话现场（出口标准的规模）：3 个生产组 + 1 个工作组 + 1 个无分组，
 *  s3 断线——分组/手选的「断线剔除」与「同组圈定」都由它显形。 */
function fiveSessions(): { tabs: BroadcastTabInfo[]; profiles: BroadcastProfileInfo[] } {
  const tabs: BroadcastTabInfo[] = [
    { id: "s1", profileId: "p1", status: "connected" },
    { id: "s2", profileId: "p2", status: "connected" },
    { id: "s3", profileId: "p3", status: "disconnected" },
    { id: "s4", profileId: "p4", status: "connected" },
    { id: "s5", profileId: "p5", status: "connecting" },
  ];
  const profiles: BroadcastProfileInfo[] = [
    { id: "p1", group_path: "prod" },
    { id: "p2", group_path: "prod" },
    { id: "p3", group_path: "prod" },
    { id: "p4", group_path: "work" },
    { id: "p5", group_path: "work" },
  ];
  return { tabs, profiles };
}

describe("resolveTargetSessions（S305）", () => {
  it("current：活动会话已连接则恰为其一；未连接/无活动为空", () => {
    const { tabs, profiles } = fiveSessions();
    expect(resolveTargetSessions("current", tabs, profiles, "s1", [])).toEqual(["s1"]);
    expect(resolveTargetSessions("current", tabs, profiles, "s3", [])).toEqual([]);
    expect(resolveTargetSessions("current", tabs, profiles, null, [])).toEqual([]);
  });

  it("all：全部已连接会话按标签序——断线的 s3 与连接中的 s5 都不得进集合", () => {
    const { tabs, profiles } = fiveSessions();
    expect(resolveTargetSessions("all", tabs, profiles, "s1", [])).toEqual(["s1", "s2", "s4"]);
  });

  it("group：同组已连接会话（含自身），断线的同组成员剔除；活动会话在异组时按活动会话的组", () => {
    const { tabs, profiles } = fiveSessions();
    expect(resolveTargetSessions("group", tabs, profiles, "s1", [])).toEqual(["s1", "s2"]);
    expect(resolveTargetSessions("group", tabs, profiles, "s4", [])).toEqual(["s4"]);
  });

  it("group：活动会话无分组退化为自身（无分组即无同伴，弹空集警告反而是打断）", () => {
    const tabs = [{ id: "s1", profileId: "p1", status: "connected" }];
    const profiles = [{ id: "p1", group_path: null }];
    expect(resolveTargetSessions("group", tabs, profiles, "s1", [])).toEqual(["s1"]);
  });

  it("pick：按手选顺序、去重、断线剔除（集合本身不动，恢复连接自然回来）", () => {
    const { tabs, profiles } = fiveSessions();
    const picked = ["s4", "s3", "s4", "s2"];
    expect(resolveTargetSessions("pick", tabs, profiles, "s1", picked)).toEqual(["s4", "s2"]);
    expect(picked).toEqual(["s4", "s3", "s4", "s2"]); // 解析不得改写手选集合本身
  });
});

describe("sendToSessions（S305：字节级一致）", () => {
  /** 断言「每个会话收到的载荷与发送方逐字节相同」的公共形状。 */
  function makeSpy(): { fn: TermInputFn; calls: { sessionId: string; dataB64: string }[] } {
    const calls: { sessionId: string; dataB64: string }[] = [];
    const fn = vi.fn(async (cmd: "term_input", args: { sessionId: string; dataB64: string }) => {
      expect(cmd).toBe("term_input");
      calls.push(args);
    }) as unknown as TermInputFn;
    return { fn, calls };
  }

  it("组合栏 5 会话同发：各会话收到字节级一致的载荷（出口标准原文）", async () => {
    const ids = ["s1", "s2", "s3", "s4", "s5"];
    const payload = "Zm9vYmFy"; // 任取一段 b64
    const { fn, calls } = makeSpy();
    const r = await sendToSessions(ids, payload, fn);
    expect(r.sent).toEqual(ids);
    expect(r.failed).toEqual([]);
    expect(calls.map((c) => c.sessionId)).toEqual(ids);
    // 判据本体：5 份载荷与原文完全相同——不是「等价」不是「解码后相同」，
    // 是同一字符串。中途任何解码重编都会在这里红。
    expect(calls.every((c) => c.dataB64 === payload)).toBe(true);
  });

  it("实时键入广播：一端连续键入（含交互式命令中途输入）各会话逐键字节级一致（出口标准原文）", async () => {
    const ids = ["s1", "s2", "s3", "s4", "s5"];
    // 模拟「敲 sudo apt upgrade，密码提示中途再键入」：逐键发送、载荷各不相同
    const keystrokes = ["cw", "N1", "by", "d", "g", "cHc=", "cGFzc3dvcmQ="];
    for (const b64 of keystrokes) {
      const { fn, calls } = makeSpy();
      const r = await sendToSessions(ids, b64, fn);
      expect(r.sent).toEqual(ids);
      expect(calls.length).toBe(5);
      expect(calls.every((c) => c.dataB64 === b64), "每一键的 5 份副本须字节级一致").toBe(true);
    }
  });

  it("单会话失败不阻断其余（广播的要点恰是一个目标死了其余照发）", async () => {
    const calls: { sessionId: string; dataB64: string }[] = [];
    const fn = (async (_cmd: "term_input", args: { sessionId: string; dataB64: string }) => {
      if (args.sessionId === "s3") throw new Error("no session");
      calls.push(args);
    }) as TermInputFn;
    const r = await sendToSessions(["s1", "s2", "s3", "s4"], "eA==", fn);
    expect(r.sent).toEqual(["s1", "s2", "s4"]);
    expect(r.failed).toEqual([{ sessionId: "s3", error: "Error: no session" }]);
    expect(calls.length).toBe(3);
  });

  it("失败结果不含载荷内容（P2-20：组合栏载荷常含口令，错误面不得回显）", async () => {
    const fn = (async () => {
      throw new Error("boom");
    }) as TermInputFn;
    const r = await sendToSessions(["s1"], "c2VjcmV0LWNtZA==", fn);
    expect(r.failed[0].error).not.toContain("c2VjcmV0LWNtZA==");
  });
});
