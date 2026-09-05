<script lang="ts">
  import { toast } from "../lib/toast";
</script>

<div class="toasts" data-testid="toasts">
  {#each $toast as t (t.id)}
    <div class="toast {t.level}" class:leaving={t.leaving} role="alert" data-testid="toast-{t.id}">
      <span>{t.msg}</span>
      <button aria-label="关闭" onclick={() => toast.dismiss(t.id)}>×</button>
    </div>
  {/each}
</div>

<style>
  .toasts { position: fixed; bottom: 20px; right: 20px; display: flex; flex-direction: column; gap: 8px; z-index: 100; }
  /* 进场：既有 keyframes，时长改走动效 token（styles.css「动效」段），与全仓一致。 */
  .toast { display: flex; align-items: center; gap: 10px; padding: 10px 14px; background: var(--fs-bg-elevated); border: 1px solid var(--fs-border); border-radius: 6px; box-shadow: 0 4px 12px rgba(0,0,0,.15); min-width: min(280px, calc(100vw - 40px)); max-width: min(420px, calc(100vw - 40px)); overflow-wrap: anywhere; font-size: 13px; animation: toast-in var(--fs-motion-base, 140ms) var(--fs-ease-out, ease-out); }
  .toast span { min-width: 0; }
  .toast button { flex: none; }
  .toast.info { border-left: 3px solid var(--fs-accent); }
  .toast.warn { border-left: 3px solid #f59e0b; }
  .toast.error { border-left: 3px solid var(--fs-danger); }
  .toast button { border: 0; background: transparent; color: var(--fs-fg-secondary); cursor: pointer; font-size: 18px; line-height: 1; }
  /* 退场（路线图 4c 动效补齐）：lib/toast.ts 的 dismiss 先标 leaving、TOAST_EXIT_MS 后才删节点，
     这里负责那一拍的淡出下沉。prefers-reduced-motion 下全局规则把 transition 清零，store 侧同步改为立即删。
     pointer-events: none——正在消失的东西不该还接住点击。 */
  .toast.leaving { opacity: 0; transform: translateY(10px); pointer-events: none; transition: opacity var(--fs-motion-base, 140ms) var(--fs-ease-out, ease-out), transform var(--fs-motion-base, 140ms) var(--fs-ease-out, ease-out); }
  @keyframes toast-in { from { opacity: 0; transform: translateY(10px); } to { opacity: 1; transform: translateY(0); } }
</style>
