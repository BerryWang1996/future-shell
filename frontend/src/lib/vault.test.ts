import { describe, it, expect } from "vitest";
import { resolveVaultToggle } from "./vault";

/** 记录被问到的命令名，并按预设表回答。 */
function fakeInvoker(answers: Record<string, unknown>) {
  const asked: string[] = [];
  const call = async <T,>(cmd: string): Promise<T> => {
    asked.push(cmd);
    if (!(cmd in answers)) throw new Error(`未预设命令 ${cmd}`);
    return answers[cmd] as T;
  };
  return { call, asked };
}

describe("resolveVaultToggle", () => {
  it("已解锁 → lock，且不必再问库文件", async () => {
    const { call, asked } = fakeInvoker({ vault_status: true });
    expect(await resolveVaultToggle(call)).toBe("lock");
    expect(asked).toEqual(["vault_status"]);
  });

  it("未解锁且 vault.json 存在 → unlock", async () => {
    const { call } = fakeInvoker({ vault_status: false, vault_has_file: true });
    expect(await resolveVaultToggle(call)).toBe("unlock");
  });

  // 本条是这个文件存在的理由：全新安装（vault.json 尚未生成）必须走 setup。
  // 历史实现靠 `vault_status` 抛异常来推断首启，而该命令在 Rust 侧是
  // `Ok(state.vault.lock().await.is_some())`，永不返回 Err —— setup 分支从来没被走到过，
  // 新装用户只会看到「解锁 Vault」，输什么密码都失败，凭据功能整条不可达。
  it("未解锁且 vault.json 不存在 → setup（首启建库路径不可省）", async () => {
    const { call, asked } = fakeInvoker({ vault_status: false, vault_has_file: false });
    expect(await resolveVaultToggle(call)).toBe("setup");
    expect(asked).toEqual(["vault_status", "vault_has_file"]);
  });

  it("判定必须实际查询 vault_has_file，不得凭异常推断", async () => {
    const { call, asked } = fakeInvoker({ vault_status: false, vault_has_file: true });
    await resolveVaultToggle(call);
    expect(asked).toContain("vault_has_file");
  });
});
