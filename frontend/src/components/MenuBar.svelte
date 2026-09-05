<script lang="ts">
  import { tick } from "svelte";
  import { menus, type MenuItem } from "../lib/menus";

  let { onAction }: { onAction(id: string): void } = $props();
  let openMenu = $state<string | null>(null);
  let bar = $state<HTMLDivElement | null>(null);

  function close(restore = false) {
    const trigger = bar?.querySelector<HTMLButtonElement>('.menu > button[aria-expanded="true"]');
    openMenu = null;
    if (restore) trigger?.focus();
  }

  async function navigate(e: KeyboardEvent) {
    const target = e.target as HTMLElement;
    if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); close(true); return; }
    if (e.key === "Tab") { close(); return; }
    if (!["ArrowDown", "ArrowUp", "ArrowLeft", "ArrowRight", "Home", "End"].includes(e.key)) return;
    const top = target.matches(".menu > button");
    const scope = target.closest<HTMLElement>('[role="menu"]');
    e.preventDefault();
    e.stopPropagation();
    if (top && ["ArrowDown", "ArrowUp"].includes(e.key)) {
      const index = Array.from(bar!.querySelectorAll('.menu > button')).indexOf(target);
      openMenu = $menus[index].id;
      await tick();
      const buttons = target.parentElement!.querySelectorAll<HTMLButtonElement>('.dropdown > .item > button:not(:disabled)');
      buttons[e.key === "ArrowUp" ? buttons.length - 1 : 0]?.focus();
      return;
    }
    if (e.key === "ArrowRight" && target.getAttribute("aria-haspopup") === "menu") {
      target.parentElement?.querySelector<HTMLButtonElement>('.submenu button:not(:disabled)')?.focus();
      return;
    }
    if (e.key === "ArrowLeft" && scope?.classList.contains("submenu")) {
      scope.parentElement?.querySelector<HTMLButtonElement>(":scope > button")?.focus();
      return;
    }
    if (top || ["ArrowLeft", "ArrowRight"].includes(e.key)) {
      const buttons = Array.from(bar!.querySelectorAll<HTMLButtonElement>('.menu > button'));
      const parent = target.closest('.menu')?.querySelector<HTMLButtonElement>(":scope > button");
      const index = buttons.indexOf(parent!);
      const next = e.key === "Home" ? 0 : e.key === "End" ? buttons.length - 1 : (index + (e.key === "ArrowLeft" ? -1 : 1) + buttons.length) % buttons.length;
      if (openMenu) openMenu = $menus[next].id;
      buttons[next].focus();
      return;
    }
    const buttons = Array.from(scope!.querySelectorAll<HTMLButtonElement>('button:not(:disabled)')).filter(b => b.closest('[role="menu"]') === scope);
    const index = buttons.indexOf(target as HTMLButtonElement);
    const next = e.key === "Home" ? 0 : e.key === "End" ? buttons.length - 1 : (index + (e.key === "ArrowUp" ? -1 : 1) + buttons.length) % buttons.length;
    buttons[next]?.focus();
  }

  function pick(item: MenuItem) {
    if (!item.enabled) return;
    close(true);
    onAction(item.id);
  }
</script>

<!-- S261：<nav> 隐含 navigation role，覆写为 menubar 属 ARIA 冲突（svelte-check a11y 告警），改用 div -->
<svelte:window onpointerdown={(e) => { if (openMenu && e.target instanceof Node && !bar?.contains(e.target)) close(); }} />
<div bind:this={bar} class="menubar" role="menubar" aria-label="主菜单" data-testid="menubar" tabindex="-1" onkeydown={navigate}>
  {#each $menus as menu (menu.id)}
    <div class="menu">
      <button
        role="menuitem"
        aria-haspopup="menu"
        aria-expanded={openMenu === menu.id}
        onmouseenter={() => { if (openMenu) openMenu = menu.id; }}
        onclick={() => (openMenu = openMenu === menu.id ? null : menu.id)}
      >{menu.label}</button>
      {#if openMenu === menu.id}
        <div class="dropdown" role="menu">
          {#each menu.items.filter(item => item.enabled || !item.note) as item (item.id)}
            <!--
              S260：子菜单原嵌在父 <button> 内部（button 套 button 属非法放置，svelte-check
              node_invalid_placement_ssr 告警），且展开只挂 .dropdown > button:hover ⇒ 纯键盘
              不可达，「应用主题」四皮肤在无鼠标时无从选取，违反 §7「全键盘可达」。
              改为同层兄弟节点 + :hover/:focus-within 双触发：Tab 走到父项即展开，可续 Tab 入子项。
            -->
            <div class="item">
              <button
                role="menuitem"
                class:disabled={!item.enabled}
                disabled={!item.enabled}
                aria-haspopup={item.children ? "menu" : undefined}
                title={item.note ?? ""}
                onclick={() => { if (!item.children) pick(item); }}
              >
                <span class="label">
                  {item.label}{#if !item.enabled && item.note}<i>（{item.note}）</i>{/if}
                </span>
                {#if item.shortcut}<span class="shortcut">{item.shortcut}</span>{/if}
                {#if item.children}<span class="shortcut" aria-hidden="true">▸</span>{/if}
              </button>
              {#if item.children}
                <div class="submenu" role="menu">
                  {#each item.children as child (child.id)}
                    <button role="menuitem" onclick={() => pick(child)}>{child.label}</button>
                  {/each}
                </div>
              {/if}
            </div>
          {/each}
        </div>
      {/if}
    </div>
  {/each}
</div>

<style>
  .menubar { display: flex; gap: 2px; padding: 2px 6px; background: var(--fs-bg-panel); border-bottom: 1px solid var(--fs-border); font-size: 12.5px; flex: none; }
  .menu { position: relative; }
  .menu > button { padding: 3px 9px; border-radius: 3px; color: var(--fs-fg-primary); background: none; border: none; cursor: pointer; }
  .menu > button:hover, .menu > button[aria-expanded="true"] { background: var(--fs-bg-hover); }
  .dropdown { position: absolute; top: 100%; left: 0; min-width: 250px; z-index: 30; padding: 4px; background: var(--fs-bg-elevated); border: 1px solid var(--fs-border-strong); border-radius: var(--fs-radius); box-shadow: var(--fs-shadow); display: flex; flex-direction: column; }
  .item { position: relative; display: flex; }
  .item > button { flex: 1; display: flex; justify-content: space-between; gap: 24px; padding: 4px 8px; border: none; background: none; border-radius: 3px; color: var(--fs-fg-primary); text-align: left; cursor: pointer; }
  .item > button:hover:not(.disabled) { background: var(--fs-bg-hover); }
  .item > button.disabled { color: var(--fs-fg-disabled); cursor: default; }
  .dropdown i { font-style: normal; margin-left: 6px; color: var(--fs-fg-disabled); font-size: 11px; }
  .shortcut { color: var(--fs-fg-secondary); font-size: 11px; }
  .submenu { display: none; position: absolute; left: 100%; top: 0; min-width: 160px; padding: 4px; background: var(--fs-bg-elevated); border: 1px solid var(--fs-border-strong); border-radius: var(--fs-radius); box-shadow: var(--fs-shadow); }
  /* S260：:focus-within 与 :hover 并列 —— 键盘 Tab 至父项即展开子菜单 */
  .item:hover > .submenu, .item:focus-within > .submenu { display: flex; flex-direction: column; z-index: 31; }
  .submenu button { padding: 4px 8px; border: none; background: none; border-radius: 3px; color: var(--fs-fg-primary); text-align: left; cursor: pointer; }
  .submenu button:hover { background: var(--fs-bg-hover); }
</style>
