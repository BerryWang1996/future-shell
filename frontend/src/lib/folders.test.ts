/** S326：会话树手动文件夹的纯逻辑判据（M4a 出口标准「新建/重命名/删除分组后
 *  会话归属即时更新、重启持久」的可离线部分）。 */
import { describe, expect, it } from "vitest";
import {
  FOLDER_COUNT_MAX,
  FOLDER_DEPTH_MAX,
  FOLDER_PATH_MAX,
  addFolder,
  hasProfilesUnder,
  mergeFolderPaths,
  normalizeFolderPath,
  parseFolders,
  removeFolder,
  renameFolder,
} from "./folders";

describe("normalizeFolderPath（S326）", () => {
  it("去空段/去空白/合并重复分隔符", () => {
    expect(normalizeFolderPath("  工作 / 生产 ")).toBe("工作/生产");
    expect(normalizeFolderPath("a//b///c")).toBe("a/b/c");
    expect(normalizeFolderPath("/leading/")).toBe("leading");
  });

  it("空、超深、含控制字符一律判非法（返回 null 而非猜一个）", () => {
    expect(normalizeFolderPath("")).toBeNull();
    expect(normalizeFolderPath("   ")).toBeNull();
    expect(normalizeFolderPath("///")).toBeNull();
    expect(normalizeFolderPath(Array(FOLDER_DEPTH_MAX + 1).fill("x").join("/"))).toBeNull();
    expect(normalizeFolderPath("ab")).toBeNull();
  });

  it("只认 `/` 分层——放行 `\\` 会让同一文件夹在两种写法下变成两个节点", () => {
    expect(normalizeFolderPath("a\\b")).toBe("a\\b"); // 反斜杠是名字的一部分，不是分隔符
  });
});

describe("mergeFolderPaths（并集 + 祖先补全）", () => {
  it("手动路径补全全部祖先层（缺祖先会让子目录挂到根上）", () => {
    expect(mergeFolderPaths(["工作/生产/华东"], [])).toEqual(["工作", "工作/生产", "工作/生产/华东"]);
  });

  it("与派生路径去重合并", () => {
    expect(mergeFolderPaths(["a/b"], ["a/b", "a/c"])).toEqual(["a", "a/b", "a/c"]);
  });

  it("非法条目跳过，不毁整棵树", () => {
    expect(mergeFolderPaths(["", "///", "ok"], [])).toEqual(["ok"]);
  });
});

describe("addFolder / removeFolder", () => {
  it("新增去重且保持有序；重复新建是幂等的（不报错）", () => {
    let r = addFolder([], "b");
    expect(r).toEqual({ list: ["b"], error: null });
    r = addFolder(r.list, "a");
    expect(r.list).toEqual(["a", "b"]);
    r = addFolder(r.list, "a");
    expect(r).toEqual({ list: ["a", "b"], error: null });
  });

  it("非法名与超上限带错因返回（原表不动）", () => {
    expect(addFolder(["x"], "  ").error).toContain("不合法");
    expect(addFolder(["x"], "  ").list).toEqual(["x"]);
    const full = Array.from({ length: FOLDER_COUNT_MAX }, (_, i) => `f${i}`);
    const r = addFolder(full, "one-more");
    expect(r.error).toContain("上限");
    expect(r.list).toHaveLength(FOLDER_COUNT_MAX);
  });

  it("删除连带手动子目录，但不碰同名前缀的兄弟（`a/b` 不该删掉 `a/bc`）", () => {
    const list = ["a", "a/b", "a/b/c", "a/bc"];
    expect(removeFolder(list, "a/b")).toEqual(["a", "a/bc"]);
  });
});

describe("hasProfilesUnder（删除提示的判据）", () => {
  it("路径下仍有会话时为真（含子层），无则为假", () => {
    const groups = ["工作/生产", "个人"];
    expect(hasProfilesUnder("工作", groups)).toBe(true);
    expect(hasProfilesUnder("工作/生产", groups)).toBe(true);
    expect(hasProfilesUnder("测试", groups)).toBe(false);
    // 同名前缀不算（`工作X` 下没有会话）
    expect(hasProfilesUnder("工作X", groups)).toBe(false);
  });
});

describe("parseFolders（读侧兜底）", () => {
  it("往返一致、去重排序、非法条目剔除", () => {
    expect(parseFolders(JSON.stringify(["b", "a", "a", "  ", "c/"]))).toEqual(["a", "b", "c"]);
  });

  it("烂值回落空表（目录结构坏了不该让侧栏不可用）", () => {
    expect(parseFolders(null)).toEqual([]);
    expect(parseFolders("nope")).toEqual([]);
    expect(parseFolders('{"a":1}')).toEqual([]);
  });
});

/**
 * 重命名（2026-08-22 补：M1/M4a 出口原文写的是「新建 / 重命名 / 删除」，而此前
 * 只有新建与删除——落勾时核的是「注记点名的测试在不在」，没核出口原文的每个子句）。
 *
 * 与删除的关键差别在**会话归属**：删除刻意不动会话（不可撤销的数据丢失），
 * 而重命名必须动——`group_path` 是归属的唯一依据，只改清单不改会话，
 * 那些会话会指向一个已不存在的名字，在树里由派生路径撑出一个「本该被改名的旧目录」。
 */
describe("renameFolder", () => {
  it("改名同时带走子目录与受影响会话的 group_path", () => {
    const list = ["工作", "工作/生产", "工作/测试", "个人"];
    const groups = ["工作", "工作/生产", "个人", null];
    const r = renameFolder(list, "工作", "线上", groups);
    expect(r.error).toBeNull();
    // 排序按码点，不是拼音："测"(U+6D4B) < "生"(U+751F)，故 线上/测试 在前
    expect(r.list).toEqual(["个人", "线上", "线上/测试", "线上/生产"]);
    // 会话映射：恰为该目录的与落在其下的都要改，别的不动
    expect(r.moves).toEqual([
      { from: "工作", to: "线上" },
      { from: "工作/生产", to: "线上/生产" },
    ]);
  });

  it("不改名时不产生任何 move（幂等，不是错误）", () => {
    const r = renameFolder(["a"], "a", "a", ["a"]);
    expect(r.error).toBeNull();
    expect(r.list).toEqual(["a"]);
    expect(r.moves).toEqual([]);
  });

  it("新名已存在 → 拒绝而不是静默合并（合并会把两批会话混在一起）", () => {
    const list = ["a", "b"];
    const r = renameFolder(list, "a", "b", []);
    expect(r.error).toContain("已存在");
    expect(r.list).toEqual(list); // 原表不动
    expect(r.moves).toEqual([]);
  });

  it("不能改名到自己的子目录（否则路径自我嵌套、旧节点永远删不掉）", () => {
    const r = renameFolder(["a", "a/b"], "a", "a/b/c", []);
    expect(r.error).toContain("子目录");
    expect(r.list).toEqual(["a", "a/b"]);
  });

  it("非法新名带错因返回，原表不动", () => {
    // 超长判据要真的越界：FOLDER_PATH_MAX = 512，故用 513（500 是合法长度，第一版写错了）
    for (const bad of ["", "   ", "/", "a".repeat(FOLDER_PATH_MAX + 1)]) {
      const r = renameFolder(["x"], "x", bad, []);
      expect(r.error, `新名 ${JSON.stringify(bad)} 应被拒`).toBeTruthy();
      expect(r.list).toEqual(["x"]);
    }
    // 原名不合法同样拒
    expect(renameFolder(["x"], "", "y", []).error).toContain("原文件夹名");
  });

  it("同名前缀的兄弟不受牵连（`a` 改名不动 `ab`）", () => {
    const list = ["a", "ab", "a/b"];
    const groups = ["a/b", "ab"];
    const r = renameFolder(list, "a", "z", groups);
    expect(r.list).toEqual(["ab", "z", "z/b"]);
    expect(r.moves).toEqual([{ from: "a/b", to: "z/b" }]);
    // `ab` 既不在清单改动里、也不在 moves 里
    expect(r.moves.some((m) => m.from === "ab")).toBe(false);
  });

  it("重复的 group_path 只产出一条 move（多个会话同组不重复下发）", () => {
    const r = renameFolder(["g"], "g", "h", ["g", "g", "g"]);
    expect(r.moves).toEqual([{ from: "g", to: "h" }]);
  });
});

/**
 * 装配层接线（M4a 出口「会话归属即时更新、重启持久」的**不可离线**那一半）。
 *
 * 上面 19 条覆盖的是纯函数：给定清单与 group_path 列表，算出新清单与该迁哪些会话。
 * 但出口原文里的两个承诺都落在 `App.svelte::renameFolderTo` 里，而那段是 IPC 调用序列：
 *
 *   * **重启持久** = 两侧都要落库。清单进 `sidebar.folders`（settings），
 *     会话的新归属逐条进 `profile_save`（DB）。少任何一侧，重启后就分叉：
 *     只存清单 ⇒ 目录改了名而会话还挂在旧名下，树里凭空多一个旧目录；
 *     只存会话 ⇒ 空目录的改名丢失（而「空目录也要能存在」正是这批手动文件夹的全部意义）。
 *   * **即时更新** = 迁完要 `loadProfiles()` 重取。不重取的话内存里的 `profiles`
 *     仍带旧 group_path，侧栏要等下一次别的操作才刷新——用户看到的是「改名没生效」。
 *
 * 这三件事没有一个能用纯函数测出来，而它们恰好是最容易在重构里掉的：
 * 纯函数的测试全绿，功能却坏了。故用一条源码扫查从外部把守整条链。
 * 与 `key_cmd.rs` 的 `the_agent_toggle_is_wired_all_the_way_through` 同一形态。
 */
describe("重命名的装配层接线（源码扫查，钉住纯函数测不到的三件事）", () => {
  /**
   * 按大括号配对取出一个函数的**真实**函数体。
   *
   * 第一版用的是「从函数名往后切 1400 字符」的固定窗口，而变异测试证明那不可靠：
   * 把 `renameFolderTo` 里的 `saveFolders(list)` 注释掉之后断言**仍然绿**——
   * 因为窗口越过了函数结尾，捞到了**邻居函数**里的同名调用。
   * 那种断言是「为了错误的理由而通过」，比不写更坏：它让人以为这条链有人守着。
   */
  const fnBody = (code: string, name: string): string => {
    const start = code.indexOf(name);
    if (start < 0) return "";
    const open = code.indexOf("{", start);
    if (open < 0) return "";
    let depth = 0;
    for (let i = open; i < code.length; i++) {
      const c = code[i];
      if (c === "{") depth++;
      else if (c === "}") {
        depth--;
        if (depth === 0) return code.slice(open, i + 1);
      }
    }
    return code.slice(open);
  };

  const read = async (rel: string) => {
    const { readFileSync } = await import("node:fs");
    const { join } = await import("node:path");
    const src = readFileSync(join(__dirname, rel), "utf8");
    // 剥注释：本测试要断言的那些串必然出现在解释接线的注释里，
    // 不剥就成了「注释给自己作证」。
    return src
      .replace(/<!--[\s\S]*?-->/g, "")
      .replace(/\/\*[\s\S]*?\*\//g, "")
      .replace(/(^|[^:])\/\/.*$/gm, "$1");
  };

  it("扫查自身有效（读到了真源码，不是空串让下面全绿）", async () => {
    const app = await read("../App.svelte");
    const sidebar = await read("../components/Sidebar.svelte");
    expect(app.length).toBeGreaterThan(10000);
    expect(sidebar.length).toBeGreaterThan(3000);
    expect(app).toContain("renameFolderTo");
    expect(sidebar).toContain("groupmenu-rename");
  });

  it("菜单项 → onRenameFolder → renameFolderFrom：三段都在，且菜单项不是禁用占位", async () => {
    const sidebar = await read("../components/Sidebar.svelte");
    const app = await read("../App.svelte");
    // 「功能做完了却忘记解禁入口」在本仓发生过四次（import.xshell、tools.schemeEditor、
    // tools.keyManager、工具栏截图按钮），所以这里显式反向断言这一项没有 disabled。
    const item = sidebar.slice(
      sidebar.indexOf('data-testid="groupmenu-rename"') - 400,
      sidebar.indexOf('data-testid="groupmenu-rename"') + 40,
    );
    expect(item).toContain("onRenameFolder");
    expect(item).not.toContain("disabled");
    expect(app).toContain("onRenameFolder={(path) => (renameFolderFrom = path)}");
  });

  it("重启持久①：清单落 settings 键 sidebar.folders", async () => {
    const app = await read("../App.svelte");
    // 只存会话不存清单 ⇒ 空目录的改名丢失，而空目录能存在正是手动文件夹的全部意义。
    expect(app).toContain('settingSet("sidebar.folders"');
    // renameFolderTo 里确实调了落库函数（而不是只改了内存里的 manualFolders）
    const body = fnBody(app, "async function renameFolderTo");
    expect(body).toContain("saveFolders(");
  });

  it("重启持久②：每个受影响会话的新 group_path 落 DB（profile_save）", async () => {
    const app = await read("../App.svelte");
    const body = fnBody(app, "async function renameFolderTo");
    // 只存清单不存会话 ⇒ 目录改了名而会话还挂在旧名下，树里凭空多出一个旧目录。
    expect(body).toContain('invoke("profile_save"');
    expect(body).toContain("group_path: to");
    // 迁的是纯函数算出来的那批，不是「所有会话」——后者会把无关会话的归属也改掉
    expect(body).toContain("moves");
  });

  it("即时更新：迁完重取 profiles", async () => {
    const app = await read("../App.svelte");
    const body = fnBody(app, "async function renameFolderTo");
    // 不重取的话内存里的 profiles 仍带旧 group_path，用户看到的是「改名没生效」。
    expect(body).toContain("loadProfiles()");
  });

  it("部分失败要说出来——不能沉默地留下一半改名", async () => {
    const app = await read("../App.svelte");
    const body = fnBody(app, "async function renameFolderTo");
    // 已迁的在新名下、没迁的在旧名下，两个目录都会显示。沉默会让用户
    // 以为「改名只生效了一半」是界面 bug，而其实是某次 profile_save 失败了。
    // 要的是**两条**失败路径各自有出口，而不是「函数里出现过 toast」。
    // 只断言后者时，删掉 profile_save 的错误提示仍然绿——因为拒绝路径那条
    // `toast.warn(error)` 还在，断言被它满足了。变异幸存过一次，故拆成两条。
    expect(body, "纯函数返回的拒绝原因（重名/非法/自嵌套）没有出口").toMatch(
      /toast\.warn\(\s*error\s*\)/,
    );
    expect(body, "单条会话迁移失败被静默吞掉了").toMatch(
      /toast\.error\([^)]*(未能更新|失败)/,
    );
  });
});
