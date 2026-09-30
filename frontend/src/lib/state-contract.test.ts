/**
 * Tauri 命令的 `State<'_, T>` 必须是 lib.rs 真 `manage` 过的类型（2026-09-02）。
 *
 * 缺陷现场：`audit_verify` / `audit_verify_quick` / `audit_export` 写的是 `State<'_, AppState>`，
 * 而 setup 里 manage 的是 `Arc<AppState>`。编译零告警，ipc-contract 门禁也绿（它只对命令名与参数名），
 * 点菜单「审计链校验」时才在运行期报 `state not managed for field state on command audit_verify`——
 * 审计这件事的全部价值就在出事之后那一次校验，而那一次此前必然失败。
 *
 * 这是 IPC 契约的第三维（命令名 / 参数名 / **状态类型**），前两维已有门禁，这里补第三维。
 */
import fs from "node:fs";
import path from "node:path";
import { describe, expect, it } from "vitest";

const APP_SRC = path.resolve(process.cwd(), "../app/src");

function walk(dir: string, out: string[] = []): string[] {
  for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
    const p = path.join(dir, e.name);
    if (e.isDirectory()) walk(p, out);
    else if (e.name.endsWith(".rs")) out.push(p);
  }
  return out;
}

/** lib.rs 里 `app.manage(<expr>)` 管起来的类型名集合（取路径最后一段；变量 `state` 解析到它的 `let`）。 */
function managedTypes(libSrc: string): Set<string> {
  const out = new Set<string>();
  for (const m of libSrc.matchAll(/app\.manage\(\s*([A-Za-z_][\w:]*)/g)) {
    const expr = m[1];
    if (expr === "state") {
      const decl = /let state\s*=\s*Arc::new\(AppState\b/.test(libSrc);
      if (decl) out.add("Arc<AppState>");
      continue;
    }
    out.add(expr.split("::").pop()!);
  }
  return out;
}

/** 惰性取用（gate-hygiene 口径）：源文件在 it 体内才读，改名/删除时是该红的用例转红，而不是整份门禁收集失败消失。 */
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

const SCAN = lazy(() => {
  const lib = fs.readFileSync(path.join(APP_SRC, "lib.rs"), "utf8");
  const managed = managedTypes(lib);
  const uses: Array<{ file: string; ty: string }> = [];
  for (const f of walk(APP_SRC)) {
    const src = fs.readFileSync(f, "utf8");
    // 归一：去空白、去模块路径（std::sync::Arc<crate::state::AppState> → Arc<AppState>），只比类型名
    for (const m of src.matchAll(/State<'_,\s*((?:[\w:]+)(?:<[^>]*>)?)\s*>/g)) uses.push({ file: path.relative(APP_SRC, f), ty: m[1].replace(/\s+/g, "").replace(/\b\w+::/g, "") });
  }
  return { managed, uses };
});

describe("命令的 State<'_, T> 与 setup 里 manage 的类型一致", () => {
  it("非空证明：解析到了 manage 列表与大量 State 参数", () => {
    const { managed, uses } = SCAN();
    expect([...managed].sort()).toEqual(expect.arrayContaining(["Arc<AppState>", "McpGlobal", "RdpGlobal"]));
    expect(uses.length).toBeGreaterThanOrEqual(100);
  });

  it("没有一处裸 State<'_, AppState>（管起来的是 Arc<AppState>，裸的在运行期 state not managed）", () => {
    const bare = SCAN().uses.filter((u) => u.ty === "AppState").map((u) => u.file);
    expect(bare).toEqual([]);
  });

  it("每个 State<'_, T> 的 T 都在 manage 列表里", () => {
    const { managed, uses } = SCAN();
    const orphans = uses.filter((u) => !managed.has(u.ty)).map((u) => `${u.file}: ${u.ty}`);
    expect(orphans).toEqual([]);
  });
});
