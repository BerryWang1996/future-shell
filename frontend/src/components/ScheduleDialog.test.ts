/**
 * 计划任务面板测试（M4a）。
 *
 * 重点钉四件：
 * ① 产品边界（只在有会话时执行）必须**常驻显示**——它是用户决策的依据，不是免责声明；
 * ② cron 校验以后端为准，且**非法时保存被禁**（前端不得自己放行）；
 * ③ 「一年内不触发」的合法表达式必须给出说明，不能只显示一个空列表；
 * ④ 删除要确认（连执行记录一起删）。
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/svelte";
import ScheduleDialog from "./ScheduleDialog.svelte";
import type { ScheduledTask } from "../lib/schedule";
import type { Profile } from "../lib/types";

const invokeMock = vi.hoisted(() =>
  vi.fn(async (_cmd: string, _args?: Record<string, unknown>): Promise<unknown> => undefined),
);
vi.mock("../lib/ipc", () => ({ invoke: invokeMock }));
const toastMock = vi.hoisted(() => ({
  info: vi.fn(),
  warn: vi.fn(),
  error: vi.fn(),
  push: vi.fn(),
  dismiss: vi.fn(),
  subscribe: vi.fn(() => () => {}),
}));
vi.mock("../lib/toast", () => ({ toast: toastMock }));

const PROFILES: Profile[] = [
  { id: "11111111-1111-1111-1111-111111111111", name: "生产 web", group_path: null,
    host: "web.example.com", port: 22, username: "root" },
  { id: "22222222-2222-2222-2222-222222222222", name: "备份机", group_path: null,
    host: "bak.example.com", port: 22, username: "admin" },
] as unknown as Profile[];

const T1: ScheduledTask = {
  id: 1, name: "每夜备份", cron: "30 3 * * *", command: "/opt/backup.sh",
  profile_id: PROFILES[0].id, enabled: true, catchup: "skip",
  tz_offset_minutes: 480, tz_follows_dst: false, last_fire_minute: 29_788_383, created_at: 1_787_270_400,
};

const VALID_PREVIEW = { valid: true, error: null, nextFires: [1_787_400_000, 1_787_486_400, 1_787_572_800], note: null };
const NEVER_PREVIEW = { valid: true, error: null, nextFires: [], note: "该表达式在未来一年内不会触发（例如 2 月 30 日这种不存在的日期）——请检查" };
const BAD_PREVIEW = { valid: false, error: "cron 的「分」字段越界：60（允许 0..=59）", nextFires: [], note: null };

function mockInvoke(map: Record<string, unknown> = {}) {
  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd in map) return map[cmd];
    return undefined;
  });
}

beforeEach(() => {
  cleanup();
  invokeMock.mockReset();
  Object.values(toastMock).forEach((f) => f.mockReset?.());
});

describe("ScheduleDialog", () => {
  it("产品边界（只在有活动会话时执行）常驻显示", async () => {
    mockInvoke({ schedule_list: [] });
    render(ScheduleDialog, { props: { open: true, profiles: PROFILES, onClose: vi.fn() } });
    const scope = await screen.findByTestId("sched-scope");
    expect(scope.textContent).toContain("有活动会话");
    expect(scope.textContent).toContain("自动拨号");
  });

  it("空表显示空态；有任务则列出其计划与目标", async () => {
    mockInvoke({ schedule_list: [] });
    render(ScheduleDialog, { props: { open: true, profiles: PROFILES, onClose: vi.fn() } });
    await screen.findByTestId("sched-empty");
  });

  it("任务行显示 cron 原文 + 偏移、目标连接名、上次触发时间", async () => {
    mockInvoke({ schedule_list: [T1] });
    render(ScheduleDialog, { props: { open: true, profiles: PROFILES, onClose: vi.fn() } });
    const t = await screen.findByTestId("sched-table");
    expect(t.textContent).toContain("30 3 * * *");
    expect(t.textContent).toContain("UTC+08:00");
    expect(t.textContent).toContain("生产 web");
    // last_fire_minute 29_788_383 = 2026-08-21 09:03 UTC → UTC+8 17:03
    expect(t.textContent).toContain("08-21 17:03");
  });

  it("无连接时「新建任务」禁用（没目标可绑，建了也只会一直 skipped）", async () => {
    mockInvoke({ schedule_list: [] });
    render(ScheduleDialog, { props: { open: true, profiles: [], onClose: vi.fn() } });
    const btn = await screen.findByTestId("sched-new");
    expect((btn as HTMLButtonElement).disabled).toBe(true);
  });

  it("cron 非法时显示后端原因，保存被禁", async () => {
    mockInvoke({
      schedule_list: [],
      schedule_preview_cron: BAD_PREVIEW,
    });
    render(ScheduleDialog, { props: { open: true, profiles: PROFILES, onClose: vi.fn() } });
    await fireEvent.click(await screen.findByTestId("sched-new"));
    // 其余条件全填满：名字、命令、目标连接都有——让「保存被禁」只能归因于 cron 非法
    await fireEvent.input(screen.getByTestId("sched-f-name"), { target: { value: "x" } });
    await fireEvent.input(screen.getByTestId("sched-f-command"), { target: { value: "echo" } });
    await screen.findByTestId("sched-cron-error");
    expect(screen.getByTestId("sched-cron-error").textContent).toContain("越界");
    expect((screen.getByTestId("sched-save") as HTMLButtonElement).disabled).toBe(true);
    // 没有预览时也存不了（校验中不算通过）
    expect(screen.queryByTestId("sched-next-fires")).toBeNull();
  });

  it("合法 cron 显示接下来三次触发", async () => {
    mockInvoke({
      schedule_list: [],
      schedule_preview_cron: VALID_PREVIEW,
    });
    render(ScheduleDialog, { props: { open: true, profiles: PROFILES, onClose: vi.fn() } });
    await fireEvent.click(await screen.findByTestId("sched-new"));
    const next = await screen.findByTestId("sched-next-fires");
    // 三个时刻都在
    expect(next.textContent).toContain("、");
    expect((next.textContent?.match(/、/g) ?? []).length, "三次触发之间应有两个分隔符").toBe(2);
  });

  it("「一年内不触发」的合法 cron 给出说明——空列表是有信息的", async () => {
    mockInvoke({
      schedule_list: [],
      schedule_preview_cron: NEVER_PREVIEW,
    });
    render(ScheduleDialog, { props: { open: true, profiles: PROFILES, onClose: vi.fn() } });
    await fireEvent.click(await screen.findByTestId("sched-new"));
    const note = await screen.findByTestId("sched-cron-note");
    expect(note.textContent).toContain("一年内");
    expect(screen.queryByTestId("sched-next-fires")).toBeNull();
  });

  it("保存把表单字段发给后端（camelCase；时区偏移原样传）", async () => {
    mockInvoke({
      schedule_list: [],
      schedule_preview_cron: VALID_PREVIEW,
    });
    render(ScheduleDialog, { props: { open: true, profiles: PROFILES, onClose: vi.fn() } });
    await fireEvent.click(await screen.findByTestId("sched-new"));
    await fireEvent.input(screen.getByTestId("sched-f-name"), { target: { value: "清理日志" } });
    await fireEvent.input(screen.getByTestId("sched-f-command"), { target: { value: "logrotate -f" } });
    await fireEvent.input(screen.getByTestId("sched-f-cron"), { target: { value: "0 4 * * *" } });
    await fireEvent.click(screen.getByTestId("sched-save"));
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("schedule_create", {
        form: expect.objectContaining({
          name: "清理日志",
          command: "logrotate -f",
          cron: "0 4 * * *",
          profileId: PROFILES[0].id,
          tzOffsetMinutes: expect.any(Number),
        }),
      }),
    );
  });

  it("保存失败把后端原因带出来（不静默）", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "schedule_preview_cron") return VALID_PREVIEW;
      if (cmd === "schedule_create") throw new Error("cron 的「时」字段越界：24（允许 0..=23）");
      if (cmd === "schedule_list") return [];
      return undefined;
    });
    render(ScheduleDialog, { props: { open: true, profiles: PROFILES, onClose: vi.fn() } });
    await fireEvent.click(await screen.findByTestId("sched-new"));
    await fireEvent.input(screen.getByTestId("sched-f-name"), { target: { value: "测试计划" } });
    await fireEvent.input(screen.getByTestId("sched-f-command"), { target: { value: "date" } });
    await waitFor(() => expect((screen.getByTestId("sched-save") as HTMLButtonElement).disabled).toBe(false));
    await fireEvent.click(screen.getByTestId("sched-save"));
    await waitFor(() => expect(toastMock.error).toHaveBeenCalled());
    expect(String(toastMock.error.mock.calls[0][0])).toContain("越界");
    // 表单还开着（保存失败不该把用户输入扔掉）
    expect(screen.getByTestId("sched-form")).toBeTruthy();
  });

  it("勾选「跟随系统时区」后预览用系统当前偏移，手输偏移被禁用", async () => {
    invokeMock.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
      if (cmd === "schedule_preview_cron") {
        return { valid: true, error: null, nextFires: [1_787_400_000], note: null, got: args };
      }
      if (cmd === "schedule_list") return [];
      return undefined;
    });
    render(ScheduleDialog, { props: { open: true, profiles: PROFILES, onClose: vi.fn() } });
    await fireEvent.click(await screen.findByTestId("sched-new"));
    // 手输一个与系统不一致的偏移
    const off = screen.getByTestId("sched-f-offset") as HTMLInputElement;
    off.value = "0";
    off.dispatchEvent(new Event("input", { bubbles: true }));
    await fireEvent.click(screen.getByTestId("sched-f-follow-dst"));
    await waitFor(() => {
      const call = invokeMock.mock.calls.find((c) => c[0] === "schedule_preview_cron");
      expect(call).toBeTruthy();
    });
    // 最后一次预览请求的偏移应是系统当前值，而不是手输的 0（除非本机恰在 UTC）
    const last = invokeMock.mock.calls.filter((c) => c[0] === "schedule_preview_cron").pop();
    const sent = (last?.[1] as { tzOffsetMinutes: number }).tzOffsetMinutes;
    expect(sent, "跟随 DST 时预览必须用系统当前偏移").toBe(-new Date().getTimezoneOffset());
    // 手输框禁用（由系统决定，改了也不生效）
    expect((screen.getByTestId("sched-f-offset") as HTMLInputElement).disabled).toBe(true);
  });

  it("编辑任务回填现有值", async () => {
    mockInvoke({ schedule_list: [T1] });
    render(ScheduleDialog, { props: { open: true, profiles: PROFILES, onClose: vi.fn() } });
    await fireEvent.click(await screen.findByTestId("sched-edit-1"));
    await waitFor(() =>
      expect((screen.getByTestId("sched-f-name") as HTMLInputElement).value).toBe("每夜备份"),
    );
    expect((screen.getByTestId("sched-f-cron") as HTMLInputElement).value).toBe("30 3 * * *");
    expect((screen.getByTestId("sched-f-command") as HTMLInputElement).value).toBe("/opt/backup.sh");
  });

  it("删除先确认，确认文案列出计划与命令（让人核对删对了没有）", async () => {
    mockInvoke({ schedule_list: [T1] });
    render(ScheduleDialog, { props: { open: true, profiles: PROFILES, onClose: vi.fn() } });
    await fireEvent.click(await screen.findByTestId("sched-del-1"));
    const msg = await screen.findByTestId("confirm-msg");
    expect(msg.textContent).toContain("每夜备份");
    expect(msg.textContent).toContain("30 3 * * *");
    expect(msg.textContent).toContain("/opt/backup.sh");
    expect(msg.textContent).toContain("执行记录");
    await fireEvent.click(screen.getByTestId("confirm-ok"));
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("schedule_delete", { id: 1 }));
  });

  it("「立即执行」调 schedule_run_now", async () => {
    mockInvoke({ schedule_list: [T1] });
    render(ScheduleDialog, { props: { open: true, profiles: PROFILES, onClose: vi.fn() } });
    await fireEvent.click(await screen.findByTestId("sched-run-1"));
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("schedule_run_now", { id: 1 }));
  });

  it("执行记录三态分开呈现，skipped 显示原因", async () => {
    mockInvoke({
      schedule_list: [T1],
      schedule_runs: [
        { id: 3, task_id: 1, fired_at: 1_787_302_980, outcome: "skipped", catchup: false, exit_code: null,
          detail: "没有该连接的活动会话——计划任务只在已开启的会话上执行（见「计划任务」说明）" },
        { id: 2, task_id: 1, fired_at: 1_787_216_580, outcome: "failed", catchup: false, exit_code: 1,
          detail: "bash: /opt/backup.sh: No such file or directory" },
        { id: 1, task_id: 1, fired_at: 1_787_130_180, outcome: "ok", catchup: true, exit_code: 0, detail: "" },
      ],
    });
    render(ScheduleDialog, { props: { open: true, profiles: PROFILES, onClose: vi.fn() } });
    await fireEvent.click(await screen.findByTestId("sched-runs-1"));
    const panel = await screen.findByTestId("sched-runs");
    expect(panel.textContent).toContain("未执行");
    expect(panel.textContent).toContain("没有该连接的活动会话");
    expect(panel.textContent).toContain("失败");
    expect(panel.textContent).toContain("No such file or directory");
    expect(panel.textContent).toContain("成功");
    expect(panel.textContent).toContain("补跑");
    expect(panel.textContent).toContain("exit 1");
  });

  it("目标连接已删除时显示占位而不是空", async () => {
    const orphan = { ...T1, profile_id: "99999999-9999-9999-9999-999999999999" };
    mockInvoke({ schedule_list: [orphan] });
    render(ScheduleDialog, { props: { open: true, profiles: PROFILES, onClose: vi.fn() } });
    const t = await screen.findByTestId("sched-table");
    expect(t.textContent).toContain("已删除的连接");
  });

  it("读取失败显示原因", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "schedule_list") throw new Error("database is locked");
      return undefined;
    });
    render(ScheduleDialog, { props: { open: true, profiles: PROFILES, onClose: vi.fn() } });
    const err = await screen.findByTestId("sched-error");
    expect(err.textContent).toContain("database is locked");
  });
});
