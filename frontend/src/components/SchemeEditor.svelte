<script lang="ts">
  import { useFocusTrap } from "../lib/focusTrap";
  import type { StoredCustomScheme } from "../lib/term-schemes";

  let {
    open = false,
    /** 编辑既有配色时传进来；新建时传 null。 */
    initial = null,
    /** 已有的名字，用来挡重名（编辑时排除自己）。 */
    takenNames = [],
    onSave = () => {},
    onCancel = () => {},
  }: {
    open?: boolean;
    initial?: StoredCustomScheme | null;
    takenNames?: string[];
    onSave?(s: StoredCustomScheme): void;
    onCancel?(): void;
  } = $props();

  let dialogEl = $state<HTMLDivElement | undefined>();
  let nameInput = $state<HTMLInputElement | undefined>();

  $effect(() => {
    if (!open || !dialogEl || !nameInput) return;
    return useFocusTrap(dialogEl, { initial: nameInput });
  });

  /** ANSI 16 色的显示名。顺序就是 xterm 的色号顺序，不能改。 */
  const ANSI_NAMES = [
    "黑", "红", "绿", "黄", "蓝", "洋红", "青", "白",
    "亮黑", "亮红", "亮绿", "亮黄", "亮蓝", "亮洋红", "亮青", "亮白",
  ];

  /** 新建时的起点：一套中性的深色，16 色取 xterm 的经典值。 */
  const BLANK: StoredCustomScheme = {
    name: "",
    foreground: "#cccccc",
    background: "#1e1e1e",
    ansi: [
      "#000000", "#cd3131", "#0dbc79", "#e5e510",
      "#2472c8", "#bc3fbc", "#11a8cd", "#e5e5e5",
      "#666666", "#f14c4c", "#23d18b", "#f5f543",
      "#3b8eea", "#d670d6", "#29b8db", "#e5e5e5",
    ],
    cursor: "#cccccc",
  };

  let name = $state("");
  let foreground = $state(BLANK.foreground);
  let background = $state(BLANK.background);
  let cursor = $state(BLANK.cursor ?? BLANK.foreground);
  let ansi = $state<string[]>([...BLANK.ansi]);

  /**
   * 每次打开都从 `initial` 重置。
   *
   * 不重置的话，「编辑 A → 取消 → 新建」会带着 A 的颜色开始——用户以为自己在造一套新的，
   * 存下来却是 A 的副本。这与 ForeignImportDialog 的 reset 是同一类问题。
   */
  $effect(() => {
    if (!open) return;
    const src = initial ?? BLANK;
    name = src.name;
    foreground = src.foreground;
    background = src.background;
    cursor = src.cursor ?? src.foreground;
    ansi = [...src.ansi];
  });

  /**
   * 名字校验。
   *
   * 重名不给存，而不是存下去再由后端跳过——后端那条「同名跳过」是给**导入**用的
   * （一次几十套，逐个问不现实）。在编辑器里，用户明确地在造这一套，
   * 静默跳过等于他按了保存却什么也没发生。
   */
  const trimmed = $derived(name.trim());
  const nameError = $derived.by(() => {
    if (trimmed === "") return "名字不能为空";
    if (trimmed.length > 64) return "名字不能超过 64 个字符";
    if (takenNames.some((n) => n !== initial?.name && n === trimmed)) return "已经有同名的配色了";
    return "";
  });

  function save(): void {
    if (nameError) return;
    onSave({
      name: trimmed,
      // 颜色一律小写：存储层只有一种表示形式（后端校验也只认小写）。
      // `<input type="color">` 在各浏览器上返回的大小写并不统一。
      foreground: foreground.toLowerCase(),
      background: background.toLowerCase(),
      ansi: ansi.map((c) => c.toLowerCase()),
      cursor: cursor.toLowerCase(),
    });
  }
</script>

{#if open}
  <div class="overlay" role="presentation" onclick={onCancel}>
    <div class="dialog" role="dialog" aria-modal="true" aria-label="配色方案编辑器" tabindex="-1"
         data-testid="scheme-editor" bind:this={dialogEl}
         onclick={(e) => e.stopPropagation()}
         onkeydown={(e) => { if (e.key === "Escape") onCancel(); }}>
      <h2>{initial ? "编辑配色" : "新建配色"}</h2>

      <label>名字
        <input bind:this={nameInput} bind:value={name} data-testid="scheme-name" maxlength="64" />
      </label>
      {#if nameError}
        <p class="err" data-testid="scheme-name-error">{nameError}</p>
      {/if}

      <div class="row">
        <label>前景 <input type="color" bind:value={foreground} data-testid="scheme-foreground" /></label>
        <label>背景 <input type="color" bind:value={background} data-testid="scheme-background" /></label>
        <label>光标 <input type="color" bind:value={cursor} data-testid="scheme-cursor" /></label>
      </div>

      <h3>ANSI 16 色</h3>
      <div class="ansi-grid">
        {#each ansi as _, i (i)}
          <label class="ansi-cell">
            <input type="color" bind:value={ansi[i]} data-testid={`scheme-ansi-${i}`} />
            <span>{i}·{ANSI_NAMES[i]}</span>
          </label>
        {/each}
      </div>

      <!-- 预览用配色自己的颜色渲染一小段终端样例。
           只看 16 个色块判断不出一套配色好不好用——前景压在背景上是否读得清，
           才是用户真正要看的那件事。 -->
      <h3>预览</h3>
      <pre class="preview" data-testid="scheme-preview"
           style:background={background} style:color={foreground}>{"$ "}<span style:color={ansi[2]}>ls</span> -la /var/log
<span style:color={ansi[4]}>drwxr-xr-x</span>  2 root root  4096 <span style:color={ansi[6]}>Aug 23 18:00</span> .
<span style:color={ansi[1]}>ERROR</span> connection refused  <span style:color={ansi[3]}>WARN</span> retrying…
<span style:background={cursor} style:color={background}>&nbsp;</span></pre>

      <footer>
        <button type="button" data-testid="scheme-save" disabled={!!nameError} onclick={save}>保存</button>
        <button type="button" data-testid="scheme-cancel" onclick={onCancel}>取消</button>
      </footer>
    </div>
  </div>
{/if}

<style>
  .overlay { position: fixed; inset: 0; background: rgba(0,0,0,.5); display: grid; place-items: center; z-index: 70; }
  .dialog { width: 520px; max-width: 94vw; max-height: 88vh; overflow: auto; padding: 16px 20px; background: var(--fs-bg-elevated); border: 1px solid var(--fs-border); border-radius: 6px; color: var(--fs-fg-primary); }
  h2 { margin: 0 0 10px; font-size: 14px; }
  h3 { margin: 14px 0 6px; font-size: 12px; color: var(--fs-fg-secondary); }
  label { display: flex; align-items: center; gap: 6px; font-size: 12px; margin: 6px 0; }
  /* 名字那个 <input> 没写 type，靠 :not([type]) 命中——写 input[type="text"] 会是死选择器。 */
  label input:not([type]) { flex: 1; }
  .row { display: flex; gap: 16px; }
  .err { color: var(--fs-danger, #e05252); font-size: 11.5px; margin: 2px 0 0; }
  .ansi-grid { display: grid; grid-template-columns: repeat(4, 1fr); gap: 4px 10px; }
  .ansi-cell { margin: 0; font-size: 11px; }
  .ansi-cell span { color: var(--fs-fg-secondary); }
  .preview { margin: 0; padding: 10px; border: 1px solid var(--fs-border); border-radius: 4px; font-size: 12px; line-height: 1.5; white-space: pre-wrap; }
  footer { display: flex; justify-content: flex-end; gap: 8px; margin-top: 14px; }
  footer button { padding: 5px 14px; border: 1px solid var(--fs-border); background: var(--fs-bg-panel); color: var(--fs-fg-primary); border-radius: 4px; cursor: pointer; }
  footer button:disabled { opacity: .5; cursor: default; }
</style>
