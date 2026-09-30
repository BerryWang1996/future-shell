<script lang="ts">
  /**
   * Icon.svelte — 16px 单色轮廓图标（2026-08-31 评审 P2-5）。
   *
   * # 为什么不用 emoji
   *
   * 工具栏与侧栏此前用 emoji 当图标（📋🔍📷⚡📢⚙✨🎨📁）。emoji 由**系统字体**
   * 渲染：Windows 是彩色 Segoe UI Emoji、macOS 是 Apple Color Emoji、Linux 各发行版
   * 又各不相同——颜色、字重、基线、实际尺寸全都不受控，同一排按钮在三个平台上高低
   * 参差、彩色图案混在单色 chrome 里。UI 规格要的是「16px 单色轮廓 SVG」，
   * 那不只是审美：单色才跟得上主题切换（currentColor），轮廓才在 hc 主题里看得清。
   *
   * # 形状约定
   *
   * 24×24 视图盒 + `stroke="currentColor"` + `fill="none"` + 1.7 线宽（对齐
   * Lucide/Feather 的画法，视觉重量与 12.5px 正文匹配）。填充型图标一律不收——
   * 混用填充与描边会让同一排按钮看起来像两套东西。
   *
   * 尺寸经 `size` 参数（默认 16）走 width/height，不用 CSS 缩放：SVG 描边宽度
   * 会跟着缩放变形，而按钮尺寸是密度系统的函数（--fs-chrome-h）。
   */
  let { name, size = 16, title = "" }: { name: string; size?: number; title?: string } = $props();

  /** 名称 → 路径。只存 `d`/元素串，不存整段 <svg>：外框统一在下面渲染。 */
  const PATHS: Record<string, string> = {
    // 会话/连接
    plus: '<path d="M12 5v14M5 12h14"/>',
    server: '<rect x="3" y="4" width="18" height="7" rx="1.5"/><rect x="3" y="13" width="18" height="7" rx="1.5"/><path d="M7 7.5h.01M7 16.5h.01"/>',
    folder: '<path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/>',
    // 编辑
    copy: '<rect x="9" y="9" width="11" height="11" rx="2"/><path d="M5 15V5a2 2 0 0 1 2-2h10"/>',
    paste: '<path d="M9 4h6a1 1 0 0 1 1 1v1H8V5a1 1 0 0 1 1-1z"/><path d="M8 6H6a2 2 0 0 0-2 2v11a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-2"/>',
    search: '<circle cx="11" cy="11" r="7"/><path d="m20 20-3.5-3.5"/>',
    camera: '<path d="M4 8h3l1.5-2h7L17 8h3a1 1 0 0 1 1 1v9a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1V9a1 1 0 0 1 1-1z"/><circle cx="12" cy="13" r="3.5"/>',
    // 工具
    bolt: '<path d="M13 2 4 14h7l-1 8 9-12h-7z"/>',
    megaphone: '<path d="M3 11v2a1 1 0 0 0 1 1h2l5 4V6L6 10H4a1 1 0 0 0-1 1z"/><path d="M16 8a5 5 0 0 1 0 8"/><path d="M19 5a9 9 0 0 1 0 14"/>',
    gear: '<circle cx="12" cy="12" r="3"/><path d="M19.4 15a1.6 1.6 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.6 1.6 0 0 0-1.8-.3 1.6 1.6 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1A1.6 1.6 0 0 0 9 19.4a1.6 1.6 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.6 1.6 0 0 0 .3-1.8 1.6 1.6 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1A1.6 1.6 0 0 0 4.6 9a1.6 1.6 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.6 1.6 0 0 0 1.8.3H9a1.6 1.6 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.6 1.6 0 0 0 1 1.5 1.6 1.6 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.6 1.6 0 0 0-.3 1.8V9a1.6 1.6 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.6 1.6 0 0 0-1.5 1z"/>',
    sparkles: '<path d="M12 3.5 13.6 8 18 9.5 13.6 11 12 15.5 10.4 11 6 9.5 10.4 8z"/><path d="M18.5 15.5 19.2 17.4 21 18l-1.8.6-.7 1.9-.7-1.9L16 18l1.8-.6z"/>',
    palette: '<path d="M12 3a9 9 0 1 0 0 18c1 0 1.6-.6 1.6-1.4 0-.4-.2-.8-.5-1.1-.3-.3-.5-.7-.5-1.1 0-.8.7-1.4 1.5-1.4H16a5 5 0 0 0 5-5c0-4.4-4-8-9-8z"/><circle cx="7.5" cy="11.5" r="1"/><circle cx="10.5" cy="7.5" r="1"/><circle cx="15" cy="8.5" r="1"/>',
    // 侧栏动作
    "arrow-down-tray": '<path d="M12 4v10m0 0 3.5-3.5M12 14l-3.5-3.5"/><path d="M5 17v2a1 1 0 0 0 1 1h12a1 1 0 0 0 1-1v-2"/>',
    "arrow-up-tray": '<path d="M12 20V10m0 0 3.5 3.5M12 10l-3.5 3.5"/><path d="M5 6V4a1 1 0 0 1 1-1h12a1 1 0 0 1 1 1v2"/>',
    warning: '<path d="M12 4.5 3.5 19h17z"/><path d="M12 10v4M12 17h.01"/>',
    "chevron-down": '<path d="m6 9 6 6 6-6"/>',
    "chevron-right": '<path d="m9 6 6 6-6 6"/>',
    // overlay 形态下叫回侧栏的入口（路线图 4c）：左栏 + 主区的「面板」轮廓
    "panel-left": '<rect x="3" y="4" width="18" height="16" rx="2"/><path d="M9 4v16"/>',
  };
</script>

<svg
  xmlns="http://www.w3.org/2000/svg"
  width={size}
  height={size}
  viewBox="0 0 24 24"
  fill="none"
  stroke="currentColor"
  stroke-width="1.7"
  stroke-linecap="round"
  stroke-linejoin="round"
  role={title ? "img" : "presentation"}
  aria-label={title || undefined}
  aria-hidden={title ? undefined : "true"}
  class="icon"
>{#if title}<title>{title}</title>{/if}{@html PATHS[name] ?? PATHS.warning}</svg>

<style>
  /* 与相邻文字基线对齐：按钮里是 grid place-items:center，这里只保证
     行内使用（如侧栏分组名前）时不把行高顶开。 */
  .icon { display: inline-block; vertical-align: -0.15em; flex: none; }
</style>
