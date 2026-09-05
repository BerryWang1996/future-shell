import { describe, it, expect } from "vitest";
import fs from "node:fs";
import path from "node:path";

/**
 * 出站面守卫：**这个程序除了用户自己配置的 SSH 目标，不该向任何地方发包。**
 *
 * 验收清单 §47 把这条写成「无遥测代码路径（grep 审查：无外发 HTTP）」——载体是「人拿 grep 看一眼」。
 * 而 grep 恰好证不了这件事：`app/src` 与各 crate 的 src 里 grep `reqwest|fetch|hyper` **确实一无所获**，
 * 但 Cargo.lock 里躺着 reqwest / hyper / ureq / tonic 四套 HTTP 栈。它们没进产物，靠的是
 *
 *   - reqwest：tauri 2.11.5 把它挂在 `[target.'cfg(android / ios)'.dependencies]` 下，桌面三平台不入图；
 *   - hyper / ureq / tonic：testcontainers → bollard，只被 `crates/itest` 这个集成测试 crate 依赖。
 *
 * 两条都不是 grep 能看出来的，得逐目标解依赖图。（点时验证已记在 docs/verification/phase1-acceptance.md：
 * `cargo tree -p future-shell-app -e normal` 共 1221 行，四者与 bollard/testcontainers/fs_itest 均为 0 命中。）
 *
 * 依赖图那一段没法在 vitest 里重算——它要 cargo 解析 registry。所以本文件钉的是**仓内可判的那几段**，
 * 也正是现实中会出事的那几段：
 *
 *   ① 出货 crate 的任何依赖表（含 target 门控表、build-dependencies）不得直接写入出站型 crate；
 *   ② `fs_itest` 不得成为出货依赖——它是那三套 HTTP 栈进入图的唯一通道；
 *   ③ 前端源码不得出现浏览器侧出站 API；前端依赖不得引入网络型 Tauri 插件；
 *   ④ CSP 的 connect-src 只放行 self 与 Tauri IPC 通道，script-src 不得开 unsafe-eval；
 *   ⑤ 能力清单不得授出 `opener:` —— 它自己的 description 就写着「不授 opener:*，外链一律经 Rust
 *      open_external 白名单」，而这句话此前没有任何东西在执行。授了它，前端就能绕开
 *      `parse_external_url` 的 scheme 白名单直接开任意 URL。
 *
 * ①②只覆盖**直接**写入，传递引入（某个新 crate 自己带 reqwest）本文件看不见——如实记在这里，
 * 那一格归 CI 的 `cargo tree` 与人工评审。
 */

const ROOT = path.resolve(process.cwd(), "..");
const read = (p: string) => fs.readFileSync(path.join(ROOT, p), "utf8");

/** 参与出货的 Rust crate 清单（app 及其 path 依赖的库 crate）。`crates/itest` 蓄意不在其中。 */
const SHIPPED_MANIFESTS = shippedManifests();

/**
 * 出货清单**从 `app/Cargo.toml` 的 path 依赖推导**，而不是写死。
 *
 * 写死过一次，然后就漏了：这张表列的是 vault/connmgr/sshengine/terminal 四个，
 * 那是 M1 时的实际情况。M2 起 `fs_policy`/`fs_ai` 也会被 app 依赖而进入产物，
 * 而一张写死的表对它们视而不见——于是「出货 crate 不得声明出站型依赖」这条
 * 在**新代码**上悄悄停止了生效。而新代码恰恰是最需要它的地方。
 *
 * 推导规则：`app/Cargo.toml` 里每一条 `path = "../crates/X"` 都算出货，
 * 加上 app 自己。传递 path 依赖（某个 core crate 依赖另一个）也一并收，
 * 因为它们同样进产物。
 *
 * # `rdp-helper` 必须显式加进种子——推导看不见它
 *
 * RDP helper 是**另一个工作区**（自己的 Cargo.lock，见 `rdp-helper/Cargo.toml`
 * 头部：IronRDP 与 russh 的依赖树不能相遇）。它随安装包一起发，
 * 是**实打实的出货二进制**，但它不是 `app` 的 path 依赖——
 * 上面那套从 `app/Cargo.toml` 出发的推导对它完全视而不见。
 *
 * 那正是这个架构引入的一个**新洞**：一个进产物的 crate 可以声明 reqwest
 * 而没有任何东西会响。故把它写进种子。
 *
 * 写死一个路径在这里是可接受的——它不像 core crate 那样会不断增加，
 * 而且一旦它改名，`read()` 会立刻抛「文件不存在」，不会静默失效。
 */
function shippedManifests(): string[] {
  const seen = new Set<string>();
  const queue = ["app/Cargo.toml", "rdp-helper/Cargo.toml"];
  while (queue.length) {
    const m = queue.shift()!;
    if (seen.has(m)) continue;
    seen.add(m);
    const src = read(m);
    // `path = "../crates/vault"` / `path = "../vault"`
    for (const [, rel] of src.matchAll(/path\s*=\s*"([^"]+)"/g)) {
      const base = path.posix.normalize(path.posix.join(path.posix.dirname(m), rel));
      const next = `${base}/Cargo.toml`;
      if (fs.existsSync(path.join(ROOT, next))) queue.push(next);
    }
  }
  return [...seen].sort();
}

/**
 * HTTP/WebSocket/gRPC 客户端。**能力**上会发请求，但发给谁由代码决定。
 *
 * 与遥测 SDK 分开列，是 M2 之后必须做的区分：AI provider（含本地 Ollama）
 * 就是「向用户自己填的地址发请求」，它与「向厂商服务器上报」是两件不同的事，
 * 而一张混在一起的拒绝名单没法表达这个差别——只能一律禁，于是 AI 层无法实现。
 */
const HTTP_CLIENTS = [
  "reqwest", "ureq", "isahc", "curl", "surf", "attohttpc", "minreq", "awc",
  "hyper", "hyper-util", "tonic", "tungstenite", "tokio-tungstenite",
  "tauri-plugin-http", "tauri-plugin-websocket",
  // surf 的可插拔后端。直接声明它就是在自己拼客户端，绕过上面任何一个名字。
  "http-client",
];

/**
 * 遥测/崩溃上报 SDK。**目的**就是把数据发给第三方，地址不由用户决定。
 *
 * 这一类**在任何 crate 里都不允许**，`fs_ai` 也不例外——包括 `tauri-plugin-updater`：
 * 它会主动去厂商的 manifest 地址查版本，那是标准的 phone home。
 * 「零遥测」这条承诺指的正是这一类，它没有例外。
 */
const TELEMETRY_CRATES = [
  "opentelemetry", "opentelemetry-otlp", "sentry", "sentry-core", "posthog-rs",
  "analytics-next", "segment", "amplitude", "mixpanel", "datadog", "bugsnag",
  "tauri-plugin-updater",
  // ── 2026-08-24 补列。下面这批的共同点是：它们是**本仓最可能真的滑进来**的那几个，
  // 而上面那张名单一个都没盖到。
  //
  // 本仓到处在用 `tracing`。往 tracing 上挂一个导出层是一行依赖的事，而且看起来
  // 完全像是「改进可观测性」——`tracing-opentelemetry` 与 `sentry-tracing` 正是那一行。
  // 名字里带 tracing 会让它读起来属于既有的日志设施，而它做的是把 span 发到外面去。
  "tracing-opentelemetry", "sentry-tracing",
  // 包名的下划线/连字符是**两个不同的包**。`opentelemetry` 已在列，
  // 但 SDK 那半发布名是 `opentelemetry_sdk`（下划线），只写连字符形式盖不住它。
  "opentelemetry_sdk",
  // Tauri 生态里的用量统计插件，以及它背后的服务。前者尤其危险：
  // 它以「一行代码接入桌面应用分析」为卖点，正好是 Tauri 项目会顺手加的东西。
  "tauri-plugin-aptabase", "aptabase",
  // `posthog-rs` 已在列，但 crates.io 上 `posthog` 这个名字也是可用的。
  "posthog",
  // statsd 家族：不发给 SaaS，但一样把数据推向一个网络端点。
  "dogstatsd", "statsd", "cadence",
  // 崩溃上报的其余几家。
  "appcenter", "rollbar", "honeycomb",
  // 指标**导出**面。本程序不导出指标，出现它就说明有人在往外送数——
  // 即使只暴露一个 /metrics 端点，那也是一个本程序不该有的面。
  "prometheus",
];

/**
 * 允许声明 HTTP 客户端的出货 crate，及其允许的具体 crate。
 *
 * 只有 `crates/ai` 一个例外，理由是产品承诺的措辞（README「出站面」节）：
 * 「零遥测；除用户显式配置的目标（SSH 主机、AI provider）外不外发」。
 * AI provider 的地址是用户自己填的——云 API 的 key 与 base URL、
 * 或本地 Ollama 的 `localhost:11434`。没有配置就没有地址，也就没有流量。
 *
 * 例外是**逐 crate 逐 crate 名**给的，不是「AI 层随便用」：
 * 多一个 HTTP 栈就多一片攻击面与许可证面，而一个 AI 层用不到两个。
 */
const HTTP_ALLOWED: Record<string, string[]> = {
  "crates/ai/Cargo.toml": ["reqwest"],
};

/** 拒绝名单全集（供「改名混入」那条复用）。 */
const EGRESS_CRATES = [...HTTP_CLIENTS, ...TELEMETRY_CRATES];

/** 浏览器侧的出站 API。xterm 的 web-links 只生成 <a>，点击走 opener，不在此列。 */
const BROWSER_EGRESS = [
  /\bfetch\s*\(/,
  /\bXMLHttpRequest\b/,
  /\bnew\s+WebSocket\b/,
  /\bnew\s+EventSource\b/,
  /\bsendBeacon\s*\(/,
  /\bnavigator\.connection\b/,
];

const NETWORK_JS_PLUGINS = [
  "@tauri-apps/plugin-http",
  "@tauri-apps/plugin-updater",
  "@tauri-apps/plugin-websocket",
];

// ---------------------------------------------------------------------------
// 解析器
// ---------------------------------------------------------------------------

/**
 * Cargo.toml 里**所有**依赖表的键名，连表名一起返回。
 *
 * 必须认 target 门控表与子表两种形态：tauri 自己就是用
 * `[target.'cfg(any(target_os = "android", …))'.dependencies]` 把 reqwest 挡在桌面之外的，
 * 只认 `[dependencies]` 的解析器会对同样手法写进本仓的依赖视而不见。
 */
export function depEntries(src: string): { table: string; name: string }[] {
  const out: { table: string; name: string }[] = [];
  const isDepTable = (t: string) => /(^|\.)(dependencies|dev-dependencies|build-dependencies)$/.test(t);
  let table = "";
  for (const raw of src.split("\n")) {
    const l = raw.replace(/^\s*#.*$/, "").trim();
    if (!l) continue;
    const head = /^\[([^\]]+)\]$/.exec(l);
    if (head) {
      table = head[1];
      // `[dependencies.foo]` / `[target.'cfg(…)'.dependencies.foo]`：表名末段就是依赖名本身。
      const m = /^(.*)\.([A-Za-z0-9_-]+)$/.exec(table);
      if (m && isDepTable(m[1])) out.push({ table: m[1], name: m[2] });
      continue;
    }
    if (!isDepTable(table)) continue;
    const kv = /^([A-Za-z0-9_-]+)\s*(?:\.[A-Za-z0-9_-]+)?\s*=/.exec(l);
    if (kv) out.push({ table, name: kv[1] });
  }
  return out;
}

/**
 * CSP 某条指令的取值表；指令缺席返回 null（与「存在但为空」是两回事——缺席会回落到 default-src，
 * 判定必须能区分）。
 *
 * 同名指令出现两次直接抛：浏览器只认**第一条**、静默忽略其余，于是「我明明加上去了」与
 * 实际生效的策略可以长期背离。
 */
export function cspDirective(csp: string, name: string): string[] | null {
  const parts = csp.split(";").map((s) => s.trim()).filter(Boolean);
  const hit = parts.filter((p) => p === name || p.startsWith(name + " "));
  if (hit.length > 1) throw new Error(`CSP 里 ${name} 出现 ${hit.length} 次：浏览器只认第一条，判定无意义`);
  if (!hit.length) return null;
  return hit[0].slice(name.length).trim().split(/\s+/).filter(Boolean);
}

// ---------------------------------------------------------------------------
// 扫描件
// ---------------------------------------------------------------------------

/**
 * 全部惰性读。**这不是风格选择**：`readFileSync` 对着不存在的文件会抛，`JSON.parse` 对着
 * 半截 JSON 会抛，而在模块顶层抛出会让 vitest 判为**收集失败**——整份门禁 16 条一起从统计里
 * 消失（total 掉、failed 仍是 0），而不是该红的那几条转红。上一轮（V24/V2）已经在枚举门禁上
 * 栽过一次：CI 照样红，但诊断指向错误的地方，且同文件其余断言当次**根本没跑**。
 * 放进 it 体里，删掉 capabilities/default.json 就只红依赖它的那两条。
 */
function lazy<T>(f: () => T): () => T {
  let v: T, done = false;
  return () => {
    if (!done) { v = f(); done = true; }
    return v;
  };
}

const TAURI_CONF = lazy(() => JSON.parse(read("app/tauri.conf.json")));
const CAPABILITY = lazy(() => JSON.parse(read("app/capabilities/default.json")));
const PKG = lazy(() => JSON.parse(read("frontend/package.json")));
const OPENER_RS = lazy(() => read("app/src/commands/opener_cmd.rs"));
const MANIFESTS = lazy(() => SHIPPED_MANIFESTS.map((p) => [p, read(p)] as const));
/** 工作区根清单：`[workspace.dependencies]` 是出货 crate 写 `x.workspace = true` 时的取值来源。 */
const ROOT_MANIFEST = lazy(() => read("Cargo.toml"));

/** 前端生产源码（剔测试与本文件自身：拒绝名单的正则写在这里，扫全文等于自己举报自己）。 */
const FE_RAW = import.meta.glob("../**/*.{ts,svelte}", {
  query: "?raw",
  import: "default",
  eager: true,
}) as Record<string, string>;
const FE = Object.entries(FE_RAW).filter(([p]) => !p.includes(".test."));

/** 剥注释：谈论 fetch 的注释不该把自己的文件判死（同 settings-wiring / action-wiring 口径）。 */
function code(src: string): string {
  return src
    .replace(/<!--[\s\S]*?-->/g, "")
    .replace(/\/\*[\s\S]*?\*\//g, "")
    .replace(/(^|[^:])\/\/.*$/gm, "$1");
}

describe("出站面：除用户配置的 SSH 目标外不得有任何外发（验收 §47）", () => {
  it("扫描件非空——任一侧读空都会让下面的判定恒真", () => {
    // 条数下限而不是等值：出货清单现在是**推导**出来的，加一个 core crate 就会变。
    // 写等值会让「新增 crate」这件正常的事变成门禁失败，于是下一步就是有人去调那个数字，
    // 而调数字的时候没人会去想「这个新 crate 是否该进出站面审查」。
    expect(MANIFESTS().length, "出货清单条数").toBeGreaterThanOrEqual(5);
    // 下限挡不住「推导把某个关键 crate 漏了」，故逐个点名那几个**必须**在网内的。
    for (const want of [
      "app/Cargo.toml",
      "crates/sshengine/Cargo.toml",
      "crates/vault/Cargo.toml",
      "crates/ai/Cargo.toml",
    ]) {
      expect(
        MANIFESTS().some(([p]) => p === want),
        `出站面审查范围没覆盖到 ${want}`,
      ).toBe(true);
    }
    // 解析器可用性：**总量**有下限，且点名的那几个各自解析到依赖。
    //
    // 不逐个清单都要求 >0：`crates/policy` 的依赖表是**空的**，而那是它的一个优点
    // ——一个纯逻辑的安全闸门没有任何供应链。要求人人有依赖会把这件好事判成门禁失败，
    // 接着有人会去给它加一个用不上的依赖来「修」测试。
    const totalDeps = MANIFESTS().reduce((n, [, src]) => n + depEntries(src).length, 0);
    expect(totalDeps, "所有出货清单解析到的依赖总数").toBeGreaterThan(20);
    for (const want of ["app/Cargo.toml", "crates/sshengine/Cargo.toml"]) {
      const src = MANIFESTS().find(([p]) => p === want)?.[1] ?? "";
      expect(depEntries(src).length, `${want} 没解析到依赖——解析器可能坏了`).toBeGreaterThan(0);
    }
    // 只数个数挡不住 glob 收窄：src/lib 一家就有 16 个非测试 .ts，光看"大于 20"
    // 仍可能已经把 components/ 与 App.svelte 整个漏在网外。三个目录各点名一个。
    // （注意 vite 会把 `../lib/x.ts` 归一成 `./x.ts`，所以按文件名钉而不是按路径片段。）
    for (const want of ["/App.svelte", "/TerminalPane.svelte", "/ipc.ts"]) {
      expect(FE.some(([p]) => p.endsWith(want)), `扫描范围没覆盖到 ${want}`).toBe(true);
    }
    expect(FE.length, "前端源码文件数").toBeGreaterThan(30);
    expect(TAURI_CONF()?.app?.security?.csp, "tauri.conf.json 的 CSP").toBeTruthy();
    expect(CAPABILITY()?.permissions?.length, "能力清单权限条数").toBeGreaterThan(0);
  });

  it("遥测/上报型 crate 在任何出货 crate 里都不允许——包括 AI 层", () => {
    // 「零遥测」没有例外。这一类的**目的**就是把数据发给第三方、地址不由用户决定，
    // 与「向用户自己填的 AI 地址发请求」是两件不同的事。
    for (const [p, src] of MANIFESTS()) {
      for (const { table, name } of depEntries(src)) {
        expect(TELEMETRY_CRATES, `${p} 的 [${table}] 引入了遥测型 ${name}`).not.toContain(name);
      }
    }
  });

  it("HTTP 客户端只允许出现在明确开了口子的那一个 crate 里", () => {
    for (const [p, src] of MANIFESTS()) {
      const allowed = HTTP_ALLOWED[p] ?? [];
      for (const { table, name } of depEntries(src)) {
        if (!HTTP_CLIENTS.includes(name)) continue;
        expect(
          allowed,
          `${p} 的 [${table}] 引入了 HTTP 客户端 ${name}。` +
            `只有 ${Object.keys(HTTP_ALLOWED).join("、")} 可以，且限于 ${JSON.stringify(HTTP_ALLOWED)}`,
        ).toContain(name);
      }
    }
  });

  it("开了口子的 crate 确实存在——豁免项不得指向一个已经改名/删掉的清单", () => {
    // 做空防护：若 HTTP_ALLOWED 的键指向不存在的文件，上面那条对它永远不会执行，
    // 而「只有 crates/ai 可以」这句话就变成了一句无人核对的注释。
    for (const p of Object.keys(HTTP_ALLOWED)) {
      expect(
        MANIFESTS().some(([m]) => m === p),
        `HTTP_ALLOWED 里的 ${p} 不在出货清单里——豁免项已失效，请删掉或改对`,
      ).toBe(true);
    }
  });

  it("工作区根的 [workspace.dependencies] 里也没有出站型 crate", () => {
    // 出货 crate 写 `x.workspace = true` 时，真正的版本与包名在根清单里。上一条按成员清单
    // 的键名判定，键名恰好就是 crate 名——除非根清单用了 `package = ` 改名（见下一条）。
    for (const { table, name } of depEntries(ROOT_MANIFEST())) {
      expect(EGRESS_CRATES, `根清单 [${table}] 引入了 ${name}`).not.toContain(name);
    }
  });

  it("没有用 `package = ` 改名把出站型 crate 换个马甲引进来", () => {
    // `http-client = { package = "reqwest" }` 之后，成员清单里写的是 http-client，
    // 按键名的拒绝名单当场失明。改名是 Cargo 的正常功能，不是刻意规避，但效果一样。
    for (const [p, src] of [["Cargo.toml", ROOT_MANIFEST()] as const, ...MANIFESTS()]) {
      for (const m of src.matchAll(/\bpackage\s*=\s*"([^"]+)"/g)) {
        expect(EGRESS_CRATES, `${p} 用别名引入了 ${m[1]}`).not.toContain(m[1]);
      }
    }
  });

  it("集成测试 crate 不在出货依赖图里——那三套 HTTP 栈的唯一入口", () => {
    // hyper / ureq / tonic 经 testcontainers → bollard 进来，而 testcontainers 只被 fs_itest 依赖。
    // 一旦哪个出货 crate 依赖了 fs_itest，上面那条按 crate 名的拒绝名单就整条被绕过。
    for (const [p, src] of MANIFESTS()) {
      for (const { name } of depEntries(src)) {
        expect(name, `${p} 依赖了集成测试 crate`).not.toBe("fs_itest");
      }
      // 改名同样绕过按名判定，再按 path 钉一次：crates/itest 这个目录不许出现在出货清单里。
      expect(/path\s*=\s*"[^"]*itest[^"]*"/.test(src), `${p} 有指向 itest 的 path 依赖`).toBe(false);
    }
  });

  it("前端源码里没有浏览器侧出站 API", () => {
    for (const [p, src] of FE) {
      const c = code(src);
      for (const re of BROWSER_EGRESS) {
        expect(re.test(c), `${p} 命中 ${re}`).toBe(false);
      }
    }
  });

  it("前端依赖里没有网络型 Tauri 插件", () => {
    const pkg = PKG();
    const all = { ...(pkg.dependencies ?? {}), ...(pkg.devDependencies ?? {}) };
    for (const n of NETWORK_JS_PLUGINS) expect(Object.keys(all)).not.toContain(n);
  });
});

describe("CSP：webview 侧的出站闸门", () => {
  // 同样惰性：cspDirective 遇到重复指令会抛，而这正是它该抛的时候——但抛在 describe 体里
  // 会把整份门禁从统计里抹掉，抛在 it 里才是"该红的红"。
  const CSP = (): string => TAURI_CONF().app.security.csp;

  it("default-src 收紧到 self", () => {
    expect(cspDirective(CSP(), "default-src")).toEqual(["'self'"]);
  });

  it("connect-src 显式存在，且只放行 self 与 Tauri IPC 通道", () => {
    // 缺席不等于安全：缺席回落到 default-src，一旦哪天 default-src 放宽，connect-src 跟着放宽。
    const v = cspDirective(CSP(), "connect-src");
    expect(v, "connect-src 缺席：出站白名单变成 default-src 的附庸").not.toBeNull();
    expect([...(v ?? [])].sort()).toEqual(["'self'", "http://ipc.localhost", "ipc:"]);
  });

  it("script-src 不开 unsafe-eval / unsafe-inline —— 注入到执行的那一步", () => {
    const csp = CSP();
    const v = cspDirective(csp, "script-src") ?? cspDirective(csp, "default-src") ?? [];
    expect(v).not.toContain("'unsafe-eval'");
    expect(v).not.toContain("'unsafe-inline'");
    expect(v.some((s) => /^https?:/.test(s)), "script-src 放行了外站脚本源").toBe(false);
  });

  it("form-action / base-uri 显式收死——这两条不回落到 default-src", () => {
    // CSP 里 form-action、base-uri、frame-ancestors 是**不**回落到 default-src 的：
    // `default-src 'self'` 写得再紧，一个 <form action="https://…" method=post> 照样能把
    // 表单内容发出去。本仓唯一的 <form> 正是 AuthPromptDialog 里收**口令/私钥密码**的那个
    // （它自己 preventDefault，所以钉成 'none' 不影响现有行为）——这一格空着，
    // 等于「除 SSH 目标外不外发」这句话在最敏感的那个输入框上不成立。
    expect(cspDirective(CSP(), "form-action"), "form-action 缺席：表单提交是 default-src 管不到的出站面").toEqual(["'none'"]);
    expect(cspDirective(CSP(), "base-uri"), "base-uri 缺席：注入 <base> 可改写全部相对 URL 的解析基准").toEqual(["'none'"]);
  });

  it("withGlobalTauri 未开——开了等于把整个 IPC 挂到 window 上", () => {
    expect(TAURI_CONF().app?.withGlobalTauri ?? false).toBe(false);
  });
});

describe("能力清单：外链只能经 Rust 白名单出去", () => {
  const PERMS = (): string[] =>
    CAPABILITY().permissions.map((p: unknown) =>
      typeof p === "string" ? p : ((p as { identifier?: string }).identifier ?? ""),
    );

  it("不授 opener: / http: / shell: —— 与清单自述的口径一致", () => {
    // capabilities/default.json 的 description 白纸黑字写着「不授 opener:*，外链一律经 Rust
    // open_external 白名单」。授出 opener 权限，前端就能直接开任意 URL，
    // parse_external_url 的 scheme 白名单形同虚设——而这句自述此前没有任何东西在执行。
    const perms = PERMS();
    expect(perms.length, "权限列表读空则本条恒真").toBeGreaterThan(0);
    for (const p of perms) {
      expect(/^(opener|http|shell):/.test(p), `能力清单授出了 ${p}`).toBe(false);
    }
  });

  it("外链命令确实先过白名单校验再交给插件", () => {
    const src = OPENER_RS();
    const body = src.slice(src.indexOf("pub fn open_external"));
    const guard = body.indexOf("parse_external_url");
    const call = body.indexOf("open_url");
    expect(guard, "open_external 里没有 parse_external_url").toBeGreaterThan(-1);
    expect(call, "open_external 里没有 open_url").toBeGreaterThan(-1);
    expect(guard, "校验必须发生在开链之前").toBeLessThan(call);
  });
});

describe("解析器反恒真探针", () => {
  it("depEntries 认得 target 门控表、子表与 workspace 简写", () => {
    const probe = [
      "[dependencies]",
      "# 注释行不算",
      'tauri = { version = "2" }',
      "tokio.workspace = true",
      "[dependencies.serde]",
      'version = "1"',
      "[target.'cfg(any(target_os = \"android\"))'.dependencies]",
      'reqwest = "0.13"',
      "[dev-dependencies]",
      'tempfile = "3"',
      "[package]",
      'name = "x"',
    ].join("\n");
    const got = depEntries(probe);
    expect(got.map((d) => d.name).sort()).toEqual(["reqwest", "serde", "tauri", "tempfile", "tokio"]);
    // 门控表里的那条必须带着它的表名回来，否则「挡在桌面之外」与「装进桌面」就分不开了。
    expect(got.find((d) => d.name === "reqwest")?.table).toMatch(/^target\..*\.dependencies$/);
    // [package] 下的 name 不是依赖。
    expect(got.map((d) => d.name)).not.toContain("x");
  });

  it("cspDirective 区分缺席与空，且同名指令重复时直接抛", () => {
    const csp = "default-src 'self'; connect-src 'self' ipc:";
    expect(cspDirective(csp, "default-src")).toEqual(["'self'"]);
    expect(cspDirective(csp, "connect-src")).toEqual(["'self'", "ipc:"]);
    expect(cspDirective(csp, "img-src")).toBeNull();
    expect(() => cspDirective("connect-src 'self'; connect-src *", "connect-src")).toThrow(/只认第一条/);
    // 前缀不得误命中：script-src 不是 script-src-elem。
    expect(cspDirective("script-src-elem 'self'", "script-src")).toBeNull();
  });

  it("出站正则真的会命中，注释剥离也真的在生效", () => {
    const hit = 'const r = await fetch("https://x");';
    expect(BROWSER_EGRESS.some((re) => re.test(code(hit)))).toBe(true);
    expect(BROWSER_EGRESS.some((re) => re.test(code("// 这里不许出现 fetch(")))).toBe(false);
    expect(BROWSER_EGRESS.some((re) => re.test(code("const ws = new WebSocket(u);")))).toBe(true);
  });
});

/**
 * MCP 传输面（M3 前置绊线，2026-08-24 立）。
 *
 * # 为什么要有这一节
 *
 * 总设计 §4.5 写着 stdio 是 M3 阶段的**唯一**传输，「传输层无监听地址」。
 * 2026-08-24 的 rmcp spike 证实这做得到：rmcp 3.1.4 的普通依赖里每一个 HTTP crate
 * （hyper / hyper-util / reqwest / http / http-body / sse-stream / oauth2 / tower-service）
 * 全是 optional，服务端那套（axum、带 server 特性的 hyper）只在 dev-dependencies 里。
 * 按 `default-features = false` + stdio 那几个特性声明，一个 HTTP crate 都不进构建图。
 *
 * 但**本文件上面那些判定看不住这件事**：它们只认「直接声明的依赖名」，而 `rmcp`
 * 本身不在 `HTTP_CLIENTS` 名单里。于是有人给它加一个
 * `transport-streamable-http-server` 特性、悄悄给应用开一个 HTTP 监听端口，
 * 全仓一条测试都不会红——「stdio 为唯一传输」就又变成一句只由注释担保的承诺。
 *
 * 而「只由注释担保的承诺」这件事，本仓在 2026-08-24 一天之内踩到两次（一次是注记说
 * 有门禁而其实没有，一次是注记说没做而其实做了）。所以这条绊线现在就立，不等接入。
 *
 * # 绊线怎么工作
 *
 * `rmcp` 此刻**尚未接入**。这一节因此断言的是「当前状态 = 未接入」——
 * 一个此刻为真、且在情况变化的那一刻**必然为假**的事实。
 * 接入 rmcp 的那个人会看到这条红，被迫回到这里写真正的特性判定
 * （下面 `HTTP_BEARING_FEATURES` 已经把名单列好了，照着写即可）。
 *
 * 这比「写一条 if 声明了就检查、没声明就放过」的条件式守卫诚实：后者在今天
 * **无法失败**，而一个无法失败的门禁与没有门禁没有区别。
 */
describe("MCP 传输面：stdio 唯一，不许开出 HTTP 监听（M3 绊线）", () => {
  /** rmcp 里会把 HTTP 拉进构建图的特性名（spike 实测，rmcp 3.1.4）。接入后一个都不许开。 */
  const HTTP_BEARING_FEATURES = [
    "server-side-http",
    "transport-streamable-http-server",
    "transport-streamable-http-server-session",
    "transport-streamable-http-client",
    "transport-streamable-http-client-reqwest",
    "transport-streamable-http-client-unix-socket",
    "client-side-sse",
    "__reqwest",
    "reqwest",
    "reqwest-tls-no-provider",
    "reqwest-native-tls",
    "auth",
    "auth-client-credentials-jwt",
  ];

  /** stdio 一路需要、且确认不带 HTTP 的特性。接入时应当只用这些。 */
  const STDIO_SAFE_FEATURES = [
    "server",
    "client",
    "macros",
    "transport-io",
    "transport-async-rw",
    "transport-child-process",
    "which-command",
    "base64",
    "schemars",
    "elicitation",
    "request-state",
    "local",
    "tower",
    "transport-worker",
  ];

  /**
   * 直接按路径读，**不走 `SHIPPED_MANIFESTS`**。
   *
   * 那张表是从 `app/Cargo.toml` 的 path 依赖推导的，而 `crates/mcpbridge` 此刻还不是
   * app 的依赖（它是个空壳）——于是它根本不在表里，任何基于那张表的判定对它一律视而不见。
   * 这正是本节要防的一段窗口：一个还没接进主程序的 crate 里先长出一个 HTTP 监听面，
   * 等它被接进来时，已经没人会重新审一遍它的依赖了。
   *
   * 反过来也成立，这是好消息：mcpbridge 一旦真成为 app 的 path 依赖，它就自动进入
   * `SHIPPED_MANIFESTS`，上面那些 HTTP_CLIENTS / TELEMETRY_CRATES 判定即刻覆盖它。
   * 本节补的是「接进来之前」那段窗口，以及那些判定看不见的**特性**维度。
   */
  const mcpbridgeToml = (): string => read("crates/mcpbridge/Cargo.toml");

  it("两张特性名单不重叠——否则接入时的判定自相矛盾", () => {
    for (const f of HTTP_BEARING_FEATURES) {
      expect(STDIO_SAFE_FEATURES, `${f} 同时在「带 HTTP」与「stdio 安全」两张名单里`).not.toContain(f);
    }
    // 做空防护：名单不能是空的，否则下面的交集判定恒真。
    expect(HTTP_BEARING_FEATURES.length).toBeGreaterThan(8);
    expect(STDIO_SAFE_FEATURES.length).toBeGreaterThan(4);
  });

  /**
   * mcpbridge 的依赖面：一张明列的名单 + rmcp 的逐特性过筛。
   *
   * 这条取代了原先那条「依赖列表必须为空」的占位绊线（2026-08-25，MCP 批次
   * 接入 fs_policy / fs_ai / serde 时按它自己注释里的指引替换）。
   *
   * 为什么是**名单**而不是「只查 rmcp」：会拉 HTTP 进来的不止 rmcp。
   * 名单让「加一个新依赖」这个动作必须在本文件里登记一次，而登记的那一刻
   * 就是回答「它会不会开出监听/出站」的时刻。
   */
  it("mcpbridge 的依赖面：只允许明列的名字，rmcp 还要逐特性过筛", () => {
    const toml = mcpbridgeToml();
    const deps = depEntries(toml);
    expect(deps.length, "扫查为空——门禁会恒真").toBeGreaterThan(0);

    // 已登记的依赖。每一条后面是「它为什么不构成出站面」。
    const ALLOWED: Record<string, string> = {
      serde: "纯序列化，无 IO",
      serde_json: "同上",
      thiserror: "错误类型派生宏",
      tracing: "日志门面，不自带 exporter",
      tokio: "运行时；MCP 只用它的 stdio 与定时器，不用 net",
      fs_policy: "本仓策略引擎，无依赖",
      fs_ai: "本仓 AI crate。它是全仓唯一允许声明 HTTP 客户端的出货 crate，" +
        "mcpbridge 因此传递地含 reqwest——这是刻意接受的（要与内置 Agent 共用同一个闸门与 run_command 契约），" +
        "由 crates/mcpbridge/src/lib.rs 的 this_crate_never_speaks_http 逐文件扫源码兜底",
      rmcp: "官方 MCP SDK；必须 default-features=false 且特性经下面的名单过筛",
    };
    for (const { table, name } of deps) {
      expect(
        Object.keys(ALLOWED),
        `crates/mcpbridge 的 [${table}] 出现了未登记的依赖 ${name}——` +
          `请先确认它不开监听、不出站，然后加进本用例的 ALLOWED 并写明理由`,
      ).toContain(name);
    }

    // rmcp 尚未接入时这段不跑；接入后必须跑，且不许靠改写声明形式绕过。
    const rmcpDeclared = deps.some((d) => d.name === "rmcp");
    const line = /^\s*rmcp\s*=\s*(.+)$/m.exec(toml)?.[1] ?? "";
    if (rmcpDeclared) {
      // 子表形式（[dependencies.rmcp] 分行写）会让下面的特性正则读不到东西，
      // 从而把一条真判定悄悄变成空转。宁可要求单行内联表。
      expect(
        line,
        "rmcp 必须写成单行内联表（rmcp = { ... }）——分行的 [dependencies.rmcp] 会让特性判定读不到列表而空转",
      ).not.toBe("");
      expect(
        line,
        "rmcp 必须显式关掉默认特性——default 里带 server，且将来可能加别的",
      ).toMatch(/default-features\s*=\s*false/);
      const feats = [...line.matchAll(/"([a-z0-9_-]+)"/g)].map((m) => m[1]);
      expect(feats.length, "rmcp 的特性列表为空——逐特性判定会恒真").toBeGreaterThan(0);
      for (const f of feats) {
        expect(HTTP_BEARING_FEATURES, `rmcp 开了带 HTTP 的特性 ${f}`).not.toContain(f);
        expect(STDIO_SAFE_FEATURES, `rmcp 开了名单外的特性 ${f}，请先确认它不拉 HTTP`).toContain(f);
      }
    }
  });

  /**
   * 上一条的**反向自检**：把它喂一份伪造的、开了 HTTP 特性的声明，它必须能判红。
   *
   * 没有这条的话，「rmcp 尚未接入 ⇒ 那段 if 不跑」与「那段 if 写错了」
   * 在今天完全不可区分——而等到真接入时，谁也不会回头验证判定本身是对的。
   */
  it("上一条的判定逻辑本身能失败（用伪造声明自检）", () => {
    const judge = (line: string): string | null => {
      if (!/default-features\s*=\s*false/.test(line)) return "没关默认特性";
      const feats = [...line.matchAll(/"([a-z0-9_-]+)"/g)].map((m) => m[1]);
      if (feats.length === 0) return "特性列表为空";
      for (const f of feats) {
        if (HTTP_BEARING_FEATURES.includes(f)) return `带 HTTP 的特性 ${f}`;
        if (!STDIO_SAFE_FEATURES.includes(f)) return `名单外的特性 ${f}`;
      }
      return null;
    };
    // 该放行的
    expect(
      judge('{ version = "3.1", default-features = false, features = ["server", "macros", "transport-io"] }'),
    ).toBeNull();
    // 该拦的，四种坏法各一条
    expect(judge('{ version = "3.1", features = ["server"] }')).toBe("没关默认特性");
    expect(judge('{ version = "3.1", default-features = false }')).toBe("特性列表为空");
    expect(
      judge('{ version = "3.1", default-features = false, features = ["server", "transport-streamable-http-server"] }'),
    ).toBe("带 HTTP 的特性 transport-streamable-http-server");
    expect(
      judge('{ version = "3.1", default-features = false, features = ["server", "something-new"] }'),
    ).toBe("名单外的特性 something-new");
  });

  it("现在全仓没有任何 HTTP 服务端框架（接入 MCP 时最容易顺手引进来的东西）", () => {
    // rmcp 的 axum 只在它自己的 dev-dependencies 里、不进我们的构建图。
    // 但「照着 rmcp 的 example 抄一段」是很自然的动作，而那些 example 用 axum。
    const SERVER_FRAMEWORKS = ["axum", "actix-web", "warp", "rocket", "tide", "poem", "salvo"];
    // 出货清单 + mcpbridge（后者此刻还不在出货清单里，见上面 mcpbridgeToml 的注释）
    const targets = [...new Set([...SHIPPED_MANIFESTS, "crates/mcpbridge/Cargo.toml"])];
    expect(targets.length, "扫查目标为空——门禁会恒真").toBeGreaterThan(3);
    for (const p of targets) {
      for (const { table, name } of depEntries(read(p))) {
        expect(
          SERVER_FRAMEWORKS,
          `${p} 的 [${table}] 引入了 HTTP 服务端框架 ${name}——本程序不监听端口`,
        ).not.toContain(name);
      }
    }
  });
});

/**
 * RDP helper 的出站面（M-RDP 绊线）。
 *
 * helper 是一个**随安装包一起发的独立二进制**，跑在自己的工作区、自己的
 * Cargo.lock 里（IronRDP 与 russh 的依赖树不能相遇，见 `rdp-helper/Cargo.toml`
 * 头部）。它已经被加进 `SHIPPED_MANIFESTS` 的种子，所以上面那些
 * HTTP_CLIENTS / TELEMETRY_CRATES 判定已经覆盖它。
 *
 * 本节钉的是它**特有**的那条约束：`ironrdp-tokio` 的 `reqwest` feature。
 *
 * 那个 feature 只服务于 Kerberos 的 KDC-proxy over HTTP。本仓裁定 RDP 只做
 * NTLM/口令认证，于是不开它 = 依赖图里零 HTTP 客户端（spike 实测 665 行依赖树
 * 零命中）。而**开它只要在 features 数组里加一个词**——那种改动在 review 里
 * 太容易滑过去，且后果是整个出站面在没人注意的情况下扩大。
 *
 * 将来真要加 Kerberos，那应当是一次显式扩大出站面、并重写本节的决定。
 */
describe("RDP helper 的出站面：零网络出口（架构上就没有）", () => {
  const HELPER_TOML = "rdp-helper/Cargo.toml";

  it("helper 清单在出货扫描表里——不在的话上面所有判定都对它视而不见", () => {
    expect(
      SHIPPED_MANIFESTS,
      "rdp-helper 是出货二进制，必须进出货清单（它不是 app 的 path 依赖，推导看不见它）",
    ).toContain(HELPER_TOML);
  });

  it("ironrdp 的任何依赖都不得开 reqwest 系特性（那是 Kerberos KDC-proxy 的 HTTP 路径）", () => {
    const src = read(HELPER_TOML);
    // 逐行看依赖行里的 features 数组。只 grep 全文 "reqwest" 会被注释骗过——
    // 而本文件头部恰恰在**解释** reqwest 为什么不能开，那段话不该把自己判死。
    const stripped = src.replace(/^\s*#.*$/gm, "");
    for (const line of stripped.split("\n")) {
      if (!/^\s*ironrdp/.test(line)) continue;
      const feats = [...line.matchAll(/"([a-z0-9_-]+)"/g)].map((m) => m[1]);
      for (const f of feats) {
        expect(
          f,
          `${HELPER_TOML} 里 ${line.trim()} 开了 ${f}——reqwest 系特性会把 HTTP 客户端拉进出货图`,
        ).not.toMatch(/reqwest/);
      }
    }
  });

  it("helper 不得直接声明任何网络型依赖——它的传输是主程序给的 stdio", () => {
    // 这一条比上面的 HTTP_CLIENTS 名单更宽：helper 连**原始套接字**都不该有。
    // 它的整个设计前提是「除 stdin/stdout 没有别的 I/O」，那条前提一旦破了，
    // 「跳板免费」与「碰不到 Vault」两个性质同时失效。
    const NETWORK_CRATES = [
      "socket2", "tokio-tungstenite", "quinn", "trust-dns-resolver",
      "hickory-resolver", "async-std", "smol", "mio",
    ];
    for (const { table, name } of depEntries(read(HELPER_TOML))) {
      expect(
        NETWORK_CRATES,
        `${HELPER_TOML} 的 [${table}] 直接声明了网络型依赖 ${name}——helper 不该有自己的网络出口`,
      ).not.toContain(name);
    }
  });

  it("helper 的 tokio 不得开 net 特性——那是它拿到套接字的唯一途径", () => {
    const src = read(HELPER_TOML).replace(/^\s*#.*$/gm, "");
    const line = src.split("\n").find((l) => /^\s*tokio\s*=/.test(l));
    expect(line, "helper 必须显式声明 tokio（好让这条判定有东西可查）").toBeTruthy();
    const feats = [...line!.matchAll(/"([a-z0-9_-]+)"/g)].map((m) => m[1]);
    expect(feats, "tokio 的 net 特性会给 helper 一个网络出口").not.toContain("net");
    expect(feats, 'tokio 的 "full" 会把 net 一起带进来').not.toContain("full");
    // 反向：它确实需要的那些必须在，否则这条判定可能是在一个空数组上恒真。
    expect(feats, "前提：helper 真的在用 tokio 的 stdio").toContain("io-std");
  });
});
