import { describe, expect, it, vi } from "vitest";
import {
  canQuickResend,
  formatUsedAt,
  sendThenRecord,
  sourceLabel,
  type HistoryEntry,
} from "./history";

const e = (o: Partial<HistoryEntry>): HistoryEntry => ({
  id: 1,
  command: "ls",
  host: "h",
  profile_id: "p",
  source: "sent",
  used_at: 1000,
  use_count: 1,
  ...o,
});

describe("formatUsedAt", () => {
  const now = 1_700_000_000;

  it("分级到分/时/天", () => {
    expect(formatUsedAt(now - 5, now)).toBe("刚刚");
    expect(formatUsedAt(now - 59, now)).toBe("刚刚");
    expect(formatUsedAt(now - 60, now)).toBe("1 分钟前");
    expect(formatUsedAt(now - 3599, now)).toBe("59 分钟前");
    expect(formatUsedAt(now - 3600, now)).toBe("1 小时前");
    expect(formatUsedAt(now - 86_399, now)).toBe("23 小时前");
    expect(formatUsedAt(now - 86_400, now)).toBe("1 天前");
    expect(formatUsedAt(now - 6 * 86_400, now)).toBe("6 天前");
  });

  it("超过一周退回绝对日期（「9 天前」已不比日期更有信息量）", () => {
    const out = formatUsedAt(now - 30 * 86_400, now);
    expect(out).toMatch(/^\d{4}-\d{2}-\d{2}$/);
  });

  it("时钟回拨不编造负数时间（含大幅提前的时间戳）", () => {
    expect(formatUsedAt(now + 500, now)).toBe("刚刚");
    // 差出一年也不能变成「-365 天前」
    expect(formatUsedAt(now + 365 * 86_400, now)).toBe("刚刚");
    expect(formatUsedAt(now + 365 * 86_400, now)).not.toContain("-");
  });

  it("非法/缺失时间显示「—」而不是 1970", () => {
    expect(formatUsedAt(0, now)).toBe("—");
    expect(formatUsedAt(-1, now)).toBe("—");
    expect(formatUsedAt(Number.NaN, now)).toBe("—");
  });
});

describe("来源区分", () => {
  it("两种来源标签不同——混为一谈会让人重发一段被误切的输出", () => {
    expect(sourceLabel("sent")).toBe("发出");
    expect(sourceLabel("grid")).toBe("屏幕");
    expect(sourceLabel("sent")).not.toBe(sourceLabel("grid"));
  });

  it("只有亲手发出的条目允许回车快速重发", () => {
    expect(canQuickResend(e({ source: "sent" }))).toBe(true);
    expect(canQuickResend(e({ source: "grid" }))).toBe(false);
  });
});

describe("sendThenRecord", () => {
  it("发送成功才记历史", async () => {
    const send = vi.fn(async () => {});
    const record = vi.fn();
    await sendThenRecord("ls -la", "s1", send, record);
    expect(send).toHaveBeenCalledWith("ls -la");
    expect(record).toHaveBeenCalledWith("ls -la", "s1");
  });

  it("发送失败**不**记历史——否则历史里会有一条其实从未发出的命令", async () => {
    const send = vi.fn(async () => {
      throw new Error("no session");
    });
    const record = vi.fn();
    await expect(sendThenRecord("rm -rf /tmp/x", "s1", send, record)).rejects.toThrow("no session");
    expect(record).not.toHaveBeenCalled();
  });

  it("次序是先发后记（记录时命令已经在线上了）", async () => {
    const order: string[] = [];
    await sendThenRecord(
      "echo hi",
      "s1",
      async () => {
        order.push("send");
      },
      () => {
        order.push("record");
      },
    );
    expect(order).toEqual(["send", "record"]);
  });
});
