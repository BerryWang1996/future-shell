/**
 * 会话树手动文件夹（M4a：手动新建文件夹/子组；UI 规格 §1.1）。
 *
 * MVP 的分组是**从 `group_path` 派生**的：有会话在 `工作/生产` 下，树里才有这两层。
 * 于是「先建好目录结构再往里拖」做不到——空文件夹没有任何存身之处，新建完立刻消失。
 *
 * 本模块补上那份「手动建出来的文件夹」清单（settings 键 `sidebar.folders`），与派生
 * 路径**并集**成树。一旦有会话搬进去，派生路径自然覆盖同一条；清单里那条留着也无害
 * （去重后是同一个节点），并且在最后一个会话搬走后目录仍在——这正是用户手动建它的
 * 意图：目录结构是他的组织方式，不该随会话增减自动消失。
 */

/** 单条路径的最大层数与总长（与 Rust 侧 group_path 的产品边界同源：GROUP_PATH_MAX 512）。 */
export const FOLDER_PATH_MAX = 512;
export const FOLDER_DEPTH_MAX = 8;
export const FOLDER_COUNT_MAX = 200;

/**
 * 规范化一条文件夹路径：去空段、去首尾空白、合并重复分隔符。
 * 返回 null = 不是合法路径（空、超长、超深、含控制字符）。
 *
 * 分隔符只认 `/`：`group_path` 全仓以 `/` 分层（Sidebar.buildTree、Rust 侧校验
 * 皆然），放行 `\` 会让同一个文件夹在两种写法下变成两个节点。
 */
export function normalizeFolderPath(raw: string): string | null {
  const segs = raw
    .split("/")
    .map((s) => s.trim())
    .filter(Boolean);
  if (segs.length === 0 || segs.length > FOLDER_DEPTH_MAX) return null;
  if (segs.some((s) => [...s].some((c) => c.charCodeAt(0) < 0x20))) return null;
  const path = segs.join("/");
  return path.length > FOLDER_PATH_MAX ? null : path;
}

/**
 * 合并手动文件夹与派生路径，产出**建树用的完整路径集**（含所有祖先层）。
 *
 * 祖先补全是必须的：用户建 `工作/生产/华东` 时只输了一条路径，但树要渲染三层——
 * 缺了祖先，`华东` 会挂在根上，用户看到的层级与他输入的不符。
 */
export function mergeFolderPaths(manual: string[], derived: string[]): string[] {
  const set = new Set<string>();
  for (const p of [...manual, ...derived]) {
    const norm = normalizeFolderPath(p);
    if (!norm) continue;
    const segs = norm.split("/");
    segs.forEach((_, i) => set.add(segs.slice(0, i + 1).join("/")));
  }
  return [...set].sort();
}

/** 新增一条手动文件夹；已存在则原样返回（不报错——重复新建是无害的幂等操作）。 */
export function addFolder(list: string[], raw: string): { list: string[]; error: string | null } {
  const norm = normalizeFolderPath(raw);
  if (!norm) return { list, error: "文件夹名不合法（空、过深或过长）" };
  if (list.includes(norm)) return { list, error: null };
  if (list.length >= FOLDER_COUNT_MAX) {
    return { list, error: `手动文件夹数量已达上限 ${FOLDER_COUNT_MAX}` };
  }
  return { list: [...list, norm].sort(), error: null };
}

/**
 * 删除一条手动文件夹**及其手动子目录**。
 *
 * 只删清单里的条目——**不动任何会话**：删文件夹不该连带删掉里面的连接配置
 * （那是不可撤销的数据丢失，而用户的意图多半只是「收拾一下目录」）。若仍有会话的
 * `group_path` 落在该路径下，树里那个节点会继续由派生路径撑着，看起来「没删掉」——
 * 这是**如实反映**：会话还在那儿，目录当然还在。UI 需就此给出提示。
 */
export function removeFolder(list: string[], path: string): string[] {
  const norm = normalizeFolderPath(path);
  if (!norm) return list;
  return list.filter((p) => p !== norm && !p.startsWith(`${norm}/`));
}

/** 该路径下是否仍有会话（决定删除后节点会不会由派生路径继续存在）。 */
export function hasProfilesUnder(path: string, groupPaths: (string | null | undefined)[]): boolean {
  const norm = normalizeFolderPath(path);
  if (!norm) return false;
  return groupPaths.some((g) => {
    const gp = (g ?? "").trim();
    return gp === norm || gp.startsWith(`${norm}/`);
  });
}

/**
 * 重命名一条手动文件夹（M1 出口原文「新建 / 重命名 / 删除」的那一半，2026-08-22 补）。
 *
 * 写法注意：块注释里不要把星号紧挨斜杠（那会拼出注释结束符，整个文件当场语法崩）。
 * 本文件与它的测试文件各栽过一次，故这里一律用空格分隔的「新建 / 重命名 / 删除」。
 *
 * 与删除的关键差别：删除**不动会话**（那是不可撤销的数据丢失，用户多半只想收拾目录），
 * 而重命名**必须动会话**——`group_path` 是会话归属的唯一依据，只改清单不改会话，
 * 那些会话会当场变成孤儿：它们的 `group_path` 指向一个已不存在的名字，于是在树里
 * 由派生路径撑出一个「本该被改名的旧目录」，用户看到的是「改了个名字，结果多出一个」。
 *
 * 故返回两样东西：新的清单，以及**需要改 group_path 的会话映射**（旧路径 → 新路径），
 * 由调用方落库。子目录一并跟随（`a` → `x` 时 `a/b` 变 `x/b`）。
 *
 * 判空与冲突：新名不合法 → 原样返回并带错因；新名已存在 → 拒绝（合并两个目录是另一件事，
 * 静默合并会让用户以为只是改名，实际把两批会话混在了一起）。
 */
export function renameFolder(
  list: string[],
  from: string,
  to: string,
  groupPaths: (string | null | undefined)[] = [],
): { list: string[]; moves: { from: string; to: string }[]; error: string | null } {
  const src = normalizeFolderPath(from);
  const dst = normalizeFolderPath(to);
  if (!src) return { list, moves: [], error: "原文件夹名不合法" };
  if (!dst) return { list, moves: [], error: "新文件夹名不合法（空、过深或过长）" };
  if (src === dst) return { list, moves: [], error: null }; // 没改，不算错
  // 不许把一个目录改名成自己的子目录（`a` → `a/b`）：那会让路径自我嵌套，
  // 每次重命名都再套一层，且旧节点永远删不掉。
  if (dst.startsWith(`${src}/`)) {
    return { list, moves: [], error: "不能把文件夹改名到它自己的子目录下" };
  }
  if (list.includes(dst)) {
    return { list, moves: [], error: `已存在同名文件夹「${dst}」（改名不做合并）` };
  }

  const rewrite = (p: string): string =>
    p === src ? dst : p.startsWith(`${src}/`) ? dst + p.slice(src.length) : p;

  const nextList = [...new Set(list.map(rewrite))].sort();

  // 受影响的会话：group_path 恰为 src 或落在 src/ 之下
  const moves: { from: string; to: string }[] = [];
  const seen = new Set<string>();
  for (const g of groupPaths) {
    const gp = (g ?? "").trim();
    if (!gp || seen.has(gp)) continue;
    if (gp === src || gp.startsWith(`${src}/`)) {
      seen.add(gp);
      moves.push({ from: gp, to: rewrite(gp) });
    }
  }

  return { list: nextList, moves, error: null };
}

/** 解析持久化的手动文件夹清单。烂值回落空表（目录结构坏了不该让侧栏不可用）。 */
export function parseFolders(json: string | null | undefined): string[] {
  if (!json) return [];
  try {
    const v = JSON.parse(json);
    if (!Array.isArray(v)) return [];
    const out: string[] = [];
    for (const x of v) {
      if (typeof x !== "string") continue;
      const norm = normalizeFolderPath(x);
      if (norm && !out.includes(norm)) out.push(norm);
      if (out.length >= FOLDER_COUNT_MAX) break;
    }
    return out.sort();
  } catch {
    return [];
  }
}
