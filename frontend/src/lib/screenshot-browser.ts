/**
 * 截图的浏览器侧落地（M4b「截图」）。
 *
 * `screenshot.ts` 保持纯（没有 DOM、没有 navigator），会出错的那些逻辑全在那边并且可测。
 * 这里只放三件**在 jsdom 里根本不存在**的事：造画布、写剪贴板、触发下载。
 */
import type { CaptureDeps } from "./screenshot";

/**
 * 写 PNG 到剪贴板。
 *
 * `ClipboardItem` 与 `navigator.clipboard.write` 在旧 WebView 上可能缺席，
 * 而缺席时的默认表现是抛一个 `undefined is not a constructor` 之类的话——
 * 用户看到那句话完全不知道发生了什么。先判断、再给一句能读懂的错。
 */
async function writeClipboard(blob: Blob): Promise<void> {
  if (typeof ClipboardItem === "undefined" || !navigator.clipboard?.write) {
    throw new Error("这个环境不支持把图片写进剪贴板，请改用「截图存为 PNG」");
  }
  await navigator.clipboard.write([new ClipboardItem({ "image/png": blob })]);
}

/**
 * 触发下载。
 *
 * 与导出配色走同一套做法（`scheme-file.ts` 的 `downloadJson`）。
 * 用 `<a download>` 而不是 Tauri 的保存对话框：后者要新增一个 IPC 命令与权限，
 * 而这里存的是一张自己刚画出来的图，没有理由比导出 JSON 更重。
 */
async function saveFile(blob: Blob, filename: string): Promise<void> {
  const url = URL.createObjectURL(blob);
  try {
    const a = document.createElement("a");
    a.href = url;
    a.download = filename;
    a.click();
  } finally {
    // finally 里放：click() 抛了也要释放，否则每次失败泄漏一个 blob。
    URL.revokeObjectURL(url);
  }
}

/** 生产环境的 deps。测试注入自己的。 */
export const browserCaptureDeps: CaptureDeps = {
  createCanvas(width, height) {
    const c = document.createElement("canvas");
    c.width = width;
    c.height = height;
    return c;
  },
  writeClipboard,
  saveFile,
  now: () => new Date(),
};
