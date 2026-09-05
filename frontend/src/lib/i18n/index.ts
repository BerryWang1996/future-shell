/**
 * 界面语言（M4b「语言切换」出口，UI 规格 §2.12）。
 *
 * # 范围
 *
 * 出口原文给的是「首发简体中文 + 英文双语**或仅搭框架**二选一」。这里做的是框架 +
 * **菜单栏一整片真实双语**：框架完整（store / 插值 / 回落 / 缺键守卫 / 持久化），
 * 词典目前覆盖菜单栏与语言设置项本身。其余界面文案仍是中文硬编码——
 * 这不是「快做完了」，是**明确的边界**，路线图那条出口里逐字写着。
 *
 * 选菜单栏而不是别处，因为它是切换语言后**一眼就能看出生没生效**的那一片；
 * 而「生没生效」正是这条出口要验的东西。
 *
 * # 为什么 label 不再直接写在 menus.ts 里
 *
 * 原来 `MENUS` 是模块级常量，label 是中文字面量。要双语就必须让它随语言变，于是
 * `menus.ts` 改成 derived store（见该文件）。这带来一个真实的好处：**菜单里不可能
 * 再出现漏翻的硬编码中文**——`i18n.test.ts` 有一条扫源码的守卫盯着它。
 */
import { derived, get, writable, type Readable } from "svelte/store";
import { settingGet, settingSet } from "../ipc";
import { ZH_CN } from "./zh-CN";
import { EN } from "./en";

export type Locale = "zh-CN" | "en";

/** settings 键。 */
export const LANGUAGE_SETTING_KEY = "ui.language";

/**
 * 回落语言。**简体中文**——本产品的第一语言，词典最全。
 *
 * 回落方向很重要：英文词典漏了一个键时回落到中文（用户看到一句中文），
 * 反过来会让中文用户看到英文。前者是「有一处没翻译」，后者是「界面坏了」。
 */
export const DEFAULT_LOCALE: Locale = "zh-CN";

/**
 * 可选语言。
 *
 * label 一律用**该语言自己的名字**（简体中文 / English），不随界面语言翻译。
 * 这是 i18n 的通行做法，理由很实际：一个人误把界面切成了看不懂的语言，
 * 他要能在下拉框里认出自己的母语才切得回来。若这一栏也跟着翻译，
 * 切到日文之后「简体中文」会显示成日文——他就只能靠位置猜了。
 */
export const SUPPORTED_LOCALES: readonly { id: Locale; label: string }[] = [
  { id: "zh-CN", label: "简体中文" },
  { id: "en", label: "English" },
] as const;

export const DICTIONARIES: Record<Locale, Record<string, string>> = {
  "zh-CN": ZH_CN,
  en: EN,
};

/** 当前界面语言。切换即时生效，不需要重启。 */
export const locale = writable<Locale>(DEFAULT_LOCALE);

/** 传入的字符串是不是我们支持的语言。库里存了别的值时按默认走，而不是让界面空掉。 */
export function isLocale(v: unknown): v is Locale {
  return typeof v === "string" && SUPPORTED_LOCALES.some((l) => l.id === v);
}

/**
 * 查词 + 插值。
 *
 * 三级：当前语言 → 回落语言 → **键名本身**。最后一级不返回空串是刻意的：
 * 空串会让按钮变成一个没有文字的方块，用户完全无从判断出了什么事；
 * 露出 `menu.tools.replay` 至少能让人（和 bug 报告）指出是哪一条漏了。
 *
 * 插值用 `{name}` 占位。缺参数时保留占位符原样——同理，宁可显示 `{phase}`
 * 也不要显示 `undefined` 或一个语义被悄悄改掉的句子。
 */
export function translate(
  l: Locale,
  key: string,
  vars?: Record<string, string | number>,
): string {
  const raw = DICTIONARIES[l]?.[key] ?? DICTIONARIES[DEFAULT_LOCALE]?.[key] ?? key;
  if (!vars) return raw;
  return raw.replace(/\{(\w+)\}/g, (m, name: string) =>
    name in vars ? String(vars[name]) : m,
  );
}

/**
 * 响应式翻译函数：组件里写 `{$t("menu.file")}`。
 *
 * 是 derived store 而不是普通函数，语言一变所有用到它的地方自动重渲——
 * 否则「切换语言生效」就要靠重启，而出口写的是「切换生效 round-trip」。
 */
export const t: Readable<(key: string, vars?: Record<string, string | number>) => string> =
  derived(locale, ($l) => (key: string, vars?: Record<string, string | number>) =>
    translate($l, key, vars),
  );

/** 非响应式场合（纯函数、测试）用的即时翻译。 */
export function tNow(key: string, vars?: Record<string, string | number>): string {
  return translate(get(locale), key, vars);
}

/** 启动时载入持久化的语言偏好。库里是脏值时回落默认，不抛。 */
export async function initLocale(): Promise<void> {
  const saved = await settingGet<string>(LANGUAGE_SETTING_KEY, DEFAULT_LOCALE);
  locale.set(isLocale(saved) ? saved : DEFAULT_LOCALE);
}

/**
 * 切换语言：即时生效 + 落库。
 *
 * 先落库再改 store：写库失败时语言不变，界面显示的与库里存的保持一致。
 * 反过来（先改 store）会让用户看到语言变了，重启后又变回去——而他不会知道为什么。
 */
export async function setLocale(next: Locale): Promise<void> {
  if (!isLocale(next)) return;
  if (await settingSet(LANGUAGE_SETTING_KEY, next) === false) throw new Error("语言未保存，请重试。");
  locale.set(next);
}
