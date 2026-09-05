import { describe, it, expect, vi } from "vitest";
import {
  canvasSize,
  captureTerminal,
  measureCellWidth,
  paint,
  planCells,
  resolveColors,
  screenshotFileName,
  xterm256,
  type CellDraw,
  type CellLike,
  type RenderStyle,
  type TerminalLike,
} from "./screenshot";

const STYLE: RenderStyle = {
  foreground: "#cccccc",
  background: "#1e1e2e",
  ansi: [
    "#000000", "#cd3131", "#0dbc79", "#e5e510", "#2472c8", "#bc3fbc", "#11a8cd", "#e5e5e5",
    "#666666", "#f14c4c", "#23d18b", "#f5f543", "#3b8eea", "#d670d6", "#29b8db", "#ffffff",
  ],
  fontFamily: "monospace",
  fontSize: 14,
  lineHeight: 1.2,
};

/** 造一格。默认是「默认前景 + 默认背景的普通字符」。 */
function cell(over: Partial<Record<keyof CellLike, unknown>> = {}): CellLike {
  const base = {
    getChars: () => "a",
    getWidth: () => 1,
    isFgDefault: () => true,
    isBgDefault: () => true,
    isFgPalette: () => false,
    isBgPalette: () => false,
    isFgRGB: () => false,
    isBgRGB: () => false,
    getFgColor: () => 0,
    getBgColor: () => 0,
    isBold: () => 0,
    isInverse: () => 0,
  };
  return { ...base, ...over } as CellLike;
}

/** 造一个终端：`rows` 行，每行由一串 cell 构成。 */
function term(rows: CellLike[][], viewportY = 0, scrollback: CellLike[][] = []): TerminalLike {
  const all = [...scrollback, ...rows];
  return {
    cols: Math.max(...rows.map((r) => r.length), 1),
    rows: rows.length,
    buffer: {
      active: {
        length: all.length,
        viewportY,
        getLine: (y: number) =>
          all[y] ? { getCell: (x: number) => all[y][x] } : undefined,
      },
    },
  };
}

/* ─────────────────── 256 色调色板 ─────────────────── */

describe("xterm 256 色", () => {
  it("0–15 走传入的 ANSI 表（配色换了截图要跟着换）", () => {
    expect(xterm256(0, STYLE.ansi)).toBe("#000000");
    expect(xterm256(1, STYLE.ansi)).toBe("#cd3131");
    expect(xterm256(15, STYLE.ansi)).toBe("#ffffff");
  });

  it("色立方的每一级不是均匀的 51", () => {
    // 0 之后直接跳到 95 是 xterm 的历史定义。按均匀 51 算的话，
    // 所有 256 色输出（htop、tmux 状态栏、diff 高亮）的颜色都会偏。
    expect(xterm256(16, STYLE.ansi)).toBe("#000000"); // (0,0,0)
    expect(xterm256(21, STYLE.ansi)).toBe("#0000ff"); // (0,0,5) → 55+5*40 = 255
    expect(xterm256(17, STYLE.ansi)).toBe("#00005f"); // (0,0,1) → 55+40 = 95 = 0x5f
    expect(xterm256(231, STYLE.ansi)).toBe("#ffffff"); // (5,5,5)
  });

  it("灰阶段是 8 + n*10", () => {
    expect(xterm256(232, STYLE.ansi)).toBe("#080808");
    expect(xterm256(255, STYLE.ansi)).toBe("#eeeeee"); // 8 + 23*10 = 238 = 0xee
  });

  it("越界索引不崩（缓冲区里出现意外值时宁可给个黑色）", () => {
    expect(() => xterm256(999, STYLE.ansi)).not.toThrow();
    expect(xterm256(-1, [])).toBe("#000000");
  });
});

/* ─────────────────── 颜色解析 ─────────────────── */

describe("一格的颜色", () => {
  it("默认色取自配色方案，不是硬编码的黑白", () => {
    const { fg, bg } = resolveColors(cell(), STYLE);
    expect(fg).toBe(STYLE.foreground);
    expect(bg).toBe(STYLE.background);
  });

  it("调色板索引经 xterm256 解析", () => {
    const c = cell({ isFgDefault: () => false, isFgPalette: () => true, getFgColor: () => 1 });
    expect(resolveColors(c, STYLE).fg).toBe("#cd3131");
  });

  it("24 位真彩色原样取出", () => {
    const c = cell({ isFgDefault: () => false, isFgRGB: () => true, getFgColor: () => 0xff8800 });
    expect(resolveColors(c, STYLE).fg).toBe("#ff8800");
  });

  it("真彩色的高位被丢掉（xterm 在高位塞了模式标志）", () => {
    const c = cell({ isFgDefault: () => false, isFgRGB: () => true, getFgColor: () => 0x7fff8800 });
    expect(resolveColors(c, STYLE).fg).toBe("#ff8800");
  });

  /**
   * 反显要在颜色解析这一层处理完。
   *
   * 选区高亮与光标就是靠反显画出来的——漏掉它，截图上会缺一块，
   * 而那一块恰好是用户截图时想让人看的东西。
   */
  it("反显把前景背景对调", () => {
    const c = cell({ isInverse: () => 1 });
    const { fg, bg } = resolveColors(c, STYLE);
    expect(fg).toBe(STYLE.background);
    expect(bg).toBe(STYLE.foreground);
  });

  it("反显叠在自定义色上同样对调", () => {
    const c = cell({
      isFgDefault: () => false,
      isFgPalette: () => true,
      getFgColor: () => 2,
      isInverse: () => 1,
    });
    const { fg, bg } = resolveColors(c, STYLE);
    expect(bg).toBe("#0dbc79"); // 原来的前景成了背景
    expect(fg).toBe(STYLE.background);
  });
});

/* ─────────────────── 取哪些格子 ─────────────────── */

describe("规划要画的格子", () => {
  it("逐行逐列取出", () => {
    const cells = planCells(term([[cell({ getChars: () => "a" }), cell({ getChars: () => "b" })]]), STYLE);
    expect(cells.map((c) => c.char)).toEqual(["a", "b"]);
    expect(cells.map((c) => [c.x, c.y])).toEqual([[0, 0], [1, 0]]);
  });

  it("宽字符的后半格被跳过（否则会画出重影）", () => {
    const wide = cell({ getChars: () => "中", getWidth: () => 2 });
    const tail = cell({ getChars: () => "", getWidth: () => 0 });
    const cells = planCells(term([[wide, tail, cell({ getChars: () => "x" })]]), STYLE);
    expect(cells.map((c) => c.char)).toEqual(["中", "x"]);
    // x 仍然在它原本的列上——跳过后半格不能把后面的字符往左挪
    expect(cells[1].x).toBe(2);
  });

  it("空串的格子按空格算（带背景色的空白要画出来）", () => {
    // `ls` 的目录高亮、diff 的整行底色都是「空字符 + 有背景色」。
    // 丢掉它们截图上会少一片颜色。
    const blank = cell({
      getChars: () => "",
      isBgDefault: () => false,
      isBgPalette: () => true,
      getBgColor: () => 4,
    });
    const cells = planCells(term([[blank]]), STYLE);
    expect(cells[0].char).toBe(" ");
    expect(cells[0].bg).toBe("#2472c8");
  });

  /**
   * 只截**可见屏**，不含回滚。
   *
   * 一屏 80×24 的 PNG 约几十 KB，一万行回滚的 PNG 是几十 MB——
   * 两者不该共用一个按钮。要整份回滚的人要的其实是文本导出。
   */
  it("从 viewportY 开始取，滚上去之后截的是当前看到的那一屏", () => {
    const back = [[cell({ getChars: () => "旧" })], [cell({ getChars: () => "旧2" })]];
    const view = [[cell({ getChars: () => "新" })]];
    const cells = planCells(term(view, 2, back), STYLE);
    expect(cells.map((c) => c.char)).toEqual(["新"]);
  });

  it("行数不足时不崩（缓冲区比 rows 短的瞬间是存在的）", () => {
    const t: TerminalLike = {
      cols: 2,
      rows: 5,
      buffer: { active: { length: 1, viewportY: 0, getLine: () => undefined } },
    };
    expect(planCells(t, STYLE)).toEqual([]);
  });
});

/* ─────────────────── 尺寸与文件名 ─────────────────── */

describe("画布尺寸", () => {
  it("按列数×格宽、行数×行高", () => {
    const s = canvasSize(term([[cell(), cell()]]), STYLE, 8);
    expect(s.width).toBe(16);
    expect(s.cellHeight).toBe(Math.round(14 * 1.2)); // 17
    expect(s.height).toBe(17);
  });

  it("永不产出 0 尺寸的画布（toBlob 会失败）", () => {
    const t: TerminalLike = {
      cols: 0,
      rows: 0,
      buffer: { active: { length: 0, viewportY: 0, getLine: () => undefined } },
    };
    const s = canvasSize(t, STYLE, 0);
    expect(s.width).toBeGreaterThan(0);
    expect(s.height).toBeGreaterThan(0);
  });
});

describe("截图文件名", () => {
  const at = new Date(2026, 7, 23, 9, 5, 7); // 2026-08-23 09:05:07

  it("带到秒的时间戳（连着截几张不互相覆盖）", () => {
    expect(screenshotFileName(at)).toBe("futureshell-20260823-090507.png");
  });

  it("带上标题便于事后辨认", () => {
    expect(screenshotFileName(at, "web01")).toBe("futureshell-web01-20260823-090507.png");
  });

  /**
   * 标题来自**远端**——它是 shell 用 OSC 0 设的，也就是说一个被入侵的主机
   * 可以往里塞任意字符。路径分隔符进了文件名就是一次任意路径写入。
   */
  it("远端可控的标题里，路径分隔符与保留字符被剔掉", () => {
    for (const evil of ["../../etc/passwd", "a/b", "C:\\x", 'a"b<c>|d?e*f']) {
      const name = screenshotFileName(at, evil);
      expect(name, evil).not.toMatch(/[\\/:*?"<>|]/);
      expect(name).toMatch(/^futureshell-.*\.png$/);
    }
  });

  it("标题被剔干净时退回不带标题的名字", () => {
    expect(screenshotFileName(at, "///")).toBe("futureshell-20260823-090507.png");
    expect(screenshotFileName(at, "   ")).toBe("futureshell-20260823-090507.png");
  });

  it("超长标题被截断（远端可以设一个几 KB 的标题）", () => {
    const name = screenshotFileName(at, "x".repeat(500));
    expect(name.length).toBeLessThan(80);
  });
});

/* ─────────────────── 绘制 ─────────────────── */

/** 记录调用序列的假 2D 上下文。 */
function fakeCtx() {
  const calls: string[] = [];
  const ctx = {
    _fill: "",
    set fillStyle(v: string) {
      this._fill = v;
    },
    get fillStyle() {
      return this._fill;
    },
    font: "",
    textBaseline: "" as CanvasTextBaseline,
    fillRect: (x: number, y: number, w: number, h: number) =>
      calls.push(`rect ${ctx._fill} ${x},${y} ${w}x${h}`),
    fillText: (t: string, x: number, y: number) => calls.push(`text ${ctx._fill} "${t}" ${x},${y}`),
    measureText: (s: string) => ({ width: s.length * 8 }),
  };
  return { ctx: ctx as unknown as CanvasRenderingContext2D, calls };
}

describe("绘制", () => {
  const draw = (over: Partial<CellDraw> = {}): CellDraw => ({
    x: 0, y: 0, char: "a", fg: "#cccccc", bg: STYLE.background, bold: false, ...over,
  });

  it("先整屏铺背景，再逐格画", () => {
    // 逐格填背景的话，80×24 上是 1920 次画同一个颜色的 fillRect。
    const { ctx, calls } = fakeCtx();
    paint(ctx, [draw()], STYLE, 8, 17, { width: 80, height: 34 });
    expect(calls[0]).toBe(`rect ${STYLE.background} 0,0 80x34`);
  });

  it("默认背景的格子不再单独填一次", () => {
    const { ctx, calls } = fakeCtx();
    paint(ctx, [draw(), draw({ x: 1 })], STYLE, 8, 17, { width: 16, height: 17 });
    expect(calls.filter((c) => c.startsWith("rect")).length).toBe(1); // 只有整屏那一次
  });

  it("非默认背景的格子各填各的，位置按格宽格高算", () => {
    const { ctx, calls } = fakeCtx();
    paint(ctx, [draw({ x: 2, y: 3, bg: "#ff0000" })], STYLE, 8, 17, { width: 80, height: 68 });
    expect(calls).toContain("rect #ff0000 16,51 8x17");
  });

  it("空格不画字（省掉一屏里绝大多数的 fillText）", () => {
    const { ctx, calls } = fakeCtx();
    paint(ctx, [draw({ char: " " })], STYLE, 8, 17, { width: 8, height: 17 });
    expect(calls.filter((c) => c.startsWith("text"))).toEqual([]);
  });

  it("粗体走 bold 字体（不加的话 ls 的目录名会比终端里细）", () => {
    const { ctx, calls } = fakeCtx();
    paint(ctx, [draw({ bold: true })], STYLE, 8, 17, { width: 8, height: 17 });
    expect(calls.some((c) => c.startsWith("text"))).toBe(true);
    expect(ctx.font).toContain("bold");
  });

  it("量不出字宽时按 0.6em 估，不产出宽度为 0 的图", () => {
    const { ctx } = fakeCtx();
    expect(measureCellWidth(ctx, STYLE)).toBe(8); // 假上下文里 "M" 是 8
    const zero = { ...ctx, measureText: () => ({ width: 0 }) } as unknown as CanvasRenderingContext2D;
    expect(measureCellWidth(zero, STYLE)).toBeCloseTo(14 * 0.6);
  });
});

/* ─────────────────── 两条落地路径（出口点名的那两条） ─────────────────── */

describe("落地：剪贴板与存盘", () => {
  const blob = new Blob(["png"], { type: "image/png" });

  function deps(over: Record<string, unknown> = {}) {
    const clipboard: Blob[] = [];
    const files: { blob: Blob; filename: string }[] = [];
    const { ctx } = fakeCtx();
    const canvas = {
      getContext: () => ctx,
      toBlob: (cb: (b: Blob | null) => void) => cb(blob),
    } as unknown as HTMLCanvasElement;
    return {
      clipboard,
      files,
      d: {
        createCanvas: () => canvas,
        writeClipboard: async (b: Blob) => void clipboard.push(b),
        saveFile: async (b: Blob, filename: string) => void files.push({ blob: b, filename }),
        now: () => new Date(2026, 7, 23, 9, 5, 7),
        ...over,
      },
    };
  }

  const oneCell = term([[cell({ getChars: () => "x" })]]);

  it("剪贴板路：PNG 进剪贴板，不落盘", async () => {
    const { clipboard, files, d } = deps();
    const r = await captureTerminal(oneCell, STYLE, "clipboard", d);
    expect(clipboard).toEqual([blob]);
    expect(files).toEqual([]);
    expect(r, "剪贴板路没有文件名").toBeNull();
  });

  it("存盘路：PNG 落盘且返回文件名，不碰剪贴板", async () => {
    const { clipboard, files, d } = deps();
    const r = await captureTerminal(oneCell, STYLE, "file", d, "web01");
    expect(files).toEqual([{ blob, filename: "futureshell-web01-20260823-090507.png" }]);
    expect(clipboard).toEqual([]);
    // 返回文件名是为了让调用方写 toast——截图这种「按一下、没有可见变化」的操作
    // 必须有反馈，否则用户会连按好几次。
    expect(r).toBe("futureshell-web01-20260823-090507.png");
  });

  it("toBlob 返回 null 时报错，不静默成功", async () => {
    // 静默返回会让用户以为截图进了剪贴板，粘贴时才发现是空的。
    const { ctx } = fakeCtx();
    const bad = {
      getContext: () => ctx,
      toBlob: (cb: (b: Blob | null) => void) => cb(null),
    } as unknown as HTMLCanvasElement;
    const { d } = deps({ createCanvas: () => bad });
    await expect(captureTerminal(oneCell, STYLE, "clipboard", d)).rejects.toThrow(/PNG/);
  });

  it("拿不到 2D 上下文时报错，不产出空图", async () => {
    const noCtx = { getContext: () => null } as unknown as HTMLCanvasElement;
    const { d } = deps({ createCanvas: () => noCtx });
    await expect(captureTerminal(oneCell, STYLE, "file", d)).rejects.toThrow(/画布/);
  });

  it("剪贴板写入失败时错误传上去（权限被拒是常见的）", async () => {
    const { d } = deps({
      writeClipboard: async () => {
        throw new Error("clipboard permission denied");
      },
    });
    await expect(captureTerminal(oneCell, STYLE, "clipboard", d)).rejects.toThrow(/permission/);
  });

  it("画的是这一屏的内容（不是空白画布直接导出）", async () => {
    // 反向对照：上面几条即使 paint 什么都不画也会通过。
    const seen: string[] = [];
    const { ctx } = fakeCtx();
    const spy = new Proxy(ctx, {
      get(t, k) {
        if (k === "fillText") return (s: string) => seen.push(s);
        return Reflect.get(t, k);
      },
    });
    const canvas = {
      getContext: () => spy,
      toBlob: (cb: (b: Blob | null) => void) => cb(blob),
    } as unknown as HTMLCanvasElement;
    const { d } = deps({ createCanvas: () => canvas });
    await captureTerminal(term([[cell({ getChars: () => "H" }), cell({ getChars: () => "i" })]]), STYLE, "clipboard", d);
    expect(seen).toEqual(["H", "i"]);
  });
});
