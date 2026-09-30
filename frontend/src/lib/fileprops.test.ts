/**
 * 文件属性呈现纯函数的测试（M4a）。
 *
 * 判据集中在「未知不许编默认值」与「setuid/sticky 的大小写区分」两处——
 * 这两处出错时界面看起来完全正常，而用户据此做的判断（我能不能写、这个配置对不对）
 * 是错的。
 */
import { describe, expect, it } from "vitest";
import { formatMode, formatOwner, typeLabel, validateSymlink, UNKNOWN } from "./fileprops";

describe("formatMode", () => {
  it("常见权限位", () => {
    expect(formatMode(0o755)).toBe("rwxr-xr-x (0755)");
    expect(formatMode(0o644)).toBe("rw-r--r-- (0644)");
    expect(formatMode(0o600)).toBe("rw------- (0600)");
    expect(formatMode(0o777)).toBe("rwxrwxrwx (0777)");
    expect(formatMode(0)).toBe("--------- (0000)");
  });

  it("未知一律「—」：不编 0644（用户会把它当成真实权限）", () => {
    expect(formatMode(null)).toBe(UNKNOWN);
    expect(formatMode(undefined)).toBe(UNKNOWN);
    expect(formatMode(Number.NaN)).toBe(UNKNOWN);
  });

  it("setuid/setgid/sticky 覆盖 x 位，无 x 时用大写（配错的情形要看得出来）", () => {
    // 4755：属主有 x → 小写 s
    expect(formatMode(0o4755)).toBe("rwsr-xr-x (4755)");
    // 4655：属主无 x → 大写 S（设了 setuid 却不可执行，通常是配错）
    expect(formatMode(0o4655)).toBe("rwSr-xr-x (4655)");
    // 2755：组有 x → 小写 s
    expect(formatMode(0o2755)).toBe("rwxr-sr-x (2755)");
    // 2745：组无 x → 大写 S
    expect(formatMode(0o2745)).toBe("rwxr-Sr-x (2745)");
    // 1777：/tmp 的经典 sticky，其他有 x → 小写 t
    expect(formatMode(0o1777)).toBe("rwxrwxrwt (1777)");
    // 1776：其他无 x → 大写 T
    expect(formatMode(0o1776)).toBe("rwxrwxrwT (1776)");
  });

  it("高位被裁到低 12 位（文件类型位不混进权限显示）", () => {
    // 0o100644 = S_IFREG | 0644：类型位不属于权限
    expect(formatMode(0o100644)).toBe("rw-r--r-- (0644)");
  });
});

describe("formatOwner", () => {
  it("uid:gid", () => {
    expect(formatOwner(1000, 1000)).toBe("1000:1000");
    expect(formatOwner(0, 0)).toBe("0:0"); // root 是 0，不能被当成「假值→未知」
  });

  it("任一缺失即整体未知（半个属主没有意义）", () => {
    expect(formatOwner(1000, null)).toBe(UNKNOWN);
    expect(formatOwner(null, 1000)).toBe(UNKNOWN);
    expect(formatOwner(undefined, undefined)).toBe(UNKNOWN);
  });
});

describe("typeLabel", () => {
  it("四种类型各有中文名，未知类型带出原值", () => {
    expect(typeLabel("regular")).toBe("普通文件");
    expect(typeLabel("dir")).toBe("目录");
    expect(typeLabel("symlink")).toBe("符号链接");
    expect(typeLabel("other")).toContain("其他");
    expect(typeLabel("weird")).toContain("weird");
  });
});

describe("validateSymlink", () => {
  it("合法：相对与绝对目标都放行", () => {
    expect(validateSymlink("link", "../shared/x.bin")).toBeUndefined();
    expect(validateSymlink("link", "/etc/hosts")).toBeUndefined();
  });

  it("链名不许带路径分隔符（面板语义是「在当前目录建」）", () => {
    expect(validateSymlink("a/b", "x")).toContain("路径分隔符");
    expect(validateSymlink("a\\b", "x")).toContain("路径分隔符");
    expect(validateSymlink("../evil", "x")).toContain("路径分隔符");
  });

  it("链名不许为空 / . / ..", () => {
    expect(validateSymlink("", "x")).toContain("不能为空");
    expect(validateSymlink("   ", "x")).toContain("不能为空");
    expect(validateSymlink(".", "x")).toContain(". 或 ..");
    expect(validateSymlink("..", "x")).toContain(". 或 ..");
  });

  it("目标不许为空（空目标建出的链必然是断链）", () => {
    expect(validateSymlink("link", "")).toContain("目标不能为空");
    expect(validateSymlink("link", "  ")).toContain("目标不能为空");
  });
});
