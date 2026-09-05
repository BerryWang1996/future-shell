/**
 * English dictionary.
 *
 * Key set must match `zh-CN.ts` exactly — `i18n.test.ts` asserts both directions.
 * A missing key here falls back to Chinese (see `DEFAULT_LOCALE`), which shows up as
 * one stray Chinese string rather than a broken UI; the guard exists so that
 * "one stray string" never ships in the first place.
 *
 * The `(F)`-style access keys are kept: they match the Windows menu convention on
 * both sides, and the letters are the same ones the Chinese menu uses — changing
 * them per-language would silently break anyone's muscle memory after a switch.
 */
export const EN: Record<string, string> = {
  // ── Menu bar: top level ──
  "menu.file": "File(F)",
  "menu.edit": "Edit(E)",
  "menu.view": "View(V)",
  "menu.tools": "Tools(T)",
  "menu.window": "Window(W)",
  "menu.help": "Help(H)",

  // ── File ──
  "menu.session.new": "New Session(N)",
  "menu.folder.new": "New Folder…",
  "menu.session.openDir": "Open Session Log Folder…",
  "menu.session.openRecordings": "Open Recordings Folder…",
  "menu.import.json": "Import (JSON)",
  "menu.import.xshell": "Import (Xshell/FinalShell/iTerm2)…",
  "menu.export": "Export",
  "menu.app.exit": "Exit",

  // ── Edit ──
  "menu.edit.copy": "Copy",
  "menu.edit.paste": "Paste",
  "menu.edit.copyPlain": "Copy as Plain Text",
  "menu.edit.selectAll": "Select All",
  "menu.edit.find": "Find",
  "menu.edit.findNext": "Find Next",
  "menu.edit.clearScreen": "Clear Screen",
  "menu.edit.clearScrollback": "Clear Scrollback",
  "menu.edit.screenshotCopy": "Screenshot to Clipboard",
  "menu.edit.screenshotSave": "Save Screenshot as PNG…",

  // ── View ──
  "menu.view.sidebar": "Session Manager",
  "menu.view.sidebarDock": "Session Manager Position",
  "menu.sidebarDock.left": "Dock Left",
  "menu.sidebarDock.right": "Dock Right",
  "menu.sidebarDock.float": "Floating (overlay)",
  "menu.view.monitor": "Monitor Drawer",
  "menu.view.compose": "Compose Bar",
  "menu.view.status": "Status Bar",
  "menu.view.toolbar": "Toolbar",
  "menu.view.fullscreen": "Full Screen",
  "menu.view.fontGrow": "Increase Font Size",
  "menu.view.fontShrink": "Decrease Font Size",
  "menu.view.theme": "Application Theme",
  "menu.theme.auto": "Follow System (auto)",

  // ── Tools ──
  "menu.tools.settings": "Options (Settings)",
  "menu.tools.vaultToggle": "Unlock/Lock Vault",
  "menu.tools.vaultManage": "Vault Manager…",
  "menu.tools.quickCommandsEdit": "Quick Command Editor…",
  "menu.tools.quickCommands": "Show/Hide Quick Command Bar",
  "menu.tools.highlight": "Highlight Keywords…",
  "menu.tools.tunnels": "Tunnel Manager…",
  "menu.tools.history": "Command History…",
  "menu.tools.schedule": "Scheduled Tasks…",
  "menu.tools.replay": "Session Replay…",
  "menu.tools.schemeEditor": "Color Scheme Editor",
  "menu.tools.keyManager": "Key / Agent Manager",
  "menu.tools.ai": "AI Assistant",
  "menu.tools.auditVerify": "Audit Chain Verify",
  "menu.tools.agent": "AI Agent (autonomous)",

  // ── Window ──
  "menu.window.new": "New Window",
  "menu.window.cascade": "Cascade",
  "menu.window.tileH": "Tile Horizontally (side by side)",
  "menu.window.tileV": "Tile Vertically (stacked)",
  "menu.window.closeAll": "Close All Tabs",
  "menu.window.sessionList": "Session List…",

  // ── Help ──
  "menu.help.docs": "Documentation",
  "menu.help.keys": "Keyboard Shortcuts",
  "menu.help.logs": "Diagnostic Log Folder",
  "menu.help.about": "About",

  /** `{phase}` is a milestone code (M4b / Phase 2) and is deliberately NOT translated. */
  "menu.soon": "Coming in {phase}",

  // ── Settings: language ──
  "settings.language": "Language",
  "settings.language.hint": "The menu bar switches immediately, without restarting. Other screens currently use Simplified Chinese.",
};
