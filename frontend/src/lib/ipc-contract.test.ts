import { describe, it, expect } from "vitest";
import fs from "node:fs";
import path from "node:path";

/**
 * IPC 契约交叉核对（审计 P1「IPC 契约」）：把前端每一处 `invoke(...)` 的命令名与参数名，
 * 对着 Rust 侧 `#[tauri::command]` 的真实签名逐个核。
 *
 * 为什么必须是**测试**而不是一次性脚本：这类错位没有任何编译期或运行期信号。
 * 前端 `invoke` 的参数是 `Record<string, unknown>`，键名写错 TypeScript 不管；
 * Rust 侧收不到必填参数则 IPC 返回 Err，而多数调用点把 Err 吞进 catch 变成一句 toast 或
 * 一行 console.warn——表现是「点了没反应」。审计实际抓到过两例（`profiles_import` 发 `json`
 * 而 Rust 形参是 `content`；`addTab` 少传两个实参），都是靠人眼读出来的，读一次只能保一次。
 *
 * Tauri v2 在 JS↔Rust 之间自动做 camelCase↔snake_case 转换，故按 camelCase 归一后比较。
 *
 * 已知的解析边界（都已实测踩中过，故写死在这里）：
 * - 泛型实参可嵌套（`invoke<Array<{a: string}>>(...)`），必须按尖括号配平跳过，
 *   `<[^>]*>` 会在 `sessions_unclosed` 这类调用上漏匹配；
 * - 参数对象里的行注释可能含逗号（`local: "", // 下载目的由 Rust 决定`），
 *   不剥注释会把紧随其后的键整个吃掉，误报「缺必填参数」（现由 stripComments 整份剥掉，
 *   见该函数头注：只剥参数对象内部还留着「注释里的 invoke 被当成调用点」这个更大的洞）；
 * - `#[tauri::command]` 这串字符也出现在 lib.rs 的模块文档注释里，
 *   若不要求属性与 `fn` 相邻，会把测试辅助函数 `commands_declared` 认成命令。
 */

const APP_SRC = path.resolve(process.cwd(), "../app/src");
const FRONTEND_SRC = path.resolve(process.cwd(), "src");

/** 由 Tauri 注入、不经 IPC 传参的形参名/类型。 */
const INJECTED_NAMES = /^(state|app|window|webview|_app|_window)$/;

/**
 * 前端从未 `invoke(...)` 的命令白名单。新增一项必须在这里写明理由——
 * 「实现了但没有任何界面到得了」正是审计点名的一类缺陷（安全承诺与可达性不符），
 * 让它悄悄躺着与让它被删掉一样，都是不作数的收敛。
 */
const UNREACHED_BY_INVOKE: Record<string, string> = {
  // lib/vault.ts 经 `call<boolean>("vault_has_file")` 这个薄封装调用，不是字面量 invoke。
  vault_has_file: "经 lib/vault.ts 的 call() 封装调用，本测试只扫字面量 invoke",
  // vault_copy_to_clipboard 已于审计2 #27 接入 VaultManagerDialog 的记录列表，白名单条目随之删除。
  // 那条注释当时记着一处待裁决的文档冲突：ui-design.md 把独立凭据管理归 M4b，而
  // phase1-acceptance.md 又把「剪贴板定时清除（默认 30s、可配、可关）」列为 Phase 1 必须人工核验
  // 的出口项——入口在 M4b，验收在 Phase 1，该条目当时无法执行。
  // 裁决结果：入口提前到 Phase 1。理由不是「顺手」，是审计2 #27 要求的删除/轮换/备份/恢复
  // 本来就得有一个凭据列表来承载，复制按钮挂在同一行上是零额外面积，而它同时把那条
  // 悬空的验收项变得可执行。M4b 仍然要做的是完整的密钥/代理管理器（生成、导入、agent 交互）。
};

function walk(dir: string, out: string[] = []): string[] {
  for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
    const p = path.join(dir, e.name);
    if (e.isDirectory()) {
      if (e.name !== "node_modules") walk(p, out);
    } else out.push(p);
  }
  return out;
}

const camel = (s: string): string => s.replace(/_([a-z])/g, (_, c: string) => c.toUpperCase());

/** 按成对括号切分顶层逗号（泛型/嵌套对象里的逗号不算分隔符）。 */
function splitTopLevel(s: string, open: string, close: string): string[] {
  const parts: string[] = [];
  let depth = 0;
  let cur = "";
  for (const ch of s) {
    if (open.includes(ch)) depth++;
    if (close.includes(ch)) depth--;
    if (ch === "," && depth === 0) {
      parts.push(cur);
      cur = "";
    } else cur += ch;
  }
  parts.push(cur);
  return parts;
}

interface CommandDef {
  required: Set<string>;
  optional: Set<string>;
}

/** 扫 app/src 下所有 `#[tauri::command]`，取命令名与经 IPC 传入的形参。 */
function readRustCommands(): Map<string, CommandDef> {
  const cmds = new Map<string, CommandDef>();
  for (const f of walk(APP_SRC).filter((p) => p.endsWith(".rs"))) {
    const src = fs.readFileSync(f, "utf8");
    // 属性必须与 fn 相邻（中间只允许其它属性/文档注释行），否则文档注释里提到的
    // `#[tauri::command]` 会把后面随便哪个 fn 拽进来。
    const re = /^[ \t]*#\[tauri::command\][ \t]*\r?\n(?:[ \t]*(?:#\[[^\]]*\]|\/\/\/[^\n]*)[ \t]*\r?\n)*[ \t]*(?:pub[ \t]+)?(?:async[ \t]+)?fn[ \t]+([a-z0-9_]+)[ \t]*\(([\s\S]*?)\)[ \t]*->/gm;
    let m: RegExpExecArray | null;
    while ((m = re.exec(src))) {
      const [, name, argsRaw] = m;
      const required = new Set<string>();
      const optional = new Set<string>();
      for (const part of splitTopLevel(argsRaw, "<([", ">)]")) {
        const t = part.trim();
        if (!t) continue;
        const c = t.indexOf(":");
        if (c < 0) continue;
        const pname = t.slice(0, c).trim().replace(/^mut\s+/, "");
        const ptype = t.slice(c + 1).trim();
        if (INJECTED_NAMES.test(pname)) continue;
        if (/^State</.test(ptype) || /AppHandle/.test(ptype) || /^Window/.test(ptype)) continue;
        (/^Option</.test(ptype) ? optional : required).add(camel(pname));
      }
      cmds.set(name, { required, optional });
    }
  }
  return cmds;
}

interface CallSite {
  name: string;
  /** null = 参数不是对象字面量（变量/展开），无法静态核参数名，只核命令名存在性 */
  keys: Set<string> | null;
  where: string;
}

/** 把匹配到的整段换成等量换行：剥注释不能改行号，否则报错位置指向别处。 */
const blankKeepingLines = (s: string): string => s.replace(/[^\n]/g, "");

/**
 * 剥注释后再扫。**注释里写的 `invoke(...)` 不是调用点**，两个方向都会出错：
 * - 假阳：文档注释里引一句 `invoke("session_open")` 说明历史实现，被当成少传必填参数的调用点
 *   （实测：lib/open-session.ts 的缺陷说明里引了这一句，本测试当场转红而生产代码完全正确）；
 * - 假阴（更要命）：一处 `// await invoke("foo", {...})` 会让 `没有新增的不可达命令` 认为 foo
 *   已接上界面——注释冒充接线，正是这套门禁本身要防的那类缺陷。原实现只在**参数对象内部**
 *   剥注释，整份源码是照单全收的。
 */
function stripComments(src: string): string {
  return src
    .replace(/<!--[\s\S]*?-->/g, blankKeepingLines) // Svelte 模板注释
    .replace(/\/\*[\s\S]*?\*\//g, blankKeepingLines) // 块注释 / JSDoc
    .replace(/(^|[^:])\/\/[^\n]*/g, "$1"); // 行注释；[^:] 保住 https:// 这类串
}

/** 扫一份源码里的 `invoke("cmd", { ... })` 调用点（rel 仅用于错误定位）。 */
function scanInvokeCalls(raw: string, rel: string): CallSite[] {
  const calls: CallSite[] = [];
  {
    const src = stripComments(raw);
    const re = /\binvoke\s*(<)?/g;
    let m: RegExpExecArray | null;
    while ((m = re.exec(src))) {
      let i = re.lastIndex;
      if (m[1]) {
        let d = 1;
        while (i < src.length && d > 0) {
          if (src[i] === "<") d++;
          else if (src[i] === ">") d--;
          i++;
        }
      }
      while (i < src.length && /\s/.test(src[i])) i++;
      if (src[i] !== "(") continue;
      i++;
      const head = /^\s*"([a-z0-9_]+)"\s*(,)?/.exec(src.slice(i, i + 200));
      if (!head) continue;
      const line = src.slice(0, m.index).split("\n").length;
      const where = `${rel}:${line}`;
      re.lastIndex = i + head[0].length;
      let keys: Set<string> | null = new Set();
      if (head[2]) {
        let j = re.lastIndex;
        while (j < src.length && /\s/.test(src[j])) j++;
        if (src[j] !== "{") {
          keys = null; // 变量/展开，静态核不了
        } else {
          let d = 0;
          let k = j;
          for (; k < src.length; k++) {
            if (src[k] === "{") d++;
            else if (src[k] === "}") {
              d--;
              if (!d) break;
            }
          }
          const obj = src.slice(j + 1, k); // 注释已在 stripComments 里整份剥掉，此处无需再剥
          if (/\.\.\./.test(obj)) keys = null;
          else {
            for (const seg of splitTopLevel(obj, "{([", "})]")) {
              const key = /^([A-Za-z0-9_]+)\s*[:,]?/.exec(seg.trim());
              if (key) keys!.add(key[1]);
            }
          }
        }
      }
      calls.push({ name: head[1], keys, where });
    }
  }
  return calls;
}

/** 扫前端所有 `invoke("cmd", { ... })` 调用点。 */
function readInvokeCalls(): CallSite[] {
  const calls: CallSite[] = [];
  for (const f of walk(FRONTEND_SRC)) {
    if (!/\.(ts|svelte)$/.test(f) || /\.test\.ts$/.test(f)) continue;
    calls.push(...scanInvokeCalls(fs.readFileSync(f, "utf8"), path.relative(FRONTEND_SRC, f).replace(/\\/g, "/")));
  }
  return calls;
}

describe("IPC 契约：前端 invoke ↔ Rust #[tauri::command]", () => {
  const cmds = readRustCommands();
  const calls = readInvokeCalls();

  // 解析器本身失灵时（路径错、正则被改坏）上面两个集合会双双为空，
  // 而空集合让下面每条断言都平凡通过——那是最坏的一种绿。
  it("解析器确实读到了两侧", () => {
    expect(cmds.size, `没从 ${APP_SRC} 读出任何 #[tauri::command]`).toBeGreaterThan(20);
    expect(calls.length, `没从 ${FRONTEND_SRC} 读出任何 invoke 调用点`).toBeGreaterThan(20);
  });

  it("注释里的 invoke 不算调用点（注释既不能冒充接线，也不该被算成缺陷）", () => {
    // 块注释与 HTML 注释都写成**跨行**的：单行样本下「剥成空串」与「剥成等量换行」结果完全相同，
    // 行号断言会对着自己想防的那件事恒真（V20-M15 实测：整份变异零转红）。
    const src = [
      `/**`, //                                                           1
      ` * 历史实现是 \`await invoke("session_open")\`（无参形态）——`, //    2
      ` * 剥不掉就是一条假阳：生产代码完全正确却报「未传必填参数」。`, //     3
      ` */`, //                                                           4
      `// await invoke("ghost_cmd", { a: 1 });  ← 注释掉的接线不算接线`, // 5
      `<!--`, //                                                          6
      `  invoke("html_ghost")`, //                                        7
      `-->`, //                                                           8
      `const u = "https://example.com/x"; // 冒号后的双斜杠不是行注释`, //  9
      `await invoke("real_cmd", { b: 2 });`, //                          10
    ].join("\n");
    const found = scanInvokeCalls(src, "probe.ts");
    expect(found.map((c) => c.name)).toEqual(["real_cmd"]);
    expect(found[0].keys && [...found[0].keys]).toEqual(["b"]);
    expect(found[0].where).toBe("probe.ts:10"); // 剥注释按等量换行补齐，行号不能漂
  });

  it("每个 invoke 的命令名都有对应的 Rust 命令", () => {
    const bad = calls.filter((c) => !cmds.has(c.name)).map((c) => `${c.where} invoke("${c.name}")`);
    expect(bad, "命令名写错 = 运行期 Err，多数调用点吞进 catch，表现为点了没反应").toEqual([]);
  });

  it("invoke 传的每个参数名都在 Rust 形参里", () => {
    const bad: string[] = [];
    for (const c of calls) {
      const def = cmds.get(c.name);
      if (!def || c.keys === null) continue;
      for (const k of c.keys) {
        if (!def.required.has(k) && !def.optional.has(k)) {
          bad.push(`${c.where} invoke("${c.name}") 传了 "${k}"，Rust 形参只有 [${[...def.required, ...def.optional].join(", ")}]`);
        }
      }
    }
    expect(bad).toEqual([]);
  });

  it("Rust 侧每个非 Option 形参都被传到了", () => {
    const bad: string[] = [];
    for (const c of calls) {
      const def = cmds.get(c.name);
      if (!def || c.keys === null) continue;
      for (const k of def.required) {
        if (!c.keys.has(k)) bad.push(`${c.where} invoke("${c.name}") 未传必填参数 "${k}"`);
      }
    }
    expect(bad).toEqual([]);
  });

  it("没有新增的不可达命令（已实现但前端到不了）", () => {
    const invoked = new Set(calls.map((c) => c.name));
    const unreached = [...cmds.keys()].filter((n) => !invoked.has(n) && !(n in UNREACHED_BY_INVOKE));
    expect(unreached, "新命令必须要么接上界面，要么写进 UNREACHED_BY_INVOKE 并说明理由").toEqual([]);
  });

  it("白名单不留过期项（已接上界面的要从白名单里删掉）", () => {
    const invoked = new Set(calls.map((c) => c.name));
    const stale = Object.keys(UNREACHED_BY_INVOKE).filter(
      (n) => cmds.has(n) && invoked.has(n),
    );
    expect(stale, "这些命令前端已有字面量 invoke，白名单条目已过期").toEqual([]);
  });
});
