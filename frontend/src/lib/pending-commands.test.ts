import { beforeEach, describe, expect, it } from "vitest";
import { dropForSession, pendingCount, queueForSession, resetPendingCommandsForTest, takeForSession } from "./pending-commands";
import PENDING_SRC from "./pending-commands.ts?raw";
import APP_SRC from "../App.svelte?raw";

/**
 * M7.3「前台运行远端脚本 → 新终端标签」的排队层。
 *
 * 一次性语义是本模块存在的全部理由：会话断线自动重连会再次进入 connected，此时把命令重发
 * 一遍就是**执行了两次**，而用户只双击过一次脚本。`takeForSession` 取走即清把这件事钉死。
 */
describe("排队与取用", () => {
  beforeEach(() => resetPendingCommandsForTest());

  it("排进去能取出来，按排入顺序", () => {
    queueForSession("s1", "sh /a.sh");
    queueForSession("s1", "sh /b.sh");
    expect(takeForSession("s1")).toEqual(["sh /a.sh", "sh /b.sh"]);
  });

  it("取走即清：第二次取是空的（重连再次 connected 不会重跑脚本）", () => {
    queueForSession("s1", "sh /a.sh");
    expect(takeForSession("s1")).toEqual(["sh /a.sh"]);
    expect(takeForSession("s1")).toEqual([]);
    expect(pendingCount("s1")).toBe(0);
  });

  it("按会话隔离：取 s1 不影响 s2", () => {
    queueForSession("s1", "a");
    queueForSession("s2", "b");
    takeForSession("s1");
    expect(takeForSession("s2")).toEqual(["b"]);
  });

  it("没排过的会话取出空数组，不抛", () => {
    expect(takeForSession("never")).toEqual([]);
  });

  it("dropForSession 丢弃：会话没连上就被关掉，命令不得留给下一个同 id 的会话", () => {
    queueForSession("s1", "sh /a.sh");
    dropForSession("s1");
    expect(takeForSession("s1")).toEqual([]);
  });
});

/**
 * 排空点的**跨文件**闭合。
 *
 * 这个模块最初的文档写着「由 App 的 session:status → connected 取走」——那是错的：首次连接时
 * 后端那几条 connected 事件全部发生在标签存在之前（见 session-status.ts），事件桥根本收不到。
 * 按那个写法接线，队列会一直躺在内存里，用户看到的是一个空白新标签。
 *
 * 所以钉住两条：排空必须发生在 `onReady`（终端建好）**和** `runScriptInNewTab` 拿到 id 之后，
 * 两处都在——谁先到不确定，只留一处就有一半概率命令永远不发；`takeForSession` 保证不会双发。
 */
describe("排空点接线（源码级）", () => {
  it("模块文档不再宣称走 session:status", () => {
    expect(PENDING_SRC).not.toMatch(/由 App 的 `session:status` → connected 那一刻取走/);
  });

  it("App 在 onReady 里排空", () => {
    expect(APP_SRC).toMatch(/onReady=\{\(api\) =>[\s\S]{0,120}flushPendingCommands\(tab\.id\)/);
  });

  it("App 在拿到新会话 id 之后也排空一次", () => {
    expect(APP_SRC).toMatch(/queueForSession\(id, command\);[\s\S]{0,120}flushPendingCommands\(id\)/);
  });

  it("会话关闭时丢弃它的队列", () => {
    expect(APP_SRC).toMatch(/dropForSession\(e\.payload\.session_id\)/);
  });
});
