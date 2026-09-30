import { describe, expect, it } from "vitest";

/**
 * 设置键接线守卫：**写入方（控件）与消费方（真正据此改行为的代码）必须两两闭合，且变更要能广播出去。**
 *
 * 为什么另起一份、而不是加强 SettingsDialog.test.ts 的结构性用例：
 * 那条判的是「本文件里 settingSet 的键，本文件里必须 settingGet 回来」——它闭合的是**对话框内部**
 * 的一圈（写了要回读，否则重启后控件显示与库里存的各说各话）。它按其设计是对的，但它看不见跨文件：
 *
 *   ① 有消费方、没有任何写入方 ⇒ 界面根本给不出这个值。实测两处：
 *      · `term.scheme`：后端与 TerminalPane 都在读，全仓零 settingSet。§3.2 的三级作用域
 *        （全局 → Profile → 会话临时）最外层永久钉死在 DEFAULT_SCHEME，选项页那一栏只有一句
 *        写着内部键名的提示文字，没有控件。
 *      · `transfer.verifyAfterTransfer`：sftp_cmd.rs 以 `settings_bool(…, true)` 消费，
 *        §1.4 写的是「可选开关，默认开」——「可选」的那一半从来不存在。
 *   ② 写了、也回读了，但消费方在**运行期不会重读** ⇒ 改了不生效。实测：TerminalPane 的 7 个全局键
 *      只在 onMount 读一次，标签又是恒挂载的（隐藏≠销毁），于是改完设置对所有已打开的终端一律无效，
 *      而 §3.2 三处写着「即时生效」、组件自己的注释也写着「设置变更即时生效不重建终端」。
 *
 * 三处缺陷的共同点是**没有任何自动化门禁在看跨文件的那一段**，而人工看不见它：单看对话框是完整的，
 * 单看 TerminalPane 也是完整的，缺的是两者之间那条线。所以这份守卫扫的是全仓（含 Rust 侧消费方），
 * 判据是集合闭合，豁免必须具名并写明理由——与 action-wiring.test.ts 同一形制。
 */

/* ---------- 源码采集 ---------- */

// ?raw 取源文本（同 action-wiring.test.ts / menus.test.ts 既有口径：tsconfig 只装 vite/client 类型、
// 无 @types/node，且 vitest 模块运行器下 import.meta.url 非 file: 方案，readFileSync(new URL(…)) 会抛）。
// Rust 侧只取 `*/src/**`，天然排除 `crates/*/tests/**` 的集成测试；vitest.config.ts 为此放开了
// server.fs.allow（仅测试配置，dev server 与生产构建的 allow 不动）。
const FE_RAW = import.meta.glob("../**/*.{ts,svelte}", { query: "?raw", import: "default", eager: true }) as Record<string, string>;
const RS_RAW = {
  ...import.meta.glob("../../../app/src/**/*.rs", { query: "?raw", import: "default", eager: true }),
  ...import.meta.glob("../../../crates/*/src/**/*.rs", { query: "?raw", import: "default", eager: true }),
} as Record<string, string>;

/** glob 的 key 是相对**导入方**的路径（同目录文件是 `./ipc.ts`，别处是 `../components/X.svelte`），
 *  故一律按文件名比对——本仓这些文件名全仓唯一，findSrc 会核这一点。 */
const fileName = (p: string): string => p.replace(/^.*\//, "");

/** 前端生产源码：剔除测试自身（测试里的 settingSet 桩不是产品写入方）。 */
const FE = Object.entries(FE_RAW).filter(([p]) => !p.includes(".test.") && fileName(p) !== "test-setup.ts");
/** 设置读写的**实现文件**本身不是调用点：ipc.ts 里的 `settingGet<T>(key: string…)` 是函数签名。 */
const CALLERS = FE.filter(([p]) => fileName(p) !== "ipc.ts");

/** 剥注释：解释接线的注释里必然写着这些键名，直接扫全文等于让注释给自己作证（同 action-wiring）。 */
function code(src: string): string {
  return src
    .replace(/<!--[\s\S]*?-->/g, "")
    .replace(/\/\*[\s\S]*?\*\//g, "")
    .replace(/(^|[^:])\/\/.*$/gm, "$1"); // [^:] 让 https:// 里的双斜杠不被当成行注释
}

/** Rust 侧再剥掉 `#[cfg(test)] mod … { … }`：测试模块里出现的键名不能算生产消费方。 */
function stripRustTests(src: string): string {
  const lines = code(src).split("\n");
  const out: string[] = [];
  for (let i = 0; i < lines.length; i++) {
    if (!/^\s*#\[cfg\(test\)\]/.test(lines[i])) { out.push(lines[i]); continue; }
    let j = i;
    while (j < lines.length && !lines[j].includes("{")) j++;
    let depth = 0;
    for (; j < lines.length; j++) {
      depth += (lines[j].match(/\{/g)?.length ?? 0) - (lines[j].match(/\}/g)?.length ?? 0);
      if (depth <= 0) break;
    }
    i = j; // 整个 mod 丢弃
  }
  return out.join("\n");
}

/**
 * 按文件名取剥注释后的源文本。**不用 endsWith(路径尾) 匹配**：`import.meta.glob` 的 key 相对
 * 导入方目录，本文件在 src/lib/ 下，于是同目录的 ipc.ts 回来是 `./ipc.ts`——`endsWith("/lib/ipc.ts")`
 * 永远匹配不到，find 返回 undefined、srcOf 得空串，而空串对 `not.toMatch` 是恒真的：
 * 断言会全绿地什么也没测。命中数必须恰为 1，否则宁可抛。
 */
function findSrc(name: string): string {
  const hits = Object.keys(FE_RAW).filter((p) => fileName(p) === name);
  if (hits.length !== 1) throw new Error(`findSrc(${name}) 命中 ${hits.length} 个：${hits.join(", ")}`);
  return code(FE_RAW[hits[0]]);
}

/**
 * findSrc 会抛，所以它**必须只在 it() 体内求值**——在模块体或 describe 体里抛出，vitest 记的是
 * **收集失败**而非用例失败：整份文件的断言一起从 total 里消失（total 掉、failed 仍是 0），
 * CI 照样红，但诊断指向「文件没跑」而不是「哪条不变量破了」，且同文件其余断言当次一条都没跑。
 * 这个坑在 egress-contract.test.ts 上踩过一次并以变异体验证过（改名藏文件 ⇒ total 必须不动）。
 * lazy 让调用点写起来仍像常量，求值却推迟到第一次真正读它的那条用例里。
 */
function lazy(f: () => string): () => string {
  let v: string, done = false;
  return () => { if (!done) { v = f(); done = true; } return v; };
}

/* ---------- 键名提取 ---------- */

/**
 * 常量键表：`export const KEYBOARD_MODE_SETTING_KEY = "keyboard.mode"` 这类。
 * 不解析它们的话，经常量调用的 settingGet/settingSet 会整条漏掉——`keyboard.mode` 与 `ui.theme`
 * 都是这么写的，漏掉即误报「有消费无写入」。
 */
const CONST_KEYS = new Map<string, string>();
for (const [, src] of FE) {
  for (const m of code(src).matchAll(/\b(?:const|let)\s+([A-Z][A-Z0-9_]*)\s*(?::[^=\n]*)?=\s*"([a-z][\w]*\.[\w.]+)"/g)) {
    CONST_KEYS.set(m[1], m[2]);
  }
}

function keysIn(src: string, fn: "settingSet" | "settingGet"): Set<string> {
  const found = new Set<string>();
  const g = fn === "settingGet" ? "(?:<[^>]*>)?" : "";
  for (const m of src.matchAll(new RegExp(`${fn}${g}\\(\\s*"([a-z][\\w]*\\.[\\w.]+)"`, "g"))) found.add(m[1]);
  for (const m of src.matchAll(new RegExp(`${fn}${g}\\(\\s*([A-Z][A-Z0-9_]*)\\b`, "g"))) {
    const k = CONST_KEYS.get(m[1]);
    if (k) found.add(k);
  }
  return found;
}

/**
 * 「控件回读面」：这些文件里的 settingGet 只是把库里的值显示回自己的控件，**不是消费**。
 * 不区分的话，一个「写进去又读回来给自己看、功能侧根本不认」的键会自证闭合——
 * `term.scheme` 修复后正是这个形态（选项页写、选项页读），若把回读算消费就再也测不出第二次退化。
 */
const CONTROL_SURFACES: Record<string, string> = {
  "SettingsDialog.svelte": "选项对话框：onMount 的 settingGet 逐项回填控件（口径见该文件 onMount 文档注）",
  "TransferQueueDrawer.svelte": "传输抽屉：只回填「传后校验」勾选框（该键的第二处控件，随手可及但**有条件渲染**，见下方可达性一节），消费方在 Rust 侧 sftp_cmd.rs",
};

/** W = 写入方（前端控件）。 */
const WRITERS = new Map<string, string[]>();
/** C = 消费方（据此改行为的代码：控件回读面之外的前端读取 + Rust 侧读取）。 */
const CONSUMERS = new Map<string, string[]>();
const push = (m: Map<string, string[]>, k: string, where: string) => m.set(k, [...(m.get(k) ?? []), where]);

for (const [path, src] of CALLERS) {
  const c = code(src);
  for (const k of keysIn(c, "settingSet")) push(WRITERS, k, fileName(path));
  if (CONTROL_SURFACES[fileName(path)]) continue; // 回读面：settingGet 不计入消费
  for (const k of keysIn(c, "settingGet")) push(CONSUMERS, k, fileName(path));
}
/**
 * Rust 侧的常量键表，与前端 CONST_KEYS 同一用意。
 *
 * 补这一段是因为守卫漏过一个真键：`update.manifestUrl` 的消费方 `update_cmd.rs` 写的是
 * `SettingsRepo::new(…).get(UPDATE_URL_KEY)`，而下面的消费方正则只认字面量，于是该键被判成
 * 「写进库里没人看」。前端侧一开始就解析了常量（keyboard.mode / ui.theme 都是常量写法），
 * Rust 侧没有——这不是那个键的问题，是**扫查两侧口径不一致**：同一种写法在前端算数、
 * 在后端不算数。按常量写键在 Rust 里反而是更规范的做法（键名要被 `#[cfg(test)]` 断言引用），
 * 越规范越容易漏，正是最坏的一种假绿。
 */
const RS_CONST_KEYS = new Map<string, string>();
for (const src of Object.values(RS_RAW)) {
  for (const m of stripRustTests(src).matchAll(
    /\bconst\s+([A-Z][A-Z0-9_]*)\s*:\s*&(?:'static\s+)?str\s*=\s*"([a-z][\w]*\.[\w.]+)"/g,
  )) {
    RS_CONST_KEYS.set(m[1], m[2]);
  }
}

for (const [path, src] of Object.entries(RS_RAW)) {
  const rs = stripRustTests(src);
  for (const m of rs.matchAll(/settings_bool\(\s*"([\w.]+)"|settings_string\(\s*"([\w.]+)"|\.get\(\s*"([a-z][\w]*\.[\w.]+)"/g)) {
    push(CONSUMERS, m[1] ?? m[2] ?? m[3], `rust:${fileName(path)}`);
  }
  // 经常量取值：`.get(UPDATE_URL_KEY)`、`settings_bool(SOME_KEY, …)`。
  // 末尾允许 `)` 或 `,`，以免匹配到 `.get(SOMETHING_ELSE_ENTIRELY_LONGER)` 的前缀。
  for (const m of rs.matchAll(/(?:\.get|settings_bool|settings_string)\(\s*([A-Z][A-Z0-9_]*)\s*[,)]/g)) {
    const k = RS_CONST_KEYS.get(m[1]);
    if (k) push(CONSUMERS, k, `rust:${fileName(path)}`);
  }
}

/* ---------- 豁免表（加一条 = 宣布一件事，要写清楚是哪件事） ---------- */

/**
 * 「写了但扫不到消费方」的豁免。只有两条，且性质不同：
 * 第一条是本扫查的**分辨力边界**（写入方与消费方同在一个文件里时，「回读给控件看」与
 * 「读出来驱动行为」在文本上没有区别）；第二条不是边界，是一个真实的残留。
 *
 * 起草时这张表还有 ui.theme 与 ui.sidebarWidth 两条，理由同样写作「读写同文件」——错的：
 * 它们的消费方分别在 theme/store.ts 与 layout.ts，**都不是控件所在的 SettingsDialog**，
 * 按本扫查的文件口径本就算消费方。是下面「豁免表不留死条目」那条把这两句自说自话的理由判红的。
 */
const EXEMPT_NO_CONSUMER: Record<string, string> = {
  "ui.density": "消费方是 SettingsDialog 自己的 $effect（写 --fs-density 令牌），与控件同文件 ⇒ 扫查按文件分辨不出",
  "term.bell": "**确无消费方**：tabs.ts 的 ringBell 不读它。当前无害——唯一非默认选项 badge+notify 在 UI 上 disabled（系统通知即将推出），控件只能取到默认值 badge，恰等于 ringBell 的固定行为。**放开该选项时必须同时给 ringBell 接上消费端并删掉本条豁免**",
};

/** 「有消费方但无写入方」的豁免。空 = 目前每个被消费的键都有控件可写。 */
const EXEMPT_NO_WRITER: Record<string, string> = {};

/**
 * 「由专属命令代写」的键。
 *
 * `ai.providers` 不由前端 `settingSet` 写——它经 `ai_provider_save` / `ai_provider_delete`
 * 两个 Tauri 命令写库，因为写它的同时还要往 Vault 里存 API key，两件事必须在
 * 同一个后端调用里成对完成。让前端直接 `settingSet("ai.providers", …)` 反而是错的：
 * 那条路会让「条目写进去了、key 没进 Vault」成为可能。
 *
 * 所以这里**不是豁免表**。豁免是「我保证它没问题」，而本仓的守卫从不接受这种保证。
 * 下面那条用例对每一项核三件事，每一件都能独立失败：
 *
 *   1. Rust 侧确有这个命令，且它所在文件里出现了这个键（后端真的写它）；
 *   2. 前端确有 `invoke("<命令名>")`（这条路真的被走）；
 *   3. 那个 invoke 出现在 `ui` 指名的组件里（用户到得了的那个面）。
 *
 * 三件里少任何一件都红。把命令改名、把面板删掉、把键换掉，本守卫都会先红。
 */
const COMMAND_WRITTEN: Record<string, { cmds: readonly string[]; ui: string }> = {
  // MCP 开关（2026-08-28）：经 mcp_set_enabled 写——它不只是写设置位，
  // 还要起/停 localhost 监听（只写位的开关是假开关）。可达性：设置 → AI ②区，
  // 无条件可达。
  "mcp.enabled": { cmds: ["mcp_set_enabled"], ui: "SettingsDialog.svelte" },
  // MCP token（2026-08-28）：程序生成的秘密，不适合经 settingSet（那会把
  // 明文 token 经通用设置通道来回传）。经 ensure/regen 命令生成与轮换，
  // 前端只在设置页展示。可达性同上。
  "mcp.token": { cmds: ["mcp_token_get", "mcp_token_regen"], ui: "SettingsDialog.svelte" },
  "ai.providers": {
    cmds: ["ai_provider_save", "ai_provider_delete"],
    // 可达性判定（本守卫要求的那个决定）：**通过**。AI 面板由菜单「工具 → AI 助手」
    // 与 Ctrl+Shift+A 打开，不依赖任何运行期数据；面板未配置时**直接落在配置页**
    // （见 AiPanel 的 refresh：`if (!status.configured) tab = "config"`）。
    // 也就是说「后端在动作当刻要读模型源」这件事发生之前，用户必然经过这个面。
    ui: "AiPanel.svelte",
  },
};


/* ---------- 用例 ---------- */

describe("设置键接线：写入方与消费方两两闭合", () => {
  it("扫查自身有效：前端与 Rust 两侧都采到源码，常量键已解析", () => {
    // 任一 glob 失效都会让下面两条用例「集合为空 ⇒ 全绿」，故先钉住采集量与两个已知锚点。
    expect(CALLERS.length).toBeGreaterThan(20);
    expect(Object.keys(RS_RAW).length).toBeGreaterThan(20);
    expect(CONST_KEYS.get("KEYBOARD_MODE_SETTING_KEY")).toBe("keyboard.mode");
    expect(CONST_KEYS.get("THEME_SETTING_KEY")).toBe("ui.theme");
    // Rust 侧常量解析的锚点：这一句失效时，经常量取值的消费方会整批变成「无人读」，
    // 而症状会落在**被消费的那个键**上，看起来像那个键接错了线。
    expect(RS_CONST_KEYS.get("UPDATE_URL_KEY")).toBe("update.manifestUrl");
    expect(WRITERS.size).toBeGreaterThanOrEqual(18);
    // Rust 侧锚点：这一处正是「消费而无写入方」的原始现场
    expect(CONSUMERS.get("transfer.verifyAfterTransfer")).toContain("rust:sftp_cmd.rs");
  });

  it("每个被消费的设置键都有前端写入方（界面给得出这个值）", () => {
    const orphans = [...CONSUMERS.keys()]
      .filter((k) => !WRITERS.has(k) && !EXEMPT_NO_WRITER[k] && !COMMAND_WRITTEN[k])
      .map((k) => `${k} ← 消费于 ${CONSUMERS.get(k)!.join("/")}，但全仓无 settingSet`)
      .sort();
    expect(orphans).toEqual([]);
  });

  /**
   * COMMAND_WRITTEN 的三件核查。
   *
   * 这条用例是那张表能存在的**唯一理由**。没有它，`COMMAND_WRITTEN` 就是一张豁免表，
   * 而豁免表的问题不是它一开始写错，是它**永远不会再红**：命令改了名、面板删掉了、
   * 键换成别的了，豁免仍然安静地生效，而那个键从此没有任何写入方。
   *
   * 反向断言也在这里：表里的键必须真的**不**被前端 settingSet 写。若哪天有人给它加了
   * 直接写入的控件，这个键就该回到主判据下，而不是继续挂在这张表上——两条路并存时，
   * 「API key 有没有一起进 Vault」就又变成了不确定的事。
   */
  it("命令代写的键：后端真的写它、前端真的调它、且调用点在用户到得了的面上", () => {
    const rsAll = Object.entries(RS_RAW).map(([p, src]) => [fileName(p), code(src)] as const);
    const feAll = CALLERS.map(([p, src]) => [fileName(p), code(src)] as const);

    for (const [key, { cmds, ui }] of Object.entries(COMMAND_WRITTEN)) {
      // ① 后端：命令存在，且其所在文件出现这个键（字面量或已解析的常量名）
      for (const cmd of cmds) {
        const owner = rsAll.find(([, src]) => src.includes(`pub async fn ${cmd}(`) || src.includes(`pub fn ${cmd}(`));
        expect(owner, `${key}：Rust 侧找不到命令 ${cmd}`).toBeDefined();
        const constNames = [...RS_CONST_KEYS.entries()].filter(([, v]) => v === key).map(([n]) => n);
        const mentionsKey =
          owner![1].includes(`"${key}"`) || constNames.some((n) => owner![1].includes(n));
        expect(mentionsKey, `${key}：${owner![0]} 里的 ${cmd} 没提到这个键——它到底写的是什么？`).toBe(true);
      }

      // ② 前端：这条路真的被走
      const callers = feAll.filter(([, src]) => cmds.some((c) => src.includes(`"${c}"`))).map(([n]) => n);
      expect(callers.length, `${key}：全仓无人 invoke ${cmds.join("/")}——这条写入路径是死的`).toBeGreaterThan(0);

      // ③ 可达面：调用点在指名的那个组件里
      expect(callers, `${key}：可达面应是 ${ui}`).toContain(ui);

      // ④ 反向：它确实不由前端 settingSet 写（否则不该待在这张表里）
      const directWriters = [...WRITERS.keys()].filter((k) => k === key);
      expect(directWriters, `${key} 已有前端直接写入方，应移出 COMMAND_WRITTEN 走主判据`).toEqual([]);
    }
  });

  it("每个被写入的设置键都有消费方（不是写进库里没人看）", () => {
    const inert = [...WRITERS.keys()]
      .filter((k) => !CONSUMERS.has(k) && !EXEMPT_NO_CONSUMER[k])
      .map((k) => `${k} → 写于 ${WRITERS.get(k)!.join("/")}，但无人读`)
      .sort();
    expect(inert).toEqual([]);
  });

  it("豁免表不留死条目：豁免的键必须确实处于被豁免的那个状态", () => {
    // 缺陷修好后忘了删豁免，下一次同款退化就会被这条早已失效的豁免放行。
    for (const k of Object.keys(EXEMPT_NO_CONSUMER)) {
      expect(WRITERS.has(k), `${k} 已无写入方，EXEMPT_NO_CONSUMER 里的条目该删了`).toBe(true);
      expect(CONSUMERS.has(k), `${k} 已经有消费方了，EXEMPT_NO_CONSUMER 里的条目该删了`).toBe(false);
    }
    for (const k of Object.keys(EXEMPT_NO_WRITER)) {
      expect(CONSUMERS.has(k), `${k} 已无消费方，EXEMPT_NO_WRITER 里的条目该删了`).toBe(true);
      expect(WRITERS.has(k), `${k} 已经有写入方了，EXEMPT_NO_WRITER 里的条目该删了`).toBe(false);
    }
    for (const f of Object.keys(CONTROL_SURFACES)) {
      expect(CALLERS.some(([p]) => fileName(p) === f), `${f} 不在扫查范围内，回读面条目已失效`).toBe(true);
    }
  });

  // 规格点名必须存在的两个控件：§2.12「终端外观（全局字体/**默认配色方案**/背景透明度/bell 行为）」，
  // §1.4 + §附表「传后校验（可选开关，默认开）」载体 = TransferQueueDrawer。
  // 上面的集合闭合已能测出它们，但那要等到「有人消费」才成立；这两条直接钉控件本身，
  // 使得「把控件删了、顺手把消费方也删了」这种整片退化仍然会红。
  it("规格点名的两个控件确实存在，且不是一句提示文字", () => {
    const dlg = findSrc("SettingsDialog.svelte");
    expect(dlg).toMatch(/data-testid="set-scheme"/);
    expect(dlg).toMatch(/settingSet\("term\.scheme", scheme\)/);
    expect(dlg).toMatch(/\{#each SCHEMES as s \(s\.id\)\}/); // 选项来自方案表本身，不是硬编码几项
    const drawer = findSrc("TransferQueueDrawer.svelte");
    expect(drawer).toMatch(/data-testid="set-verify-after-transfer"/);
    expect(drawer).toMatch(/settingSet\("transfer\.verifyAfterTransfer", verifyAfterTransfer\)/);
  });
});

describe("设置变更广播：改了要对已挂载的终端即时生效（UI 规格 §3.2）", () => {
  const IPC = lazy(() => findSrc("ipc.ts"));
  const PANE = lazy(() => findSrc("TerminalPane.svelte"));

  it("广播挂在 settingSet 这唯一写入口上，且只在落库成功后发", () => {
    expect(IPC()).toMatch(/export const settingChanged/);
    // 失败分支必须先 return：写库失败还广播的话，界面会显示一个重启即消失的值
    expect(IPC()).toMatch(/catch \(e\) \{[\s\S]{0,200}?return false;\s*\}\s*settingBus\.set\(\{ key, value \}\)/);
  });

  it("TerminalPane 订阅了变更，且 onMount 读到的每个全局键都在分发表里", () => {
    expect(PANE()).toMatch(/settingChanged\.subscribe\(/);
    // 缺退订则每关一个标签留一个持有已 dispose 终端的处理器
    expect(PANE()).toMatch(/unSetting\?\.\(\)/);
    // 这条是 C-1 的正解：**凡本组件 onMount 读的全局键，必须有对应的 case**。
    // 将来加第 8 个全局键、只加了 settingGet 没加 case，这里会红——而那正是原缺陷的形态。
    const read = [...keysIn(PANE(), "settingGet")];
    expect(read.length).toBeGreaterThanOrEqual(7);
    const unrouted = read.filter((k) => !PANE().includes(`case "${k}":`)).sort();
    expect(unrouted).toEqual([]);
  });

  it("透明度可在运行期重设（原实现里它被 setScheme 的闭包钉死在构造期）", () => {
    const term = findSrc("term.ts");
    expect(term).toMatch(/setOpacity\(percent: number\)/);
    // setScheme 若仍读构造期的 opts.opacity，换配色就会把透明度打回初值
    expect(term).not.toMatch(/setScheme\(s: Scheme\) \{[\s\S]{0,200}?opts\.opacity/);
    expect(PANE()).toMatch(/ctrl\.setOpacity\(globalOpacity\)/);
  });

  it("守卫自身有效：不存在的键判死，注释里的键不算数", () => {
    expect(WRITERS.has("term.thisDoesNotExist")).toBe(false);
    expect(WRITERS.has("term.scheme")).toBe(true); // 反向对照
    expect(keysIn(code('// 由后续 Task 接线：settingSet("term.ghost", x)'), "settingSet").size).toBe(0);
  });
});

/**
 * 控件的**可达性**：闭合 ≠ 改得到。
 *
 * 上一节判的是「这个键有没有写入方」。`transfer.verifyAfterTransfer` 曾经通过那一节——
 * TransferQueueDrawer 里确实有一个勾选框——但整个抽屉包在 `{#if $rows.size > 0}` 里，
 * 本会话有第一件传输之前根本不渲染。而这个键是**持久**的，后端又在 `transfer_submit`
 * **当刻**读它（sftp_cmd.rs 的 `settings_bool(…, true)`）。三件事凑在一起就是个死角：
 *
 *   上次会话关掉传后校验 → 本次启动，队列空、抽屉不渲染 → 想开却找不到开关
 *   → 提交第一件传输，作业已按「关」建好（verify: None）→ 抽屉这时才出现，
 *     再勾上也追不回那一件。
 *
 * 而「那一件」恰恰是最该被校验的一件：它是重新开始信任这条链路的第一次落盘。
 * 同类形态审计里点过一次（P1-16：Vault 自动锁定的设置是个当时无人执行的承诺），
 * 区别只在这次界面**给了**控件、只是在需要它的那一刻拿不到。
 *
 * 故此处的判据不是「有没有控件」，而是「**后端在动作当刻据以决定行为的键，用户能不能在动作
 * 发生之前改到**」。无条件可达的面只有选项对话框一处（菜单/快捷键随时可开，不依赖任何运行期数据）。
 */
describe("设置控件可达性：动作当刻才读的键，必须在动作之前改得到", () => {
  /** Rust 侧消费的键 = 后端行为直接取值于此的键。 */
  const RUST_CONSUMED = [...CONSUMERS.entries()]
    .filter(([, where]) => where.some((w) => w.startsWith("rust:")))
    .map(([k]) => k)
    .sort();

  it("扫查自身有效：Rust 侧消费键就是这十五个（新增一个即在此处做决定）", () => {
    // 钉死集合而非只钉下限：将来后端多读一个持久键，这条会先红，逼着人回答
    // 「它在选项页里改得到吗」——而不是等某个用户在动作前找不到开关。
    // 六个都是**动作当刻**才取值的：
    //   security.clipboardClear  → vault_cmd.rs 每次复制时决定要不要定时清、清多久
    //   session.log             → state.rs 在 session_open 当刻决定要不要转录、写哪里（M4a）
    //   sftp.sandboxRoot         → state.rs 解析下载落点时
    //   term.customSchemes      → import_cmd.rs 在落库导入的配色时读现有列表（M4b）
    //   term.zmodem             → state.rs 在 session_open 当刻决定要不要装 ZMODEM 拦截器（M4a）
    //   transfer.verifyAfterTransfer → sftp_cmd.rs 在 transfer_submit 当刻建作业时
    //   update.manifestUrl      → update_cmd.rs 在用户点「检查更新」当刻取地址（M4b）
    //   vault.autoLockMinutes    → vault_cmd.rs 自锁巡检每轮
    //
    // term.customSchemes 的可达性判定：**通过**，「终端外观」页列出每套导入的配色并各带一个
    // 删除按钮。这一条的可达性方向与别的键相反：其余键是「后端要用之前改得到」，这一条是
    // **「加进去之后删得掉」**——导入路径只写不删，本页那个删除按钮是唯一的出口。
    //
    // update.manifestUrl 的可达性判定：**通过**，「更新」页有输入框，随选项对话框无条件可达。
    // 这个键比其余六个更依赖可达性，因为它的**空值是一条安全承诺**（不填 = 永不联网）：
    // 用户要撤回「允许它联网」这个决定，唯一的办法就是把这个框清空。若哪天这个输入框被挪进
    // 某个条件渲染的容器，用户就会处在「地址还在库里、界面上却找不到地方删」的状态——
    // 与 transfer.verifyAfterTransfer 当初那个死角同型，只是方向相反（那个是开不了，这个是关不掉）。
    //
    // term.zmodem 的可达性判定：**通过**，与 session.log 同一处（「安全与 Vault」页，
    // 选项对话框无条件可达）。它同样是连接**建立时**定格的——改完要重连才生效，
    // 所以「连接之前改得到」正是本守卫要保的那件事。
    //
    // session.log 的可达性判定（本守卫要求的那个决定）：**通过**。它在
    // 「安全与 Vault」页有控件，而选项对话框是无条件可达的（菜单/快捷键随时可开，
    // 不依赖任何运行期数据）——用户在发起连接**之前**总能改到。若将来把这个开关
    // 挪进某个条件渲染的抽屉，本守卫下面那条会立刻红。
    expect(RUST_CONSUMED).toEqual([
      // ai.mode 的可达性判定：**通过**，选项页「AI」有 set-ai-mode 控件（无条件可达）。
      // ai.providers 的判定见 COMMAND_WRITTEN——它由 ai_provider_save 代写，面板配置页是那个面。
      "ai.mode",
      "ai.providers",
      // mcp.* 的可达性判定：**通过**，选项页「AI」的 MCP 区有 set-mcp-enabled /
      // set-mcp-caller / set-mcp-tools 控件（无条件可达）。三键在 MCP 服务**启动当刻**
      // 读（app/src/mcp::build_server），改完要重启传输才生效——「启动之前改得到」
      // 正是本守卫要保的。
      "mcp.caller",
      "mcp.enabled",
      "mcp.mounts",
      "mcp.token",
      "mcp.tools",
      "security.clipboardClear",
      "session.log",
      "sftp.sandboxRoot",
      "term.customSchemes",
      "term.zmodem",
      "transfer.verifyAfterTransfer",
      "update.manifestUrl",
      "vault.autoLockMinutes",
    ]);
  });

  it("每个 Rust 侧消费的键，在选项对话框（无条件可达）里都有控件", () => {
    const dlg = keysIn(findSrc("SettingsDialog.svelte"), "settingSet");
    const unreachable = RUST_CONSUMED.filter((k) => !dlg.has(k) && !COMMAND_WRITTEN[k])
      .map((k) => `${k} ← 后端在动作当刻读它，但选项对话框里没有控件；` +
        `其余写入方 ${(WRITERS.get(k) ?? []).join("/")} 是否恒可达？`)
      .sort();
    expect(unreachable).toEqual([]);
  });

  it("两处「传后校验」控件都在，且经 settingChanged 同步（不会各显各的值）", () => {
    const drawer = findSrc("TransferQueueDrawer.svelte");
    const dlg = findSrc("SettingsDialog.svelte");
    // 两个 testid 必须不同，否则 getByTestId 在同屏时抛「found multiple」
    expect(dlg).toMatch(/data-testid="set-verify-after-transfer-global"/);
    expect(drawer).toMatch(/data-testid="set-verify-after-transfer"(?!-)/);
    // 抽屉跟随广播：否则在抽屉开着时改选项页，抽屉那个勾选框停在旧值，
    // 用户看到两个自称同一件事的开关处于相反状态。
    expect(drawer).toMatch(/settingChanged\.subscribe\(/);
    expect(drawer).toMatch(/ev\?\.key === "transfer\.verifyAfterTransfer"/);
    expect(drawer).toMatch(/unSetting\?\.\(\)/); // 缺退订 = 每次抽屉销毁留一个订阅
  });

  it("抽屉的订阅建立在首读之后（settingChanged 保留末值，顺序颠倒则陈旧值取胜）", () => {
    // settingChanged 是 writable(初值 null)，订阅当场就会收到一次末值重放。
    // 若先订阅、后 await settingGet，那次 await 落地的**旧**读会把刚广播来的新值盖回去。
    // 这是纯顺序性缺陷：两行都在、都对，只是写反了，任何「控件存在」类断言都看不见。
    const drawer = findSrc("TransferQueueDrawer.svelte");
    const iRead = drawer.indexOf('await settingGet<boolean>("transfer.verifyAfterTransfer"');
    const iSub = drawer.indexOf("settingChanged.subscribe(");
    expect(iRead).toBeGreaterThanOrEqual(0); // 两个锚点都必须真的找到，否则 -1 < -1 恒假成不了证据
    expect(iSub).toBeGreaterThanOrEqual(0);
    expect(iSub).toBeGreaterThan(iRead);
  });

  it("三处默认值逐字一致：控件初值 = 前端 fallback = 后端 fallback（都为开）", () => {
    // 走散的后果不是崩溃，是「界面显示的、库里存的、后端实际用的」是三个值。
    for (const src of [findSrc("TransferQueueDrawer.svelte"), findSrc("SettingsDialog.svelte")]) {
      expect(src).toMatch(/let verifyAfterTransfer = \$state\(true\)/);
      expect(src).toMatch(/settingGet<boolean>\("transfer\.verifyAfterTransfer", true\)/);
    }
    const rs = Object.entries(RS_RAW).find(([p]) => fileName(p) === "sftp_cmd.rs");
    expect(rs, "sftp_cmd.rs 不在扫查范围内，本条已失效").toBeTruthy();
    expect(stripRustTests(rs![1])).toMatch(/settings_bool\(\s*"transfer\.verifyAfterTransfer",\s*true\s*\)/);
  });
});
