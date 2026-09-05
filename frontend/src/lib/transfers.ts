import { get, writable } from "svelte/store";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

/** 传输队列行（UI 规格 §1.4 TransferQueueDrawer 渲染单元；id = Task 12 管理器分配的传输作业 id） */
export interface Row {
  id: number; direction: string; local: string; remote: string; sessionId: string;
  done: number; total: number; state: string; attempt?: number;
  verify?: "sha256_match" | "size_only_match" | "mismatch" | "unverified";
  startedAt: number;
}

/** 传输作业终态集合（Task 12 `TransferState` 的 serde 变体名）。
 *  P2 审计：原判据只列 Done/Failed，漏了 Cancelled——用户取消的作业永远算作「排队中」，
 *  关闭窗口/标签的确认框此后每次必弹且计数不归零。终态判定只此一处，抽屉与关闭门控共用。 */
const TERMINAL_STATES = new Set(["Done", "Failed", "Cancelled"]);
export function isTerminal(state: string): boolean {
  return TERMINAL_STATES.has(state);
}

/**
 * 「全部取消」要取消的那些行（M4a）。
 *
 * 判据就是**非终态**，与关闭门控同一个 `isTerminal`——两处各写一份「哪些算在跑」
 * 迟早分叉，而分叉的表现是「全部取消」之后关闭确认框还在弹。
 *
 * 返回数组而不是只返回个数：调用方要逐件下发取消，还要在失败时报出**是哪几件**
 * 失败了。只回个数就只能给出一句「部分失败」。
 *
 * 顺序按 id 升序（= 提交顺序）：取消是逐件下发的，按提交顺序取消让「先提交的先停」
 * 这件事可预期；用 Map 的迭代序则取决于插入历史，看起来是随机的。
 */
export function cancellableRows(all: Map<number, Row>): Row[] {
  return [...all.values()].filter((r) => !isTerminal(r.state)).sort((a, b) => a.id - b.id);
}

/** 终态行保留上限：传输历史只增不减时，长时间运行（批量同步、日志拉取）会让 Map 无界增长，
 *  抽屉的 `{#each}` 也随之线性变慢。超限时按 startedAt 丢弃最旧的终态行；
 *  非终态行（Running/Retrying/排队中）**一律不裁**——它们是关闭门控的计数源，丢一条就漏一次确认。 */
const MAX_TERMINAL_ROWS = 200;

/** 全局传输队列 store（同 lib/tabs.ts 的 svelte/store writable 模式；runes $state 不可跨模块导出）：
 *  三条事件订阅经 initTransferStore 绑定一次，TransferQueueDrawer（Task 21 Step 4）与关闭确认门控（Task 22）共订；
 *  更新一律以新 Map 替换保证订户感知变更。
 *  P2 审计：抽屉组件曾自行 listen 同样三个事件并维护私有 Map，形成两套独立状态源
 *  （抽屉显示的行与关闭门控统计的行可以不一致）；现抽屉改订本 store，单一数据源。 */
export const rows = writable<Map<number, Row>>(new Map());

/** 抽屉展开态（UI 规格 §1.4「默认折叠、有新传输自动展开」）。
 *  自动展开的触发点在 transfer:submitted 处理里，而处理器已收归本模块，故展开态一并上提为 store，
 *  抽屉双向绑定即可——否则抽屉为了「自动展开」又得自建一条 listen，回到双状态源。 */
export const drawerOpen = writable(false);

/** 已注册的事件解绑函数。active 为幂等判据，generation 用于识别「已被 dispose 的那一批订阅」——
 *  listen 是异步的，init→dispose→init 快速往返时旧批次的 resolve 可能晚于新批次的注册，
 *  不按批次区分会把旧监听器混进新列表（旧的解不掉、新的被重复解）。 */
let unlisteners: UnlistenFn[] = [];
let active = false;
let generation = 0;

/** 把 `TransferState` 的 serde 形态还原成变体名：unit 变体是裸字符串（`"Running"`），
 *  带载荷的是单键对象（`{ Retrying: { attempt: 2 } }`、`{ Failed: "…" }`）。
 *  载荷损坏/字段缺失返回 undefined 交由调用方兜底——原先直接 `Object.keys(p.state)[0]`
 *  在 state 缺失时抛 TypeError，异常从 `rows.update` 里穿出去，这条事件就整个丢了。 */
function parseState(raw: unknown): string | undefined {
  if (typeof raw === "string") return raw;
  if (raw && typeof raw === "object") return Object.keys(raw)[0];
  return undefined;
}

/** 裁剪终态行至 MAX_TERMINAL_ROWS（就地改传入的新 Map；调用方已复制过）。 */
function trimTerminalRows(next: Map<number, Row>): void {
  const terminal = [...next.values()].filter((r) => isTerminal(r.state));
  if (terminal.length <= MAX_TERMINAL_ROWS) return;
  // startedAt 升序 → 头部即最旧；只删超出的那部分
  terminal.sort((a, b) => a.startedAt - b.startedAt);
  for (const r of terminal.slice(0, terminal.length - MAX_TERMINAL_ROWS)) next.delete(r.id);
}

/**
 * 初始化传输 store（App.svelte onMount 调用）：绑定三条后端事件并返回解绑函数。
 *
 * **事件乱序**：`transfer:submitted` 并不保证第一个到。后端 `transfer_submit` 在
 * `mgr.submit()` 返回后还要过一次 `verify_plans.lock().await`（真实的让出点）才 emit 它，
 * 而事件泵是另一个 task——作业被并发信号量放行就开始发 `transfer:progress`。
 * 后端自己为这一顺序建了握手（`RegisterOutcome::AlreadySettled` 分支，注释写着
 * 「终态已先到（小文件常见）」）。故本模块两条进度类事件一律 **upsert**：
 * progress 撞上未知 id 立占位行（描述字段留空等 submitted 补），submitted 撞上已有行
 * 只补描述字段、不回写 state/done/total。原实现 progress 遇未知 id 直接丢、submitted 无条件
 * 覆写，二者叠加的后果是终态行被钉死在 `Queued`——`isTerminal` 为假，关闭确认框此后每次
 * 必弹且「排队 N」永不归零。
 *
 * P2 审计：原实现把 listen 写在模块顶层且从不解绑——模块一被 import 就产生副作用（单测无法在
 * 不启动 Tauri 事件系统的前提下引用本模块），且监听器生命周期与 App 组件脱钩。现改为显式初始化，
 * 与 initTheme/initLayout 的调用形态对齐；重复调用是 no-op（返回既有解绑函数），防 HMR 叠加订阅。
 */
export function initTransferStore(): () => void {
  if (active) return disposeTransferStore;
  active = true;
  const gen = ++generation;

  // listen 返回 Promise<UnlistenFn>：若在 resolve 前就 dispose，解绑函数将无人认领 →
  // resolve 时发现本批次已作废（gen 过期或整体停用）就地解绑，不留悬挂监听器。
  const track = (p: Promise<UnlistenFn>) => {
    void p.then((un) => {
      if (active && gen === generation) unlisteners.push(un);
      else void un();
    });
  };

  track(listen("transfer:submitted", (e: any) => {
    const p = e.payload;
    rows.update((m) => {
      const next = new Map(m);
      // 事件泵可能已经先到（见文件头「事件乱序」注）：那种情况下本事件只补描述性字段，
      // 进度与状态一律以已到的进度事件为准，绝不回写。
      const prior = m.get(p.id);
      next.set(p.id, {
        id: p.id, direction: p.direction, local: p.local, remote: p.remote,
        sessionId: p.sessionId,
        done: prior?.done ?? 0,
        total: prior?.total ?? 0,
        // state 取后端下发值（`transfer_submit` 现在 emit `"Queued"`），只在字段缺失时兜底。
        // 原先硬编码 "Running" 是在替后端撒谎：作业进的是并发信号量的等待队列，可能几分钟
        // 都轮不到执行，抽屉却显示「传输中 0/0」。用户看不出是卡住了还是在排队，唯一的证据
        // ——进度条一直不动——恰好也是真卡住时的表现。兜底写 "Queued" 而非 "Running"：
        // 事件重放/旧后端时，把未知状态说成「排队中」至少不会把停滞谎报成进行中。
        state: prior?.state ?? p.state ?? "Queued",
        attempt: prior?.attempt,
        verify: prior?.verify,
        startedAt: prior?.startedAt ?? Date.now(),
      });
      trimTerminalRows(next);
      return next;
    });
    drawerOpen.set(true); // 有新传输自动展开（UI 规格 §1.4）
  }));

  track(listen("transfer:progress", (e: any) => {
    const p = e.payload;
    rows.update((m) => {
      const r = m.get(p.id);
      const next = new Map(m);
      next.set(p.id, {
        id: p.id,
        // 描述性字段只有 submitted 事件带得出（引擎的 `TransferEvent` 里没有
        // direction/local/remote）。本事件先到时留空占位，等 submitted 补齐。
        direction: r?.direction ?? "",
        local: r?.local ?? "",
        remote: r?.remote ?? "",
        // 后端补发 sessionId 后以其为准；该字段缺失时保留 submitted 时记下的值——
        // 直接取 p.sessionId 会把已有行的归属抹成 undefined，标签级关闭门控随之漏计（R1 计数源）
        sessionId: p.sessionId ?? r?.sessionId ?? "",
        done: p.bytes_done, total: p.bytes_total,
        state: parseState(p.state) ?? r?.state ?? "Running",
        attempt: p.state?.Retrying?.attempt,
        verify: r?.verify,
        startedAt: r?.startedAt ?? Date.now(),
      });
      trimTerminalRows(next);
      return next;
    });
  }));

  // 与上面两条相反，校验结果在行不存在时**丢弃**，这是刻意的不对称：
  // `transfer_verified` 只跟在 Done 之后发，而那条 Done 进度事件已经会把行立起来；
  // 此刻仍查不到，唯一的成因是该行已被 trimTerminalRows 按上限裁掉。
  // 若这里也 upsert，就会复活一条 state 为空的幽灵行——空 state 不是终态，
  // 关闭门控会把它永远算成「排队中」。
  track(listen("transfer_verified", (e: any) => {
    rows.update((m) => {
      const r = m.get(e.payload.id);
      if (!r) return m;
      const next = new Map(m);
      next.set(e.payload.id, { ...r, verify: e.payload.outcome });
      return next;
    });
  }));

  return disposeTransferStore;
}

/** 解绑三条事件订阅（App.svelte onDestroy 调用）；幂等。尚未 resolve 的 listen 由 track 的批次判据兜底。 */
export function disposeTransferStore(): void {
  active = false;
  const pending = unlisteners;
  unlisteners = [];
  for (const un of pending) void un();
}

/** 派生助手：活动传输计数（R1 关闭门控计数源：Task 20 ④ requestCloseTab 标签级 / Task 22 Step 4(e) 窗口级 / Task 21 抽屉文案共用）：
 *  running 计 state∈{Running, Retrying}，queued 计已提交未 Running 且未到终态（Done/Failed/Cancelled）者；
 *  sessionId 省略为全局合计，传值按 Row.sessionId 过滤。同步取 store 快照（get），供事件回调外直接调用。 */
export function activeTransfers(sessionId?: string): { queued: number; running: number } {
  let queued = 0;
  let running = 0;
  for (const r of get(rows).values()) {
    if (sessionId !== undefined && r.sessionId !== sessionId) continue;
    if (r.state === "Running" || r.state === "Retrying") running += 1;
    else if (!isTerminal(r.state)) queued += 1;
  }
  return { queued, running };
}
