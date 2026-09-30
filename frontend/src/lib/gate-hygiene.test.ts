import { describe, it, expect } from "vitest";

/**
 * 门禁自身的卫生：**扫描件一律惰性取用，不在模块体里直接读。**
 *
 * 本仓有一批"读源码判契约"的门禁（ipc-contract / event-contract / enum-contract /
 * settings-wiring / egress-contract / connect-state / action-wiring …）。它们都要按名字去
 * 拿别的文件的源文本，而这件事在模块体里做有一个共同的坏结局：
 *
 *   被读的源文件一旦改名或删除 → `readFileSync` 抛在模块体 → vitest 记的是**收集失败**
 *   → 该门禁文件的**全部**断言从 total 里消失（total 掉、failed 仍是 0），
 *     而不是"该红的那几条转红"。
 *
 * 这不会假绿——收集失败退出码非 0，CI 照样红。坏在两处：诊断指向"这个文件没跑"而不是
 * "哪条不变量断了"；以及同一次运行里该文件其余覆盖被静默丢掉，于是没人知道另外那些
 * 契约此刻到底还成不成立。
 *
 * 实测数据（不是假想）：V26 的 L1 变异体改名藏掉 TransferQueueDrawer.svelte，
 * enum-contract.test.ts 当场 `26 → 0`、全仓 325 → 299。egress-contract（V25）、
 * settings-wiring（V26）、enum-contract（本次）已先后改成惰性；本文件是防第四次的那道门。
 *
 * 判据：**模块体（顶格）与 describe 体**的 `const/let/var` 声明里若出现读文件调用，
 * 该调用必须在某个函数体内——`lazy(() => read(...))` 是本仓的惯用写法，
 * `const read = (p) => fs.readFileSync(p)` 这类**定义**同样合格（定义不等于求值），
 * 记录里挂着箭头函数的（如 event-contract 的 INDIRECT_KEYS）也合格。
 * 真正被打的只有"声明当场就把文件读了"这一种形态。
 *
 * describe 体和模块体一样要管：`describe(…, () => { … })` 的回调在收集阶段就执行，
 * 抛在里面同样是收集失败、同样整份消失。enum-contract 的三处 `rustEnum(XXX_RS(), …)`
 * 原先就写在 describe 体里。
 *
 * 四条已知边界，写在这里而不是等人踩——免得把"没管"读成"管住了"：
 *  1. 嵌套 describe（缩进 4 及以上）不扫。区分"describe 体里的语句"与"函数体里的语句"
 *     靠的是顶格 `describe(` + 缩进 2 这个组合，再往里就分不开了。
 *  2. 只看声明自己那段文本，不跟进被调用的辅助函数。`const X = loadEverything();` 里若藏着
 *     按名读，本门禁看不见（event-contract 的 `const sources = frontendSources()` 即此形）。
 *  3. 判据是"读调用之前出现过 `=>` 或 `function`"，于是 `walk(dir).map((f) => read(f))`
 *     被判合格——它形态上有箭头、实质上即时求值。这不是漏判：它读的是**枚举出来的**文件，
 *     不存在"按名字读一个不在的文件"，因而不属于本门禁要防的那类塌陷（event-contract 与
 *     ipc-contract 全走这条路，所以它们天然免疫，代价是改名不报错、只是覆盖悄悄变少）。
 *     真骗得过判据的是"箭头写在读调用左边、却仍即时求值的**按名**读"，prettier 排版下
 *     的真实代码不长这样。
 *  4. `import X from "./y.svelte?raw"` 这类**静态 ?raw 导入**不在管辖内：它的失败是 Vite
 *     解析期的 "Failed to resolve import"，同样整份塌陷，但那是构建图上的边、报错极其显眼，
 *     且 tsconfig 只装 vite/client 类型（无 @types/node）的文件只能这么读。要改得动到那批
 *     文件的读法本身，不属于本门禁要管的事。
 */

const TEST_RAW = import.meta.glob("../**/*.test.ts", {
  query: "?raw",
  import: "default",
  eager: true,
}) as Record<string, string>;

const fileName = (p: string): string => p.replace(/^.*\//, "");

/** 剥注释再扫：本文件上方那段说明里就写着这些形态，不剥就是让注释给自己作证/自我误伤。 */
const stripComments = (s: string): string =>
  s.replace(/\/\*[\s\S]*?\*\//g, (m) => m.replace(/[^\n]/g, "")).replace(/(^|[^:])\/\/[^\n]*/g, "$1");

/** 读文件调用：`fs.readFileSync(` / `readFileSync(` / 本仓惯用的裸 `read(` 包装。 */
const READ_SRC = "(?:\\bfs\\s*\\.\\s*readFileSync|\\breadFileSync|(?<![.\\w$])read)\\s*\\(";

/** 本文件里以 `const X = lazy(…)` 声明出来的取值器名字。 */
const lazyNames = (src: string): string[] =>
  [...src.matchAll(/^\s*(?:export\s+)?(?:const|let|var)\s+([\w$]+)\s*(?::[^=]+)?=\s*lazy\s*\(/gm)].map(
    (m) => m[1],
  );

/**
 * 收集阶段的危险调用 = 直接读文件 **∪ 调用本文件的 lazy 取值器**。
 *
 * 后半截不可省：`lazy()` 只是把读推迟到**第一次调用**，在 describe 体里写
 * `rustEnum(VERIFY_RS(), …)` 等于当场把它读回来，塌陷原样复发——enum-contract 的三处
 * 枚举解构原先正是这个形态。只钉"有没有 read(" 会让这一整类改法悄悄溜过去。
 */
const hazardRe = (src: string): RegExp => {
  const names = lazyNames(src);
  return new RegExp(names.length ? `${READ_SRC}|\\b(?:${names.join("|")})\\s*\\(` : READ_SRC);
};

/**
 * 收集阶段就会求值的声明：顶格（模块体）的，以及顶格 `describe(` 内缩进 2 的。
 * 续行 = 后面缩进更深的行（空行照收，跨空行的多行声明才不会被截断）。
 */
function collectTimeDecls(src: string): { name: string; text: string }[] {
  const lines = src.split("\n");
  const out: { name: string; text: string }[] = [];
  let inDescribe = false;
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    // 顶格行决定归属：`describe(` 开一段，其它顶格行（含收尾的 `});`）关掉它。
    if (/^\S/.test(line)) inDescribe = /^describe\s*(?:\.\w+)?\s*\(/.test(line);
    // 解构声明也要认：`const { variants, renameAll } = rustEnum(VERIFY_RS(), …)` 正是
    // enum-contract 三处枚举的原写法，只认标识符就会把这一整类放跑。
    const m = /^(\s*)(?:export\s+)?(?:const|let|var)\s+([\w$]+|\{[^}]*\}|\[[^\]]*\])/.exec(line);
    if (!m) continue;
    const indent = m[1].length;
    if (indent !== 0 && !(inDescribe && indent === 2)) continue;
    let text = line;
    for (let j = i + 1; j < lines.length; j++) {
      const next = lines[j];
      if (next.trim() !== "" && next.search(/\S/) <= indent) break;
      text += "\n" + next;
    }
    out.push({ name: m[2], text });
  }
  return out;
}

/** 收集阶段声明里"当场把文件读回来"的那些（返回声明名/解构模式原文）。 */
export function eagerReads(src: string): string[] {
  const stripped = stripComments(src);
  const re = hazardRe(stripped);
  const bad: string[] = [];
  for (const { name, text } of collectTimeDecls(stripped)) {
    const at = text.search(re);
    if (at < 0) continue;
    // 危险调用左边出现过箭头/function ⇒ 它在某个函数体里，收集时不求值。
    if (/=>|\bfunction\b/.test(text.slice(0, at))) continue;
    bad.push(name);
  }
  return bad;
}

describe("门禁卫生：扫描件不得在模块体里直接读", () => {
  it("扫查自身有效：抓到了这批门禁文件，且判据对正反样本都判得对", () => {
    const names = Object.keys(TEST_RAW).map(fileName);
    // 抓不到文件 ⇒ 下面那条"违规集合为空"恒真。这里同时钉住数量下限与几个点名的门禁。
    expect(names.length, "import.meta.glob 没抓到测试文件：glob 模式失效了").toBeGreaterThanOrEqual(20);
    for (const n of ["enum-contract.test.ts", "egress-contract.test.ts", "settings-wiring.test.ts"]) {
      expect(names, `${n} 不在扫查范围内`).toContain(n);
    }

    // 反恒真探针：正样本必须放行，负样本必须抓住。判据若被改松（比如把 READ_CALL 改成
    // 匹配不到的东西），下面这条会先红，而不是让"违规集合为空"静静地永远成立。
    const probe = [
      'const A = stripRust(read(path.join(X, "a.rs")));', // ← 当场读：必须抓
      "const B = lazy(() => stripRust(read(p)));", // ← lazy 包住：放行
      'const C = (p: string) => fs.readFileSync(p, "utf8");', // ← 定义读取器：放行
      "const D: Record<string, () => string> = {", // ← 记录里挂箭头：放行
      "  x: () => fs.readFileSync(p),",
      "};",
      'const E = fs.readFileSync(path.join(Y, "b.rs"), "utf8");', // ← 当场读：必须抓
      '// const F = read("整行注释里的不算");',
      "const G = spread(xs); // read(行尾注释里的也不算)", // ← 两件事：`spread(` 里嵌着 read(，
      "const R = 1; /* fs.readFileSync(p) */", //     不该误伤；注释不剥就会把这两行判成违规
      'const VERIFY_RS = lazy(() => read("v.rs"));', // ← 取值器本身：放行
      "const N = matchArms(VERIFY_RS());", // ← 模块体调用取值器 = 当场读回来：必须抓
      "const P = stripRust(", // ← 读被 prettier 折到续行上：必须抓
      '  read(path.join(X, "c.rs")),',
      ");",
      "const Q = stripRust(", // ← 续行之间夹空行：空行不算声明结束，仍必须抓
      "",
      '  read(path.join(X, "d.rs")),',
      ");",
      "",
      'describe("x", () => {',
      '  const H = rustEnum(read("v.rs"), "T");', // ← describe 体当场读：必须抓
      "  const I = lazy(() => read(p));", // ← describe 体 lazy：放行
      '  const { variants, renameAll } = rustEnum(VERIFY_RS(), "T");', // ← 解构 + 取值器：必须抓
      "  const wire = () => variants.map((v) => wireName(v, renameAll));", // ← 箭头里用：放行
      '  it("y", () => {',
      "    const J = fs.readFileSync(p);", // ← it 体内（缩进 4）：不归本门禁管
      "  });",
      "});",
      "",
      "function k() {", // ← 函数体内缩进 2，但不在 describe 段里：不该被当成 describe 体
      "  const L = fs.readFileSync(p);",
      "}",
    ].join("\n");
    expect(eagerReads(probe)).toEqual(["A", "E", "N", "P", "Q", "H", "{ variants, renameAll }"]);
  });

  it("没有任何门禁文件在模块体里当场读源文件", () => {
    const bad = Object.entries(TEST_RAW)
      .flatMap(([p, src]) => eagerReads(src).map((n) => `${fileName(p)}:${n}`))
      .sort();
    expect(
      bad,
      "模块体当场读：被读的文件一改名，整份门禁从 total 里消失（不是该红的转红）。请改成 lazy(() => …)",
    ).toEqual([]);
  });
});
