import { describe, it, expect } from "vitest";
import { readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";

/**
 * 下拉箭头不可被组件盒样式静默误伤（2026-09-01 对抗审计报出后立的守卫）。
 *
 * # 这条门禁存在的理由
 *
 * 全局给 select 自绘了下拉箭头（styles.css，background-image）。而 CSS 的
 * `background:` **简写**会把 background-image 一并重置为 none——组件里任何一处
 * `select { background: ... }` 都会让箭头**静默消失**：没有报错、没有类型错误、
 * 没有测试转红，只是那个下拉看起来不像下拉。
 *
 * 这个坑实际发生过：上一版加箭头时，8 个组件的 select 全部没有箭头，用户报
 * 「这个下拉没有样式」。人工评审看不住它——要同时想到「这条规则命中 select」
 * 与「简写会清 background-image」两件事。
 *
 * 现在全局那三条加了 !important 守住，本门禁再加一道：**组件里给 select 写
 * background 简写**时提醒改用 background-color。两道一起，才不至于哪天有人
 * 把 !important 删了又悄悄回到原样。
 */
const COMP_DIR = join(process.cwd(), "src", "components");
const GLOBAL_CSS = join(process.cwd(), "src", "styles.css");

describe("select 下拉箭头的防误伤守卫", () => {
  it("全局箭头三条声明必须带 !important（组件的 background 简写会清掉它）", () => {
    const css = readFileSync(GLOBAL_CSS, "utf8");
    const block = css.slice(css.indexOf("select {"), css.indexOf("@media (forced-colors"));
    for (const prop of ["background-image", "background-repeat", "background-position"]) {
      const line = block.split("\n").find((l) => l.trim().startsWith(prop));
      expect(line, `styles.css 里 select 的 ${prop} 不见了——箭头没了`).toBeTruthy();
      expect(
        line,
        `${prop} 必须带 !important：组件里的 background 简写会把它静默清零（真发生过，8 处）`,
      ).toContain("!important");
    }
  });

  it("四套主题各有自己的箭头颜色（写死一个灰会在浅色/高对比主题下不达标）", () => {
    const css = readFileSync(GLOBAL_CSS, "utf8");
    for (const theme of ["daylight", "slate", "hc"]) {
      expect(
        css,
        `主题 ${theme} 没有自己的箭头颜色——写死的灰在浅色主题实测仅 3.09:1（地板 3:1）`,
      ).toContain(`:root[data-theme="${theme}"] select`);
    }
  });

  it("forced-colors（Windows 高对比度）下把控件交还系统", () => {
    const css = readFileSync(GLOBAL_CSS, "utf8");
    expect(css).toContain("@media (forced-colors: active)");
    const block = css.slice(css.indexOf("@media (forced-colors: active)"));
    expect(
      block,
      "appearance:none 交出了系统绘制的控件，强制颜色模式下必须还回去，否则高对比度用户两头落空",
    ).toContain("appearance: auto");
  });

  it("给 select 写了自定义 padding 的组件，必须给箭头留出右内距", () => {
    const offenders: string[] = [];
    for (const f of readdirSync(COMP_DIR).filter((n) => n.endsWith(".svelte"))) {
      const src = readFileSync(join(COMP_DIR, f), "utf8");
      const style = src.slice(src.indexOf("<style>"));
      for (const rule of style.match(/[^{}]*select[^{}]*\{[^}]*\}/g) ?? []) {
        if (!/padding\s*:/.test(rule)) continue;
        // 命中 select 且自己写了 padding → 必须给右侧留位（22px 一档，或显式 padding-right）
        const hasRoom =
          /padding-right\s*:\s*(1[6-9]|2[0-9])px/.test(style) ||
          /padding:\s*[^;]*\s(1[6-9]|2[0-9])px\s/.test(rule);
        if (!hasRoom) offenders.push(`${f}: ${rule.split("{")[0].trim()}`);
      }
    }
    expect(
      offenders,
      "这些规则给 select 设了 padding 却没给箭头留位——文字会压在箭头下（改法：padding-right: 22px）",
    ).toEqual([]);
  });
});
