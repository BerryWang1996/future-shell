import { describe, it, expect, beforeEach, vi } from "vitest";
import { get } from "svelte/store";

/** settings 桩：语言的持久化要走真实的 settingSet 路径，且要能扮演后端拒绝。 */
const store = new Map<string, unknown>();
const writes: { key: string; value: unknown }[] = [];
let rejectWith: string | null = null;

vi.mock("../ipc", () => ({
  settingGet: async <T>(key: string, fallback: T): Promise<T> =>
    store.has(key) ? (store.get(key) as T) : fallback,
  settingSet: async (key: string, value: unknown): Promise<void> => {
    if (rejectWith) throw new Error(rejectWith);
    writes.push({ key, value });
    store.set(key, value);
  },
}));

import {
  DEFAULT_LOCALE,
  DICTIONARIES,
  LANGUAGE_SETTING_KEY,
  SUPPORTED_LOCALES,
  initLocale,
  isLocale,
  locale,
  setLocale,
  t,
  translate,
  type Locale,
} from "./index";
import { currentMenus } from "../menus";
import MENUS_SRC from "../menus.ts?raw";

beforeEach(() => {
  store.clear();
  writes.length = 0;
  rejectWith = null;
  locale.set(DEFAULT_LOCALE);
});

/* ─────────────────────────── 词典闭合 ─────────────────────────── */

describe("词典：两种语言的键集必须完全一致", () => {
  /**
   * 这是这一组里最重要的一条。缺一个键的后果按方向不同：
   *  · 英文缺 ⇒ 回落中文 ⇒ 英文界面里露出一句中文（难看，但还能用）；
   *  · 中文缺 ⇒ 回落也是中文（DEFAULT_LOCALE）⇒ 直接露出键名 `menu.tools.replay`。
   * 两种都不该发布，而人工比对两份七十多条的表是不可能可靠的。
   */
  it("双向包含：任一侧多出或缺少一个键都判红", () => {
    const zh = Object.keys(DICTIONARIES["zh-CN"]).sort();
    const en = Object.keys(DICTIONARIES.en).sort();
    const onlyZh = zh.filter((k) => !(k in DICTIONARIES.en));
    const onlyEn = en.filter((k) => !(k in DICTIONARIES["zh-CN"]));
    expect(onlyZh, "这些键只有中文有，英文界面会露出中文").toEqual([]);
    expect(onlyEn, "这些键只有英文有，是死词条").toEqual([]);
  });

  it("提取有效：词条数有下限（两份都空时上一条会假绿）", () => {
    // 空集是空集的子集——上一条在两份词典都被清空时会全绿。
    // 下限取 50：光菜单栏就有 6 个顶层 + 四十多个条目，低于这个数只可能是
    // 词典被清空或者 import 断了，而不是「这一版菜单项少了几条」。
    expect(Object.keys(DICTIONARIES["zh-CN"]).length).toBeGreaterThanOrEqual(50);
    expect(Object.keys(DICTIONARIES.en).length).toBeGreaterThanOrEqual(50);
  });

  it("没有空值：一个空串词条 = 界面上一个没有文字的按钮", () => {
    for (const [l, dict] of Object.entries(DICTIONARIES)) {
      const empty = Object.entries(dict)
        .filter(([, v]) => v.trim() === "")
        .map(([k]) => k);
      expect(empty, `${l} 里有空词条`).toEqual([]);
    }
  });

  it("插值占位符两侧一致（漏一个 {phase} 就是漏掉半句话）", () => {
    const placeholders = (s: string) => (s.match(/\{(\w+)\}/g) ?? []).sort();
    const mismatched: string[] = [];
    for (const k of Object.keys(DICTIONARIES["zh-CN"])) {
      const a = placeholders(DICTIONARIES["zh-CN"][k]);
      const b = placeholders(DICTIONARIES.en[k] ?? "");
      if (JSON.stringify(a) !== JSON.stringify(b)) mismatched.push(`${k}: ${a} vs ${b}`);
    }
    expect(mismatched).toEqual([]);
    // 反向对照：确实有带占位符的词条，否则这条什么也没测
    expect(placeholders(DICTIONARIES["zh-CN"]["menu.soon"])).toEqual(["{phase}"]);
  });

  it("英文词典里不应残留中文（漏翻的最常见形态是把中文抄过去）", () => {
    const cjk = /[一-鿿]/;
    const leftovers = Object.entries(DICTIONARIES.en)
      .filter(([, v]) => cjk.test(v))
      .map(([k]) => k);
    expect(leftovers).toEqual([]);
  });
});

/* ─────────────────────────── 查词与回落 ─────────────────────────── */

describe("查词、回落与插值", () => {
  it("缺键时露出键名，而不是空串", () => {
    // 空串会让按钮变成一个没有文字的方块——用户完全无从判断出了什么事，
    // 而露出 `menu.does.not.exist` 至少能让人（和 bug 报告）指出是哪一条。
    expect(translate("en", "menu.does.not.exist")).toBe("menu.does.not.exist");
  });

  it("英文缺键时回落中文，而不是回落成键名", () => {
    // 回落方向是刻意的：中文是 DEFAULT_LOCALE，词典最全。
    // 反过来（回落英文）会让中文用户在某个词条漏翻时看到英文——那更像「界面坏了」。
    expect(DEFAULT_LOCALE).toBe("zh-CN");
    const key = "menu.file";
    // 造一个只有中文有的场景：直接查一个英文词典里删掉的键不现实（守卫不允许），
    // 故用 translate 的第一参数扮演一种没有词典的语言。
    expect(translate("de" as Locale, key)).toBe(DICTIONARIES["zh-CN"][key]);
  });

  it("插值替换命名占位符", () => {
    expect(translate("zh-CN", "menu.soon", { phase: "M4b" })).toBe("即将推出（M4b）");
    expect(translate("en", "menu.soon", { phase: "M4b" })).toBe("Coming in M4b");
  });

  it("缺参数时保留占位符原样，不写 undefined", () => {
    // 显示 `{phase}` 是个明显的 bug 信号；显示「即将推出（undefined）」看起来
    // 像一句正常的话，只是内容错了——后者更难被发现。
    expect(translate("zh-CN", "menu.soon")).toContain("{phase}");
    expect(translate("zh-CN", "menu.soon", {})).toContain("{phase}");
  });

  it("$t 随语言变化重新求值（切换即时生效的机制本身）", () => {
    expect(get(t)("menu.file")).toBe("文件(F)");
    locale.set("en");
    expect(get(t)("menu.file")).toBe("File(F)");
  });
});

/* ─────────────────────────── 语言表 ─────────────────────────── */

describe("可选语言表", () => {
  it("每种可选语言都有一份词典（选得到却没词典 = 整个界面回落）", () => {
    for (const l of SUPPORTED_LOCALES) {
      expect(DICTIONARIES[l.id], `${l.id} 没有词典`).toBeTruthy();
    }
    // 反向：没有多余的词典（那说明有语言做好了却没上表，用户选不到）
    expect(Object.keys(DICTIONARIES).sort()).toEqual(SUPPORTED_LOCALES.map((l) => l.id).sort());
  });

  it("语言名用各自的语言写，不随界面语言翻译", () => {
    // 一个人误把界面切成看不懂的语言之后，要能在下拉框里认出母语才切得回来。
    // 若这一栏也翻译，切到英文后「简体中文」会显示成 "Simplified Chinese"——
    // 对一个只认得「简体中文」四个字的人来说，那就等于没有退路。
    expect(SUPPORTED_LOCALES.find((l) => l.id === "zh-CN")!.label).toBe("简体中文");
    expect(SUPPORTED_LOCALES.find((l) => l.id === "en")!.label).toBe("English");
    // 而且它们不是词典键——翻译它们的唯一办法是改这张表，那会立刻被上面两条判红
    for (const l of SUPPORTED_LOCALES) {
      expect(l.label in DICTIONARIES["zh-CN"]).toBe(false);
    }
  });

  it("isLocale 只认表上的值", () => {
    expect(isLocale("zh-CN")).toBe(true);
    expect(isLocale("en")).toBe(true);
    for (const bad of ["EN", "zh", "zh_CN", "ja", "", null, undefined, 42, {}]) {
      expect(isLocale(bad), `${String(bad)} 不该被认成语言`).toBe(false);
    }
  });
});

/* ─────────────────────────── 持久化 round-trip ─────────────────────────── */

describe("切换语言 round-trip（M4b 出口的判据）", () => {
  it("切换 → 落库 → 重启后仍是新语言", async () => {
    await setLocale("en");
    expect(get(locale)).toBe("en");
    expect(writes).toEqual([{ key: LANGUAGE_SETTING_KEY, value: "en" }]);

    // 「重启」：store 回到初值，库还在
    locale.set(DEFAULT_LOCALE);
    await initLocale();
    expect(get(locale)).toBe("en");
    expect(get(t)("menu.tools")).toBe("Tools(T)");
  });

  it("落库失败时语言不变（界面显示的与库里存的不许走散）", async () => {
    // 先改 store 再落库的写法会让用户看到语言变了、重启后又变回去，
    // 而他不会知道为什么。这条钉住顺序。
    rejectWith = "ui.language 取值非法：de";
    await expect(setLocale("en")).rejects.toThrow();
    expect(get(locale)).toBe(DEFAULT_LOCALE);
  });

  it("库里是脏值时回落默认，不让界面空掉", async () => {
    store.set(LANGUAGE_SETTING_KEY, "克林贡语");
    await initLocale();
    expect(get(locale)).toBe(DEFAULT_LOCALE);
  });

  it("表外的语言写不进去", async () => {
    await setLocale("ja" as Locale);
    expect(writes).toEqual([]);
    expect(get(locale)).toBe(DEFAULT_LOCALE);
  });
});

/* ─────────────────────────── 菜单栏双语 ─────────────────────────── */

describe("菜单栏：切换语言后整片都变", () => {
  /** 递归收集一棵菜单树里的全部 label。 */
  const labels = (): string[] => {
    const out: string[] = [];
    for (const m of currentMenus()) {
      out.push(m.label);
      for (const i of m.items) {
        out.push(i.label);
        if (i.note) out.push(i.note);
        for (const c of i.children ?? []) out.push(c.label);
      }
    }
    return out;
  };

  it("中文下没有一个 label 是漏掉的键名", () => {
    // 漏词条的表现就是 label 等于 `menu.xxx`。逐条扫比「抽查几个」可靠。
    const raw = labels().filter((l) => /^menu\./.test(l));
    expect(raw, "这些菜单项没有词条").toEqual([]);
    expect(labels().length).toBeGreaterThanOrEqual(50); // 提取有效性下限
  });

  it("英文下同样没有漏键，且不再有中文", () => {
    locale.set("en");
    const ls = labels();
    expect(ls.filter((l) => /^menu\./.test(l))).toEqual([]);
    // 主题名是专名（Obsidian / Daylight / Slate Blue / High Contrast），本就不含中文，
    // 所以这条可以对**全部** label 生效，不需要豁免名单。
    const cjk = /[一-鿿]/;
    expect(ls.filter((l) => cjk.test(l)), "英文界面里还有中文菜单项").toEqual([]);
  });

  it("主题名不翻译（专名跟着语言变会对不上文档与截图）", () => {
    const themeChildren = () =>
      currentMenus()
        .find((m) => m.id === "view")!
        .items.find((i) => i.id === "view.theme")!
        .children!.filter((c) => c.id !== "theme.auto")
        .map((c) => c.label);
    const zh = themeChildren();
    locale.set("en");
    expect(themeChildren()).toEqual(zh);
    expect(zh).toContain("Obsidian");
  });

  it("「即将推出」的相位代号不翻译，两种语言下都认得出 M4b", () => {
    const noteOf = (menu: string, id: string) =>
      currentMenus().find((m) => m.id === menu)!.items.find((i) => i.id === id)!.note!;
    expect(noteOf("window", "window.sessionList")).toContain("M4b");
    locale.set("en");
    expect(noteOf("window", "window.sessionList")).toContain("M4b");
    expect(noteOf("window", "window.sessionList")).toMatch(/^Coming in /);
  });

  /**
   * 结构性守卫：**menus.ts 里不许出现中文字面量**。
   *
   * 上面几条是行为断言，它们只覆盖当下这几十条。这一条覆盖的是**下一条**——
   * 谁往菜单里加一项时顺手写了个中文 label，这里立刻红，而不是等到有人
   * 把界面切成英文才发现那一条没翻。
   */
  it("menus.ts 源码里没有中文字面量（新加的菜单项必须走词典）", () => {
    // 剥注释：解释性注释里必然有中文，扫全文等于恒红。
    const code = MENUS_SRC.replace(/\/\*[\s\S]*?\*\//g, "").replace(/(^|[^:])\/\/.*$/gm, "$1");
    const cjk = /[一-鿿]/;
    const offenders = code
      .split("\n")
      .map((line, i) => ({ line, n: i + 1 }))
      .filter(({ line }) => cjk.test(line));
    expect(
      offenders.map((o) => `${o.n}: ${o.line.trim()}`),
      "菜单结构里出现了中文字面量，应改为词典键",
    ).toEqual([]);
    // 反向自检：剥注释这一步不能把整个文件吃掉
    expect(code).toContain("menu.soon");
    expect(code.length).toBeGreaterThan(1000);
  });
});
