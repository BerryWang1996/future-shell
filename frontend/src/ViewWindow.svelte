<script lang="ts">
  /**
   * 视图窗口（M4b「标签拖出/平铺」）。
   *
   * 一个只装一个终端的窗口，显示 URL 里 `?view=<sessionId>` 指定的那个会话。
   *
   * # 为什么是独立入口而不是 App.svelte 的一个分支
   *
   * App.svelte 装配的是整个主界面：会话管理器、标签栏、工具栏、状态栏、十几个模态、
   * 恢复对话框、拖拽上传……在它里面加一个「其实这次什么都不要渲染」的分支，
   * 等于让那一整套装配逻辑在一个它从没被设计过的形态下运行。
   * 独立入口把这件事变成了：**这个窗口本来就只有一个终端。**
   *
   * # 「输入同步到同一会话」不需要任何同步机制
   *
   * 会话活在后端，输出经 `term:data:{sessionId}` 广播（Tauri 的 emit 是全窗口的），
   * 输入走 `term_input(sessionId, …)` 进同一个 PTY。两个窗口看的本来就是同一份流，
   * 所以不存在「两个视图不一致」这种 bug——唯一各自独立的是滚动位置与选区，
   * 而那正该各自独立。
   */
  import { onMount } from "svelte";
  import TerminalPane from "./components/TerminalPane.svelte";
  import Toast from "./components/Toast.svelte";
  import { initTheme } from "./lib/theme/store";
  import { initLocale } from "./lib/i18n";
  import { parseViewParams } from "./lib/view-window";

  /** URL 里指定的会话与标题。解析与 main.ts 的入口判据共用一处（见 lib/view-window.ts）。 */
  const { sessionId, title } = parseViewParams(window.location.search);

  /** 会话是否还活着。对端断了 / 用户在主窗口里关掉了这个标签，都会走到这里。 */
  let alive = $state(true);

  onMount(() => {
    void initTheme();
    void initLocale();
    // 任务栏上要分得清哪个窗口是哪个——三个都叫「FutureShell」的窗口没法选。
    if (title) document.title = `${title} — FutureShell`;
  });
</script>

<div class="view-window">
  {#if !sessionId}
    <!-- 「新建窗口」开出来的空窗口。不假装它是主界面：一个没有侧栏、
         没有标签栏的窗口摆出主界面的样子只会让人以为程序坏了。 -->
    <div class="empty" data-testid="view-window-empty">
      <p>这是一个新窗口。</p>
      <p class="hint">在主窗口里把标签拖到这里，或者从会话管理器新建一个连接。</p>
    </div>
  {:else if alive}
    <TerminalPane
      {sessionId}
      onLocalAction={(id) => {
        // 视图窗口里没有菜单，本地动作只有终端自己那几个（清屏等）由 TerminalPane
        // 内部处理。走到这里的是需要主界面才做得到的动作——忽略比假装做了好。
        void id;
      }}
    />
  {:else}
    <div class="empty" data-testid="view-window-closed">
      <p>这个会话已经关闭。</p>
      <p class="hint">可以关掉这个窗口了。</p>
    </div>
  {/if}
</div>
<Toast />

<style>
  .view-window { position: fixed; inset: 0; display: flex; flex-direction: column; background: var(--fs-bg-base); color: var(--fs-fg-primary); }
  .empty { margin: auto; text-align: center; font-size: 13px; }
  .empty p { margin: 4px 0; }
  .hint { color: var(--fs-fg-secondary); font-size: 11.5px; }
</style>
