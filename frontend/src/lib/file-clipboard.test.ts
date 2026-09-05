import { beforeEach, describe, expect, it } from "vitest";
import { get } from "svelte/store";
import {
  clearClip,
  clipSummary,
  fileClip,
  looksLikeScript,
  pasteBlockedReason,
  setClip,
  summarizePaste,
  type PasteResult,
} from "./file-clipboard";

/**
 * M7.3 远端剪贴板的纯逻辑层。
 *
 * 这里钉的三件事都不是「函数返回对不对」那么轻：
 *
 * ① **跨会话粘贴必须被禁用而不是失败**。范围裁定是「先只做同主机远端↔远端」。让按钮可点、
 *    点了报一句「失败」，用户合理的解读是「程序坏了」而不是「这个功能还没做」。
 * ② **部分失败不得被吞**。文件操作不可撤销；一次粘贴里成功 3 个失败 1 个，只报「粘贴完成」
 *    会让用户以为 4 个都在。summarizePaste 的每一类信息各自成句、失败点名到文件。
 * ③ **空选择不清剪贴板**。先复制、再误点一次空白处取消选择、再点复制——如果空选择也写
 *    剪贴板，刚才复制的内容就被悄悄清掉了，而用户不知道。
 */

const ok = (name: string, renamed = false) => ({ name, ok: true, error: "", renamed });
const bad = (name: string, error: string) => ({ name, ok: false, error, renamed: false });
const result = (p: Partial<PasteResult>): PasteResult => ({ done: [], skipped: [], same_path: [], ...p });

describe("剪贴板存取", () => {
  beforeEach(() => clearClip());

  it("setClip 记下会话、路径与剪切标记", () => {
    setClip("s1", ["/a/x.txt", "/a/y.txt"], true);
    expect(get(fileClip)).toEqual({ sessionId: "s1", paths: ["/a/x.txt", "/a/y.txt"], cut: true });
  });

  it("空选择不写剪贴板——否则会把上一次复制的内容悄悄清掉", () => {
    setClip("s1", ["/a/x.txt"], false);
    setClip("s1", [], false);
    expect(get(fileClip)?.paths).toEqual(["/a/x.txt"]);
  });

  it("存进去的是副本：调用方之后改自己那个数组不影响剪贴板", () => {
    const src = ["/a/x.txt"];
    setClip("s1", src, false);
    src.push("/a/后加的.txt");
    expect(get(fileClip)?.paths).toEqual(["/a/x.txt"]);
  });
});

describe("粘贴可用性（pasteBlockedReason）", () => {
  it("同会话且有内容 → 可用", () => {
    expect(pasteBlockedReason({ sessionId: "s1", paths: ["/a/x"], cut: false }, "s1")).toBeNull();
  });

  it("剪贴板为空 → 说清是空的", () => {
    expect(pasteBlockedReason(null, "s1")).toContain("空");
    expect(pasteBlockedReason({ sessionId: "s1", paths: [], cut: false }, "s1")).toContain("空");
  });

  it("跨会话 → 禁用，且理由里说明这是「还没做」而不是「出错了」", () => {
    const why = pasteBlockedReason({ sessionId: "s1", paths: ["/a/x"], cut: false }, "s2");
    expect(why).not.toBeNull();
    expect(why).toContain("另一个会话");
    expect(why).toContain("还没做");
  });

  it("没有活动会话 → 禁用", () => {
    expect(pasteBlockedReason({ sessionId: "s1", paths: ["/a/x"], cut: false }, null)).not.toBeNull();
  });
});

describe("剪贴板摘要（按钮 title）", () => {
  it("单项报名字，多项报数量 + 第一项", () => {
    expect(clipSummary({ sessionId: "s", paths: ["/a/b/x.txt"], cut: false })).toBe("已复制：x.txt");
    expect(clipSummary({ sessionId: "s", paths: ["/a/b/x.txt", "/a/y"], cut: true })).toBe("已剪切 2 项，第一项：x.txt");
  });
});

describe("粘贴结果播报（summarizePaste）", () => {
  it("全成功 → info，一句话", () => {
    const s = summarizePaste(result({ done: [ok("a"), ok("b")] }));
    expect(s.level).toBe("info");
    expect(s.lines).toEqual(["成功 2 项"]);
  });

  it("部分失败 → error，且逐条点名文件与原因（不得只说「粘贴完成」）", () => {
    const s = summarizePaste(result({ done: [ok("a"), bad("b.txt", "Permission denied")] }));
    expect(s.level).toBe("error");
    expect(s.lines).toContain("成功 1 项");
    expect(s.lines.some((l) => l.includes("b.txt") && l.includes("Permission denied"))).toBe(true);
  });

  it("远端没给原因时也要成句，不能出现空白的「失败：x——」", () => {
    const s = summarizePaste(result({ done: [bad("b.txt", "")] }));
    expect(s.lines[0]).toBe("失败：b.txt——远端没有给出原因");
  });

  it("改名/跳过/同路径三类各自成句，且都降级为至少 warn", () => {
    const s = summarizePaste(result({
      done: [ok("a (2).txt", true)],
      skipped: ["c.txt"],
      same_path: ["d.txt"],
    }));
    expect(s.level).toBe("warn");
    expect(s.lines.some((l) => l.includes("因重名改存为") && l.includes("a (2).txt"))).toBe(true);
    expect(s.lines.some((l) => l.includes("已跳过") && l.includes("c.txt"))).toBe(true);
    expect(s.lines.some((l) => l.includes("同一个位置") && l.includes("d.txt"))).toBe(true);
  });

  it("失败与跳过同时存在时按最重的报（error 压过 warn）", () => {
    expect(summarizePaste(result({ done: [bad("a", "x")], skipped: ["b"] })).level).toBe("error");
  });

  it("空结果也要说话——静默会让用户以为粘贴还在进行", () => {
    expect(summarizePaste(result({})).lines).toEqual(["没有需要处理的条目"]);
  });
});

describe("looksLikeScript", () => {
  it("认 sh/bash/zsh/ksh，大小写不敏感", () => {
    for (const n of ["a.sh", "a.BASH", "deploy.zsh", "x.ksh"]) expect(looksLikeScript(n)).toBe(true);
  });

  it("不认非脚本，也不认「名字里带 .sh 但结尾不是」", () => {
    for (const n of ["a.txt", "sh", "a.sh.txt", "notes.shell"]) expect(looksLikeScript(n)).toBe(false);
  });
});
