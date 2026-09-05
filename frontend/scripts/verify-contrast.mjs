#!/usr/bin/env node
// WCAG 2.1 AA 对比度核验：四主题文本组合比值 ≥4.5:1，High Contrast ≥7:1，违例非零退出。
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const css = readFileSync(join(root, "src", "lib", "theme", "tokens.css"), "utf8");

/** 解析各主题令牌块：:root[data-theme="id"] { --fs-k: v; ... } */
const themes = {};
for (const block of css.matchAll(/:root\[data-theme="([^"]+)"\]\s*\{([^}]*)\}/g)) {
  const tokens = {};
  for (const kv of block[2].matchAll(/--fs-([a-z-]+)\s*:\s*(#[0-9a-fA-F]{6})/g)) {
    tokens[kv[1]] = kv[2].toLowerCase();
  }
  themes[block[1]] = tokens;
}

/** WCAG 相对亮度：sRGB 通道 /255 后 c≤0.03928 ? c/12.92 : ((c+0.055)/1.055)^2.4；L = 0.2126R + 0.7152G + 0.0722B。 */
function relLum(hex) {
  const n = parseInt(hex.slice(1), 16);
  const chan = (v) => {
    const c = v / 255;
    return c <= 0.03928 ? c / 12.92 : Math.pow((c + 0.055) / 1.055, 2.4);
  };
  return 0.2126 * chan((n >> 16) & 255) + 0.7152 * chan((n >> 8) & 255) + 0.0722 * chan(n & 255);
}
const ratio = (a, b) => {
  const la = relLum(a);
  const lb = relLum(b);
  return (Math.max(la, lb) + 0.05) / (Math.min(la, lb) + 0.05);
};

const COMBOS = [
  ["fg-primary", "bg-panel"],
  ["fg-primary", "bg-app"],
  ["fg-secondary", "bg-panel"],
  ["fg-secondary", "bg-app"],
  ["accent-fg", "accent"],
  ["fg-primary", "selection"],
];

let failures = 0;
for (const [id, tokens] of Object.entries(themes)) {
  const min = id === "hc" ? 7 : 4.5;
  console.log(`[${id}] 阈值 ≥ ${min}:1`);
  for (const [fg, bg] of COMBOS) {
    const r = ratio(tokens[fg], tokens[bg]);
    const ok = r >= min;
    if (!ok) failures += 1;
    console.log(`  ${ok ? "PASS" : "FAIL"}  ${fg} / ${bg} = ${r.toFixed(2)}:1`);
  }
}
if (failures > 0) {
  console.error(`${failures} 项对比度违例`);
  process.exit(1);
}
console.log("对比度核验全部通过");
