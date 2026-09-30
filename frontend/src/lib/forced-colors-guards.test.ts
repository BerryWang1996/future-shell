/**
 * Windows 高对比度（forced-colors）结构守卫（路线图 4c，2026-09-02）。
 *
 * 真机判据是 `scripts/forced-colors-audit.mjs`——它把每个状态构件的各语义状态在强制颜色下的
 * 计算样式两两比对，还查「画得出来吗」。这里守的是让那次核查的结论**不随后续改动失效**的四条结构：
 *
 *  ① **状态集不得漂移**：常态里有几个 `.lamp.X` / `.status.X` / `.dot.X`，forced-colors 块里就得有几个。
 *     新增一个状态却忘了补高对比度规则，是这一层最可能的退化方式，且没有任何别的测试会为它变红。
 *  ② **填充不得用 currentColor**：实测 `background: currentColor` 被引擎强制成 Canvas，点就画不出来了，
 *     而两两比对还会因为圆角不同判它「可辨」。只有系统色关键字（CanvasText / GrayText）留得住。
 *     这条最容易被后人当成「统一用 currentColor 更整洁」顺手改掉。
 *  ③ **「选中/强调」用内嵌 outline，不用 `background: Highlight`**：后者会把里面那些 CanvasText
 *     填充的指示点放到没人保证过对比度的高亮底上。
 *  ④ 房规文档还在 styles.css 里（解释「为什么是形状不是颜色」的那段）。
 */
import fs from "node:fs";
import path from "node:path";
import { describe, expect, it } from "vitest";

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

const read = (rel: string) => fs.readFileSync(path.join(SRC, rel), "utf8");
/** 组件 `<style>` 段，去注释。 */
const styleOf = (src: string) =>
  (/<style[^>]*>([\s\S]*?)<\/style>/.exec(src)?.[1] ?? "").replace(/\/\*[\s\S]*?\*\//g, "");
/** `@media (forced-colors: active) { … }` 块体（按花括号配平取，块内还有嵌套规则）。 */
function forcedBlock(css: string): string | null {
  const head = /@media \(forced-colors: active\)\s*\{/.exec(css);
  if (!head) return null;
  let depth = 0;
  for (let i = head.index + head[0].length - 1; i < css.length; i++) {
    if (css[i] === "{") depth++;
    else if (css[i] === "}") {
      depth--;
      if (depth === 0) return css.slice(head.index + head[0].length, i);
    }
  }
  return null;
}
/** 某个基类下出现过的修饰类集合，例如 `.lamp.connected` → "connected"。 */
const modifiers = (css: string, base: string) =>
  [...css.matchAll(new RegExp(`\\.${base}\\.([\\w-]+)`, "g"))].map((m) => m[1]).sort();

/** 状态构件：文件、基类、以及「常态里出现但不属于状态」的修饰类（不要求高对比度覆盖）。 */
const WIDGETS = [
  { file: "components/Sidebar.svelte", base: "lamp", exempt: [] as string[] },
  { file: "components/TabBar.svelte", base: "status", exempt: [] as string[] },
  { file: "components/StatusBar.svelte", base: "dot", exempt: [] as string[] },
];

const SOURCES = lazy(() => Object.fromEntries(WIDGETS.map((w) => [w.file, styleOf(read(w.file))])));

describe("① 状态构件的高对比度规则不得漏项", () => {
  it("非空证明：三个构件都有 forced-colors 块", () => {
    for (const w of WIDGETS) {
      expect(forcedBlock(SOURCES()[w.file]), `${w.file} 没有 @media (forced-colors: active) 块`).not.toBeNull();
    }
  });

  it.each(WIDGETS)("$file 的 .$base 状态集：常态有几个，强制颜色下就得有几个", ({ file, base, exempt }) => {
    const css = SOURCES()[file];
    const block = forcedBlock(css)!;
    const all = new Set(modifiers(css, base));
    const covered = new Set(modifiers(block, base));
    const normal = [...all].filter((m) => !covered.has(m) && !exempt.includes(m));
    expect(normal.length, `${base} 的状态一个都没解析到，判据失效`).toBeLessThan(all.size);
    expect(
      normal,
      `这些状态没有高对比度规则：强制颜色下它们只剩底色，会与别的状态塌成一个样子`,
    ).toEqual([]);
  });
});

describe("② 填充用系统色关键字，不用 currentColor", () => {
  it("三个构件的 forced-colors 块里没有 background: currentColor", () => {
    const bad: string[] = [];
    for (const w of WIDGETS) {
      const block = forcedBlock(SOURCES()[w.file]) ?? "";
      if (/background:\s*currentColor/i.test(block)) bad.push(w.file);
    }
    expect(bad, "实测 background: currentColor 会被强制成 Canvas——那是一颗画不出来的点；改用 CanvasText / GrayText").toEqual([]);
  });

  it("非空证明：确实用了系统色关键字填充", () => {
    const n = WIDGETS.reduce(
      (acc, w) => acc + ((forcedBlock(SOURCES()[w.file]) ?? "").match(/background:\s*(CanvasText|GrayText)\b/g)?.length ?? 0),
      0,
    );
    expect(n).toBeGreaterThanOrEqual(6);
  });
});

describe("③ 选中/强调用内嵌 outline", () => {
  const SELECTED = [
    { file: "components/Sidebar.svelte", sel: ".leaf.active" },
    { file: "components/TabBar.svelte", sel: ".tab.active" },
    { file: "components/ToolBar.svelte", sel: ".tbtn.primary" },
  ];

  it.each(SELECTED)("$file 的 $sel 在强制颜色下有 outline，且不靠 background: Highlight", ({ file, sel }) => {
    const block = forcedBlock(styleOf(read(file)));
    expect(block, `${file} 没有 forced-colors 块`).not.toBeNull();
    const rule = new RegExp(`${sel.replace(/\./g, "\\.")}[^{}]*\\{([^}]*)\\}`).exec(block!);
    expect(rule, `${sel} 在 forced-colors 块里没有规则`).not.toBeNull();
    expect(rule![1]).toMatch(/outline:\s*2px solid Highlight/);
    expect(rule![1], "background: Highlight 会把里面 CanvasText 填充的指示点放到没保证过对比度的底上").not.toMatch(/background:\s*Highlight/);
  });
});

describe("④ 房规文档在位", () => {
  it("styles.css 里写着「为什么是形状不是颜色」与 currentColor 的实测结论", () => {
    const css = fs.readFileSync(path.join(SRC, "styles.css"), "utf8");
    expect(css).toMatch(/forced-colors: active/);
    expect(css, "房规里要留着 currentColor 被强制成 Canvas 这条实测结论——它是②那条守卫的理由").toMatch(/currentColor[\s\S]{0,120}Canvas/);
    expect(css).toMatch(/outline: 2px solid Highlight/);
  });

  it("断线横幅：动态核查量不到它（要真实会话才挂载），故在此守它的两态区分", () => {
    const block = forcedBlock(styleOf(read("components/TerminalPane.svelte")));
    expect(block, "TerminalPane 没有 forced-colors 块").not.toBeNull();
    const rule = /\.disconnect-banner\.gave-up[^{}]*\{([^}]*)\}/.exec(block!);
    expect(rule, "「已放弃重连」在强制颜色下与「重连中」只剩同宽左条，需要一个非颜色信号").not.toBeNull();
    expect(rule![1]).toMatch(/border-left-width|border-left-style/);
  });
});
