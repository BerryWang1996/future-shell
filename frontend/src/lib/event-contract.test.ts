import { describe, it, expect } from "vitest";
import fs from "node:fs";
import path from "node:path";

/**
 * 事件契约交叉核对（审计：`invoke ↔ #[tauri::command]` 有门禁，`emit ↔ listen` 一直没有）。
 *
 * ipc-contract.test.ts 只管命令通道。事件通道有四类错法，**全部静默**：
 *  1. 事件名写错一侧 —— 发的没人收 / 收的没人发，没有任何报错；
 *  2. 载荷键写错 —— `e.payload.sessionId` 在后端发 `session_id` 时得到 `undefined`，
 *     TypeScript 完全不管（`listen<T>` 的 T 是断言不是校验，载荷来自 IPC 边界外）；
 *  3. emit 了无人 listen —— 一段自称「信号」的死代码；
 *  4. listen 了无人 emit —— 一个永远等不到的握手。
 *
 * 这不是假想。本测试落地时全仓正有两处 3/4 类实缺陷，方向还正好相反：
 *  - `coldstart_ready`（app/src/lib.rs `setup()` 发）零监听者；
 *  - `app:ready`（frontend/src/App.svelte 发）零监听者，Rust 侧根本没有任何 `listen`
 *    ——计划里写的「app 侧 listen 后打印 FUTURE_SHELL_READY_MS」从未落笔。
 * 两处都已删除（见各自删除点的注释）。删除是收敛，不是掩埋：本文件此后不允许再出现第三处。
 *
 * **Tauri v2 只对命令参数做 camelCase↔snake_case 转换，事件载荷不转。** 故本文件逐字比较，
 * 不做归一——归一恰好会把「后端发 session_id、前端读 sessionId」这类真缺陷抹平。
 * 本仓两种写法混用（`session_id`/`promptId`/`bytes_done`/`sessionId` 同时存在），
 * 正是最需要机器来核的形态。
 */

const APP_SRC = path.resolve(process.cwd(), "../app/src");
const ENGINE_SRC = path.resolve(process.cwd(), "../crates/sshengine/src");
const FRONTEND_SRC = path.resolve(process.cwd(), "src");

/**
 * 发了但前端不监听的事件白名单。新增一项必须写明**为什么它不需要消费者**——
 * 「以后可能会用上」不算理由，那正是被本测试删掉的两处的原话。
 */
const EMIT_WITHOUT_LISTENER: Record<string, string> = {};

/**
 * 前端监听但后端从不发的事件白名单。除 Tauri 内建事件（`tauri://…`，由框架发）外，
 * 一条监听不到的事件意味着某段界面逻辑永远不会执行。
 */
const LISTEN_WITHOUT_EMITTER: Record<string, string> = {};

// ---------------------------------------------------------------------------
// 通用扫描件
// ---------------------------------------------------------------------------

function walk(dir: string, out: string[] = []): string[] {
  for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
    const p = path.join(dir, e.name);
    if (e.isDirectory()) {
      if (e.name !== "node_modules") walk(p, out);
    } else out.push(p);
  }
  return out;
}

/** 剥注释一律换等量换行：行号是报错定位的唯一线索，剥掉后漂了等于报错指向别的文件位置。 */
const blankKeepingLines = (s: string): string => s.replace(/[^\n]/g, "");

/** JS/TS/Svelte 注释。注释里的 `emit("x")` 不是发送点，注释里的 `listen("y")` 不是监听点。 */
function stripJs(src: string): string {
  return src
    .replace(/<!--[\s\S]*?-->/g, blankKeepingLines)
    .replace(/\/\*[\s\S]*?\*\//g, blankKeepingLines)
    .replace(/(^|[^:])\/\/[^\n]*/g, "$1");
}

/** Rust 注释（含 `///` 文档注释）。同理：文档注释里引用的事件名不是发送点。 */
function stripRust(src: string): string {
  return src.replace(/\/\*[\s\S]*?\*\//g, blankKeepingLines).replace(/(^|[^:])\/\/[^\n]*/g, "$1");
}

/** 从 `src[i]` 处的开括号找到配对的闭括号下标；找不到返回 -1。跳过字符串字面量。 */
function matchPair(src: string, i: number, open: string, close: string): number {
  let depth = 0;
  for (; i < src.length; i++) {
    const ch = src[i];
    if (ch === '"' || ch === "`") {
      const q = ch;
      i++;
      while (i < src.length && !(src[i] === q && src[i - 1] !== "\\")) i++;
      continue;
    }
    if (ch === open) depth++;
    else if (ch === close && --depth === 0) return i;
  }
  return -1;
}

/**
 * 按成对括号切分顶层逗号。
 *
 * **不把 `<`/`>` 算作括号**（ipc-contract.test.ts 的同名函数算，那里要跳泛型实参）：
 * Rust 的 match 臂 `Decision::Changed { .. } => "changed"` 每写一条就多一个裸 `>`，
 * 计进深度会让 `hostkey:prompt` 的载荷从中间被截断——实测表现是 `old_fingerprint`
 * 被判成「前端读了后端没发的键」，一条完全正确的接线被报成缺陷。
 * 本文件不需要跳泛型：`listen<T>` 的 T 在进入本函数之前就已按尖括号配平单独取走了。
 */
function splitTopLevel(s: string): string[] {
  const parts: string[] = [];
  let depth = 0;
  let cur = "";
  for (let i = 0; i < s.length; i++) {
    const ch = s[i];
    if (ch === '"' || ch === "`") {
      const q = ch;
      let j = i + 1;
      while (j < s.length && !(s[j] === q && s[j - 1] !== "\\")) j++;
      cur += s.slice(i, j + 1);
      i = j;
      continue;
    }
    if ("([{".includes(ch)) depth++;
    else if (")]}".includes(ch)) depth--;
    if (ch === "," && depth === 0) {
      parts.push(cur);
      cur = "";
    } else cur += ch;
  }
  parts.push(cur);
  return parts;
}

/**
 * 取 `src[i]`（必须是 `{`）这个对象字面量的**顶层**字符串键。
 * 必须按深度取：`auth:prompt` 的载荷里嵌着 `json!({"text": …, "echo": …})`，
 * 不看深度就会把 prompts 元素的键当成事件顶层键，"前端读的键都在后端发的键里" 随之被放宽。
 */
function jsonTopKeys(src: string, i: number): Set<string> {
  const keys = new Set<string>();
  let depth = 0;
  for (; i < src.length; i++) {
    const ch = src[i];
    if (ch === '"') {
      const start = i + 1;
      i++;
      while (i < src.length && !(src[i] === '"' && src[i - 1] !== "\\")) i++;
      if (depth === 1) {
        let j = i + 1;
        while (j < src.length && /\s/.test(src[j])) j++;
        if (src[j] === ":") keys.add(src.slice(start, i));
      }
      continue;
    }
    if (ch === "{") depth++;
    else if (ch === "}" && --depth === 0) break;
  }
  return keys;
}

/** 把 `{…}` 体内深度 ≥1 的部分抹成空格（保留换行），留下顶层供正则取键。 */
function flattenTopLevel(body: string): string {
  let out = "";
  let depth = 0;
  for (let i = 0; i < body.length; i++) {
    const ch = body[i];
    if (ch === "{" || ch === "(" || ch === "[") {
      out += " "; // 开括号本身不可能是换行，无需像下面的普通字符那样保留 \n
      depth++;
      continue;
    }
    if (ch === "}" || ch === ")" || ch === "]") {
      depth--;
      out += " ";
      continue;
    }
    out += depth === 0 ? ch : ch === "\n" ? "\n" : " ";
  }
  return out;
}

/** TS 对象类型体的顶层键名（`a: string; b?: number`）。 */
function tsTopKeys(body: string): Set<string> {
  const keys = new Set<string>();
  const flat = flattenTopLevel(body);
  for (const m of flat.matchAll(/(?:^|[;,\n])\s*(?:readonly\s+)?([A-Za-z_]\w*)\s*\??\s*:/g)) keys.add(m[1]);
  return keys;
}

/** `format!("term:data:{session_id}")` / `` `term:data:${id}` `` 一律归一成 `term:data:*`。 */
const toPattern = (s: string): string => s.replace(/\$?\{[^}]*\}/g, "*");

// ---------------------------------------------------------------------------
// Rust 侧
// ---------------------------------------------------------------------------

interface EmitSite {
  event: string;
  /** null = 载荷不是 `json!` 字面量（变量 / 辅助函数返回值），键需另行解析 */
  keys: Set<string> | null;
  file: string;
  where: string;
}

/**
 * 单份 Rust 源码里的全部 emit 站点。**必须与探针用例共用同一份实现**——探针若自己另抄一遍
 * 取实参的逻辑，它证明的就只是那几个小工具函数（strip/split/jsonTopKeys），而本函数自己的
 * 实参选取（`emit_to(target, name, payload)` 比 `emit(name, payload)` 多一位）无人证。
 */
function parseEmitSites(src: string, rel: string): EmitSite[] {
  const sites: EmitSite[] = [];
  const re = /\.\s*emit(?:_to|_filter|_all)?\s*\(/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(src))) {
    const open = re.lastIndex - 1;
    const close = matchPair(src, open, "(", ")");
    if (close < 0) continue;
    const args = splitTopLevel(src.slice(open + 1, close));
    // emit_to(target, "name", payload) 时首个实参是目标；取第一个能解析成事件名的实参。
    let ni = -1;
    let event = "";
    for (let k = 0; k < args.length; k++) {
      const t = args[k].trim();
      const lit = /^"([^"]+)"$/.exec(t) ?? /^&?\s*format!\(\s*"([^"]+)"/.exec(t);
      if (lit) {
        ni = k;
        event = toPattern(lit[1]);
        break;
      }
    }
    if (ni < 0) continue;
    const payload = (args[ni + 1] ?? "").trim();
    let keys: Set<string> | null;
    const j = payload.search(/json!\s*\(/);
    if (j >= 0) {
      const b = payload.indexOf("{", j);
      keys = b >= 0 ? jsonTopKeys(payload, b) : new Set<string>();
    } else if (payload === "" || payload === "()") {
      keys = new Set<string>();
    } else {
      keys = null;
    }
    sites.push({ event, keys, file: rel, where: `${rel}:${src.slice(0, m.index).split("\n").length}` });
    re.lastIndex = close;
  }
  return sites;
}

function rustEmits(): EmitSite[] {
  return walk(APP_SRC)
    .filter((p) => p.endsWith(".rs"))
    .flatMap((f) => parseEmitSites(stripRust(fs.readFileSync(f, "utf8")), path.basename(f)));
}

function rustListens(): { event: string; where: string }[] {
  const out: { event: string; where: string }[] = [];
  for (const f of walk(APP_SRC).filter((p) => p.endsWith(".rs"))) {
    const src = stripRust(fs.readFileSync(f, "utf8"));
    const re = /\.\s*listen(?:_any|_global)?\s*\(\s*(?:&?\s*format!\(\s*)?"([^"]+)"/g;
    let m: RegExpExecArray | null;
    while ((m = re.exec(src))) {
      out.push({ event: toPattern(m[1]), where: `${path.basename(f)}:${src.slice(0, m.index).split("\n").length}` });
    }
  }
  return out;
}

/**
 * 载荷不是 `json!` 字面量的那几处，键从哪儿来。
 *
 * 这张表不是"声明契约"（那就成了被本测试点名的那类散文台账），而是**指路**：值是解析器，
 * 每一把键都仍从 Rust 源码里现读。下面还有一条用例钉住「间接站点的集合恰好等于本表的覆盖」，
 * 所以第四处间接 emit 出现时会转红，逼人来这里做决定，而不是悄悄退化成 null 被跳过。
 */
/** 从 Rust 源码里读一个 pub struct 的字段名（serde 默认按字段名序列化）。 */
function structFields(src: string, name: string): Set<string> {
  const i = src.search(new RegExp(`pub struct\\s+${name}\\s*\\{`));
  if (i < 0) return new Set();
  const open = src.indexOf("{", i);
  const close = src.indexOf("}", open);
  const body = src.slice(open + 1, close);
  const out = new Set<string>();
  for (const m of body.matchAll(/pub\s+([a-z_][a-z0-9_]*)\s*:/g)) out.add(m[1]);
  return out;
}

const INDIRECT_KEYS: Record<string, (rust: Map<string, string>) => Set<string>> = {
  // M3 Agent 的三条事件：载荷是 serde 结构体（不是 json! 字面量），
  // 键 = 结构体的 pub 字段名。三处都从源码现读——写死一份清单就成了
  // 「被本测试点名的那类散文台账」。
  "confirm_port.rs|agent:confirm": (rust) =>
    structFields(rust.get("confirm_port.rs") ?? "", "PendingConfirm"),
  "confirm_port.rs|agent:ask": (rust) =>
    structFields(rust.get("confirm_port.rs") ?? "", "PendingAsk"),
  "agent_cmd.rs|agent:stopped": (rust) =>
    structFields(rust.get("agent_cmd.rs") ?? "", "RunFinished"),

  // M3 MCP 确认：载荷是 emit 前一个 `serde_json::json!({...})` 字面量，
  // 键从该字面量现读（取 emit 点之前最后一个 json!）。
  "ports.rs|mcp:confirm": (rust) => {
    const src = rust.get("ports.rs") ?? "";
    const e = src.indexOf('"mcp:confirm"');
    if (e < 0) return new Set();
    const j = src.lastIndexOf("serde_json::json!", e);
    if (j < 0) return new Set();
    const b = src.indexOf("{", j);
    return b >= 0 ? jsonTopKeys(src, b) : new Set();
  },

  // MCP 对外服务状态（2026-08-28）：emit 前一个 `let payload = json!({…})`
  // 变量（serve.rs 的 McpRuntime::emit），键从该字面量现读。
  "serve.rs|mcp:status": (rust) => {
    const src = rust.get("serve.rs") ?? "";
    const e = src.indexOf('"mcp:status"');
    if (e < 0) return new Set();
    const j = src.lastIndexOf("serde_json::json!", e);
    if (j < 0) return new Set();
    const b = src.indexOf("{", j);
    return b >= 0 ? jsonTopKeys(src, b) : new Set();
  },

  // `let mut payload = json!({…}); payload["exit_code"] = …;` —— 键分两处
  "session_cmd.rs|session:disconnected": (rust) => {
    const src = rust.get("session_cmd.rs") ?? "";
    const keys = new Set<string>();
    const j = src.search(/let mut payload\s*=\s*serde_json::json!\s*\(/);
    if (j >= 0) {
      const b = src.indexOf("{", j);
      if (b >= 0) for (const k of jsonTopKeys(src, b)) keys.add(k);
    }
    for (const m of src.matchAll(/\bpayload\[\s*"([^"]+)"\s*\]\s*=/g)) keys.add(m[1]);
    return keys;
  },
  // 载荷由辅助函数 `submitted_payload` 造（transfer_submit 与其单测共用同一份，故不内联）
  "sftp_cmd.rs|transfer:submitted": (rust) => {
    const src = rust.get("sftp_cmd.rs") ?? "";
    const f = src.search(/fn submitted_payload\b/);
    if (f < 0) return new Set();
    const j = src.indexOf("json!", f);
    const b = j >= 0 ? src.indexOf("{", j) : -1;
    return b >= 0 ? jsonTopKeys(src, b) : new Set();
  },
  // 引擎的 `TransferEvent` 整份序列化后再注入 sessionId（app 层刻意不另抄一份结构体）
  "sftp_cmd.rs|transfer:progress": (rust) => {
    const keys = new Set<string>();
    const engine = stripRust(fs.readFileSync(path.join(ENGINE_SRC, "transfer.rs"), "utf8"));
    const s = engine.search(/pub struct TransferEvent\b/);
    if (s >= 0) {
      const b = engine.indexOf("{", s);
      const e = matchPair(engine, b, "{", "}");
      for (const m of engine.slice(b + 1, e).matchAll(/(?:^|[,\n])\s*pub\s+([a-z_0-9]+)\s*:/g)) keys.add(m[1]);
    }
    for (const m of (rust.get("sftp_cmd.rs") ?? "").matchAll(/\bobj\.insert\(\s*"([^"]+)"/g)) keys.add(m[1]);
    return keys;
  },
};

// ---------------------------------------------------------------------------
// 前端侧
// ---------------------------------------------------------------------------

interface ListenSite {
  event: string;
  /** 处理器里实际读到的载荷键 ∪ `listen<T>` 的 T 声明的键 */
  reads: Set<string>;
  where: string;
}

/** 前端源码表（相对路径 → 已剥注释的源码）。 */
function frontendSources(): Map<string, string> {
  const out = new Map<string, string>();
  for (const f of walk(FRONTEND_SRC)) {
    if (!/\.(ts|svelte)$/.test(f) || /\.test\.ts$/.test(f)) continue;
    out.set(path.relative(FRONTEND_SRC, f).replace(/\\/g, "/"), stripJs(fs.readFileSync(f, "utf8")));
  }
  return out;
}

/** 解析 `listen<T>` 的 T 声明了哪些键：内联对象直接取，具名类型全仓查 interface/type。 */
function typeArgKeys(typeText: string, sources: Map<string, string>): Set<string> {
  const t = typeText.trim();
  if (t.startsWith("{")) {
    const end = matchPair(t, 0, "{", "}");
    return tsTopKeys(t.slice(1, end < 0 ? t.length : end));
  }
  const name = /^([A-Za-z_]\w*)$/.exec(t)?.[1];
  if (!name) return new Set();
  for (const src of sources.values()) {
    const re = new RegExp(`\\b(?:interface|type)\\s+${name}\\b[^{;]*\\{`);
    const m = re.exec(src);
    if (!m) continue;
    const b = src.indexOf("{", m.index);
    const e = matchPair(src, b, "{", "}");
    return tsTopKeys(src.slice(b + 1, e < 0 ? src.length : e));
  }
  return new Set();
}

/** 处理器体里对载荷的读取：`e.payload.k`、`const p = e.payload; p.k`、`const {a,b} = e.payload`。 */
function payloadReads(body: string): Set<string> {
  const keys = new Set<string>();
  const aliases = new Set<string>(["e\\.payload", "event\\.payload"]);
  for (const m of body.matchAll(/\b(?:const|let|var)\s+([A-Za-z_]\w*)\s*(?::[^=]+?)?=\s*(?:e|event)\.payload\b/g)) {
    aliases.add(m[1].replace(/[.*+?^${}()|[\]\\]/g, "\\$&"));
  }
  for (const a of aliases) {
    for (const m of body.matchAll(new RegExp(`${a}\\s*\\??\\.\\s*([A-Za-z_]\\w*)`, "g"))) keys.add(m[1]);
    for (const m of body.matchAll(new RegExp(`\\b(?:const|let|var)\\s*\\{([^}]*)\\}\\s*=\\s*${a}\\b`, "g"))) {
      for (const seg of m[1].split(",")) {
        const k = /^\s*([A-Za-z_]\w*)/.exec(seg);
        if (k) keys.add(k[1]);
      }
    }
  }
  return keys;
}

function scanListens(src: string, rel: string, sources: Map<string, string>): ListenSite[] {
  const sites: ListenSite[] = [];
  const re = /\blisten\s*(<)?/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(src))) {
    let i = re.lastIndex;
    let typeText = "";
    if (m[1]) {
      const start = i;
      let d = 1;
      while (i < src.length && d > 0) {
        if (src[i] === "<") d++;
        else if (src[i] === ">") d--;
        i++;
      }
      typeText = src.slice(start, i - 1);
    }
    while (i < src.length && /\s/.test(src[i])) i++;
    if (src[i] !== "(") continue;
    const close = matchPair(src, i, "(", ")");
    if (close < 0) continue;
    const args = splitTopLevel(src.slice(i + 1, close));
    const lit = /^\s*(?:"([^"]+)"|`([^`]*)`)\s*$/.exec(args[0] ?? "");
    if (!lit) continue;
    const reads = payloadReads(args.slice(1).join(","));
    for (const k of typeArgKeys(typeText, sources)) reads.add(k);
    sites.push({
      event: toPattern(lit[1] ?? lit[2]),
      reads,
      where: `${rel}:${src.slice(0, m.index).split("\n").length}`,
    });
    re.lastIndex = close;
  }
  return sites;
}

/**
 * 前端 emit 站点。**`emitTo` 的事件名在第二个实参上**（`emitTo(target, event, payload)`），
 * 与 `emit(event, payload)` 差一位——按第一个实参一把抓会把窗口标签 `"main"` 当成事件名，
 * 于是一个真实的前端→后端事件既不会被核对，还会凭空多出一个查无此发的 `"main"`。
 * 这条差异是本函数的探针用例扫出来的（写这个函数时正是按第一实参抓的）。
 */
function scanEmits(src: string, rel: string): { event: string; where: string }[] {
  const out: { event: string; where: string }[] = [];
  const re = /\bemit(To)?\s*(?:<[^>]*>)?\s*\(/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(src))) {
    const open = re.lastIndex - 1;
    const close = matchPair(src, open, "(", ")");
    if (close < 0) continue;
    const args = splitTopLevel(src.slice(open + 1, close));
    const arg = (args[m[1] ? 1 : 0] ?? "").trim();
    const lit = /^(?:"([^"]+)"|`([^`]*)`)$/.exec(arg);
    if (!lit) continue;
    out.push({ event: toPattern(lit[1] ?? lit[2]), where: `${rel}:${src.slice(0, m.index).split("\n").length}` });
    re.lastIndex = close;
  }
  return out;
}

// ---------------------------------------------------------------------------

describe("事件契约：Rust emit ↔ 前端 listen", () => {
  const rustSrc = new Map<string, string>(
    walk(APP_SRC)
      .filter((p) => p.endsWith(".rs"))
      .map((p) => [path.basename(p), stripRust(fs.readFileSync(p, "utf8"))]),
  );
  const sources = frontendSources();
  const emits = rustEmits();
  const listens = [...sources].flatMap(([rel, src]) => scanListens(src, rel, sources));
  const feEmits = [...sources].flatMap(([rel, src]) => scanEmits(src, rel));
  const rsListens = rustListens();

  const emitted = new Set(emits.map((e) => e.event));
  const listened = new Set(listens.map((l) => l.event));

  /** 事件名 → 后端发出的键并集（跨该事件的所有 emit 站点；可选键只在部分站点出现是正常的）。 */
  const emittedKeys = new Map<string, Set<string>>();
  for (const s of emits) {
    const keys = s.keys ?? INDIRECT_KEYS[`${s.file}|${s.event}`]?.(rustSrc) ?? null;
    if (!keys) continue;
    const acc = emittedKeys.get(s.event) ?? new Set<string>();
    for (const k of keys) acc.add(k);
    emittedKeys.set(s.event, acc);
  }

  /**
   * 解析器失灵会让下面每条断言平凡通过——那是最坏的一种绿（V20 实测过一次恒真断言）。
   * 用**全量事件名**而非计数下界当哨兵：正则被改窄时计数下界照样过得去（少读两个事件，
   * 剩下的仍 ≥ 阈值），而全量清单一定对不上。清单变动本身也需要人来签字：
   * 新事件出现即转红，逼人回答「两侧都接好了吗」，同 UNREACHED_BY_INVOKE 的用法。
   */
  it("解析器读到的事件清单与全仓一致", () => {
    expect([...emitted].sort()).toEqual([
      // M3 Agent：确认队列（两条待答）与终态。三条都由 app/src/agent 与
      // commands/agent_cmd.rs 发出，AgentPanel.svelte 监听。
      "agent:ask",
      "agent:confirm",
      "agent:stopped",
      "auth:prompt",
      "hostkey:prompt",
      "mcp:confirm", // M3 MCP 阻塞式确认（§4.5），app/src/mcp/ports.rs 单点发出，McpConfirmDialog 监听
      "mcp:status", // 对外 MCP 服务状态（2026-08-28），serve.rs 的 McpRuntime::emit 单点，SettingsDialog 监听
      // 阶段 3 音频（per-会话动态名；RdpPane 按自己的 sid 监听，Web Audio 播放）
      "rdp:audio-close:*",
      "rdp:audio-format:*",
      "rdp:audio:*",
      "rdp:cert", // RDP 证书裁决（固定名+sessionId 载荷；与活动面板无关）
      "rdp:closed:*", // per-会话动态名（RdpPane 按自己的 sid 监听）
      "rdp:connected:*",
      "rdp:frame:*",
      "rdp:share:*", // RDPDR：挂载确认（helper 宣告成功），RdpPane 刷新共享状态
      "rdp:status", // 固定名+sessionId 载荷（App 层 toast）
      "schedule:run", // M4a 计划任务执行结果，app/src/scheduler.rs 的 emit_run 单点发出
      "session:closed",
      "session:disconnected",
      "session:status",
      "term:data:*", // sessions.rs 的 `format!("term:data:{session_id}")`，per-会话动态名
      "transfer:progress",
      "transfer:submitted",
      "transfer_verified",
      "vault:locked",
      "zmodem:progress", // M4a 终端内传输（rz/sz），zmodem_bridge.rs 的 emit_progress 单点发出
    ]);
    // 16 = 原 15 + vault_restore_backup 里那一处 `vault:locked{reason:"restored"}`
    //（审计2 #27：恢复成功后内存里那份 Store 是恢复前的快照，必须丢掉并广播）；
    // 17 = 16 + 连接时输口令的 `password_prompt` 里第二处 `auth:prompt{kind,profileId}`
    //（Task 47：客户端主动要口令，与 kbd-interactive 共用事件名、多两个键）。
    // 18 = 17 + `zmodem:progress`。它刻意只有**一个**站点：三处调用方（拦截器、
    // 驱动的开始/收尾）都走 emit_progress，好让载荷键集恒定——各处各发自己需要的
    // 键会把「这个 phase 有没有 path」变成只能读后端代码才知道的隐性契约。
    // 19 = 18 + `schedule:run`（同样的单点理由，见 scheduler.rs 的 emit_run）。
    // 22 = 19 + M3 Agent 三处（confirm_port.rs 两处待答 + agent_cmd.rs 的终态）。
    // 23 = 22 + M3 MCP 的 `mcp:confirm`（ports.rs 阻塞式确认单点）。
    // 24 = 23 + 2026-08-26 zmodem_finalize 命令里的 `saved`（两段式第二段；键集与
    //     zmodem_bridge 的 emit_progress_full 逐字一致——键集恒定纪律不分站点）。
    // 24 → 31 = +RDP 七站点（cert×1 / status×2 / frame×2（连接期+会话期）/
    //     connected×1 / closed×1；per-会话动态名归一为 rdp:*:*）。
    // 30 → 33 = +阶段 3 音频三站点（format / data / close）
    // 33 → 34 = +会话中途的 `rdp:connected` 补发（2026-08-27）。远端接受分辨率
    //     变更后走 Deactivation-Reactivation，桌面尺寸变了必须再报一次——
    //     前端画布的位图尺寸只在这个事件上定格，不补发就按旧尺寸错切贴图。
    //     两个站点同名同键集（width/height），符合「键集恒定」纪律。
    // 34 → 35 = +serve.rs 的 `mcp:status`（2026-08-28 对外 MCP 套接字形态）：
    //     开关/连接/断开时推送在线状态，SettingsDialog 的状态点消费。
    // 35 → 38 = +serial_cmd.rs 三站点（M7.4）：串口拔线发 `session:disconnected`、
    //     插回来发 `session:status`（带 state:"connected"）、用户关闭发 `session:closed`。
    //     **刻意复用 SSH 那三个事件名而不是另起 `serial:*`**：前端的事件桥、标签状态机、
    //     断线 banner、重连提示全都挂在这三个名字上，另起一套等于把它们再写一遍——
    //     而出口标准要的正是「串口会话与 SSH 会话在公共设施上行为一致」。
    //     键集与 SSH 那三处逐字一致（session_id / reason / attempt / state / message），
    //     故「键集恒定」纪律不因新增站点而破。
    expect(emits.length, "emit 站点数（一个事件名可有多个站点，键取并集）").toBe(39);
    // 17 = 14 + AgentPanel.svelte 的三处（confirm / ask / stopped）。
    // 18 = 17 + McpConfirmDialog.svelte 的 `mcp:confirm`。
    // 18 → 23 = +RdpPane 三处（frame / connected / closed）+ App 的 rdp:status
    //     + RdpCertDialog 的 rdp:cert（cert/status 用固定名，见 rdp.rs 注释）。
    // 23 → 26 = +RdpPane 的音频三处
    // 26 → 27 = +SettingsDialog 的 `mcp:status`（对外 MCP 在线状态点）
    expect(listens.length, `没从 ${FRONTEND_SRC} 读出全部 listen`).toBe(28);
  });

  it("扫描器认注释与模板名（注释里的 emit/listen 不算接线）", () => {
    const probe = [
      `/**`, //                                                        1
      ` * 历史实现：app.emit("ghost_block", ()) —— 块注释里的不算`, //   2
      ` */`, //                                                        3
      `// let _ = app.emit("ghost_line", json!({}));`, //               4
      `let _ = app.emit("real:evt", serde_json::json!({`, //            5
      `    "session_id": id,`, //                                       6
      `    "nested": serde_json::json!({"inner": 1}),`, //              7
      // match 臂里的裸 `>`：算进括号深度就会把载荷从这里截断，其后的键全部消失
      // （实测原形：hostkey:prompt 的 old_fingerprint 被报成「后端没发」）。
      `    "kind": match hint { Decision::Changed { .. } => "changed", _ => "tofu" },`, // 8
      `    "tail": last,`, //                                           9
      `}));`, //                                                       10
      `let _ = app.emit(&format!("term:data:{session_id}"), serde_json::json!({ "data_b64": b }));`, // 11
      // emit_to 的事件名在第二位，且目标表达式里的 `<` 落在第一个顶层逗号**之前**——
      // 这是全仓唯一能让「把 `<` 计进括号深度」显形的形状：计进去则深度再也回不到 0，
      // 三个实参并成一坨，事件名认不出来，整个站点被静默丢弃（不是报错，是少一条契约）。
      `let _ = app.emit_to(EventTarget::<Wry>::from(label), "gen:evt", serde_json::json!({ "z": 1 }));`, // 15
    ].join("\n");
    const found = parseEmitSites(stripRust(probe), "probe.rs");
    expect(found.map((f) => f.event)).toEqual(["real:evt", "term:data:*", "gen:evt"]);
    expect([...found[2].keys!], "emit_to 的载荷在第三位，取错一位就会把事件名当载荷").toEqual(["z"]);
    expect([...found[0].keys!], "嵌套对象/match 臂的键不得算成事件顶层键，且 `=>` 之后的键不得丢").toEqual([
      "session_id",
      "nested",
      "kind",
      "tail",
    ]);
    expect(found[0].where, "剥注释按等量换行补齐，行号不能漂").toBe("probe.rs:5");
  });

  it("前端 listen 扫描器认得三种载荷读法与具名类型", () => {
    const probe = [
      `// listen("ghost", …) 注释掉的监听不算监听`,
      `listen<{ a: string; nested?: { deep: number } }>("evt:one", (e) => { const { a } = e.payload; });`,
      `listen("evt:two", (e: any) => { const p = e.payload; use(p.bee, p.cee?.deep); });`,
      `listen<Probe>("evt:three", (e) => { keep(e.payload.dee); });`,
    ].join("\n");
    const fake = new Map<string, string>([["probe.ts", "export interface Probe { eee: string; fff?: number }"]]);
    const got = scanListens(stripJs(probe), "probe.ts", fake);
    expect(got.map((g) => g.event)).toEqual(["evt:one", "evt:two", "evt:three"]);
    expect([...got[0].reads].sort(), "内联类型只取顶层键，nested.deep 不算").toEqual(["a", "nested"]);
    expect([...got[1].reads].sort()).toEqual(["bee", "cee"]);
    expect([...got[2].reads].sort(), "具名类型的键要全仓解析出来，否则键核对整条平凡通过").toEqual(["dee", "eee", "fff"]);
  });

  /**
   * 全仓当前 `feEmits` 为空集（前端→后端方向一个事件都不剩，`app:ready` 删掉之后）。
   * 空集意味着下面「前端 emit 都有后端监听者」和最后那条「app:ready 没有回潮」**当前都恒真**：
   * 把 scanEmits 整个改成 `return []` 两条照样绿。所以这条探针不是锦上添花，
   * 它是那两条断言此刻唯一的非平凡性来源。
   */
  it("前端 emit 扫描器认得字符串名、模板名与注释", () => {
    const probe = [
      `// await emit("ghost:line", {}) —— 注释里的不算`,
      `/* emit("ghost:block", {}) */`,
      `void emit("app:ready");`,
      `await emitTo("main", "win:evt", { a: 1 });`,
      "await emit(`term:data:${id}`, { data_b64 });",
    ].join("\n");
    const got = scanEmits(stripJs(probe), "probe.ts");
    expect(got.map((g) => g.event)).toEqual(["app:ready", "win:evt", "term:data:*"]);
    expect(got[0].where, "剥注释按等量换行补齐，行号不能漂").toBe("probe.ts:3");
  });

  it("后端发的每个事件都有前端监听者", () => {
    const orphan = emits
      .filter((e) => !listened.has(e.event) && !(e.event in EMIT_WITHOUT_LISTENER))
      .map((e) => `${e.where} emit("${e.event}") 无人监听`);
    expect(orphan, "要么接上消费者，要么删掉，要么写进 EMIT_WITHOUT_LISTENER 并说明为什么不需要").toEqual([]);
  });

  it("前端监听的每个事件都有后端发送者", () => {
    const orphan = listens
      .filter((l) => !emitted.has(l.event) && !l.event.startsWith("tauri://") && !(l.event in LISTEN_WITHOUT_EMITTER))
      .map((l) => `${l.where} listen("${l.event}") 无人发送`);
    expect(orphan, "监听不到的事件 = 那段界面逻辑永远不执行").toEqual([]);
  });

  it("前端 emit 的每个事件都有后端监听者", () => {
    const rsHeard = new Set(rsListens.map((l) => l.event));
    const orphan = feEmits
      .filter((e) => !rsHeard.has(e.event) && !(e.event in EMIT_WITHOUT_LISTENER))
      .map((e) => `${e.where} emit("${e.event}") Rust 侧无人监听`);
    expect(orphan, "前端→后端方向同样是契约：app:ready 就是这么空发了一整个阶段").toEqual([]);
  });

  it("前端读的每个载荷键都在后端发的键里", () => {
    const bad: string[] = [];
    for (const l of listens) {
      if (l.event.startsWith("tauri://")) continue;
      const have = emittedKeys.get(l.event);
      if (!have) continue;
      for (const k of l.reads) {
        if (!have.has(k)) bad.push(`${l.where} listen("${l.event}") 读 "${k}"，后端只发 [${[...have].sort().join(", ")}]`);
      }
    }
    expect(bad, "键写错的表现是 undefined 一路穿过去，TS 不管、运行期不报（v2 不转事件载荷的大小写）").toEqual([]);
  });

  it("每个事件的载荷键都解析得出（间接站点不得静默退化成不核）", () => {
    const unresolved = [...listened]
      .filter((e) => !e.startsWith("tauri://"))
      .filter((e) => !emittedKeys.has(e) || emittedKeys.get(e)!.size === 0);
    expect(unresolved, "载荷键为空 = 上一条断言对该事件恒真").toEqual([]);
  });

  it("间接载荷站点的集合恰好等于 INDIRECT_KEYS 的覆盖", () => {
    const indirect = emits.filter((e) => e.keys === null).map((e) => `${e.file}|${e.event}`);
    expect([...new Set(indirect)].sort(), "新出现的间接站点必须在 INDIRECT_KEYS 里给出解析器").toEqual(
      Object.keys(INDIRECT_KEYS).sort(),
    );
    for (const [k, resolve] of Object.entries(INDIRECT_KEYS)) {
      expect(resolve(rustSrc).size, `INDIRECT_KEYS["${k}"] 解析不出任何键，形同虚设`).toBeGreaterThan(0);
    }
  });

  it("TransferEvent 未加 serde 改名（改名会让上面按字段名取键的解析整份失真）", () => {
    const engine = fs.readFileSync(path.join(ENGINE_SRC, "transfer.rs"), "utf8");
    const decl = engine.slice(Math.max(0, engine.search(/pub struct TransferEvent\b/) - 200), engine.search(/pub struct TransferEvent\b/));
    expect(decl).not.toMatch(/serde\s*\(\s*rename_all/);
  });

  it("白名单不留过期项", () => {
    expect(Object.keys(EMIT_WITHOUT_LISTENER).filter((e) => listened.has(e))).toEqual([]);
    expect(Object.keys(LISTEN_WITHOUT_EMITTER).filter((e) => emitted.has(e))).toEqual([]);
  });

  /**
   * 被删掉的那两处必须留在测试里点名。否则「删了」这件事只活在 git 历史里，
   * 下一个人照着同一段计划文档再补一遍 `emit("app:ready")` 时，唯一会响的只有上面那条通用断言，
   * 而通用断言不会告诉他「这条路走过，是死的」。
   */
  it("两处已删除的空发事件没有回潮", () => {
    expect(emitted.has("coldstart_ready"), "coldstart_ready：setup() 发在前端注册监听之前，无法被安全消费").toBe(false);
    expect(
      feEmits.map((e) => e.event),
      "app:ready：计划中的 Rust 侧监听从未落笔，Rust 全仓零 listen",
    ).not.toContain("app:ready");
  });
});

/**
 * S304：注册了 `onCloseRequested` 就**必须**授 `core:window:allow-destroy`。
 *
 * 这是一条被 Tauri 内部实现绑死、却在两份文件里各写一半的耦合，漏授时全程静默：
 *
 * Rust 侧 `manager/window.rs` 的窗口事件处理是——
 *   `if window.has_js_listener(WINDOW_CLOSE_REQUESTED_EVENT) { api.prevent_close(); }`
 * 即**只要 JS 注册了监听，每一次关闭请求都被无条件拦下**（含 `window.close()`
 * 自己触发的那次），真正关窗完全交由 JS 侧的包装器决定：
 *   `await handler(evt); if (!evt.isPreventDefault()) { await this.destroy(); }`
 *
 * 于是放行路径的最后一步恒为 `destroy()`——而它在 Tauri v2 是**独立于 close 的
 * 一条权限**（`core:window:allow-destroy`，不含在 `core:default` 里）。漏授的
 * 后果不是报错而是：确认框点了确认，窗口纹丝不动，只能去任务管理器结束进程；
 * 被拒的 invoke 发生在 Tauri 包装器内部，应用代码接不到，日志里一个字都没有。
 * （2026-08-19 实测现场：`session_close_all` 0 ms 完成后日志彻底静默。）
 */
describe("窗口关闭能力：onCloseRequested ⇒ allow-destroy", () => {
  it("注册了 onCloseRequested 就必须授 core:window:allow-destroy", () => {
    const capPath = path.resolve(process.cwd(), "../app/capabilities/default.json");
    const cap = JSON.parse(fs.readFileSync(capPath, "utf8")) as { permissions: string[] };
    const usesCloseRequested = [...walk(FRONTEND_SRC)]
      .map((f) => fs.readFileSync(f, "utf8"))
      .some((s) => s.includes("onCloseRequested"));
    if (!usesCloseRequested) return; // 不用这条 API 就不需要这条权限

    expect(
      cap.permissions,
      "前端注册了 onCloseRequested：Tauri 会拦下每一次关闭请求，放行唯一靠 destroy()。" +
        "不授 core:window:allow-destroy 则「点了确认窗口关不掉」，且完全静默",
    ).toContain("core:window:allow-destroy");
    // close 同样不在 core:default 里；两条缺一，关闭路径就断在不同的位置上。
    expect(cap.permissions, "close 是发起关闭请求本身所需").toContain("core:window:allow-close");
  });
});
