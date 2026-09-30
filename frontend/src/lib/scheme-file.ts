/**
 * 配色的导出格式与存盘（M4b 出口「配色编辑器/主题目录：JSON 导出 round-trip 一致」）。
 *
 * 与 Rust 侧 `app/src/commands/import_cmd.rs` 的 `NativeSchemeFile` 一一对应。
 * 导出的文件由**同一个**「文件 → 导入」入口读回来（后端 `ForeignKind::NativeScheme`）——
 * 为自家格式单开一个菜单项等于把内部分类摆到界面上让用户去分辨。
 */
import type { StoredCustomScheme } from "./term-schemes";

/**
 * 格式标识与版本。两个常量都必须与 Rust 侧逐字一致，否则导出的文件自己读不回来——
 * 而那正是这条出口要验的东西。`scheme-file.test.ts` 读 Rust 源码比对。
 */
export const NATIVE_SCHEME_FORMAT = "future-shell-color-schemes";
export const NATIVE_SCHEME_VERSION = 1;

export interface NativeSchemeFile {
  format: string;
  version: number;
  schemes: StoredCustomScheme[];
}

/**
 * 打包成导出文件的形状。
 *
 * `cursor` 缺省时**写 null 而不是省略字段**：Rust 的 `Option<String>` 两种都能读，
 * 但省略会让两次导出的同一套配色产生不同的字节，而「导出→导入→再导出」应当是幂等的。
 */
export function toExportFile(schemes: StoredCustomScheme[]): NativeSchemeFile {
  return {
    format: NATIVE_SCHEME_FORMAT,
    version: NATIVE_SCHEME_VERSION,
    schemes: schemes.map((s) => ({
      name: s.name,
      foreground: s.foreground,
      background: s.background,
      ansi: [...s.ansi],
      cursor: s.cursor ?? null,
    })),
  };
}

/** 序列化。缩进 2 空格——导出的配色是给人看、给人手改的，压成一行没有好处。 */
export function serializeSchemes(schemes: StoredCustomScheme[]): string {
  return JSON.stringify(toExportFile(schemes), null, 2);
}

/**
 * 文件名。
 *
 * 单套用配色自己的名字（用户导出一套是为了发给别人或存起来，`Dracula.json` 认得出），
 * 多套用一个带日期的通用名。名字里的路径分隔符与保留字符要剔掉——
 * 配色名是用户起的，`../` 或 `:` 进了 download 属性会得到一个意外的落点或一个失败的下载。
 */
export function exportFileName(schemes: StoredCustomScheme[], stamp: string): string {
  const safe = (raw: string): string => {
    const cleaned = raw.trim().replace(/[\\/:*?"<>|]/g, "_");
    // 兜底的判据是「**剩下的东西还认得出是个名字吗**」，不是「是不是空串」。
    // 只判空串的话，一个叫 `///` 的配色会导出成 `___.json` ——合法，但用户在
    // 下载目录里认不出那是什么。要求至少有一个既不是下划线也不是点的字符。
    return /[^_.]/.test(cleaned) ? cleaned : "配色";
  };
  return schemes.length === 1
    ? `${safe(schemes[0].name)}.json`
    : `color-schemes-${stamp}.json`;
}

/**
 * 触发浏览器下载。
 *
 * 与 App.svelte 里导出连接用的那个是同一套做法。抽到这里是因为配色导出在
 * SettingsDialog 里，两处各写一份迟早会有一处忘了 revokeObjectURL——
 * 那是每导出一次泄漏一个 blob，长会话里会攒起来。
 */
export function downloadJson(json: string, filename: string): void {
  const url = URL.createObjectURL(new Blob([json], { type: "application/json" }));
  const a = document.createElement("a");
  a.href = url;
  a.download = filename;
  a.click();
  URL.revokeObjectURL(url);
}
