import { describe, it, expect } from "vitest";
import {
  EMPTY_SELECTION,
  ROOT_LABEL,
  joinPath,
  parentOf,
  reduceSelection,
  selectedBytes,
  sortEntries,
  splitBreadcrumbs,
  splitSegments,
  type PickerEntry,
} from "./file-picker";

/**
 * 选择器纯逻辑。
 *
 * 判据重点在**根目录**与**分隔符混用**两处：用户报的原始问题就是
 * 「进了文件夹回不去」，而回不去的两种成因恰是这两个——要么根的边界判错
 * （上级按钮永远可点但点了没用），要么路径拼出一个后端不认的形状。
 */

describe("joinPath", () => {
  it("空目录（浏览根）时直接返回名字", () => {
    expect(joinPath("", "docs")).toBe("docs");
  });

  it("目录里出现过反斜杠就用反斜杠，否则用正斜杠", () => {
    expect(joinPath("a/b", "c")).toBe("a/b/c");
    expect(joinPath("a\\b", "c")).toBe("a\\b\\c");
  });

  it("已有结尾分隔符时不重复添加", () => {
    expect(joinPath("a/", "c")).toBe("a/c");
    expect(joinPath("a\\", "c")).toBe("a\\c");
  });
});

describe("splitSegments", () => {
  it("空串没有段——那是浏览根", () => {
    expect(splitSegments("")).toEqual([]);
  });

  it("两种分隔符都认", () => {
    expect(splitSegments("a/b\\c")).toEqual(["a", "b", "c"]);
  });

  it("连续分隔符与结尾分隔符不产生空段", () => {
    // 空段会在面包屑上变成一个没有文字的按钮，点下去跳到重复路径。
    expect(splitSegments("a//b/")).toEqual(["a", "b"]);
    expect(splitSegments("/")).toEqual([]);
  });
});

describe("splitBreadcrumbs", () => {
  it("根目录只有一段，且它就是根", () => {
    expect(splitBreadcrumbs("")).toEqual([{ label: ROOT_LABEL, path: "" }]);
  });

  it("每一段的 path 是点它之后要跳去的路径（逐级累加）", () => {
    expect(splitBreadcrumbs("docs/sub/deep")).toEqual([
      { label: ROOT_LABEL, path: "" },
      { label: "docs", path: "docs" },
      { label: "sub", path: "docs/sub" },
      { label: "deep", path: "docs/sub/deep" },
    ]);
  });

  it("**第一段恒为根**——任何深度都有一键回根的入口", () => {
    // 这是「进了文件夹回不去」的第二道保险（第一道是上级按钮）。
    for (const p of ["", "a", "a/b", "a\\b\\c\\d\\e"]) {
      const crumbs = splitBreadcrumbs(p);
      expect(crumbs[0], `路径 ${p} 的面包屑首段不是根`).toEqual({
        label: ROOT_LABEL,
        path: "",
      });
    }
  });

  it("反斜杠路径重建时保持反斜杠，不偷偷换成正斜杠", () => {
    // 换了的话 cwd 在用户点面包屑前后长得不一样，调试时平添一层困惑。
    expect(splitBreadcrumbs("a\\b")).toEqual([
      { label: ROOT_LABEL, path: "" },
      { label: "a", path: "a" },
      { label: "b", path: "a\\b" },
    ]);
  });
});

describe("parentOf", () => {
  it("**已在浏览根时返回 null**，不是空串", () => {
    // 返回空串的话「上级」按钮永远可点，点了又什么都不发生——
    // 而那种按钮比禁用的按钮更让人困惑（会怀疑是不是卡住了）。
    expect(parentOf("")).toBeNull();
  });

  it("第一层的上级是根（空串），不是 null", () => {
    // 这一条与上一条一起把「差一级」的错逼出来：两者返回值必须不同。
    expect(parentOf("docs")).toBe("");
  });

  it("深层逐级上行", () => {
    expect(parentOf("a/b/c")).toBe("a/b");
    expect(parentOf("a/b")).toBe("a");
  });

  it("反斜杠路径的上级仍用反斜杠", () => {
    expect(parentOf("a\\b\\c")).toBe("a\\b");
  });

  it("结尾分隔符不会多算一级", () => {
    // "a/b/" 的上级是 "a"，不是 "a/b"（空段已被丢弃）。
    expect(parentOf("a/b/")).toBe("a");
  });
});

describe("sortEntries", () => {
  const mk = (name: string, is_dir: boolean): PickerEntry => ({ name, is_dir, size: 0 });

  it("目录恒在文件之前", () => {
    const out = sortEntries([mk("b.txt", false), mk("a-dir", true), mk("a.txt", false)]);
    expect(out.map((e) => e.name)).toEqual(["a-dir", "a.txt", "b.txt"]);
  });

  it("同类内按 localeCompare 排（中文名要有可读顺序）", () => {
    const out = sortEntries([mk("张三.txt", false), mk("李四.txt", false)]);
    // 只断言「不是按 UTF-16 码元」：具体顺序由 locale 决定，钉死会在别的机器上假红。
    expect(out.map((e) => e.name).sort((a, b) => a.localeCompare(b))).toEqual(
      out.map((e) => e.name),
    );
  });

  it("不修改入参（组件会把原数组另作他用）", () => {
    const input = [mk("b", false), mk("a", true)];
    const copy = [...input];
    sortEntries(input);
    expect(input).toEqual(copy);
  });
});

describe("reduceSelection", () => {
  const ordered = ["a", "b", "c", "d", "e"];
  const noMods = { ctrl: false, shift: false };

  it("无修饰键 = 替换成单选", () => {
    const s1 = reduceSelection(EMPTY_SELECTION, "b", noMods, ordered);
    expect([...s1.names]).toEqual(["b"]);
    const s2 = reduceSelection(s1, "d", noMods, ordered);
    expect([...s2.names]).toEqual(["d"]);
  });

  it("Ctrl = 切换本条；再点一次取消", () => {
    let s = reduceSelection(EMPTY_SELECTION, "a", noMods, ordered);
    s = reduceSelection(s, "c", { ctrl: true, shift: false }, ordered);
    expect([...s.names].sort()).toEqual(["a", "c"]);
    s = reduceSelection(s, "a", { ctrl: true, shift: false }, ordered);
    expect([...s.names]).toEqual(["c"]);
  });

  it("Ctrl 可以一路取消到空集", () => {
    let s = reduceSelection(EMPTY_SELECTION, "a", noMods, ordered);
    s = reduceSelection(s, "a", { ctrl: true, shift: false }, ordered);
    expect(s.names.size).toBe(0);
  });

  it("Shift = 锚点到本条的闭区间（含两端）", () => {
    let s = reduceSelection(EMPTY_SELECTION, "b", noMods, ordered);
    s = reduceSelection(s, "d", { ctrl: false, shift: true }, ordered);
    expect([...s.names].sort()).toEqual(["b", "c", "d"]);
  });

  it("Shift 向上选也对（锚点在后、目标在前）", () => {
    let s = reduceSelection(EMPTY_SELECTION, "d", noMods, ordered);
    s = reduceSelection(s, "b", { ctrl: false, shift: true }, ordered);
    expect([...s.names].sort()).toEqual(["b", "c", "d"]);
  });

  it("**连续两次 Shift 不会让选区自己长大**——锚点不动", () => {
    // 用「上一次点击」当锚点的实现在这里现形：那样第二次 Shift 会从 d 起算，
    // 得到 {d,e} 或 {b..e}，而正确答案是从锚点 b 起算的 {b,c}。
    let s = reduceSelection(EMPTY_SELECTION, "b", noMods, ordered);
    s = reduceSelection(s, "d", { ctrl: false, shift: true }, ordered);
    s = reduceSelection(s, "c", { ctrl: false, shift: true }, ordered);
    expect([...s.names].sort()).toEqual(["b", "c"]);
    expect(s.anchor).toBe("b");
  });

  it("无锚点时 Shift 退化成单选", () => {
    const s = reduceSelection(EMPTY_SELECTION, "c", { ctrl: false, shift: true }, ordered);
    expect([...s.names]).toEqual(["c"]);
    expect(s.anchor).toBe("c");
  });

  it("锚点已不在当前列表里（换了目录）时退化成单选，不选出空区间", () => {
    // 空区间在屏幕上看起来像「点了没反应」。
    const stale = { names: new Set(["gone"]), anchor: "gone" };
    const s = reduceSelection(stale, "c", { ctrl: false, shift: true }, ordered);
    expect([...s.names]).toEqual(["c"]);
  });

  it("Shift 之后锚点保留，可以再用 Ctrl 加选", () => {
    let s = reduceSelection(EMPTY_SELECTION, "a", noMods, ordered);
    s = reduceSelection(s, "b", { ctrl: false, shift: true }, ordered);
    s = reduceSelection(s, "e", { ctrl: true, shift: false }, ordered);
    expect([...s.names].sort()).toEqual(["a", "b", "e"]);
  });
});

describe("selectedBytes", () => {
  const entries: PickerEntry[] = [
    { name: "a.bin", is_dir: false, size: 100 },
    { name: "b.bin", is_dir: false, size: 250 },
    { name: "dir", is_dir: true, size: 4096 },
  ];

  it("只累加被选中的文件", () => {
    expect(selectedBytes(entries, new Set(["a.bin"]))).toBe(100);
    expect(selectedBytes(entries, new Set(["a.bin", "b.bin"]))).toBe(350);
  });

  it("**目录不计入**——目录的 size 是元数据大小，加进去会给出一个荒谬的总量", () => {
    expect(selectedBytes(entries, new Set(["dir"]))).toBe(0);
    expect(selectedBytes(entries, new Set(["a.bin", "dir"]))).toBe(100);
  });

  it("选中一个不在列表里的名字不会崩", () => {
    expect(selectedBytes(entries, new Set(["ghost"]))).toBe(0);
  });
});
