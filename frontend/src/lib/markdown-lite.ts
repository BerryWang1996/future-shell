/**
 * 极小的 Markdown 解析器（M2 出口第 12 项「净化 Markdown 渲染」）。
 *
 * # 为什么不用 marked/markdown-it + DOMPurify
 *
 * 那条路的终点是 `{@html sanitized}`——也就是说「没有脚本执行路径」这件事最终由
 * **净化器的正确性**保证。净化器是好东西，但它是一道需要一直对的闸：
 * 一个绕过（mXSS、新的 HTML 解析怪癖、配置里漏开一个选项）就是一次 XSS，
 * 而这段文本来自**远端模型**、内容由远端服务器（或提示注入）决定。
 *
 * 这里走另一条路：解析成 **token 树**，由 Svelte 用 `{#each}` 渲染成真实元素。
 * 于是整条路径上**没有 `{@html}`**——不是「净化过的 HTML 是安全的」，
 * 而是「从来没有 HTML」。一段 `<img onerror=...>` 在这个渲染器里的结局是
 * 被当成普通文字显示出来，因为 Svelte 的文本插值会转义它。
 *
 * 代价是只支持一个很小的子集。那正好：解读结果需要的是段落、列表、行内代码、
 * 代码块与粗体，别的（表格、图片、链接、HTML 内联）在一个终端工具的侧栏里
 * 本来就不该出现。**尤其是链接**——一个模型生成的可点链接是钓鱼的完美载体。
 */

/** 行内片段。 */
export type Inline =
  | { kind: "text"; text: string }
  | { kind: "code"; text: string }
  | { kind: "strong"; text: string };

/** 块级节点。 */
export type Block =
  | { kind: "p"; spans: Inline[] }
  | { kind: "ul"; items: Inline[][] }
  | { kind: "ol"; items: Inline[][] }
  | { kind: "pre"; text: string; lang: string }
  | { kind: "h"; level: 1 | 2 | 3; spans: Inline[] };

/** 单块最大字符数。模型偶尔会吐一个几十 KB 的代码块，而它要进 DOM。 */
const MAX_BLOCK_CHARS = 20_000;
/** 最多渲染多少块。一份解读超过这个数只可能是模型跑飞了。 */
const MAX_BLOCKS = 200;

/**
 * 解析行内标记。
 *
 * 顺序是**先代码、后粗体**：`` `**a**` `` 里的星号属于代码内容，不该被当成粗体。
 * 反过来的话，一段讲 Markdown 语法的解读结果会被自己的渲染器改写。
 */
export function parseInline(line: string): Inline[] {
  const out: Inline[] = [];
  let rest = line;
  // 行内代码优先：把 `...` 切出来，剩下的段落再找粗体
  const codeRe = /`([^`]+)`/;
  while (rest.length > 0) {
    const m = codeRe.exec(rest);
    if (!m) {
      out.push(...parseStrong(rest));
      break;
    }
    if (m.index > 0) out.push(...parseStrong(rest.slice(0, m.index)));
    out.push({ kind: "code", text: m[1] });
    rest = rest.slice(m.index + m[0].length);
  }
  // 全空时也要有一个节点：空数组会让那一行整个消失，而它可能是有意的空行
  return out.length > 0 ? out : [{ kind: "text", text: "" }];
}

function parseStrong(s: string): Inline[] {
  const out: Inline[] = [];
  let rest = s;
  const re = /\*\*([^*]+)\*\*/;
  while (rest.length > 0) {
    const m = re.exec(rest);
    if (!m) {
      if (rest) out.push({ kind: "text", text: rest });
      break;
    }
    if (m.index > 0) out.push({ kind: "text", text: rest.slice(0, m.index) });
    out.push({ kind: "strong", text: m[1] });
    rest = rest.slice(m.index + m[0].length);
  }
  return out;
}

/**
 * 解析成块序列。
 *
 * 代码块**优先于一切**：围栏里的 `- ` 与 `# ` 是代码内容，不是列表和标题。
 * 不这么做的话，一段包含 diff 或 shell 脚本的解读会被切得七零八落。
 */
export function parseMarkdown(src: string): Block[] {
  const lines = src.replace(/\r\n?/g, "\n").split("\n");
  const blocks: Block[] = [];
  let i = 0;

  const push = (b: Block): void => {
    if (blocks.length < MAX_BLOCKS) blocks.push(b);
  };

  while (i < lines.length) {
    const line = lines[i];

    // ── 代码块 ──
    const fence = /^```(\w*)\s*$/.exec(line.trim());
    if (fence) {
      const lang = fence[1] ?? "";
      const body: string[] = [];
      i++;
      while (i < lines.length && !/^```/.test(lines[i].trim())) {
        body.push(lines[i]);
        i++;
      }
      // 越过收尾围栏；没有收尾（模型截断了）也照样把已有内容显示出来——
      // 丢掉它等于把用户最想看的那段代码吞了。
      if (i < lines.length) i++;
      push({ kind: "pre", text: clamp(body.join("\n")), lang });
      continue;
    }

    // ── 标题 ──
    const h = /^(#{1,3})\s+(.*)$/.exec(line);
    if (h) {
      push({
        kind: "h",
        level: h[1].length as 1 | 2 | 3,
        spans: parseInline(clamp(h[2])),
      });
      i++;
      continue;
    }

    // ── 列表（有序/无序）──
    if (/^\s*[-*+]\s+/.test(line) || /^\s*\d+[.)]\s+/.test(line)) {
      const ordered = /^\s*\d+[.)]\s+/.test(line);
      const items: Inline[][] = [];
      while (i < lines.length) {
        const l = lines[i];
        const isOrdered = /^\s*\d+[.)]\s+/.test(l);
        const isUnordered = /^\s*[-*+]\s+/.test(l);
        if (!isOrdered && !isUnordered) break;
        // 有序与无序混排时断开成两个列表——把 `1.` 塞进 `-` 的列表里
        // 会让编号消失，而编号在「下一步做什么」里是有意义的。
        if (isOrdered !== ordered) break;
        const text = l.replace(/^\s*(?:[-*+]|\d+[.)])\s+/, "");
        items.push(parseInline(clamp(text)));
        i++;
      }
      push({ kind: ordered ? "ol" : "ul", items });
      continue;
    }

    // ── 段落（连续非空行合成一段）──
    if (line.trim() === "") {
      i++;
      continue;
    }
    const para: string[] = [];
    while (
      i < lines.length &&
      lines[i].trim() !== "" &&
      !/^```/.test(lines[i].trim()) &&
      !/^#{1,3}\s/.test(lines[i]) &&
      !/^\s*[-*+]\s+/.test(lines[i]) &&
      !/^\s*\d+[.)]\s+/.test(lines[i])
    ) {
      para.push(lines[i]);
      i++;
    }
    push({ kind: "p", spans: parseInline(clamp(para.join(" "))) });
  }

  return blocks;
}

function clamp(s: string): string {
  return s.length > MAX_BLOCK_CHARS ? `${s.slice(0, MAX_BLOCK_CHARS)}…（已截断）` : s;
}
