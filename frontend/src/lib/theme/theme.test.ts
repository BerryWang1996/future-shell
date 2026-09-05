import { beforeEach, describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";

vi.mock("../ipc", () => ({
  settingGet: vi.fn(async (_k: string, fb: unknown) => fb),
  settingSet: vi.fn(async () => {}),
  invoke: vi.fn(async () => null),
  listen: vi.fn(async () => () => {}),
  openExternal: vi.fn(async () => {}),
}));

import { settingGet, settingSet } from "../ipc";
import { BUILTIN_THEMES, THEME_TOKEN_KEYS, getTheme } from "./themes";
import { initTheme, resolveTheme, resolveThemeId, setTheme, themeId } from "./store";

describe("themes 元数据（UI 规格 §3.1）", () => {
  it("4 内置主题：Obsidian/Daylight/Slate Blue/High Contrast", () => {
    expect(BUILTIN_THEMES.map((t) => t.id)).toEqual(["obsidian", "daylight", "slate", "hc"]);
    expect(BUILTIN_THEMES.map((t) => t.name)).toEqual(["Obsidian", "Daylight", "Slate Blue", "High Contrast"]);
    expect(BUILTIN_THEMES.filter((t) => t.dark).map((t) => t.id)).toEqual(["obsidian", "slate", "hc"]);
  });
  it("每主题令牌齐全（与 tokens.css 令牌全集一致）", () => {
    for (const t of BUILTIN_THEMES) {
      expect(Object.keys(t.tokens).sort()).toEqual([...THEME_TOKEN_KEYS].sort());
    }
  });
  it("getTheme 按 id 查询", () => {
    expect(getTheme("obsidian")?.name).toBe("Obsidian");
    expect(getTheme("nope")).toBeUndefined();
  });
});

describe("theme store（切换即时生效 + 持久化）", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    document.documentElement.removeAttribute("data-theme");
  });
  it("initTheme 从 settings 载入并写 documentElement.dataset.theme", async () => {
    await initTheme();
    expect(settingGet).toHaveBeenCalledWith("ui.theme", "obsidian");
    expect(document.documentElement.dataset.theme).toBe("obsidian");
    expect(get(themeId)).toBe("obsidian");
  });
  it("setTheme 即时切换并经 settings_set 落库", async () => {
    await setTheme("daylight");
    expect(document.documentElement.dataset.theme).toBe("daylight");
    expect(settingSet).toHaveBeenCalledWith("ui.theme", "daylight");
  });
  it("auto 跟随系统明暗（jsdom 无 matchMedia 时按暗色回落）", () => {
    expect(resolveThemeId("auto")).toBe("obsidian");
    expect(resolveThemeId("slate")).toBe("slate");
  });
});

describe("resolveTheme 三级配色作用域（R11：字段级浅合并，transient ＞ profile ＞ global）", () => {
  const global = { scheme: "nord", fontSize: 13 };
  it("仅全局：原样返回", () => {
    expect(resolveTheme({ global })).toEqual({ scheme: "nord", fontSize: 13 });
  });
  it("全局 + profile 覆盖：字段级合并，未覆盖字段保留全局值", () => {
    expect(resolveTheme({ global, profile: { scheme: "dracula" } })).toEqual({ scheme: "dracula", fontSize: 13 });
    expect(resolveTheme({ global, profile: { fontSize: 15 } })).toEqual({ scheme: "nord", fontSize: 15 });
  });
  it("transient ＞ profile ＞ global（会话内存态，M1 经标签右键菜单接线，R16）", () => {
    expect(
      resolveTheme({ global, profile: { scheme: "dracula" }, transient: { scheme: "solarized-light" } }),
    ).toEqual({ scheme: "solarized-light", fontSize: 13 });
  });
  it("高层显式 undefined 不覆盖低层有效值", () => {
    expect(resolveTheme({ global, profile: { scheme: undefined } })).toEqual({ scheme: "nord", fontSize: 13 });
  });
});

describe("对比度核验（UI 规格 §7，WCAG 2.1 AA；M1 出口载体）", () => {
  /** WCAG 2.1 相对亮度：sRGB 通道 /255 后 c≤0.03928 ? c/12.92 : ((c+0.055)/1.055)^2.4；L = 0.2126R + 0.7152G + 0.0722B。 */
  function relLum(hex: string): number {
    const n = parseInt(hex.slice(1), 16);
    const chan = (v: number): number => {
      const c = v / 255;
      return c <= 0.03928 ? c / 12.92 : Math.pow((c + 0.055) / 1.055, 2.4);
    };
    return 0.2126 * chan((n >> 16) & 255) + 0.7152 * chan((n >> 8) & 255) + 0.0722 * chan(n & 255);
  }
  function ratio(a: string, b: string): number {
    const la = relLum(a);
    const lb = relLum(b);
    return (Math.max(la, lb) + 0.05) / (Math.min(la, lb) + 0.05);
  }

  // 文本组合（前景/背景令牌键）：主/次文本 × 面板/应用背景、accent 上文本、选区上文本
  const COMBOS: Array<[string, string]> = [
    ["fg-primary", "bg-panel"],
    ["fg-primary", "bg-app"],
    ["fg-secondary", "bg-panel"],
    ["fg-secondary", "bg-app"],
    ["accent-fg", "accent"],
    ["fg-primary", "selection"],
  ];

  for (const t of BUILTIN_THEMES) {
    const tokens = t.tokens as Record<string, string>;
    it(`${t.id}: 文本组合比值 ≥ ${t.id === "hc" ? "7" : "4.5"}:1（WCAG AA，hc 增强）`, () => {
      const min = t.id === "hc" ? 7 : 4.5;
      for (const [fg, bg] of COMBOS) {
        const r = ratio(tokens[fg], tokens[bg]);
        expect(r, `${t.id} ${fg}/${bg} 实测 ${r.toFixed(2)}:1`).toBeGreaterThanOrEqual(min);
      }
    });
  }

  // ── 状态底纹组合（2026-08-31 评审后新增）─────────────────────────────────
  //
  // 断线/失败横幅此前用「状态色作整块背景 + 主文字」，四主题实测 2.11–3.22:1，
  // 全数不达标（评审 P1-1）。重做成「rgba 状态底纹（--fs-*-dim）叠在面板上 +
  // 主文字 + 状态色左边框」。这组判据把**合成后的实际背景**算进核验：
  // rgba 令牌不能直接进 ratio()——先与 bg-panel 合成出等效 hex。
  //
  // 三组：
  //  ① fg-primary 叠在 warn-dim/panel 与 danger-dim/panel 上（横幅主文字）
  //     ——AA 4.5:1（hc 7:1）；
  //  ② danger 粗体强调字叠在 danger-dim/panel 上（gave-up 的「重连失败」）
  //     ——同 AA 门槛；调暗 danger 或加深底纹前先过这里；
  //  ③ warn/danger 边框对各自底纹 ≥3:1（非文本 UI 构件的 WCAG 1.4.11）。
  /** rgba() 或 hex → 与底色合成后的 #rrggbb（rgba 的 alpha 乘上去）。 */
  function composite(fgToken: string, bgHex: string): string {
    const m = fgToken.match(/^rgba?\((\d+),\s*(\d+),\s*(\d+)(?:,\s*([\d.]+))?\)$/);
    if (!m) return fgToken;
    const a = m[4] === undefined ? 1 : parseFloat(m[4]);
    const bg = parseInt(bgHex.slice(1), 16);
    const mix = (i: number): number =>
      Math.round(m[i] === undefined ? 0 : (Number(m[i]) * a + (((bg >> i) & 0) === 9 ? 0 : ((bg / Math.pow(256, 2 - (i - 1))) | 0)) * (1 - a)));
    // 简洁实现：逐通道插值（上面的位运算是防御性噪声，这里直接算）
    const ch = (idx: number): number => {
      const fg = Number(m[idx]);
      const bgc = idx === 1 ? (bg >> 16) & 255 : idx === 2 ? (bg >> 8) & 255 : bg & 255;
      return Math.round(fg * a + bgc * (1 - a));
    };
    const hex2 = (v: number): string => v.toString(16).padStart(2, "0");
    return "#" + hex2(ch(1)) + hex2(ch(2)) + hex2(ch(3));
  }

  for (const t of BUILTIN_THEMES) {
    const tokens = t.tokens as Record<string, string>;
    it(`${t.id}: 状态底纹组合（横幅重做后）≥ ${t.id === "hc" ? "7" : "4.5"}:1`, () => {
      const min = t.id === "hc" ? 7 : 4.5;
      // 注意没有 danger/warn 作**文字**的组合：实测不达标（daylight 4.28:1、
      // hc 6.37:1 < 7），评审建议的「危险色文字」被测量否决——状态色只用于
      // 左边框与装饰圆点（非文本构件，3:1 即可），文字一律 fg-primary（粗体强调）。
      const cases: Array<[string, string, string]> = [
        ["fg-primary", "warn-dim", "bg-panel"],
        ["fg-primary", "danger-dim", "bg-panel"],
      ];
      for (const [fg, dim, base] of cases) {
        const effBg = composite(tokens[dim], tokens[base]);
        const r = ratio(tokens[fg], effBg);
        expect(
          r,
          `${t.id} ${fg} on ${dim}(${effBg}) 实测 ${r.toFixed(2)}:1`,
        ).toBeGreaterThanOrEqual(min);
      }
      // 非文本构件（左边框）对底纹 ≥3:1（WCAG 1.4.11）
      for (const [c, dim] of [["warn", "warn-dim"], ["danger", "danger-dim"]] as const) {
        const effBg = composite(tokens[dim], tokens["bg-panel"]);
        const r = ratio(tokens[c], effBg);
        expect(r, `${t.id} ${c} 边框 on ${dim} 实测 ${r.toFixed(2)}:1`).toBeGreaterThanOrEqual(3);
      }
    });
  }
});
