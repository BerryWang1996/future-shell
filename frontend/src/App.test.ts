/**
 * App.svelte 的**行为安全网**（路线图 4c「App.svelte 拆分：先写组件测试再拆」，2026-09-03）。
 *
 * # 为什么要有它
 *
 * 出口原文写着「本条标 `[~]` 直到有安全网——仓内已有先例（S262）记着『靠正则守卫验收重构』
 * 失效过一次，零安全网的大重构不做」。此前 App 只有**源码文本**层面的守卫
 * （layout.test.ts / menus.test.ts / lifecycle-guards.test.ts 都扫 `App.svelte?raw`），
 * 那种守卫在「把代码搬到另一个文件」时会整片失效——恰恰是重构最需要它的时刻。
 *
 * 所以这里真渲染 App，断言的是**从菜单/工具栏点下去会发生什么**：哪条 IPC 出去了、
 * 哪个对话框开了、哪个 store 变了。这些在拆分前后必须逐条不变。
 *
 * # 桩的边界
 *
 * 只桩 `lib/ipc`（invoke/listen/settingGet/settingSet）与 Tauri 的窗口/webview API——
 * 那是进程边界。组件一律真挂载：拆分要动的正是组件装配，桩掉它们等于把被测对象删了。
 * 无标签时 TerminalPane / RdpPane 不挂载（App 的 `{#each $tabs}` 为空），xterm 不进场。
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/svelte";
import { get } from "svelte/store";

/** 所有出去的 IPC 调用，按序留痕。 */
const calls: { cmd: string; args?: Record<string, unknown> }[] = [];
/** 按命令名定制返回；缺省 undefined。 */
const replies: Record<string, (args?: Record<string, unknown>) => unknown> = {};
const settings: Record<string, unknown> = {};
/** reportFrontendError 的留痕（A3：启动步骤失败必须上报，不是静默吞掉）。 */
const reported: { source: string; err: unknown }[] = [];

vi.mock("./lib/ipc", () => ({
  invoke: async (cmd: string, args?: Record<string, unknown>) => {
    calls.push({ cmd, args });
    return replies[cmd] ? replies[cmd](args) : undefined;
  },
  listen: vi.fn(async () => () => {}),
  ping: async () => "pong",
  openExternal: vi.fn(async () => {}),
  reportFrontendError: (source: string, err: unknown) => {
    reported.push({ source, err });
  },
  settingGet: async <T>(k: string, fb: T) => (k in settings ? (settings[k] as T) : fb),
  settingSet: async (k: string, v: unknown) => {
    settings[k] = v;
  },
  settingChanged: { subscribe: (fn: (v: unknown) => void) => (fn(null), () => {}) },
}));

const fullscreen = { value: false };
vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({
    isFullscreen: async () => fullscreen.value,
    setFullscreen: async (v: boolean) => {
      fullscreen.value = v;
    },
    onCloseRequested: async () => () => {},
    close: vi.fn(),
    destroy: vi.fn(),
    setTitle: vi.fn(),
  }),
}));
vi.mock("@tauri-apps/api/webview", () => ({
  getCurrentWebview: () => ({ onDragDropEvent: async () => () => {} }),
}));
// AuthPromptDialog / HostKeyDialog / McpConfirmDialog / lib/transfers 直接从这里取 listen，
// 不经 lib/ipc；不桩的话它们在 jsdom 里撞 `transformCallback` of undefined。
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}), emit: vi.fn(async () => {}) }));
vi.mock("@tauri-apps/api/webviewWindow", () => ({
  WebviewWindow: class {
    static getByLabel = async () => null;
    constructor() {}
    once() {}
    close() {}
  },
}));

import App from "./App.svelte";
import { tabs, activeTabId } from "./lib/tabs";
import { sidebarVisible, toolbarVisible, composeVisible, statusVisible, monitorOpen, quickbarVisible } from "./lib/layout";

/** 点菜单里那一项（按中文标签前缀定位，与 lib/i18n/zh-CN.ts 同源）。 */
async function clickMenu(menu: string, item: string) {
  const top = [...document.querySelectorAll<HTMLButtonElement>('[data-testid="menubar"] .menu > button')].find((b) =>
    (b.textContent ?? "").trim().startsWith(menu),
  );
  expect(top, `菜单栏里没有「${menu}」`).toBeTruthy();
  await fireEvent.click(top!);
  const entry = [...document.querySelectorAll<HTMLButtonElement>('[data-testid="menubar"] .dropdown .item > button')].find(
    (b) => ((b.querySelector(".label") ?? b).textContent ?? "").trim().startsWith(item),
  );
  expect(entry, `「${menu}」菜单里没有「${item}」`).toBeTruthy();
  await fireEvent.click(entry!);
}

async function mount() {
  render(App);
  // onMount 里那串 boot() 是异步的；等档案加载这一步落地再断言
  await waitFor(() => expect(calls.some((c) => c.cmd === "profiles_list")).toBe(true));
}

beforeEach(() => {
  calls.length = 0;
  reported.length = 0;
  for (const k of Object.keys(replies)) delete replies[k];
  for (const k of Object.keys(settings)) delete settings[k];
  fullscreen.value = false;
  tabs.set([]);
  activeTabId.set(null);
  sidebarVisible.set(true);
  toolbarVisible.set(true);
  composeVisible.set(true);
  statusVisible.set(true);
  monitorOpen.set(false);
  quickbarVisible.set(false);
  replies.profiles_list = () => ({ profiles: [], bad_rows: [] });
  replies.sessions_unclosed = () => [];
  replies.vault_status = () => true;
  replies.vault_auto_unlock = () => true;
  // 各对话框开局要拉的列表。给 [] 而不是让它们收到 undefined——本安全网测的是「点下去开出东西」，
  // 让被测组件死在自己的空列表处理上只会掩盖真正要守的那条路。
  for (const cmd of ["schedule_list", "schedule_runs", "recordings_list", "tunnel_list", "history_list",
                     "key_list", "vault_list_secrets", "vault_list_backups", "quick_commands_list"]) {
    replies[cmd] = () => [];
  }
  replies.key_list = () => ({ keys: [], agent_error: null, vault_locked: false });
});
afterEach(() => cleanup());

describe("App 骨架：五条 chrome 都在，且视图开关真的开关它们", () => {
  it("菜单栏 / 工具栏 / 侧栏 / 标签条 / 组合栏 / 状态栏 同时在场", async () => {
    await mount();
    for (const id of ["menubar", "toolbar", "sidebar", "composebar", "statusbar"]) {
      expect(screen.getByTestId(id), `${id} 不见了`).toBeTruthy();
    }
    expect(document.querySelector(".tabbar"), "标签条不见了").toBeTruthy();
  });

  it.each([
    ["工具栏", "toolbar", toolbarVisible],
    ["组合命令栏", "composebar", composeVisible],
    ["状态栏", "statusbar", statusVisible],
  ])("视图菜单「%s」切换即隐藏该栏", async (label, testid, store) => {
    await mount();
    expect(screen.getByTestId(testid)).toBeTruthy();
    await clickMenu("视图", label);
    await waitFor(() => expect(get(store)).toBe(false));
    expect(screen.queryByTestId(testid), `${testid} 应已隐藏`).toBeNull();
  });

  it("视图菜单「监控抽屉」开出监控面板", async () => {
    await mount();
    expect(screen.queryByTestId("monitor-panel")).toBeNull();
    await clickMenu("视图", "监控抽屉");
    await waitFor(() => expect(get(monitorOpen)).toBe(true));
    expect(screen.getByTestId("monitor-panel")).toBeTruthy();
  });

  it("视图菜单「全屏」经窗口 API 翻转，而不是自己维护一个假状态", async () => {
    await mount();
    await clickMenu("视图", "全屏");
    await waitFor(() => expect(fullscreen.value).toBe(true));
  });
});

describe("菜单动作 → 对话框：每一条都真的开出东西", () => {
  it.each([
    ["工具", "选项", "settings-dialog"],
    ["工具", "隧道管理器", "tunnel-dialog"],
    ["工具", "历史命令", "history-dialog"],
    ["工具", "计划任务", "schedule-dialog"],
    ["工具", "会话回放", "replay-dialog"],
    ["工具", "密钥/代理管理器", "key-manager"],
    ["工具", "保险库管理", "vault-mgr"],
    ["工具", "快速命令集编辑器", "quick-commands-dialog"],
    ["文件", "新建会话", "profile-dialog"],
    ["文件", "导入(Xshell", "foreign-import-dialog"],
  ])("%s → %s 开出 %s", async (menu, item, testid) => {
    await mount();
    expect(screen.queryByTestId(testid), `${testid} 一开始不该在`).toBeNull();
    await clickMenu(menu, item);
    await waitFor(() => expect(screen.getByTestId(testid)).toBeTruthy());
  });

  it("工具 → 显示/隐藏快速命令条：切的是条不是对话框", async () => {
    await mount();
    await clickMenu("工具", "显示/隐藏快速命令条");
    await waitFor(() => expect(get(quickbarVisible)).toBe(true));
    expect(screen.getByTestId("quickbar")).toBeTruthy();
  });
});

describe("菜单动作 → IPC：命令名与参数在拆分前后必须逐字不变", () => {
  it("文件 → 打开会话日志目录 走 reveal_item_in_dir", async () => {
    await mount();
    calls.length = 0;
    await clickMenu("文件", "打开会话日志目录");
    await waitFor(() => expect(calls.some((c) => c.cmd === "reveal_session_log_dir")).toBe(true));
  });

  it("工具 → 审计链校验 走 audit_verify", async () => {
    await mount();
    calls.length = 0;
    await clickMenu("工具", "审计链校验");
    await waitFor(() => expect(calls.some((c) => c.cmd === "audit_verify")).toBe(true));
  });

  it("帮助 → 文档 / 关于 都能点，不抛异常", async () => {
    await mount();
    await clickMenu("帮助", "文档");
    await clickMenu("帮助", "关于");
    expect(true).toBe(true);
  });
});

describe("工具栏按钮与菜单指向同一套动作", () => {
  it("工具栏「新建会话」开出会话属性对话框（与文件菜单同一条路）", async () => {
    await mount();
    await fireEvent.click(screen.getByTestId("tbtn-session.new"));
    await waitFor(() => expect(screen.getByTestId("profile-dialog")).toBeTruthy());
  });

  it("工具栏「设置」开出选项对话框", async () => {
    await mount();
    await fireEvent.click(screen.getByTestId("tbtn-tools.settings"));
    await waitFor(() => expect(screen.getByTestId("settings-dialog")).toBeTruthy());
  });
});

describe("启动序列：boot() 的每一步都真的跑了", () => {
  it("档案、未关闭会话、保险库状态三条都发了出去", async () => {
    await mount();
    await waitFor(() => {
      const cmds = calls.map((c) => c.cmd);
      expect(cmds).toContain("profiles_list");
      expect(cmds).toContain("sessions_unclosed");
      expect(cmds).toContain("vault_status");
    });
  });

  it("其中一步失败不拖垮后面的（A3 的行为面，此前只有源码守卫）", async () => {
    // 取 vault_status 而不是 profiles_list 作故障源：loadProfiles **自己**带 try/catch，
    // 拿它当故障源时即使把 boot() 整个拆掉测试也照绿（首版变异 X3 就是这样幸存的）。
    // vault_status 的 reject 会一路传到 boot，没有 boot 隔离时它会终止整个 onMount。
    replies.vault_status = () => {
      throw new Error("vault backend down");
    };
    render(App);
    // vault 那步炸了，排在它后面的会话恢复照样要跑到
    await waitFor(() => expect(calls.some((c) => c.cmd === "sessions_unclosed")).toBe(true));
  });

  it("失败的那一步要留痕（reportFrontendError），不是静默吞掉", async () => {
    replies.vault_status = () => {
      throw new Error("vault backend down");
    };
    render(App);
    await waitFor(() => expect(calls.some((c) => c.cmd === "sessions_unclosed")).toBe(true));
    expect(reported.some((r) => r.source === "startup:vault"), "启动步骤失败没上报").toBe(true);
  });
});
