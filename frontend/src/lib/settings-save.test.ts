import { beforeEach, describe, expect, it, vi } from "vitest";
const { invoke, error } = vi.hoisted(() => ({ invoke: vi.fn(), error: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
vi.mock("./toast", () => ({ toast: { error, warn: vi.fn() } }));
import { settingSet, settingChanged } from "./ipc";

describe("设置持久化结果", () => {
  beforeEach(() => vi.clearAllMocks());
  it("写入成功才广播并返回成功", async () => {
    const seen: unknown[] = [];
    const off = settingChanged.subscribe(v => seen.push(v));
    invoke.mockResolvedValueOnce(undefined);
    expect(await settingSet("term.fontSize", 18)).toBe(true);
    expect(seen.at(-1)).toEqual({ key: "term.fontSize", value: 18 });
    expect(invoke).toHaveBeenCalledWith("settings_set", { key: "term.fontSize", value: "18" });
    off();
  });
  it("落库失败返回 false，不广播假值，并提示用户重试", async () => {
    const seen: unknown[] = [];
    const off = settingChanged.subscribe(v => seen.push(v));
    const count = seen.length;
    invoke.mockRejectedValueOnce(new Error("磁盘已满"));
    expect(await settingSet("term.fontSize", 20)).toBe(false);
    expect(seen).toHaveLength(count);
    expect(error).toHaveBeenCalledWith(expect.stringContaining("设置未保存"));
    off();
  });
});
