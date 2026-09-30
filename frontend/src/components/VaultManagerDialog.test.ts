import { describe, it, expect, beforeEach, vi } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/svelte";

/**
 * 保险库管理对话框（审计2 #27/#28 的 UI 一半，发布门槛第 5 条）。
 *
 * 这里钉的不是「按钮长什么样」，而是四条在此之前**用户根本到不了**的通路：
 * 删除记录、设置/轮换应用密码、导出备份、从备份恢复。`Store::delete` 与
 * `Store::change_passphrase` 在 core 里早就写好了，只是从未接到 IPC 上——
 * 「实现了但到不了」与「没实现」对用户是同一件事。
 *
 * 其中最要紧的一条是**恢复必须在锁定态可用**：需要恢复的时刻，恰恰就是库打不开的时刻。
 */

const calls: { cmd: string; args: any }[] = [];
let records: any[] = [];
let backups: any[] = [];
let hasPass = false;
let displaced: string | null = null;
const errors: Record<string, string> = {};

vi.mock("../lib/ipc", () => ({
  invoke: async (cmd: string, args?: any) => {
    calls.push({ cmd, args });
    if (errors[cmd]) throw errors[cmd];
    switch (cmd) {
      case "vault_list_secrets": return records;
      case "vault_list_backups": return backups;
      case "vault_backup_dir": return "C:\\data\\vault-backups";
      case "vault_has_passphrase": return hasPass;
      case "vault_create_backup": return "vault-backup-1760000000.json";
      case "vault_restore_backup": return displaced;
      default: return undefined;
    }
  },
}));

import VaultManagerDialog from "./VaultManagerDialog.svelte";
import SRC from "./VaultManagerDialog.svelte?raw";

const REC_PW = { id: 1, kind: "password", label: "prod-pw" };
const REC_KEY = { id: 2, kind: "private_key", label: "prod-key" };
const BK = { name: "vault-backup-1750000000.json", bytes: 2048, modified_unix: 1750000000 };

const cmds = () => calls.map((c) => c.cmd);
const argsOf = (cmd: string) => calls.find((c) => c.cmd === cmd)?.args;

/** 打开并等首轮取数落地（都是异步 invoke，不等就会对着空列表断言出恒真的绿）。 */
async function open(props: { locked?: boolean; onChanged?(): void } = {}) {
  const view = render(VaultManagerDialog, { props: { open: true, locked: false, ...props } });
  await waitFor(() => expect(cmds()).toContain("vault_list_backups"));
  if (props.locked !== true) await waitFor(() => expect(cmds()).toContain("vault_has_passphrase"));
  return view;
}

describe("VaultManagerDialog", () => {
  beforeEach(() => {
    calls.length = 0;
    records = [REC_PW, REC_KEY];
    backups = [BK];
    hasPass = true;
    displaced = "vault.json.replaced-1760000000";
    for (const k of Object.keys(errors)) delete errors[k];
  });

  // ── 锁定态 ────────────────────────────────────────────────────────────────
  it("锁定态下「恢复备份」仍然可用：需要恢复的时刻正是库打不开的时刻", async () => {
    await open({ locked: true });
    // 记录与改密两栏明说要先解锁，但不能因此把备份/恢复一起关掉
    expect(screen.getByTestId("vm-records-locked").textContent).toContain("解锁");
    expect(screen.getByTestId(`vm-backup-${BK.name}`)).toBeTruthy();
    expect(
      cmds(),
      "锁定态不该去问需要解锁的那两个命令（必然报错，只会在界面上留一条无意义的红字）",
    ).not.toContain("vault_list_secrets");

    await fireEvent.click(screen.getByTestId(`vm-restore-${BK.name}`));
    await fireEvent.input(screen.getByTestId("vm-restore-pass"), { target: { value: "pw-of-backup" } });
    await fireEvent.click(screen.getByTestId("vm-restore-confirm"));
    await waitFor(() => expect(cmds()).toContain("vault_restore_backup"));
    expect(argsOf("vault_restore_backup")).toEqual({ name: BK.name, passphrase: "pw-of-backup" });
  });

  it("锁定态下「立即导出备份」置灰（导出要读库内容，必然失败）", async () => {
    await open({ locked: true });
    expect((screen.getByTestId("vm-backup-create") as HTMLButtonElement).disabled).toBe(true);
  });

  // ── 删除 ──────────────────────────────────────────────────────────────────
  it("删除要二次确认：点「删除…」本身不发任何 IPC", async () => {
    await open();
    await fireEvent.click(screen.getByTestId(`vm-del-${REC_PW.id}`));
    expect(cmds(), "一键即删——删掉的可能是唯一一份私钥").not.toContain("vault_delete_secret");
    expect(screen.getByTestId(`vm-del-confirm-row-${REC_PW.id}`).textContent).toContain("无法恢复");

    await fireEvent.click(screen.getByTestId(`vm-del-yes-${REC_PW.id}`));
    await waitFor(() => expect(cmds()).toContain("vault_delete_secret"));
    expect(argsOf("vault_delete_secret")).toEqual({ recordId: REC_PW.id });
  });

  it("确认框里点「取消」= 什么都不发生，且回到原来那两个按钮", async () => {
    await open();
    await fireEvent.click(screen.getByTestId(`vm-del-${REC_KEY.id}`));
    await fireEvent.click(screen.getByTestId(`vm-del-no-${REC_KEY.id}`));
    expect(cmds()).not.toContain("vault_delete_secret");
    expect(screen.getByTestId(`vm-del-${REC_KEY.id}`)).toBeTruthy();
  });

  it("删除后重新拉取列表并通知宿主（不然界面上那条已经不存在的记录还挂着）", async () => {
    const onChanged = vi.fn();
    await open({ onChanged });
    await fireEvent.click(screen.getByTestId(`vm-del-${REC_PW.id}`));
    records = [REC_KEY];
    await fireEvent.click(screen.getByTestId(`vm-del-yes-${REC_PW.id}`));
    await waitFor(() => expect(screen.queryByTestId(`vm-record-${REC_PW.id}`)).toBeNull());
    expect(onChanged).toHaveBeenCalled();
  });

  it("删除失败时记录仍在列表里，且给出原因", async () => {
    await open();
    errors["vault_delete_secret"] = "vault 未解锁";
    await fireEvent.click(screen.getByTestId(`vm-del-${REC_PW.id}`));
    await fireEvent.click(screen.getByTestId(`vm-del-yes-${REC_PW.id}`));
    await waitFor(() => expect(screen.getByTestId("vm-err").textContent).toContain("vault 未解锁"));
    expect(screen.getByTestId(`vm-record-${REC_PW.id}`)).toBeTruthy();
  });

  // ── 复制（把 phase1-acceptance 里那条悬空的验收项接上载体）────────────────
  it("复制只把 record_id 交给后端，明文永不进 IPC", async () => {
    await open();
    await fireEvent.click(screen.getByTestId(`vm-copy-${REC_KEY.id}`));
    await waitFor(() => expect(cmds()).toContain("vault_copy_to_clipboard"));
    expect(argsOf("vault_copy_to_clipboard")).toEqual({ recordId: REC_KEY.id });
  });

  it("未知类别原样回显，不冒充成「密码」", async () => {
    records = [{ id: 7, kind: "hardware_token", label: "yubikey" }];
    await open();
    const row = screen.getByTestId("vm-record-7").textContent ?? "";
    expect(row).toContain("hardware_token");
    expect(row).not.toContain("密码");
  });

  // ── 应用密码 ──────────────────────────────────────────────────────────────
  it("已设密码：出「当前密码」栏，旧口令原样下发", async () => {
    hasPass = true;
    await open();
    await fireEvent.input(screen.getByTestId("vm-pass-current"), { target: { value: "old" } });
    await fireEvent.input(screen.getByTestId("vm-pass-new"), { target: { value: "new" } });
    await fireEvent.input(screen.getByTestId("vm-pass-new2"), { target: { value: "new" } });
    await fireEvent.click(screen.getByTestId("vm-pass-submit"));
    await waitFor(() => expect(cmds()).toContain("vault_change_passphrase"));
    expect(argsOf("vault_change_passphrase")).toEqual({ current: "old", newPassphrase: "new" });
  });

  // 审计2 #28：没设应用密码 = 主密钥只在本机 keyring，且导不出可离机恢复的备份。
  // 这一栏是那个决定的**唯一**补救入口，所以它必须在「还没设」的时候就把代价说清楚。
  it("未设密码：不出「当前密码」栏、current 传 null，且当场说明风险", async () => {
    hasPass = false;
    await open();
    expect(screen.queryByTestId("vm-pass-current")).toBeNull();
    expect(screen.getByTestId("vm-pass-state").textContent).toContain("永久无法恢复");
    await fireEvent.input(screen.getByTestId("vm-pass-new"), { target: { value: "first" } });
    await fireEvent.input(screen.getByTestId("vm-pass-new2"), { target: { value: "first" } });
    await fireEvent.click(screen.getByTestId("vm-pass-submit"));
    await waitFor(() => expect(cmds()).toContain("vault_change_passphrase"));
    expect(argsOf("vault_change_passphrase")).toEqual({ current: null, newPassphrase: "first" });
  });

  it("两次新密码不一致 → 拦下，不发 IPC", async () => {
    await open();
    await fireEvent.input(screen.getByTestId("vm-pass-new"), { target: { value: "a" } });
    await fireEvent.input(screen.getByTestId("vm-pass-new2"), { target: { value: "b" } });
    await fireEvent.click(screen.getByTestId("vm-pass-submit"));
    expect(cmds()).not.toContain("vault_change_passphrase");
    expect(screen.getByTestId("vm-err").textContent).toContain("不一致");
  });

  it("新密码为空 → 拦下（空口令等于把应用密码这条路悄悄拆掉）", async () => {
    await open();
    await fireEvent.click(screen.getByTestId("vm-pass-submit"));
    expect(cmds()).not.toContain("vault_change_passphrase");
    expect(screen.getByTestId("vm-err").textContent).toContain("不能为空");
  });

  // ── 备份与恢复 ────────────────────────────────────────────────────────────
  it("导出备份成功 → 回显文件名并刷新列表", async () => {
    await open();
    await fireEvent.click(screen.getByTestId("vm-backup-create"));
    await waitFor(() => expect(screen.getByTestId("vm-ok").textContent).toContain("vault-backup-1760000000.json"));
    expect(cmds().filter((c) => c === "vault_list_backups").length).toBeGreaterThan(1);
  });

  // 后端在没设应用密码时**直接拒绝导出**：备份里的主密钥是用应用密码封装的，
  // 没有它，导出的文件换台机器一样打不开——那种备份比没有更坏，因为它看着像安全网。
  it("导出被后端拒绝 → 原样呈现拒绝理由，不改写成一句「失败」", async () => {
    errors["vault_create_backup"] = "尚未设置应用密码：主密钥只存在于本机系统 keyring 中……";
    await open();
    await fireEvent.click(screen.getByTestId("vm-backup-create"));
    await waitFor(() => expect(screen.getByTestId("vm-err").textContent).toContain("尚未设置应用密码"));
  });

  it("恢复成功 → 告诉用户原有的库留档在哪儿（恢复错了要有退路）", async () => {
    await open();
    await fireEvent.click(screen.getByTestId(`vm-restore-${BK.name}`));
    await fireEvent.input(screen.getByTestId("vm-restore-pass"), { target: { value: "pw" } });
    await fireEvent.click(screen.getByTestId("vm-restore-confirm"));
    await waitFor(() => expect(screen.getByTestId("vm-ok").textContent).toContain("vault.json.replaced-1760000000"));
    expect(screen.queryByTestId("vm-restore-form"), "恢复完成后表单应当收起").toBeNull();
  });

  it("恢复到空目录（没有东西被顶替）→ 不谎报一个并不存在的留档文件", async () => {
    displaced = null;
    await open();
    await fireEvent.click(screen.getByTestId(`vm-restore-${BK.name}`));
    await fireEvent.click(screen.getByTestId("vm-restore-confirm"));
    await waitFor(() => expect(screen.getByTestId("vm-ok").textContent).toContain("已从备份恢复"));
    expect(screen.getByTestId("vm-ok").textContent).not.toContain("留档");
  });

  it("恢复失败 → 出原因，表单留在原地让用户改口令重试", async () => {
    errors["vault_restore_backup"] = "口令错误或备份损坏";
    await open();
    await fireEvent.click(screen.getByTestId(`vm-restore-${BK.name}`));
    await fireEvent.input(screen.getByTestId("vm-restore-pass"), { target: { value: "wrong" } });
    await fireEvent.click(screen.getByTestId("vm-restore-confirm"));
    await waitFor(() => expect(screen.getByTestId("vm-err").textContent).toContain("口令错误或备份损坏"));
    expect(screen.getByTestId("vm-restore-form")).toBeTruthy();
  });

  it("恢复表单点「取消」→ 收起且不发 IPC", async () => {
    await open();
    await fireEvent.click(screen.getByTestId(`vm-restore-${BK.name}`));
    await fireEvent.click(screen.getByTestId("vm-restore-cancel"));
    expect(screen.queryByTestId("vm-restore-form")).toBeNull();
    expect(cmds()).not.toContain("vault_restore_backup");
  });

  it("备份目录路径照实显示（用户得知道去哪儿把备份拷走）", async () => {
    await open();
    expect(screen.getByTestId("vm-backup-dir").textContent).toBe("C:\\data\\vault-backups");
  });

  // ── 明文生命周期（与 VaultDialog 同一条纪律，审计2 #30）────────────────────
  // 取证方式同 VaultDialog.test.ts 里那一条：对话框内容在 `{#if open}` 里，关闭即卸载，
  // 组件本身却活到进程结束，「关闭之后那份口令还在不在」没有 DOM 表面可观测；
  // 重新打开时新旧实现都会清，行为断言区分不了。能钉住的只有「重置无条件、在 open 早退之前」。
  it("关闭时也清空口令：重置写在 open 早退之前，无条件执行", () => {
    const start = SRC.indexOf("$effect(() => {");
    expect(start, "找不到 $effect").toBeGreaterThan(-1);
    const code = SRC.slice(start, SRC.indexOf("\n  });", start))
      .split("\n")
      .filter((l) => !l.trim().startsWith("//"))
      .join("\n");
    const reset = code.indexOf("resetTransient()");
    const earlyReturn = code.indexOf("if (!open)");
    expect(reset, "$effect 里找不到 resetTransient()").toBeGreaterThan(-1);
    expect(earlyReturn, "$effect 里找不到 open 早退分支").toBeGreaterThan(-1);
    expect(reset, "口令重置被 open 早退挡在后面 → 关闭后口令仍留在组件状态里").toBeLessThan(earlyReturn);
    // 三个承载明文的字段都必须在 resetTransient 里
    const fn = SRC.slice(SRC.indexOf("function resetTransient()"));
    for (const name of ["curPass", "newPass", "newPass2", "restorePass"]) {
      expect(fn.slice(0, fn.indexOf("\n  }")), `${name} 未被清`).toContain(`${name} = ""`);
    }
  });
});
