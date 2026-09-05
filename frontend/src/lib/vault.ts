import { invoke } from "./ipc";

/**
 * Vault 开关（工具栏 / 菜单 `tools.vaultToggle`）的三种去向。
 *
 * 之所以把这个判定从 App.svelte 里提出来单独成文件：它有**三**个分支而不是两个，
 * 而三分支里最容易漏的那一支（首启建库）恰恰没有任何日常路径会走到——开发机上
 * vault.json 早就存在了，漏掉它在本地怎么点都点不出来，只有全新安装的用户会撞上。
 * 提成纯函数后可以直接用假 invoker 把三条分支全钉在单元测试里。
 */
export type VaultToggle = "lock" | "unlock" | "setup";

/** 只需要「命令名 → 结果」的最小 invoke 形状；两个查询命令都不带参数。 */
type Invoker = <T>(cmd: string) => Promise<T>;

/**
 * 判定 Vault 开关该做什么。
 *
 * - 已解锁 → `lock`（把它锁上）
 * - 未解锁且 vault.json 存在 → `unlock`
 * - 未解锁且 vault.json 不存在 → `setup`（首次建库）
 *
 * 第三支必须问 `vault_has_file`，不能靠「`vault_status` 抛异常」来推断：后者的 Rust 签名是
 * `Ok(state.vault.lock().await.is_some())`——锁一把 Mutex 读个 Option，**没有**返回 Err
 * 的路径。历史实现把 setup 挂在 catch 上，等于新装用户永远只能看见「解锁 Vault」，
 * 而任何密码都会被拿去打开一个并不存在的 vault.json，必然失败：整条凭据链在新机器上不可达。
 */
export async function resolveVaultToggle(call: Invoker = invoke as Invoker): Promise<VaultToggle> {
  if (await call<boolean>("vault_status")) return "lock";
  return (await call<boolean>("vault_has_file")) ? "unlock" : "setup";
}
