import { beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, waitFor } from "@testing-library/svelte";
import { get } from "svelte/store";
import { pendingConfirm, resetConfirmGateForTest } from "../lib/confirm-gate";
import MonitorPanel from "./MonitorPanel.svelte";

// 形参必须写出来：进程页签的测试要按命令名分派（`mockImplementation((cmd) => …)`），
// 而零参数的 mock 类型会让那种实现签名对不上（vitest 跑得过，svelte-check 报错）。
const invokeMock = vi.hoisted(() =>
  vi.fn(async (_cmd: string, _args?: Record<string, unknown>): Promise<unknown> => ({})),
);
// 确认闸（M7.2）要读设置里的豁免集，故桩里必须带 settingGet/settingSet——
// 缺了它 confirmAction 会在读设置那一步就 reject，确认框永远挂不上。
vi.mock("../lib/ipc", () => ({
  invoke: invokeMock,
  settingGet: async <T>(_k: string, fb: T) => fb,
  settingSet: async () => {},
  reportFrontendError: () => {},
}));

const SNAP = {
  hostname: "web-01",
  uptime: "up 3 days",
  load_1: 0.15,
  load_5: 0.2,
  load_15: 0.18,
  mem_used_mb: 1234,
  mem_total_mb: 7984,
  disk_used: "12G",
  disk_total: "50G",
};

describe("MonitorPanel（M4a 首增量）", () => {
  beforeEach(() => {
    cleanup();
    invokeMock.mockReset();
    invokeMock.mockResolvedValue(SNAP);
  });

  it("有会话时挂载即轮询 session_monitor 并渲染快照字段", async () => {
    render(MonitorPanel, { props: { sessionId: "s1" } });
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("session_monitor", { sessionId: "s1" }));
    await waitFor(() => expect(document.querySelector('[data-testid="monitor-panel"]')?.textContent).toContain("web-01"));
    const t = document.querySelector('[data-testid="monitor-panel"]')!.textContent!;
    expect(t).toContain("up 3 days");
    expect(t).toContain("0.15 / 0.2 / 0.18");
    expect(t).toContain("1234 / 7984 MB");
    expect(t).toContain("12G / 50G");
  });

  it("无会话时不轮询，显示「无活动会话」", async () => {
    render(MonitorPanel, { props: { sessionId: null } });
    expect(invokeMock).not.toHaveBeenCalled();
    expect(document.querySelector('[data-testid="monitor-panel"]')?.textContent).toContain("无活动会话");
  });

  it("采集失败显示错误，不整屏崩溃", async () => {
    invokeMock.mockRejectedValueOnce("no session");
    render(MonitorPanel, { props: { sessionId: "s1" } });
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("session_monitor", { sessionId: "s1" }));
    await waitFor(() => expect(document.querySelector('[data-testid="monitor-panel"]')?.textContent).toContain("采集失败：no session"));
  });
});

/**
 * 进程页签（M4a：服务器进程管理）。
 *
 * 钉的重点：进程列表**只在该页签可见时**才轮询（`ps -e` 全表要走 exec 通道，
 * 后台常驻轮询是给别人的生产机加负载）；终止必须先确认；失败原因必须原样呈现
 * （「Operation not permitted」与「No such process」对用户是两件不同的事）。
 */
const PROCS = [
  { pid: 1, ppid: 0, user: "root", cpu: 0.0, mem: 0.1, rss_kb: 13245, state: "Ss", command: "/sbin/init" },
  { pid: 1337, ppid: 1, user: "alice", cpu: 12.5, mem: 3.2, rss_kb: 331200, state: "R+", command: "python3 train.py" },
  { pid: 42, ppid: 2, user: "root", cpu: null, mem: null, rss_kb: null, state: "I<", command: "[kworker/0:1H]" },
];

describe("MonitorPanel 进程页签", () => {
  const panel = () => document.querySelector('[data-testid="monitor-panel"]')!;
  const click = async (sel: string) => {
    const el = document.querySelector(sel) as HTMLElement;
    expect(el, `未找到 ${sel}`).toBeTruthy();
    el.click();
    await Promise.resolve();
  };

  beforeEach(() => {
    resetConfirmGateForTest(); // 确认闸是模块级 store，跨用例必须清（M7.2）
    cleanup();
    invokeMock.mockReset();
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "session_monitor") return SNAP;
      if (cmd === "session_processes") return PROCS;
      return undefined;
    });
  });

  it("未切到进程页时不取进程列表（不给生产机白加负载）", async () => {
    render(MonitorPanel, { props: { sessionId: "s1" } });
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("session_monitor", { sessionId: "s1" }));
    expect(invokeMock).not.toHaveBeenCalledWith("session_processes", { sessionId: "s1" });
  });

  it("切到进程页即取列表并渲染各列", async () => {
    render(MonitorPanel, { props: { sessionId: "s1" } });
    await click('[data-testid="mon-tab-procs"]');
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("session_processes", { sessionId: "s1" }));
    await waitFor(() => expect(document.querySelector('[data-testid="proc-table"]')).toBeTruthy());
    const t = panel().textContent!;
    expect(t).toContain("python3 train.py");
    expect(t).toContain("12.5%");
    expect(t).toContain("323.4 MiB"); // 331200 KiB
    // 空值列显示「—」而不是 0.0%
    expect(t).toContain("—");
  });

  it("默认按 CPU 降序——打开就该看见吃 CPU 的那个", async () => {
    render(MonitorPanel, { props: { sessionId: "s1" } });
    await click('[data-testid="mon-tab-procs"]');
    await waitFor(() => expect(document.querySelector('[data-testid="proc-table"]')).toBeTruthy());
    const firstRowPid = document.querySelector('[data-testid="proc-table"] tbody tr td')!.textContent;
    expect(firstRowPid).toBe("1337");
  });

  it("过滤按 PID/用户/命令生效，并更新计数", async () => {
    render(MonitorPanel, { props: { sessionId: "s1" } });
    await click('[data-testid="mon-tab-procs"]');
    await waitFor(() => expect(document.querySelector('[data-testid="proc-table"]')).toBeTruthy());
    const input = document.querySelector('[data-testid="proc-filter"]') as HTMLInputElement;
    input.value = "alice";
    input.dispatchEvent(new Event("input", { bubbles: true }));
    await waitFor(() =>
      expect(document.querySelector('[data-testid="proc-count"]')!.textContent).toBe("1 / 3"),
    );
    expect(panel().textContent).not.toContain("[kworker/0:1H]");
  });

  /* 2026-09-03（M7.2）：确认不再由本组件自己挂 ConfirmDialog，而是走
   * lib/confirm-gate.ts 的统一闸（按类别记忆「以后不再显示」+ 审计照写），
   * 对话框是挂在 App 顶层的单实例。故这几条改为对着 pendingConfirm store 断言。 */
  /** 等确认闸挂上待确认项。 */
  async function pending() {
    await waitFor(() => expect(get(pendingConfirm)).not.toBeNull());
    return get(pendingConfirm)!;
  }

  it("终止先走确认闸，取消则不发命令", async () => {
    render(MonitorPanel, { props: { sessionId: "s1" } });
    await click('[data-testid="mon-tab-procs"]');
    await waitFor(() => expect(document.querySelector('[data-testid="proc-kill-1337"]')).toBeTruthy());
    await click('[data-testid="proc-kill-1337"]');
    (await pending()).answer(false, false);
    // 断言「没发生」必须等到所有排队的异步都跑完：只 flush 一个微任务的话，
    // 即使把闸拆了（confirmAction 的结果被无视）这条也照绿——首版变异 M10 正是这样幸存的。
    await new Promise((r) => setTimeout(r, 0));
    expect(invokeMock).not.toHaveBeenCalledWith("session_kill_process", expect.anything());
  });

  it("确认闸拿到的是命令全文与动作类别（出口标准①/②）", async () => {
    render(MonitorPanel, { props: { sessionId: "s1" } });
    await click('[data-testid="mon-tab-procs"]');
    await waitFor(() => expect(document.querySelector('[data-testid="proc-kill-1337"]')).toBeTruthy());
    await click('[data-testid="proc-kill-1337"]');
    const req = await pending();
    expect(req.kind).toBe("process.kill");
    expect(req.command).toBe("kill -TERM 1337");
    expect(req.sessionId).toBe("s1");
    req.answer(false, false);
  });

  it("确认后发送所选信号，并立即重取列表（信号是异步生效的，不乐观抹行）", async () => {
    render(MonitorPanel, { props: { sessionId: "s1" } });
    await click('[data-testid="mon-tab-procs"]');
    await waitFor(() => expect(document.querySelector('[data-testid="proc-kill-1337"]')).toBeTruthy());
    const before = invokeMock.mock.calls.filter((c) => c[0] === "session_processes").length;
    await click('[data-testid="proc-kill-1337"]');
    (await pending()).answer(true, false);
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("session_kill_process", {
        sessionId: "s1",
        pid: 1337,
        signal: "TERM",
      }),
    );
    await waitFor(() =>
      expect(
        invokeMock.mock.calls.filter((c) => c[0] === "session_processes").length,
      ).toBeGreaterThan(before),
    );
  });

  it("PID 1 的后果说明写出「整台机器或整个容器」，且按危险色渲染", async () => {
    render(MonitorPanel, { props: { sessionId: "s1" } });
    await click('[data-testid="mon-tab-procs"]');
    await waitFor(() => expect(document.querySelector('[data-testid="proc-kill-1"]')).toBeTruthy());
    await click('[data-testid="proc-kill-1"]');
    const req = await pending();
    expect(req.note).toContain("整台机器或整个容器");
    expect(req.danger, "终止 PID 1 必须按危险色渲染").toBe(true);
    req.answer(false, false);
  });

  it("列表取不到时原样呈现后端给的原因，且不清空上一次的列表", async () => {
    render(MonitorPanel, { props: { sessionId: "s1" } });
    await click('[data-testid="mon-tab-procs"]');
    await waitFor(() => expect(document.querySelector('[data-testid="proc-table"]')).toBeTruthy());
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "session_monitor") return SNAP;
      if (cmd === "session_processes") throw new Error("远端进程采集失败（exit 127）：ps: not found");
      return undefined;
    });
    await click('[data-testid="proc-refresh"]');
    await waitFor(() =>
      expect(document.querySelector('[data-testid="proc-error"]')!.textContent).toContain("ps: not found"),
    );
    // 上一次的**行**仍在——只断言表格元素存在是不够的：把 procs 清空后表格外壳
    // 照样渲染（只是零行），断言照过。变异验证抓到过这一点。
    expect(document.querySelectorAll('[data-testid="proc-table"] tbody tr').length).toBe(3);
    expect(panel().textContent).toContain("python3 train.py");
    expect(document.querySelector('[data-testid="proc-count"]')!.textContent).toBe("3 / 3");
  });

  it("点表头切换排序方向", async () => {
    render(MonitorPanel, { props: { sessionId: "s1" } });
    await click('[data-testid="mon-tab-procs"]');
    await waitFor(() => expect(document.querySelector('[data-testid="proc-th-pid"]')).toBeTruthy());
    await click('[data-testid="proc-th-pid"]'); // 切到 pid 升序
    await waitFor(() =>
      expect(document.querySelector('[data-testid="proc-table"] tbody tr td')!.textContent).toBe("1"),
    );
    await click('[data-testid="proc-th-pid"]'); // 再点 → 降序
    await waitFor(() =>
      expect(document.querySelector('[data-testid="proc-table"] tbody tr td')!.textContent).toBe("1337"),
    );
  });
});

/**
 * 监控 × AI 联动（M4b 出口第 2 项）。
 *
 * 判定本身由 `monitor-anomaly.test.ts` 的 25 例覆盖（阈值场景夹逼 + 变异验证）。
 * 这里只钉**接线**：判定的结果有没有真的显示出来、按钮有没有把已拼好的提问交出去。
 */
describe("监控 × AI 联动（M4b）", () => {
  const HOT = {
    ...SNAP,
    cpu_cores: 4,
    load_1: 40, // 每核 10 → critical
    mem_used_mb: 7900,
    mem_total_mb: 7984, // 98.9% → critical
  };

  beforeEach(() => {
    cleanup();
    invokeMock.mockReset();
  });

  it("一切正常时不显示异常摘要，也不显示诊断按钮", async () => {
    // 一条常驻的「一切正常」横条只占地方；一个常驻的「AI 诊断」按钮
    // 会让人在没有问题的时候也去点它。
    invokeMock.mockResolvedValue({ ...SNAP, cpu_cores: 8 });
    const { queryByTestId, findByTestId } = render(MonitorPanel, {
      props: { sessionId: "s1", onDiagnose: () => {} },
    });
    await findByTestId("mon-stat-cpu");
    expect(queryByTestId("mon-anomalies")).toBeNull();
    expect(queryByTestId("mon-diagnose")).toBeNull();
  });

  it("异常指标逐条列出，且对应的格子挂上严重度 class", async () => {
    invokeMock.mockResolvedValue(HOT);
    const { findByTestId, getByTestId, queryByTestId } = render(MonitorPanel, {
      props: { sessionId: "s1", onDiagnose: () => {} },
    });
    await findByTestId("mon-anomalies");
    // 负载与内存异常、CPU 与磁盘正常——**逐项**断言，而不是只数条数：
    // 只数条数的话「四项全标红」也是 2 条以上，看起来一样绿。
    expect(getByTestId("mon-anomaly-load")).toBeTruthy();
    expect(getByTestId("mon-anomaly-mem")).toBeTruthy();
    expect(queryByTestId("mon-anomaly-cpu")).toBeNull();
    expect(queryByTestId("mon-anomaly-disk")).toBeNull();
    // class 挂到了正确的格子上（挂错格子会让用户去看一个其实正常的数）
    expect(getByTestId("mon-stat-load").className).toContain("critical");
    expect(getByTestId("mon-stat-mem").className).toContain("critical");
    expect(getByTestId("mon-stat-cpu").className).not.toContain("critical");
    expect(getByTestId("mon-stat-cpu").className).not.toContain("warn");
    expect(getByTestId("mon-stat-disk").className).not.toContain("critical");
  });

  it("核数采不到时不判负载，且 tooltip 说明了原因", async () => {
    // 这是「采不到 ≠ 正常」在界面上的落点：不判，而且**说出来为什么不判**。
    // 不说的话用户看到一个 40 的负载没有任何标记，会以为程序认为它正常。
    invokeMock.mockResolvedValue({ ...HOT, cpu_cores: null });
    const { findByTestId, queryByTestId, getByTestId } = render(MonitorPanel, {
      props: { sessionId: "s1", onDiagnose: () => {} },
    });
    await findByTestId("mon-anomalies"); // 内存那条还在
    expect(queryByTestId("mon-anomaly-load")).toBeNull();
    expect(getByTestId("mon-stat-load").getAttribute("title")).toContain("核数采不到");
  });

  it("点「AI 诊断」把已拼好的提问交给装配层——含主机名与每条异常原文", async () => {
    invokeMock.mockResolvedValue(HOT);
    const got: string[] = [];
    const { findByTestId, getByTestId } = render(MonitorPanel, {
      props: { sessionId: "s1", onDiagnose: (p: string) => got.push(p) },
    });
    await findByTestId("mon-diagnose");
    getByTestId("mon-diagnose").click();
    await waitFor(() => expect(got).toHaveLength(1));
    expect(got[0]).toContain("web-01");
    expect(got[0]).toContain("只读命令");
    // 两条异常都进了提问，不是只带最严重的那一条——
    // 「负载高 + 内存快满」合起来指向的方向和单看任一条不同。
    expect(got[0]).toContain("负载");
    expect(got[0]).toContain("内存");
  });

  it("没有 onDiagnose 回调时按钮不出现（本组件不知道有没有配模型源）", async () => {
    invokeMock.mockResolvedValue(HOT);
    const { findByTestId, queryByTestId } = render(MonitorPanel, {
      props: { sessionId: "s1" },
    });
    // 异常摘要照常显示——判定与 AI 无关，没配模型源也该看到异常
    await findByTestId("mon-anomalies");
    expect(queryByTestId("mon-diagnose")).toBeNull();
  });
});
