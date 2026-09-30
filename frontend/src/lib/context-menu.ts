/**
 * 全局右键兜底的判定：**这一次右键该不该让 WebView 自己的菜单出来。**
 *
 * # 为什么单拎成一个纯函数
 *
 * 判定本身只有两行，但它是「用户觉得这软件像不像原生应用」的分界线，
 * 且有一个**极易被后人顺手改坏**的例外（输入框放行）。放在 App.svelte 的
 * 事件处理器里没法直接测——App 依赖整个应用状态；抽出来之后行为可逐条钉，
 * 接线另有一条源码守卫（见 context-menu.test.ts）。
 *
 * # 规则
 *
 * 默认吃掉。理由：右键此前只在少数组件上被显式处理（侧边栏条目、标签、
 * 工具栏、终端、RDP 画布），其余一律漏给 WebView——设置面板、传输列表、
 * AI 面板、SFTP 面板、以及所有空白区域，弹的都是「返回/重新加载/查看
 * 源代码/检查」这类浏览器菜单。除了出戏，其中「重新加载」被误点会把整个
 * 会话界面重置。
 *
 * **唯一放行文本输入区**（`input` / `textarea` / contenteditable）：那里的
 * 原生菜单是真有用的（剪切/复制/粘贴/全选，以及输入法与拼写建议入口），
 * 自己重造一套只会更差。这与 `App.svelte` 里 `onWindowKeydown` 的 `inField`
 * 同一条原则：输入框内让原生行为生效。两处口径必须一致，否则用户会觉得
 * 规则随机。
 *
 * **注意 `SELECT` 不在放行之列**（键盘那条口径里有）：下拉框的原生右键菜单
 * 是浏览器菜单，不是编辑菜单——放行它等于漏一个洞。
 */
export function shouldAllowNativeContextMenu(target: {
  tagName?: string;
  isContentEditable?: boolean;
}): boolean {
  if (target.isContentEditable === true) return true;
  const tag = target.tagName ?? "";
  return tag === "INPUT" || tag === "TEXTAREA";
}
