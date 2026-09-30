<script lang="ts">
  /**
   * 虚拟窗口画布（M7.3）：铺在终端区之上，装所有打开的虚拟窗口。
   *
   * # 为什么这一层要自己量尺寸
   *
   * 夹紧要的是**画布**的尺寸，不是浏览器视口：画布上方有标签栏、下方有状态栏、右边可能
   * 还开着 AI 面板。拿 `window.innerWidth` 去夹，窗口就能被推到状态栏底下——那正是
   * 出口标准要禁掉的「拖丢了找不回来」。故用 ResizeObserver 盯住本元素的真实尺寸，
   * 变化时 `reflowVWindows` 把所有窗口重新夹一遍（主窗口缩小、监控面板拉开都会走到这里）。
   *
   * # 画布本身不吃事件
   *
   * `pointer-events: none` 在画布上、`auto` 在窗口上：没有窗口的地方点下去应该落到底下的
   * 终端上（用户看到的就是终端，点了却没反应会让人以为终端卡死了）。
   */
  import { onMount } from "svelte";
  import SftpPane from "./SftpPane.svelte";
  import VirtualWindow from "./VirtualWindow.svelte";
  import { reflowVWindows, vwindows, type Viewport } from "../lib/vwindow";

  let {
    /** 窗口里那个文件面板要开新终端标签时（双击 .sh → 前台运行）转给 App。 */
    onRunInNewTab = (_cmd: string) => {},
    /** 档案默认目录：按 sessionId 查（与 TerminalPane 里那份同源，由 App 装配）。 */
    dirsFor = (_sessionId: string) => ({ local: null as string | null, remote: null as string | null }),
  }: {
    onRunInNewTab?(command: string): void;
    dirsFor?(sessionId: string): { local: string | null; remote: string | null };
  } = $props();

  let el = $state<HTMLDivElement | undefined>();
  let viewport = $state<Viewport>({ width: 0, height: 0 });

  onMount(() => {
    if (!el) return;
    const ro = new ResizeObserver(() => {
      if (!el) return;
      viewport = { width: el.clientWidth, height: el.clientHeight };
      reflowVWindows(viewport);
    });
    ro.observe(el);
    viewport = { width: el.clientWidth, height: el.clientHeight };
    return () => ro.disconnect();
  });
</script>

<div class="layer" bind:this={el} data-testid="vwindow-layer">
  {#each $vwindows as w (w.id)}
    <VirtualWindow win={w} {viewport}>
      {#if w.kind === "sftp"}
        <SftpPane
          sessionId={w.sessionId}
          localDir={dirsFor(w.sessionId).local}
          remoteDir={dirsFor(w.sessionId).remote}
          floating
          onRunInNewTab={(cmd) => onRunInNewTab(cmd)}
        />
      {/if}
    </VirtualWindow>
  {/each}
</div>

<style>
  .layer { position: absolute; inset: 0; pointer-events: none; overflow: hidden; }
  .layer > :global(.vwin) { pointer-events: auto; }
</style>
