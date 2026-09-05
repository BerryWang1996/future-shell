import { describe, it, expect } from "vitest";
import fs from "node:fs";
import path from "node:path";

/**
 * 跨语言**字符串枚举**契约核对。
 *
 * 已有的三道门禁各管一段：ipc-contract 管命令名与参数名，event-contract 管事件名与载荷键，
 * connect-state 管连接态那一条链。三者都不看**取值**——而本仓被取值咬过两次，都是 P0：
 *
 *  - `hostkey-accept-record-choice-mismatch`：前端发 `accept_persist`，后端认 `accept_record`，
 *    后端当时还留着 `_ => Refuse` 兜底，于是**每一次首连 TOFU 的「接受并记录」都被静默翻译成拒绝**；
 *  - `hostkey-choice-string-mismatch`：同一处的 `reject` ↔ `refuse`。
 *
 * 两次都修了，也各自补了测试——但补的是**各自那一侧**：HostKeyDialog.test.ts 钉前端发什么
 * （5 个投递点逐一有行为用例），auth_cmd.rs 的 `contract_choices_parse` 钉后端认什么。两套
 * 测试之间没有任何连接：把 Rust 的字面量改成 `accept_and_record`，两边依旧全绿，P0 原样复发。
 * 本文件补的就是那根连接——只补连接，不重复各自已有的行为覆盖。
 *
 * 逐字比较，不做大小写/命名风格归一：Tauri v2 只对**命令参数名**做 camelCase↔snake_case，
 * 取值一律原样过线。归一恰好会把这一整类缺陷抹平。
 */

const APP_SRC = path.resolve(process.cwd(), "../app/src");
const CONNMGR_SRC = path.resolve(process.cwd(), "../crates/connmgr/src");
const SSHENGINE_SRC = path.resolve(process.cwd(), "../crates/sshengine/src");
const VAULT_SRC = path.resolve(process.cwd(), "../crates/vault/src");
const FE_SRC = path.resolve(process.cwd(), "src");

const read = (p: string) => fs.readFileSync(p, "utf8");

/**
 * 一次性求值 + 记忆化。**扫描件与其派生量一律经它取用，不在模块体/describe 体里直接求值。**
 *
 * 理由不是风格：`read`（文件不存在即抛）、`wireName`（不认识的 rename_all 即抛）、
 * `tsFieldUnion` / `jsonFieldLiterals`（歧义即抛）都会抛，而**抛在模块体或 describe 体里，
 * vitest 记的是「收集失败」而不是「用例失败」**——本文件的全部断言会一起从 total 里消失
 * （total 掉、failed 仍是 0），而不是该红的那几条转红。CI 依旧红（收集失败退出码非 0），
 * 所以不会假绿；坏在诊断指向「这个文件没跑」，且同一次运行里本文件其余覆盖被静默丢掉。
 *
 * 这不是假想：V26 的 L1 变异体（改名藏掉 TransferQueueDrawer.svelte）实测让本文件
 * `26 → 0`、总数 325 → 299。egress-contract.test.ts 与 settings-wiring.test.ts 已先后按同一
 * 手法修过，这里是同类里最后一处大面。
 *
 * 抛出不被吞：`done` 只在成功后置位，因此每个依赖它的 `it` 都会各自重跑并各自转红。
 */
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

/**
 * Rust 认得、但前端从不发送的取值。**每一项都必须写明理由**，且由下方的「别名只许指向拒绝」
 * 一并把关：宽容只施于 fail-closed 方向。
 */
const RUST_ONLY_ALIASES: Record<string, string> = {
  reject:
    "`refuse` 的近义词别名。把「拒绝」的近义词认成拒绝，最坏是用户重试一次；反向猜测则是拿主机密钥校验做赌注。",
};

// ---------------------------------------------------------------------------
// 扫描件（与 event-contract.test.ts 同源：剥注释一律补等量换行，行号不漂）
// ---------------------------------------------------------------------------

const blankKeepingLines = (s: string): string => s.replace(/[^\n]/g, "");

function stripRust(src: string): string {
  return src.replace(/\/\*[\s\S]*?\*\//g, blankKeepingLines).replace(/(^|[^:])\/\/[^\n]*/g, "$1");
}

function stripJs(src: string): string {
  return src
    .replace(/<!--[\s\S]*?-->/g, blankKeepingLines)
    .replace(/\/\*[\s\S]*?\*\//g, blankKeepingLines)
    .replace(/(^|[^:])\/\/[^\n]*/g, "$1");
}

/** 从 `src[i]` 的左括号找到配对的右括号；跳过字符串字面量。找不到返回 -1。 */
export function matchPair(src: string, i: number, open: string, close: string): number {
  let depth = 0;
  for (; i < src.length; i++) {
    const ch = src[i];
    if (ch === '"' || ch === "'" || ch === "`") {
      const q = ch;
      i++;
      while (i < src.length && src[i] !== q) i += src[i] === "\\" ? 2 : 1;
      continue;
    }
    if (ch === open) depth++;
    else if (ch === close && --depth === 0) return i;
  }
  return -1;
}

/** 顶层逗号切分：`Map<string, number>` / 嵌套调用里的逗号不算分隔符。 */
export function splitTop(s: string): string[] {
  const out: string[] = [];
  let depth = 0;
  let start = 0;
  for (let i = 0; i < s.length; i++) {
    const ch = s[i];
    if (ch === '"' || ch === "'" || ch === "`") {
      const q = ch;
      i++;
      while (i < s.length && s[i] !== q) i += s[i] === "\\" ? 2 : 1;
      continue;
    }
    if ("([{<".includes(ch)) depth++;
    else if (")]}>".includes(ch)) depth--;
    else if (ch === "," && depth === 0) {
      out.push(s.slice(start, i));
      start = i + 1;
    }
  }
  if (s.slice(start).trim() || out.length) out.push(s.slice(start));
  return out;
}

/**
 * 找出形参标注为 `type` 的函数：名字 → 该形参的位置。
 *
 * 锚在**类型标注**而不是函数名上：日后多加一个投递助手，它一样会被扫出来。
 */
export function fnsTakingType(src: string, type: string): Map<string, number> {
  const out = new Map<string, number>();
  const re = /function\s+([A-Za-z_$][\w$]*)\s*\(([^)]*)\)/g;
  const want = new RegExp(`:\\s*${type}\\s*$`);
  let m: RegExpExecArray | null;
  while ((m = re.exec(src))) {
    const i = splitTop(m[2]).findIndex((p) => want.test(p.trim()));
    if (i >= 0) out.set(m[1], i);
  }
  return out;
}

/** `name(a, "lit", c)` 中第 idx 个实参是字符串字面量时收集之（变量实参跳过：它在自己的源头受检）。 */
export function literalArgsAt(src: string, name: string, idx: number): string[] {
  const out: string[] = [];
  const re = new RegExp(`\\b${name}\\s*\\(`, "g");
  let m: RegExpExecArray | null;
  while ((m = re.exec(src))) {
    const open = re.lastIndex - 1;
    const close = matchPair(src, open, "(", ")");
    if (close < 0) continue;
    const arg = (splitTop(src.slice(open + 1, close))[idx] ?? "").trim();
    const lit = /^"([^"]*)"$|^'([^']*)'$/.exec(arg);
    if (lit) out.push(lit[1] ?? lit[2]);
  }
  return out;
}

/** 取 `fn <name>` 的函数体（不含最外层花括号）。 */
export function fnBody(src: string, name: string): string {
  const i = src.search(new RegExp(`\\bfn\\s+${name}\\b`));
  if (i < 0) return "";
  const open = src.indexOf("{", i);
  const close = matchPair(src, open, "{", "}");
  return close < 0 ? "" : src.slice(open + 1, close);
}

/** 取 JS/TS `function <name>` 的函数体（不含最外层花括号）。 */
export function jsFnBody(src: string, name: string): string {
  const i = src.search(new RegExp(`\\bfunction\\s+${name}\\b`));
  if (i < 0) return "";
  const open = src.indexOf("{", i);
  const close = matchPair(src, open, "{", "}");
  return close < 0 ? "" : src.slice(open + 1, close);
}

/** 函数体/片段里写死的双引号字符串字面量。 */
export function strLits(s: string): string[] {
  return [...s.matchAll(/"([^"]*)"/g)].map((x) => x[1]);
}

/**
 * `Kind::Alpha => "alpha",` 形态的臂：**变体名 → 过线字面量**。
 *
 * `matchArms` 只认左边是字符串字面量的臂（解析器方向）；`aad_tag`/`wire_tag` 这类
 * 生产方向的函数，臂的左边是枚举路径，它一条都看不见——直接拿它去扫会静默得到空集合，
 * 而空集合会让下游的"两侧一致"比较恒真。
 */
export function variantTagArms(body: string): Map<string, string> {
  const out = new Map<string, string>();
  const re = /(?:^|\n)\s*(?:[A-Za-z_]\w*::)?([A-Z]\w*)\s*=>\s*"([^"]*)"/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(body))) out.set(m[1], m[2]);
  return out;
}

/**
 * `<select bind:value={name} …> … </select>` 块里写死的 `value="…"` 字面量。
 *
 * 锚在 `bind:value` 上而不是 `data-testid` 上：决定过线取值的是这个绑定，
 * testid 只是测试用的把手，改掉它不影响发出去的串。
 */
export function selectOptionValues(src: string, bindTo: string): string[] {
  const i = src.search(new RegExp(`bind:value=\\{${bindTo}\\}`));
  if (i < 0) return [];
  const end = src.indexOf("</select>", i);
  if (end < 0) return [];
  return [...src.slice(i, end).matchAll(/<option[^>]*\bvalue="([^"]*)"/g)].map((x) => x[1]);
}

/** `"a" | "b" => RHS` 与 `_ => RHS`。lits 为空即兜底臂。 */
export function matchArms(body: string): { lits: string[]; rhs: string }[] {
  const out: { lits: string[]; rhs: string }[] = [];
  const re = /(?:^|\n)\s*((?:"[^"]*"(?:\s*\|\s*"[^"]*")*)|_)\s*=>\s*([^,\n]+)/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(body))) {
    const lits = m[1] === "_" ? [] : [...m[1].matchAll(/"([^"]*)"/g)].map((x) => x[1]);
    out.push({ lits, rhs: m[2].trim() });
  }
  return out;
}

/** Rust 枚举的变体名 + 其 `#[serde(rename_all = "…")]`（没有则 null）。 */
export function rustEnum(src: string, name: string): { variants: string[]; renameAll: string | null } {
  const i = src.search(new RegExp(`\\benum\\s+${name}\\b`));
  if (i < 0) return { variants: [], renameAll: null };
  const head = src.slice(Math.max(0, i - 400), i);
  const rn = /#\[serde\([^\]]*rename_all\s*=\s*"([^"]+)"/.exec(head);
  const open = src.indexOf("{", i);
  const close = matchPair(src, open, "{", "}");
  const variants = src
    .slice(open + 1, close)
    .split("\n")
    .map((l) => l.replace(/#\[[^\]]*\]/g, "").trim())
    .map((l) => /^([A-Z][A-Za-z0-9_]*)\s*,?$/.exec(l)?.[1])
    .filter((x): x is string => !!x);
  return { variants, renameAll: rn ? rn[1] : null };
}

/**
 * 变体名 → 过线时的字面量。
 *
 * 不认识的 `rename_all` **直接抛**，不默默按 snake_case 处理：把 rename_all 改成 camelCase
 * 正是这类缺陷的典型走法，静默套用旧规则等于本测试自己把缺陷抹平。
 */
export function wireName(variant: string, renameAll: string | null): string {
  if (renameAll === null) return variant; // serde 默认：变体名原样
  if (renameAll === "snake_case") return variant.replace(/([a-z0-9])([A-Z])/g, "$1_$2").toLowerCase();
  throw new Error(`rename_all="${renameAll}" 本测试不认识：请补上对应转换，别让它按旧规则蒙混过去`);
}

/** `type X = "a" | "b";` → ["a","b"]。 */
export function tsUnion(src: string, name: string): string[] {
  const m = new RegExp(`type\\s+${name}\\s*=\\s*([^;]+);`).exec(src);
  return m ? [...m[1].matchAll(/"([^"]*)"/g)].map((x) => x[1]) : [];
}

/**
 * 字段位上的内联联合：`verify?: "a" | "b";` → ["a","b"]。
 *
 * 命中多于一处就抛：同名字段挂了两个不同的联合，"比对哪一个"就成了掷骰子，
 * 静默取第一个正是把契约核对变成安家家酒的做法。
 */
export function tsFieldUnion(src: string, field: string): string[] {
  const re = new RegExp(`\\b${field}\\??\\s*:\\s*("[^"]*"(?:\\s*\\|\\s*"[^"]*")*)\\s*;`, "g");
  const all = [...src.matchAll(re)];
  if (all.length > 1) throw new Error(`字段 ${field} 有 ${all.length} 处联合声明，无法确定钉哪一个`);
  return all.length ? [...all[0][1].matchAll(/"([^"]*)"/g)].map((x) => x[1]) : [];
}

/**
 * Rust `pub struct X { pub a: u32, … }` 的 **pub 字段名**（按声明顺序）+ 紧贴其上的属性行。
 *
 * 只认 `pub`：非 pub 字段既出不了 crate，也不会出现在 IPC 载荷里，把它算进契约等于
 * 要求前端去读一个根本不过线的名字。属性行一并取回，是因为一个 `#[serde(rename_all)]`
 * 就能让所有字段换一套过线名字，而字段名本身**一个字都没改**。
 */
export function rustStruct(src: string, name: string): { fields: string[]; attrs: string } {
  const i = src.search(new RegExp(`\\bstruct\\s+${name}\\b`));
  if (i < 0) return { fields: [], attrs: "" };
  // 往回收 `#[…]` 行：`src.slice(0, i)` 的最后一段是 `pub ` 那半行，故从倒数第二行起。
  // 空行要**跨过去**：rustc 不认为空行隔断属性与它修饰的项，剥注释后留下的空行更是
  // 处处都是。在空行处停下，等于一个隔了空行写的 `rename_all` 就能躲开本门禁。
  const before = src.slice(0, i).split("\n");
  const attrs: string[] = [];
  for (let k = before.length - 2; k >= 0; k--) {
    const l = before[k].trim();
    if (!l) continue;
    if (!l.startsWith("#[")) break;
    attrs.unshift(l);
  }
  const open = src.indexOf("{", i);
  const close = matchPair(src, open, "{", "}");
  const fields =
    close < 0
      ? []
      : src
          .slice(open + 1, close)
          .split("\n")
          .map((l) => /^\s*pub\s+([A-Za-z_]\w*)\s*:/.exec(l)?.[1])
          .filter((x): x is string => !!x);
  return { fields, attrs: attrs.join("\n") };
}

/** TS `export interface X { a: number; b?: string }` → 字段名（按声明顺序）。 */
export function tsInterfaceFields(src: string, name: string): string[] {
  const i = src.search(new RegExp(`\\binterface\\s+${name}\\b`));
  if (i < 0) return [];
  const open = src.indexOf("{", i);
  const close = matchPair(src, open, "{", "}");
  return close < 0
    ? []
    : src
        .slice(open + 1, close)
        .split("\n")
        .map((l) => /^\s*([A-Za-z_$][\w$]*)\??\s*:/.exec(l)?.[1])
        .filter((x): x is string => !!x);
}

/** `const NAME: Record<…> = { a: …, b: … };` → 顶层键名。 */
export function recordKeys(src: string, name: string): string[] {
  const i = src.search(new RegExp(`\\b(?:const|let)\\s+${name}\\b`));
  if (i < 0) return [];
  const open = src.indexOf("{", i);
  const close = matchPair(src, open, "{", "}");
  return src
    .slice(open + 1, close)
    .split("\n")
    .map((l) => /^\s*"?([A-Za-z_$][\w$]*)"?\s*:/.exec(l)?.[1])
    .filter((x): x is string => !!x);
}

/**
 * `serde_json::json!` 里某个字段位上写死的字符串取值：
 * `"kind": match hint { A => "changed", _ => "tofu" }` → ["changed","tofu"]。
 *
 * 这类取值根本不是 Rust 枚举，`rustEnum` 看不见它——而它照样是过线契约。
 *
 * 同一文件里出现第二个同名字段就抛：静默取第一个，等于新增的生产点从诞生起就在门禁之外。
 */
export function jsonFieldLiterals(src: string, field: string): string[] {
  const all = [...src.matchAll(new RegExp(`"${field}"\\s*:\\s*`, "g"))];
  if (all.length > 1) throw new Error(`"${field}" 有 ${all.length} 个生产点，无法确定钉哪一个`);
  const m = all[0];
  if (!m) return [];
  const val = src.slice(m.index + m[0].length);
  // 值是 `match … { … }` → 取整个配对块；否则是裸字面量 → 只取到行尾，
  // 免得把后面的字段一起吃进来。
  const isMatch = /^match\b/.test(val);
  const seg = isMatch
    ? val.slice(0, matchPair(val, val.indexOf("{"), "{", "}") + 1)
    : val.slice(0, val.indexOf("\n") < 0 ? val.length : val.indexOf("\n"));
  return [...seg.matchAll(/"([^"]*)"/g)].map((x) => x[1]);
}

// ---------------------------------------------------------------------------

// 十三份扫描件。全部 lazy：任一源文件被改名/删除时，只红真的读它的那几条，
// 本文件其余用例照跑照报（见上方 `lazy` 的说明与 V26/L1 的实测数据）。
const AUTH_RS = lazy(() => stripRust(read(path.join(APP_SRC, "commands", "auth_cmd.rs"))));
const EVENTS_RS = lazy(() => stripRust(read(path.join(APP_SRC, "events.rs"))));
const MODEL_RS = lazy(() => stripRust(read(path.join(CONNMGR_SRC, "model.rs"))));
const VERIFY_RS = lazy(() => stripRust(read(path.join(SSHENGINE_SRC, "verify.rs"))));
const HOSTKEY_RS = lazy(() => stripRust(read(path.join(SSHENGINE_SRC, "hostkey.rs"))));
const SECRETS_RS = lazy(() => stripRust(read(path.join(SSHENGINE_SRC, "secrets.rs"))));
const STORE_RS = lazy(() => stripRust(read(path.join(VAULT_SRC, "store.rs"))));
const HK_DIALOG = lazy(() => stripJs(read(path.join(FE_SRC, "components", "HostKeyDialog.svelte"))));
const HK_TEST = lazy(() => stripJs(read(path.join(FE_SRC, "components", "HostKeyDialog.test.ts"))));
const PROFILE_DLG = lazy(() => stripJs(read(path.join(FE_SRC, "components", "ProfileDialog.svelte"))));
const DRAWER = lazy(() => stripJs(read(path.join(FE_SRC, "components", "TransferQueueDrawer.svelte"))));
const TRANSFERS_TS = lazy(() => stripJs(read(path.join(FE_SRC, "lib", "transfers.ts"))));
const TYPES_TS = lazy(() => stripJs(read(path.join(FE_SRC, "lib", "types.ts"))));
const CONNECT_FAILURE_RS = lazy(() => stripRust(read(path.join(APP_SRC, "connect_failure.rs"))));
const CF_PANEL = lazy(() => stripJs(read(path.join(FE_SRC, "components", "ConnectFailurePanel.svelte"))));

// 派生量同样 lazy：它们求值即触发上面的 read，写成常量等于把读文件搬回模块体。
const ARMS = lazy(() => matchArms(fnBody(AUTH_RS(), "parse_hostkey_choice")));
const RUST_ACCEPTED = lazy(() => {
  const m = new Map<string, string>();
  for (const a of ARMS()) for (const l of a.lits) m.set(l, a.rhs);
  return m;
});

const FE_UNION = lazy(() => tsUnion(HK_DIALOG(), "HostKeyChoice"));
const SENDERS = lazy(() => fnsTakingType(HK_DIALOG(), "HostKeyChoice"));
const FE_SENT = lazy(() =>
  [...SENDERS()].flatMap(([name, idx]) => literalArgsAt(HK_DIALOG(), name, idx)),
);

describe("主机密钥裁决取值（两次 P0 的原址）", () => {
  it("三侧都真的解析到了东西——任一侧空集合都会让下面的比较恒真", () => {
    expect(ARMS().length, "parse_hostkey_choice 的 match 臂").toBe(4);
    expect(RUST_ACCEPTED().size, "Rust 认得的字面量数").toBe(4);
    expect(FE_UNION().length, "HostKeyDialog 的 HostKeyChoice 联合类型").toBe(3);
    // 投递点：Esc、拒绝键、仅本次、接受并记录，加上 dismissForVaultLock 直接调 send 的那处。
    // 新增投递入口就更新这个数字，并顺手确认它发的取值在联合类型里。
    expect([...SENDERS().keys()].sort(), "收 HostKeyChoice 形参的函数").toEqual(["decide", "send"]);
    expect(FE_SENT().length, "写死字面量的投递点").toBe(5);
  });

  it("前端能发出的每一个取值，后端都认得", () => {
    const accepted = RUST_ACCEPTED();
    for (const c of new Set(FE_SENT())) {
      expect(accepted.has(c), `前端会发 "${c}"，但 parse_hostkey_choice 不认——这正是那两次 P0`).toBe(
        true,
      );
    }
  });

  it("投递点实发的取值与联合类型一致（多一个即越界，少一个即死枝）", () => {
    // 联合类型只是类型断言：`decide("accept")` 会被 tsc 拦住，但 `send(p, x as HostKeyChoice)`
    // 或日后放宽成 string 的形参不会。这里比的是**源码里真写下的字面量**。
    expect([...new Set(FE_SENT())].sort()).toEqual([...FE_UNION()].sort());
  });

  it("行为测试里那份手写契约集合没有和联合类型走散", () => {
    // HostKeyDialog.test.ts 里 `const contract = new Set([...])` 是第三份拷贝。它若单独漂了，
    // 那批行为用例就会开始为一套过时的契约背书。
    const m = /const\s+contract\s*=\s*new Set\(\[([^\]]*)\]\)/.exec(HK_TEST());
    expect(m, "锚点失效（变量改名或写法变了）：请同步更新本断言，别删掉").not.toBeNull();
    const listed = [...m![1].matchAll(/"([^"]*)"/g)].map((x) => x[1]).sort();
    expect(listed).toEqual([...FE_UNION()].sort());
  });

  it("三个取值各自映射到预期语义，接受与拒绝不得对调", () => {
    const rhs = (lit: string) => (RUST_ACCEPTED().get(lit) ?? "").replace(/\s+/g, "");
    expect(rhs("accept_record")).toBe("Some(HostKeyChoice::AcceptAndRecord)");
    expect(rhs("accept_once")).toBe("Some(HostKeyChoice::AcceptOnce)");
    expect(rhs("refuse")).toBe("Some(HostKeyChoice::Refuse)");
  });

  it("契约外取值必须落到 None，不得被兜底猜成某个裁决", () => {
    const fallback = ARMS().filter((a) => a.lits.length === 0);
    expect(fallback.length, "没有 `_` 兜底臂 = match 不穷尽，编译就过不了；有两条则是解析器读错了").toBe(1);
    // 老代码是 `_ => Refuse`：契约漂移被翻译成一个**看起来合理**的裁决，于是没人发现。
    // 现在必须是 None，由调用方回 Err 让漂移当场可见。
    expect(fallback[0].rhs.replace(/\s+/g, "")).toBe("None");
  });

  it("Rust 独有的别名必须登记在册，且只许指向拒绝", () => {
    const extra = [...RUST_ACCEPTED().keys()].filter((k) => !FE_UNION().includes(k));
    expect(extra.sort(), "Rust 多认了前端从不发送的取值：要么删掉，要么登记理由").toEqual(
      Object.keys(RUST_ONLY_ALIASES).sort(),
    );
    for (const alias of extra) {
      // 宽容只施于 fail-closed 方向：别名指向「接受」意味着一个前端根本不会发、
      // 却能让主机密钥校验放行的取值——那是白送的攻击面。
      expect(RUST_ACCEPTED().get(alias)?.replace(/\s+/g, ""), `别名 "${alias}" 指向了非拒绝语义`).toBe(
        "Some(HostKeyChoice::Refuse)",
      );
    }
  });
});

describe("HostKeyPolicy（Rust 枚举 ↔ TS 联合类型）", () => {
  const HKP = lazy(() => rustEnum(MODEL_RS(), "HostKeyPolicy"));

  it("Rust 侧读得到变体与 rename_all", () => {
    const { variants, renameAll } = HKP();
    expect(variants).toEqual(["Tofu", "Strict", "FingerprintPinned"]);
    expect(renameAll, "改成别的风格会让所有已存档 Profile 的策略反序列化失败").toBe("snake_case");
  });

  it("过线字面量与 types.ts 的联合类型逐字一致", () => {
    const { variants, renameAll } = HKP();
    const wire = variants.map((v) => wireName(v, renameAll)).sort();
    const ts = tsUnion(TYPES_TS(), "HostKeyPolicy").sort();
    expect(ts.length, "types.ts 里没解析到 HostKeyPolicy").toBe(3);
    expect(ts, "少一个变体 = 界面永远选不到它；多一个 = 存进去后端反序列化失败").toEqual(wire);
  });
});

describe("AutoExec（Rust 枚举 ↔ TS 联合类型）", () => {
  // 这一组**已经咬过一次**：types.ts 旧定义把它写成 boolean，前端发 `false`，Rust 侧当场
  // 反序列化失败（见 types.ts:19 的注释）。当时靠人眼发现，此后仍无门禁。
  const AE = lazy(() => rustEnum(MODEL_RS(), "AutoExec"));

  it("过线字面量与 types.ts 的联合类型逐字一致", () => {
    const { variants, renameAll } = AE();
    const wire = variants.map((v) => wireName(v, renameAll)).sort();
    expect(wire, "Rust 侧没解析到 AutoExec").toEqual(["off", "read_only", "with_confirm"]);
    expect(tsUnion(TYPES_TS(), "AutoExec").sort()).toEqual(wire);
  });

  it("ProfileDialog 造的默认值落在契约内", () => {
    // 新建 Profile 时前端自造 AiPolicy 默认值；这个字面量与 Rust 的 `#[default] Off` 之间
    // 同样没有编译期联系。
    const { variants, renameAll } = AE();
    const lits = [...PROFILE_DLG().matchAll(/auto_execute:\s*"([^"]*)"/g)].map((x) => x[1]);
    expect(lits.length, "ProfileDialog 里没找到 auto_execute 字面量：锚点失效了").toBeGreaterThan(0);
    for (const l of lits) expect(variants.map((v) => wireName(v, renameAll))).toContain(l);
  });
});

describe("VerifyOutcome（传后校验结果，Rust 枚举 ↔ 前端三处消费点）", () => {
  // 传后校验是审计点名的数据完整性一环。`mismatch` 一旦漂移：`class:bad` 不再命中，
  // 「校验失败」的红标永不渲染，VERIFY_BADGE[r.verify] 取到 undefined——用户看到的是
  // 一个没有告警、也没有文案的传输行，据此认为损坏的文件传成功了。
  // 惰性求值不是风格选择：wireName 遇到不认识的 rename_all 会抛、read 读不到文件也会抛，
  // 而 describe 体里抛出的异常会让**整个文件收集失败**——整份文件的断言一起从统计里消失，
  // 而不是该红的那几条转红。放进 it 里，抛出就只打掉依赖它的那几条，其余照跑。
  const VO = lazy(() => rustEnum(VERIFY_RS(), "VerifyOutcome"));
  const wire = () => {
    const { variants, renameAll } = VO();
    return variants.map((v) => wireName(v, renameAll)).sort();
  };

  it("过线字面量与 transfers.ts 的内联联合逐字一致", () => {
    expect(wire()).toEqual(["mismatch", "sha256_match", "size_only_match", "unverified"]);
    expect(tsFieldUnion(TRANSFERS_TS(), "verify").sort()).toEqual(wire());
  });

  it("每个结果都有文案，且没有为不存在的结果留文案", () => {
    // 少一项 = 界面印 undefined；多一项 = 死文案，且多半意味着 Rust 那边刚删了一个变体。
    expect(recordKeys(DRAWER(), "VERIFY_BADGE").sort()).toEqual(wire());
  });

  it("抽屉里逐字比较的取值都在契约内（红标/黄标的判据）", () => {
    const wireNow = wire();
    const cmp = [...DRAWER().matchAll(/verify\s*===\s*"([^"]*)"/g)].map((x) => x[1]);
    expect(cmp.length, "抽屉里没找到 verify === \"…\" 比较：锚点失效了").toBeGreaterThan(0);
    for (const c of cmp) expect(wireNow, `抽屉拿 "${c}" 比对，但 Rust 从不发这个值`).toContain(c);
    // 失败态必须有人管：mismatch 不在比较集合里 = 红标判据被删了。
    expect(cmp, "「校验失败」的红标判据不见了").toContain("mismatch");
  });
});

describe("hostkey prompt 的 kind（json! 内联字面量 ↔ 前端两处消费点）", () => {
  // 这个取值根本不是 Rust 枚举，是 `serde_json::json!` 里 match 出来的裸字符串，
  // rustEnum 看不见它——但它照样决定两件安全相关的事：
  //   1) `kind === "changed"` 时默认焦点落在**拒绝**键而不是接受键；
  //   2) `{#if cur.kind === "changed"}` 才渲染密钥变更的红色告警块。
  // 漂移的后果不是报错，是密钥变更弹框静默退回成一个「首次连接」样子的接受态弹框。
  // 同样惰性：多一个 "kind" 生产点时 jsonFieldLiterals 会抛，而这正是它该抛的时候——
  // 但抛在 describe 体里会把整份门禁从统计里抹掉，抛在 it 里才是"该红的红"。
  const rustKinds = () => jsonFieldLiterals(EVENTS_RS(), "kind").sort();

  it("后端发得出的 kind 取值与两侧前端声明逐字一致", () => {
    const kinds = rustKinds();
    expect(kinds, "events.rs 的 hostkey:prompt 里没解析到 kind").toEqual(["changed", "tofu"]);
    expect(tsFieldUnion(TYPES_TS(), "kind").sort(), "types.ts 侧").toEqual(kinds);
    expect(tsFieldUnion(HK_DIALOG(), "kind").sort(), "HostKeyDialog 组件内那份").toEqual(kinds);
  });

  it("组件里逐字比较的 kind 都在契约内，且「变更」分支仍有人守", () => {
    const kinds = rustKinds();
    const cmp = [...HK_DIALOG().matchAll(/kind\s*===\s*"([^"]*)"/g)].map((x) => x[1]);
    expect(cmp.length, "组件里没找到 kind === \"…\" 比较：锚点失效了").toBeGreaterThan(0);
    for (const c of cmp) expect(kinds, `组件拿 "${c}" 比对，但后端从不发这个值`).toContain(c);
    expect(cmp, "密钥变更分支（默认焦点 + 红色告警）的判据不见了").toContain("changed");
  });
});

describe("SecretKind（凭据用途：vault 存储 ↔ 引擎 ↔ 前端两个选取器）", () => {
  // 审计2 #20 之前，「这份材料是口令还是私钥」由引擎**猜明文内容**（`contains("PRIVATE KEY")`）
  // 决定。修法是让用途只来自记录自己声明的 kind，于是这个串成了一条真正的过线契约：
  // vault 用它做 AEAD 的 AAD、IPC 用它过线、前端用它决定一条记录能不能选。
  //
  // Rust ↔ Rust 那一段已由 `secret_kind_mapping_is_total_and_faithful`（app 层）逐条钉住，
  // 且钉得比源码扫描更强（它遍历 `SecretKind::ALL`，真的调函数）。本组补的是它够不着的
  // 那一段：**Rust ↔ TypeScript**。前端多一个取值 = 界面允许选一条后端必拒的记录；
  // 少一个 = 有类别的记录在界面上永远显示不出正确的名字与可用性。
  //
  // 注意不能用 `rustEnum` + `wireName` 取这组串：vault 的 `SecretKind` 没有 `rename_all`，
  // serde 名是 `Password`/`PrivateKey`/`ApiKey`（那是写进 vault.json 的形态），而过线与
  // AAD 用的是 `aad_tag()` 手写的 snake_case 串。拿 serde 名去比，比的是另一个东西。
  const VAULT_TAGS = lazy(() => variantTagArms(fnBody(STORE_RS(), "aad_tag")));
  const ENGINE_TAGS = lazy(() => variantTagArms(fnBody(SECRETS_RS(), "wire_tag")));
  const wire = () => [...VAULT_TAGS().values()].sort();

  it("四侧都真的解析到了东西——任一侧空集合都会让下面的比较恒真", () => {
    expect([...VAULT_TAGS().keys()], "vault aad_tag 的变体臂").toEqual([
      "Password",
      "PrivateKey",
      "ApiKey",
    ]);
    expect(wire(), "vault 侧过线标签").toEqual(["api_key", "password", "private_key"]);
    expect([...ENGINE_TAGS().keys()], "引擎 wire_tag 的变体臂").toEqual([
      "Password",
      "PrivateKey",
      "ApiKey",
    ]);
    expect(tsUnion(TYPES_TS(), "SecretKind").length, "types.ts 里没解析到 SecretKind").toBe(3);
    expect(recordKeys(PROFILE_DLG(), "KIND_LABEL").length, "ProfileDialog 里没解析到 KIND_LABEL").toBe(3);
  });

  it("引擎的 wire_tag 与 vault 的 aad_tag 是同一张表（同变体名、同串）", () => {
    // app 层的对偶测试已经用 `ALL` 遍历钉过这件事；这里再钉一次源码形态，是因为两个枚举
    // 分处两个互不依赖的 crate（引擎不许依赖 vault 的存储），编译器对这张表一无所知——
    // 而 app 层那份测试自己也可能被删。两处判据不同（一处调函数、一处读源码），不重叠。
    expect(Object.fromEntries(ENGINE_TAGS())).toEqual(Object.fromEntries(VAULT_TAGS()));
  });

  it("过线标签与 types.ts 的 SecretKind 联合逐字一致", () => {
    expect(
      tsUnion(TYPES_TS(), "SecretKind").sort(),
      "多一个 = 界面允许选一条后端会硬拒的记录；少一个 = 那类记录在界面上没有名字",
    ).toEqual(wire());
  });

  it("每个类别都有中文名，且没有为不存在的类别留文案", () => {
    // 少一项 = `kindLabel` 回落到原样回显那条路，界面上印出裸标签；
    // 多一项 = 死文案，多半意味着 Rust 那边刚删了一个变体而前端没跟上。
    expect(recordKeys(PROFILE_DLG(), "KIND_LABEL").sort()).toEqual(wire());
  });

  it("两个可用性判据比对的串都在契约内，且各自守住该守的那一条", () => {
    // 只提取类别参数 k 的比较，协议条件不属于 SecretKind。
    const kinds = (body: string) => [...body.matchAll(/\bk\s*===\s*"([^"]+)"/g)].map(m => m[1]);
    const cred = kinds(jsFnBody(PROFILE_DLG(), "usableAsCredential"));
    const pass = kinds(jsFnBody(PROFILE_DLG(), "usableAsPassphrase"));
    expect(cred.length, "ProfileDialog 里没找到 usableAsCredential：锚点失效了").toBeGreaterThan(0);
    expect(pass.length, "ProfileDialog 里没找到 usableAsPassphrase：锚点失效了").toBeGreaterThan(0);
    for (const c of [...cred, ...pass]) {
      expect(wire(), `前端拿 "${c}" 判定可用性，但后端从不发这个类别——该判据恒为假`).toContain(c);
    }
    // 判据本身也得对：私钥必须**能**当主凭据（漏了它，所有密钥登录的 profile 都会被界面
    // 标成不可用），而私钥口令这一栏只能收口令（放进私钥就是拿私钥去解私钥）。
    expect(cred.sort(), "SSH 主凭据的可用类别").toEqual(["password", "private_key"]);
    expect(pass, "私钥口令只能是口令类别").toEqual(["password"]);
  });

  it("行内录入的类别选项都在契约内", () => {
    // 这两个 option 的 value 直接过线进 `vault_put_secret`，而那里现在用 `from_aad_tag`
    // 解析、认不出即报错。写错一个字母的后果不再是"存成别的类别"，而是新建凭据一律失败。
    const opts = selectOptionValues(PROFILE_DLG(), "newKind");
    expect(opts, "行内录入表单的类别选项").toEqual(["password", "private_key"]);
  });

  it("两侧解析器都不得把认不出的标签兜底成某个用途", () => {
    // 前端的可用性提示、后端的硬拒，都建立在"认不出即失败"上。任何一侧改回
    // `_ => ApiKey` 这类兜底，一个拼错的类别串就会被静默变成一个具体用途。
    for (const [what, body] of [
      ["vault from_aad_tag", fnBody(STORE_RS(), "from_aad_tag")],
      ["引擎 from_wire_tag", fnBody(SECRETS_RS(), "from_wire_tag")],
    ] as const) {
      const arms = matchArms(body);
      expect(arms.filter((a) => a.lits.length > 0).flatMap((a) => a.lits).sort(), `${what} 认得的串`).toEqual(
        wire(),
      );
      const fallback = arms.filter((a) => a.lits.length === 0);
      expect(fallback.length, `${what}：没有 \`_\` 兜底臂 = match 不穷尽，编译就过不了`).toBe(1);
      expect(fallback[0].rhs.replace(/\s+/g, ""), `${what} 的兜底臂`).toBe("None");
    }
  });
});

describe("ImportSummary（known_hosts 导入回执：Rust 结构 ↔ TS 接口 ↔ 导入提示）", () => {
  // 审计2 #22 之前这里只有一个 `skipped`，它同时装着「本来就在库里」和「整行安全信息
  // 被丢掉」。现在按原因分成九格——而九格能不能到达用户，全靠**字段名逐字对上**：
  // serde 默认按字段名序列化，前端 `r.cert_authority` 取的就是那个名字。任一侧改名、
  // 加一个 `rename_all`、或前端漏读一格，后果都不是报错，而是 `undefined`——条件表达式
  // 取假，那一整段提示**静默消失**。用户看到「导入 0」，看不到「12 行通配主机没有进来」，
  // 于是以为整个 known_hosts 都已生效。这正是本次要消灭的那种「跳过 N」的翻版。
  const SUMMARY = lazy(() => rustStruct(HOSTKEY_RS(), "ImportSummary"));
  const TS_FIELDS = lazy(() => tsInterfaceFields(TYPES_TS(), "ImportSummary"));
  const TOAST = lazy(() => jsFnBody(PROFILE_DLG(), "onImportKnownHosts"));
  const used = (re: RegExp) => [...new Set([...TOAST().matchAll(re)].map((x) => x[1]))].sort();

  it("三侧都真的解析到了东西——任一侧空集合都会让下面的比较恒真", () => {
    expect(SUMMARY().fields, "hostkey.rs 的 ImportSummary 字段（顺序即声明顺序）").toEqual([
      "imported",
      "upgraded",
      "conflicted",
      "duplicate",
      "revoked",
      "cert_authority",
      "hashed",
      "pattern",
      "malformed",
    ]);
    expect(TS_FIELDS().length, "types.ts 里没解析到 ImportSummary").toBe(9);
    expect(TOAST(), "ProfileDialog 的 onImportKnownHosts 里没找到提示赋值：锚点失效了").toContain(
      "importToast =",
    );
  });

  it("Rust 字段与 types.ts 的接口逐字一致", () => {
    expect(
      TS_FIELDS().sort(),
      "多一格 = 前端读到恒为 undefined 的死字段；少一格 = 一整类结局在界面上没有出口",
    ).toEqual([...SUMMARY().fields].sort());
  });

  it("每一格都被提示读到，且都把数字印出来", () => {
    const fields = [...SUMMARY().fields].sort();
    // 引用了后端没有的字段 = 该段提示恒不显示（`undefined` 取假），和压根没写没有区别。
    expect(used(/\br\.([A-Za-z_]\w*)/g), "提示里引用的计数格").toEqual(fields);
    // 只做条件判断却不插值，用户只知道「有」，不知道有多少——「有 1 行没进来」和
    // 「有 400 行没进来」是完全不同的两件事。
    expect(used(/\$\{r\.([A-Za-z_]\w*)\}/g), "提示里真的印出数字的计数格").toEqual(fields);
  });

  it("这个结构必须按字段名原样过线（serde 默认名 = 前端取的名）", () => {
    const { attrs } = SUMMARY();
    expect(attrs, "ImportSummary 头上没解析到属性行：锚点失效了").toContain("derive");
    expect(attrs, "少了 Serialize，这个结构根本过不了线").toContain("Serialize");
    expect(
      attrs,
      "加了 rename_all 之后过线的是另一套名字，而 TS 侧的九格会一起变成 undefined——" +
        "界面从「导入 3 / 通配主机 12 行未导入」退回成一句「导入 undefined」",
    ).not.toContain("rename_all");
  });
});

describe("解析器反恒真探针", () => {
  // 上面十三份扫描件的"抛出只打掉依赖它的那几条"全建立在 lazy 真的推迟求值上。
  // 把 lazy 写成 `const v = f(); return () => v;` 一样能通过所有结构性检查
  // （声明形态还是 `= lazy(`），却让十次读全部搬回模块体——塌陷原样复发。
  it("lazy 到第一次调用才求值，且只求值一次（塌陷防护的前提本身）", () => {
    let n = 0;
    const f = lazy(() => ++n);
    expect(n, "声明当刻就求值了：惰性是假的，读文件照样发生在模块体").toBe(0);
    expect(f()).toBe(1);
    expect(f()).toBe(1);
    expect(n, "每次调用都重求一遍：十份扫描件会被反复读盘").toBe(1);
  });

  it("match 臂解析器认得多字面量或分支、兜底臂与注释", () => {
    const probe = stripRust(
      [
        "fn parse_probe(x: &str) -> Option<T> {",
        "    match x {",
        '        // "ghost_a" => Some(T::Ghost),   —— 注释里的不算',
        '        "a" => Some(T::A),',
        '        "b" | "c" => Some(T::BC),',
        "        _ => None,",
        "    }",
        "}",
      ].join("\n"),
    );
    const arms = matchArms(fnBody(probe, "parse_probe"));
    expect(arms.map((a) => a.lits)).toEqual([["a"], ["b", "c"], []]);
    expect(arms.map((a) => a.rhs)).toEqual(["Some(T::A)", "Some(T::BC)", "None"]);
  });

  it("fnBody 取的是**那一个**函数，不会串到下一个函数去", () => {
    const probe = [
      "fn first(x: &str) -> u8 {",
      "    if x.is_empty() { return 0; }",
      "    1",
      "}",
      "fn second(x: &str) -> u8 { 2 }",
    ].join("\n");
    const body = fnBody(probe, "first");
    expect(body).toContain("return 0;");
    expect(body, "串到了下一个函数：花括号配对没做对").not.toContain("fn second");
  });

  it("投递点扫描锚在类型标注上，且认得字面量在第几个实参", () => {
    const probe = stripJs(
      [
        'type C = "yes" | "no";',
        "async function post(p: Pending, choice: C) {}",
        "function pick(choice: C) { void post(head, choice); }",
        "const ignored = post(p, other);",
        'const a = () => pick("yes");',
        'const b = () => post(p, "no");',
        'const c = () => other("no");',
        '// const d = () => pick("ghost");',
      ].join("\n"),
    );
    const fns = fnsTakingType(probe, "C");
    expect([...fns]).toEqual([
      ["post", 1],
      ["pick", 0],
    ]);
    const lits = [...fns].flatMap(([n, i]) => literalArgsAt(probe, n, i));
    expect(lits.sort()).toEqual(["no", "yes"]);
  });

  it("splitTop 不被泛型与嵌套调用里的逗号骗到", () => {
    expect(splitTop("a: Map<string, number>, b: C").map((s) => s.trim())).toEqual([
      "a: Map<string, number>",
      "b: C",
    ]);
    expect(splitTop('f(x, y), "a, b"').map((s) => s.trim())).toEqual(["f(x, y)", '"a, b"']);
    expect(splitTop("")).toEqual([]);
  });

  it("枚举解析器跳过属性行，并读得到 rename_all", () => {
    const probe = [
      "#[derive(Serialize, Deserialize, Default)]",
      '#[serde(rename_all = "snake_case")]',
      "pub enum Probe {",
      "    #[default]",
      "    Alpha,",
      "    BetaGamma,",
      "}",
    ].join("\n");
    expect(rustEnum(probe, "Probe")).toEqual({ variants: ["Alpha", "BetaGamma"], renameAll: "snake_case" });
  });

  it("wireName 按 rename_all 转换，遇到不认识的规则直接抛", () => {
    expect(wireName("FingerprintPinned", "snake_case")).toBe("fingerprint_pinned");
    expect(wireName("FingerprintPinned", null)).toBe("FingerprintPinned");
    expect(() => wireName("FingerprintPinned", "camelCase")).toThrow(/不认识/);
  });

  it("tsFieldUnion 认得可选字段，且同名字段有歧义时直接抛", () => {
    expect(tsFieldUnion('  verify?: "a" | "b";', "verify")).toEqual(["a", "b"]);
    expect(tsFieldUnion('  kind: "tofu" | "changed";', "kind")).toEqual(["tofu", "changed"]);
    expect(tsFieldUnion('  other: "z";', "verify")).toEqual([]);
    expect(() => tsFieldUnion('a: "x" | "y";\nb: "p" | "q";', "[ab]")).toThrow(/无法确定钉哪一个/);
  });

  it("recordKeys 取顶层键，不把值里的冒号当键", () => {
    const probe = [
      "const M: Record<string, string> = {",
      '  alpha: "含冒号的值：像这样",',
      '  "beta": "带引号的键",',
      "};",
      "const N = { gamma: 1 };",
    ].join("\n");
    expect(recordKeys(probe, "M")).toEqual(["alpha", "beta"]);
    expect(recordKeys(probe, "Missing")).toEqual([]);
  });

  it("rustStruct 只取 pub 字段；属性行跨空行收，但不越过上一个项", () => {
    const probe = stripRust(
      [
        "#[derive(Clone)]",
        "pub struct Other { pub ghost: u32 }",
        "",
        '#[serde(rename_all = "camelCase")]',
        "", // 空行不隔断属性：这一行照样修饰 Probe，门禁必须看得见
        "#[derive(Debug, Serialize, Deserialize)]",
        "pub struct Probe {",
        "    /// 文档注释不是字段",
        "    pub alpha: u32,",
        "    pub beta_gamma: u32,",
        "    private_field: u32,",
        "}",
        "pub struct After { pub delta: u32 }",
      ].join("\n"),
    );
    const { fields, attrs } = rustStruct(probe, "Probe");
    expect(fields, "非 pub 字段/文档注释被算成了字段，或串到了下一个结构").toEqual([
      "alpha",
      "beta_gamma",
    ]);
    expect(attrs.split("\n"), "隔着空行的属性被漏掉，或把上一个项的属性也收了进来").toEqual([
      '#[serde(rename_all = "camelCase")]',
      "#[derive(Debug, Serialize, Deserialize)]",
    ]);
    expect(rustStruct(probe, "Missing")).toEqual({ fields: [], attrs: "" });
  });

  it("tsInterfaceFields 认得可选字段，不串到下一个接口去", () => {
    const probe = stripJs(
      [
        "export interface Probe {",
        "  alpha: number;",
        "  beta_gamma?: string | null;",
        "  // ghost: number;",
        "}",
        "export interface After { delta: number }",
      ].join("\n"),
    );
    expect(tsInterfaceFields(probe, "Probe")).toEqual(["alpha", "beta_gamma"]);
    expect(tsInterfaceFields(probe, "Missing")).toEqual([]);
  });

  it("jsonFieldLiterals 取 match 块的全部分支，裸字面量则只取本行", () => {
    const probe = stripRust(
      [
        "serde_json::json!({",
        '    "kind": match hint { Decision::Changed { .. } => "changed", _ => "tofu" },',
        '    "host": host,',
        '    "note": "别的字段不该被吃进来",',
        "})",
      ].join("\n"),
    );
    expect(jsonFieldLiterals(probe, "kind")).toEqual(["changed", "tofu"]);
    expect(jsonFieldLiterals(probe, "note")).toEqual(["别的字段不该被吃进来"]);
    expect(jsonFieldLiterals(probe, "absent")).toEqual([]);
    expect(() => jsonFieldLiterals('"k": "a",\n"k": "b",', "k")).toThrow(/无法确定钉哪一个/);

    // events.rs 现在恰好把整个 match 写在一行上，于是"取到行尾"与"取配对块"输出相同——
    // 单行样本证明不了配对块那条规则。这里单独钉多行形态：那条 match 臂再长一点，
    // rustfmt 就会把它拆成下面这个样子，届时按行尾截断会**只剩一个空的 match 头**，
    // 契约集合凭空缩水成空集，比较恒真。
    const wrapped = stripRust(
      [
        "serde_json::json!({",
        '    "kind": match hint {',
        "        Decision::Changed { .. } => \"changed\",",
        '        _ => "tofu",',
        "    },",
        '    "note": "别的字段不该被吃进来",',
        "})",
      ].join("\n"),
    );
    expect(jsonFieldLiterals(wrapped, "kind")).toEqual(["changed", "tofu"]);
  });

  it("variantTagArms 认得枚举路径臂，且不把解析器方向的臂算进来", () => {
    const probe = stripRust(
      [
        "fn tag(self) -> &'static str {",
        "    match self {",
        '        // Kind::Ghost => "ghost",  —— 注释里的不算',
        '        Kind::Alpha => "alpha",',
        "        Self::BetaGamma => \"beta_gamma\",",
        "    }",
        "}",
      ].join("\n"),
    );
    expect(Object.fromEntries(variantTagArms(fnBody(probe, "tag")))).toEqual({
      Alpha: "alpha",
      BetaGamma: "beta_gamma",
    });
    // 反向：`"alpha" => Kind::Alpha` 是 matchArms 的活儿，这里必须一条都不认，
    // 否则拿它去扫解析器会得到一张"看起来对"的表，而表里的键是大写变体名，恒不相等。
    expect(variantTagArms('    "alpha" => Some(Kind::Alpha),').size).toBe(0);
  });

  it("jsFnBody + strLits 取的是那一个函数的字面量，不串到下一个函数", () => {
    const probe = stripJs(
      [
        'function first(k: string): boolean { return k === "a" || k === "b"; }',
        'function second(k: string): boolean { return k === "ghost"; }',
      ].join("\n"),
    );
    expect(strLits(jsFnBody(probe, "first"))).toEqual(["a", "b"]);
    expect(jsFnBody(probe, "first"), "串到了下一个函数：花括号配对没做对").not.toContain("ghost");
    expect(jsFnBody(probe, "missing")).toBe("");
  });

  it("selectOptionValues 只取该 select 的写死取值，表达式取值不算", () => {
    const probe = stripJs(
      [
        "<select bind:value={other}><option value=\"ghost\">别的选择器</option></select>",
        "<select bind:value={want} data-testid=\"t\">",
        '  <option value={null}>无</option>',
        '  <option value="alpha">甲</option>',
        '  <option value={r.id} disabled={x}>动态</option>',
        '  <option value="beta">乙</option>',
        "</select>",
        '<option value="after">块外的不算</option>',
      ].join("\n"),
    );
    expect(selectOptionValues(probe, "want")).toEqual(["alpha", "beta"]);
    expect(selectOptionValues(probe, "missing")).toEqual([]);
  });

  it("tsUnion 只取该类型自己的字面量", () => {
    const probe = ['type Other = "x" | "y";', 'export type Want = "a" | "b" | "c";', 'const s = "z";'].join(
      "\n",
    );
    expect(tsUnion(probe, "Want")).toEqual(["a", "b", "c"]);
    expect(tsUnion(probe, "Missing")).toEqual([]);
  });
});

describe("ConnectFailure.category（Rust match 的右值字面量 ↔ types.ts 联合 ↔ 面板比较点）", () => {
  // 2026-09-01（路线图 4c）：session_open 的失败现在结构化过线，`category` 决定失败面板
  // 挂哪些按钮——auth 才有「用 Agent 重试」，非 auth 不得渲染「服务器只接受 X」。
  // 它不是 Rust 枚举，是 `category_of` 里 match 出来的 &'static str；漂移的后果不是报错，
  // 是某一类失败静默落进 other、面板上少一排按钮。
  const rustCats = () => {
    const body = fnBody(CONNECT_FAILURE_RS(), "category_of");
    const lits = [...body.matchAll(/=>\s*"([a-z_]+)"/g)].map((m) => m[1]);
    return [...new Set(lits)].sort();
  };

  it("Rust 侧真的解析到了分类（空集合会让下面的比较恒真）", () => {
    expect(rustCats().length).toBeGreaterThanOrEqual(3);
  });

  it("Rust 产得出的 category 与 types.ts 的联合类型逐字一致", () => {
    expect(tsFieldUnion(TYPES_TS(), "category").sort()).toEqual(rustCats());
  });

  it("面板里逐字比较的 category 都在契约内，且 auth 分支仍有人守", () => {
    const cmp = [...CF_PANEL().matchAll(/category\s*===\s*"([^"]*)"/g)].map((x) => x[1]);
    expect(cmp.length, "面板里没找到 category === \"…\" 比较：锚点失效了").toBeGreaterThan(0);
    for (const c of cmp) expect(rustCats(), `面板拿 "${c}" 比对，但后端从不发这个值`).toContain(c);
    expect(cmp, "auth 分支（agent/服务器通告那一排按钮）的判据不见了").toContain("auth");
  });
});
