import { describe, expect, it } from "vitest";
// S262 守卫取 App.svelte 源文本：走 vite 的 ?raw 导入而非 node:fs——tsconfig 只装 vite/client 类型、
// 无 @types/node，且 vitest 模块运行器下 import.meta.url 非 file: 方案，readFileSync(new URL(…)) 会抛。
import APP_SRC from "../App.svelte?raw";
import { currentMenus } from "./menus";

describe("菜单栏数据（UI 规格 §2.1）", () => {
  const item = (menu: string, id: string) =>
    currentMenus().find((m) => m.id === menu)!.items.find((i) => i.id === id)!;

  it("六菜单齐全：文件/编辑/视图/工具/窗口/帮助", () => {
    expect(currentMenus().map((m) => m.id)).toEqual(["file", "edit", "view", "tools", "window", "help"]);
  });
  it("MVP 必含项可用", () => {
    for (const [menu, id] of [
      ["file", "session.new"], ["file", "import.json"], ["file", "app.exit"],
      ["edit", "edit.copy"], ["edit", "edit.paste"], ["edit", "edit.find"],
      ["view", "view.sidebar"], ["view", "view.monitor"], ["view", "view.fullscreen"], ["view", "view.theme"],
      ["view", "view.fontGrow"], ["view", "view.fontShrink"],
      ["tools", "tools.settings"], ["tools", "tools.vaultToggle"],
    ] as const) {
      expect(item(menu, id).enabled, `${menu}/${id}`).toBe(true);
    }
  });
  it("Phase 项禁用并标注", () => {
    // M2 已实装 AI 面板（components/AiPanel.svelte + app 侧 ai_* 命令），故与下面几项同理
    // 改钉 enabled 而非 note。这一条曾是本用例里唯一的 "Phase 2"，
    // 于是它红过一次——一个功能做完了却忘记解禁入口，本仓已经发生过四次
    //（import.xshell / tools.schemeEditor / tools.keyManager / 工具栏截图按钮），
    // 所以这条断言的价值不在「AI 现在可用」，而在「下一次遗漏也会红」。
    expect(item("tools", "tools.ai").enabled).toBe(true);
    expect(item("tools", "tools.ai").note).toBeUndefined();

    // M4a 已实装（Task 66 快速命令集/片段库、Task 69+70 会话日志目录）：这两项
    // 从「即将推出」转为可用，故此处改钉 enabled 而非 note。剩下仍标 M4a 的
    // tools.highlight（高亮关键字）是本里程碑尚未做的项。
    expect(item("tools", "tools.quickCommands").enabled).toBe(true);
    // Task 71 已实装（规则编辑器在选项页「终端外观」，菜单项直达该页）
    expect(item("tools", "tools.highlight").enabled).toBe(true);
    // M4b 已实装（Xshell .xsh/.xcs、FinalShell JSON、iTerm2 .itermcolors 三路 + 导入对话框），
    // 故此处与 quickCommands 同理改钉 enabled 而非 note。
    expect(item("file", "import.xshell").enabled).toBe(true);
    expect(item("file", "import.xshell").note).toBeUndefined();
    expect(item("file", "session.openDir").enabled).toBe(true);
    // M4b 已实装：配色编辑器（设置页「终端外观」，菜单项直达）与密钥/代理管理器。
    // 仍禁用的只剩 window.sessionList（M4b 未做）。
    expect(item("tools", "tools.schemeEditor").enabled).toBe(true);
    expect(item("tools", "tools.keyManager").enabled).toBe(true);
    // M4b 已实装多窗口：新建/层叠/水平平铺/垂直平铺四项转为可用，
    // 剩下两项（关闭全部标签、会话列表）仍是本里程碑未做的。
    for (const id of ["window.new", "window.cascade", "window.tileH", "window.tileV"]) {
      expect(item("window", id).enabled, id).toBe(true);
      expect(item("window", id).note, id).toBeUndefined();
    }
    // window.closeAll 随「关闭全部标签」实装（M4b）；只剩会话列表还没做。
    expect(item("window", "window.closeAll").enabled).toBe(true);
    expect(item("window", "window.sessionList").note).toContain("M4b");
  });
  it("应用主题子菜单含 auto + 4 内置主题", () => {
    const themes = item("view", "view.theme").children!;
    expect(themes.map((t) => t.id)).toEqual([
      "theme.auto", "theme.obsidian", "theme.daylight", "theme.slate", "theme.hc",
    ]);
  });

  /**
   * S262 台账守卫（Task 17 第二轮复审）：启用态菜单项一旦既无 onAction 分支、又不在承接注释台账里，
   * 点击就是静默无事且无人认领——edit.copy/paste/copyPlain/selectAll/clearScrollback 与 import.json
   * 六项正是这样漏掉的（前五项系人工复审所得，import.json 由本守卫首跑当场揪出）。
   * 判据：id 以整词形式出现在 App.svelte 即算「有分支或有承接记录」，二者皆可——本用例防的是**零记录**，
   * 不试图区分实体与台账（区分需语义分析，且台账本身就是合法的承接形态）。
   * 整词匹配（前后不接 \w 或 .）而非裸 includes：否则 `export` 这类短 id 会被无关文本蒙混过关。
   */
  describe("S262：启用项承接台账不得留空白", () => {
    /** 结构性豁免：父项自身无动作，点击只展开子菜单（S260），子项经 id.startsWith("theme.") 前缀分支消费 */
    const EXEMPT = new Set(["view.theme"]);
    const mentions = (id: string) =>
      new RegExp(`(^|[^\\w.])${id.replace(/\./g, "\\.")}([^\\w.]|$)`).test(APP_SRC);

    it("每个 enabled 菜单项（含子项）都在 App.svelte 有分支或承接记录", () => {
      const orphans: string[] = [];
      for (const menu of currentMenus()) {
        for (const it0 of menu.items) {
          if (!it0.enabled) continue;
          if (!EXEMPT.has(it0.id) && !mentions(it0.id)) orphans.push(`${menu.id}/${it0.id}`);
          for (const child of it0.children ?? []) {
            if (!child.enabled) continue;
            const prefix = `${child.id.split(".")[0]}.`; // 前缀分支（如 id.startsWith("theme.")）亦算已承接
            if (!mentions(child.id) && !APP_SRC.includes(`"${prefix}`)) orphans.push(`${menu.id}/${it0.id}/${child.id}`);
          }
        }
      }
      expect(orphans).toEqual([]);
    });

    it("守卫用例本身有效：未提及的 id 判孤儿，且整词匹配不被子串蒙混", () => {
      expect(mentions("edit.thisIdDoesNotExist")).toBe(false);
      expect(mentions("session.ne")).toBe(false); // session.new 的真子串不得算数
      expect(mentions("session.new")).toBe(true); // 反向对照：真 id 确能匹配到
    });
  });
});
