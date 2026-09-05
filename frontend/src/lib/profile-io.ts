/**
 * profile-io.ts — 连接档案的导入 / 导出（路线图 4c「App.svelte 拆分」，2026-09-03 从 App.svelte 搬出）。
 *
 * 搬出来的理由不是「App 太长」，是这四件事**与 App 的组件状态无关**：它们只用 IPC、toast 与
 * 浏览器的文件 API，唯一的外部依赖是「导入成功后请重新载入档案列表」这一个回调。
 * 留在 App 里时它们只能靠渲染整个 App 才测得到；搬出来之后可以直接对着函数测。
 *
 * 两个入口的历史教训原样留在各函数的注释里——它们是这些函数为什么是**具名函数**而不是
 * 内联 prop 的原因（内联 prop 那次，菜单项无处可接，点击静默无事）。
 */
import { invoke } from "./ipc";
import { toast } from "./toast";
import type { Profile } from "./types";

/** 文件名里的日期戳（导出文件按天区分，避免同名覆盖）。 */
export function todayStamp(now: Date = new Date()): string {
  return now.toISOString().slice(0, 10);
}

/** 把一段 JSON 交给浏览器下载。 */
export function saveJsonFile(json: string, filename: string): void {
  const url = URL.createObjectURL(new Blob([json], { type: "application/json" }));
  const a = document.createElement("a");
  a.href = url;
  a.download = filename;
  a.click();
  URL.revokeObjectURL(url);
}

/** 导入上限：与 Rust 侧 `import_json` 解析前那道尺寸闸同值（8 MiB）。 */
export const IMPORT_MAX_BYTES = 8 * 1024 * 1024;

/**
 * 导入 JSON。具名函数是因为它有**两个**入口：侧栏 ⤵ 按钮与菜单「文件 → 导入(JSON)」。
 * 原先只作为 Sidebar 的内联 prop 存在，菜单项 `import.json` 便无处可接，点击静默无事。
 *
 * `reload` = 导入成功后重新载入档案列表（App 的 loadProfiles）。
 */
export function importProfilesFromFile(reload: () => Promise<void> | void): void {
  const input = document.createElement("input");
  input.type = "file";
  input.accept = ".json";
  // P1-13：Rust 侧签名是 profiles_import(content: String, ..)，原先发的键名是 json ——
  // Tauri 按形参名取实参，键名对不上直接报「missing required key content」，导入功能从来没能用过。
  // 另：try/catch 原本套在 input.click() 外层，而失败发生在异步 onchange 回调里，永远捕不到；
  // 用户看到的是「点了没反应」。故 try/catch 内移到回调内部。
  input.onchange = async (e) => {
    const file = (e.target as HTMLInputElement).files?.[0];
    if (!file) return;
    // 审计2 #37 纵深防御：真正的闸在 Rust 侧（import_json 解析前量尺寸），
    // 这里先挡一次，免得把超大文件整份读进内存、推过 IPC 再被后端拒。
    if (file.size > IMPORT_MAX_BYTES) {
      toast.error("导入失败：文件超过 8 MiB 上限");
      return;
    }
    try {
      const text = await file.text();
      const imported = await invoke<number>("profiles_import", { content: text });
      await reload();
      toast.info(`导入成功，共 ${imported} 条`);
    } catch (err) {
      toast.error(`导入失败: ${err}`);
    }
  };
  input.click();
}

/** 全量导出。同上，两个入口：侧栏 ⤴ 按钮与菜单「文件 → 导出」。 */
export async function exportAllProfiles(): Promise<void> {
  try {
    // ids 省略 = 全量（Rust `profiles_export(ids: Option<Vec<String>>)`）
    const json = await invoke<string>("profiles_export");
    saveJsonFile(json, `profiles-${todayStamp()}.json`);
    toast.info("导出成功");
  } catch (e) {
    toast.error(`导出失败: ${e}`);
  }
}

/**
 * 右键菜单「导出」：只导出这一条连接。
 * Sidebar 早就声明并调用了 `onExportOne(p)`，但 App 从未传这个 prop——props 的默认值是空函数，
 * 于是点击静默无事，且没有任何编译期或运行期信号。Rust 侧 `profiles_export(ids)` 本就支持子集。
 */
export async function exportOneProfile(p: Profile): Promise<void> {
  try {
    const json = await invoke<string>("profiles_export", { ids: [p.id] });
    saveJsonFile(json, `${safeFileStem(p.name)}-${todayStamp()}.json`);
    toast.info(`已导出连接 ${p.name}`);
  } catch (e) {
    toast.error(`导出失败: ${e}`);
  }
}

/**
 * 档案名 → 可用作文件名的词干。
 *
 * 路径分隔符与 Windows 保留字符会让保存直接失败，逐一替换成 `_`；全被替换掉或本来就是空白时
 * 回落 `profile`——空词干拼出来的是 `-2026-09-03.json` 这种以横杠打头的怪名字。
 */
export function safeFileStem(name: string): string {
  return name.replace(/[\\/:*?"<>|]/g, "_").trim() || "profile";
}
