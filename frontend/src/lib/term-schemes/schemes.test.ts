import { describe, expect, it } from "vitest";
import {
  DEFAULT_SCHEME, SCHEMES, getScheme, resolveScheme, schemeToXtermTheme, validateScheme,
} from "./index";

describe("term schemes（UI 规格 §3.2）", () => {
  it("自带 ≥10 套且全部通过 schema 校验", () => {
    expect(SCHEMES.length).toBeGreaterThanOrEqual(10);
    for (const s of SCHEMES) expect(validateScheme(s)).toBe(true);
  });
  it("规格清单 12 套齐全", () => {
    const ids = SCHEMES.map((s) => s.id).sort();
    expect(ids).toEqual([
      "dracula", "futureshell-dark", "gruvbox-dark", "monokai-pro", "nord", "one-dark",
      "solarized-dark", "solarized-light", "tomorrow-night", "ubuntu",
      "windows-terminal-light", "xshell-standard",
    ]);
  });
  it("默认方案 = FutureShell Dark", () => {
    expect(DEFAULT_SCHEME.id).toBe("futureshell-dark");
  });
  it("映射到 xterm theme 选项（§3.2）：ansi→命名字段，selection→selectionBackground", () => {
    const t = schemeToXtermTheme(DEFAULT_SCHEME);
    expect(t.background).toBe("#0f1115");
    expect(t.foreground).toBe("#d7dbe2");
    expect(t.cursor).toBe("#4f8cff");
    expect(t.selectionBackground).toBe("#2b3a55");
    expect(t.black).toBe("#1f232b");
    expect(t.red).toBe("#ff5f56");
    expect(t.brightWhite).toBe("#ffffff");
  });
  it("validateScheme 拒绝残缺/非法数据", () => {
    expect(validateScheme({ ...DEFAULT_SCHEME, ansi: DEFAULT_SCHEME.ansi.slice(0, 15) })).toBe(false);
    expect(validateScheme({ ...DEFAULT_SCHEME, background: "red" })).toBe(false);
    expect(validateScheme({ ...DEFAULT_SCHEME, boldBright: "yes" })).toBe(false);
    expect(validateScheme(null)).toBe(false);
  });
  it("三级作用域优先级：临时 > Profile > 全局 > 默认", () => {
    const g = getScheme("nord")!;
    const p = getScheme("dracula")!;
    const t = getScheme("solarized-light")!;
    expect(resolveScheme(g, p, t).id).toBe("solarized-light");
    expect(resolveScheme(g, p, null).id).toBe("dracula");
    expect(resolveScheme(g, null, null).id).toBe("nord");
    expect(resolveScheme(null, null, null).id).toBe("futureshell-dark");
  });
});
