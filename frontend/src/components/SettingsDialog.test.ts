import { describe, it, expect, beforeEach, vi } from "vitest";
import { render, screen, fireEvent, waitFor, cleanup } from "@testing-library/svelte";
import { tick } from "svelte";
import { get } from "svelte/store";
import { locale } from "../lib/i18n";
// 组件源码原文。走 Vite 的 `?raw` 而非 node:fs：vitest 转换后的 `import.meta.url`
// 不是 file:// URL（fileURLToPath 会抛 "The URL must be of scheme file"），
// 而 `?raw` 由 Vite 自己解析，既不依赖进程工作目录，也让这份源码进入依赖图（改了会重跑）。
import SOURCE from "./SettingsDialog.svelte?raw";

/**
 * 一个跨「重启」存活的假 settings 库。
 *
 * 这正是被测缺陷的舞台：SettingsDialog 常驻挂载、onMount 一进程只跑一次，所以
 * 「写了不回读」在同一次会话里看不出来——写完 `$state` 还在内存里，控件显示是对的。
 * 错位只在**下一次启动**显形。测试里用「卸载 + 重新 render」来扮演那次重启：
 * store 活过 cleanup，组件状态不活。
 */
const store = new Map<string, unknown>();
const writes: { key: string; value: unknown }[] = [];
/** 事件监听表（mcp:status 状态点）。 */
const listeners: Record<string, ((e: { payload: unknown }) => void)[]> = {};

/** 本页发出的裸 invoke 调用（M4b 手动更新检查用到 app_version / update_check）。 */
const invoked: { cmd: string }[] = [];
/** 桩的 update_check 返回什么，由用例逐个设定。 */
let updateCheckReply: unknown = { kind: "up-to-date", message: "已是最新版本。" };
/** 桩的 mcp_status 返回什么（状态点三态由它驱动）。 */
let mcpStatusReply: unknown = { enabled: false, listening: false, port: 0, connections: 0 };
/** openExternal 收到的 URL——「打开发布页」按钮的落点。 */
const opened: string[] = [];
let failSettingWrites = false;

vi.mock("../lib/ipc", () => ({
  // mcp:status 监听（2026-08-28 对外 MCP 状态点）：onMount 就调 listen，
  // 桩里没有这个导出会让**所有**用例在 onMount 处炸掉——不是少测一点，
  // 是别的用例全部假红（同 app_version 的教训，见下）。
  listen: async (name: string, cb: (e: { payload: unknown }) => void) => {
    (listeners[name] ??= []).push(cb);
    return () => {
      listeners[name] = (listeners[name] ?? []).filter((f) => f !== cb);
    };
  },
  settingGet: async <T>(key: string, fallback: T): Promise<T> =>
    store.has(key) ? (store.get(key) as T) : fallback,
  settingSet: async (key: string, value: unknown): Promise<void | boolean> => {
    if (failSettingWrites) return false;
    // 桩会**拒绝**后端白名单拒绝的值。一个永远成功的桩会让「保存失败要告诉用户」
    // 这条路径无法被测到——而那条路径正是这里最容易写错的一处（`void settingSet(…)`
    // 把 rejection 吞掉，界面显示的与库里存的从此各说各话）。
    // 规则与 app/src/commands/settings_cmd.rs 的 update.manifestUrl 分支同口径。
    if (key === "update.manifestUrl") {
      const s = String(value).trim();
      if (s !== "" && !s.startsWith("https://")) throw new Error("update.manifestUrl 必须以 https:// 开头");
    }
    writes.push({ key, value });
    store.set(key, value);
  },
  // 这三个此前不在桩里。它们缺席不是「少测一点」而是**让别的用例假红**：
  // 组件 onMount 第一行就是 `invoke("app_version")`，桩里没有这个导出时它是
  // undefined，调用直接抛 TypeError（不是 rejected promise，`.catch` 挂不住），
  // onMount 整条 await 链就此中断，后面十几个 settingGet 一个都不跑——
  // 于是「重启后显示库里的值」那一组会集体红在一个与它们无关的原因上。
  invoke: async <T>(cmd: string): Promise<T> => {
    invoked.push({ cmd });
    if (cmd === "app_version") return "0.5.0" as T;
    if (cmd === "update_check") return updateCheckReply as T;
    // MCP 区（2026-08-28）：状态/指引/token 三条命令，回复由用例设定
    if (cmd === "mcp_status") return mcpStatusReply as T;
    if (cmd === "mcp_connection_info")
      return {
        exe: "C:\\fs\\app.exe",
        config: '{\n  "mcpServers": { "future-shell": {} }\n}',
      } as T;
    if (cmd === "mcp_token_get" || cmd === "mcp_token_regen") return "tok-1234" as T;
    if (cmd === "mcp_set_enabled")
      return { enabled: true, listening: true, port: 54321, connections: 0 } as T;
    throw new Error(`未桩化的命令：${cmd}`);
  },
  openExternal: async (url: string): Promise<void> => {
    opened.push(url);
  },
}));

import SettingsDialog from "./SettingsDialog.svelte";

/** 从源码里抠出形如 `settingXxx("key"` 的键名（允许泛型参数与换行）。 */
function keysOf(re: RegExp): Set<string> {
  return new Set(Array.from(SOURCE.matchAll(re), (m) => m[1]));
}
const WRITTEN = keysOf(/settingSet\(\s*"([^"]+)"/g);
const READ = keysOf(/settingGet(?:<[^>]*>)?\(\s*"([^"]+)"/g);

async function openAt(tab?: string) {
  render(SettingsDialog, { props: { open: true } });
  if (tab) await fireEvent.click(screen.getByRole("tab", { name: new RegExp(tab) }));
}

/**
 * 等 onMount 那串 await 全部落地，再等 Svelte 把结果刷进 DOM。
 *
 * 断言「控件显示的就是默认值」时不能用 `waitFor`：控件的 `$state` 初值本来就等于默认值，
 * waitFor 第一次尝试立刻成功，onMount 还没跑完就返回绿。M40（把 fallback 从 "0" 改成 "5"）
 * 曾在这条上假绿——测的是初值，不是回读。桩里的 settingGet 无真实 I/O，
 * 一个宏任务边界足以清空整条微任务链，因此这里是确定性的等待而非轮询。
 */
async function flush() {
  await new Promise((r) => setTimeout(r, 0));
  await tick();
}

describe("SettingsDialog 设置回读", () => {
  beforeEach(() => {
    store.clear();
    writes.length = 0;
  });

  // ── 结构性门禁 ──────────────────────────────────────────────────────────
  //
  // 审计 P2「设置项只写不读」原文点名四个键（vault.autoLockMinutes / sftp.sandboxRoot /
  // ui.restoreUnclosed / term.bell）。逐个写用例只能钉住这四个，钉不住**第五个**——
  // 下一个往这个对话框里加控件的人照样会漏。所以真正的门禁是这条：
  // 本文件里凡 settingSet 出去的键，必须有一条 settingGet 把它读回来。
  //
  // 它是读源码而非跑组件的：这条不变量说的是「代码里有没有这一行」，不是「某次渲染的表现」，
  // 用行为测试去逼近反而要为每个键补一套 tab 切换与选择器，成本高且仍会漏。
  it("凡写入的设置键，必有对应的回读（新增控件漏了回读即红）", () => {
    const missing = [...WRITTEN].filter((k) => !READ.has(k)).sort();
    expect(missing).toEqual([]);
  });

  // 上一条若因正则失配而抓不到任何键，会「全部通过」——空集是任何集合的子集。
  // 这条钉死抓取本身有效：16 个控件键一个不少。
  // 第 15 个是 term.scheme：原先本页只有一句写着内部键名的提示文字、没有控件，
  // 而 TerminalPane 与三级作用域一直在消费它 ⇒ 全局层永久钉死在 DEFAULT_SCHEME
  // （详见 lib/settings-wiring.test.ts 的跨文件闭合守卫）。
  // 第 16 个是 transfer.verifyAfterTransfer：它在 TransferQueueDrawer 里**也**有一处控件
  // （UI 规格 §附表把载体钉在那里），但抽屉整体包在 `{#if $rows.size > 0}` 内，本会话有第一件
  // 传输之前根本不渲染；而该键是持久的、后端在 transfer_submit 当刻读它 ⇒ 上次关掉它的用户
  // 在传第一个文件之前无处可开，而那正是最需要它的一件。本页这一处是补上的可达入口，
  // 两处经 settingChanged 总线同步。
  // 第 22 个是 update.manifestUrl（M4b 手动更新检查）。它比别的键更需要这条回读保证：
  // 值为空 ≡ 永不联网，若写了不回读，用户重启后看到的是空输入框，会以为自己没配过
  // ——而库里那个地址仍然在，一点「检查更新」就发包到一个他以为已经不存在的地址。
  // 第 23 个是 term.customSchemes（M4b 导入的配色）。它是本页**唯一**的删除入口：
  // 「文件 → 导入」那条路径只往里加，若这里不给删，用户就处在「界面上能添加、不能移除」
  // 的状态——那与要求他去改数据库没有区别。
  // 28 → 27（2026-08-28）：mcp.enabled 不再经 settingSet 写——它改走
  // mcp_set_enabled 命令（开关要**即时起停监听**，只写位的开关是假开关）。
  // 它的「命令代写」登记与三件核查在 settings-wiring.test.ts。
  it("键提取有效：写入侧抓到全部 28 个键，读取侧不少于写入侧", () => {
    expect([...WRITTEN].sort()).toEqual([
      // M2：AI 执行档位（出口第 14 项的总开关）。断言是排序后的全集，
      // 所以位置无关；写在这里是为了挨着它的注释。
      "ai.mode",
      // M3：MCP 对外服务的调用方标签 / 工具白名单（§4.5，默认关）。
      // （开关本身见上面的 28 → 27 注记。）
      "mcp.caller",
      "mcp.mounts",
      "mcp.tools",
      "update.manifestUrl",
      "term.customSchemes",
      "hostkey.defaultPolicy",
      "keyboard.mode",
      "security.clipboardClear",
      "session.log",
      "keyboard.bindings",
      "sftp.sandboxRoot",
      "sidebar.hostProbeSeconds",
      "term.bell",
      "term.fontSize",
      "term.highlights",
      "term.opacity",
      "term.scheme",
      "term.zmodem",
      // 4d：RDP 帧投递路径（raw 裸字节通道 / event 对照）。设置页「终端外观」页可选。
      "rdp.frameTransport",
      "transfer.verifyAfterTransfer",
      "ui.copyOnSelect",
      "ui.ctrlVPaste",
      "ui.density",
      "ui.multilinePasteConfirm",
      "ui.restoreUnclosed",
      "ui.rightClick",
      "vault.autoLockMinutes",
    ].sort());
    expect(READ.size).toBeGreaterThanOrEqual(WRITTEN.size);
  });

  // `ui.language` 缺席不是遗漏，但**理由已经换了一个**（M4b）。
  //
  // 原来的理由是「控件 disabled、一个字节都写不出去」（§2.12 的 MVP 措辞）。M4b 到了，
  // 语言可切换，那个理由作废——是这条用例把它拦下来的，而不是有人记得回来改。
  //
  // 新理由：语言的真身是 i18n 的 `locale` store，持久化归 `lib/i18n/index.ts` 的
  // `setLocale` 一处。本页只是它的一个控件面，所以本页既不 settingSet 也不 settingGet 它。
  // 这不是绕过上面那条结构性门禁，是**把写入口收成一个**：语言要驱动菜单栏等全局面，
  // 若本页也留一份 $state 各写各的，「控件显示的」与「界面实际用的」就成了两个值。
  // 跨文件的那一圈（写入方 ↔ 消费方）由 lib/settings-wiring.test.ts 闭合。
  it("ui.language 不经本页写入——持久化归 i18n 模块单一入口", () => {
    expect(WRITTEN.has("ui.language")).toBe(false);
    expect(READ.has("ui.language")).toBe(false);
    // 控件确实已解禁（否则「不写」的理由会退回旧的那个，而那个已经不成立）
    expect(SOURCE).toMatch(/data-testid="set-language"/);
    expect(SOURCE).not.toMatch(/bind:value=\{language\}\s+disabled/);
    // 且它走的是 setLocale，不是又一处 settingSet
    expect(SOURCE).toMatch(/await setLocale\(/);
  });

  // ── 行为：重启后控件显示库里的值 ─────────────────────────────────────────

  // 这条钉的是审计里后果最重的一项，也是 P1-16 刚刚补上后端巡检才变得危险的那一项：
  // 库里存着 30（Vault 每 30 分钟自锁），重启后下拉框却显示「从不」。
  // 用户看到的与系统执行的不是同一件事，而且他**点不动**——select 的值本来就是「从不」，
  // 再点一次「从不」不产生 change 事件，settingSet 不会被调用，库里那个 30 原封不动。
  it("重启后 Vault 自动锁定显示库里的 30，而非控件初值「从不」", async () => {
    store.set("vault.autoLockMinutes", "30");
    await openAt("安全与 Vault");
    const el = screen.getByTestId("set-auto-lock") as HTMLSelectElement;
    await waitFor(() => expect(el.value).toBe("30"));
  });

  it("重启后沙箱根目录显示库里的路径，而非空", async () => {
    store.set("sftp.sandboxRoot", "D:/dl");
    await openAt("安全与 Vault");
    const el = screen.getByTestId("set-sandbox-root") as HTMLInputElement;
    await waitFor(() => expect(el.value).toBe("D:/dl"));
  });

  it("重启后 bell 行为显示库里的值", async () => {
    store.set("term.bell", "badge+notify");
    await openAt("终端外观");
    const el = screen.getByTestId("set-bell") as HTMLSelectElement;
    await waitFor(() => expect(el.value).toBe("badge+notify"));
  });

  // 关掉「启动时询问恢复」→ 重启 → 勾选框必须还是关的。
  // 这是完整的一圈：写入落库、卸载（模拟退出）、重新挂载后回读。App.svelte 那侧
  // 已按这个键决定要不要弹恢复框，回读若失灵，用户会看到「明明关了却还在弹」。
  it("关闭『启动时询问恢复』后重启，勾选框仍是关的", async () => {
    await openAt();
    const box = screen.getByTestId("set-restore-unclosed") as HTMLInputElement;
    await waitFor(() => expect(box.checked).toBe(true));

    await fireEvent.click(box);
    expect(writes).toEqual([{ key: "ui.restoreUnclosed", value: false }]);

    cleanup(); // ← 「退出应用」：组件状态没了，store 还在
    await openAt();
    const again = screen.getByTestId("set-restore-unclosed") as HTMLInputElement;
    await waitFor(() => expect(again.checked).toBe(false));
  });

  // 「传后校验」在 TransferQueueDrawer 里也有一处控件，但那个抽屉整体包在
  // `{#if $rows.size > 0}` 内，本会话有第一件传输之前不渲染；而这个键是持久的、
  // 后端在 `transfer_submit` **当刻**才读它。于是「上次会话把它关了的人，能不能在传第一个
  // 文件之前把它开回来」这个问题，只由本页这处控件回答——那第一件恰恰是最该被校验的一件。
  // 走完整一圈（关 → 退出 → 重启读回「关」→ 就地开回来），中间不经过抽屉。
  it("关闭『传后校验』后重启仍显示关，并可就地开回（抽屉未渲染时的唯一入口）", async () => {
    await openAt("安全与 Vault");
    const box = screen.getByTestId("set-verify-after-transfer-global") as HTMLInputElement;
    await waitFor(() => expect(box.checked).toBe(true)); // 默认开（前置条件，非本条的证据）

    await fireEvent.click(box);
    expect(writes).toEqual([{ key: "transfer.verifyAfterTransfer", value: false }]);

    cleanup(); // ← 「退出应用」：组件状态没了，store 还在
    await openAt("安全与 Vault");
    const again = screen.getByTestId("set-verify-after-transfer-global") as HTMLInputElement;
    // $state 初值是 true，因此这里读到 false 只可能来自回读——不是残留也不是初值
    await waitFor(() => expect(again.checked).toBe(false));
    await fireEvent.click(again); // ← 传第一个文件之前就够得着
    expect(writes.at(-1)).toEqual({ key: "transfer.verifyAfterTransfer", value: true });
  });

  // 回读的 fallback 必须与控件的 $state 初值逐字一致，否则「首次启动」和
  // 「读到空库」会呈现两套不同的默认值。空库时控件显示的就是 fallback，
  // 于是这条同时验了两边。
  it("空库时控件呈现声明的默认值（fallback 与 $state 初值一致）", async () => {
    await openAt("安全与 Vault");
    await flush();
    expect((screen.getByTestId("set-auto-lock") as HTMLSelectElement).value).toBe("0");
    expect((screen.getByTestId("set-sandbox-root") as HTMLInputElement).value).toBe("");
    // 这一处的默认值还要与后端 `settings_bool("transfer.verifyAfterTransfer", true)` 一致：
    // 空库时前后端都必须落到「开」，否则用户从没动过这个开关，界面说开、后端按关执行。
    expect((screen.getByTestId("set-verify-after-transfer-global") as HTMLInputElement).checked).toBe(true);
    expect(writes).toEqual([]); // 纯回读不得反向写库
  });

  it("空库时通用/终端页的默认值同样来自 fallback 而非残留状态", async () => {
    await openAt();
    await flush();
    expect((screen.getByTestId("set-restore-unclosed") as HTMLInputElement).checked).toBe(true);
    await fireEvent.click(screen.getByRole("tab", { name: /终端外观/ }));
    expect((screen.getByTestId("set-bell") as HTMLSelectElement).value).toBe("badge");
    expect(writes).toEqual([]);
  });
});

/**
 * 手动更新检查（M4b）。这一组测的不是「功能能不能用」，而是**一条承诺**：
 *
 *   「本程序不会在后台检查更新，只有你点那个按钮时才联网一次。」
 *
 * 这句话在界面上是文字，在代码里的落点是**没有那行代码**——onMount 里没有 update_check，
 * 没有 setInterval，没有 $effect 触发。「没有」这件事没法靠读代码保证（谁都可能加回来），
 * 只能靠一条会因为它的出现而变红的用例。所以下面第一条才是这组的主角：它不断言任何
 * 可见行为，只断言「打开设置页、切到更新页、什么都不点」之后，update_check 的调用次数是 0。
 */
describe("SettingsDialog 手动更新检查", () => {
  beforeEach(() => {
    store.clear();
    writes.length = 0;
    invoked.length = 0;
    opened.length = 0;
    updateCheckReply = { kind: "up-to-date", message: "已是最新版本。" };
  });

  it("光是打开设置页、切到更新页，一个包都不发", async () => {
    store.set("update.manifestUrl", "https://example.invalid/latest.json"); // 地址已配好也一样
    await openAt("更新");
    await flush();
    expect(invoked.filter((i) => i.cmd === "update_check")).toEqual([]);
    // 反向对照：确实渲染到了更新页、桩也确实在工作（否则上一条会因「什么都没发生」假绿）
    expect(screen.getByTestId("update-check-btn")).toBeTruthy();
    expect(invoked.some((i) => i.cmd === "app_version")).toBe(true);
  });

  it("地址回读进输入框（否则用户以为自己没配过，而库里那个地址仍会被用）", async () => {
    store.set("update.manifestUrl", "https://example.invalid/latest.json");
    await openAt("更新");
    await flush();
    expect((screen.getByTestId("set-update-url") as HTMLInputElement).value).toBe(
      "https://example.invalid/latest.json",
    );
    expect(screen.getByTestId("update-current-version").textContent).toContain("0.5.0");
  });

  it("点了按钮才发，且结果文案来自后端（不在前端重拼承诺性表述）", async () => {
    updateCheckReply = { kind: "up-to-date", message: "已是最新版本（0.5.0）。" };
    await openAt("更新");
    await flush();
    await fireEvent.click(screen.getByTestId("update-check-btn"));
    await waitFor(() => expect(screen.getByTestId("update-result")).toBeTruthy());
    expect(invoked.filter((i) => i.cmd === "update_check").length).toBe(1);
    expect(screen.getByTestId("update-result").textContent).toContain("已是最新版本（0.5.0）。");
    // 无新版本时不给「打开发布页」按钮
    expect(screen.queryByTestId("update-open-page")).toBeNull();
  });

  it("有新版本时给的是「打开发布页」，不是「下载并安装」", async () => {
    // 这条钉的是裁决本身：只做检查，不下载、不替换自己。界面上唯一的后续动作
    // 必须是把用户交给浏览器——本程序不碰那个文件。
    updateCheckReply = {
      kind: "available",
      message: "有新版本 0.6.0（当前 0.5.0）。",
      url: "https://example.invalid/releases/0.6.0",
    };
    await openAt("更新");
    await flush();
    await fireEvent.click(screen.getByTestId("update-check-btn"));
    await waitFor(() => expect(screen.getByTestId("update-open-page")).toBeTruthy());
    await fireEvent.click(screen.getByTestId("update-open-page"));
    expect(opened).toEqual(["https://example.invalid/releases/0.6.0"]);
    // 全仓意义上的「不自我替换」由 update_check.rs 的源码断言把守；这里只保证
    // 界面没有第二个动作按钮（比如「立即更新」）绕过它。
    expect(screen.queryByText(/立即更新|下载并安装|自动更新/)).toBeNull();
  });

  it("地址清空即写回空串——用户撤回「允许联网」的唯一出路必须真的通到库里", async () => {
    store.set("update.manifestUrl", "https://example.invalid/latest.json");
    await openAt("更新");
    await flush();
    const input = screen.getByTestId("set-update-url") as HTMLInputElement;
    await fireEvent.input(input, { target: { value: "  " } });
    await fireEvent.change(input);
    expect(writes).toContainEqual({ key: "update.manifestUrl", value: "" });
    expect(store.get("update.manifestUrl")).toBe(""); // 空白被 trim 掉，不是存了两个空格
  });
});

describe("SettingsDialog 更新地址的保存失败要说出来", () => {
  beforeEach(() => {
    store.clear();
    writes.length = 0;
    invoked.length = 0;
  });

  it("填了明文 http 会当场报错，且库里不留这个值", async () => {
    await openAt("更新");
    await flush();
    const input = screen.getByTestId("set-update-url") as HTMLInputElement;
    await fireEvent.input(input, { target: { value: "http://evil.example/latest.json" } });
    await fireEvent.change(input);
    await waitFor(() => expect(screen.getByTestId("update-url-error")).toBeTruthy());
    expect(screen.getByTestId("update-url-error").textContent).toContain("https://");
    expect(store.has("update.manifestUrl")).toBe(false);
    expect(writes).toEqual([]);
  });

  it("改对之后错误提示消失（不是贴上去就不走了）", async () => {
    await openAt("更新");
    await flush();
    const input = screen.getByTestId("set-update-url") as HTMLInputElement;
    await fireEvent.input(input, { target: { value: "http://evil.example/latest.json" } });
    await fireEvent.change(input);
    await waitFor(() => expect(screen.queryByTestId("update-url-error")).toBeTruthy());

    await fireEvent.input(input, { target: { value: " https://ok.example/latest.json " } });
    await fireEvent.change(input);
    await waitFor(() => expect(screen.queryByTestId("update-url-error")).toBeNull());
    // 落库的是 trim 过的串，且输入框显示的就是落库的那一个
    expect(store.get("update.manifestUrl")).toBe("https://ok.example/latest.json");
    expect(input.value).toBe("https://ok.example/latest.json");
  });
});

/**
 * 导入进来的配色（M4b）。本页是它**唯一**的删除入口——「文件 → 导入」那条路径只往里加。
 *
 * 一个能被添加却不能被移除的东西，等于要求用户去改数据库。这一组钉的就是那个出口，
 * 以及它连带的一个陷阱：删掉的正好是当前选中的那一套时，`term.scheme` 会指向一个
 * 已经不存在的 id，而消费侧遇到未知 id 是**静默回落**默认色的——用户看到终端变了色，
 * 却在设置页里找不到原因（下拉框显示的是一个已经不在列表里的值）。
 */
describe("SettingsDialog 导入的配色", () => {
  const mkScheme = (name: string, bg = "#1e1e2e") => ({
    name,
    foreground: "#cccccc",
    background: bg,
    ansi: Array.from({ length: 16 }, () => "#112233"),
    cursor: null,
  });

  beforeEach(() => {
    store.clear();
    writes.length = 0;
  });

  it("库里有几套就列几行，且下拉里也选得到", async () => {
    store.set("term.customSchemes", [mkScheme("MyDark"), mkScheme("Solarized 改")]);
    await openAt("终端外观");
    await flush();
    await waitFor(() => expect(screen.getAllByTestId("custom-scheme-row").length).toBe(2));
    const sel = screen.getByTestId("set-scheme") as HTMLSelectElement;
    const values = Array.from(sel.options).map((o) => o.value);
    expect(values).toContain("custom:MyDark");
    expect(values).toContain("custom:Solarized 改");
    // 内置的 12 套一个没少（导入的是**追加**，不是替换）
    expect(values).toContain("futureshell-dark");
  });

  it("形状不合规的条目被滤掉，不会带着坏数据去渲染终端", async () => {
    // 这些值最终进 xterm 的 theme 对象。一条坏数据能把整个终端渲染搞崩，
    // 而那种崩法（白屏/无字）用户完全无从判断原因——宁可少显示一套。
    store.set("term.customSchemes", [
      mkScheme("好的"),
      { name: "少了颜色", foreground: "#cccccc" }, // 缺 background/ansi
      { name: "ansi 不足", foreground: "#cccccc", background: "#000000", ansi: ["#111111"] },
      { ...mkScheme("大写十六进制"), background: "#1E1E2E" }, // 前端 HEX6 只认小写
    ]);
    await openAt("终端外观");
    await flush();
    await waitFor(() => expect(screen.getAllByTestId("custom-scheme-row").length).toBe(1));
    expect(screen.getByTestId("custom-scheme-row").textContent).toContain("好的");
  });

  it("删一套：界面少一行，库里也少一条", async () => {
    store.set("term.customSchemes", [mkScheme("MyDark"), mkScheme("留下的")]);
    await openAt("终端外观");
    await flush();
    await waitFor(() => expect(screen.getAllByTestId("custom-scheme-row").length).toBe(2));

    await fireEvent.click(screen.getByTestId("delete-custom-scheme-MyDark"));
    await waitFor(() => expect(screen.getAllByTestId("custom-scheme-row").length).toBe(1));
    expect(screen.getByTestId("custom-scheme-row").textContent).toContain("留下的");
    const saved = store.get("term.customSchemes") as { name: string }[];
    expect(saved.map((s) => s.name)).toEqual(["留下的"]);
  });

  it("删掉的正好是当前选中的那套时，全局方案退回默认", async () => {
    store.set("term.customSchemes", [mkScheme("MyDark")]);
    store.set("term.scheme", "custom:MyDark");
    await openAt("终端外观");
    await flush();
    const sel = screen.getByTestId("set-scheme") as HTMLSelectElement;
    await waitFor(() => expect(sel.value).toBe("custom:MyDark"));

    await fireEvent.click(screen.getByTestId("delete-custom-scheme-MyDark"));
    await waitFor(() => expect(store.get("term.scheme")).toBe("futureshell-dark"));
    // 下拉框也要跟着变——否则它显示的是一个已经不在选项里的值
    expect(sel.value).toBe("futureshell-dark");
  });

  it("删的不是选中的那套时，不动 term.scheme", async () => {
    // 反向对照：上一条若因「删任何一套都重置」而通过，这一条会红。
    store.set("term.customSchemes", [mkScheme("MyDark"), mkScheme("别动我")]);
    store.set("term.scheme", "custom:别动我");
    await openAt("终端外观");
    await flush();
    await waitFor(() => expect(screen.getAllByTestId("custom-scheme-row").length).toBe(2));

    await fireEvent.click(screen.getByTestId("delete-custom-scheme-MyDark"));
    await waitFor(() => expect(screen.getAllByTestId("custom-scheme-row").length).toBe(1));
    expect(store.get("term.scheme")).toBe("custom:别动我");
    expect(writes.some((w) => w.key === "term.scheme")).toBe(false);
  });

  it("一套都没有时不显示那一块（空列表比不显示更让人以为出了错）", async () => {
    await openAt("终端外观");
    await flush();
    expect(screen.queryByTestId("custom-schemes")).toBeNull();
    const sel = screen.getByTestId("set-scheme") as HTMLSelectElement;
    expect(Array.from(sel.options).every((o) => !o.value.startsWith("custom:"))).toBe(true);
  });
});

/**
 * 语言切换（M4b 出口「设置项切换语言生效 round-trip」的界面半边）。
 *
 * round-trip 本身（落库 → 重启 → 仍是新语言）在 lib/i18n/i18n.test.ts 里测。
 * 这里只钉一件事：**这个下拉框真的接在那条链路上**——控件存在但没接线，
 * 是这个仓里出现过不止一次的形态（term.scheme 只有提示文字没有控件、
 * import.xshell 有菜单项没有分支）。
 */
describe("SettingsDialog 语言切换", () => {
  beforeEach(() => {
    store.clear();
    writes.length = 0;
    locale.set("zh-CN");
  });

  it("下拉框列出全部可选语言，当前值来自 locale store", async () => {
    locale.set("en");
    await openAt();
    await flush();
    const sel = screen.getByTestId("set-language") as HTMLSelectElement;
    expect(Array.from(sel.options).map((o) => o.value)).toEqual(["zh-CN", "en"]);
    expect(sel.value).toBe("en");
  });

  it("选了英文：store 变、库里也变（不是只改了控件显示）", async () => {
    await openAt();
    await flush();
    const sel = screen.getByTestId("set-language") as HTMLSelectElement;
    await fireEvent.change(sel, { target: { value: "en" } });
    await waitFor(() => expect(get(locale)).toBe("en"));
    expect(store.get("ui.language")).toBe("en");
    expect(writes).toContainEqual({ key: "ui.language", value: "en" });
  });

  it("同一页上的文案跟着变（说明本页也接了 $t，不只是菜单栏）", async () => {
    await openAt();
    await flush();
    const label = () => screen.getByTestId("language-scope").textContent ?? "";
    expect(label()).toContain("菜单栏");
    await fireEvent.change(screen.getByTestId("set-language") as HTMLSelectElement, {
      target: { value: "en" },
    });
    await waitFor(() => expect(label()).toContain("menu bar"));
  });

  it("范围说明必须在页面上——半翻译的界面要说清是半翻译", async () => {
    // 切成英文后其余界面仍是中文。不说明的话，用户会以为翻译坏了；
    // 说明了，那就是一个已知边界（M4b 出口原文允许「双语或仅搭框架二选一」）。
    await openAt();
    await flush();
    expect(screen.getByTestId("language-scope")).toBeTruthy();
  });
});

/**
 * 配色编辑器与导出（M4b 出口第 4 项的界面半边）。
 *
 * round-trip 的格式一致性在 lib/scheme-file.test.ts 与 Rust 侧测；编辑器自身在
 * SchemeEditor.test.ts 测。这里只钉两者之间那条线——以及一个只在这里出得来的坑：
 * 改名时选中状态要跟着搬走。
 */
describe("SettingsDialog 配色编辑器接线", () => {
  const mkScheme = (name: string, bg = "#1e1e2e") => ({
    name,
    foreground: "#cccccc",
    background: bg,
    ansi: Array.from({ length: 16 }, () => "#112233"),
    cursor: null,
  });

  beforeEach(() => {
    store.clear();
    writes.length = 0;
  });

  it("一套配色都没有时，「新建配色…」仍然在", async () => {
    // 这个按钮若包在「有自定义配色才显示」的条件里，用户第一次要造配色时
    // 会发现界面上没有任何入口——而那正是最需要它的时刻。
    await openAt("终端外观");
    await flush();
    expect(screen.queryByTestId("custom-schemes")).toBeNull(); // 前提：确实一套都没有
    expect(screen.getByTestId("new-custom-scheme")).toBeTruthy();
  });

  it("新建：编辑器存了之后，列表与库里都多一套", async () => {
    await openAt("终端外观");
    await flush();
    await fireEvent.click(screen.getByTestId("new-custom-scheme"));
    await waitFor(() => expect(screen.getByTestId("scheme-editor")).toBeTruthy());

    await fireEvent.input(screen.getByTestId("scheme-name"), { target: { value: "新配色" } });
    await fireEvent.click(screen.getByTestId("scheme-save"));

    await waitFor(() => expect(screen.getAllByTestId("custom-scheme-row").length).toBe(1));
    const saved = store.get("term.customSchemes") as { name: string }[];
    expect(saved.map((s) => s.name)).toEqual(["新配色"]);
    expect(screen.queryByTestId("scheme-editor")).toBeNull(); // 存完自动关
  });

  it("编辑：打开时带着这一套的值，存了之后是替换不是新增", async () => {
    store.set("term.customSchemes", [mkScheme("A", "#111111"), mkScheme("B")]);
    await openAt("终端外观");
    await flush();
    await waitFor(() => expect(screen.getAllByTestId("custom-scheme-row").length).toBe(2));

    await fireEvent.click(screen.getByTestId("edit-custom-scheme-A"));
    await waitFor(() =>
      expect((screen.getByTestId("scheme-name") as HTMLInputElement).value).toBe("A"),
    );
    await fireEvent.input(screen.getByTestId("scheme-background"), { target: { value: "#222222" } });
    await fireEvent.click(screen.getByTestId("scheme-save"));

    await waitFor(() => expect(screen.queryByTestId("scheme-editor")).toBeNull());
    const saved = store.get("term.customSchemes") as { name: string; background: string }[];
    expect(saved.length).toBe(2); // 替换，不是变成三套
    expect(saved.find((s) => s.name === "A")!.background).toBe("#222222");
  });

  /**
   * 改名时把选中状态一起搬走。
   *
   * `term.scheme` 存的是 `custom:旧名`。不跟着改的话它就指向一个不存在的 id，
   * 而消费侧遇到未知 id 是**静默回落**默认色的——用户只是改了个名字，
   * 终端却变回默认配色，而设置页里看不出为什么。
   */
  it("改名时，选中的那套跟着改（不会指向一个不存在的 id）", async () => {
    store.set("term.customSchemes", [mkScheme("旧名")]);
    store.set("term.scheme", "custom:旧名");
    await openAt("终端外观");
    await flush();
    await waitFor(() =>
      expect((screen.getByTestId("set-scheme") as HTMLSelectElement).value).toBe("custom:旧名"),
    );

    await fireEvent.click(screen.getByTestId("edit-custom-scheme-旧名"));
    await waitFor(() => expect(screen.getByTestId("scheme-editor")).toBeTruthy());
    await fireEvent.input(screen.getByTestId("scheme-name"), { target: { value: "新名" } });
    await fireEvent.click(screen.getByTestId("scheme-save"));

    await waitFor(() => expect(store.get("term.scheme")).toBe("custom:新名"));
    expect((screen.getByTestId("set-scheme") as HTMLSelectElement).value).toBe("custom:新名");
  });

  it("改名时若选中的是**别的**那套，不动 term.scheme", async () => {
    // 反向对照：上一条若因「改名就重设」而通过，这一条会红。
    store.set("term.customSchemes", [mkScheme("A"), mkScheme("别动我")]);
    store.set("term.scheme", "custom:别动我");
    await openAt("终端外观");
    await flush();
    await waitFor(() => expect(screen.getAllByTestId("custom-scheme-row").length).toBe(2));

    await fireEvent.click(screen.getByTestId("edit-custom-scheme-A"));
    await waitFor(() => expect(screen.getByTestId("scheme-editor")).toBeTruthy());
    await fireEvent.input(screen.getByTestId("scheme-name"), { target: { value: "A2" } });
    await fireEvent.click(screen.getByTestId("scheme-save"));

    await waitFor(() => expect(screen.queryByTestId("scheme-editor")).toBeNull());
    expect(store.get("term.scheme")).toBe("custom:别动我");
    expect(writes.some((w) => w.key === "term.scheme")).toBe(false);
  });

  it("编辑器里的重名判断认得出已有的那些名字", async () => {
    store.set("term.customSchemes", [mkScheme("A"), mkScheme("B")]);
    await openAt("终端外观");
    await flush();
    await waitFor(() => expect(screen.getAllByTestId("custom-scheme-row").length).toBe(2));

    await fireEvent.click(screen.getByTestId("new-custom-scheme"));
    await waitFor(() => expect(screen.getByTestId("scheme-editor")).toBeTruthy());
    await fireEvent.input(screen.getByTestId("scheme-name"), { target: { value: "B" } });
    await waitFor(() => expect(screen.getByTestId("scheme-name-error")).toBeTruthy());
    expect((screen.getByTestId("scheme-save") as HTMLButtonElement).disabled).toBe(true);
  });

  it("导出按钮在（单套与全部两个入口）", async () => {
    store.set("term.customSchemes", [mkScheme("A"), mkScheme("B")]);
    await openAt("终端外观");
    await flush();
    await waitFor(() => expect(screen.getAllByTestId("custom-scheme-row").length).toBe(2));
    expect(screen.getByTestId("export-custom-scheme-A")).toBeTruthy();
    expect(screen.getByTestId("export-all-schemes")).toBeTruthy();
  });
});

/* ------------------------------------------------------------------------------
 * 对外 MCP 区（2026-08-28 套接字形态）
 *
 * 用户四条反馈对应的判据：
 * -「分不明白」→ 三组方向化标题（① 提供方 / ② 对外服务 / ③ 外部挂载）；
 * -「不知道怎么连」→ 连接指引（可粘贴 JSON + 复制按钮）；
 * -「看不到在线状态」→ 状态点 + mcp:status 事件实时驱动；
 * -「没有 secret token」→ token 展示/复制/轮换。
 * ---------------------------------------------------------------------------- */
describe("SettingsDialog 对外MCP", () => {
  function emitStatus(payload: unknown): void {
    for (const cb of listeners["mcp:status"] ?? []) cb({ payload });
  }

  async function openAiTab(): Promise<void> {
    await openAt("AI");
    await flush();
  }

  beforeEach(() => {
    writes.length = 0;
    invoked.length = 0;
    mcpStatusReply = { enabled: false, listening: false, port: 0, connections: 0 };
    store.clear();
  });

  it("三组方向化标题齐全——MCP 一词出现在两组里，不写方向必然混淆", async () => {
    await openAiTab();
    expect(screen.getByTestId("ai-grp-provider").textContent).toContain("本程序 → 模型服务");
    expect(screen.getByTestId("ai-grp-server").textContent).toContain("外部 AI 客户端 → 本程序");
    expect(screen.getByTestId("ai-grp-mounts").textContent).toContain("本程序 → 其他 MCP 服务器");
  });

  it("开关走 mcp_set_enabled（即时起停），不得再走 settingSet（那是假开关）", async () => {
    await openAiTab();
    await fireEvent.click(screen.getByTestId("set-mcp-enabled"));
    await waitFor(() => expect(invoked.some((c) => c.cmd === "mcp_set_enabled")).toBe(true));
    expect(writes.some((w) => w.key === "mcp.enabled")).toBe(false);
  });

  it("mcp:status 事件驱动状态点：监听中（含端口）→ 已连接客户端（含数量）", async () => {
    await openAiTab();
    // 未开时不渲染状态行（没有可报告的东西）
    expect(screen.queryByTestId("mcp-status")).toBeNull();

    // 打开开关（桩 mcp_set_enabled 成功回 listening 状态）
    mcpStatusReply = { enabled: true, listening: true, port: 54321, connections: 0 };
    await fireEvent.click(screen.getByTestId("set-mcp-enabled"));
    await waitFor(() =>
      expect(screen.getByTestId("mcp-status").textContent).toContain("监听中"),
    );
    expect(screen.getByTestId("mcp-status").textContent).toContain("54321");

    // 客户端连上：事件推送 connections: 2
    emitStatus({ enabled: true, listening: true, port: 54321, connections: 2 });
    await waitFor(() =>
      expect(screen.getByTestId("mcp-status").textContent).toContain("已连接客户端 ×2"),
    );
  });

  it("开启后展示连接指引与 token（各带复制按钮）；轮换按钮调命令", async () => {
    await openAiTab();
    mcpStatusReply = { enabled: true, listening: true, port: 54321, connections: 0 };
    await fireEvent.click(screen.getByTestId("set-mcp-enabled"));
    await waitFor(() => expect(screen.getByTestId("mcp-guide")).toBeTruthy());
    expect(screen.getByTestId("mcp-guide").textContent).toContain("future-shell");
    expect(screen.getByTestId("mcp-token").textContent).toBe("tok-1234");
    expect(screen.getByTestId("mcp-copy-guide")).toBeTruthy();
    expect(screen.getByTestId("mcp-copy-token")).toBeTruthy();

    await fireEvent.click(screen.getByTestId("mcp-regen"));
    await waitFor(() => expect(invoked.some((c) => c.cmd === "mcp_token_regen")).toBe(true));
  });
});

/**
 * 4d：RDP 帧传输（rdp.frameTransport）——raw 默认，event 留作真机 A/B 对比
 * （FS_RDP_FRAMESTATS=1 的判据要求两条路径在同一构建里可比）与排查。
 */
describe("SettingsDialog · RDP 帧传输（4d）", () => {
  it("终端页有下拉、两档齐、默认 raw、改动即写设置", async () => {
    await openAt("安全与 Vault");
    await flush();
    const sel = document.querySelector('[data-testid="set-rdp-transport"]') as HTMLSelectElement;
    expect(sel).not.toBeNull();
    const opts = [...sel.options].map((o) => o.value);
    expect(opts).toEqual(expect.arrayContaining(["raw", "event"]));
    expect(sel.value).toBe("raw"); // 默认档
    // settingSet 桩（见文件头 mock）：写入会进 setCalls
    writes.length = 0;
    sel.value = "event";
    await fireEvent.change(sel);
    await waitFor(() => expect(writes.some((w) => w.key === "rdp.frameTransport" && w.value === "event")).toBe(true));
  });
});

describe("SettingsDialog 保存反馈与完整参数", () => {
  beforeEach(() => { store.clear(); writes.length = 0; failSettingWrites = false; });
  it("失败后显示重试入口，重试成功才显示已保存", async () => {
    render(SettingsDialog, { open: true });
    await flush();
    failSettingWrites = true;
    await fireEvent.click(screen.getByTestId("set-restore-unclosed"));
    expect(screen.getByTestId("settings-save-status").textContent).toContain("未保存");
    expect(writes).toHaveLength(0);
    failSettingWrites = false;
    await fireEvent.click(screen.getByRole("button", { name: "重试保存" }));
    expect(screen.getByTestId("settings-save-status").textContent).toContain("已保存");
    expect(store.get("ui.restoreUnclosed")).toBe(false);
  });
  it("外部服务器填写完整再保存，参数路径中的空格保持完整", async () => {
    render(SettingsDialog, { open: true, initialTab: "ai" });
    await flush();
    await fireEvent.click(screen.getByTestId("add-mcp-mount"));
    await fireEvent.change(screen.getByLabelText("服务器标识"), { target: { value: "files" } });
    await fireEvent.change(screen.getByLabelText("启动命令"), { target: { value: "node" } });
    await fireEvent.change(screen.getByLabelText("启动参数（每行一个）"), { target: { value: "C:/Program Files/server.js\n--readonly" } });
    expect(writes).toHaveLength(0);
    await fireEvent.click(screen.getByTestId("save-mcp-mounts"));
    expect(store.get("mcp.mounts")).toEqual([{ server_id: "files", command: "node", args: ["C:/Program Files/server.js", "--readonly"] }]);
  });
  it("设置分类支持方向键，隐藏尚未实现的空白页", async () => {
    render(SettingsDialog, { open: true });
    await flush();
    const tab = screen.getByRole("tab", { name: "通用" });
    tab.focus();
    await fireEvent.keyDown(tab, { key: "ArrowDown" });
    expect(screen.getByRole("tab", { name: "键盘与鼠标" }).getAttribute("aria-selected")).toBe("true");
    expect(screen.queryByRole("tab", { name: /高级/ })).toBeNull();
  });
});
