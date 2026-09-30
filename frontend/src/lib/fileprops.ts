/**
 * 文件属性的呈现纯函数（M4a）。
 *
 * 抽出来的理由是「缺省值」这件事必须可被单测钉住：mode/uid/gid 在部分 SFTP 实现、
 * Windows 远端、以及本地 Windows 上**就是没有**，界面必须显示「—」而不是编一个
 * 0o644/0 出来 —— 用户看到具体数字就会当成真实权限，据此判断「我能不能写」。
 */

export type FileTypeStr = "regular" | "dir" | "symlink" | "other";

export interface FileMeta {
  file_type: FileTypeStr;
  size: number;
  mtime: number;
  mode: number | null;
  uid: number | null;
  gid: number | null;
}

export interface EntryDetail {
  path: string;
  meta: FileMeta;
  link_target: string | null;
  target_meta: FileMeta | null;
}

/** 未知一律「—」（不编默认值，见文件头）。 */
export const UNKNOWN = "—";

/**
 * POSIX mode 低 12 位 → `rwxr-xr-x (0755)`，含 setuid/setgid/sticky 的大写位。
 *
 * setuid/setgid/sticky 用 `s`/`s`/`t` 覆盖对应的 `x` 位，该位原本无 x 时用大写
 * `S`/`S`/`T`（`ls` 的惯例）——这个区分不是装饰：`S` 意味着「设了 setuid 但属主
 * 没有执行权」，通常是配错了，界面上必须能看出来。
 */
export function formatMode(mode: number | null | undefined): string {
  if (mode === null || mode === undefined || !Number.isFinite(mode)) return UNKNOWN;
  const m = mode & 0o7777;
  const bits = ["r", "w", "x"];
  let s = "";
  for (let g = 0; g < 3; g++) {
    const three = (m >> (6 - g * 3)) & 0o7;
    for (let b = 0; b < 3; b++) s += three & (4 >> b) ? bits[b] : "-";
  }
  const special = [
    { mask: 0o4000, pos: 2, low: "s", high: "S" }, // setuid → 属主 x 位
    { mask: 0o2000, pos: 5, low: "s", high: "S" }, // setgid → 组 x 位
    { mask: 0o1000, pos: 8, low: "t", high: "T" }, // sticky → 其他 x 位
  ];
  const arr = s.split("");
  for (const sp of special) {
    if (m & sp.mask) arr[sp.pos] = arr[sp.pos] === "x" ? sp.low : sp.high;
  }
  // 八进制固定四位（含特殊位那一位）：0644 / 4755 / 1777 宽度一致，肉眼可比对。
  // 不写成「0 + 三位」——那样 setuid 的 4755 会变成 04755，同一栏出现两种宽度。
  return `${arr.join("")} (${m.toString(8).padStart(4, "0")})`;
}

/** uid/gid → `1000:1000`；任一缺失即整体未知（半个属主没有意义）。 */
export function formatOwner(uid: number | null | undefined, gid: number | null | undefined): string {
  if (uid === null || uid === undefined || gid === null || gid === undefined) return UNKNOWN;
  return `${uid}:${gid}`;
}

export function typeLabel(t: FileTypeStr | string): string {
  switch (t) {
    case "regular":
      return "普通文件";
    case "dir":
      return "目录";
    case "symlink":
      return "符号链接";
    case "other":
      return "其他（设备/管道/套接字）";
    default:
      return `未知（${t}）`;
  }
}

/**
 * 「新建软链」的输入校验。
 *
 * 链名（在当前目录里创建的那个条目）不许带路径分隔符：面板的语义是「在**当前目录**
 * 建一个链」，允许 `a/b` 就等于允许往别处写，而那个「别处」用户在界面上看不见。
 * 目标可以是任意路径（相对链是常态，`../shared/x` 合法且常用），只要非空。
 */
export function validateSymlink(name: string, target: string): string | undefined {
  const n = name.trim();
  if (!n) return "链接名不能为空";
  if (n === "." || n === "..") return "链接名不能是 . 或 ..";
  if (n.includes("/") || n.includes("\\")) return "链接名不能包含路径分隔符（只在当前目录创建）";
  if (!target.trim()) return "链接目标不能为空";
  return undefined;
}
