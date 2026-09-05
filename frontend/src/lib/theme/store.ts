import { get, writable } from "svelte/store";
import { getTheme } from "./themes";
import { settingGet, settingSet } from "../ipc";

/** 主题偏好：主题 id 或 "auto"（按 prefers-color-scheme 在 Daylight/Obsidian 间切换，UI 规格 §3.1）。 */
export const THEME_SETTING_KEY = "ui.theme";

export const themeId = writable<string>("obsidian");

const mql =
  typeof window !== "undefined" && typeof window.matchMedia === "function"
    ? window.matchMedia("(prefers-color-scheme: light)")
    : null;

/** auto → 跟随系统明暗；jsdom / 无 matchMedia 环境按暗色（Obsidian）回落。 */
export function resolveThemeId(preference: string): string {
  if (preference !== "auto") return preference;
  return mql?.matches ? "daylight" : "obsidian";
}

function apply(preference: string): void {
  const resolved = resolveThemeId(preference);
  document.documentElement.dataset.theme = resolved;
  document.documentElement.style.colorScheme = getTheme(resolved)?.dark ? "dark" : "light";
}

/** 启动时调用：载入持久化偏好并应用；settings 命令未注册（Task 20 前）时回落默认。 */
export async function initTheme(): Promise<void> {
  const pref = await settingGet<string>(THEME_SETTING_KEY, "obsidian");
  themeId.set(pref);
  apply(pref);
  mql?.addEventListener("change", () => {
    if (get(themeId) === "auto") apply("auto");
  });
}

/** 切换主题：即时生效（无刷新）+ 经 settings_set 持久化。 */
export async function setTheme(preference: string): Promise<void> {
  if (await settingSet(THEME_SETTING_KEY, preference) === false) return;
  themeId.set(preference);
  apply(preference);
}

/* ---------- 三级配色作用域解析（R11 裁决；Task 18 TerminalPane 消费） ---------- */

/**
 * 终端主题设置（三级作用域合并目标）：对应全局 settings 键 `term.scheme`/`term.fontSize`（UI 规格 §3.3）。
 * Profile 级覆盖存于 profile 记录 settings JSON（term_blob）的 `theme_override` 键——不动 Task 6 schema（R11）；
 * 读写路径由 Task 20 装配层接线（LL2b），本模块只做纯合并。
 */
export interface TermThemeSettings {
  /** 终端配色方案 id（term-schemes 12 套；非法 id 由消费侧 resolveScheme 回落默认） */
  scheme: string;
  /** 字号 px（默认 13，clamp 8–32 由终端侧执行） */
  fontSize: number;
}

/** 字段覆盖（Partial 浅层）：profile 层 = theme_override JSON；transient 层 = 会话内存态（R16 收进 M1：tabs.ts transientSchemes store，标签右键菜单写入） */
export type TermThemeOverride = Partial<TermThemeSettings>;

/** 三级解析结果（完整设置） */
export type ResolvedTheme = TermThemeSettings;

/**
 * 三级配色作用域解析（R11）：合并顺序 transient ＞ profile ＞ global，字段级浅合并。
 * 高层显式 undefined 不覆盖低层有效值；三级全接线（R16 将 transient 收进 M1）：transient = 会话内存态，
 * 标签右键「配色方案 ▾」菜单经 tabs.ts `transientSchemes` store + 装配层 `tempSchemeId` prop 接线（不写库、关标签即释放），单测保留。
 */
export function resolveTheme(input: {
  global: TermThemeSettings;
  profile?: TermThemeOverride | null;
  transient?: TermThemeOverride | null;
}): ResolvedTheme {
  const out: ResolvedTheme = { ...input.global };
  for (const layer of [input.profile, input.transient]) {
    if (!layer) continue;
    for (const [k, v] of Object.entries(layer)) {
      if (v !== undefined) (out as unknown as Record<string, unknown>)[k] = v; // S245：TS2352 要求先经 unknown 中转（无索引签名接口不得直转 Record）
    }
  }
  return out;
}
