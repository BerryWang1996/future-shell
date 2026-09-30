#!/usr/bin/env node
/**
 * Shell 脚本可移植性门禁：`$VAR` 后面紧贴非 ASCII 字节（如 `$PROFILE，`）。
 *
 * # 为什么是门禁而不是约定
 *
 * macOS 自带的 bash 3.2 在 C locale 下把高位字节当成标识符的一部分：`"$PROFILE，"`
 * 被解析成一个叫 `PROFILE\xef` 的变量，配合 `set -u` 当场 `unbound variable` 退出。
 * 1.0.0 候选的第一次 GitHub CI（2026-09-05，macos-14）就死在
 * `scripts/build-rdp-helper.sh` 的这一行上，后面的 fmt/clippy/test 一步没跑。
 *
 * Linux 与 Windows（Git Bash）都是 bash 5，**不会复现**——本地全绿、Linux CI 全绿，
 * 只有 mac runner 红。这类只在一种环境里炸的问题，只能靠静态扫描拦：写 `${VAR}` 就没事。
 *
 * 用法：`node scripts/check-shell-portability.mjs [--selftest]`
 */
import { readFileSync, readdirSync, statSync } from "node:fs";
import { join, relative } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = join(fileURLToPath(new URL(".", import.meta.url)), "..");
/** `$NAME` 紧贴一个 ≥0x80 的字节。`${NAME}`、`$1`、`$#`、`$?` 不在此列。 */
const BAD = /\$([A-Za-z_][A-Za-z0-9_]*)(?=[\u0080-\uffff])/g;

/** 扫一段文本，返回违规点。行号从 1 起。 */
export function scan(text) {
  const out = [];
  text.split("\n").forEach((line, i) => {
    for (const m of line.matchAll(BAD)) out.push({ line: i + 1, name: m[1], text: line.trim() });
  });
  return out;
}

function selftest() {
  const cases = [
    ['echo "构建（$PROFILE，$TRIPLE）"', 2],
    ['echo "构建（${PROFILE}，${TRIPLE}）"', 0],
    ['fail "只剩 $n条"', 1],
    ['echo "$n 条"', 0], // 空格隔开：安全
    ['echo "$1，$#，$?"', 0], // 位置参数与特殊参数不是标识符
    ["echo '$PROFILE，'", 1], // 单引号里不展开，但照样拦：规则简单比规则精确更不容易被绕
  ];
  let bad = 0;
  for (const [src, want] of cases) {
    const got = scan(src).length;
    if (got !== want) {
      console.error(`selftest 失败：${JSON.stringify(src)} 期望 ${want} 处，实得 ${got} 处`);
      bad++;
    }
  }
  if (bad) process.exit(1);
  console.log(`shell 可移植性自检：${cases.length} 例全部符合`);
}

function* walk(dir) {
  for (const name of readdirSync(dir)) {
    if (["node_modules", "target", ".git", ".cache", "dist"].includes(name)) continue;
    const p = join(dir, name);
    const st = statSync(p);
    if (st.isDirectory()) yield* walk(p);
    else if (/\.(sh|bash)$/.test(name) || /\.github[\/]workflows[\/].+\.ya?ml$/.test(p)) yield p;
  }
}

if (process.argv.includes("--selftest")) {
  selftest();
} else {
  const files = [...walk(ROOT)];
  // 做空防护：一个脚本都没扫到 = 扫描本身坏了（路径改了/过滤写错了），不是「全干净」。
  if (files.length < 8) {
    console.error(`只扫到 ${files.length} 个脚本/工作流——扫描范围坏了，拒绝报绿`);
    process.exit(1);
  }
  const hits = [];
  for (const f of files) {
    for (const h of scan(readFileSync(f, "utf8"))) hits.push({ file: relative(ROOT, f), ...h });
  }
  if (hits.length) {
    console.error("这些 `$VAR` 紧贴非 ASCII 字符，macOS bash 3.2 会把后面的字节读进变量名（改成 `${VAR}`）：");
    for (const h of hits) console.error(`  ${h.file}:${h.line}  $${h.name}  →  ${h.text.slice(0, 100)}`);
    process.exit(1);
  }
  console.log(`shell 可移植性：${files.length} 个脚本/工作流无 \`$VAR\` 紧贴非 ASCII`);
}
