/**
 * 文件选择器的纯逻辑（路径、排序、多选）。
 *
 * 与 `zmodem.ts` 同一个分工：可离线测的东西全在这里，组件只负责渲染与 invoke。
 * 这样「点了 Shift 之后该选中哪几个」可以钉死，而不必起一个真会话、真目录树。
 *
 * # 路径是**相对浏览根**的，不是绝对路径
 *
 * 后端 `local_list`（`app/src/commands/sftp_cmd.rs`）的语义是：空串 = 用户家目录，
 * 相对路径以家目录解析，canonicalize 之后必须仍在家目录内。所以选择器全程活在
 * 「相对家目录」这个空间里，空串就是根。
 *
 * 这一点决定了下面每个函数的边界条件：**根是空串，不是 `/` 也不是 `C:\`**。
 * 把根当成 `"/"` 处理会让 `parentOf` 在根目录返回 `""` 而不是 `null`，
 * 于是「上级」按钮永远是可点的，点了又什么都不发生——那正是本次要修的那类 bug。
 *
 * # 分隔符两种都要认
 *
 * `joinPath` 从旧代码搬过来时保持了原行为：目录串里出现过 `\` 就用 `\`，
 * 否则用 `/`。于是同一个 cwd 里可能混着两种（根是空串 → 第一层用 `/`，
 * 而 Windows 上后端给回的名字里不会有分隔符，所以实际只会是纯 `/`）。
 * 但**不能假设**只有一种：用户从别处粘一个路径进来就会破这个假设，
 * 而那时的表现是「上级」跳到一个不存在的地方。
 */

/** 目录与名字拼成路径。空目录（浏览根）时直接返回名字。 */
export function joinPath(dir: string, name: string): string {
  if (!dir) return name;
  const sep = dir.includes("\\") ? "\\" : "/";
  return dir.endsWith(sep) ? `${dir}${name}` : `${dir}${sep}${name}`;
}

/**
 * 拆成路径段（两种分隔符都认，空段丢弃）。
 *
 * 丢空段是必需的：`"docs//sub"` 或结尾的 `"docs/"` 都会产生空段，
 * 而一个空段会在面包屑上显示成一个没有文字的按钮，点下去跳到重复路径。
 */
export function splitSegments(path: string): string[] {
  return path.split(/[\\/]+/).filter((s) => s.length > 0);
}

/** 面包屑一段。`path` 是点它之后要跳去的路径。 */
export interface Crumb {
  label: string;
  path: string;
}

/** 浏览根在面包屑上的显示名。 */
export const ROOT_LABEL = "主目录";

/**
 * 面包屑。**第一段恒为浏览根**，所以任何位置都有一键回根的入口。
 *
 * 每一段的 `path` 用原串的分隔符重建，而不是统一成 `/`：
 * 那个路径会原样发回 `local_list`，换分隔符虽然后端也认，
 * 但会让 cwd 在用户点面包屑前后长得不一样，调试时平添一层困惑。
 */
export function splitBreadcrumbs(path: string): Crumb[] {
  const out: Crumb[] = [{ label: ROOT_LABEL, path: "" }];
  const segs = splitSegments(path);
  const sep = path.includes("\\") ? "\\" : "/";
  let acc = "";
  for (const s of segs) {
    acc = acc ? `${acc}${sep}${s}` : s;
    out.push({ label: s, path: acc });
  }
  return out;
}

/**
 * 上级路径；**已在浏览根时返回 `null`**。
 *
 * 返回 null 而不是空串，是为了让调用方能区分「上级是根」与「已经在根了」。
 * UI 据此禁用「上级」按钮——一个点了没反应的按钮比一个禁用的按钮更让人困惑，
 * 因为前者会让人怀疑是不是卡住了。
 */
export function parentOf(path: string): string | null {
  const segs = splitSegments(path);
  if (segs.length === 0) return null;
  if (segs.length === 1) return "";
  const sep = path.includes("\\") ? "\\" : "/";
  return segs.slice(0, -1).join(sep);
}

/** `local_list` 返回的一条。 */
export interface PickerEntry {
  name: string;
  is_dir: boolean;
  size: number;
}

/**
 * 目录在前，再按名字排。
 *
 * 用 `localeCompare` 而不是 `<`：后者按 UTF-16 码元比，中文名会排成一个
 * 用户完全看不出规律的顺序（而这个程序的用户大量使用中文文件名）。
 *
 * **不在这里做去重或过滤**：后端给什么就显示什么。前端悄悄少显示一条，
 * 用户会以为文件不存在。
 */
export function sortEntries(entries: PickerEntry[]): PickerEntry[] {
  return [...entries].sort((a, b) => {
    if (a.is_dir !== b.is_dir) return a.is_dir ? -1 : 1;
    return a.name.localeCompare(b.name);
  });
}

/** 一次点击的修饰键状态。 */
export interface ClickMods {
  ctrl: boolean;
  shift: boolean;
}

/** 多选归约的输入输出。锚点要在调用之间保留，故随选择集一起进出。 */
export interface Selection {
  /** 已选中的文件名 */
  names: Set<string>;
  /**
   * 区间选择的锚点：**最后一次非 Shift 点击**的那一条。
   *
   * 必须记住它：Shift 的语义是「从锚点到这里」，而不是「从上一次点击到这里」。
   * 用上一次点击当锚点的话，连续按两次 Shift 会把区间起点一路挪走，
   * 用户看到的是选区在自己长大。
   */
  anchor: string | null;
}

export const EMPTY_SELECTION: Selection = { names: new Set(), anchor: null };

/**
 * 归约一次点击。
 *
 * - 无修饰键：替换成单选，并把锚点挪到这里；
 * - Ctrl：切换本条，锚点挪到这里（与资源管理器一致）；
 * - Shift：选中锚点到本条的**闭区间**（按 `ordered` 的顺序），锚点不动。
 *   无锚点时退化成单选。
 *
 * `ordered` 必须是**当前显示顺序**（已过 `sortEntries` 且只含可选条目）。
 * 传原始顺序会让 Shift 选出一段用户在屏幕上看不出连续性的条目。
 */
export function reduceSelection(
  prev: Selection,
  name: string,
  mods: ClickMods,
  ordered: string[],
): Selection {
  if (mods.shift && prev.anchor !== null) {
    const a = ordered.indexOf(prev.anchor);
    const b = ordered.indexOf(name);
    if (a >= 0 && b >= 0) {
      const [lo, hi] = a <= b ? [a, b] : [b, a];
      return { names: new Set(ordered.slice(lo, hi + 1)), anchor: prev.anchor };
    }
    // 锚点已不在当前列表里（换了目录、或被过滤掉）：退化成单选，
    // 而不是静默选出一个空区间——空区间看起来像「点了没反应」。
    return { names: new Set([name]), anchor: name };
  }
  if (mods.ctrl) {
    const next = new Set(prev.names);
    if (next.has(name)) next.delete(name);
    else next.add(name);
    return { names: next, anchor: name };
  }
  return { names: new Set([name]), anchor: name };
}

/** 已选条目的总字节数（底部「已选 N 个（共 X）」用）。 */
export function selectedBytes(entries: PickerEntry[], names: Set<string>): number {
  let sum = 0;
  for (const e of entries) {
    if (!e.is_dir && names.has(e.name)) sum += e.size;
  }
  return sum;
}
