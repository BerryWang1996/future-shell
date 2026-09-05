import { describe, it, expect } from "vitest";
import { KIND_LABEL, summarize, type ForeignKind, type ForeignPreview } from "./foreign-import";

/** 造一个预览壳，各用例只覆盖自己关心的字段。 */
function mk(over: Partial<ForeignPreview> = {}): ForeignPreview {
  return {
    kind: "xshell-session",
    filename: "a.xsh",
    profiles: [],
    schemes: [],
    unresolved: [],
    ...over,
  };
}

const prof = (over: Record<string, unknown> = {}) => ({
  name: "生产 web",
  host: "10.0.0.5",
  port: 2222,
  username: "ops",
  auth_hint: "password",
  private_key_path: null,
  group_path: null,
  ...over,
});

const scheme = (name: string) => ({
  name,
  foreground: "#cccccc",
  background: "#1e1e2e",
  ansi: Array.from({ length: 16 }, () => "#112233"),
  cursor: null,
});

describe("导入格式的中文名", () => {
  // 显示格式名而不只是文件名，是因为**认错格式是可能的**（后端按内容认，扩展名只决定
  // 先试哪个）。用户看到「xxx.xsh → Xshell 配色」就知道哪里不对；只显示文件名的话，
  // 他要等到导入完成、连接列表里多出一条空连接才发现。
  it("四种格式一个不缺，且没有多余条目", () => {
    const kinds: ForeignKind[] = [
      "xshell-session",
      "xshell-colors",
      "finalshell",
      "iterm-colors",
    ];
    // 双向包含：少一条 ⇒ 界面显示内部标识符；多一条 ⇒ 与 Rust 的枚举已经对不上了
    expect(Object.keys(KIND_LABEL).sort()).toEqual([...kinds].sort());
    for (const k of kinds) {
      expect(KIND_LABEL[k], `${k} 没有中文名`).toBeTruthy();
      // 不能是把 kebab-case 原样抄一遍——那等于没翻译
      expect(KIND_LABEL[k]).not.toBe(k);
    }
  });
});

describe("一句话说清这个文件里有什么", () => {
  it("单条连接把地址写出来（用户核对的是「这是不是我那台机器」）", () => {
    const s = summarize(mk({ profiles: [prof()] }));
    expect(s).toContain("ops@10.0.0.5:2222");
  });

  it("多条连接只报条数（十几条地址会把这一行撑成一屏）", () => {
    const s = summarize(mk({ profiles: [prof(), prof({ host: "10.0.0.6" })] }));
    expect(s).toContain("2 条");
    expect(s).not.toContain("10.0.0.5"); // 不该混着列一部分地址
  });

  it("单套配色报名字，多套报数量", () => {
    expect(summarize(mk({ schemes: [scheme("Dracula")] }))).toContain("Dracula");
    expect(summarize(mk({ schemes: [scheme("A"), scheme("B"), scheme("C")] }))).toContain("3 套");
  });

  it("连接与配色都有时两样都说", () => {
    const s = summarize(mk({ profiles: [prof()], schemes: [scheme("Dracula")] }));
    expect(s).toContain("10.0.0.5");
    expect(s).toContain("Dracula");
  });

  it("两者都空时说「没有可导入的内容」，而不是留一行空白", () => {
    // 一个语法正确但内容为空的文件是可能的。返回空串的话，那一行看起来像渲染坏了，
    // 而用户分不清「解析失败」与「这个文件本来就是空的」。
    expect(summarize(mk())).toBe("没有可导入的内容");
  });
});
