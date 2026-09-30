import { describe, it, expect } from "vitest";
import { parseInline, parseMarkdown, type Block } from "./markdown-lite";

const kinds = (bs: Block[]) => bs.map((b) => b.kind);
const textOf = (b: Block): string => {
  if (b.kind === "pre") return b.text;
  if (b.kind === "p" || b.kind === "h") return b.spans.map((s) => s.text).join("");
  return b.items.map((i) => i.map((s) => s.text).join("")).join("|");
};

describe("行内标记", () => {
  it("纯文本原样", () => {
    expect(parseInline("hello")).toEqual([{ kind: "text", text: "hello" }]);
  });

  it("行内代码", () => {
    expect(parseInline("跑 `ls -la` 看看")).toEqual([
      { kind: "text", text: "跑 " },
      { kind: "code", text: "ls -la" },
      { kind: "text", text: " 看看" },
    ]);
  });

  it("粗体", () => {
    expect(parseInline("这是 **重点** 内容")).toEqual([
      { kind: "text", text: "这是 " },
      { kind: "strong", text: "重点" },
      { kind: "text", text: " 内容" },
    ]);
  });

  /**
   * **先代码、后粗体**：反引号里的星号属于代码内容。
   *
   * 反过来的话，一段讲 Markdown 语法的解读结果会被自己的渲染器改写——
   * 而解读结果里出现「用 `**` 加粗」这种句子完全正常。
   */
  it("代码里的星号不当粗体", () => {
    const spans = parseInline("写成 `**a**` 就是粗体");
    expect(spans.find((s) => s.kind === "code")?.text).toBe("**a**");
    expect(spans.some((s) => s.kind === "strong")).toBe(false);
  });

  it("空行也产出一个节点（不然那一行整个消失）", () => {
    expect(parseInline("")).toEqual([{ kind: "text", text: "" }]);
  });

  it("未闭合的标记按普通文字处理，不吞后面的内容", () => {
    // 模型被截断时常留一个孤立的反引号或双星号。吞掉后面的正文是最坏的处置——
    // 用户看到的是一段话突然断在中间。
    //
    // 断言的是**正文还在**，而不是「标记符号被去掉了」：未闭合的符号原样留着
    // 才是对的（它就是模型吐出来的内容），要保的是它后面那些字没丢。
    expect(parseInline("未闭合 `code").map((x) => x.text).join("")).toContain("code");
    expect(parseInline("未闭合 **bold").map((x) => x.text).join("")).toContain("bold");
    // 孤立符号本身不崩、不产出奇怪类型
    for (const s of ["`", "**", "``", "***"]) {
      const spans = parseInline(s);
      expect(spans.length).toBeGreaterThan(0);
      expect(spans.every((x) => x.kind === "text" || x.kind === "code" || x.kind === "strong")).toBe(true);
    }
  });
});

describe("块级结构", () => {
  it("段落：连续非空行合成一段", () => {
    const bs = parseMarkdown("第一行\n第二行\n\n另一段");
    expect(kinds(bs)).toEqual(["p", "p"]);
    expect(textOf(bs[0])).toBe("第一行 第二行");
  });

  it("标题三级", () => {
    const bs = parseMarkdown("# 一\n## 二\n### 三");
    expect(kinds(bs)).toEqual(["h", "h", "h"]);
    expect((bs[0] as { level: number }).level).toBe(1);
    expect((bs[2] as { level: number }).level).toBe(3);
  });

  it("四个 # 不是标题（只支持三级）", () => {
    expect(kinds(parseMarkdown("#### 四"))).toEqual(["p"]);
  });

  it("无序列表三种符号都认", () => {
    for (const mark of ["-", "*", "+"]) {
      const bs = parseMarkdown(`${mark} 甲\n${mark} 乙`);
      expect(kinds(bs)).toEqual(["ul"]);
      expect(textOf(bs[0])).toBe("甲|乙");
    }
  });

  it("有序列表两种写法都认", () => {
    for (const src of ["1. 甲\n2. 乙", "1) 甲\n2) 乙"]) {
      const bs = parseMarkdown(src);
      expect(kinds(bs)).toEqual(["ol"]);
      expect(textOf(bs[0])).toBe("甲|乙");
    }
  });

  /**
   * 有序与无序混排时断开成两个列表。
   *
   * 把 `1.` 塞进 `-` 的列表里会让编号消失，而编号在「下一步做什么」这种
   * 解读结果里是有意义的。
   */
  it("有序无序混排断成两个列表", () => {
    expect(kinds(parseMarkdown("- 甲\n1. 乙"))).toEqual(["ul", "ol"]);
  });

  it("代码块带语言标记", () => {
    const bs = parseMarkdown("```bash\nls -la\ndf -h\n```");
    expect(kinds(bs)).toEqual(["pre"]);
    expect(textOf(bs[0])).toBe("ls -la\ndf -h");
    expect((bs[0] as { lang: string }).lang).toBe("bash");
  });

  /**
   * 代码块**优先于一切**：围栏里的 `- ` 与 `# ` 是代码内容。
   *
   * 不这么做的话，一段包含 diff 或 shell 脚本的解读会被切得七零八落——
   * 而 diff 的每一行都以 `-` 或 `+` 开头。
   */
  it("代码块里的列表符号与井号不被当成结构", () => {
    const bs = parseMarkdown("```diff\n- 删掉的行\n+ 加上的行\n# 注释\n```");
    expect(kinds(bs)).toEqual(["pre"]);
    expect(textOf(bs[0])).toBe("- 删掉的行\n+ 加上的行\n# 注释");
  });

  it("没有收尾围栏时仍把已有内容显示出来", () => {
    // 模型被截断是常事。丢掉它等于把用户最想看的那段代码吞了。
    const bs = parseMarkdown("```sh\nls -la\ndf -h");
    expect(kinds(bs)).toEqual(["pre"]);
    expect(textOf(bs[0])).toContain("ls -la");
  });

  it("混合文档结构正确", () => {
    const bs = parseMarkdown(
      "## 原因\n端口被占用。\n\n- 查占用：`ss -ltnp`\n- 杀进程\n\n```sh\nkill -9 1234\n```",
    );
    expect(kinds(bs)).toEqual(["h", "p", "ul", "pre"]);
  });
});

describe("上限", () => {
  it("单块超长时截断并标注", () => {
    const huge = "x".repeat(50_000);
    const bs = parseMarkdown("```\n" + huge + "\n```");
    expect(textOf(bs[0]).length).toBeLessThan(21_000);
    expect(textOf(bs[0])).toContain("已截断");
  });

  it("块数有上限（模型跑飞时不把几千个节点塞进 DOM）", () => {
    const many = Array.from({ length: 500 }, (_, i) => `段落 ${i}`).join("\n\n");
    expect(parseMarkdown(many).length).toBeLessThanOrEqual(200);
  });
});

/**
 * **注入用例**（出口点名的那一项）。
 *
 * 判据不是「这些串被净化了」，而是「它们作为 token 树的一部分，**只能**是文本」。
 * 渲染侧没有 `{@html}`（见 Markdown.svelte 的组件注释），所以这里要证明的是
 * 解析器不会把它们变成除 text/code/strong 之外的任何东西——
 * 那三种在 Svelte 模板里都走文本插值。
 */
describe("注入：危险输入只能变成文字", () => {
  const PAYLOADS = [
    '<script>alert(1)</script>',
    '<img src=x onerror=alert(1)>',
    '<iframe src="javascript:alert(1)"></iframe>',
    '[点我](javascript:alert(1))',
    '<a href="javascript:void(0)">x</a>',
    '<svg onload=alert(1)>',
    "<!--[if IE]><script>alert(1)</script><![endif]-->",
    '<style>body{display:none}</style>',
    "<math><mtext><table><mglyph><style><!--</style><img src=x onerror=alert(1)>",
    '<form><button formaction="javascript:alert(1)">x',
    "javascript:alert(1)",
    '"><script>alert(1)</script>',
  ];

  it("没有任何一种 payload 产出 text/code/strong 之外的行内类型", () => {
    const allowed = new Set(["text", "code", "strong"]);
    for (const p of PAYLOADS) {
      for (const b of parseMarkdown(p)) {
        const spans = b.kind === "pre" ? [] : b.kind === "p" || b.kind === "h" ? b.spans : b.items.flat();
        for (const s of spans) {
          expect(allowed.has(s.kind), `${p} → ${s.kind}`).toBe(true);
        }
      }
    }
  });

  it("块类型也只有五种，没有「原始 HTML」这一档", () => {
    // 加一个 `{ kind: "html" }` 变体是这条路上唯一能引入 XSS 的改动。
    // 这条钉住那个变体不存在。
    const allowed = new Set(["p", "ul", "ol", "pre", "h"]);
    for (const p of PAYLOADS) {
      for (const b of parseMarkdown(p)) {
        expect(allowed.has(b.kind), `${p} → ${b.kind}`).toBe(true);
      }
    }
  });

  it("payload 的文字内容原样保留（不是被删掉，是被当成文字）", () => {
    // 删掉它反而更糟：用户看不出模型说了什么，而那可能正是要报告的东西。
    const bs = parseMarkdown("<script>alert(1)</script>");
    expect(textOf(bs[0])).toContain("alert(1)");
  });

  it("链接语法不产出链接（模型生成的可点链接是钓鱼的完美载体）", () => {
    const bs = parseMarkdown("[点我](https://evil.example)");
    // 整段是文字，没有任何「链接」类型
    expect(bs.every((b) => b.kind !== "pre")).toBe(true);
    const s = bs.map(textOf).join("");
    expect(s).toContain("点我");
    expect(s).toContain("evil.example"); // 地址也照实显示，用户看得见它想带你去哪
  });

  it("渲染组件里没有 {@html}", async () => {
    // 这是整条路径的根据。行为测试证明不了「不存在」，源码断言才行。
    //
    // **必须先剥注释**：那个组件的文档注释里就写着「这个组件里没有 {@html}」——
    // 首跑时这条断言被自己那句话判红了。一个扫全文的断言会让「解释为什么安全」
    // 变成「不安全」的证据，而那种假红最消耗信任：下一个看到它的人会倾向于
    // 把断言改松，而不是去看清它在说什么。
    const raw = (await import("../components/Markdown.svelte?raw")).default;
    const code = raw
      .replace(/<!--[\s\S]*?-->/g, "")
      .replace(/\/\*[\s\S]*?\*\//g, "")
      .replace(/(^|[^:])\/\/.*$/gm, "$1");
    expect(code, "渲染组件里出现了 {@html}——那是这条路径上唯一能引入 XSS 的写法").not.toContain("@html");
    // 反向对照两条：剥注释没把整个文件吃掉，且确实是那个组件
    expect(code).toContain("parseMarkdown");
    expect(code.length).toBeGreaterThan(500);
  });
});
