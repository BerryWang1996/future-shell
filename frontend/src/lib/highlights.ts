/**
 * 高亮关键字集（M4a，路线图 §M4 / UI 规格 §2.11）。
 *
 * 规则表（settings 键 `term.highlights`）逐条描述「命中什么 → 涂什么色 → 是否提醒」。
 * 输出流里命中即着色；带提醒的规则命中时联动标签角标（与 bell 同一通道）。
 *
 * ## 为什么匹配在**剥离后的文本**上做
 *
 * 会话字节里混着 ANSI（`\x1b[0;32mERROR\x1b[0m`）。在原始字节上匹配 `ERROR` 会
 * 漏掉被序列切开的情形，也会把序列里的字母当正文（`[0;32m` 里的 `m`）。故匹配
 * 走与会话日志同一个流式剥离器口径的纯文本，且**按行**——高亮的语义单位是行
 * （用户想的是「把含 ERROR 的那一行标红」），而行会跨读块，故需要行缓冲。
 *
 * ## 正则的两条边界
 *
 * ① **必须可失败编译**：用户写的正则可以是 `(` 这种非法式。编译失败的规则**整条
 *    禁用**并在 UI 上标错，不静默降级成字面量匹配——降级会让规则「看起来生效了」
 *    却匹配到完全不同的东西。
 * ② **必须挡住灾难性回溯**：`(a+)+$` 之类式子在 JS 正则上是指数级的，而 JS 正则**没有
 *    超时机制**，一条这样的规则就能把 UI 线程冻住。
 *
 *    这里有两道**尺寸**闸（规则长度上限 + 匹配前把行截断到 MAX_LINE_SCAN）与一道
 *    **形状**闸（[`hasNestedQuantifier`] 静态拒收嵌套量词）。
 *
 *    尺寸闸挡不住回溯——这一点曾被本文件的注释说反过，实测纠正如下（M4a.1 T94）：
 *    `(a+)+$` 在**30 个字符**的行上就要跑 10.5 秒，远在 8 KiB 截断之下。指数曲线在
 *    输入很短时就已经爆了，「把行截短」这个方向根本不起作用。
 *
 *    故真正生效的是形状闸：含嵌套量词的规则**整条禁用**并标错，压根不进匹配路径。
 *    它是静态判定，必然有假阴（构造得刁钻的式子仍可能漏过）——**真正的耗时上界**
 *    需要把扫描搬出 UI 线程（Worker）或换用线性时间引擎，那是独立一项工作，
 *    尚未做，不在此假称已解决。
 */

/** 一条高亮规则。 */
export interface HighlightRule {
  id: string;
  /** 规则名（UI 列表显示；空则显示 pattern） */
  name: string;
  /** 匹配式：literal = 字面量子串（大小写不敏感），regex = JS 正则源 */
  pattern: string;
  kind: "literal" | "regex";
  /** 命中行的着色（CSS 颜色串；前端直接塞进装饰样式） */
  color: string;
  /** 命中时是否联动提醒角标（UI 规格 §2.11 的「提醒角标联动」） */
  alert: boolean;
  /** 关闭而不删除（临时禁用一条规则不必先记下它的正则） */
  enabled: boolean;
}

/** 单行扫描上限（见模块头②）。8 KiB 远超任何有阅读价值的单行。 */
export const MAX_LINE_SCAN = 8 * 1024;
/** 单条正则源长度上限（长式子既难维护又是回溯灾难的温床）。 */
export const MAX_PATTERN_CHARS = 200;
/** 规则条数上限（settings 值 8 KiB 总闸之内的产品边界）。 */
export const MAX_RULES = 64;

/**
 * 嵌套量词的静态检测（灾难性回溯的形状闸，M4a.1 T94）。
 *
 * 判的是「一个带量词的**分组**又被套了一层量词」这一形状：`(a+)+`、`(a*)*`、`(x+)?`+…
 * 这类式子在回溯引擎上是指数级的。实测 `(a+)+$` 在 30 字符的行上跑 10.5 秒——
 * 尺寸闸（8 KiB 截断）在这个量级上完全无效。
 *
 * 判定刻意保守（宁可放过也不误伤）：只在**同一个分组**的紧邻位置同时看到内外量词才拒收。
 * - 会拒：`(a+)+`、`(a*)*`、`(a+)*`、`([0-9]+)+`、`(?:a+)+`
 * - 不拒：`a+b+`（无嵌套）、`(abc)+`（分组内无量词）、`(a|b)+`（分组内无量词）
 *
 * 已知假阴：跨分组的间接嵌套（如 `((a+)b?)+`）与回溯放大的其他形态（如
 * `(a|a)+`）不在此列。这不是「已解决灾难性回溯」，只是把最常见、最容易被随手写出来的
 * 那一类挡在门外；真正的耗时上界见模块头②的说明。
 */
export function hasNestedQuantifier(pattern: string): boolean {
  // 逐字符扫，跳过转义与字符类内部（`[(+)]` 里的括号与加号都是字面量）
  let i = 0;
  let inClass = false;
  // 每个未闭合分组：记「组内是否出现过量词」
  const stack: { sawQuantifier: boolean }[] = [];
  while (i < pattern.length) {
    const c = pattern[i];
    if (c === "\\") {
      i += 2; // 转义对，整体跳过
      continue;
    }
    if (inClass) {
      if (c === "]") inClass = false;
      i += 1;
      continue;
    }
    if (c === "[") {
      inClass = true;
      i += 1;
      continue;
    }
    if (c === "(") {
      stack.push({ sawQuantifier: false });
      i += 1;
      continue;
    }
    if (c === ")") {
      const group = stack.pop();
      // 看紧跟其后的量词：`*` `+` `?` `{n,m}`
      const next = pattern[i + 1];
      const quantified =
        next === "*" ||
        next === "+" ||
        next === "?" ||
        (next === "{" && /^\{\d+(,\d*)?\}/.test(pattern.slice(i + 1)));
      if (group?.sawQuantifier && quantified) return true;
      i += 1;
      continue;
    }
    if (c === "*" || c === "+" || (c === "{" && /^\{\d+(,\d*)?\}/.test(pattern.slice(i)))) {
      // 量词记在**当前所在分组**上；`?` 不记——它既可能是量词也可能是
      // `(?:` / 惰性修饰符的一部分，且 `(x?)?` 的回溯代价是线性的。
      if (stack.length > 0) stack[stack.length - 1].sawQuantifier = true;
    }
    i += 1;
  }
  return false;
}

/** 编译后的规则：正则已构造好，或带编译错误。 */
export interface CompiledRule {
  rule: HighlightRule;
  /** kind=regex 且编译成功时为 RegExp；literal 规则为 null */
  re: RegExp | null;
  /** 非空 = 本条**整条禁用**的原因（编译失败/超长），UI 需显式标错 */
  error: string | null;
}

/**
 * 编译规则表。**不抛异常**：坏规则带着 error 留在结果里——UI 要能把
 * 「第 3 条正则写错了」指给用户，而不是整表静默失效或整个面板崩掉。
 */
export function compileRules(rules: HighlightRule[]): CompiledRule[] {
  return rules.slice(0, MAX_RULES).map((rule) => {
    if (!rule.enabled) return { rule, re: null, error: null };
    if (rule.pattern.length === 0) {
      return { rule, re: null, error: "匹配式为空" };
    }
    if (rule.pattern.length > MAX_PATTERN_CHARS) {
      return { rule, re: null, error: `匹配式超过 ${MAX_PATTERN_CHARS} 字符上限` };
    }
    if (rule.kind === "literal") return { rule, re: null, error: null };
    // 形状闸（M4a.1 T94）：嵌套量词整条禁用。必须排在 `new RegExp` **之前**——
    // 这种式子语法完全合法，编译得过，代价全在匹配时（实测 30 字符行 10.5 秒）。
    if (hasNestedQuantifier(rule.pattern)) {
      return {
        rule,
        re: null,
        error:
          "含嵌套量词（如 (a+)+），在回溯引擎上是指数级耗时，会冻住终端界面——本条已禁用。" +
          "请改写成等价的非嵌套形式（例如 (a+)+ → a+）。",
      };
    }
    try {
      // 'i' 与 'g'：大小写不敏感与全局（一行内多处命中都要着色）。
      // 不给 'm'——匹配以行为单位，`^`/`$` 应指整行首尾。
      return { rule, re: new RegExp(rule.pattern, "gi"), error: null };
    } catch (e) {
      return { rule, re: null, error: `正则非法：${e instanceof Error ? e.message : String(e)}` };
    }
  });
}

/** 一次命中（行内区间 + 生效的规则）。 */
export interface HighlightHit {
  /** 行内起始/结束偏移（以 UTF-16 码元计——与 xterm 的列号口径一致） */
  start: number;
  end: number;
  rule: HighlightRule;
}

/**
 * 扫一行，返回全部命中（按 start 升序；重叠命中保留**先声明**的那条——
 * 规则表有序，先声明者优先是可预期的行为，比「后者覆盖」更符合用户排规则的直觉）。
 */
export function scanLine(compiled: CompiledRule[], line: string): HighlightHit[] {
  const text = line.length > MAX_LINE_SCAN ? line.slice(0, MAX_LINE_SCAN) : line;
  const hits: HighlightHit[] = [];
  for (const c of compiled) {
    if (!c.rule.enabled || c.error) continue;
    if (c.rule.kind === "literal") {
      const needle = c.rule.pattern.toLowerCase();
      const hay = text.toLowerCase();
      let from = 0;
      for (;;) {
        const i = hay.indexOf(needle, from);
        if (i < 0) break;
        hits.push({ start: i, end: i + needle.length, rule: c.rule });
        from = i + Math.max(1, needle.length);
      }
    } else if (c.re) {
      c.re.lastIndex = 0;
      let m: RegExpExecArray | null;
      while ((m = c.re.exec(text))) {
        // 零宽匹配（如 `a*` 对空串）会让 exec 原地打转：手动推进并跳过
        if (m[0].length === 0) {
          c.re.lastIndex += 1;
          continue;
        }
        hits.push({ start: m.index, end: m.index + m[0].length, rule: c.rule });
      }
    }
  }
  hits.sort((a, b) => a.start - b.start);
  // 去重叠：保留先声明的规则（compiled 顺序即声明序）
  const order = new Map(compiled.map((c, i) => [c.rule.id, i]));
  const kept: HighlightHit[] = [];
  for (const h of hits) {
    const clash = kept.find((k) => h.start < k.end && k.start < h.end);
    if (!clash) {
      kept.push(h);
      continue;
    }
    if ((order.get(h.rule.id) ?? 0) < (order.get(clash.rule.id) ?? 0)) {
      kept[kept.indexOf(clash)] = h;
    }
  }
  return kept.sort((a, b) => a.start - b.start);
}

/** 本行是否命中了任何**带提醒**的规则（角标联动判据）。 */
export function shouldAlert(hits: HighlightHit[]): boolean {
  return hits.some((h) => h.rule.alert);
}

/** 解析规则表（settings JSON）。烂值回落空表——高亮坏了不该让终端不可用。 */
export function parseHighlightRules(json: string | null | undefined): HighlightRule[] {
  if (!json) return [];
  try {
    const v = JSON.parse(json);
    if (!Array.isArray(v)) return [];
    return v
      .filter(
        (x): x is HighlightRule =>
          x !== null &&
          typeof x === "object" &&
          typeof x.id === "string" &&
          typeof x.pattern === "string" &&
          (x.kind === "literal" || x.kind === "regex"),
      )
      .slice(0, MAX_RULES)
      .map((x) => ({
        id: x.id,
        name: typeof x.name === "string" ? x.name : "",
        pattern: x.pattern,
        kind: x.kind,
        color: typeof x.color === "string" && x.color ? x.color : "#e5c07b",
        alert: x.alert === true,
        enabled: x.enabled !== false, // 缺省视为启用
      }));
  } catch {
    return [];
  }
}

/** 新规则的 id（与 quick-commands 同款生成口径）。 */
export function newRuleId(): string {
  if (typeof crypto !== "undefined" && "randomUUID" in crypto) return crypto.randomUUID();
  return `hl-${Date.now()}-${Math.floor(Math.random() * 1e6)}`;
}
