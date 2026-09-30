import { describe, expect, it } from "vitest";
// ?raw 取源文本（同 menus.test.ts / session-restore.test.ts 既有口径：tsconfig 只装 vite/client 类型、
// 无 @types/node，且 vitest 模块运行器下 import.meta.url 非 file: 方案，readFileSync(new URL(…)) 会抛）。
import APP_SRC from "../App.svelte?raw";
import TOOLBAR_SRC from "../components/ToolBar.svelte?raw";
import KEYMAP_SRC from "./keymap.ts?raw";
import PROFILE_IO_SRC from "./profile-io.ts?raw";
import { currentMenus } from "./menus";

/**
 * 动作接线守卫：**凡是用户能触发的动作 id，App.svelte 的 onAction 必须有实体分支。**
 *
 * 为什么另起一份、而不是加强 menus.test.ts 里的 S262：
 * S262 判的是「id 以整词形式出现在 App.svelte」，并在自己的注释里明写「不试图区分实体与台账，
 * 台账本身就是合法的承接形态」。那条守卫按其设计是对的——它防的是**零记录**。
 * 但实际发生的事情是：Task 18/20 在 onAction 里留下一段「由后续 Task 接线」的注释台账，
 * 随后两个 Task 都收尾了，接线从没发生，台账留在原地继续满足守卫。结果是
 *   菜单 11 项（导入/导出、编辑整组 8 项、字体放大/缩小）
 *   + 工具栏 3 个未置灰按钮（复制/粘贴/查找）
 *   + 快捷键 8 个（Ctrl+F、F3、Ctrl+L、Ctrl+Tab、Ctrl+Shift+Tab、Alt+P、终端失焦时的 Ctrl+=/-/0）
 * 长期静默失效，全套门禁全绿。其中 F3（查找下一个）与 Ctrl+Tab（MRU 切换）在全仓找不出
 * 第二个触发点，即功能整体不可达。
 *
 * 所以这里换判据：**只认 `"<id>"` 这个字符串字面量出现在剥掉注释后的代码里**。
 * 承诺方（写注释的人）与验收方（守卫）必须是两个人，否则守卫只是把注释念了一遍。
 * 确实无需分支的项必须进 EXEMPT 并写明理由——豁免要具名、要留痕，不能靠一段散文蒙混。
 *
 * 三个来源都查，因为三者是并列的触发面，任一漏接的后果相同（点了/按了没反应）：
 * menus.ts（菜单栏）、ToolBar.svelte（工具栏按钮）、shortcuts.ts（§4 键→动作表）。
 */

/** 剥注释再找字面量：解释接线的注释里必然写着这些 id，直接扫全文等于让注释给自己作证。 */
function code(src: string): string {
  return src
    .replace(/<!--[\s\S]*?-->/g, "")
    .replace(/\/\*[\s\S]*?\*\//g, "")
    .replace(/(^|[^:])\/\/.*$/gm, "$1"); // [^:] 让 https:// 里的双斜杠不被当成行注释
}

const APP_CODE = code(APP_SRC);

/** 实体分支判据：形如 `id === "edit.find"` / `case "edit.find"` 里的那个带引号字面量。 */
const hasBranch = (id: string): boolean => APP_CODE.includes(`"${id}"`);

/**
 * 豁免表：key = 动作 id，value = 理由（写给下一个改这里的人看，不是写给守卫看）。
 * 加一条豁免就是宣布「这个入口点了不会有实体分支响应」，请确认那确实是想要的。
 */
const EXEMPT: Record<string, string> = {
  "view.theme": "父项自身无动作，点击只展开子菜单；四个子项由 id.startsWith(\"theme.\") 前缀分支消费",
  "view.sidebarDock": "同上：父项只展开子菜单；三个子项由 id.startsWith(\"sidebarDock.\") 前缀分支消费（M4b 第 16 项）",
};

/**
 * 走前缀分支的子项不会有逐项字面量。
 *
 * 判据是**两边都要成立**：id 有那个前缀，**且** App 里确实有对应的
 * `id.startsWith("前缀")`。只判前缀的话，删掉 App 里那行分支之后这些子项会静默
 * 变成「已接线」——而它们全都点了没反应。
 */
const PREFIX_BRANCHES = ["theme.", "sidebarDock."] as const;
const byPrefix = (id: string): boolean =>
  PREFIX_BRANCHES.some((p) => id.startsWith(p) && APP_CODE.includes(`id.startsWith("${p}")`));

function assertWired(entries: Array<{ id: string; where: string }>): void {
  const dead = entries
    .filter((e) => !EXEMPT[e.id] && !byPrefix(e.id) && !hasBranch(e.id))
    .map((e) => `${e.where} → ${e.id}`);
  expect(dead).toEqual([]);
}

describe("动作接线：可触发的入口必须有实体分支（不接受注释台账）", () => {
  it("menus.ts 的每个 enabled 菜单项（含子项）在 onAction 有实体分支", () => {
    const entries: Array<{ id: string; where: string }> = [];
    for (const menu of currentMenus()) {
      for (const item of menu.items) {
        if (!item.enabled) continue;
        entries.push({ id: item.id, where: `菜单 ${menu.id}` });
        for (const child of item.children ?? []) {
          if (child.enabled) entries.push({ id: child.id, where: `菜单 ${menu.id}/${item.id}` });
        }
      }
    }
    // 反向自检：清单本身不能是空的（正则/导入一旦失效，空清单会让本用例假绿）
    expect(entries.length).toBeGreaterThan(20);
    assertWired(entries);
  });

  it("ToolBar 的每个 enabled 按钮在 onAction 有实体分支", () => {
    // 置灰按钮（enabled:false）只显示「即将推出」提示，不派发动作，故只查 enabled:true。
    // 先剥注释：ToolBar 里有一行被注掉的 `// { id: "session.openDir", …, enabled: true }`，
    // 不剥的话它会被当成真按钮，守卫要求给一个用户根本点不到的入口接线。
    const src = code(TOOLBAR_SRC);
    const entries = [...src.matchAll(/\{[^{}]*\bid:\s*"([\w.]+)"[^{}]*\benabled:\s*true[^{}]*\}/g)]
      .map((m) => ({ id: m[1], where: "工具栏" }));
    expect(entries.map((e) => e.id)).toContain("edit.copy"); // 锚：正则失配时不至于空跑成绿
    assertWired(entries);
  });

  it("keymap.ts 默认键位表映射到的每个动作在 onAction 有实体分支", () => {
    // 键表里的 id 未必都是菜单项：view.fontReset(Ctrl+0)、session.properties(Alt+P)、
    // tab.mruNext/Prev(Ctrl+Tab) 都只有快捷键入口，菜单守卫看不见它们。
    //
    // M4a：键位表从 shortcuts.ts 的硬编码 if 链搬到 keymap.ts 的 DEFAULT_BINDINGS
    // （数据驱动，支持全量重绑）。守卫随之改扫那张表——扫源码而非 import 值，
    // 与本文件其余三条守卫同口径（它们要的是「代码里有没有这一行」）。
    // keyboard.toggleMode 不入断言：它走 actionForCombo 的 toggle-mode 分支、
    // 不经 onAction（模式切换是 shortcuts 内部状态，非应用动作）。
    const entries = [...code(KEYMAP_SRC).matchAll(/"[^"]+":\s*"([\w.]+)"/g)]
      .map((m) => ({ id: m[1], where: "快捷键" }))
      .filter((e) => e.id !== "keyboard.toggleMode");
    for (const id of ["view.fontReset", "session.properties", "tab.mruNext"]) {
      expect(entries.map((e) => e.id)).toContain(id);
    }
    assertWired(entries);
  });

  /**
   * 上面三条只保证「有分支」。分支里可以是空的 `return;`——那正是修复前 edit.find 与
   * view.font* 的形态：分支在、体是一句注释加 return，静默无事。故再钉一条：
   * 终端类动作必须真的落到 termApis 上，而 termApis 必须真的被挂载点填充。
   */
  it("终端类动作确实落到活动终端的句柄上，且 onReady 已在挂载点接线", () => {
    // 不钉整句字面量：onReady 里后来还挂了别的（M7.3 的排队命令排空）。钉的是不变量本身——
    // 挂载点的 onReady 必须把这个标签的句柄填进 termApis，参数名与花括号形态随它去。
    expect(APP_CODE).toMatch(/onReady=\{\(api\) =>[\s\S]{0,80}termApis\.set\(tab\.id, api\)/);
    expect(APP_CODE).toMatch(/const activeTermApi = \(\)[^\n]*termApis\.get\(\$activeTabId\)/);
    // 逐项钉消费方，避免「分支在但体是空 return」重演
    expect(APP_CODE).toMatch(/id === "edit\.find"\)[\s\S]{0,40}api\.openSearch\(\)/);
    expect(APP_CODE).toMatch(/api\.findNext\(\)/);
    expect(APP_CODE).toMatch(/id === "edit\.copyPlain"\)[\s\S]{0,60}activeTermApi\(\)\?\.copySelection\(\)/);
    expect(APP_CODE).toMatch(/id === "edit\.paste"\)[\s\S]{0,60}activeTermApi\(\)\?\.pasteFromClipboard\(\)/);
    expect(APP_CODE).toMatch(/id === "edit\.selectAll"\)[\s\S]{0,80}\.selectAll\(\)/);
    expect(APP_CODE).toMatch(/id === "edit\.clearScrollback"\)[\s\S]{0,80}\.clearScrollback\(\)/);
    expect(APP_CODE).toMatch(/ctrl\.resetFontSize\(\)/);
    expect(APP_CODE).toMatch(/ctrl\.zoomFont\(id === "view\.fontGrow" \? 1 : -1\)/);
    // 关标签要清句柄：不清则表只增不减，且 controller() 指着已 dispose 的终端
    expect(APP_CODE).toMatch(/termApis\.delete\(id\)/);
  });

  it("守卫自身有效：不存在的 id 判死，注释里的 id 不算数", () => {
    expect(hasBranch("edit.thisDoesNotExist")).toBe(false);
    expect(hasBranch("edit.find")).toBe(true); // 反向对照
    // 注释里写了带引号的 id 也不该被认作接线——这正是本守卫与 S262 的分界
    expect(code('// 这项由后续 Task 接线：if (id === "edit.ghost")').includes('"edit.ghost"')).toBe(false);
  });

  // ── 审计2 #37：JSON 导入的尺寸闸（前端纵深防御层）────────────────────────────
  it("导入文件在 file.text() 之前量尺寸：超过 8 MiB 直接拒绝", () => {
    // 真正的闸在 Rust（import_json 解析前量尺寸）。前端这一层是「别把超大文件整份读进
    // 内存、推过 IPC 再被后端拒」的纵深防御；删掉它编译照过、功能照常。
    //
    // **只有「顺序」这一条归本守卫**：闸挪到 `await file.text()` 之后时，行为测试
    // （lib/profile-io.test.ts 的「超过 8 MiB 当场挡下」）**仍然全绿**——它断言的是没发 IPC，
    // 而 invoke 本来就排在读文件之后。内存峰值已经打出来了，只有源码顺序看得见。
    //
    // 2026-09-03：导入逻辑从 App.svelte 搬到 lib/profile-io.ts，本守卫随之改读那个文件。
    // 这次「搬家 → 守卫失效」正是路线图 S262 记着的那件事，也是这次拆分先建行为安全网
    // （src/App.test.ts）才动手的理由。
    const IO_CODE = code(PROFILE_IO_SRC);
    const gate = IO_CODE.match(/file\.size > IMPORT_MAX_BYTES/g) ?? [];
    expect(gate.length).toBe(1);
    expect(IO_CODE, "上限常量必须就是 8 MiB").toMatch(/IMPORT_MAX_BYTES = 8 \* 1024 \* 1024/);
    const gatePos = IO_CODE.indexOf("file.size > IMPORT_MAX_BYTES");
    const readPos = IO_CODE.indexOf("await file.text()");
    expect(gatePos).toBeGreaterThan(-1);
    expect(readPos).toBeGreaterThan(-1);
    expect(gatePos).toBeLessThan(readPos); // 顺序颠倒 = 闸白设，内存峰值已打出来
    const between = IO_CODE.slice(gatePos, readPos);
    expect(between).toContain("return"); // 拒绝后必须直接返回，不得继续读文件
  });
});

/**
 * 审计2 #18/#35：App → TerminalPane 的档案值贯通。
 *
 * `sftp.local_dir`/`sftp.remote_dir`/`term.scrollback_lines` 曾是「可编辑可保存、但没有任何
 * 消费方」的死配置——修复链的最后一环就是 App 把档案字段接进 TerminalPane 的 props。
 * 这一环没有行为可断言（App 的 TerminalPane 在测试里不真挂载），故按本文件既有口径取
 * 源码守卫：三行接线都在、且接的就是档案字段本身。删掉任一接线 = 死配置复活，只有这里红。
 */
describe("审计2 #18/#35：App 把档案默认目录与滚动行数接进 TerminalPane", () => {
  it("sftpLocalDir/sftpRemoteDir/scrollbackLines 三行接线都在且接自档案字段", () => {
    expect(APP_CODE).toContain("sftpLocalDir={prof?.sftp?.local_dir ?? null}");
    expect(APP_CODE).toContain("sftpRemoteDir={prof?.sftp?.remote_dir ?? null}");
    expect(APP_CODE).toContain("scrollbackLines={prof?.term?.scrollback_lines ?? null}");
  });
});
