<script lang="ts">
  import Icon from "./Icon.svelte"; // 16px 单色轮廓图标（评审 P2-5）
  import { clampToViewport } from "../lib/menu-pos"; // 右键菜单窗口边缘避让（评审 P2-8）
  import { invoke } from "../lib/ipc";
  import type { Profile } from "../lib/types";
  import { toast } from "../lib/toast";
  import { mergeFolderPaths } from "../lib/folders"; // M4a 手动文件夹（S326）
  import { activeTabId, tabs } from "../lib/tabs"; // 活动会话态（评审 P1-4）

  type Lamp = "connecting" | "connected" | "disconnected" | "error";

  let {
    profiles,
    badRows = [],
    sessionStates = new Map(),
    hostLights = new Map(),
    manualFolders = [],
    onDeleteFolder = (_path: string) => {},
    onRenameFolder = (_path: string) => {},
    onOpen = (_p: Profile) => {},
    onNew = () => {},
    onEdit = (_p: Profile) => {},
    onDelete = (_p: Profile) => {},
    onExportOne = (_p: Profile) => {},
    onImport = () => {},
    onExport = () => {},
    onSaved = (_p: Profile) => {},
  }: {
    profiles: Profile[];
    /** 后端读不出来的行（`profiles_list` 的 `badRows`）。必须呈现出来：
     *  一条配置因为 auth_blob 损坏/字段漂移而解析失败时，静默丢弃的表现是
     *  「我那条生产机配置凭空消失了」，用户既不知道发生过什么，也无从修复。 */
    badRows?: { id: string; error: string }[];
    sessionStates?: Map<string, { status: Lamp; sessionId: string }>;
    /** M4a 主机状态灯（host-lights store 的快照）：**未连接**档案的探测灯。
     *  已连接档案优先用 sessionStates（连接是最强的可达证明，探测灯让位）。 */
    hostLights?: Map<string, "green" | "red" | "gray">;
    /** M4a 手动新建的文件夹（settings 键 sidebar.folders）：与派生路径并集成树，
     *  故**空文件夹**也能存在——MVP 期分组纯派生，新建完立刻消失。 */
    manualFolders?: string[];
    /** M4a：删除手动文件夹（装配层落库）。只删目录、不删其中的连接。 */
    onDeleteFolder?(path: string): void;
    onRenameFolder?(path: string): void;
    onOpen?(p: Profile): void;
    onNew?(): void;
    onEdit?(p: Profile): void;
    onDelete?(p: Profile): void;
    onExportOne?(p: Profile): void;
    onImport?(): void;
    onExport?(): void;
    onSaved?(p: Profile): void;
  } = $props();

  interface Group { name: string; path: string; children: (Group | Profile)[] }
  interface Row { depth: number; kind: "group" | "profile"; group?: Group; profile?: Profile }

  let query = $state("");
  let menu = $state<{ x: number; y: number; profile: Profile } | null>(null);
  /** M4a：分组行右键菜单（重命名 + 删除；与会话菜单分开、不混判据——删会话与删目录后果差别巨大）。 */
  let groupMenu = $state<{ x: number; y: number; path: string } | null>(null);
  /** 右键菜单子菜单的默认展开方向；贴着视口右缘时由 menu-pos.ts 的 clampToViewport 改写成 "left"。
   *  用变量而不写字面量 "right"：Svelte 会按字面量做 CSS 剪枝，把 `[data-sub-side="left"]` 那条规则
   *  判成无用选择器直接剔掉——运行期才写上的属性值它看不见。 */
  const SUB_SIDE_DEFAULT: "right" | "left" = "right";
  let dragProfile = $state<Profile | null>(null);
  let collapsed = $state<Set<string>>(new Set());

  let tree = $derived(buildTree(filterProfiles(profiles, query)));
  let rows = $derived(flatten(tree, 0, collapsed));
  let groupPaths = $derived(collectGroupPaths(profiles));

  function filterProfiles(list: Profile[], q: string): Profile[] {
    const s = q.trim().toLowerCase();
    if (!s) return list;
    return list.filter((p) => [p.name, p.host, p.group_path ?? ""].some((v) => v.toLowerCase().includes(s)));
  }

  function buildTree(list: Profile[]): Group[] {
    const root: Group = { name: "", path: "", children: [] };
    // M4a：先按「手动文件夹 ∪ 派生路径」把目录骨架建全，再挂会话——只按会话建的话，
    // 空文件夹没有任何存身之处（用户建完立刻消失）。ensure 是幂等的，两条路径命中
    // 同一节点时复用同一个对象，不会出现重影。
    for (const path of mergeFolderPaths(manualFolders, list.map((p) => p.group_path ?? ""))) {
      let node = root;
      let acc = "";
      for (const seg of path.split("/")) {
        acc = acc ? `${acc}/${seg}` : seg;
        let next = node.children.find((c): c is Group => "children" in c && c.name === seg);
        if (!next) { next = { name: seg, path: acc, children: [] }; node.children.push(next); }
        node = next;
      }
    }
    for (const p of list) {
      let node = root;
      let acc = "";
      for (const seg of (p.group_path ?? "").split("/").filter(Boolean)) {
        acc = acc ? `${acc}/${seg}` : seg;
        let next = node.children.find((c): c is Group => "children" in c && c.name === seg);
        if (!next) { next = { name: seg, path: acc, children: [] }; node.children.push(next); }
        node = next;
      }
      node.children.push(p);
    }
    return root.children as Group[];
  }

  function flatten(nodes: (Group | Profile)[], depth: number, collapsed: Set<string>): Row[] {
    const out: Row[] = [];
    for (const c of nodes) {
      if ("children" in c) {
        out.push({ depth, kind: "group", group: c });
        if (!collapsed.has(c.path)) out.push(...flatten(c.children, depth + 1, collapsed));
      } else {
        out.push({ depth, kind: "profile", profile: c });
      }
    }
    return out;
  }

  function collectGroupPaths(list: Profile[]): string[] {
    const set = new Set<string>();
    for (const p of list) {
      const segs = (p.group_path ?? "").split("/").filter(Boolean);
      segs.forEach((_, i) => set.add(segs.slice(0, i + 1).join("/")));
    }
    return [...set];
  }

  function closeMenu() { menu = null; groupMenu = null; }

  function toggleGroup(path: string) {
    const next = new Set(collapsed);
    if (next.has(path)) { next.delete(path); } else { next.add(path); }
    collapsed = next;
  }

  async function duplicate(p: Profile) {
    closeMenu();
    const name = `${p.name} 副本`;
    try {
      const id = await invoke<string>("profile_save", { id: null, profile: { ...p, name } });
      onSaved({ ...p, id, name });
      toast.push("info", `已复制为「${name}」`);
    } catch (e) { toast.push("error", `复制失败：${e}`); }
  }

  async function moveToGroup(p: Profile, groupPath: string) {
    closeMenu();
    try {
      const moved = { ...p, group_path: groupPath || null };
      const id = await invoke<string>("profile_save", { id: p.id, profile: moved });
      onSaved({ ...moved, id });
    } catch (e) { toast.push("error", `移动失败：${e}`); }
  }

  // ── 右键菜单动作：抽成具名入口，使鼠标点击与键盘 Enter/Space 走同一条路径 ──
  // 之所以不在模板里各写一份内联箭头，是为了杜绝「点击改了、键盘忘改」的行为漂移；
  // 各动作的 closeMenu 时机保持原样（duplicate/moveToGroup 自身首行已 closeMenu）。
  const ctxActions = {
    open: () => { const p = menu?.profile; closeMenu(); if (p) onOpen(p); },
    edit: () => { const p = menu?.profile; closeMenu(); if (p) onEdit(p); },
    duplicate: () => { const p = menu?.profile; if (p) void duplicate(p); },
    exportOne: () => { const p = menu?.profile; closeMenu(); if (p) onExportOne(p); },
    remove: () => { const p = menu?.profile; closeMenu(); if (p) onDelete(p); },
  };
  /** 「移动到…」子菜单项：按目标分组路径柯里化出动作（根目录传空串）。 */
  function ctxMoveTo(groupPath: string) {
    return () => { const p = menu?.profile; if (p) void moveToGroup(p, groupPath); };
  }
  /** 菜单项键盘等价触发：Enter/Space 与 click 同义（a11y_click_events_have_key_events）。 */
  function onMenuKey(e: KeyboardEvent, run: () => void) {
    if (e.key === "Enter" || e.key === " ") { e.preventDefault(); run(); }
  }
</script>

<svelte:window onclick={closeMenu} />

<nav class="sidebar"
     ondragover={(e) => e.preventDefault()}
     ondrop={(e) => { e.preventDefault(); if (dragProfile) void moveToGroup(dragProfile, ""); }}>
  <input class="search" bind:value={query} placeholder="搜索（名称/主机/分组）" data-testid="sidebar-search" />
  <div class="tree" data-testid="sidebar-tree" role="tree" aria-label="会话管理器">
    {#each rows as r (r.kind === "group" ? `g:${r.group?.path}` : `p:${r.profile?.id}`)}
      {#if r.kind === "group"}
        <!-- treeitem 必带 aria-selected（ARIA 规范硬要求）：分组行只做展开/折叠、永不作为选中项，故恒 false -->
        <div class="row group" style:padding-left="{8 + r.depth * 12}px" title={r.group?.path}
             role="treeitem" aria-expanded={r.group ? !collapsed.has(r.group.path) : true} aria-level={r.depth + 1} tabindex="0"
             aria-selected={false}
             onclick={() => { if (r.group) toggleGroup(r.group.path); }}
             onkeydown={(e) => { if ((e.key === "Enter" || e.key === " ") && r.group) { e.preventDefault(); toggleGroup(r.group.path); } }}
             ondragover={(e) => e.preventDefault()}
             ondrop={(e) => {
               e.preventDefault(); e.stopPropagation();
               if (dragProfile && r.group) void moveToGroup(dragProfile, r.group.path);
             }}
             oncontextmenu={(e) => {
               e.preventDefault(); e.stopPropagation();
               if (r.group) groupMenu = { x: e.clientX, y: e.clientY, path: r.group.path };
             }}>
          <Icon name={r.group && collapsed.has(r.group.path) ? "chevron-right" : "chevron-down"} size={14} />
          <Icon name="folder" />
          {r.group?.name}
        </div>
      {:else if r.profile}
        {@const p = r.profile}
        {@const lamp = sessionStates.get(p.id)?.status}
        {@const probeLight = lamp ? null : hostLights.get(p.id) ?? null}
        <!-- 三态拆分（2026-08-31 评审 P1-4）：状态灯答「连没连」，行高亮答
             「我现在在哪个会话」，弱标记答「哪些已开着但不是当前」。此前
             aria-selected 挂在「已打开」上——多标签全亮，既违反单选树的
             ARIA 语义，也回答不了「我在哪」。 -->
        {@const activeProfileId = $tabs.find((t) => t.id === $activeTabId)?.profileId ?? null}
        {@const isActive = p.id === activeProfileId}
        {@const isOpen = sessionStates.has(p.id)}
        <div class="row leaf" class:active={isActive} class:open-other={isOpen && !isActive}
             style:padding-left="{8 + r.depth * 12}px" title={p.host}
             role="treeitem" aria-level={r.depth + 1} tabindex="0"
             aria-selected={isActive}
             draggable="true"
             ondragstart={(e) => { dragProfile = p; e.dataTransfer?.setData("text/plain", p.id); }}
             ondragend={() => (dragProfile = null)}
             ondblclick={() => onOpen(p)}
             onkeydown={(e) => { if (e.key === "Enter") onOpen(p); }}
             oncontextmenu={(e) => { e.preventDefault(); e.stopPropagation(); menu = { x: e.clientX, y: e.clientY, profile: p }; }}>
          {#if lamp}
            <i class="lamp {lamp}" aria-label={lamp}></i>
          {:else if probeLight}
            <i class="lamp probe-{probeLight}" aria-label="主机{probeLight === "green" ? "可达" : probeLight === "red" ? "不可达" : "未知"}"></i>
          {/if}
          <span class="pname">{p.name}</span>
        </div>
      {/if}
    {:else}
      <p class="empty">{query ? "无匹配会话" : "暂无会话（底部 ＋ 新建）"}</p>
    {/each}
  </div>
  {#if badRows.length}
    <!-- 坏行诊断固定挂在列表下方，不参与搜索过滤：这些行连 id 之外的字段都没解析出来，
         按名称/主机筛选无从谈起，而恰恰是它们最需要被用户看到。 -->
    <details class="bad-rows" data-testid="sidebar-bad-rows">
      <!-- 摘要行常驻可见、明细默认折叠：坏行数量必须第一眼看到（这条审计要修的就是「静默消失」），
           而每行的 id 与解析错误是排障细节，展开再看即可，不该长期挤占本就窄的侧栏。
           用原生 <details>/<summary>：点击与键盘展开、屏幕阅读器语义全部现成。 -->
      <summary class="bad-head"><Icon name="warning" title="警告" /> {badRows.length} 条连接无法读取</summary>
      <ul>
        {#each badRows as b (b.id)}
          <li title={b.error}><code>{b.id}</code> — {b.error}</li>
        {/each}
      </ul>
    </details>
  {/if}
  {#if menu}
    <div class="menu-overlay" role="presentation" onclick={closeMenu}
         oncontextmenu={(e) => { e.preventDefault(); closeMenu(); }}>
      <ul class="ctxmenu" data-sub-side={SUB_SIDE_DEFAULT} use:clampToViewport style:left="{menu.x}px" style:top="{menu.y}px" role="menu" data-testid="sidebar-ctxmenu">
        <li role="menuitem" tabindex="0" onclick={ctxActions.open}
            onkeydown={(e) => onMenuKey(e, ctxActions.open)}>连接</li>
        <li role="menuitem" tabindex="0" onclick={ctxActions.edit}
            onkeydown={(e) => onMenuKey(e, ctxActions.edit)}>属性</li>
        <li role="menuitem" tabindex="0" onclick={ctxActions.duplicate}
            onkeydown={(e) => onMenuKey(e, ctxActions.duplicate)}>复制</li>
        <li role="menuitem" tabindex="0" class="has-sub">移动到…
          <ul class="submenu" role="menu">
            <li role="menuitem" tabindex="0" onclick={ctxMoveTo("")}
                onkeydown={(e) => onMenuKey(e, ctxMoveTo(""))}>根目录</li>
            {#each groupPaths as g}
              <li role="menuitem" tabindex="0" onclick={ctxMoveTo(g)}
                  onkeydown={(e) => onMenuKey(e, ctxMoveTo(g))}>{g}</li>
            {/each}
          </ul>
        </li>
        <li role="menuitem" tabindex="0" onclick={ctxActions.exportOne}
            onkeydown={(e) => onMenuKey(e, ctxActions.exportOne)}>导出</li>
        <li role="menuitem" tabindex="0" class="danger" onclick={ctxActions.remove}
            onkeydown={(e) => onMenuKey(e, ctxActions.remove)}>删除…</li>
      </ul>
    </div>
  {/if}
  {#if groupMenu}
    <!-- M4a 分组行右键菜单：重命名 + 删除。与会话菜单分开渲染，
         免得两套判据混在一个 menu 状态里（删会话与删目录后果差别巨大）。 -->
    <div class="menu-overlay" role="presentation" onclick={closeMenu}
         oncontextmenu={(e) => { e.preventDefault(); closeMenu(); }}>
      <ul class="ctxmenu" data-sub-side={SUB_SIDE_DEFAULT} use:clampToViewport style:left="{groupMenu.x}px" style:top="{groupMenu.y}px" role="menu" data-testid="sidebar-groupmenu">
        <li role="menuitem" tabindex="0"
            onclick={() => { const p = groupMenu?.path; closeMenu(); if (p) onRenameFolder(p); }}
            onkeydown={(e) => onMenuKey(e, () => { const p = groupMenu?.path; closeMenu(); if (p) onRenameFolder(p); })}
            data-testid="groupmenu-rename">重命名文件夹…</li>
        <li role="menuitem" tabindex="0" class="danger"
            onclick={() => { const p = groupMenu?.path; closeMenu(); if (p) onDeleteFolder(p); }}
            onkeydown={(e) => onMenuKey(e, () => { const p = groupMenu?.path; closeMenu(); if (p) onDeleteFolder(p); })}
            data-testid="groupmenu-del">删除文件夹（不删其中的连接）</li>
      </ul>
    </div>
  {/if}
  <footer class="actions">
    <button title="新建会话" onclick={onNew} data-testid="sidebar-new"><Icon name="plus" title="新建会话" /></button>
    <button title="导入（JSON）" onclick={onImport} data-testid="sidebar-import"><Icon name="arrow-down-tray" title="导入" /></button>
    <button title="导出（JSON）" onclick={onExport} data-testid="sidebar-export"><Icon name="arrow-up-tray" title="导出" /></button>
  </footer>
</nav>

<style>
  .sidebar { width: 100%; height: 100%; display: flex; flex-direction: column; overflow: hidden; }
  .search { margin: 8px; padding: 5px 8px; background: var(--fs-bg-input); border: 1px solid var(--fs-border); border-radius: 4px; color: var(--fs-fg-primary); }
  .tree { flex: 1; overflow-y: auto; }
  .row { display: flex; align-items: center; gap: var(--fs-chrome-gap, 6px); padding: var(--fs-chrome-pad-y, 3px) 8px; font-size: 12.5px; color: var(--fs-fg-primary); user-select: none; }
  /* 活动会话行：左侧 2px 强调条 + 悬停级底色（评审 P1-4 的「行高亮」）。
   * 用 box-shadow 而不是 border——border 会把行内容顶移 2px，切换标签时跳。 */
  .leaf.active { background: var(--fs-bg-hover); box-shadow: inset 2px 0 0 var(--fs-accent); }
  /* 已开但非当前：弱标记——文字保持主色，只在名字前加一枚次色小点。
   * 不用降低文字对比度做弱化（会伤可读性），也不用整行变灰（那和「断线」撞语义）。 */
  .leaf.open-other .pname::before { content: "•"; color: var(--fs-fg-secondary); margin-right: 4px; font-size: 10px; }
  .row.group { color: var(--fs-fg-secondary); cursor: pointer; }
  .chevron { width: 10px; flex: none; font-size: 10px; color: var(--fs-fg-disabled); }
  .row.leaf { cursor: default; }
  .row.leaf:hover { background: var(--fs-accent-dim); }
  .pname { flex: 1; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .empty { padding: 10px; font-size: 12px; color: var(--fs-fg-secondary); }
  .bad-rows { flex: none; max-height: 30%; overflow-y: auto; padding: 8px 10px; border-top: 1px solid var(--fs-border); background: var(--fs-bg-app); }
  .bad-head { margin: 0 0 4px; font-size: 12px; font-weight: 600; color: var(--fs-danger); cursor: pointer; }
  .bad-rows ul { list-style: none; margin: 0; padding: 0; }
  .bad-rows li { font-size: 11.5px; color: var(--fs-fg-secondary); padding: 2px 0; overflow-wrap: anywhere; }
  .bad-rows code { color: var(--fs-fg-primary); }
  .lamp { width: 8px; height: 8px; border-radius: 50%; display: inline-block; flex: none; }
  .lamp.connecting { background: var(--fs-accent); animation: lamp-blink 1s infinite; }
  .lamp.connected { background: var(--fs-ok); box-shadow: 0 0 5px var(--fs-ok); }
  .lamp.disconnected { background: var(--fs-fg-disabled); }
  .lamp.error { background: var(--fs-danger); }
  /* M4a 主机状态灯（探测三态）：形状改为空心以区分「会话灯」（实心）——
   *  一眼分清「连着」与「只是可达」。 */
  .lamp.probe-green { background: none; border: 1.5px solid var(--fs-ok); }
  .lamp.probe-red { background: none; border: 1.5px solid var(--fs-danger); }
  .lamp.probe-gray { background: none; border: 1.5px solid var(--fs-fg-disabled); }
  /* 高对比度（房规见 styles.css）：七盏灯此前只靠底色/边框色区分，强制颜色下塌成两组——
     实心的四盏（连上/连接中/已断开/出错）彼此一样，空心的三盏（可达/不可达/未知）也彼此一样。
     填充写 CanvasText 而不是 currentColor：后者会被引擎强制成 Canvas，点就画不出来了（实测）。
     改成形状矩阵，二十一对两两可辨；`disconnected` / `probe-gray` 取 GrayText 是语义对位
     （系统调色板里它就是「次要/不可用」那一档）。 */
  @media (forced-colors: active) {
    .lamp.connected { background: CanvasText; border: 0; border-radius: 50%; }
    .lamp.connecting { background: Canvas; border: 2px solid CanvasText; border-radius: 50%; }
    .lamp.disconnected { background: GrayText; border: 0; border-radius: 50%; }
    .lamp.error { background: CanvasText; border: 0; border-radius: 0; }
    .lamp.probe-green { background: Canvas; border: 1.5px solid CanvasText; border-radius: 50%; }
    .lamp.probe-red { background: Canvas; border: 1.5px solid CanvasText; border-radius: 0; }
    .lamp.probe-gray { background: Canvas; border: 1.5px solid GrayText; border-radius: 50%; }
    /* 活动行：inset 投影被抹成 none、底色与普通行同为 Canvas → 完全看不出选中。 */
    .leaf.active { outline: 2px solid Highlight; outline-offset: -2px; }
  }
  @keyframes lamp-blink { 0%,100% { opacity: 1; } 50% { opacity: .3; } }
  .actions { display: flex; border-top: 1px solid var(--fs-border); flex: none; }
  .actions button { flex: 1; padding: 6px 0; border: 0; background: transparent; color: var(--fs-fg-secondary); cursor: pointer; }
  .actions button:hover { background: var(--fs-accent-dim); color: var(--fs-accent); }
  .menu-overlay { position: fixed; inset: 0; z-index: 70; }
  .ctxmenu { position: fixed; list-style: none; margin: 0; padding: 4px 0; min-width: 150px; background: var(--fs-bg-elevated); border: 1px solid var(--fs-border); border-radius: 4px; box-shadow: 0 4px 12px rgba(0,0,0,.4); z-index: 71; }
  .ctxmenu li { padding: 5px 12px; font-size: 12.5px; color: var(--fs-fg-primary); cursor: pointer; position: relative; }
  .ctxmenu li:hover { background: var(--fs-accent-dim); }
  .ctxmenu li.danger { color: var(--fs-danger); }
  .ctxmenu li.has-sub:hover .submenu { display: block; }
  .submenu { display: none; position: absolute; left: 100%; top: -5px; list-style: none; margin: 0; padding: 4px 0; min-width: 140px; max-height: 220px; overflow-y: auto; background: var(--fs-bg-elevated); border: 1px solid var(--fs-border); border-radius: 4px; box-shadow: 0 4px 12px rgba(0,0,0,.4); }
  /* 菜单贴着视口右缘时子菜单翻到左侧（menu-pos.ts 的 clampToViewport 按平移后右缘写 data-sub-side；
     模板上的 data-sub-side="right" 是默认值，也是让 Svelte 看见这个属性选择器有宿主、不把规则当无用剔掉）。
     不翻的话，右停靠 / 窄窗口下「移动到…」的分组列表整个出界，点不到（2026-09-02 响应式核查）。 */
  .ctxmenu[data-sub-side="left"] .submenu { left: auto; right: 100%; }
</style>
