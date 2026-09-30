<script lang="ts">
  /**
   * 一个应用内虚拟窗口的外壳（M7.3 第三条出口）：标题栏拖动、右下角改尺寸、点击置顶、关闭。
   *
   * # 几何全部经 lib/vwindow.ts
   *
   * 本组件**不自己算最终位置**：拖动时把「指针位置 − 起始偏移」交给 `moveVWindow`，由它过
   * `clampRect` 夹进视口。出口标准里那句「窗口不可拖出可视区外」只有一个可靠的实现方式，
   * 就是让所有写入都走同一个夹紧函数——组件自己就地算一次、再顺手写进 store 的写法，
   * 迟早有一条路径漏掉夹紧，而那条路径造出的窗口**找不回来**（虚拟窗口没有任务栏、
   * 没有 Alt+Tab、没有「窗口」菜单）。
   *
   * # 为什么用 Pointer Events 而不是 mousedown/mousemove
   *
   * 一套代码同时管鼠标、触摸与手写笔，且 `setPointerCapture` 保证指针移出窗口（甚至移出
   * WebView）时事件照旧回到本元素——用 mousemove + document 监听器的写法在快速拖动时会
   * 掉事件，表现为「窗口跟不上鼠标然后卡住」。三平台一致也是这么来的：全在 WebView 里算，
   * 不经过任何窗口管理器。
   *
   * # 键盘同样能移动和缩放
   *
   * 只能拖 = 只能用鼠标。标题栏可聚焦，方向键移动（Shift 加速），`Ctrl+方向键`改尺寸。
   * 这不是可选的润色：一个只能用指针操作的窗口对键盘用户等于不存在。
   */
  import type { Snippet } from "svelte";
  import { closeVWindow, moveVWindow, raiseVWindow, resizeVWindow, type VWindow, type Viewport } from "../lib/vwindow";

  let {
    win,
    /** 画布尺寸（虚拟窗口的活动范围）。夹紧以它为准。 */
    viewport,
    children,
  }: {
    win: VWindow;
    viewport: Viewport;
    /** 窗口内容。可缺省：外壳本身（拖动/缩放/置顶/关闭）不依赖内容，
     *  缺内容时渲染一个空壳而不是整窗抛错——外壳的行为要能单独测。 */
    children?: Snippet;
  } = $props();

  /** 拖动起点与窗口左上角的差；null = 没在拖。 */
  let dragOff: { dx: number; dy: number } | null = null;
  /** 缩放起点：指针位置与当时的尺寸。 */
  let resizeFrom: { px: number; py: number; w: number; h: number } | null = null;

  const KEY_STEP = 8;
  const KEY_STEP_FAST = 40;

  function onTitlePointerDown(e: PointerEvent) {
    // 只接左键/主指针：中键与右键在标题栏上拖窗口是没人预期的行为
    if (e.button !== 0) return;
    raiseVWindow(win.id);
    dragOff = { dx: e.clientX - win.x, dy: e.clientY - win.y };
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    e.preventDefault(); // 否则标题文字会被选中，拖出一条蓝色选区
  }

  function onTitlePointerMove(e: PointerEvent) {
    if (!dragOff) return;
    moveVWindow(win.id, e.clientX - dragOff.dx, e.clientY - dragOff.dy, viewport);
  }

  function onTitlePointerUp(e: PointerEvent) {
    if (!dragOff) return;
    dragOff = null;
    (e.currentTarget as HTMLElement).releasePointerCapture?.(e.pointerId);
  }

  function onGripPointerDown(e: PointerEvent) {
    if (e.button !== 0) return;
    raiseVWindow(win.id);
    resizeFrom = { px: e.clientX, py: e.clientY, w: win.w, h: win.h };
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    e.preventDefault();
  }

  function onGripPointerMove(e: PointerEvent) {
    if (!resizeFrom) return;
    resizeVWindow(
      win.id,
      resizeFrom.w + (e.clientX - resizeFrom.px),
      resizeFrom.h + (e.clientY - resizeFrom.py),
      viewport,
    );
  }

  function onGripPointerUp(e: PointerEvent) {
    if (!resizeFrom) return;
    resizeFrom = null;
    (e.currentTarget as HTMLElement).releasePointerCapture?.(e.pointerId);
  }

  /** 标题栏上的键盘操作：方向键移动，Ctrl+方向键改尺寸，Shift 加速。 */
  function onTitleKeydown(e: KeyboardEvent) {
    const step = e.shiftKey ? KEY_STEP_FAST : KEY_STEP;
    const d = { ArrowLeft: [-1, 0], ArrowRight: [1, 0], ArrowUp: [0, -1], ArrowDown: [0, 1] }[e.key];
    if (!d) return;
    e.preventDefault();
    if (e.ctrlKey) resizeVWindow(win.id, win.w + d[0] * step, win.h + d[1] * step, viewport);
    else moveVWindow(win.id, win.x + d[0] * step, win.y + d[1] * step, viewport);
  }
</script>

<!-- 整窗按下即置顶：用户点窗口里任何地方的意思都是「我现在要用这个」。
     用 pointerdown 的捕获相位（onpointerdowncapture）而不是冒泡：内容里的控件可能
     stopPropagation，那样点了内容就不置顶，窗口会一直压在别人下面。 -->
<div
  class="vwin"
  role="dialog"
  aria-label={win.title}
  data-testid="vwin-{win.id}"
  style="left: {win.x}px; top: {win.y}px; width: {win.w}px; height: {win.h}px; z-index: {win.z};"
  onpointerdowncapture={() => raiseVWindow(win.id)}
>
  <div
    class="title"
    role="toolbar"
    tabindex="0"
    aria-label="{win.title} 窗口标题栏（方向键移动，Ctrl+方向键改尺寸）"
    data-testid="vwin-title-{win.id}"
    onpointerdown={onTitlePointerDown}
    onpointermove={onTitlePointerMove}
    onpointerup={onTitlePointerUp}
    onpointercancel={onTitlePointerUp}
    onkeydown={onTitleKeydown}
  >
    <span class="label">{win.title}</span>
    <button
      class="close"
      title="关闭窗口"
      aria-label="关闭 {win.title}"
      data-testid="vwin-close-{win.id}"
      onclick={() => closeVWindow(win.id)}
    >×</button>
  </div>
  <div class="body">{@render children?.()}</div>
  <div
    class="grip"
    role="slider"
    tabindex="-1"
    aria-label="改变 {win.title} 的大小"
    aria-valuenow={win.w}
    aria-valuemin={0}
    aria-valuemax={viewport.width}
    data-testid="vwin-grip-{win.id}"
    onpointerdown={onGripPointerDown}
    onpointermove={onGripPointerMove}
    onpointerup={onGripPointerUp}
    onpointercancel={onGripPointerUp}
  ></div>
</div>

<style>
  .vwin {
    position: absolute;
    display: flex;
    flex-direction: column;
    min-width: 0;
    background: var(--fs-bg-elevated);
    border: 1px solid var(--fs-border-strong);
    border-radius: var(--fs-radius);
    box-shadow: var(--fs-shadow);
    color: var(--fs-fg-primary);
    overflow: hidden;
  }
  .title {
    flex: none;
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 4px 6px 4px 10px;
    background: var(--fs-bg-panel);
    border-bottom: 1px solid var(--fs-border);
    cursor: move;
    touch-action: none; /* 触摸拖动时不让浏览器把手势当成滚动 */
    user-select: none;
  }
  .title:focus-visible { outline: 2px solid var(--fs-accent); outline-offset: -2px; }
  .label { flex: 1; font-size: 12.5px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .close {
    flex: none;
    width: 22px; height: 20px; line-height: 1;
    background: transparent; color: var(--fs-fg-secondary);
    border: 1px solid transparent; border-radius: var(--fs-radius);
    cursor: pointer; font-size: 14px;
  }
  .close:hover { background: var(--fs-danger); color: #fff; }
  .body { flex: 1; min-height: 0; display: flex; flex-direction: column; overflow: hidden; }
  .grip {
    position: absolute; right: 0; bottom: 0; width: 16px; height: 16px;
    cursor: nwse-resize; touch-action: none;
    background: linear-gradient(135deg, transparent 50%, var(--fs-border-strong) 50%);
  }
  /* 高对比度：阴影被强制去掉，窗口与背后的内容会糊成一片，靠边框把边界画出来。 */
  @media (forced-colors: active) {
    .vwin { border: 1px solid CanvasText; }
    .title { border-bottom: 1px solid CanvasText; }
    .grip { background: none; border-right: 3px solid CanvasText; border-bottom: 3px solid CanvasText; }
  }
</style>
