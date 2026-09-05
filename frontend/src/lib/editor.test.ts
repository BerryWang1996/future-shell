/**
 * 外部编辑器关联的呈现层测试（M4a）。
 *
 * 判定本身在后端（fs_sshengine::editsync 单测），这里钉的是「结论怎么用」：
 * 只有冲突/远端消失才拦人，无冲突自动回传，且冲突文案必须把两个时刻都说出来
 * ——用户要据此判断远端那份是谁改的。
 */
import { describe, expect, it } from "vitest";
import {
  POLL_MS,
  conflictMessage,
  goneMessage,
  needsConfirm,
  shouldAutoUpload,
  type EditDecision,
} from "./editor";

const CONFLICT: EditDecision = {
  kind: "conflict",
  remote_at_open: { size: 100, mtime: 1_787_300_000 },
  remote_now: { size: 250, mtime: 1_787_310_000 },
};

describe("结论的用法", () => {
  it("无本地改动：既不回传也不拦人（打开又关掉是常态）", () => {
    const d: EditDecision = { kind: "no_local_change" };
    expect(shouldAutoUpload(d)).toBe(false);
    expect(needsConfirm(d)).toBe(false);
  });

  it("本地存盘、远端未动：自动回传，不打扰用户", () => {
    const d: EditDecision = { kind: "upload" };
    expect(shouldAutoUpload(d)).toBe(true);
    expect(needsConfirm(d)).toBe(false);
  });

  it("冲突：必须拦人，且绝不自动回传", () => {
    expect(needsConfirm(CONFLICT)).toBe(true);
    expect(shouldAutoUpload(CONFLICT)).toBe(false);
  });

  it("远端消失：不自动回传（重建一个用户可能正想删掉的文件）", () => {
    const d: EditDecision = { kind: "remote_gone" };
    expect(shouldAutoUpload(d)).toBe(false);
  });
});

describe("冲突文案", () => {
  it("同时给出「打开时」与「现在」两个状态（只说「已变更」用户无法决定）", () => {
    const m = conflictMessage("/etc/nginx.conf", CONFLICT);
    expect(m).toContain("/etc/nginx.conf");
    expect(m).toContain("100 字节");
    expect(m).toContain("250 字节");
    expect(m).toContain("打开时");
    expect(m).toContain("现在");
    // 覆盖不可撤销这件事必须说破
    expect(m).toContain("覆盖");
    expect(m).toContain("不可撤销");
  });

  it("mtime 为 0（服务端未回）显示「未知时间」而不是 1970 年", () => {
    const m = conflictMessage("x", {
      kind: "conflict",
      remote_at_open: { size: 1, mtime: 0 },
      remote_now: { size: 2, mtime: 0 },
    });
    expect(m).toContain("未知时间");
    expect(m).not.toContain("1970");
  });

  it("非冲突结论不产出文案（避免误用成通用提示）", () => {
    expect(conflictMessage("x", { kind: "upload" })).toBe("");
    expect(conflictMessage("x", { kind: "no_local_change" })).toBe("");
  });

  it("远端消失的文案说明「会重新创建」并给出替代动作", () => {
    const m = goneMessage("/tmp/gone.conf");
    expect(m).toContain("/tmp/gone.conf");
    expect(m).toContain("重新创建");
    expect(m).toContain("停止编辑");
  });
});

describe("轮询间隔", () => {
  it("既不过快也不过慢：过快会在「先截断再写」的瞬间抓到空文件", () => {
    expect(POLL_MS).toBeGreaterThanOrEqual(1000);
    expect(POLL_MS).toBeLessThanOrEqual(5000);
  });
});
