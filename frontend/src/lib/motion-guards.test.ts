/**
 * 动效结构守卫（路线图 4c「动效补齐」，2026-09-02）。
 *
 * 真机上的动效是否生效由 `scripts/motion-audit.mjs` 对着 WebView2 量（transition 时长、@starting-style
 * 支持、prefers-reduced-motion 仿真下清零）。这里守的是让那次核查的结论**不随后续改动失效**的几条结构：
 *
 *  ① 全局动效表存在：token（--fs-motion-*）+ 每个进场对象既有 transition 规则也有 @starting-style 起始态。
 *  ② prefers-reduced-motion 的兜底仍在，且同时清零 transition 与 animation。
 *  ③ 不绕过兜底：组件模板零 Svelte 过渡指令（transition:/in:/out:/animate:），全仓零 `.animate(`——
 *     它们是 JS 驱动的，CSS 兜底管不到。
 *  ④ 有限动效 ≤ 250ms（无限循环的状态指示灯除外）：终端不是营销页。
 *  ⑤ toast 退场：组件按 leaving 播退场，store 侧 dismiss 分两拍且看 prefers-reduced-motion。
 */
import fs from "node:fs";
import path from "node:path";
import { describe, expect, it } from "vitest";
import TOAST_SVELTE from "../components/Toast.svelte?raw";
import TOAST_TS from "./toast.ts?raw";

const SRC = path.resolve(process.cwd(), "src");

function lazy<T>(f: () => T): () => T {
  let v: T;
  let done = false;
  return () => {
    if (!done) {
      v = f();
      done = true;
    }
    return v;
  };
}

function walk(dir: string, out: string[] = []): string[] {
  for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
    const p = path.join(dir, e.name);
    if (e.isDirectory()) walk(p, out);
    else if (/\.(svelte|ts)$/.test(e.name) && !/\.test\.ts$/.test(e.name)) out.push(p);
  }
  return out;
}

/** 组件源码 → 模板部分（去掉 <script> 与 <style>）；过渡指令只可能出现在模板里。 */
const templateOf = (src: string) => src.replace(/<script[\s\S]*?<\/script>/g, "").replace(/<style[\s\S]*?<\/style>/g, "");
const styleOf = (src: string) => /<style[^>]*>([\s\S]*?)<\/style>/.exec(src)?.[1] ?? "";

/** Svelte 过渡指令：`transition:fade` / `in:fly={…}` / `out:slide` / `animate:flip`。CSS 的 `transition: left 120ms` 是冒号后有空格且后面不是标识符接 =/空白/>，不命中。 */
const DIRECTIVE = /(?:^|\s)(?:in|out|transition|animate):[A-Za-z_$][\w$]*(?=[\s=>/])/;

/** styles.css 全文。不用 `?raw`：vitest 对 .css 导入默认返回空串，首版就是这样拿到 "" 还全绿了一半。 */
const STYLES = lazy(() => fs.readFileSync(path.resolve(process.cwd(), "src/styles.css"), "utf8"));

const SOURCES = lazy(() => walk(SRC).map((f) => [path.relative(SRC, f), fs.readFileSync(f, "utf8")] as const));

/** 进场对象：既要有 transition 规则（@starting-style 之外），也要有起始态（@starting-style 之内）。
 *  标签 [role="tab"] 不在此列——它是状态过渡（底色/指示），不是进场，见下面单独一条。 */
const MOTION_SELECTORS = [
  '[role="dialog"]',
  ':has(> [role="dialog"])',
  '[role="menu"]',
  ".term-wrapper.active",
  '[data-testid="monitor"]',
  '[data-testid="composebar"]',
];

/** 一段 CSS 里是否有一条规则的选择器**恰好**是 sel（`:has(...)` 那条允许带前缀，如 `.overlay:has(...)`）。
 *  不用 includes：`.overlay:has(> [role="dialog"])` 这一行会让 `[role="dialog"]` 的 includes 恒真——
 *  首版守卫就是这样让「删掉对话框起始态」的变异幸存的。 */
function hasRuleFor(css: string, sel: string): boolean {
  const re = /([^{}]+)\{([^{}]*)\}/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(css))) {
    const selectors = m[1].split(",").map((s) => s.trim());
    if (sel.startsWith(":has") ? selectors.some((s) => s.includes(sel)) : selectors.includes(sel)) return true;
  }
  return false;
}

describe("① 全局动效表（styles.css）", () => {
  const startingBlock = () => /@starting-style\s*\{([\s\S]*?)\n\}/.exec(STYLES())?.[1] ?? null;
  const outsideStarting = () => STYLES().replace(/@starting-style\s*\{[\s\S]*?\n\}/, "");

  it("动效 token 存在且 ≤ 200ms（终端不是营销页）", () => {
    const tokens = [...STYLES().matchAll(/--fs-motion-(\w+):\s*(\d+)ms/g)];
    expect(tokens.map((m) => m[1]).sort()).toEqual(["base", "fast"]);
    for (const m of tokens) expect(Number(m[2]), `--fs-motion-${m[1]}`).toBeLessThanOrEqual(200);
    expect(STYLES()).toMatch(/--fs-ease-out:\s*cubic-bezier/);
  });

  it("每个进场对象既有 transition 规则，也有 @starting-style 起始态（少一半就是没动效或直接跳变）", () => {
    const start = startingBlock();
    expect(start, "styles.css 里没有 @starting-style 块").not.toBeNull();
    const outside = outsideStarting().replace(/\/\*[\s\S]*?\*\//g, "");
    for (const sel of MOTION_SELECTORS) {
      expect(hasRuleFor(outside, sel), `${sel} 缺 transition 规则`).toBe(true);
      expect(hasRuleFor(start!, sel), `@starting-style 缺 ${sel} 的起始态`).toBe(true);
    }
  });

  it("标签 [role=tab]：底色 / 前景 / 选中指示的状态过渡（会话标签、设置页与会话属性的页签共用）", () => {
    const rule = /\[role="tab"\]\s*\{([^}]*)\}/.exec(outsideStarting());
    expect(rule, "缺 [role=\"tab\"] 规则").not.toBeNull();
    expect(rule![1]).toMatch(/transition:[^;]*background-color/);
    expect(rule![1]).toMatch(/box-shadow/);
  });

  it("菜单只动 opacity：clampToViewport 用 translate 钳位，transform 过渡会跟它打架", () => {
    const rule = /\[role="menu"\]\s*\{([^}]*)\}/.exec(outsideStarting());
    expect(rule).not.toBeNull();
    expect(rule![1]).toMatch(/transition:\s*opacity/);
    expect(rule![1]).not.toMatch(/transform/);
  });
});

describe("② prefers-reduced-motion 兜底", () => {
  it("仍在，且同时清零 transition 与 animation（带 !important）", () => {
    expect(STYLES()).toMatch(/@media \(prefers-reduced-motion: reduce\)\s*\{\s*\*\s*\{[^}]*transition:\s*none !important;[^}]*animation:\s*none !important;/);
  });
});

describe("③ 不绕过兜底：零 Svelte 过渡指令、零 Web Animations", () => {
  it("判据对正反样本判得对（否则下面两条是在守空）", () => {
    expect(DIRECTIVE.test('<div transition:fade={{ duration: 100 }}>')).toBe(true);
    expect(DIRECTIVE.test("<li in:fly out:fade>")).toBe(true);
    expect(DIRECTIVE.test("<div animate:flip>")).toBe(true);
    expect(DIRECTIVE.test("  transition: left 120ms ease-out;")).toBe(false);
    expect(DIRECTIVE.test(".x { transition: opacity var(--fs-motion-fast); }")).toBe(false);
  });

  it("组件模板里没有 transition:/in:/out:/animate: 指令", () => {
    const hits = SOURCES()
      .filter(([rel]) => rel.endsWith(".svelte"))
      .filter(([, src]) => DIRECTIVE.test(templateOf(src)))
      .map(([rel]) => rel);
    expect(hits, "Svelte 过渡指令是 JS 驱动的，styles.css 的 prefers-reduced-motion 兜底管不到；改用 CSS + @starting-style").toEqual([]);
  });

  it("全仓没有 .animate( 调用（Web Animations API 同理）", () => {
    const hits = SOURCES().filter(([, src]) => /\.animate\(/.test(src)).map(([rel]) => rel);
    expect(hits).toEqual([]);
    expect(SOURCES().length, "非空证明：扫到的源文件数").toBeGreaterThanOrEqual(40);
  });
});

describe("④ 组件里的有限动效 ≤ 250ms", () => {
  it("每条 animation:/transition: 声明（无限循环的指示灯除外）时长不超过 250ms", () => {
    const over: string[] = [];
    let seen = 0;
    for (const [rel, src] of SOURCES()) {
      if (!rel.endsWith(".svelte")) continue;
      const css = styleOf(src).replace(/\/\*[\s\S]*?\*\//g, "");
      for (const m of css.matchAll(/(?:^|[;{\s])(animation|transition):\s*([^;}]+)/g)) {
        const decl = m[2];
        if (/\binfinite\b/.test(decl) || /\bnone\b/.test(decl)) continue;
        seen++;
        for (const d of decl.matchAll(/(\d*\.?\d+)(ms|s)\b/g)) {
          const ms = d[2] === "s" ? Number(d[1]) * 1000 : Number(d[1]);
          if (ms > 250) over.push(`${rel}: ${m[1]}: ${decl.trim()}`);
        }
      }
    }
    expect(seen, "非空证明：扫到的有限动效声明数").toBeGreaterThanOrEqual(5);
    expect(over).toEqual([]);
  });
});

describe("⑤ toast 退场", () => {
  it("Toast.svelte 按 leaving 播退场，退场规则只动 opacity/transform 且走 token", () => {
    expect(TOAST_SVELTE).toMatch(/class:leaving=\{t\.leaving\}/);
    const rule = /\.toast\.leaving\s*\{([^}]*)\}/.exec(TOAST_SVELTE);
    expect(rule, "缺 .toast.leaving 规则").not.toBeNull();
    expect(rule![1]).toMatch(/opacity:\s*0/);
    expect(rule![1]).toMatch(/transition:[^;]*var\(--fs-motion-base/);
  });

  it("toast.ts 的 dismiss 分两拍，且退场那一拍看 prefers-reduced-motion", () => {
    expect(TOAST_TS).toMatch(/import \{ prefersReducedMotion \} from "\.\/motion"/);
    expect(TOAST_TS).toMatch(/if \(prefersReducedMotion\(\)\) remove\(\);/);
    expect(TOAST_TS).toMatch(/setTimeout\(remove, TOAST_EXIT_MS\)/);
  });
});
