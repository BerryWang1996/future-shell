import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { writable, type Readable } from "svelte/store";
import { toast } from "./toast"; // 仅用于 settingGet 的 IPC 故障可见化；toast 不反向依赖 ipc，无循环

export { invoke, listen };
export type { UnlistenFn };

export async function ping(): Promise<string> {
  return invoke("ping");
}

/** 外链打开（http/https/mailto 白名单由 Rust 侧 open_external 命令校验，spec §2.8）。 */
export async function openExternal(url: string): Promise<void> {
  await invoke("open_external", { url });
}

/**
 * 前端异常上报到后端日志（S300）。
 *
 * 改造前整个前端**零**全局错误处理器，三条静默吞没路径（`ctrl?.write` 的可选链
 * 短路、`void invoke(...)` 丢弃的 rejection、`decodeB64` 的同步抛）任何一条触发
 * 都会丢帧且不留痕。2026-08-19 的渲染帧丢失事故复盘时无法在这三个出口之间定位，
 * 就是因为它们全都不出声。后端按 `source + message` 做 60 s 去重，不必在此限流。
 *
 * 本函数**恒不抛**：错误上报路径自己再抛会造成递归上报，且它永远只是诊断侧支，
 * 不该影响调用方的控制流。
 */
export function reportFrontendError(source: string, err: unknown): void {
  const message = err instanceof Error ? `${err.message}\n${err.stack ?? ""}` : String(err);
  void invoke("log_frontend_error", { source, message }).catch(() => {
    // 上报本身失败（IPC 不通）只能落控制台——再 invoke 一次只会同样失败。
    console.error(`[${source}]`, err);
  });
}

/** 设置读取故障的 toast 节流窗口：SettingsDialog 单次打开会连发十余次 settingGet，
 *  IPC 整体不通时每次都弹会瞬间挤爆 4 条堆叠上限并淹没真正的业务提示。 */
const SETTING_ERROR_TOAST_WINDOW_MS = 5000;
let lastSettingErrorToastAt = 0;

/**
 * 读取设置（settings_get：值为 JSON 字符串；不存在返回 None → null）。
 *
 * P2 审计：原实现一个 catch 吞掉全部异常并静默回落，把两类语义完全不同的情况混为一谈——
 * 「键不存在」是首次运行的正常路径（回落默认值即正确行为，静默无妨），而
 * 「IPC 调用失败/命令未注册/JSON 已写脏」是故障（用户看到的是设置被悄悄重置回默认，
 * 且改完再打开又变回去，无任何线索）。这里按来源分流：
 * - raw == null → 键不存在，静默回落（不打日志、不 toast）；
 * - invoke 抛出 → 真故障：console.error 留栈 + 节流 toast 告知「设置读取失败，暂用默认值」；
 * - JSON.parse 抛出 → 值已损坏：console.error 留栈（含原始串便于排障）+ 同一节流 toast。
 * 三者一律仍返回 fallback：调用方遍布 initTheme/initLayout/TerminalPane 挂载链，
 * 上抛会让整条初始化链断在中途（黑屏），故障可见性由日志 + toast 承担。
 */
export async function settingGet<T>(key: string, fallback: T): Promise<T> {
  let raw: string | null;
  try {
    raw = await invoke<string | null>("settings_get", { key });
  } catch (e) {
    console.error(`settings_get(${key}) IPC 调用失败，暂用默认值`, e);
    notifySettingFailure();
    return fallback;
  }
  if (raw == null) return fallback; // 键不存在：正常回落，非故障
  try {
    return JSON.parse(raw) as T;
  } catch (e) {
    console.error(`设置 ${key} 的持久值不是合法 JSON，暂用默认值；原始值：${raw}`, e);
    notifySettingFailure();
    return fallback;
  }
}

/** 节流推送设置读取故障 toast（不带 key：用户无从据 key 行动，且多键同故障文案应合一）。 */
function notifySettingFailure(): void {
  const now = Date.now();
  if (now - lastSettingErrorToastAt < SETTING_ERROR_TOAST_WINDOW_MS) return;
  lastSettingErrorToastAt = now;
  toast.warn("设置读取失败，暂用默认值。请重新打开设置重试；可在「帮助 → 诊断日志目录」查看原因。");
}

/**
 * 设置变更广播（UI 规格 §3.2「即时生效」的传输通道）。
 *
 * 缺这条通道的后果，实测：`TerminalPane` 的 7 个全局键（term.scheme/term.fontSize/term.opacity/
 * ui.rightClick/ui.copyOnSelect/ui.multilinePasteConfirm/ui.ctrlVPaste）只在 onMount 读一次，
 * 选项对话框改完对**所有已打开的终端**一律不生效——而标签是恒挂载的（隐藏≠销毁），陈旧值
 * 一直续到关标签为止。用户唯一的出路是把每个标签关掉重开，界面上没有任何东西提示这一点。
 *
 * 总线挂在 `settingSet` 这**唯一写入口**上，而不是让各调用点自己 emit：调用点有十余处、且还会增加，
 * 任何一处忘了 emit 就是又一个「改了不生效」，而那种缺陷不报错、不留痕、门禁看不见。
 * 落在写入口上则漏无可漏——写入即广播是同一个函数体里的两行。
 *
 * 两条口径：
 * - **invoke 成功后才广播**。写库失败仍广播的话，界面会显示一个重启即消失的值（更糟：用户据此
 *   以为设置生效了）；不广播则「我选了 Dracula，什么都没发生」是一个能被看见的信号，与 catch 里的
 *   console.warn 同向。
 * - `writable` 保留末值，故**后订阅者会立刻收到最后一次变更**（新开标签订阅时重放）。这是良性的：
 *   重放的值恰等于该订阅者刚刚 settingGet 读到的持久值，赋一次等值不产生行为差异。
 *   订阅方因此**不得**在处理器里做「变更即弹提示」一类有副作用的事。
 */
const settingBus = writable<{ key: string; value: unknown } | null>(null);
export const settingChanged: Readable<{ key: string; value: unknown } | null> = {
  subscribe: settingBus.subscribe, // 只导出 subscribe：外部只能订阅，不能伪造一条没落库的变更
};

/** 成功后广播并返回 true；失败向用户报错并返回 false，允许自动保存调用安全地忽略 Promise。 */
export async function settingSet(key: string, value: unknown): Promise<boolean> {
  try {
    await invoke("settings_set", { key, value: JSON.stringify(value) });
  } catch (e) {
    console.warn(`setting ${key} 落库失败`, e);
    toast.error(`设置未保存，请重试：${e}`);
    return false;
  }
  settingBus.set({ key, value });
  return true;
}
