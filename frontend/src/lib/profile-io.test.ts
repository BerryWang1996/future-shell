/**
 * profile-io（2026-09-03 从 App.svelte 搬出）。
 *
 * 搬出来的价值就在这里：这四件事此前只能靠渲染整个 App 才碰得到，于是「文件名怎么拼」
 * 「超限文件挡不挡」这类分支从来没人测过——而它们各自都对应过一次真实缺陷
 * （P1-13 的键名、审计2 #37 的尺寸闸、onExportOne 那个从没传过的 prop）。
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";

const calls: { cmd: string; args?: Record<string, unknown> }[] = [];
const replies: Record<string, (a?: Record<string, unknown>) => unknown> = {};
vi.mock("./ipc", () => ({
  invoke: async (cmd: string, args?: Record<string, unknown>) => {
    calls.push({ cmd, args });
    const r = replies[cmd];
    if (!r) return undefined;
    return r(args);
  },
}));

import { resetToastsForTest, toast } from "./toast";
import {
  IMPORT_MAX_BYTES,
  exportAllProfiles,
  exportOneProfile,
  importProfilesFromFile,
  safeFileStem,
  todayStamp,
} from "./profile-io";
import type { Profile } from "./types";

/** 记下最后一次 saveJsonFile 触发的下载（jsdom 没有真实下载）。 */
let downloaded: { name: string } | null = null;
beforeEach(() => {
  calls.length = 0;
  for (const k of Object.keys(replies)) delete replies[k];
  downloaded = null;
  resetToastsForTest(); // dismiss 分两拍（动效批），同步清不干净
  // Blob URL 在 jsdom 里不存在
  (URL as unknown as { createObjectURL: unknown }).createObjectURL = () => "blob:x";
  (URL as unknown as { revokeObjectURL: unknown }).revokeObjectURL = () => {};
  vi.spyOn(HTMLAnchorElement.prototype, "click").mockImplementation(function (this: HTMLAnchorElement) {
    downloaded = { name: this.download };
  });
});

const profile = (over: Partial<Profile> = {}) =>
  ({ id: "p1", name: "web-01", host: "10.0.0.1", port: 22, username: "root", ...over }) as Profile;

describe("todayStamp / safeFileStem", () => {
  it("日期戳是 YYYY-MM-DD", () => {
    expect(todayStamp(new Date("2026-09-03T15:04:05Z"))).toBe("2026-09-03");
  });

  it("档案名里的路径分隔符与 Windows 保留字符换成下划线", () => {
    expect(safeFileStem('a/b\\c:d*e?f"g<h>i|j')).toBe("a_b_c_d_e_f_g_h_i_j");
  });

  it("全是非法字符或空白 → 回落 profile（空词干会拼出以横杠打头的怪名字）", () => {
    expect(safeFileStem("///")).toBe("___");
    expect(safeFileStem("   ")).toBe("profile");
    expect(safeFileStem("")).toBe("profile");
  });
});

describe("导出", () => {
  it("全量导出：profiles_export 不带 ids，文件名带日期戳", async () => {
    replies.profiles_export = () => "[]";
    await exportAllProfiles();
    expect(calls).toEqual([{ cmd: "profiles_export", args: undefined }]);
    expect(downloaded!.name).toBe(`profiles-${todayStamp()}.json`);
  });

  it("单条导出：ids 只带这一条，文件名取档案名（Rust 侧本就支持子集）", async () => {
    replies.profiles_export = () => "[]";
    await exportOneProfile(profile({ name: "生产/主库" }));
    expect(calls[0]).toEqual({ cmd: "profiles_export", args: { ids: ["p1"] } });
    expect(downloaded!.name).toBe(`生产_主库-${todayStamp()}.json`);
  });

  it("导出失败 → 报错而不是静默（此前 onExportOne 这条路点了没反应）", async () => {
    replies.profiles_export = () => {
      throw new Error("db locked");
    };
    await exportOneProfile(profile());
    expect(get(toast).some((t) => t.level === "error" && t.msg.includes("导出失败"))).toBe(true);
    expect(downloaded, "失败了还下载 = 存下一个空文件").toBeNull();
  });
});

describe("导入", () => {
  /** 造一次 <input type=file> 的 change：拦住 click，拿到那个 input 自己塞文件。 */
  async function pickFile(file: File, reload = vi.fn(async () => {})) {
    let input: HTMLInputElement | null = null;
    const spy = vi.spyOn(HTMLInputElement.prototype, "click").mockImplementation(function (this: HTMLInputElement) {
      input = this;
    });
    importProfilesFromFile(reload);
    spy.mockRestore();
    expect(input, "没拿到 file input").not.toBeNull();
    Object.defineProperty(input!, "files", { value: [file], configurable: true });
    await input!.onchange!({ target: input } as unknown as Event);
    return reload;
  }

  it("正常导入：键名是 content（Tauri 按形参名取实参，键名错了导入从来没能用过）", async () => {
    replies.profiles_import = () => 3;
    const reload = await pickFile(new File(["[]"], "p.json"));
    expect(calls[0]).toEqual({ cmd: "profiles_import", args: { content: "[]" } });
    expect(reload).toHaveBeenCalled();
    expect(get(toast).some((t) => t.msg.includes("共 3 条"))).toBe(true);
  });

  it("超过 8 MiB 当场挡下，不读进内存也不发 IPC（Rust 侧那道闸的纵深防御）", async () => {
    const big = new File(["x"], "p.json");
    Object.defineProperty(big, "size", { value: IMPORT_MAX_BYTES + 1 });
    const reload = await pickFile(big);
    expect(calls).toEqual([]);
    expect(reload).not.toHaveBeenCalled();
    expect(get(toast).some((t) => t.level === "error" && t.msg.includes("8 MiB"))).toBe(true);
  });

  it("后端拒绝 → 报错且不重载列表（try/catch 必须在异步回调**内部**，否则永远捕不到）", async () => {
    replies.profiles_import = () => {
      throw new Error("bad json");
    };
    const reload = await pickFile(new File(["{"], "p.json"));
    expect(reload).not.toHaveBeenCalled();
    expect(get(toast).some((t) => t.level === "error" && t.msg.includes("导入失败"))).toBe(true);
  });

  it("用户按了取消（没选文件）→ 什么都不做", async () => {
    let input: HTMLInputElement | null = null;
    const spy = vi.spyOn(HTMLInputElement.prototype, "click").mockImplementation(function (this: HTMLInputElement) {
      input = this;
    });
    importProfilesFromFile(vi.fn());
    spy.mockRestore();
    Object.defineProperty(input!, "files", { value: [], configurable: true });
    await input!.onchange!({ target: input } as unknown as Event);
    expect(calls).toEqual([]);
  });
});
