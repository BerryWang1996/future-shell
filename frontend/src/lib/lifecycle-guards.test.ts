/**
 * 生命周期两条结构守卫（路线图 4c 架构缺陷 A2 / A3，2026-09-02）。
 *
 * 行为本身各有单测（untilUnmount.test.ts / host-lights-loop.test.ts）；这里守的是
 * **调用点纪律**——修法是十几行，但会随每个新面板复制扩散，而复制出来的那一份
 * 没有任何测试会为它变红。同 gate-hygiene.test.ts 的口径：扫源码、非空证明、说清为什么。
 *
 *  A2 事件监听器竞态：任何组件都不得再手写 `x = await listen(...)` 或
 *     `.then((fn) => { unlisten = fn; })`——那两种写法在「组件先卸载、listen 后落定」时
 *     把一个指向死组件的处理器永久登记。统一走 `untilUnmount(listen<T>("evt", …))`。
 *  A3 启动期静默失败：App 的 onMount 此前是一串裸 await，第一个 reject 就让后面的
 *     loadProfiles / vault_status / 会话恢复全部不跑，且没有任何报错。每一步都要过 `boot()`。
 */
import fs from "node:fs";
import path from "node:path";
import { describe, expect, it } from "vitest";
import APP_SRC from "../App.svelte?raw";

const FE_SRC = path.resolve(process.cwd(), "src");

function walk(dir: string, out: string[] = []): string[] {
  for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
    const p = path.join(dir, e.name);
    if (e.isDirectory()) walk(p, out);
    else if (/\.(svelte|ts)$/.test(e.name) && !/\.test\.ts$/.test(e.name)) out.push(p);
  }
  return out;
}

/** 去掉块注释与行注释（注释里复述旧写法是合法的，本守卫只看代码）。 */
function stripComments(src: string): string {
  return src.replace(/\/\*[\s\S]*?\*\//g, "").replace(/(^|[^:"'`])\/\/[^\n]*/g, "$1");
}

describe("A2：监听器一律经 untilUnmount 挂到生命周期上", () => {
  const files = walk(FE_SRC).filter((f) => !f.endsWith(path.join("lib", "lifecycle.ts")));
  const sources = files.map((f) => [path.relative(FE_SRC, f), stripComments(fs.readFileSync(f, "utf8"))] as const);

  it("没有一处 `= await listen(` 或 `.then((fn) => { unlisten = fn })`", () => {
    const bad: string[] = [];
    for (const [rel, src] of sources) {
      if (/=\s*await\s+listen\s*[<(]/.test(src)) bad.push(`${rel}: "= await listen"`);
      if (/\.then\(\s*\(\s*\w+\s*\)\s*=>\s*\{?\s*un\w*\s*=\s*\w+\s*;?\s*\}?\s*\)/.test(src)) bad.push(`${rel}: ".then(fn => unlisten = fn)"`);
    }
    expect(bad, "这些写法在「组件先卸载、listen 后落定」时把死处理器永久登记，改用 untilUnmount(listen<T>(…))").toEqual([]);
  });

  it("非空证明：untilUnmount 确有调用点（否则上一条是在守一个没人用的规则）", () => {
    const n = sources.reduce((acc, [, src]) => acc + (src.match(/untilUnmount\(\s*listen\s*[<(]/g)?.length ?? 0), 0);
    expect(n).toBeGreaterThanOrEqual(12);
  });
});

describe("A3：App.onMount 的每个启动步骤都经 boot() 隔离", () => {
  /** onMount(async () => { … }) 的函数体（花括号配平；模板串里的 ${} 自身配平，不必单独处理）。 */
  function onMountBody(): string {
    const src = stripComments(APP_SRC);
    const head = "onMount(async () => {";
    const start = src.indexOf(head);
    expect(start, "App.svelte 里找不到 onMount(async () => {：锚点失效，请同步本守卫").toBeGreaterThan(0);
    let depth = 0;
    for (let i = start + head.length - 1; i < src.length; i++) {
      if (src[i] === "{") depth++;
      else if (src[i] === "}") { depth--; if (depth === 0) return src.slice(start + head.length, i); }
    }
    throw new Error("onMount 函数体花括号不配平");
  }

  /** 函数体里**顶层**（深度 0）的语句起始行。 */
  function topLevelLines(body: string): string[] {
    const out: string[] = [];
    let depth = 0;
    for (const line of body.split("\n")) {
      if (depth === 0) out.push(line);
      for (const ch of line) {
        if (ch === "{" || ch === "(") depth++;
        else if (ch === "}" || ch === ")") depth--;
      }
    }
    return out;
  }

  it("顶层没有裸 `await …` / `void …`：每一步都是 `await boot(` 或 `void boot(`", () => {
    const lines = topLevelLines(onMountBody());
    const bare = lines.filter((l) => /^\s*(?:\w+\s*=\s*)?(?:await|void)\s+(?!boot\()/.test(l));
    expect(bare, "裸 await 在 reject 时会让后面的启动步骤全部不跑，且无任何报错；裸 void 的 rejection 无人接").toEqual([]);
  });

  it("非空证明：boot() 至少包住了档案加载、保险库状态、会话恢复三步", () => {
    const body = onMountBody();
    for (const name of ["loadProfiles", "vault", "sessions_unclosed"]) {
      expect(body, `boot("${name}") 不见了`).toMatch(new RegExp(`boot\\("${name}"`));
    }
    expect(body.match(/boot\(/g)?.length ?? 0).toBeGreaterThanOrEqual(8);
  });

  it("boot 失败必须留痕：上报后端日志而不是只 console", () => {
    const src = stripComments(APP_SRC);
    const i = src.indexOf("async function boot(");
    expect(i, "App.svelte 里找不到 boot()").toBeGreaterThan(0);
    const fnSrc = src.slice(i, i + 800);
    expect(fnSrc).toContain("reportFrontendError(");
  });
});
