<script lang="ts">
  type SessionStatus = "connecting" | "connected" | "disconnected" | "error";

  let {
    status = null,
    host = null,
    encoding = "UTF-8",
    term = "xterm-256color",
    rows = 24,
    cols = 80,
    vaultLocked = true,
    keyboardMode = "remote",
    caps = false,
    num = false,
    onEncodingClick = null,
    onHostClick = null,
    onVaultClick = () => {},
    onKeyboardModeClick = () => {},
    onReconnect = () => {},
    onStatusClick = null,
    errorText,
    onErrorClick = null,
  }: {
    status?: SessionStatus | null;
    host?: string | null;
    encoding?: string;
    term?: string;
    rows?: number;
    cols?: number;
    vaultLocked?: boolean;
    /** 键盘模式（UI 规格 §2.7）：remote=按键原样发会话，local=快捷键作用于应用 UI。MVP 默认远程。 */
    keyboardMode?: "local" | "remote";
    caps?: boolean;
    num?: boolean;
    /** M7.4：点编码段换终端编码（切换不重连）。null = 无会话时不可点。 */
    onEncodingClick?: (() => void) | null;
    /** M7.4：串口会话点主机段改波特率。null = 非串口会话（此时该段是只读文本）。 */
    onHostClick?: (() => void) | null;
    onVaultClick?(): void;
    onKeyboardModeClick?(): void;
    /** M4a 连接详情弹层（UI 规格 §2.7）：点状态段回投装配层。null = 无会话时不可点。 */
    onStatusClick?: (() => void) | null;
    onReconnect?(): void;
    errorText?: string;
    /** 错误态下点状态段：打开完整错误（2026-09-01 用户报「报错文本很长看不全」）。
     *  状态栏 24px 固定高 + nowrap，长文必被裁——而这段文本恰恰是用户唯一能拿去
     *  搜索/求助的东西，读不全等于排障断路。 */
    onErrorClick?: (() => void) | null;
  } = $props();

  const STATUS_TEXT: Record<SessionStatus, string> = {
    connecting: "连接中…",
    connected: "已连接",
    disconnected: "已断开",
    error: "错误",
  };
</script>

<footer class="statusbar" role="status" data-testid="statusbar">
  <svelte:element
    this={onStatusClick ? "button" : "span"}
    class="seg"
    class:clickable={!!onStatusClick}
    role={onStatusClick ? "button" : "status"}
    title={status === "error" ? `${errorText}

（点击查看完整错误）` : (onStatusClick ? "点击查看连接详情（认证方式/密钥指纹/往返延时）" : undefined)}
    data-testid="status-seg"
    onclick={status === "error" && errorText && onErrorClick ? onErrorClick : (onStatusClick ?? undefined)}
  >
    <i
      class="dot"
      class:ok={status === "connected"}
      class:warn={status === "connecting"}
      class:err={status === "error"}
      aria-hidden="true"
    ></i>
    {status === "error" && errorText ? `错误：${errorText}` : (status ? STATUS_TEXT[status] : "无会话")}
    {#if status === "error" && errorText}<span class="more" data-testid="status-error-more">详情</span>{/if}
    {#if status === "disconnected"}<button class="link" onclick={onReconnect}>[重连]</button>{/if}
  </svelte:element>
  {#if onHostClick}
    <button class="seg link p-mid" data-testid="status-host"
            title="改波特率（立即生效，不重开端口）" onclick={onHostClick}>{host ?? "—"}</button>
  {:else}
    <span class="seg dim p-mid" data-testid="status-host">{host ?? "—"}</span>
  {/if}
  <!-- 编码段（M7.4 起可点）：显示当前会话的终端编码，点击换一个。
       无会话时退回只读文本——一个点了什么都不会发生的按钮比不可点更糟。 -->
  {#if onEncodingClick}
    <button class="seg link p-low" data-testid="status-encoding"
            title="切换终端编码（立即生效，不需要重连）" onclick={onEncodingClick}>{encoding}</button>
  {:else}
    <span class="seg dim p-low" data-testid="status-encoding" title="终端编码（连接后可切换）">{encoding}</span>
  {/if}
  <span class="seg dim p-low">{term}</span>
  <span class="seg dim p-mid">{rows}×{cols}</span>
  <button class="seg link" aria-label={vaultLocked ? "解锁保险库" : "锁定保险库"} title={vaultLocked ? "点击解锁 Vault" : "点击锁定 Vault"} onclick={onVaultClick}>
    {vaultLocked ? "🔒" : "🔓"}
  </button>
  <span class="seg dim p-key" class:on={caps}>Caps</span>
  <span class="seg dim p-key" class:on={num}>Num</span>
  <!-- 键盘模式段：点击切换为三平台主入口；Scroll Lock 为 Windows 肌肉记忆快捷，二者等价（UI 规格 §2.7，i4 裁决）。默认远程。 -->
  <button class="seg link" title="切换键盘模式（本地/远程）" onclick={onKeyboardModeClick}>
    键盘: {keyboardMode === "local" ? "本地" : "远程"}
  </button>
  <!-- AI 状态段（Phase 2 预留）：✨ 就绪/思考中/执行中 -->
</footer>

<style>
  .statusbar { display: flex; align-items: center; gap: 0; padding: 0 8px; height: 24px; background: var(--fs-bg-panel); border-top: 1px solid var(--fs-border); font-size: 11.5px; color: var(--fs-fg-primary); flex: none; }
  /* 「详情」小标：告诉用户这条被截断的错误点得开（2026-09-01）。 */
  .more { margin-left: 6px; padding: 0 5px; border: 1px solid currentColor; border-radius: 3px; font-size: 10.5px; opacity: .85; flex: none; }
  .seg { display: inline-flex; align-items: center; gap: 4px; padding: 0 8px; border-right: 1px solid var(--fs-border); white-space: nowrap; }

  /* 窄窗降级阶梯（2026-08-31 评审 P2-7）：全局 overflow-x hidden 之后，
   * 「放不下」不能等于「无声消失」——按信息价值逐级退场，交互段
   * （状态/重连/Vault/键盘模式）任何宽度都保留。
   * 退场顺序的裁决：编码与终端类型信息量最低（编码 v1 恒 UTF-8；终端类型
   * 从不变化）→ 最先退；主机名在标签标题里已有 → 其次；行列数与 Caps/Num
   * 是操作时有用的 → 最后退。窗口最小 800px（tauri.conf），阶梯在
   * 800–980 区间就已开始生效。 */
  @media (max-width: 980px) { .p-low { display: none; } }
  @media (max-width: 860px) { .p-mid { display: none; } }
  @media (max-width: 820px) { .p-key { display: none; } }
  .seg:last-child { border-right: none; margin-left: auto; }
  .dim { color: var(--fs-fg-secondary); }
  .dim.on { color: var(--fs-fg-primary); font-weight: 600; }
  .link { border: none; background: none; color: var(--fs-fg-primary); cursor: pointer; font: inherit; }
  .link:hover { color: var(--fs-accent); }
  /* M4a：状态段在有会话时是按钮（点开连接详情），无会话时是普通 span——
     不可点的按钮比不是按钮更糟（鼠标变手型却什么都不发生）。 */
  .seg.clickable { background: none; border: none; font: inherit; color: inherit; cursor: pointer; }
  .seg.clickable:hover { background: var(--fs-bg-hover); }
  .dot { width: 8px; height: 8px; border-radius: 50%; background: var(--fs-fg-disabled); display: inline-block; }
  .dot.ok { background: var(--fs-ok); box-shadow: 0 0 5px var(--fs-ok); }
  .dot.warn { background: var(--fs-warn); }
  .dot.err { background: var(--fs-danger); }
  /* 高对比度（房规见 styles.css）：三色点只靠底色区分，强制颜色下三个一模一样。
     裸 `.dot`（无修饰类）同样要给个底色，否则它变透明——一个看不见的指示点比没有更糟。 */
  @media (forced-colors: active) {
    .dot { background: GrayText; }
    .dot.ok { background: CanvasText; border: 0; border-radius: 50%; }
    .dot.warn { background: Canvas; border: 2px solid CanvasText; border-radius: 50%; }
    .dot.err { background: CanvasText; border: 0; border-radius: 0; }
  }
</style>
