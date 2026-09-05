/**
 * S319–S321：高亮关键字集的纯逻辑判据（M4a 出口标准「CRUD UI 测试 +
 * 正则/字面量双路单测」的单测一半）。
 */
import { describe, expect, it } from "vitest";
import {
  MAX_LINE_SCAN,
  MAX_PATTERN_CHARS,
  compileRules,
  hasNestedQuantifier,
  parseHighlightRules,
  scanLine,
  shouldAlert,
  type HighlightRule,
} from "./highlights";

function rule(over: Partial<HighlightRule> = {}): HighlightRule {
  return {
    id: over.id ?? "r1",
    name: over.name ?? "",
    pattern: over.pattern ?? "ERROR",
    kind: over.kind ?? "literal",
    color: over.color ?? "#ff0000",
    alert: over.alert ?? false,
    enabled: over.enabled ?? true,
    ...over,
  };
}

describe("compileRules（S319：坏规则整条禁用且带原因）", () => {
  it("字面量规则无需正则；合法正则编译出 RegExp", () => {
    const [lit, re] = compileRules([rule(), rule({ id: "r2", kind: "regex", pattern: "\\d+" })]);
    expect(lit.re).toBeNull();
    expect(lit.error).toBeNull();
    expect(re.re).toBeInstanceOf(RegExp);
    expect(re.error).toBeNull();
  });

  it("非法正则**整条禁用**并带错因——不静默降级成字面量（降级会让规则看起来生效却匹配别的东西）", () => {
    const [c] = compileRules([rule({ kind: "regex", pattern: "(" })]);
    expect(c.re).toBeNull();
    expect(c.error).toContain("正则非法");
  });

  it("空匹配式与超长匹配式禁用（超长既难维护又是回溯灾难温床）", () => {
    expect(compileRules([rule({ pattern: "" })])[0].error).toContain("为空");
    const long = "a".repeat(MAX_PATTERN_CHARS + 1);
    expect(compileRules([rule({ kind: "regex", pattern: long })])[0].error).toContain("上限");
  });

  it("禁用的规则不编译也不报错（临时关一条不必先删正则）", () => {
    const [c] = compileRules([rule({ enabled: false, kind: "regex", pattern: "(" })]);
    expect(c.error).toBeNull();
    expect(c.re).toBeNull();
  });
});

describe("scanLine（S320：字面量/正则双路）", () => {
  it("字面量大小写不敏感、一行多处全命中", () => {
    const c = compileRules([rule({ pattern: "error" })]);
    const hits = scanLine(c, "ERROR: first; error: second");
    expect(hits.map((h) => [h.start, h.end])).toEqual([
      [0, 5],
      [14, 19],
    ]);
  });

  it("正则全局命中且大小写不敏感", () => {
    const c = compileRules([rule({ kind: "regex", pattern: "warn\\w*" })]);
    const hits = scanLine(c, "WARNING and warned");
    expect(hits.map((h) => h.end - h.start)).toEqual([7, 6]);
  });

  it("零宽匹配不死循环（`a*` 对任意行都能匹配空串）", () => {
    const c = compileRules([rule({ kind: "regex", pattern: "a*" })]);
    const hits = scanLine(c, "bbaab");
    // 只留非零宽命中；关键是**返回了**（不挂死）
    expect(hits.every((h) => h.end > h.start)).toBe(true);
  });

  it("重叠命中保留先声明的规则（规则表有序，先声明优先符合排规则的直觉）", () => {
    const c = compileRules([
      rule({ id: "first", pattern: "ERROR" }),
      rule({ id: "second", pattern: "ERR" }),
    ]);
    const hits = scanLine(c, "ERROR");
    expect(hits).toHaveLength(1);
    expect(hits[0].rule.id).toBe("first");
  });

  it("超长行截断到 MAX_LINE_SCAN——超长行里的高亮无阅读价值，挂死终端有代价", () => {
    const c = compileRules([rule({ pattern: "needle" })]);
    const line = "x".repeat(MAX_LINE_SCAN + 100) + "needle";
    expect(scanLine(c, line)).toEqual([]); // 截断后不含 needle
    const near = "y".repeat(MAX_LINE_SCAN - 10) + "needle";
    expect(scanLine(c, near).length).toBe(1);
  });

  it("禁用与编译失败的规则不参与扫描", () => {
    const c = compileRules([
      rule({ id: "off", pattern: "ERROR", enabled: false }),
      rule({ id: "bad", kind: "regex", pattern: "(" }),
    ]);
    expect(scanLine(c, "ERROR (")).toEqual([]);
  });
});

describe("shouldAlert（S321：角标联动判据）", () => {
  it("命中带提醒的规则才联动；不带提醒的命中不响", () => {
    const quiet = compileRules([rule({ pattern: "INFO" })]);
    const loud = compileRules([rule({ id: "r2", pattern: "FATAL", alert: true })]);
    expect(shouldAlert(scanLine(quiet, "INFO ok"))).toBe(false);
    expect(shouldAlert(scanLine(loud, "FATAL disk"))).toBe(true);
    expect(shouldAlert(scanLine(loud, "nothing here"))).toBe(false);
  });
});

describe("parseHighlightRules（读侧兜底）", () => {
  it("往返一致；缺省字段补默认（enabled 缺省为真、色缺省有值）", () => {
    const json = JSON.stringify([{ id: "a", pattern: "x", kind: "literal" }]);
    const [r] = parseHighlightRules(json);
    expect(r).toMatchObject({ id: "a", pattern: "x", kind: "literal", enabled: true, alert: false });
    expect(r.color).toBeTruthy();
  });

  it("烂值回落空表（高亮坏了不该让终端不可用）", () => {
    expect(parseHighlightRules(null)).toEqual([]);
    expect(parseHighlightRules("nope")).toEqual([]);
    expect(parseHighlightRules('{"a":1}')).toEqual([]);
    expect(parseHighlightRules('[{"id":"a","pattern":"x","kind":"bogus"}]')).toEqual([]);
  });
});

/**
 * 灾难性回溯的形状闸（M4a.1 T94）。
 *
 * 背景实测：`(a+)+$` 在 **30 个字符**的行上跑 10.5 秒。模块头曾声称「两道闸防灾难性
 * 回溯」，但那两道闸限的是**输入尺寸**（规则长度、8 KiB 行截断）——指数曲线在输入远小于
 * 8 KiB 时就已经爆了，方向完全不对。故加静态形状闸并把注释改成实话。
 */
describe("hasNestedQuantifier（嵌套量词静态拒收）", () => {
  it("拒收典型的指数级形状", () => {
    for (const p of ["(a+)+", "(a*)*", "(a+)*", "(a*)+", "([0-9]+)+", "(?:a+)+", "(\\d+)+"]) {
      expect(hasNestedQuantifier(p), `应拒收：${p}`).toBe(true);
    }
    // 计数量词同理（`{1,}` 与 `+` 等价）
    expect(hasNestedQuantifier("(a+){2,}")).toBe(true);
    expect(hasNestedQuantifier("(a{1,3})+")).toBe(true);
  });

  it("不误伤常见的正常式子（假阳会让用户的规则莫名失效）", () => {
    for (const p of [
      "ERROR",
      "a+b+", // 无嵌套
      "(abc)+", // 分组内无量词
      "(a|b)+", // 分组内无量词
      "^\\s*WARN", // 锚点 + 字符类
      "\\d{4}-\\d{2}-\\d{2}", // 纯计数量词
      "(?:GET|POST) /\\S+", // 非捕获组 + 择一
      "\\[(\\w+)\\]", // 分组内无量词（\\w 后无量词）
      "[(+)]+", // 字符类里的括号与加号都是字面量
      "\\(a+\\)+", // 转义括号：不是分组
    ]) {
      expect(hasNestedQuantifier(p), `不应拒收：${p}`).toBe(false);
    }
  });

  it("字符类与转义内部不参与分组判定", () => {
    // `[)]` 里的右括号是字面量，不能被当成分组闭合
    expect(hasNestedQuantifier("[)]+")).toBe(false);
    // 转义的量词也不算量词
    expect(hasNestedQuantifier("(a\\+)+")).toBe(false);
    // ── 下面这组才是真正考察「字符类豁免」的：量词字符必须落在**分组内的字符类里**。
    // 变异验证 N6 首轮存活正是因为我先前的用例把字符类放在了分组**外**（`[(+)]+`），
    // 那条路径压根没被走到——去掉豁免也照样绿。
    // `([+])+`：分组里是一个只含 `+` 字面量的字符类，整体不是嵌套量词。
    for (const p of ["([+])+", "([a+])+", "([*])+", "(x[+]y)+", "([0-9+])+", "([{2}])+"]) {
      expect(hasNestedQuantifier(p), `字符类内的量词字符不算量词：${p}`).toBe(false);
    }
    // 对照：把字符类换成真分组，就该被拒
    expect(hasNestedQuantifier("((+))+")).toBe(false); // `(+)` 非法量词位置，但形状上不算嵌套
    expect(hasNestedQuantifier("([0-9]+)+")).toBe(true); // 类**后**带量词 → 真嵌套
  });
});

describe("compileRules 对嵌套量词整条禁用（M4a.1 T94）", () => {
  const nested: HighlightRule = {
    id: "n1",
    name: "坏正则",
    kind: "regex",
    pattern: "(a+)+$",
    color: "#f00",
    alert: false,
    enabled: true,
  };

  it("含嵌套量词 → re 为 null、error 非空且说清后果与改法", () => {
    const [c] = compileRules([nested]);
    expect(c.re).toBeNull();
    expect(c.error).toBeTruthy();
    expect(c.error).toContain("嵌套量词");
    expect(c.error).toContain("指数级"); // 说清为什么
    expect(c.error).toContain("已禁用"); // 说清后果
  });

  it("被禁用的规则压根不进匹配路径（这才是「不冻住 UI」的实际保证）", () => {
    const compiled = compileRules([nested]);
    // 30 个 a + b：若真去匹配 (a+)+$，实测要 10 秒以上；这里必须瞬间返回
    const line = "a".repeat(30) + "b";
    const t0 = Date.now();
    const hits = scanLine(compiled, line);
    const ms = Date.now() - t0;
    expect(hits).toEqual([]);
    expect(ms, `被禁用的规则不该进匹配路径，实耗 ${ms}ms`).toBeLessThan(500);
  });

  it("反向对照：正常正则照旧编译并命中（防「恒拒收」的假绿）", () => {
    const ok: HighlightRule = { ...nested, id: "ok", pattern: "ERR\\w+" };
    const [c] = compileRules([ok]);
    expect(c.error).toBeNull();
    expect(c.re).toBeInstanceOf(RegExp);
    expect(scanLine([c], "an ERRCODE here").length).toBe(1);
  });

  it("形状闸排在 new RegExp 之前：嵌套量词是**合法**语法，编译得过", () => {
    // 自证前提：这个式子本身能被 JS 编译——所以不能指望「编译失败」来挡它
    expect(() => new RegExp("(a+)+$", "gi")).not.toThrow();
    const [c] = compileRules([nested]);
    expect(c.error).toContain("嵌套量词"); // 报的是形状闸的错，不是编译错
    expect(c.error).not.toContain("正则非法");
  });
});
