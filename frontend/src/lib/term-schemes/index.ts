import type { ITheme } from "@xterm/xterm";

/** 终端配色方案（UI 规格 §3.2 JSON 格式） */
export interface Scheme {
  id: string;
  name: string;
  dark: boolean;
  background: string;
  foreground: string;
  cursor: string;
  selection: string;
  /** 16 ANSI 色，顺序 black/red/green/yellow/blue/magenta/cyan/white + bright* */
  ansi: string[];
  /** 粗体是否用亮色 → xterm.js drawBoldTextInBrightColors */
  boldBright: boolean;
}

/** ansi[16] → xterm.js ITheme 命名字段的顺序映射（已核实 6.0.0 ITheme 无 ansi 数组字段） */
const ANSI_KEYS = [
  "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
  "brightBlack", "brightRed", "brightGreen", "brightYellow",
  "brightBlue", "brightMagenta", "brightCyan", "brightWhite",
] as const;

const COLOR_KEYS = ["background", "foreground", "cursor", "selection"] as const;

const HEX6 = /^#[0-9a-f]{6}$/;

/** schema 校验：字段类型 + 小写六位 hex + ansi 恰 16 色（红测要求拒绝残缺/非法数据） */
export function validateScheme(v: unknown): v is Scheme {
  if (typeof v !== "object" || v === null) return false;
  const s = v as Record<string, unknown>;
  if (typeof s.id !== "string" || s.id.length === 0) return false;
  if (typeof s.name !== "string") return false;
  if (typeof s.dark !== "boolean") return false;
  if (typeof s.boldBright !== "boolean") return false;
  for (const k of COLOR_KEYS) {
    const c = s[k];
    if (typeof c !== "string" || !HEX6.test(c)) return false;
  }
  if (!Array.isArray(s.ansi) || s.ansi.length !== 16) return false;
  return s.ansi.every((c: unknown) => typeof c === "string" && HEX6.test(c));
}

// 编译期 eager 加载同目录 12 套内置方案 JSON（UI 规格 §3.2 清单）
const modules = import.meta.glob<{ default: Scheme }>("./*.json", { eager: true });

export const SCHEMES: Scheme[] = Object.values(modules).map((m) => m.default);

export function getScheme(id: string): Scheme | undefined {
  return SCHEMES.find((s) => s.id === id);
}

/** 默认方案 = FutureShell Dark（与 Obsidian 主题配套，UI 规格 §3.2） */
export const DEFAULT_SCHEME: Scheme = (() => {
  const s = getScheme("futureshell-dark");
  if (!s) throw new Error("内置默认配色方案 futureshell-dark.json 缺失");
  return s;
})();

/**
 * 方案 → xterm.js `theme` 选项（UI 规格 §3.2 映射表）：
 * background/foreground/cursor/selection → theme.{background,foreground,cursor,selectionBackground}；
 * ansi[16] → 命名字段 black..brightWhite。boldBright 由调用方写 drawBoldTextInBrightColors。
 */
export function schemeToXtermTheme(s: Scheme): ITheme {
  const theme: ITheme = {
    background: s.background,
    foreground: s.foreground,
    cursor: s.cursor,
    selectionBackground: s.selection,
  };
  s.ansi.forEach((color, i) => {
    (theme as Record<string, string>)[ANSI_KEYS[i]] = color;
  });
  return theme;
}

/**
 * 三级配色作用域优先级（UI 规格 §3.2）：
 * 临时（标签右键菜单，不写库、关标签即释放）> Profile > 全局（settings term.scheme）> 默认。
 */
export function resolveScheme(
  global: Scheme | null,
  profile: Scheme | null = null,
  temp: Scheme | null = null,
): Scheme {
  return temp ?? profile ?? global ?? DEFAULT_SCHEME;
}

/* ---------- 自定义/导入的配色（M4b） ---------- */

/**
 * 自定义配色的 id 前缀。
 *
 * 用前缀而不是让用户自取 id，是为了让「这是内置的还是导入的」在**任何**只拿得到 id 的
 * 地方都能判断——`term.scheme` 存的是一个字符串，后端校验、Profile 覆盖、标签临时覆盖
 * 三处都只见得到它。没有前缀的话，一个名叫 `nord` 的导入配色会与内置的 Nord 撞 id，
 * 而撞了之后谁赢取决于查找顺序，这种东西不该由查找顺序决定。
 */
export const CUSTOM_SCHEME_PREFIX = "custom:";

/** 导入/自定义配色的存储形状（与 Rust `ImportedScheme` 对应，见 lib/foreign-import.ts）。 */
export interface StoredCustomScheme {
  name: string;
  foreground: string;
  background: string;
  ansi: string[];
  cursor?: string | null;
}

/** 自定义配色的 id：`custom:` + 名字。名字是它的主键——同名不共存（导入侧跳过重名）。 */
export function customSchemeId(name: string): string {
  return CUSTOM_SCHEME_PREFIX + name;
}

export function isCustomSchemeId(id: string): boolean {
  return id.startsWith(CUSTOM_SCHEME_PREFIX);
}

/**
 * 判断一套配色是深色还是浅色，用来填 `Scheme.dark`。
 *
 * 导入来的格式（`.xcs` / `.itermcolors`）里没有这个信息，而它不是可有可无的：
 * 主题联动与「粗体用亮色」都读它。按背景色的感知亮度判断——用 ITU-R BT.601 的
 * 权重而非算术平均，因为人眼对绿色远比对蓝色敏感，算术平均会把深蓝背景判成浅色。
 */
function looksDark(background: string): boolean {
  const m = /^#([0-9a-f]{2})([0-9a-f]{2})([0-9a-f]{2})$/i.exec(background);
  if (!m) return true; // 认不出就按深色——本产品的默认是深色，猜错的代价更小
  const [r, g, b] = [1, 2, 3].map((i) => parseInt(m[i], 16));
  return 0.299 * r + 0.587 * g + 0.114 * b < 128;
}

/**
 * 存储形状 → 运行期 `Scheme`。
 *
 * 补齐三个导入格式里没有的字段：
 * - `cursor` 缺省时用前景色（终端的常规做法，且保证光标一定可见）；
 * - `selection` 一律由背景色推导（见下），不从文件里取——`.xcs` 与 `.itermcolors`
 *   对选区色的定义互不兼容，取过来的值经常与本程序的渲染方式不搭；
 * - `boldBright` 取 true，与 12 套内置方案的多数一致。
 */
export function customToScheme(c: StoredCustomScheme): Scheme {
  const dark = looksDark(c.background);
  return {
    id: customSchemeId(c.name),
    name: c.name,
    dark,
    background: c.background,
    foreground: c.foreground,
    cursor: c.cursor || c.foreground,
    // 选区色：深色底提亮、浅色底压暗，固定 25% 混向前景。写死一个半透明值
    // （如 rgba(255,255,255,.2)）在浅色配色上会几乎看不见。
    selection: mix(c.background, c.foreground, 0.25),
    ansi: [...c.ansi],
    boldBright: true,
  };
}

/** 两个 `#rrggbb` 按比例混合。`t=0` 全取 a，`t=1` 全取 b。 */
function mix(a: string, b: string, t: number): string {
  const parse = (s: string): [number, number, number] => {
    const m = /^#([0-9a-f]{2})([0-9a-f]{2})([0-9a-f]{2})$/i.exec(s);
    return m ? [1, 2, 3].map((i) => parseInt(m[i], 16)) as [number, number, number] : [0, 0, 0];
  };
  const [ar, ag, ab] = parse(a);
  const [br, bg, bb] = parse(b);
  const ch = (x: number, y: number) =>
    Math.round(x + (y - x) * t).toString(16).padStart(2, "0");
  return `#${ch(ar, br)}${ch(ag, bg)}${ch(ab, bb)}`;
}

/**
 * 在内置 + 自定义里按 id 找。
 *
 * 内置**优先**：万一某天有人手工往库里塞了一条 id 与内置撞车的记录，程序仍按内置走，
 * 而不是让库里的数据静默改掉一套随版本发布的配色。
 */
export function getSchemeFrom(id: string, custom: StoredCustomScheme[]): Scheme | undefined {
  const builtin = getScheme(id);
  if (builtin) return builtin;
  if (!isCustomSchemeId(id)) return undefined;
  const name = id.slice(CUSTOM_SCHEME_PREFIX.length);
  const c = custom.find((x) => x.name === name);
  return c ? customToScheme(c) : undefined;
}

/**
 * 从 settings 读来的原始值里挑出**形状合法**的自定义配色。
 *
 * 后端 `term.customSchemes` 的校验已经很严，但这个键也可能被手工改过库，
 * 而这里的产物直接进 xterm 的 theme 对象。宁可少显示一套，不要让一套坏数据
 * 把整个终端渲染搞崩——那种崩法（白屏/无字）用户完全无从判断原因。
 */
export function parseCustomSchemes(raw: unknown): StoredCustomScheme[] {
  if (!Array.isArray(raw)) return [];
  return raw.filter((x): x is StoredCustomScheme => {
    if (typeof x !== "object" || x === null) return false;
    const s = x as Record<string, unknown>;
    if (typeof s.name !== "string" || s.name.length === 0) return false;
    if (typeof s.foreground !== "string" || !HEX6.test(s.foreground)) return false;
    if (typeof s.background !== "string" || !HEX6.test(s.background)) return false;
    if (!Array.isArray(s.ansi) || s.ansi.length !== 16) return false;
    if (!s.ansi.every((c) => typeof c === "string" && HEX6.test(c))) return false;
    if (s.cursor != null && (typeof s.cursor !== "string" || !HEX6.test(s.cursor))) return false;
    return true;
  });
}
