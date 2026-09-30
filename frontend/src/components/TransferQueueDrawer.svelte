<script lang="ts">
  /**
   * 全局传输队列抽屉（UI 规格 §1.4）：挂载于 App.svelte、跨标签持久可见（传输为会话级，不随标签切换消失）；
   * 默认折叠、有新传输自动展开。
   * 单件取消经 `transfer_cancel` 于 MVP 提供（非终态行显「取消」按钮）；
   * 「全部取消」已于 M4a 落地（头部按钮，逐件下发 + 收集失败 + 先确认，见 cancelAll）；
   * 多文件拖拽仍属 M4a 未落项。失败作业经管理器 3 次退避自动重试，
   * 终态 Failed 行提供重试按钮（携原参数 + 断点偏移重提交）。
   *
   * P2 审计：本组件原先自行 listen transfer:submitted / transfer:progress / transfer_verified 并维护私有 Map，
   * 与 lib/transfers.ts 的同名订阅并存 —— 两套状态各算各的，抽屉显示的行与关闭确认门控（activeTransfers）
   * 统计的行可以对不上，且事件从不解绑。现统一由 lib/transfers.ts 持有唯一订阅，本组件只读 $rows / $drawerOpen。
   */
  import { onDestroy, onMount } from "svelte";
  import { invoke, settingChanged, settingGet, settingSet } from "../lib/ipc";
  import { rows, drawerOpen, isTerminal, cancellableRows, type Row } from "../lib/transfers";
  import { toast } from "../lib/toast";
  import ConfirmDialog from "./ConfirmDialog.svelte";

  /**
   * 传后校验开关（UI 规格 §1.4「传后校验（**可选开关，默认开**）」；§附表把该开关的载体钉在本组件）。
   *
   * 后端 `sftp_cmd.rs` 一直以 `settings_bool("transfer.verifyAfterTransfer", true)` 消费它，
   * 而全仓**没有任何写入方**——即「可选」的那一半从来不存在，用户关不掉，每次传输都要跑一趟
   * 远端 sha256sum。默认 true 是安全方向，所以它不表现为数据事故，只是一个不可关的开关。
   *
   * onMount 首读同 SettingsDialog 的规矩：fallback 与 $state 初值逐字一致，且与后端的 `true` 一致——
   * 三处默认值若走散，界面显示的、库里存的、后端实际用的会是三个值。
   *
   * 本组件整体包在 `{#if $rows.size > 0}` 里，本会话有第一件传输之前不渲染；而这个键是持久的、
   * 后端又在 `transfer_submit` 当刻读它。只留这一处控件的话，上次关掉它的用户在传第一个文件
   * 之前无处可开——那恰恰是最需要它的一件。故 SettingsDialog 里补了第二处控件（随时可达），
   * 这里订阅 `settingChanged` 跟随，避免两处各显各的值。
   */
  let verifyAfterTransfer = $state(true);
  let unSetting: (() => void) | null = null;
  onMount(async () => {
    verifyAfterTransfer = await settingGet<boolean>("transfer.verifyAfterTransfer", true);
    // 订阅在首读**之后**建立：settingChanged 是保留末值的 writable，先订阅会先收到一次重放，
    // 随后 await 落地的旧读又把它盖回去（两者顺序颠倒即陈旧值取胜）。
    unSetting = settingChanged.subscribe((ev) => {
      if (ev?.key === "transfer.verifyAfterTransfer") verifyAfterTransfer = ev.value !== false; // 默认开，非布尔按默认解
    });
  });
  onDestroy(() => unSetting?.());

  const VERIFY_BADGE: Record<string, string> = {
    sha256_match: "✓ 已核对 SHA256",
    size_only_match: "仅大小核对（降级：服务端无 sha256sum）",
    mismatch: "校验失败",
    unverified: "未核对（降级：大小信息不可取）",
  };
  // Queued：作业已提交但仍在并发信号量的等待队列里，一个字节都没发。缺这一项时
  // `STATE_LABEL[r.state] ?? r.state` 会把裸英文 "Queued" 印到界面上。
  const STATE_LABEL: Record<string, string> = { Queued: "排队中", Running: "传输中", Done: "完成", Failed: "失败", Cancelled: "已取消" };
  function rate(r: Row): string {
    const s = Math.max((Date.now() - r.startedAt) / 1000, 0.001);
    return `${(r.done / 1048576 / s).toFixed(1)} MB/s`;
  }
  function eta(r: Row): string {
    if (r.done <= 0 || r.total <= r.done) return "";
    const bps = r.done / Math.max((Date.now() - r.startedAt) / 1000, 0.001);
    return `ETA ${Math.round((r.total - r.done) / bps)}s`;
  }
  async function retry(r: Row) {
    await invoke("transfer_submit", { sessionId: r.sessionId, direction: r.direction,
      local: r.local, remote: r.remote, resumeOffset: r.done }); // 断点续传（Down 方向管理器再按本地 size 校正）
  }

  /**
   * 「全部取消」（M4a）。
   *
   * 三条决定：
   * ① **先确认**。取消在途传输会留下 `.fspart` 半成品，而用户可能只是想停一件却点了
   *    全部；确认框里报出件数与涉及的会话数，让人看清波及面。
   * ② **逐件下发并收集失败**。会话已断时那一件的 cancel 会失败，而其余的该照常取消。
   *    报「N 已请求 / M 失败」而不是一句「已取消」——后者在部分失败时是假话。
   * ③ **不乐观改状态**。取消是一个请求：作业可能正在写最后一块，终态由后端事件带回来。
   *    这里把行标成「已取消」会让用户看到一个还在涨进度条的「已取消」行。
   */
  let confirmCancelAll = $state(false);
  let cancelling = $state(false);
  const pending = $derived(cancellableRows($rows));
  const pendingSessions = $derived(new Set(pending.map((r) => r.sessionId)).size);

  async function cancelAll() {
    confirmCancelAll = false;
    cancelling = true;
    const targets = pending; // 定格：期间可能又有新作业提交，那些不在本次意图内
    const failed: { id: number; error: string }[] = [];
    for (const r of targets) {
      try {
        await invoke("transfer_cancel", { sessionId: r.sessionId, id: r.id });
      } catch (e) {
        failed.push({ id: r.id, error: String(e) });
      }
    }
    cancelling = false;
    if (failed.length === 0) {
      toast.info(`已请求取消 ${targets.length} 件传输`);
    } else {
      toast.error(
        `取消 ${targets.length} 件：${targets.length - failed.length} 已请求 / ${failed.length} 失败` +
          `（#${failed.map((f) => f.id).join("、#")}）`,
      );
    }
  }
</script>

{#if $rows.size > 0}
  <div class="drawer">
    <div class="head">
      <button class="handle" onclick={() => drawerOpen.set(!$drawerOpen)}>传输队列 ({$rows.size}) {$drawerOpen ? "▾" : "▴"}</button>
      <!-- 无在途作业时禁用而不是隐藏：按钮时隐时现会让人以为界面出了问题，
           而一个明确禁用的按钮同时说明了「现在没有可取消的东西」。 -->
      <button
        class="cancel-all" data-testid="transfer-cancel-all"
        disabled={pending.length === 0 || cancelling}
        title={pending.length === 0 ? "没有在途传输" : `取消 ${pending.length} 件在途传输`}
        onclick={() => (confirmCancelAll = true)}
      >{cancelling ? "取消中…" : `全部取消 (${pending.length})`}</button>
      <label class="verify-opt" title="传输后比对 SHA-256；服务器不支持时改为比对文件大小，并在结果中标注「降级」">
        <input type="checkbox" bind:checked={verifyAfterTransfer} data-testid="set-verify-after-transfer"
          onchange={() => void settingSet("transfer.verifyAfterTransfer", verifyAfterTransfer)} /> 传后校验
      </label>
    </div>
    {#if $drawerOpen}
      <ul>
        {#each [...$rows.values()] as r (r.id)}
          <li>
            <span class="name" title={r.local}>#{r.id} {r.direction === "up" ? "↑" : "↓"} {r.remote.split("/").pop()}</span>
            <progress value={r.done} max={Math.max(r.total, 1)}></progress>
            <span class="rate">{rate(r)} {eta(r)}</span>
            <span class="state">{r.state === "Retrying" ? `重试 #${r.attempt ?? "?"}` : STATE_LABEL[r.state] ?? r.state}</span>
            {#if r.verify}
              <span class="verify" class:bad={r.verify === "mismatch"}
                    class:warn={r.verify === "size_only_match" || r.verify === "unverified"}>
                {VERIFY_BADGE[r.verify]}</span>
            {/if}
            {#if r.state === "Failed"}
              <button onclick={() => retry(r)}>重试</button>
            {:else if !isTerminal(r.state)}
              <!-- 终态判据统一取自 lib/transfers.ts：Failed 已被上一分支接走，此处剩 Done/Cancelled 不显「取消」 -->
              <button onclick={() => invoke("transfer_cancel", { sessionId: r.sessionId, id: r.id })}>取消</button>
            {/if}
          </li>
        {/each}
      </ul>
    {/if}
  </div>
{/if}

<ConfirmDialog
  open={confirmCancelAll}
  title="全部取消"
  message={`将请求取消 ${pending.length} 件在途传输` +
    (pendingSessions > 1 ? `（涉及 ${pendingSessions} 个会话）` : "") +
    `。\n\n已传输的部分会留下 .fspart 半成品文件；重新提交同一件可从断点续传。`}
  confirmText={`取消这 ${pending.length} 件`}
  danger
  onConfirm={() => void cancelAll()}
  onCancel={() => (confirmCancelAll = false)}
/>

<style>
  .drawer { flex: none; background: var(--fs-bg-elevated); border-top: 1px solid var(--fs-border); }
  .cancel-all { flex: none; padding: 1px 8px; margin-right: 4px; font-size: 12px; cursor: pointer;
                background: var(--fs-bg-panel); color: var(--fs-fg-primary);
                border: 1px solid var(--fs-border); border-radius: 4px; white-space: nowrap; }
  .cancel-all:disabled { opacity: .5; cursor: default; }
  .head { display: flex; align-items: center; gap: 8px; }
  .handle { flex: 1; min-width: 0; text-align: left; padding: 2px 10px; background: none; border: 0; color: var(--fs-fg-primary); cursor: pointer; font-size: 12px; }
  .verify-opt { display: flex; align-items: center; gap: 4px; padding-right: 10px; font-size: 12px; color: var(--fs-fg-secondary); white-space: nowrap; cursor: pointer; }
  ul { list-style: none; margin: 0; padding: 0 10px 6px; max-height: 160px; overflow-y: auto; }
  li { display: flex; gap: 8px; align-items: center; font-size: 12px; padding: 2px 0; }
  .name { max-width: 240px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  progress { flex: 1; }
  .verify { color: var(--fs-ok); }
  .verify.warn { color: var(--fs-warn); }
  .verify.bad { color: var(--fs-danger); }
</style>
