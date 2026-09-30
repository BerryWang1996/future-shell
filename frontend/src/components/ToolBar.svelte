<script lang="ts">
  import Icon from "./Icon.svelte"; // 16px 单色轮廓图标（评审 P2-5：emoji 三平台不可控）
  import { clampToViewport } from "../lib/menu-pos"; // 右键菜单窗口边缘避让（评审 P2-8）
  import { onMount } from "svelte";
  import { BUILTIN_THEMES } from "../lib/theme/themes";
  import { setTheme, themeId } from "../lib/theme/store";
  import { settingGet, settingSet } from "../lib/ipc";
  import {
    EMPTY_LAYOUT,
    TOOLBAR_LAYOUT_KEY,
    applyToolbarLayout,
    needsSeparator,
    parseToolbarLayout,
    reorderToolbar,
    toggleToolbarItem,
    type ToolbarItem,
    type ToolbarLayout,
  } from "../lib/toolbar-layout";

  let {
    onAction,
    /** 开关类按钮的按压态（M4a 广播）：id → 是否处于开启。缺省无按压态。 */
    pressed = () => false,
    /** 侧栏此刻是 overlay 形态（窄屏 / 用户选「浮动」）：显示「叫回侧栏」按钮（路线图 4c）。 */
    overlaySidebar = false,
    /** overlay 浮层此刻是否展开：那颗按钮的按压态。 */
    overlayOpen = false,
  }: {
    onAction(id: string): void;
    pressed?: (id: string) => boolean;
    overlaySidebar?: boolean;
    overlayOpen?: boolean;
  } = $props();
  let current = $derived($themeId);

  /**
   * 全部按钮，**扁平**一张表。
   *
   * 从「二维 GROUPS」改成「扁平 + group 序号」是自定义排序的前提：排序会打散分组，
   * 而二维结构表达不了「第 3 组的按钮现在排在第 1 组两个按钮中间」。
   * 分隔符改由 `needsSeparator` 按相邻两项的 group 是否不同来画——
   * 那样它标记的仍然是真实的边界，而不是一个固定间隔。
   */
  const ITEMS: ToolbarItem[] = [
    { id: "session.new", icon: "plus", tip: "新建会话 (Ctrl+N)", enabled: true, group: 0, primary: true },
    // session.openDir（打开目录）：MVP 隐藏、不入本表（UI 规格 §2.2 权威「MVP 隐藏」）；
    // M4a 启用时在此位补回。与菜单 session.openDir 同一 reveal-item-in-dir 入口。
    { id: "edit.copy", icon: "copy", tip: "复制 (Ctrl+Shift+C)", enabled: true, group: 1 },
    { id: "edit.paste", icon: "paste", tip: "粘贴 (Ctrl+Shift+V)", enabled: true, group: 1 },
    { id: "edit.find", icon: "search", tip: "查找 (Ctrl+F)", enabled: true, group: 2 },
    // M4b 截图已实装（lib/screenshot.ts）。这个按钮此前一直是禁用态、tip 写着
    // 「即将推出 Phase 4b」——功能做完了却没人回来解禁它，是很典型的一处遗漏。
    // 走剪贴板那一路：工具栏是一键操作，存盘那一路在编辑菜单里。
    { id: "edit.screenshotCopy", icon: "camera", tip: "截图到剪贴板", enabled: true, group: 2 },
    { id: "tools.quickCommands", icon: "bolt", tip: "快速命令集（按钮条与片段库管理）", enabled: true, group: 3 },
    { id: "tools.broadcast", icon: "megaphone", tip: "实时键入广播：本终端的键入同步到组合栏选定的目标（全部/分组/手选）", enabled: true, group: 3 },
    { id: "tools.settings", icon: "gear", tip: "设置 (Ctrl+,)", enabled: true, group: 4 },
    { id: "tools.ai", icon: "sparkles", tip: "AI 助手 (Ctrl+Shift+A)", enabled: true, group: 4 },
  ];

  /* ── 自定义布局（M4b 第 17 项）────────────────────────────────────────── */

  let layout = $state<ToolbarLayout>(EMPTY_LAYOUT);
  let menuPos = $state<{ x: number; y: number } | null>(null);
  let dragId = $state<string | null>(null);

  const shown = $derived(applyToolbarLayout(ITEMS, layout));

  onMount(async () => {
    layout = parseToolbarLayout(await settingGet<unknown>(TOOLBAR_LAYOUT_KEY, null));
  });

  /**
   * 落库。
   *
   * `next === layout`（同一引用）表示这次操作没改变任何东西——`reorderToolbar` 在
   * 「拖到自己身上」「目标不在序列里」时就是这样返回的。那时**不写库**：
   * 一次没有内容的写入除了多一次 IPC，还会让「有没有改动」这件事在日志里说不清。
   *
   * 失败不回滚界面——用户刚拖完的顺序在眼前，回滚会像是操作被吞了。
   */
  async function persist(next: ToolbarLayout): Promise<void> {
    if (next === layout) return;
    layout = next;
    try {
      if (await settingSet(TOOLBAR_LAYOUT_KEY, next) === false) return;
    } catch (e) {
      console.warn("工具栏布局保存失败（本次会话仍生效）", e);
    }
  }
</script>

<!-- 右键工具栏任意处 → 勾选要显示哪些项（出口原文：「右键菜单勾选隐藏某项后该项即时消失」）。
     挂在容器上而不是逐个按钮：用户想显示一个已经隐藏的项时，那个按钮不在页面上，
     没法右键它——菜单必须能从别处打开。 -->
<div
  class="toolbar"
  role="toolbar"
  aria-label="工具栏"
  data-testid="toolbar"
  tabindex="-1"
  oncontextmenu={(e) => { e.preventDefault(); menuPos = { x: e.clientX, y: e.clientY }; }}
>
  {#if overlaySidebar}
    <!-- overlay 形态下侧栏藏在屏外，这颗按钮是把它叫回来的唯一鼠标入口（路线图 4c）。
         刻意不进 ITEMS：ITEMS 里的项可被右键隐藏、可被拖排序，而「叫回侧栏」的入口一旦被藏掉，
         用户就只剩 Ctrl+Shift+S 一条暗路。停靠形态下不显示——那时侧栏在版面里，视图菜单已够用。 -->
    <button
      class="tbtn"
      class:pressed={overlayOpen}
      aria-pressed={overlayOpen}
      title="显示/隐藏会话管理器 (Ctrl+Shift+S)"
      data-testid="tbtn-view.sidebar"
      onclick={() => onAction("view.sidebar")}
    ><Icon name="panel-left" /></button>
    <span class="tsep" aria-hidden="true"></span>
  {/if}
  {#each shown as btn, i (btn.id)}
    {#if needsSeparator(shown[i - 1], btn)}<span class="tsep" aria-hidden="true"></span>{/if}
    <button
      class="tbtn"
      class:primary={btn.primary}
      class:pressed={pressed(btn.id)}
      class:dragging={dragId === btn.id}
      aria-pressed={pressed(btn.id) || undefined}
      disabled={!btn.enabled}
      title={btn.tip}
      data-testid={`tbtn-${btn.id}`}
      draggable="true"
      ondragstart={() => (dragId = btn.id)}
      ondragover={(e) => e.preventDefault()}
      ondrop={(e) => {
        e.preventDefault();
        if (dragId) void persist(reorderToolbar(layout, shown.map((x) => x.id), dragId, btn.id));
        dragId = null;
      }}
      ondragend={() => (dragId = null)}
      onclick={() => onAction(btn.id)}
    ><Icon name={btn.icon} /></button>
  {/each}
  <span class="tsep" aria-hidden="true"></span>
  <label class="skinbox" title="应用主题，切换后立即生效">
    <Icon name="palette" />
    <select
      aria-label="应用主题"
      value={current}
      onchange={(e) => void setTheme(e.currentTarget.value)}
    >
      <option value="auto">跟随系统</option>
      {#each BUILTIN_THEMES as t (t.id)}
        <option value={t.id}>{t.name}</option>
      {/each}
    </select>
  </label>
</div>

{#if menuPos}
  <!-- svelte-ignore a11y_click_events_have_key_events -->
  <div class="tb-scrim" role="presentation" onclick={() => (menuPos = null)}></div>
  <div class="tb-menu" use:clampToViewport style="left: {menuPos.x}px; top: {menuPos.y}px" role="menu"
       aria-label="工具栏显示项" data-testid="toolbar-menu">
    <div class="tb-menu-label">显示这些按钮</div>
    {#each ITEMS as it (it.id)}
      <!-- 列的是全部项而不是当前可见项：隐藏了的那些要能勾回来，
           而它们此刻不在工具栏上。 -->
      <button
        role="menuitemcheckbox"
        aria-checked={!layout.hidden.includes(it.id)}
        data-testid={`tb-toggle-${it.id}`}
        onclick={() => void persist(toggleToolbarItem(layout, it.id))}
      >{layout.hidden.includes(it.id) ? "☐" : "☑"} {it.tip.split("（")[0].split(" (")[0]}</button>
    {/each}
    <hr />
    <button role="menuitem" data-testid="tb-reset"
            onclick={() => { void persist(EMPTY_LAYOUT); menuPos = null; }}>恢复默认</button>
  </div>
{/if}

<style>
  .toolbar { display: flex; align-items: center; gap: 2px; padding: 4px 8px; background: var(--fs-bg-panel); border-bottom: 1px solid var(--fs-border); flex: none; }
  .tbtn { width: calc(28px * var(--fs-density, 1.2)); height: var(--fs-chrome-h, 26px); display: grid; place-items: center; border: none; background: none; border-radius: var(--fs-radius); color: var(--fs-fg-secondary); cursor: pointer; font-size: 14px; }
  .tbtn:hover:not(:disabled) { background: var(--fs-bg-hover); color: var(--fs-fg-primary); }
  .tbtn:disabled { color: var(--fs-fg-disabled); cursor: default; }
  .tbtn.primary { background: var(--fs-accent); color: var(--fs-accent-fg); }
  .tbtn.primary:hover { background: var(--fs-accent-hover); }
  /* M4a 广播按压态：开启中的开关要有「此刻正在同步」的持续视觉——不然一次误触
   * 开了广播，用户毫无察觉地把命令敲进了所有机组。用 accent 底色与 primary 同源。 */
  .tbtn.pressed { background: var(--fs-accent); color: var(--fs-accent-fg); }
  .tbtn.pressed:hover { background: var(--fs-accent-hover); }
  /* 高对比度（房规见 styles.css）：主按钮与按压态的强调全靠 accent 底色，强制颜色下底色一律
     变成 Canvas，与普通按钮无从分辨。**按压态尤其要紧**——它表示广播此刻正把你的键入同步到
     所有机组，看不出来就等于那颗开关消失了。 */
  @media (forced-colors: active) {
    .tbtn.primary, .tbtn.pressed { outline: 2px solid Highlight; outline-offset: -2px; }
  }
  .tsep { width: 1px; height: 20px; background: var(--fs-border); margin: 0 6px; flex: none; }
  .skinbox { display: flex; align-items: center; gap: 6px; padding: 0 8px; height: var(--fs-chrome-h, 26px); border-radius: var(--fs-radius); color: var(--fs-fg-secondary); }
  .skinbox select { background: var(--fs-bg-input); color: var(--fs-fg-primary); border: 1px solid var(--fs-border); border-radius: 3px; padding: 1px 20px 1px 4px; font: inherit; } /* 右内距给箭头让位 */
  /* ── 自定义（M4b 第 17 项）──────────────────────────────────────────── */
  /* 拖动中的按钮压暗：不给反馈的话，拖拽在一个只有图标的横条上完全看不出发生了什么。 */
  .tbtn.dragging { opacity: 0.4; }
  /* 遮罩接住「点别处关菜单」。透明——把界面压暗会让人以为程序被禁用了。 */
  .tb-scrim { position: fixed; inset: 0; z-index: 49; }
  .tb-menu {
    position: fixed;
    z-index: 50;
    min-width: 200px;
    padding: 4px;
    background: var(--fs-bg-elevated);
    border: 1px solid var(--fs-border);
    border-radius: var(--fs-radius);
    box-shadow: var(--fs-shadow, 0 2px 12px rgba(0, 0, 0, 0.35));
    display: flex;
    flex-direction: column;
  }
  .tb-menu-label { padding: 4px 8px; font-size: 11px; color: var(--fs-fg-secondary); }
  .tb-menu button {
    text-align: left;
    padding: 4px 8px;
    border: 0;
    background: none;
    color: var(--fs-fg-primary);
    font-size: 12px;
    cursor: pointer;
    border-radius: var(--fs-radius);
  }
  .tb-menu button:hover { background: var(--fs-bg-hover); }
  .tb-menu hr { border: 0; border-top: 1px solid var(--fs-border); margin: 4px 0; }
</style>
