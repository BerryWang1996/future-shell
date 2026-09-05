<script lang="ts">
  /**
   * TabBar.svelte — MDI 标签栏（Task 20 Step 4 ③，UI 规格 §2.2）
   *
   * 功能：
   * - 渲染活动会话标签队列（lib/tabs store），左对齐横向排列，活动标签高亮（顶部 accent 条）
   * - 状态灯四态（connecting/connected/disconnected/error，§2.2.1）：圆点颜色/动画
   * - 铃铛角标（bell: boolean）：输出提醒，点击清除（clearBell）
   * - 拖拽重排（HTML5 Drag & Drop API，reorderTab 原语）
   * - 中键关闭标签（auxclick button=1 → onClose prop 回传 App.svelte requestCloseTab）
   * - 右键配色方案菜单（临时覆盖 per-session，setTransientScheme）+ 关闭标签项（§2.2.3）
   * - 溢出时左右箭头滚动 + 下拉完整列表（overflow 响应式计算）
   *
   * Props:
   * - onClose: (tabId: string) => void — 标签关闭回调（App.svelte requestCloseTab 门控）
   *
   * 数据源：
   * - tabs, activeTabId, transientSchemes（lib/tabs store，Task 18 契约）
   * - SCHEMES（lib/term-schemes.ts 配色方案元数据）
   */

  import { clampToViewport } from "../lib/menu-pos"; // 右键菜单窗口边缘避让（评审 P2-8）
  import { tabs, activeTabId, activateTab, clearBell, reorderTab, setTransientScheme, transientSchemes } from "../lib/tabs";
  import { SCHEMES } from "../lib/term-schemes";
  import { canCloseOthers } from "../lib/tab-close";

  let {
    onClose = (_id: string) => {},
    onRecord,
    /**
     * 标签被拖出标签栏（M4b「标签拖出」）。App 据此开一个视图窗口。
     *
     * **拖出 ≠ 移走**：主窗口的标签留在原处。UI 规格 §66 的措辞是「平铺为视图克隆」——
     * 两个视图看的是同一份流，克隆而不是搬家。这也让误拖没有代价：多开的窗口关掉即可，
     * 而搬走的标签要用户自己找回来。
     */
    onDetach,
    /**
     * 「关闭其他标签」（M4b）。留下的是**右键点中的那个**，不是当前活动的那个：
     * 用户在一个非活动标签上右键选「关闭其他」，他要留的显然是手指底下这个。
     */
    onCloseOthers,
  }: {
    onClose?(id: string): void;
    onRecord?(id: string): void;
    onDetach?(id: string, title: string): void;
    onCloseOthers?(keepId: string): void;
  } = $props();

  /**
   * 拖出多远算「拖出去了」。
   *
   * 判据是**离开标签栏的距离**，与 Chrome / VS Code 同款：往下拽一段就分离。
   * 用 dragend 的 dropEffect 是否为 "none" 来判会更"精确"，但那只在拖到**窗口外**
   * 时成立——而人拖标签时大多只是往下拽到页面中间，那样就永远触发不了。
   *
   * 80px 约等于两个标签栏的高度：正常重排时指针在标签栏内上下抖动几像素，
   * 够不着这个距离。
   */
  const DETACH_THRESHOLD = 80;

  /** 拖拽结束时判断是不是拖出了标签栏，是则请求分离。 */
  function maybeDetach(e: DragEvent, id: string, title: string): void {
    if (!onDetach || !strip) return;
    const r = strip.getBoundingClientRect();
    // clientX/Y 在某些平台的 dragend 上是 0——那不是「拖到了左上角」，是「拿不到坐标」。
    // 拿不到就什么都不做：宁可少分离一次，也不要凭一个假坐标凭空开个窗口。
    if (e.clientX === 0 && e.clientY === 0) return;
    const out =
      e.clientY > r.bottom + DETACH_THRESHOLD ||
      e.clientY < r.top - DETACH_THRESHOLD ||
      e.clientX < r.left - DETACH_THRESHOLD ||
      e.clientX > r.right + DETACH_THRESHOLD;
    if (out) onDetach(id, title);
  }

  /**
   * 「关闭其他标签」。
   *
   * 条件在**这里**再判一次，而不是只靠按钮的 `disabled`。这不是保险起见：
   * `disabled` 拦的是鼠标点击，而这个 `<button role="menuitem">` 还可能被键盘
   * 或辅助技术以别的路径触发。而它做的事是**关掉一批会话**——破坏性操作的
   * 前置条件应当由做事的那一侧把关，而不是由长得像闸的那个属性。
   */
  function closeOthers(keepId: string): void {
    if (!canCloseOthers($tabs)) return;
    onCloseOthers?.(keepId);
    closeSchemeMenu();
  }

  let schemeMenuTabId = $state<string | null>(null);
  let schemeMenuPos = $state<{ x: number; y: number } | null>(null);
  let schemeMenu = $derived(schemeMenuTabId !== null && schemeMenuPos !== null ? { tabId: schemeMenuTabId, pos: schemeMenuPos } : null);
  function openSchemeMenu(tabId: string, x: number, y: number): void { schemeMenuTabId = tabId; schemeMenuPos = { x, y }; }
  function closeSchemeMenu(): void { schemeMenuTabId = null; schemeMenuPos = null; }

  let strip: HTMLDivElement;
  let overflow = $state(false);
  let dropdownOpen = $state(false);
  function measure() { overflow = strip ? strip.scrollWidth > strip.clientWidth : false; }
  function scrollBy(dx: number) { strip?.scrollBy({ left: dx, behavior: "smooth" }); measure(); }

  let dragId = $state<string | null>(null);

  /**
   * Enter/Space 键盘等价触发。标签内的铃铛/关闭是嵌在 `<button class="tab">` 里的 `<span>`
   * （HTML 不允许 button 嵌套 button，故只能用带 role/tabindex 的 span 承载），
   * 因此这里必须 stopPropagation，否则回车会连带触发外层标签的激活。
   */
  function onSpanKey(e: KeyboardEvent, run: () => void): void {
    if (e.key !== "Enter" && e.key !== " ") return;
    e.preventDefault();
    e.stopPropagation();
    run();
  }
</script>

<svelte:window onresize={() => measure()} onclick={() => { dropdownOpen = false; closeSchemeMenu(); }} />

<div class="tabbar" data-testid="tabbar">
  {#if overflow}<button class="nav" title="向左滚动" onclick={() => scrollBy(-240)}>‹</button>{/if}
  <div class="strip" role="tablist" aria-label="会话标签" bind:this={strip} onscroll={() => measure()}>
    {#each $tabs as t (t.id)}
      <button
        class="tab"
        class:active={$activeTabId === t.id}
        role="tab"
        aria-selected={$activeTabId === t.id}
        title={t.title}
        draggable="true"
        ondragstart={() => (dragId = t.id)}
        ondragend={(e) => { maybeDetach(e, t.id, t.title); dragId = null; }}
        ondragover={(e) => e.preventDefault()}
        ondrop={(e) => { e.preventDefault(); if (dragId && dragId !== t.id) reorderTab(dragId, t.id); dragId = null; }}
        onclick={() => activateTab(t.id)}
        onauxclick={(e) => { if (e.button === 1) { e.preventDefault(); e.stopPropagation(); onClose(t.id); } }}
        oncontextmenu={(e) => { e.preventDefault(); e.stopPropagation(); openSchemeMenu(t.id, e.clientX, e.clientY); }}
      >
        <i class="status {t.status}" aria-label={t.status}></i>
        {#if t.bell}
          <span class="bell" data-testid={`bell-${t.id}`} title="输出提醒，点击清除" aria-label="有新输出，点击清除"
                role="button" tabindex="0"
                onclick={(e) => { e.stopPropagation(); clearBell(t.id); }}
                onkeydown={(e) => onSpanKey(e, () => clearBell(t.id))}>🔔</span>
        {/if}
        <span class="name">{t.title}</span>
        <span
          class="close"
          role="button"
          tabindex="0"
          aria-label={`关闭 ${t.title}`}
          onclick={(e) => { e.stopPropagation(); onClose(t.id); }}
          onkeydown={(e) => onSpanKey(e, () => onClose(t.id))}
        >×</span>
      </button>
    {/each}
  </div>
  {#if overflow}
    <button class="nav" title="向右滚动" onclick={() => scrollBy(240)}>›</button>
    <div class="dropdown">
      <button class="nav" title="全部标签" onclick={(e) => { e.stopPropagation(); dropdownOpen = !dropdownOpen; }}>▾</button>
      {#if dropdownOpen}
        <ul class="list" role="menu">
          {#each $tabs as t (t.id)}
            <li role="menuitem" tabindex="0"
                onclick={() => { activateTab(t.id); dropdownOpen = false; }}
                onkeydown={(e) => onSpanKey(e, () => { activateTab(t.id); dropdownOpen = false; })}>{t.title}</li>
          {/each}
        </ul>
      {/if}
    </div>
  {/if}
</div>

{#if schemeMenu}
  <div class="scheme-menu" use:clampToViewport style="left: {schemeMenu.pos.x}px; top: {schemeMenu.pos.y}px" role="menu" aria-label="配色方案">
    <div class="group-label">配色方案 ▾</div>
    {#each SCHEMES as s (s.id)}
      <!-- 配色项是「单选式勾选」而非普通菜单项：menuitem 不支持 aria-pressed，须用 menuitemcheckbox + aria-checked -->
      <button
        role="menuitemcheckbox"
        aria-checked={$transientSchemes.get(schemeMenu.tabId) === s.id}
        onclick={() => { setTransientScheme(schemeMenu.tabId, s.id); closeSchemeMenu(); }}
      >{s.name}{#if $transientSchemes.get(schemeMenu.tabId) === s.id} ✓{/if}</button>
    {/each}
    <hr />
    <button role="menuitem" onclick={() => { setTransientScheme(schemeMenu.tabId, null); closeSchemeMenu(); }}>跟随默认（清除临时覆盖）</button>
    <hr />
    <button role="menuitem" data-testid="ctx-close-tab" onclick={() => { onClose(schemeMenu.tabId); closeSchemeMenu(); }}>关闭标签</button>
    <button role="menuitem" data-testid="ctx-record-toggle" onclick={() => { onRecord?.(schemeMenu.tabId); closeSchemeMenu(); }}>录制/停止录制</button>
    <!-- 「关闭其他」在只有一个标签时没有意义：点了什么也不会发生，
         而一个点了没反应的菜单项看起来像 bug。禁用并说明。 -->
    <button
      role="menuitem"
      data-testid="ctx-close-others"
      disabled={!canCloseOthers($tabs)}
      title={canCloseOthers($tabs) ? "" : "只有这一个标签"}
      onclick={() => { closeOthers(schemeMenu.tabId); }}
    >关闭其他标签</button>
  </div>
{/if}

<style>
  /* 标签条自身的外形（高度/底色/底边框）必须定义在**这里**。
     这些声明原先写在 App.svelte 的 `.tabbar` 规则里，但 App 渲染的是 `<TabBar />` 组件、
     没有同名宿主元素，而 Svelte 的样式作用域不跨组件边界——那条规则从组件化那天起就没生效过
     （svelte-check 报的 css_unused_selector）。同时本处原本写 `height: 100%`，父级 `.main`
     是 auto 高度的列向 flex，百分比高度无从解析，于是标签条实际是按内容撑高的：
     两边合起来的效果是「设计稿写了 34px，界面上从来不是 34px，且没有底边分隔线」。 */
  .tabbar { display: flex; align-items: stretch; flex: none; height: var(--fs-chrome-bar, 34px); background: var(--fs-bg-app); border-bottom: 1px solid var(--fs-border); }
  .strip { display: flex; align-items: stretch; flex: 1; min-width: 0; overflow-x: auto; }
  .nav { flex: none; border: 0; background: var(--fs-bg-app); color: var(--fs-fg-secondary); padding: 0 6px; cursor: pointer; }
  .nav:hover { color: var(--fs-accent); }
  .tab { display: inline-flex; align-items: center; gap: 6px; padding: 0 10px; border: 0; border-right: 1px solid var(--fs-border); background: var(--fs-bg-panel); color: var(--fs-fg-secondary); font-size: 12.5px; cursor: pointer; white-space: nowrap; }
  .tab.active { background: var(--fs-bg-app); color: var(--fs-fg-primary); box-shadow: inset 0 2px 0 var(--fs-accent); }
  .status { width: 8px; height: 8px; border-radius: 50%; display: inline-block; flex: none; }
  .status.connecting { border: 2px solid var(--fs-accent); animation: tab-blink 1s infinite; }
  .status.connected { background: var(--fs-ok); box-shadow: 0 0 5px var(--fs-ok); }
  .status.disconnected { border: 1px solid var(--fs-fg-disabled); }
  .status.error { background: var(--fs-danger); }
  /* 高对比度（房规见 styles.css）：选中标签的 inset 投影被抹掉、底色与未选中同为 Canvas，
     用户看不出自己在哪个会话；状态点里「已连接」与「出错」都是实心圆，也无从分辨。
     形状方案与侧栏主机灯一致（Sidebar.svelte），两处对同一组会话状态给同一套形状。 */
  @media (forced-colors: active) {
    .tab.active { outline: 2px solid Highlight; outline-offset: -2px; }
    .status.connected { background: CanvasText; border: 0; border-radius: 50%; }
    .status.connecting { background: Canvas; border: 2px solid CanvasText; border-radius: 50%; }
    .status.disconnected { background: Canvas; border: 1px solid GrayText; border-radius: 50%; }
    .status.error { background: CanvasText; border: 0; border-radius: 0; }
  }
  @keyframes tab-blink { 0%,100% { opacity: 1; } 50% { opacity: .3; } }
  .tab .name { max-width: 180px; overflow: hidden; text-overflow: ellipsis; }
  .tab .bell { color: #ff9500; font-size: 11px; cursor: pointer; }
  .tab .close { margin-left: 2px; color: var(--fs-fg-secondary); border-radius: 3px; padding: 0 3px; }
  .tab .close:hover { background: var(--fs-accent-dim); color: var(--fs-fg-primary); }
  .dropdown { position: relative; display: flex; }
  .list { position: absolute; right: 0; top: 100%; list-style: none; margin: 0; padding: 4px 0; min-width: 180px; max-height: 300px; overflow-y: auto; background: var(--fs-bg-elevated); border: 1px solid var(--fs-border); border-radius: 4px; box-shadow: 0 4px 12px rgba(0,0,0,.4); z-index: 40; }
  .list li { padding: 5px 12px; font-size: 12.5px; cursor: pointer; }
  .list li:hover { background: var(--fs-accent-dim); }
  .scheme-menu { position: fixed; z-index: 41; min-width: 180px; max-height: 320px; overflow-y: auto; padding: 4px; background: var(--fs-bg-elevated); border: 1px solid var(--fs-border-strong); border-radius: var(--fs-radius); box-shadow: var(--fs-shadow); display: flex; flex-direction: column; }
  .scheme-menu .group-label { padding: 4px 8px; font-size: 11px; color: var(--fs-fg-secondary); }
  .scheme-menu button { border: none; background: none; color: var(--fs-fg-primary); text-align: left; padding: 4px 8px; border-radius: 3px; cursor: pointer; font: inherit; }
  .scheme-menu button:hover:not(:disabled) { background: var(--fs-bg-panel); }
  .scheme-menu button:disabled { opacity: .5; cursor: default; color: var(--fs-fg-disabled); }
  .scheme-menu hr { border: none; border-top: 1px solid var(--fs-border); margin: 4px 0; }
</style>
