/**
 * 简体中文词典（回落语言，见 `index.ts` 的 `DEFAULT_LOCALE`）。
 *
 * 键名规约：`menu.<菜单或条目 id>`。条目 id 就是 `MENUS` 里那个 id，一一对应——
 * 于是「菜单里加了一条却忘了加词条」在 `i18n.test.ts` 里是必红的。
 *
 * 括号里的助记字母（`文件(F)`）保留：它是 Windows 菜单的惯例，英文侧同样保留。
 */
export const ZH_CN: Record<string, string> = {
  // ── 菜单栏：顶层 ──
  "menu.file": "文件(F)",
  "menu.edit": "编辑(E)",
  "menu.view": "视图(V)",
  "menu.tools": "工具(T)",
  "menu.window": "窗口(W)",
  "menu.help": "帮助(H)",

  // ── 文件 ──
  "menu.session.new": "新建会话(N)",
  "menu.folder.new": "新建文件夹…",
  "menu.session.openDir": "打开会话日志目录…",
  "menu.session.openRecordings": "打开终端录制目录…",
  "menu.import.json": "导入(JSON)",
  "menu.import.xshell": "导入(Xshell/FinalShell/iTerm2 格式)…",
  "menu.export": "导出",
  "menu.app.exit": "退出",

  // ── 编辑 ──
  "menu.edit.copy": "复制",
  "menu.edit.paste": "粘贴",
  "menu.edit.copyPlain": "复制为纯文本",
  "menu.edit.selectAll": "全选",
  "menu.edit.find": "查找",
  "menu.edit.findNext": "查找下一个",
  "menu.edit.clearScreen": "清除屏幕",
  "menu.edit.clearScrollback": "清除滚动缓冲",
  "menu.edit.screenshotCopy": "截图到剪贴板",
  "menu.edit.screenshotSave": "截图存为 PNG…",

  // ── 视图 ──
  "menu.view.sidebar": "会话管理器",
  "menu.view.sidebarDock": "会话管理器位置",
  "menu.sidebarDock.left": "停靠在左侧",
  "menu.sidebarDock.right": "停靠在右侧",
  "menu.sidebarDock.float": "浮动（覆盖层）",
  "menu.view.monitor": "监控抽屉",
  "menu.view.compose": "组合命令栏",
  "menu.view.status": "状态栏",
  "menu.view.toolbar": "工具栏",
  "menu.view.fullscreen": "全屏",
  "menu.view.fontGrow": "字体放大",
  "menu.view.fontShrink": "字体缩小",
  "menu.view.theme": "应用主题",
  "menu.theme.auto": "跟随系统(auto)",

  // ── 工具 ──
  "menu.tools.settings": "选项(设置)",
  "menu.tools.vaultToggle": "Vault 解锁/锁定",
  "menu.tools.vaultManage": "保险库管理…",
  "menu.tools.quickCommandsEdit": "快速命令集编辑器…",
  "menu.tools.quickCommands": "显示/隐藏快速命令条",
  "menu.tools.highlight": "高亮关键字…",
  "menu.tools.tunnels": "隧道管理器…",
  "menu.tools.history": "历史命令…",
  "menu.tools.schedule": "计划任务…",
  "menu.tools.replay": "会话回放…",
  "menu.tools.schemeEditor": "配色方案编辑器",
  "menu.tools.keyManager": "密钥/代理管理器",
  "menu.tools.ai": "AI 助手",
  "menu.tools.auditVerify": "审计链校验",
  "menu.tools.agent": "AI Agent（自主排查）",

  // ── 窗口 ──
  "menu.window.new": "新建窗口",
  "menu.window.cascade": "层叠",
  "menu.window.tileH": "水平平铺（左右并排）",
  "menu.window.tileV": "垂直平铺（上下堆叠）",
  "menu.window.closeAll": "关闭全部标签",
  "menu.window.sessionList": "会话列表…",

  // ── 帮助 ──
  "menu.help.docs": "文档",
  "menu.help.keys": "快捷键一览",
  "menu.help.logs": "诊断日志目录",
  "menu.help.about": "关于",

  /**
   * 禁用条目的说明。`{phase}` 是里程碑代号（M4b / Phase 2），**不翻译**——
   * 它是这个项目内部的标识，翻成「第二阶段」反而对不上文档里的任何一处。
   */
  "menu.soon": "即将推出（{phase}）",

  // ── 设置：语言 ──
  "settings.language": "语言",
  "settings.language.hint": "切换后菜单栏立即使用新语言，无需重启。其他界面目前使用简体中文。",
};
