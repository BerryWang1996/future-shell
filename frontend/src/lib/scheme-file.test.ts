import { describe, it, expect, vi, afterEach } from "vitest";
import {
  NATIVE_SCHEME_FORMAT,
  NATIVE_SCHEME_VERSION,
  downloadJson,
  exportFileName,
  serializeSchemes,
  toExportFile,
} from "./scheme-file";
import type { StoredCustomScheme } from "./term-schemes";
// Rust 侧的导出格式定义。同 settings-wiring.test.ts 的既有口径（?raw + glob）。
import IMPORT_CMD_RS from "../../../app/src/commands/import_cmd.rs?raw";

const s = (over: Partial<StoredCustomScheme> = {}): StoredCustomScheme => ({
  name: "MyDark",
  foreground: "#cccccc",
  background: "#1e1e2e",
  // 16 色各不相同：顺序被弄反时才测得出来（`.xcs` 那条路径上真出过这种事）
  ansi: Array.from({ length: 16 }, (_, i) =>
    `#${(i * 16).toString(16).padStart(2, "0")}${i.toString(16).padStart(2, "0")}${(255 - i * 16).toString(16).padStart(2, "0")}`,
  ),
  cursor: "#f5e0dc",
  ...over,
});

/* ───────────── 跨语言：格式标识必须两边一致 ───────────── */

describe("导出格式与 Rust 侧同源", () => {
  /**
   * 这条是这条出口最容易悄悄失效的地方。
   *
   * 两边的 format 串或 version 走散时，症状是**导出的文件自己读不回来**——
   * 而「JSON 导出 round-trip 一致」正是出口的字面判据。前端单测再多也测不出来，
   * 因为前端自己序列化、自己反序列化永远是一致的。
   */
  it("format 标识与版本号逐字相等", () => {
    const fmt = /NATIVE_SCHEME_FORMAT: &str = "([^"]+)"/.exec(IMPORT_CMD_RS);
    const ver = /NATIVE_SCHEME_VERSION: u32 = (\d+)/.exec(IMPORT_CMD_RS);
    // 抓不到就是守卫失效（Rust 侧改了写法），要响亮失败而不是跳过
    expect(fmt, "在 import_cmd.rs 里找不到 NATIVE_SCHEME_FORMAT").toBeTruthy();
    expect(ver, "在 import_cmd.rs 里找不到 NATIVE_SCHEME_VERSION").toBeTruthy();
    expect(fmt![1]).toBe(NATIVE_SCHEME_FORMAT);
    expect(Number(ver![1])).toBe(NATIVE_SCHEME_VERSION);
  });

  it("Rust 侧确实按 format 字段判格式（不是认任何 JSON）", () => {
    // 没有这道闸的话，「认自家格式」会退化成「认任何 JSON」，
    // 而它在尝试顺序里排第一，会把 FinalShell 的配置全吃掉。
    expect(IMPORT_CMD_RS).toMatch(/f\.format != NATIVE_SCHEME_FORMAT/);
  });
});

/* ───────────── round-trip ───────────── */

describe("导出结构", () => {
  it("带 format 与 version（缺了后端认不出来）", () => {
    const f = toExportFile([s()]);
    expect(f.format).toBe(NATIVE_SCHEME_FORMAT);
    expect(f.version).toBe(NATIVE_SCHEME_VERSION);
  });

  it("前端自身 round-trip：序列化再解析回来逐字段相等", () => {
    const list = [s(), s({ name: "另一套", cursor: null })];
    const back = JSON.parse(serializeSchemes(list)) as { schemes: StoredCustomScheme[] };
    expect(back.schemes).toEqual(toExportFile(list).schemes);
    // ansi 顺序原样——逐项比，不是只看长度
    expect(back.schemes[0].ansi).toEqual(list[0].ansi);
    expect(back.schemes[0].ansi[0]).not.toBe(back.schemes[0].ansi[15]);
  });

  it("cursor 缺省写成 null 而不是省略字段（导出应当幂等）", () => {
    // Rust 的 Option<String> 两种都读得了，但省略会让同一套配色的两次导出产生
    // 不同的字节——「导出→导入→再导出」就不是幂等的了。
    const f = toExportFile([s({ cursor: undefined }), s({ name: "B", cursor: null })]);
    expect(f.schemes[0].cursor).toBeNull();
    expect(f.schemes[1].cursor).toBeNull();
    expect(JSON.stringify(f)).toContain('"cursor":null');
  });

  it("导出的是副本，改它不会动到原对象（ansi 是同一个数组的话，编辑器一改导出内容跟着变）", () => {
    const orig = s();
    const f = toExportFile([orig]);
    f.schemes[0].ansi[0] = "#ffffff";
    expect(orig.ansi[0]).not.toBe("#ffffff");
  });

  it("缩进 2 空格：导出的配色是给人看、给人手改的", () => {
    expect(serializeSchemes([s()])).toContain('\n  "format"');
  });
});

/* ───────────── 文件名 ───────────── */

describe("导出文件名", () => {
  it("单套用配色自己的名字，多套用带日期的通用名", () => {
    expect(exportFileName([s({ name: "Dracula" })], "2026-08-23")).toBe("Dracula.json");
    expect(exportFileName([s(), s({ name: "B" })], "2026-08-23")).toBe(
      "color-schemes-2026-08-23.json",
    );
  });

  it("配色名里的路径分隔符与保留字符被剔掉", () => {
    // 名字是用户起的。`../` 进了 download 属性会得到一个意外的落点，
    // `:` 与 `?` 在 Windows 上直接让下载失败。
    for (const [name, want] of [
      // `..` 本身不构成遍历——路径分隔符没了它就只是两个点。留着比再造一套
      // 「点号也要处理」的规则简单，且用户还认得出这原本叫什么。
      ["../../etc/passwd", ".._.._etc_passwd.json"],
      ["a/b", "a_b.json"],
      ["C:\\x", "C__x.json"],
      ['a"b<c>d|e?f*g', "a_b_c_d_e_f_g.json"],
    ] as const) {
      const got = exportFileName([s({ name })], "2026-08-23");
      expect(got).toBe(want);
      // 无论怎么替换，成品里都不许再有分隔符
      expect(got).not.toMatch(/[\\/]/);
    }
  });

  it("剩不下能认的字符时用兜底名，而不是一串下划线", () => {
    // `___.json` 是个合法文件名，但用户在下载目录里认不出那是什么。
    expect(exportFileName([s({ name: "///" })], "2026-08-23")).toBe("配色.json");
    expect(exportFileName([s({ name: "   " })], "2026-08-23")).toBe("配色.json");
    expect(exportFileName([s({ name: "..." })], "2026-08-23")).toBe("配色.json");
    // 反向对照：只要有一个认得出的字符就保留原名
    expect(exportFileName([s({ name: "/a/" })], "2026-08-23")).toBe("_a_.json");
  });
});

/* ───────────── 存盘 ───────────── */

describe("触发下载", () => {
  afterEach(() => vi.restoreAllMocks());

  it("建了 blob URL 就必须释放（每导一次泄漏一个，长会话里会攒起来）", () => {
    const created: string[] = [];
    const revoked: string[] = [];
    vi.stubGlobal("URL", {
      ...URL,
      createObjectURL: () => {
        const u = `blob:${created.length}`;
        created.push(u);
        return u;
      },
      revokeObjectURL: (u: string) => revoked.push(u),
    });
    const clicks: { href: string; download: string }[] = [];
    const real = document.createElement.bind(document);
    vi.spyOn(document, "createElement").mockImplementation(((tag: string) => {
      const el = real(tag);
      if (tag === "a") {
        (el as HTMLAnchorElement).click = () => {
          clicks.push({
            href: (el as HTMLAnchorElement).href,
            download: (el as HTMLAnchorElement).download,
          });
        };
      }
      return el;
    }) as typeof document.createElement);

    downloadJson('{"a":1}', "x.json");
    expect(clicks).toEqual([{ href: "blob:0", download: "x.json" }]);
    expect(revoked).toEqual(created);
    vi.unstubAllGlobals();
  });
});
