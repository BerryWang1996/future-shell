import { describe, it, expect } from "vitest";
import { isViewWindow, parseViewParams } from "./view-window";
// Rust 侧拼这个 URL 的地方。两边走散的表现是「窗口开出来是空的」。
import WINDOW_CMD_RS from "../../../app/src/commands/window_cmd.rs?raw";

describe("入口判据：这是不是一个视图窗口", () => {
  it("有 view 参数就是，没有就不是", () => {
    expect(isViewWindow("?view=abc")).toBe(true);
    expect(isViewWindow("?view=abc&title=x")).toBe(true);
    expect(isViewWindow("")).toBe(false);
    expect(isViewWindow("?title=x")).toBe(false);
    expect(isViewWindow("?viewer=abc")).toBe(false); // 前缀相同的别的参数不算
  });

  it("空值的 view 仍然是视图窗口", () => {
    // 判据是「有没有这个参数」，不是「它有没有值」。按「有值」判的话，
    // `?view=` 会挂成主界面——而那个 URL 是 window_new 开出来的窗口，
    // 主界面在那里会是完整的第二份应用（第二个侧栏、第二套模态）。
    expect(isViewWindow("?view=")).toBe(true);
    // 而它解析出来是「没有会话」，走空窗口那一支
    expect(parseViewParams("?view=").sessionId).toBeNull();
  });
});

describe("视图窗口的参数", () => {
  it("取出会话 id", () => {
    expect(parseViewParams("?view=0d5a2f11-8c3e-4a6b").sessionId).toBe("0d5a2f11-8c3e-4a6b");
  });

  it("空串与纯空白按「没有会话」处理", () => {
    // sessionId 为空串的终端连不上任何东西，而它看起来会像一个正常的、
    // 只是没有输出的终端——用户会坐在那里等它连上。
    for (const s of ["?view=", "?view=%20%20", "?view=+"]) {
      expect(parseViewParams(s).sessionId, s).toBeNull();
    }
  });

  it("标题原样还原，含会拆坏 query 的字符", () => {
    for (const title of ["生产 web", "a&b=c", "a#b", "100% CPU", "a+b"]) {
      const search = `?view=x&title=${encodeURIComponent(title)}`;
      expect(parseViewParams(search).title, title).toBe(title);
    }
  });

  it("没有 title 时是空串，不是 undefined/null", () => {
    // 调用方拿它去写 document.title。给 undefined 的话标题栏上会出现
    // 「undefined — FutureShell」，那看起来像程序坏了。
    expect(parseViewParams("?view=x").title).toBe("");
  });

  it("超长标题被截断（URL 可以被手工改）", () => {
    const long = "长".repeat(500);
    const p = parseViewParams(`?view=x&title=${encodeURIComponent(long)}`);
    expect(p.title.length).toBe(120);
  });
});

describe("与 Rust 侧的参数名一致", () => {
  /**
   * 这两个名字是**跨语言**的：Rust 拼，前端读。走散的表现不是报错，
   * 是「窗口开出来是空的」——用户拖出一个标签，得到一个说「这是一个新窗口」
   * 的窗口，而他刚刚明明拖的是一个正在运行的会话。
   */
  it("view 与 title 两个参数名 Rust 侧确实在用", () => {
    expect(WINDOW_CMD_RS, "Rust 侧不再拼 ?view=").toMatch(/index\.html\?view=/);
    expect(WINDOW_CMD_RS, "Rust 侧不再拼 &title=").toMatch(/&title=/);
  });

  it("标题长度上限两边一致", () => {
    // 后端截 120 字，前端也截 120 字。前端更宽的话，一个手工改过的 URL
    // 能让标题栏上出现一整段文本；更窄的话，正常拖出的标题会被无端截短。
    const m = /chars\(\)\.take\((\d+)\)/.exec(WINDOW_CMD_RS);
    expect(m, "在 window_cmd.rs 里找不到标题截断——守卫已失效").toBeTruthy();
    const long = "x".repeat(500);
    expect(parseViewParams(`?view=a&title=${long}`).title.length).toBe(Number(m![1]));
  });
});
