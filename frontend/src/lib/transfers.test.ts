import { describe, it, expect, beforeEach, vi } from "vitest";
import { get } from "svelte/store";

/** 事件名 → 处理器。initTransferStore 绑定后由测试直接投递 payload。 */
const handlers = new Map<string, (e: any) => void>();
vi.mock("@tauri-apps/api/event", () => ({
  listen: (name: string, cb: (e: any) => void) => {
    handlers.set(name, cb);
    return Promise.resolve(() => handlers.delete(name));
  },
}));

import { rows, drawerOpen, initTransferStore, disposeTransferStore, activeTransfers, isTerminal, cancellableRows, type Row } from "./transfers";

/** listen 是异步的（track 里 `.then` 收解绑函数），绑定完成要让出一次微任务队列。 */
async function boot(): Promise<void> {
  initTransferStore();
  await Promise.resolve();
  await Promise.resolve();
}

function emit(name: string, payload: any): void {
  const h = handlers.get(name);
  if (!h) throw new Error(`未绑定事件 ${name}`);
  h({ payload });
}

const SUBMITTED = {
  id: 1, direction: "up", local: "C:/a.bin", remote: "/srv/a.bin", sessionId: "s1", state: "Queued",
};

describe("transfers store", () => {
  beforeEach(() => {
    disposeTransferStore();
    handlers.clear();
    rows.set(new Map());
    drawerOpen.set(false);
  });

  // 后端 `transfer_submit` emit 的是 `state: "Queued"`（作业只是入了并发信号量的等待队列，
  // 一个字节都没发）。此前前端在这里硬编码 "Running"，抽屉于是显示「传输中 0/0」——
  // 用户分不清是在排队还是卡死了，而「进度条不动」恰好也是真卡死的表现。
  it("transfer:submitted 采用后端下发的 Queued，不再谎报 Running", async () => {
    await boot();
    emit("transfer:submitted", SUBMITTED);
    expect(get(rows).get(1)!.state).toBe("Queued");
    expect(activeTransfers()).toEqual({ queued: 1, running: 0 });
    expect(get(drawerOpen)).toBe(true);
  });

  it("旧后端/事件重放缺 state 时兜底为 Queued 而非 Running", async () => {
    await boot();
    emit("transfer:submitted", { ...SUBMITTED, state: undefined });
    expect(get(rows).get(1)!.state).toBe("Queued");
  });

  it("Queued 不是终态：关闭门控必须拦得住", () => {
    expect(isTerminal("Queued")).toBe(false);
  });

  it("progress 到来后转 Running，计数从 queued 挪到 running", async () => {
    await boot();
    emit("transfer:submitted", SUBMITTED);
    emit("transfer:progress", { id: 1, bytes_done: 10, bytes_total: 100, state: "Running" });
    expect(activeTransfers()).toEqual({ queued: 0, running: 1 });
    expect(get(rows).get(1)!.done).toBe(10);
  });

  it("Retrying 的 serde 外层形态（对象）被拆成变体名并带出 attempt", async () => {
    await boot();
    emit("transfer:submitted", SUBMITTED);
    emit("transfer:progress", { id: 1, bytes_done: 10, bytes_total: 100, state: { Retrying: { attempt: 2 } } });
    expect(get(rows).get(1)!.state).toBe("Retrying");
    expect(get(rows).get(1)!.attempt).toBe(2);
  });

  // ---------------------------------------------------------------------
  // 事件乱序：progress 先于 submitted 到达
  //
  // 这不是理论竞态，后端自己为它建了专门的代码路径：`transfer_submit` 在
  // `mgr.submit()` 返回之后还要 `verify_plans.lock().await`（一个真实的让出点），
  // 才轮到 emit `transfer:submitted`；而事件泵是**另一个** task，作业一旦被信号量
  // 放行就开始发 progress。sftp_cmd.rs 里那句「终态已先到（小文件常见）」与
  // `RegisterOutcome::AlreadySettled` 分支，正是后端为这一顺序准备的握手。
  // 前端此前没有对应握手：progress 撞上未知 id 直接 `return m` 丢弃。
  // ---------------------------------------------------------------------

  it("progress 先到时立占位行，不丢事件", async () => {
    await boot();
    emit("transfer:progress", { id: 7, sessionId: "s1", bytes_done: 40, bytes_total: 100, state: "Running" });
    const r = get(rows).get(7);
    expect(r, "progress 撞上未知 id 不得丢弃——它是关闭门控与抽屉的唯一进度来源").toBeTruthy();
    expect(r!.state).toBe("Running");
    expect(r!.done).toBe(40);
    expect(activeTransfers("s1")).toEqual({ queued: 0, running: 1 });
  });

  it("submitted 后到只补描述字段，不把已跑完的行打回 Queued", async () => {
    await boot();
    // 小文件：Running→Done 都在 submitted 之前发完
    emit("transfer:progress", { id: 7, sessionId: "s1", bytes_done: 100, bytes_total: 100, state: "Done" });
    emit("transfer:submitted", { ...SUBMITTED, id: 7 });

    const r = get(rows).get(7)!;
    expect(r.state, "submitted 覆写 state 会把终态行钉死在 Queued：isTerminal 为假 → 关闭确认框此后每次必弹且计数永不归零").toBe("Done");
    expect(r.done).toBe(100);
    expect(r.total).toBe(100);
    // 描述性字段只有 submitted 带（TransferEvent 里没有 direction/local/remote），必须补上
    expect(r.direction).toBe("up");
    expect(r.local).toBe("C:/a.bin");
    expect(r.remote).toBe("/srv/a.bin");
    expect(activeTransfers()).toEqual({ queued: 0, running: 0 });
  });

  it("乱序不影响正常顺序：submitted 在前时 progress 仍以自己为准", async () => {
    await boot();
    emit("transfer:submitted", { ...SUBMITTED, id: 8 });
    emit("transfer:progress", { id: 8, sessionId: "s1", bytes_done: 100, bytes_total: 100, state: "Done" });
    expect(get(rows).get(8)!.state).toBe("Done");
    expect(get(rows).get(8)!.direction).toBe("up");
  });

  // 载荷损坏（旧后端 / 事件重放）不得把整条监听器打崩：`Object.keys(undefined)` 会抛，
  // 异常从 rows.update 里穿出去后这条事件就整个丢了，且后续事件的处理与否取决于宿主。
  it("progress 缺 state 时不抛，按已有状态兜底", async () => {
    await boot();
    emit("transfer:submitted", { ...SUBMITTED, id: 9 });
    emit("transfer:progress", { id: 9, sessionId: "s1", bytes_done: 5, bytes_total: 100, state: "Running" });
    expect(() =>
      emit("transfer:progress", { id: 9, sessionId: "s1", bytes_done: 6, bytes_total: 100 }),
    ).not.toThrow();
    expect(get(rows).get(9)!.state).toBe("Running");
    expect(get(rows).get(9)!.done).toBe(6);
  });

  // 与 progress/submitted 的 upsert 相反，verify 结果**必须**在行不存在时丢弃：
  // `transfer_verified` 只跟在 Done 之后发，而 Done 那条 progress 已经会立行；
  // 此刻查不到行只有一个成因——该行已被 trimTerminalRows 按上限裁掉。
  // 若这里也 upsert，就会把裁掉的历史行复活成一条 state 为空的幽灵，
  // 而空 state 不是终态 → 关闭门控把它算成「排队中」，永不归零。
  // progress 处理器原先是 `{ ...r, 覆写几个字段 }`，改成逐字段列举后，
  // 任何忘了列的字段都会在下一条 progress 到达时被静默抹掉，且没有编译期信号
  //（Row 的 verify/attempt 都是可选字段）。这两条把「不该动的字段别动」钉死。
  it("progress 不重置 startedAt——否则速率/ETA 按最近一次事件间隔算，恒显天文数字", async () => {
    await boot();
    emit("transfer:submitted", SUBMITTED);
    const t0 = get(rows).get(1)!.startedAt;
    await new Promise((r) => setTimeout(r, 5));
    emit("transfer:progress", { id: 1, sessionId: "s1", bytes_done: 10, bytes_total: 100, state: "Running" });
    expect(get(rows).get(1)!.startedAt).toBe(t0);
  });

  // 变异 V-M7 首轮幸存：把 `sessionId: p.sessionId ?? r?.sessionId ?? ""` 收窄成
  // `p.sessionId`，12 条测试全绿——原有用例要么带着 sessionId 发 progress，要么只查全局合计。
  // 该兜底守的是**按会话**的关闭门控：归属被抹成 undefined 后，`activeTransfers(sid)` 过滤不到，
  // 用户关标签时不会被拦，在途传输被静默掐断。
  it("progress 缺 sessionId 时保留行原有归属，标签级关闭门控不漏计", async () => {
    await boot();
    emit("transfer:submitted", SUBMITTED); // sessionId: "s1"
    emit("transfer:progress", { id: 1, bytes_done: 10, bytes_total: 100, state: "Running" });
    expect(get(rows).get(1)!.sessionId).toBe("s1");
    expect(activeTransfers("s1")).toEqual({ queued: 0, running: 1 });
  });

  // 变异 V-M8 首轮幸存：submitted 里把 `startedAt: prior?.startedAt ?? Date.now()`
  // 收窄成 `Date.now()`。与 V-M5 同类——只是发生在乱序那一侧，原有用例都没走到。
  it("submitted 后到时不重置 startedAt，速率仍从首个进度事件起算", async () => {
    await boot();
    emit("transfer:progress", { id: 7, sessionId: "s1", bytes_done: 40, bytes_total: 100, state: "Running" });
    const t0 = get(rows).get(7)!.startedAt;
    await new Promise((r) => setTimeout(r, 5));
    emit("transfer:submitted", { ...SUBMITTED, id: 7 });
    expect(get(rows).get(7)!.startedAt).toBe(t0);
  });

  it("progress 不抹掉已落定的 verify 徽标", async () => {
    await boot();
    emit("transfer:submitted", SUBMITTED);
    emit("transfer:progress", { id: 1, sessionId: "s1", bytes_done: 100, bytes_total: 100, state: "Done" });
    emit("transfer_verified", { id: 1, sessionId: "s1", outcome: "sha256_match" });
    // 终态之后仍可能收到重放/迟到的同 id 事件（引擎重试路径、事件补发）
    emit("transfer:progress", { id: 1, sessionId: "s1", bytes_done: 100, bytes_total: 100, state: "Done" });
    expect(get(rows).get(1)!.verify).toBe("sha256_match");
  });

  it("verify 结果落在已被裁剪的行上时丢弃，不复活幽灵行", async () => {
    await boot();
    emit("transfer_verified", { id: 999, sessionId: "s1", outcome: "sha256_match" });
    expect(get(rows).has(999)).toBe(false);
    expect(activeTransfers()).toEqual({ queued: 0, running: 0 });
  });
});

/**
 * 「全部取消」的选行判据（M4a）。
 *
 * 与关闭门控共用 `isTerminal`：两处各写一份「哪些算在跑」迟早分叉，而分叉的表现是
 * 「全部取消」之后关闭确认框还在弹。
 */
describe("cancellableRows", () => {
  const mk = (id: number, state: string, sessionId = "s1"): Row => ({
    id,
    direction: "up",
    local: `/l/${id}`,
    remote: `/r/${id}`,
    sessionId,
    done: 0,
    total: 100,
    state,
    startedAt: 0,
  });
  const map = (...rs: Row[]) => new Map(rs.map((r) => [r.id, r]));

  it("只挑非终态行", () => {
    const m = map(
      mk(1, "Queued"),
      mk(2, "Running"),
      mk(3, "Done"),
      mk(4, "Failed"),
      mk(5, "Cancelled"),
      mk(6, "Retrying"),
    );
    expect(cancellableRows(m).map((r) => r.id)).toEqual([1, 2, 6]);
  });

  it("Retrying 也算在跑——它占着并发额度，也在关闭门控的计数里", () => {
    expect(cancellableRows(map(mk(9, "Retrying")))).toHaveLength(1);
  });

  it("按 id 升序（= 提交顺序），不随 Map 插入历史变化", () => {
    // 刻意乱序插入
    const m = map(mk(30, "Running"), mk(10, "Queued"), mk(20, "Running"));
    expect(cancellableRows(m).map((r) => r.id)).toEqual([10, 20, 30]);
  });

  it("跨会话的在途行都算进来（抽屉是全局的）", () => {
    const m = map(mk(1, "Running", "s1"), mk(2, "Running", "s2"), mk(3, "Done", "s2"));
    expect(cancellableRows(m).map((r) => r.sessionId)).toEqual(["s1", "s2"]);
  });

  it("全是终态时返回空数组（按钮据此禁用）", () => {
    expect(cancellableRows(map(mk(1, "Done"), mk(2, "Cancelled")))).toEqual([]);
    expect(cancellableRows(new Map())).toEqual([]);
  });

  it("与 isTerminal 严格一致（不得各自维护一份状态名单）", () => {
    for (const s of ["Queued", "Running", "Retrying", "Done", "Failed", "Cancelled", "未知态"]) {
      const picked = cancellableRows(map(mk(1, s))).length === 1;
      expect(picked).toBe(!isTerminal(s));
    }
  });
});
