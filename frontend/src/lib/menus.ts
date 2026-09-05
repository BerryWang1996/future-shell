export interface MenuItem {
  id: string;
  label: string;
  shortcut?: string;
  enabled: boolean;
  /** 禁用原因，如「即将推出（M4a）」；相位一律细化到 4a/4b，不写裸「Phase 4 / M4」（R42-L1） */
  note?: string;
  children?: MenuItem[];
}
export interface Menu { id: string; label: string; items: MenuItem[] }

/**
 * 菜单的**结构**（id / 快捷键 / 启用与否 / 层级）。
 *
 * 这里刻意没有 `label`：文案由 `menu.<id>` 从词典取（M4b i18n）。
 * 分成两半的好处不只是能切语言——`i18n.test.ts` 有一条守卫扫本文件的源码，
 * **不允许出现中文字面量**，于是「菜单里加了一条却忘了加词条」是必红的，
 * 而不是等到有人把界面切成英文才发现那一条还是中文。
 *
 * `note` 存的是里程碑代号（`M4b` / `Phase 2`），不是整句话——整句由
 * `menu.soon` 带 `{phase}` 插值拼出。代号本身不翻译：它对应文档里的标识，
 * 翻成「第二阶段」反而对不上任何一处。
 */
interface MenuItemSpec {
  id: string;
  shortcut?: string;
  enabled: boolean;
  /** 里程碑代号；有值 ⇒ note = t("menu.soon", { phase }) */
  soon?: string;
  children?: MenuItemSpec[];
  /** 文案不走词典、直接用这个值（主题名等专名，见 view.theme 的子项）。 */
  literalLabel?: string;
}
interface MenuSpec { id: string; items: MenuItemSpec[] }

import { derived, get, type Readable } from "svelte/store";
import { BUILTIN_THEMES } from "./theme/themes";
import { t } from "./i18n";

const SPEC: MenuSpec[] = [
  {
    id: "file",
    items: [
      { id: "session.new", shortcut: "Ctrl+N", enabled: true },
      { id: "folder.new", enabled: true },
      { id: "session.openDir", enabled: true },
      { id: "session.openRecordings", enabled: true },
      { id: "import.json", enabled: true },
      { id: "import.xshell", enabled: true },
      { id: "export", enabled: true },
      { id: "app.exit", enabled: true },
    ],
  },
  {
    id: "edit",
    items: [
      { id: "edit.copy", shortcut: "Ctrl+Shift+C", enabled: true },
      { id: "edit.paste", shortcut: "Ctrl+Shift+V", enabled: true },
      { id: "edit.copyPlain", enabled: true },
      { id: "edit.selectAll", enabled: true },
      { id: "edit.find", shortcut: "Ctrl+F", enabled: true },
      { id: "edit.findNext", shortcut: "F3", enabled: true },
      { id: "edit.clearScreen", shortcut: "Ctrl+L", enabled: true },
      { id: "edit.clearScrollback", enabled: true },
      { id: "edit.screenshotCopy", enabled: true },
      { id: "edit.screenshotSave", enabled: true },
    ],
  },
  {
    id: "view",
    items: [
      { id: "view.sidebar", shortcut: "Ctrl+Shift+S", enabled: true },
      {
        id: "view.sidebarDock",
        enabled: true,
        children: [
          { id: "sidebarDock.left", enabled: true },
          { id: "sidebarDock.right", enabled: true },
          { id: "sidebarDock.float", enabled: true },
        ],
      },
      { id: "view.monitor", shortcut: "Ctrl+Shift+M", enabled: true },
      { id: "view.compose", enabled: true },
      { id: "view.status", enabled: true },
      { id: "view.toolbar", enabled: true },
      { id: "view.fullscreen", shortcut: "F11", enabled: true },
      { id: "view.fontGrow", shortcut: "Ctrl+=", enabled: true },
      { id: "view.fontShrink", shortcut: "Ctrl+-", enabled: true },
      {
        id: "view.theme",
        enabled: true,
        children: [
          { id: "theme.auto", enabled: true },
          // 主题名是专名（Obsidian / Daylight / Slate Blue / High Contrast），不进词典：
          // 翻译一个产品自己起的名字只会让人对不上文档与截图。
          ...BUILTIN_THEMES.map((th) => ({
            id: `theme.${th.id}`,
            enabled: true,
            literalLabel: th.name,
          })),
        ],
      },
    ],
  },
  {
    id: "tools",
    items: [
      { id: "tools.settings", shortcut: "Ctrl+,", enabled: true },
      { id: "tools.vaultToggle", enabled: true },
      // 删除记录、设置/修改应用密码、备份与恢复（审计2 #27/#28）。core 侧的 delete 与
      // change_passphrase 早就写好了，缺的正是这个用户到得了的入口——没有入口就等于没有功能。
      { id: "tools.vaultManage", enabled: true },
      { id: "tools.quickCommandsEdit", enabled: true },
      { id: "tools.quickCommands", enabled: true },
      { id: "tools.highlight", enabled: true },
      { id: "tools.tunnels", enabled: true },
      { id: "tools.history", shortcut: "Ctrl+Shift+H", enabled: true },
      { id: "tools.schedule", enabled: true },
      { id: "tools.replay", enabled: true },
      { id: "tools.schemeEditor", enabled: true },
      { id: "tools.keyManager", enabled: true },
      { id: "tools.ai", shortcut: "Ctrl+Shift+A", enabled: true },
      // 审计链校验（M3 第 7 项）：hash 链的 verify_chain 一直只是库函数——
      // 有实现、有单测、没有任何用户到得了的入口。这条菜单项就是那个入口。
      { id: "tools.auditVerify", enabled: true },
      // M3：Agent（自主排查）。与 AI 助手分开——那个是一问一答，
      // 这个是「派任务」，两者的风险面完全不同（前者不执行，后者会执行）。
      { id: "tools.agent", enabled: true },
    ],
  },
  {
    id: "window",
    items: [
      { id: "window.new", enabled: true },
      { id: "window.cascade", enabled: true },
      { id: "window.tileH", enabled: true },
      { id: "window.tileV", enabled: true },
      { id: "window.closeAll", enabled: true },
      { id: "window.sessionList", enabled: false, soon: "M4b" },
    ],
  },
  {
    id: "help",
    items: [
      { id: "help.docs", enabled: true },
      { id: "help.keys", enabled: true },
      { id: "help.logs", enabled: true },
      { id: "help.about", enabled: true },
    ],
  },
];

type T = (key: string, vars?: Record<string, string | number>) => string;

function buildItem(spec: MenuItemSpec, tr: T): MenuItem {
  const item: MenuItem = {
    id: spec.id,
    label: spec.literalLabel ?? tr(`menu.${spec.id}`),
    enabled: spec.enabled,
  };
  if (spec.shortcut) item.shortcut = spec.shortcut;
  if (spec.soon) item.note = tr("menu.soon", { phase: spec.soon });
  if (spec.children) item.children = spec.children.map((c) => buildItem(c, tr));
  return item;
}

/** 按当前语言构建整棵菜单树。语言一变，订阅方自动重渲。 */
export const menus: Readable<Menu[]> = derived(t, ($t) =>
  SPEC.map((m) => ({
    id: m.id,
    label: $t(`menu.${m.id}`),
    items: m.items.map((i) => buildItem(i, $t)),
  })),
);

/**
 * 当前语言下的菜单快照。
 *
 * 给非响应式场合用（纯函数、测试、以及 action-wiring 这类只关心 id 与 enabled 的扫查）。
 *
 * 刻意**不叫** `MENUS`：旧名字是个常量，读起来像「菜单就是这样」；而现在它是一次取值，
 * 结果取决于当次的语言。沿用旧名会让调用方以为可以在模块顶层求一次值存起来——
 * 那样切了语言之后它就是陈旧的，且没有任何东西会提示。
 */
export function currentMenus(): Menu[] {
  return get(menus);
}
