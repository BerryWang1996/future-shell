import { mount } from "svelte";
import App from "./App.svelte";
import ViewWindow from "./ViewWindow.svelte";
import "./lib/theme/tokens.css";
import "./styles.css";
import { initTheme } from "./lib/theme/store";
import { reportFrontendError } from "./lib/ipc";
import { isViewWindow } from "./lib/view-window";

// 全局兜底：未捕获异常与未处理 rejection 落进后端日志（S300）。
//
// 改造前全前端零全局处理器——WebView 里抛出的任何异常只进 devtools 控制台，
// 而用户跑的是打包版，控制台没人看。终端渲染回路上的异常尤其致命：它会静默
// 丢帧，而丢帧曾等于永久冻结会话（见 Rust 侧 fs_terminal::flow 模块头）。
// 装在 mount 之前，覆盖组件初始化期间的抛出。
window.addEventListener("error", (e) => {
  reportFrontendError("window.onerror", e.error ?? e.message);
});
window.addEventListener("unhandledrejection", (e) => {
  reportFrontendError("unhandledrejection", e.reason);
});

// 主题尽早应用，避免首帧闪烁（settings 命令未就绪时回落默认 Obsidian）
void initTheme();

/**
 * 视图窗口 vs 主窗口（M4b「标签拖出/平铺」）。
 *
 * 判据是 URL 上有没有 `?view=`——由 Rust 侧 `window_new` 拼进去。
 * 在**入口**分叉而不是在 App.svelte 里加分支，是因为 App 装配的是整个主界面
 * （会话管理器、标签栏、工具栏、状态栏、十几个模态、恢复对话框、拖拽上传……）。
 * 在它里面加一个「这次什么都不要渲染」的分支，等于让那一整套装配逻辑在一个
 * 它从没被设计过的形态下运行；而在这里分叉，视图窗口从第一帧起就只有一个终端。
 *
 * 一个纯 URL 判据还有个附带好处：开发时直接在浏览器地址栏加 `?view=xxx`
 * 就能看视图窗口的样子，不需要真的去拖一个标签出来。
 */
const app = mount(isViewWindow(window.location.search) ? ViewWindow : App, {
  target: document.getElementById("app")!,
});
export default app;
