import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/svelte";
import fs from "node:fs";
import path from "node:path";

const calls: { cmd: string; args: any }[] = [];
/** 按命令名定制应答/抛错（缺省一律 resolve undefined，既有用例不受影响）。 */
const handlers: Record<string, (args: any) => unknown> = {};
vi.mock("../lib/ipc", () => ({
  invoke: async (cmd: string, args?: any) => {
    calls.push({ cmd, args });
    const h = handlers[cmd];
    return h ? await h(args) : undefined;
  },
}));

import VaultDialog from "./VaultDialog.svelte";
// 源文本守卫，见文末「关闭时也清」一条对取证方式的说明。
import VAULT_DIALOG_SRC from "./VaultDialog.svelte?raw";

describe("VaultDialog", () => {
  beforeEach(() => {
    calls.length = 0;
  });

  // 这条钉的是一个把用户永久锁在门外的缺陷：首次设置里「应用密码」是**可选**的
  // （留空 = 主密钥仅存系统 keyring，且这是推荐默认），可解锁框此前写着
  // `if (!pass1) { fail("请输入应用密码"); return; }`。按默认配置建库的用户此后
  // 永远解锁不了自己的 Vault，提示语还在催他输入一个从未设过的密码。
  // 后端 `vault_unlock(None)` 在库文件已存在时正是 `unlock_with_keyring`——这条路必须留口。
  it("unlock 模式留空密码 → 走 keyring 路径（passphrase: null），不报错", async () => {
    render(VaultDialog, { props: { open: true, mode: "unlock" } });
    await fireEvent.click(screen.getByTestId("vault-submit"));
    expect(calls).toEqual([{ cmd: "vault_unlock", args: { passphrase: null } }]);
    expect(screen.queryByTestId("vault-err")).toBeNull();
  });

  it("unlock 模式填了密码 → 原样下发口令", async () => {
    render(VaultDialog, { props: { open: true, mode: "unlock" } });
    await fireEvent.input(screen.getByTestId("vault-pass"), { target: { value: "pw" } });
    await fireEvent.click(screen.getByTestId("vault-submit"));
    expect(calls).toEqual([{ cmd: "vault_unlock", args: { passphrase: "pw" } }]);
  });

  it("setup 模式留空 + 勾选知情 → vault_init 收到 null（仅 keyring）", async () => {
    render(VaultDialog, { props: { open: true, mode: "setup" } });
    await fireEvent.click(screen.getByTestId("vault-ack"));
    await fireEvent.click(screen.getByTestId("vault-submit"));
    expect(calls).toEqual([{ cmd: "vault_init", args: { passphrase: null } }]);
  });

  // 审计2 #28：「应用密码（可选）」这四个字掩盖了一个不可逆的选择——留空建成的库，
  // 主密钥只存在于本机 keyring，凭据管理器一被清理就是全部凭据永久不可恢复，
  // 且此后连一份可离机恢复的备份都导不出来（后端 export_backup 会直接拒绝）。
  // 代价必须由用户按一下才生效，而不是默认替他承担。
  it("setup 模式留空但未勾选知情 → 拦下，不发任何 IPC", async () => {
    render(VaultDialog, { props: { open: true, mode: "setup" } });
    expect(screen.getByTestId("vault-keyring-only-warning").textContent).toContain("永久无法恢复");
    await fireEvent.click(screen.getByTestId("vault-submit"));
    expect(calls).toEqual([]);
    expect(screen.getByTestId("vault-err").textContent).toContain("勾选");
  });

  it("setup 模式设了应用密码 → 警告与勾选框消失，不再要求确认", async () => {
    render(VaultDialog, { props: { open: true, mode: "setup" } });
    await fireEvent.input(screen.getByTestId("vault-pass1"), { target: { value: "pw" } });
    await fireEvent.input(screen.getByTestId("vault-pass2"), { target: { value: "pw" } });
    expect(screen.queryByTestId("vault-keyring-only-warning")).toBeNull();
    await fireEvent.click(screen.getByTestId("vault-submit"));
    expect(calls).toEqual([{ cmd: "vault_init", args: { passphrase: "pw" } }]);
  });

  it("setup 模式两次输入不一致 → 拦下，不发任何 IPC", async () => {
    render(VaultDialog, { props: { open: true, mode: "setup" } });
    await fireEvent.input(screen.getByTestId("vault-pass1"), { target: { value: "a" } });
    await fireEvent.input(screen.getByTestId("vault-pass2"), { target: { value: "b" } });
    await fireEvent.click(screen.getByTestId("vault-submit"));
    expect(calls).toEqual([]);
    expect(screen.getByTestId("vault-err").textContent).toContain("不一致");
  });

  // 只在「再次输入」里敲了密码，第一个框空着——旧判据 `pass1 && pass1 !== pass2` 在
  // pass1 为空时短路，于是静默按 `passphrase: null` 建库：用户以为自己设了应用密码，
  // 拿到的却是一份只有本机 keyring 能开、且导不出备份的库。这比报错严重得多。
  it("setup 模式只填了「再次输入」→ 判不一致，绝不静默建成仅-keyring 库", async () => {
    render(VaultDialog, { props: { open: true, mode: "setup" } });
    await fireEvent.input(screen.getByTestId("vault-pass2"), { target: { value: "pw" } });
    await fireEvent.click(screen.getByTestId("vault-submit"));
    expect(calls).toEqual([]);
    expect(screen.getByTestId("vault-err").textContent).toContain("不一致");
  });

  // 审计2 #30：关闭对话框后口令仍留在组件状态里。
  //
  // 取证方式说明（这条**只能**做源文本守卫，不是偷懒）：对话框内容在 `{#if open}` 里，
  // 关闭即卸载，组件本身却挂在 App 顶层、活到进程结束。「关闭之后那份状态还在不在」
  // 因此没有任何 DOM 表面可观测——重新打开时新旧实现都会清（旧实现正是在 open 时清的），
  // 行为断言无法区分二者。能钉住的只有「清除是无条件的、发生在 open 早退之前」这件事实。
  //
  // 同时说清这条修复的边界：JS 字符串不可变、无法擦除，这里做到的是**尽早丢掉引用**，
  // 不是清零；真正的清零在 Rust 侧（vault_cmd 各命令入口的 Zeroizing）。
  it("关闭时也清空口令：重置写在 open 早退之前，无条件执行", () => {
    const start = VAULT_DIALOG_SRC.indexOf("$effect(() => {");
    expect(start, "找不到 $effect").toBeGreaterThan(-1);
    const end = VAULT_DIALOG_SRC.indexOf("\n  });", start);
    // 整行注释先剔掉：上面那段注释本身就引用了 `if (!open) return;` 这段旧代码，
    // 不剔就会扫到注释里的它，把守卫扫成恒假。
    const code = VAULT_DIALOG_SRC.slice(start, end)
      .split("\n")
      .filter((l) => !l.trim().startsWith("//"))
      .join("\n");

    const reset = code.indexOf('pass1 = ""');
    const earlyReturn = code.indexOf("if (!open");
    expect(reset, "$effect 里找不到口令重置").toBeGreaterThan(-1);
    expect(earlyReturn, "$effect 里找不到 open 早退分支").toBeGreaterThan(-1);
    expect(reset, "口令重置被 open 早退挡在后面 → 关闭后口令仍留在组件状态里").toBeLessThan(
      earlyReturn,
    );
    for (const name of ["pass2", "err", "ack"]) {
      expect(code.slice(0, earlyReturn), `${name} 未被一并重置`).toContain(`${name} = `);
    }
  });
});

/* ------------------------------------------------------------------------------
 * 解锁框的口径（2026-09-01 对抗审计 + 用户裁定「保持现状」后）
 *
 * 事实：主密钥在系统凭据库里，keyring 解锁路径**不校验应用密码**
 * （crates/vault/src/store.rs 的 unlock_with_keyring 不读 passphrase_phc），
 * 所以设过密码的库留空点「解锁」照样开得了。用户裁定保留这个行为——
 * 换机器/备份恢复后还开得了库靠的正是它。
 *
 * 既然保留，界面就必须**把这件事说出来**：让用户以为「设了密码就安全」
 * 比缺口本身更危险，他会据此把更敏感的东西放进来。
 * ---------------------------------------------------------------------------- */
describe("解锁框如实陈述应用密码的强度", () => {
  it("不把应用密码说成硬闸，并点明留空也能开", async () => {
    render(VaultDialog, { props: { open: true, mode: "unlock", onClose: () => {}, onUnlocked: () => {} } });
    const text = document.body.textContent ?? "";
    expect(text).toContain("不能阻止已经能操作这台电脑的人");
    expect(
      text,
      "必须点明「留空也能打开」——含糊其辞会让用户高估这道锁",
    ).toContain("留空使用本机系统凭据库解锁");
  });

  it("明确说明应用密码和系统凭据库两种解锁方式", async () => {
    render(VaultDialog, { props: { open: true, mode: "unlock", onClose: () => {}, onUnlocked: () => {} } });
    expect(document.body.textContent ?? "").toContain("可输入应用密码解锁");
  });
});

/* 路线图 4c「keyring 条目缺失时解锁框要说真话」（2026-09-02）。
 * 此前无论什么原因都是「解锁失败（密码错误或 keyring/文件异常）」——对「没设应用口令 + keyring
 * 条目已不在」的库，用户唯一被引导去做的事（再试空口令）一万次也不会成功。 */
describe("VaultDialog：解锁失败按真实原因分岔文案", () => {
  const KEYRING_MISSING_ERR = "keyring entry missing: this vault's master key is not in the OS credential store";
  const resetHandlers = () => { for (const k of Object.keys(handlers)) delete handlers[k]; };
  beforeEach(() => { calls.length = 0; resetHandlers(); });
  afterEach(resetHandlers);

  async function failUnlock(fileHasPassphrase: () => unknown) {
    handlers.vault_unlock = async () => { throw KEYRING_MISSING_ERR; };
    handlers.vault_file_has_passphrase = fileHasPassphrase;
    render(VaultDialog, { props: { open: true, mode: "unlock" } });
    await fireEvent.click(screen.getByTestId("vault-submit"));
    await waitFor(() => expect(screen.queryByTestId("vault-err")).not.toBeNull());
    return screen.getByTestId("vault-err").textContent ?? "";
  }

  it("没设应用密码 + keyring 条目不在 → 直说不可恢复并指向备份，不再劝「再试」", async () => {
    const t = await failUnlock(async () => false);
    expect(t).toContain("无法恢复");
    expect(t).toContain("备份");
    expect(t).not.toContain("密码错误");
    expect(calls.map((c) => c.cmd), "分岔依据必须真的问过库文件头，不是猜").toContain("vault_file_has_passphrase");
  });

  it("设了应用密码 + keyring 条目不在 → 让用户输入应用密码，不说「不可恢复」", async () => {
    const t = await failUnlock(async () => true);
    expect(t).toContain("请输入应用密码");
    expect(t).not.toContain("无法恢复");
  });

  it("库文件头读不出 → 不下结论：两种可能都摆出来，并带原始错误", async () => {
    const t = await failUnlock(async () => { throw "读文件失败"; });
    expect(t).toContain("从备份恢复");
    expect(t).toContain("设过应用密码");
    expect(t).toContain(KEYRING_MISSING_ERR);
  });

  it("不是 keyring 缺失的失败 → 沿用通用文案，且不多问一次库文件头", async () => {
    handlers.vault_unlock = async () => { throw "integrity check failed"; };
    render(VaultDialog, { props: { open: true, mode: "unlock" } });
    await fireEvent.click(screen.getByTestId("vault-submit"));
    await waitFor(() => expect(screen.queryByTestId("vault-err")).not.toBeNull());
    expect(screen.getByTestId("vault-err").textContent).toContain("integrity check failed");
    expect(calls.map((c) => c.cmd)).not.toContain("vault_file_has_passphrase");
  });

  it("判据子串与 Rust 侧 Error::KeyringMissing 的 Display 文案一致（跨语言契约）", () => {
    const rs = fs.readFileSync(path.resolve(process.cwd(), "../crates/vault/src/error.rs"), "utf8");
    const m = /#\[error\("(keyring entry missing[^"]*)"\)\]\s*KeyringMissing/.exec(rs);
    expect(m, "error.rs 里找不到 KeyringMissing 的 #[error] 文案（或前缀改了）").not.toBeNull();
    expect(VAULT_DIALOG_SRC).toContain('const KEYRING_MISSING = "keyring entry missing"');
  });
});
