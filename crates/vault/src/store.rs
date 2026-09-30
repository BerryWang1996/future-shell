use crate::crypto;
use crate::Error;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use zeroize::Zeroizing;

const SERVICE: &str = "future-shell";
const USER: &str = "master-key";
/// 「恢复备份」时被顶替掉的那把主密钥的存放处，与 `vault.json.replaced-*` 配对使用。
/// 见 `stash_replaced_master`。
const USER_REPLACED: &str = "master-key-replaced";

/// vault.json 格式版本。每次 `save()` 都是整文件重写：若旧版本程序打开新版本写的库，
/// serde 会静默丢弃它不认识的字段，回写即永久截断（与 `Db::MAX_SCHEMA` 同构的防线）。
/// 故读入时凡 `format > VAULT_FORMAT` 一律拒绝打开，绝不回写。
///
/// v2（S8）：AEAD 的 AAD 口径变更 —— 新增绑定 `kind` 与 `label`，见 `crypto::Aad`。
const VAULT_FORMAT: u32 = 2;

/// 本程序能读的**最低**格式版本。
///
/// S8 配套：format ≤ 1 的密文用旧 AAD（只绑 id+version）封装，本版本一律解不开。
/// 这必须在「打开」这一步就明确告知 —— 否则用户拿到的是 `Error::Integrity`
///（「完整性校验失败」），会被误导去查磁盘、查病毒、查是否被人动过文件，
/// 而真相只是「这库是旧口径写的，需要重新初始化」（与 S18「坏数据 ≠ 查无此条」同理）。
const MIN_VAULT_FORMAT: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SecretKind {
    Password,
    PrivateKey,
    ApiKey,
}

impl SecretKind {
    /// AEAD AAD 中使用的稳定标签（S8）。与 IPC 边界的 kind 串同口径（见下方边界注记）。
    ///
    /// **永不改名**：这些串已被 AAD 认证，改名等价于让既有密文全部解不开。
    pub fn aad_tag(self) -> &'static str {
        match self {
            SecretKind::Password => "password",
            SecretKind::PrivateKey => "private_key",
            SecretKind::ApiKey => "api_key",
        }
    }

    /// [`SecretKind::aad_tag`] 的逆。**认不出即 `None`，绝不回落到某个默认值**（审计2 #20）。
    ///
    /// 与 `aad_tag` 贴在一起，是为了让「两个方向」成为一处而不是两处：IPC 入口原先自己写了一份
    /// `match`，结尾是 `_ => ApiKey`——一个拼错的类别串（前端改名、旧版本客户端、手工构造的
    /// IPC 调用）会被**静默**变成 ApiKey，而类别正是决定这份材料将被拿去干什么的那一栏。
    /// 静默回落把「输入有误」变成了「用途被改写」，这两件事该给用户的反馈完全不同。
    pub fn from_aad_tag(tag: &str) -> Option<SecretKind> {
        match tag {
            "password" => Some(SecretKind::Password),
            "private_key" => Some(SecretKind::PrivateKey),
            "api_key" => Some(SecretKind::ApiKey),
            _ => None,
        }
    }

    /// 三个变体的全集，供跨边界映射的门禁测试遍历（新增变体时它们会自动纳入检查，
    /// 而不是等某个人想起来去改测试）。
    pub const ALL: [SecretKind; 3] = [
        SecretKind::Password,
        SecretKind::PrivateKey,
        SecretKind::ApiKey,
    ];
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordMeta {
    pub id: u64,
    pub kind: SecretKind,
    pub label: String,
}

// IPC 边界注记（i3③）：本结构体 Serialize/Deserialize 仅供 vault.json 落盘，**不经 IPC 直出 core**——
// app 层 Task 20 Step 1 `vault_list_secrets` 经 `RecordMetaDto { id, kind: String, label }` 映射出边界，
// kind 映射为小写串（password/private_key/api_key），且永不回 secret 材料（spec §3.2）。

/// 保险库备份存放的子目录名（相对 data_dir）。
pub const BACKUP_DIR: &str = "vault-backups";

/// 备份目录里的一份候选，供 UI 列表（审计2 #27）。
///
/// 只有文件层面的元信息，**不含任何库内容**：这个列表要在每次开对话框时刷新，
/// 让它去读、去解每一份备份既慢又毫无必要——真正的把关在 `Store::restore_backup`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupEntry {
    pub name: String,
    pub bytes: u64,
    pub modified_unix: u64,
}

#[derive(Serialize, Deserialize)]
struct Record {
    id: u64,
    kind: SecretKind,
    label: String,
    version: u64,
    sealed: crypto::Sealed,
}

#[derive(Serialize, Deserialize)]
struct VaultFile {
    /// 格式版本，见 `VAULT_FORMAT`。`default` 为 0 = 未带版本号的历史文件。
    #[serde(default)]
    format: u32,
    next_id: u64,
    /// 口令**校验串**（Argon2id PHC，自带随机盐）。仅供 `verify_passphrase`，
    /// 绝不参与 KEK 派生 —— 见 `crypto::derive_kek` 的 R117 注记。
    passphrase_phc: Option<String>,
    /// KEK 派生专用随机盐（base64，16 字节）。与 `passphrase_phc` 的内嵌盐相互独立，
    /// 这正是 R117 的修复要点：盐本身无需保密，独立性才是安全属性所在。
    #[serde(default)]
    kdf_salt: Option<String>,
    /// master key 用 `crypto::derive_kek(口令, kdf_salt)` 加密后的 Sealed（passphrase 备用解锁路径）
    master_sealed: Option<(u64, crypto::Sealed)>,
    records: BTreeMap<u64, Record>,
}

impl Default for VaultFile {
    fn default() -> Self {
        Self {
            format: VAULT_FORMAT,
            next_id: 0,
            passphrase_phc: None,
            kdf_salt: None,
            master_sealed: None,
            records: BTreeMap::new(),
        }
    }
}

/// 解析 vault.json 并把住格式版本闸门。三条打开路径共用，确保没有一条能绕过版本检查。
fn parse_vault(bytes: &[u8]) -> Result<VaultFile, Error> {
    let mut file: VaultFile =
        serde_json::from_slice(bytes).map_err(|e| Error::Storage(e.to_string()))?;
    if file.format > VAULT_FORMAT {
        return Err(Error::Storage(format!(
            "vault.json 格式版本 {} 高于本程序支持的 {VAULT_FORMAT}：请升级 future-shell 后再打开（继续打开会在回写时永久丢弃新版字段）",
            file.format
        )));
    }
    if file.format < MIN_VAULT_FORMAT {
        return Err(Error::Storage(format!(
            "vault.json 格式版本 {} 低于本程序支持的最低版本 {MIN_VAULT_FORMAT}：该库的密文以旧 AAD 口径封装（未绑定 kind/label），本版本无法解开，请以应用密码重新初始化保险库",
            file.format
        )));
    }
    // S8 配套（AAD 锚点一致性）：记录**必须**存在 `键 == r.id`。
    //
    // `id` 在 JSON 里是一个独立的字段，和 BTreeMap 的键是两份可以各写各的数据。
    // 二者一旦分叉，「按 3 号取到的其实是 7 号的密文」就成立了 —— 而 `get` 单看
    // 记录对象是察觉不到的（AAD 的四个分量原先全读自那条记录自己，见 `Store::get`）。
    // 这里在**入口**就把错位挡掉：诚实写出来的库永远满足这条，不满足即意味着
    // 文件被程序之外的东西改过，此时唯一安全的动作是拒绝打开，而不是继续解密。
    if let Some((k, r)) = file.records.iter().find(|(k, r)| **k != r.id) {
        return Err(Error::Storage(format!(
            "vault.json 记录错位：键 {k} 下存放的记录自称 id {}（该文件已被篡改或损坏，拒绝打开）",
            r.id
        )));
    }
    // 同族不变式：`put` 先 `next_id += 1` 再用它做 id，故 next_id 恒 ≥ 任何已发出的 id。
    // 反过来若磁盘上出现「最大 id > next_id」，说明有人回退过 next_id（丢更新的典型残迹，
    // 见 `Store::epoch`）—— 放任不管的话下一条新记录会**复用**一个仍在文件里的 id。
    if let Some(max_id) = file.records.keys().next_back() {
        if *max_id > file.next_id {
            return Err(Error::Storage(format!(
                "vault.json 计数器倒退：已存在 id {max_id}，next_id 却是 {}（该文件已被篡改或损坏，拒绝打开）",
                file.next_id
            )));
        }
    }
    // 读入即归一到当前版本：`save()` 是整文件重写，版本号必须随之更新
    file.format = VAULT_FORMAT;
    Ok(file)
}

// ── P1-17：落盘耐久性与并发闸门 ──────────────────────────────────────────────
//
// 保险库是**单文件整体重写**的存储：每次 `save()` 都把整个 vault.json 换掉。
// 这种写法只有配齐三件套才是安全的 —— 缺一件都会把「一次写失败」放大成「库损坏」：
//   ① 临时名唯一（否则并发写同一临时名 = 互相把对方写了一半的内容 rename 上去）；
//   ② sync_all + rename（否则崩溃/掉电后 rename 已生效、内容还在页缓存里 = 拿到空/半截文件）；
//   ③ 失败即回滚内存（否则内存与磁盘分叉，见 `Store::put` 的 P1-17 注记）。
// 本节提供 ①②所需的进程内/跨进程闸门，③ 在各 mutator 里就地实现。

/// 指向同一份 `vault.json` 的所有 `Store` 在**本进程内**共用的守卫。
///
/// 承担两件事：
///   · `save_gate` —— 进程内保存串行化。闸门必须挂在「文件」而不是「Store 实例」上：
///     app 侧 `vault_init` 会在旧 `Store` 还活着时先建出新 `Store`（赋值语句求值序），
///     两个实例指向同一个文件；若锁只是 `Store` 的私有字段，两边各锁各的，
///     等于没锁 —— 后完成的那次 rename 会把先完成的那次整份覆盖掉。
///   · `lock_file` —— 跨进程独占（第二个实例别来动同一个库）。
///
/// 生命周期刻意与进程等长（见 `vault_guard` 的注册表）：一是同一目录反复 open/unlock
/// 属于正常用法（解锁 → 改密 → 再解锁），每次都重新抢锁只会自己卡自己；
/// 二是「本进程正在用这个库」本就该覆盖整个进程存活期。
struct VaultGuard {
    /// 保存闸门**兼磁盘世代计数器**：每成功落盘一次 +1。
    ///
    /// 计数器与闸门必须是同一把锁，不能是并排的两个字段：`save()` 要做的是
    /// 「读世代 → 比对 → 写盘 → 递增」这一整段的原子化，拆成两把锁就留出了
    /// 「两个实例都比对通过、再依次写盘」的缝，等于没比对。
    /// 语义见 `Store::epoch` 与 `Store::save`。
    save_gate: Mutex<u64>,
    /// 独占句柄本身就是锁：Windows 靠 `share_mode(0)`（真·强制锁，进程一死由内核释放），
    /// Unix 靠 `create_new` 抢占的锁文件（建议锁，见 `open_exclusive`）。
    /// 字段只为「持有」而存在，不读内容 —— 故标 `dead_code`。
    #[allow(dead_code)]
    lock_file: File,
    lock_path: PathBuf,
}

impl Drop for VaultGuard {
    fn drop(&mut self) {
        // Windows 无需删文件：句柄一关锁即释放，留下的空文件下次直接复用。
        // Unix 的锁文件存在性本身就是锁，必须删，否则下一次运行要走陈旧检测那条慢路。
        //
        // 注意这条路径**只**由 `release_process_locks()` 触发。守卫的 `Arc` 存活在进程等长的
        // static 注册表里，正常运行期间强引用计数不会归零；而即便归零，Rust 也从不为 static
        // 跑析构。历史上这里被当成「退出时会自动清理」的保证来写，实际是一行谁也走不到的死代码
        // —— 而它一死，Unix 侧就只剩「陈旧锁检测」这一条回收路径，那条路径当时在非 Linux 上
        // 恒判「持有者还活着」，于是 macOS 用户每启动一次就得手工删一次 vault.lock。
        // 现在两条路都修好了：优雅退出走这里，非优雅退出（SIGKILL/掉电）走 `pid_is_alive`。
        #[cfg(unix)]
        let _ = std::fs::remove_file(&self.lock_path);
        #[cfg(not(unix))]
        let _ = &self.lock_path;
    }
}

impl VaultGuard {
    fn acquire(dir: &Path) -> Result<VaultGuard, Error> {
        let lock_path = dir.join("vault.lock");
        let lock_file = Self::open_exclusive(&lock_path)?;
        Ok(VaultGuard {
            save_gate: Mutex::new(0),
            lock_file,
            lock_path,
        })
    }

    /// 在保存闸门下读盘：文件内容与它所对应的**世代号**同刻取得。
    ///
    /// 「同刻」是这个方法存在的全部理由。世代号若在读盘之后另取，就会出现
    /// 「快照是第 N 代、却自称第 N+1 代」的 `Store` —— `save()` 的丢更新比对
    /// 当场失效，而那个比对恰恰是用来防这件事的。反过来先取号后读盘则是
    /// 「快照第 N+1 代、自称第 N 代」，只会多报一次拒绝（fail-closed，可重试），
    /// 但没有理由接受这种退化，闸门本来就在手边。
    fn read_snapshot(&self, path: &Path) -> (std::io::Result<Vec<u8>>, u64) {
        let gate = self
            .save_gate
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        (std::fs::read(path), *gate)
    }

    /// Windows：`share_mode(0)` 打开即取得内核级独占，第二个进程直接吃到共享冲突
    /// （os error 32）。这是真正的强制锁，且进程异常终止时由内核负责释放 —— 没有陈旧锁问题。
    #[cfg(windows)]
    fn open_exclusive(p: &Path) -> Result<File, Error> {
        use std::os::windows::fs::OpenOptionsExt;
        OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false) // 锁文件的内容无关紧要，重要的只有「句柄被独占持有」这件事
            .share_mode(0) // FILE_SHARE_NONE
            .open(p)
            .map_err(|e| {
                Error::Storage(format!(
                    "无法独占数据目录（锁文件 {}）：{e}（另一个 future-shell 实例正在使用同一数据目录；请先关闭它）",
                    p.display()
                ))
            })
    }

    /// Unix：标准库没有 flock/fcntl 绑定，且本 crate 不引入新依赖，故退化为
    /// **尽力而为的建议锁** —— 用 `create_new` 抢一个含 pid 的锁文件。
    ///
    /// 它拦不住蓄意绕过者（谁都能删掉这个文件），只用来挡「用户不小心开了两个实例」。
    /// 进程被 SIGKILL 时锁文件会留下，故必须做陈旧检测：Linux 上查 `/proc/<pid>` 判活，
    /// 判定为死进程即回收锁文件；其余 Unix 无法免依赖判活，一律保守视为存活并报错 ——
    /// 宁可让用户手工删一次锁文件，也不能猜错后让两个实例同时整文件重写同一个库。
    #[cfg(unix)]
    fn open_exclusive(p: &Path) -> Result<File, Error> {
        use std::os::unix::fs::OpenOptionsExt;
        // 最多两轮：第一轮抢锁，撞上陈旧锁就回收后再抢一次。第二轮再撞即认输（有人在竞争）。
        for attempt in 0..2u8 {
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(p)
            {
                Ok(mut f) => {
                    // 写入 pid 供下一次陈旧检测使用；写失败不影响锁本身（文件已抢到）
                    let _ = f.write_all(format!("{}\n", std::process::id()).as_bytes());
                    let _ = f.flush();
                    return Ok(f);
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && attempt == 0 => {
                    let owner = std::fs::read_to_string(p).unwrap_or_default();
                    if !lock_is_stale(&owner, pid_is_alive) {
                        return Err(Error::Storage(format!(
                            "数据目录已被 pid {} 锁定（锁文件 {}）：另一个 future-shell 实例正在使用同一数据目录。\
                             若确认该进程已不存在，删除该锁文件后重试",
                            owner.trim(),
                            p.display()
                        )));
                    }
                    let _ = std::fs::remove_file(p); // 陈旧锁：持有者已死，回收后重抢
                }
                Err(e) => {
                    return Err(Error::Storage(format!(
                        "无法创建数据目录锁 {}：{e}",
                        p.display()
                    )))
                }
            }
        }
        Err(Error::Storage(format!(
            "无法取得数据目录锁 {}：回收陈旧锁后仍被抢占（疑似有并发实例正在启动）",
            p.display()
        )))
    }
}

/// 陈旧锁判定：锁文件内容 + 判活谓词 → 「这把锁能不能回收」。
///
/// 刻意**不带 `#[cfg]`**，也刻意把判活做成参数：真正出过事的就是这段判定，而它原先整段
/// 藏在 `#[cfg(unix)]` 里 —— 开发与 CI 的主力平台（本仓的日常开发环境是 Windows）根本
/// 编不到它，一个「非 Linux 恒判存活」的分支因此可以长期无人发现。抽出来之后，判定逻辑
/// 在任何平台上都能被单测钉住，平台差异只剩下一次 `kill(pid, 0)` 系统调用。
///
/// 内容不可信（空文件、非数字、写 pid 前进程就死了）时返回 `false` = 不回收：
/// 误判「占用」只让用户手工删一次文件，误判「陈旧」则是两个实例同时整文件重写同一个库。
// Windows 走内核级独占句柄，没有锁文件，本判定在 lib target 里用不到；
// 但它必须在所有平台上**编译并被测试**，否则又回到「出事的分支没人编得到」的老路。
#[cfg_attr(not(unix), allow(dead_code))]
fn lock_is_stale(owner_text: &str, alive: impl Fn(u32) -> bool) -> bool {
    match owner_text.trim().parse::<u32>() {
        Ok(pid) => !alive(pid),
        Err(_) => false,
    }
}

/// 判活谓词：`kill(pid, 0)` 不投递信号，只做「进程存在且可寻址」的检查。
///
/// 历史实现是 `cfg!(target_os = "linux")` 时查 `/proc/<pid>`、**其余 Unix 一律返回 true**。
/// 那等于在 macOS 上彻底关闭了陈旧锁回收：配合「`Drop` 从不执行」（见 `VaultGuard::drop`），
/// 用户第一次正常退出后留下的 vault.lock 会把之后**每一次**启动挡在门外，
/// 且前端还会把这条错误包进「解锁失败（密码错误或 keyring/文件异常）」里，
/// 用户第一反应是怀疑自己记错了密码，而不是去隐藏目录里删锁文件。
///
/// `EPERM` 判活着：进程存在但属于别的用户，此时更不能回收它的锁。
/// 残余风险是 pid 回绕后被别的进程占用 → 误判占用（fail-closed 方向），
/// 用户按错误串里的指引删一次锁文件即可；优雅退出路径已由 `release_process_locks` 覆盖。
#[cfg(unix)]
fn pid_is_alive(pid: u32) -> bool {
    // pid 0 在 kill(2) 里是「本进程组」的特殊值，绝不能拿去探活。
    if pid == 0 {
        return true;
    }
    // SAFETY: kill 是纯查询（sig=0 不投递信号），入参只是一个整数，无内存安全前提。
    let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if rc == 0 {
        return true;
    }
    !matches!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    )
}

/// 释放本进程持有的全部数据目录锁（Unix 上删锁文件，Windows 上关独占句柄）。
///
/// 存在的理由：注册表持强引用且与进程等长，`VaultGuard::drop` 因此永不执行 ——
/// 不给一条显式的排空路径，「退出时清理锁」就只是一句写在注释里的空头承诺。
/// 由 app 在 `RunEvent::Exit` 上调用。
///
/// 仍被 `Store` 持有的守卫不会在此刻真正释放（`Arc` 计数未归零），这是对的：
/// 保险库还在用就不该松锁；进程随后退出时内核会收走 Windows 句柄，
/// Unix 侧则回落到下次启动的陈旧锁检测 —— 那条路径现在是通的。
pub fn release_process_locks() {
    let reg = guard_registry();
    let mut map = reg.lock().unwrap_or_else(|e| e.into_inner());
    map.clear();
}

/// 在启动最早期抢占整个数据目录的跨进程独占权（审计2 #12）。
///
/// 这把锁本来只在 `Store::open_or_create` / `unlock_*` 里**惰性**抢占，于是「同一数据目录
/// 同时只有一个实例」这条约束在保险库被解锁之前根本不成立。第二个实例照常建窗口、照常打开
/// 同一个 `fs.db`、照常往同一个日志文件里写、照常跑传输——而传输引擎的 per-target 锁是
/// **进程内** static（`fs_sshengine::transfer` 的 `TARGET_LOCKS`），于是两个实例能同时往同一个
/// `.fspart` 里写，谁也不知道对方在场。那正是审计2 #12 点名的那条路。
///
/// 惰性抢占还让失败文案彻底错位：第二个实例最终仍会撞上同一把锁，但撞的时机是用户**输完
/// 主口令之后**，冒出来的是一句「解锁失败（密码错误或 keyring/文件异常）」——与真实原因
/// （另一个实例）毫不相干，用户会去怀疑自己的密码。
///
/// 提前到启动最早期抢，把这条约束从「希望如此」变成「进不来」：第二个实例在建任何窗口、
/// 碰任何文件之前就以「另一个 future-shell 实例正在使用同一数据目录」收场。
///
/// 守卫存进与进程等长的注册表，故本函数**只需调用一次**，也无需保存返回值；随后保险库自己
/// 再调 `vault_guard(dir)` 会命中同一条注册表项（键为规范化路径），不会二次抢锁把自己锁在
/// 门外——这一点由 `lock_data_dir_is_reentrant_so_the_vault_can_still_open_afterwards` 钉住。
/// 释放仍走 `release_process_locks()`（app 在 `RunEvent::Exit` 上调用）。
///
/// 锁文件沿用 `vault.lock` 这个名字而不改成 `instance.lock`：改名会让新旧两个版本各持一把
/// 互不相干的锁，于是「升级期间不小心同时开着两个」恰恰是唯一一种能绕过闸门的情形——
/// 而那正是这条修复要挡的。名字不准确的代价只是一行注释，改名的代价是一个真实的漏洞窗口。
pub fn lock_data_dir(dir: &Path) -> Result<(), Error> {
    vault_guard(dir).map(|_| ())
}

/// 取得（或复用）某个数据目录的进程内守卫。
///
/// 注册表持**强引用**：守卫的语义就是「本进程占用着这个库」，
/// 用 `Weak` 反而会在「最后一个 Store 刚 drop、下一个 open 紧随其后」的窗口里
/// 释放又重抢跨进程锁 —— 平白制造一个别的实例能插进来的缝。
/// 条目数量上界 = 本进程打开过的数据目录数（现实里 1 个，测试里每个临时目录一条），可忽略。
/// 唯一的移除路径是显式的 `release_process_locks()`（进程退出时调用）。
fn guard_registry() -> &'static Mutex<HashMap<PathBuf, Arc<VaultGuard>>> {
    static REGISTRY: OnceLock<Mutex<HashMap<PathBuf, Arc<VaultGuard>>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn vault_guard(dir: &Path) -> Result<Arc<VaultGuard>, Error> {
    let reg = guard_registry();
    // 锁中毒不该让保险库变砖：注册表里只有 HashMap，被 panic 打断也不会留下逻辑上的半成品
    let mut map = reg.lock().unwrap_or_else(|e| e.into_inner());
    // 键用规范化路径，免得 `./data` 与 `data` 被当成两个库各拿一把闸门（那等于没有闸门）。
    // 规范化失败（目录还不存在等）就退回原路径 —— 退化成「多一条注册表项」，不影响正确性。
    let key = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    if let Some(g) = map.get(&key) {
        return Ok(g.clone());
    }
    let g = Arc::new(VaultGuard::acquire(&key)?);
    map.insert(key, g.clone());
    Ok(g)
}

pub struct Store {
    path: PathBuf,
    /// 主密钥。S7/S14（med）：类型必须是「drop 即擦」的 `Zeroizing`，不得退回裸 `[u8; 32]`。
    ///
    /// 历史实现用裸数组 + 手写 `impl Drop for Store` 兜底，有两处漏网：
    ///   ① `[u8; 32]` 是 `Copy` —— 每次 `Store { key, .. }` 字面量构造都会在栈上留一份
    ///      不受那个 `Drop` 管辖的明文副本；
    ///   ② 删掉 `impl Drop` 不会让任何测试变红（安全 Rust 里读已释放内存是 UB，无法直接断言）。
    /// 换成 `Zeroizing` 后：移动语义杜绝了①，擦除保证由类型本身携带；
    /// 而 `tests::master_key_field_must_self_zeroize` 把②变成**编译期**断言。
    key: Zeroizing<[u8; 32]>,
    file: VaultFile,
    /// P1-17：保存闸门 + 跨进程锁。按数据目录共享，见 `VaultGuard`。
    lock: Arc<VaultGuard>,
    /// 本实例的 `file` 快照取自磁盘的**第几代**（见 `VaultGuard::save_gate`）。
    ///
    /// 闸门只解决「两次写不交错」，解决不了「基于旧快照的整份覆盖」：
    /// 同一个库同时挂着两个 `Store`（app 的 `vault_init` 在旧实例仍活着时先建新实例，
    /// 解锁/改密路径也各建一个），A 存了一条凭据之后 B 再做任何一次 `put`/`delete`，
    /// B 都会拿自己那份**不含 A 那条记录**的快照整份写回去 —— A 那条凭据无声消失，
    /// 且 `next_id` 一并退回，下一条新记录会**复用同一个 id**（旧密文的 AAD 从此对不上）。
    /// 这不是理论推演：`tests/robustness.rs::in_process_reopen_shares_the_same_lock`
    /// 原样跑完后磁盘上剩的是 `next_id: 1, records: {}`，两条凭据全没了。
    ///
    /// 用原子量而不是普通字段，是因为 `save(&self)` 只拿得到共享引用（`change_passphrase`
    /// 之外的调用方都持 `&mut self`，但把 `save` 改成 `&mut self` 会波及全部调用点）。
    epoch: std::sync::atomic::AtomicU64,
}

fn keyring_entry() -> Result<keyring::Entry, Error> {
    keyring::Entry::new(SERVICE, USER).map_err(|e| Error::Keyring(e.to_string()))
}

/// 取出 OS 凭据库里的主密钥；条目不存在时**铸一把新的并写回**。
///
/// 「查—判断—铸—写回」这一整段必须互斥，且互斥必须是**进程全局**的 ——
/// 不能挂在 `VaultGuard` 上：凭据库条目由 `(SERVICE, USER)` 唯一确定，与数据目录无关，
/// 而 `VaultGuard` 是按目录分的，两个目录各自的闸门互不相干，挡不住两条首启路径同时铸钥。
/// 也不宜借用 `save_gate`：那把锁会被跨越 rename 的落盘持有，而 keyring 在部分平台上
/// 可能弹出交互授权框，把「等用户点确认」压进落盘闸门是另一种糟糕。
///
/// 不加这把锁的后果不是理论推演，本仓库实测过（见 `tests/common/mod.rs` 头注记录的现象）：
/// 两条 `open_or_create` 同时读到 `NoEntry` → 各铸一把 → 依次 `set_password`，
/// 凭据库里只剩后写的那把，先写者手里的 `Store` 却仍拿自己那把封记录。
/// 密文与凭据库就此错配，且错配**当场无感** —— 那次会话里读写都用同一个 `Store`，一切正常；
/// 直到下一次启动走 `unlock_with_keyring`（app 的默认静默解锁路径）才暴露成一片 `Integrity`，
/// 而此时已无从恢复：封了那些记录的密钥根本没被保存到任何地方。
/// 当时的处置是在测试装置里预先把条目铸好、令测试一律走「条目已存在」分支，
/// 产品侧的窗口原样留着 —— 本函数补的正是这一处。
///
/// 可达路径：`vault_init`，以及「库文件尚不存在时的 `vault_unlock`」（两者都落到
/// `open_or_create`，见 app/src/commands/vault_cmd.rs），两条 IPC 命令之间没有任何互斥。
fn master_key_from_keyring() -> Result<Zeroizing<[u8; 32]>, Error> {
    // 锁中毒（某次铸钥中途 panic）不该让保险库从此打不开：临界区内除 keyring 调用外不留状态，
    // 恢复用锁是安全的（与 `save_gate` 同一口径）。
    static MINT: Mutex<()> = Mutex::new(());
    let _mint = MINT.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let entry = keyring_entry()?;
    // 主密钥及其 base64 形态一律 Zeroizing 持有：离开作用域即擦除（spec §3.2「堆缓冲与栈副本用毕即擦」兑现）
    let key_b64 = match entry.get_password() {
        Ok(s) => Zeroizing::new(s),
        Err(keyring::Error::NoEntry) => {
            let mut k = Zeroizing::new([0u8; 32]);
            rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut *k);
            use base64::Engine;
            // 取切片而非 `&*k`/`*k`：后者会把 32 字节主密钥按值 Copy 进一份不受 Zeroizing
            // 管辖的栈临时量（clippy::needless_borrows_for_generic_args 正是要推向这一步）。
            let b64 = Zeroizing::new(base64::engine::general_purpose::STANDARD.encode(&k[..]));
            entry
                .set_password(&b64)
                .map_err(|e| Error::Keyring(e.to_string()))?;
            b64
        }
        Err(e) => return Err(Error::Keyring(e.to_string())),
    };
    decode_master_key(&key_b64)
}

/// 取 OS 凭据库里的主密钥（base64 原样）。条目不存在 = `None`，**不铸新钥**。
///
/// 与 `master_key_from_keyring` 的区别正在这里：那个函数是「打开保险库」用的，
/// 没有就铸一把是对的；本函数供恢复流程盘点现状，此处铸钥只会把一把与任何库都无关的
/// 新密钥写进凭据库，然后被下一行覆盖掉——纯粹的噪声。
fn keyring_master_b64() -> Result<Option<Zeroizing<String>>, Error> {
    match keyring_entry()?.get_password() {
        Ok(s) => Ok(Some(Zeroizing::new(s))),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(Error::Keyring(e.to_string())),
    }
}

fn set_keyring_master(b64: &str) -> Result<(), Error> {
    keyring_entry()?
        .set_password(b64)
        .map_err(|e| Error::Keyring(e.to_string()))
}

fn delete_keyring_master() -> Result<(), Error> {
    match keyring_entry()?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(Error::Keyring(e.to_string())),
    }
}

/// 把**被顶替**的那把主密钥挪到一个专用条目里（审计2 #27）。
///
/// `vault.json.replaced-<时间戳>` 是恢复操作留下的后悔药，可它只是一份密文；解它的钥
/// 恰好就是恢复时被覆盖掉的那个 keyring 条目。不留这一份，「留档」就只是留了个打不开的
/// 文件——比不留更坏，因为它看上去像是安全网。
///
/// 只保留**最近一次**被顶替的主密钥：连续恢复多次的话，更早那些留档文件就只能靠它们
/// 自身的应用密码打开了。这是有意的取舍——凭据库里长期堆积历史主密钥，本身就是一个
/// 越积越大的、无人清理的秘密面。
fn stash_replaced_master(b64: &str) -> Result<(), Error> {
    keyring::Entry::new(SERVICE, USER_REPLACED)
        .map_err(|e| Error::Keyring(e.to_string()))?
        .set_password(b64)
        .map_err(|e| Error::Keyring(e.to_string()))
}

/// base64 → 32 字节主密钥。长度不符即报错，不截断也不补零：
/// 凭据库里的条目若被外部工具改坏，宁可打不开，也不能拿一把长度凑出来的密钥去解密
///（那只会换来一片 `Integrity`，把排障的人引向「文件损坏」这条错误的路）。
fn decode_master_key(b64: &str) -> Result<Zeroizing<[u8; 32]>, Error> {
    use base64::Engine;
    let raw = Zeroizing::new(
        base64::engine::general_purpose::STANDARD
            .decode(b64.as_bytes())
            .map_err(|e| Error::Crypto(e.to_string()))?,
    );
    if raw.len() != 32 {
        return Err(Error::Crypto(format!(
            "keyring master key has invalid length {} (expected 32 bytes)",
            raw.len()
        )));
    }
    // S7：Zeroizing 非 Copy —— 移动进结构体后栈上不留明文副本
    let mut key = Zeroizing::new([0u8; 32]);
    key.copy_from_slice(&raw);
    Ok(key)
}

/// 这份库**要不要应用口令**——不解锁就能问（2026-08-31）。
///
/// 与 [`Store::has_passphrase`] 的区别只有一个，但那个区别是决定性的：那个是
/// `&self`，得先有一个解开的 `Store`；而调用方需要在**决定要不要弹解锁框之前**
/// 就知道答案——鸡生蛋。这里直接读文件头里的 `passphrase_phc`，不碰主密钥、
/// 不碰 keyring、不解任何密文。
///
/// 返回：
/// · `Ok(true)`  = 设过应用口令 → 调用方**不应替他静默解锁**（用户要的是有一道锁）。
///                 注意这不代表那道锁是严的：`unlock_with_keyring` 不读这一栏，
///                 空口令解锁照样开得了（2026-09-01 对抗审计核实，缺口待产品裁定）；
/// · `Ok(false)` = 没设 → 主密钥只在系统 keyring 里，可静默解锁，别拿框去烦用户；
/// · `Err(_)`    = 库文件读不出/解析不了 → 调用方按「不能静默」处置（fail-closed：
///                 宁可多问一次，也不要在一份坏掉的库上做假设）。
///
/// 库文件**不存在**时返回 `Ok(false)`：还没有库就谈不上口令。调用方据此决定
/// 是「首次建库」还是「静默解锁」（二者语义互斥，见 [`Store::unlock_with_keyring`]
/// 的文档警告——混用即静默数据毁灭）。
pub fn file_needs_passphrase(data_dir: &Path) -> Result<bool, Error> {
    let path = data_dir.join("vault.json");
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(Error::Storage(format!("读取 {} 失败：{e}", path.display()))),
    };
    Ok(parse_vault(&bytes)?.passphrase_phc.is_some())
}

impl Store {
    pub fn open_or_create(data_dir: &Path, passphrase: Option<&str>) -> Result<Store, Error> {
        std::fs::create_dir_all(data_dir).map_err(|e| Error::Storage(e.to_string()))?;
        let path = data_dir.join("vault.json");
        // P1-17：闸门必须在**读之前**取得。本方法是三条打开路径里唯一会写盘的
        //（首启建库 / 首次装口令），读—判断—回写这段区间若不受保护，
        // 两个实例可以同时读到「文件不存在」而各建一把主密钥，后写者赢，先写者的密文全成死信。
        let lock = vault_guard(data_dir)?;
        // S2（high）：只有「文件确实不存在」才允许当空库起步。历史实现 `Err(_) => default()`
        // 把权限拒绝、句柄被占、IO 故障、目录顶替文件等一切读失败都吞成空库，
        // 随后无条件 `save()` 覆写 —— 用户全部凭据在一次「打开」中静默灰飞烟灭。
        let (read, epoch) = lock.read_snapshot(&path);
        let (file, created) = match read {
            Ok(b) => (parse_vault(&b)?, false),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (VaultFile::default(), true),
            Err(e) => {
                return Err(Error::Storage(format!(
                    "无法读取 {}：{e}（拒绝以空库覆写既有文件）",
                    path.display()
                )))
            }
        };
        let key = master_key_from_keyring()?;
        let mut store = Store {
            path,
            key,
            file,
            lock,
            epoch: std::sync::atomic::AtomicU64::new(epoch),
        };
        let mut dirty = created;
        if let Some(p) = passphrase {
            // S20（low）：既有库带口令时，本方法语义是「打开」而非「改密」。历史实现无条件
            // 重铸 PHC/kdf_salt/master_sealed —— 一次输入笔误就把库口令悄悄改成了那个笔误，
            // 用户下次用真口令必然打不开。改密走 `change_passphrase`（需验现口令）。
            match store.file.passphrase_phc.clone() {
                Some(existing) => crypto::verify_passphrase(p, &existing)?,
                None => {
                    store.arm_passphrase(p)?;
                    dirty = true;
                }
            }
        }
        // 无变更就不重写：既有库的每一次无谓整文件重写都是一扇多余的损坏窗口
        if dirty {
            store.save()?;
        }
        Ok(store)
    }

    /// 修改应用口令：必须先验证现口令。无现口令的库（仅 keyring 路径）直接设定。
    ///
    /// 与 `open_or_create` 的分工见 S20 注记：打开永不改密，改密必经本方法。
    pub fn change_passphrase(&mut self, current: Option<&str>, new: &str) -> Result<(), Error> {
        if let Some(existing) = self.file.passphrase_phc.clone() {
            let cur = current.ok_or(Error::Locked)?;
            crypto::verify_passphrase(cur, &existing)?;
        }
        // P1-17：口令三元组（校验串 / KEK 盐 / master 副本）必须整组回滚。
        // 只回滚其中一两个比不回滚更糟：三者是**同一次派生**的产物，混搭之后
        // 新口令验得过 PHC、却解不开用旧 KEK 封的 master_sealed，库当场变砖。
        let prev = (
            self.file.passphrase_phc.clone(),
            self.file.kdf_salt.clone(),
            self.file.master_sealed.clone(),
        );
        self.arm_passphrase(new)?;
        if let Err(e) = self.save() {
            // 落盘失败 ⇒ 磁盘上仍是旧口令。内存不退回去的话，本次会话之后的每一次
            // `save()`（put/delete）都会顺手把这个从未落过盘的新口令写进去 —— 用户
            // 明明看到「改密失败」，改密却在下一次存凭据时悄悄生效了。
            self.file.passphrase_phc = prev.0;
            self.file.kdf_salt = prev.1;
            self.file.master_sealed = prev.2;
            return Err(e);
        }
        Ok(())
    }

    /// 存入 Argon2id 口令校验串 + KEK 独立盐 + 用 KEK 加密的 master key 副本（便携/降级解锁路径）。
    ///
    /// R117（critical）：校验串与 KEK 走**两把互不相干的随机盐**。历史实现从 PHC 自身的盐
    /// 用同一套 Argon2 参数重算 KEK，导致 KEK ≡ PHC 末段 hash 字段 —— 而 PHC 与它加密的
    /// `master_sealed` 明文并排躺在 vault.json 里，读到文件即可解密全部凭据。
    /// 回归测试见 `tests/kek_isolation.rs`。
    fn arm_passphrase(&mut self, passphrase: &str) -> Result<(), Error> {
        use base64::Engine;
        let phc = crypto::derive_key_from_passphrase(passphrase, "")?; // 仅作校验凭据
        let kdf_salt = crypto::generate_kdf_salt(); // 与 PHC 内嵌盐相互独立
        let kek = crypto::derive_kek(passphrase, &kdf_salt)?; // Zeroizing<[u8; 32]>：drop 即擦
        let sealed = crypto::seal(&kek, crypto::Aad::master(0), self.key.as_slice())?;
        self.file.passphrase_phc = Some(phc);
        self.file.kdf_salt = Some(base64::engine::general_purpose::STANDARD.encode(kdf_salt));
        self.file.master_sealed = Some((0, sealed));
        Ok(())
    }

    /// 用应用口令解出这份库的主密钥。**验证与解封是同一段代码**——这一点是有意为之。
    ///
    /// 审计2 #27 补上「恢复备份」之后，出现了第二个需要「验口令能不能打开这份库」的调用方。
    /// 若它自己另写一遍验证，两份代码迟早分叉，而分叉的现象是最恶劣的一种：恢复时告诉用户
    /// 「口令正确」，换好库之后解锁却说口令错——此刻旧库已被顶替，用户手里只剩一份
    /// 打不开的备份。让两条路径共用同一个函数，「验得过 ⇒ 解得开」就是结构上的事实，
    /// 而不是一句需要人去维护的注释。
    fn master_from_passphrase(
        file: &VaultFile,
        passphrase: &str,
    ) -> Result<Zeroizing<[u8; 32]>, Error> {
        let phc = file.passphrase_phc.as_deref().ok_or(Error::Locked)?;
        crypto::verify_passphrase(passphrase, phc)?;
        let (ver, master_sealed) = file.master_sealed.clone().ok_or(Error::Locked)?;
        // KEK 走独立盐（R117）：缺 kdf_salt 说明是修复前的旧格式库，明确报错而非回落到 PHC 盐
        use base64::Engine;
        let salt_b64 = file.kdf_salt.as_deref().ok_or_else(|| {
            Error::Crypto(
                "vault.json 缺少 kdf_salt（R117 修复前的旧格式）：请以应用密码重新初始化保险库"
                    .to_string(),
            )
        })?;
        let kdf_salt = Zeroizing::new(
            base64::engine::general_purpose::STANDARD
                .decode(salt_b64)
                .map_err(|e| Error::Crypto(e.to_string()))?,
        );
        let kek = crypto::derive_kek(passphrase, &kdf_salt)?; // Zeroizing<[u8; 32]>：drop 即擦
        let master = crypto::open(&kek, crypto::Aad::master(ver), &master_sealed)?; // Zeroizing<Vec<u8>>：主密钥明文，drop 即擦

        // S4（high）：master 明文长度来自文件内容，`copy_from_slice` 长度不符会 panic。
        // vault.json 是可被外部改写的数据，绝不能让畸形数据把库变成 panic 源（跨 IPC 即中止整个 core）。
        if master.len() != 32 {
            return Err(Error::Crypto(format!(
                "master_sealed 解出 {} 字节，应为 32 字节（vault.json 已损坏或被篡改）",
                master.len()
            )));
        }
        let mut key = Zeroizing::new([0u8; 32]);
        key.copy_from_slice(&master);
        Ok(key)
    }

    pub fn unlock_with_passphrase(data_dir: &Path, passphrase: &str) -> Result<Store, Error> {
        let path = data_dir.join("vault.json");
        let bytes = std::fs::read(&path).map_err(|e| Error::Storage(e.to_string()))?;
        let file = parse_vault(&bytes)?;
        let key = Self::master_from_passphrase(&file, passphrase)?;
        // P1-17：闸门在读盘成功之后才取 —— 本路径打开时不写盘，先读后锁不改变任何持久状态，
        // 却能保住既有的错误语义（库不存在/格式过旧仍报原来那条错，而不是先冒出一条「拿不到锁」）。
        // 闸门是给之后的 `save()`（改密、put/delete）用的。
        let lock = vault_guard(data_dir)?;
        let (file, epoch) = Self::adopt_snapshot(&lock, &path, |e| Error::Storage(e.to_string()))?;
        Ok(Store {
            path,
            key,
            file,
            lock,
            epoch: std::sync::atomic::AtomicU64::new(epoch),
        })
    }

    /// 闸门内重读一次，让 `file` 快照与 `epoch` 世代号同刻确定（见 `Store::epoch`）。
    ///
    /// 两条 `unlock_*` 路径都是「先读盘验密码、再取闸门」，中间还夹着一次 Argon2
    ///（几十到几百毫秒，故意慢）。这段窗口里同进程的另一个 `Store` 完全可能存进一条凭据；
    /// 若此刻直接采用最初那份快照并给它盖上当前世代号，新实例就成了一个
    /// 「自称最新、实则过期」的 `Store` —— 丢更新比对被它绕过去，本次修复形同虚设。
    ///
    /// 重读到的内容**无条件**采用，包括口令三元组（PHC / kdf_salt / master_sealed）：
    /// `change_passphrase` 只是拿新 KEK 重新封装**同一把** master key，
    /// 调用方手里已解出的明文主密钥不受影响，采用最新的三元组反而正确。
    /// 唯一在这条路径上会另铸主密钥的是 `open_or_create`，而它先锁后读，不经过这里。
    fn adopt_snapshot(
        lock: &VaultGuard,
        path: &Path,
        map_err: impl Fn(std::io::Error) -> Error,
    ) -> Result<(VaultFile, u64), Error> {
        let (read, epoch) = lock.read_snapshot(path);
        let bytes = read.map_err(map_err)?;
        Ok((parse_vault(&bytes)?, epoch))
    }

    /// 仅经 OS keyring 解锁**既有**库（R112）：`vault.json` 必须已存在、keyring 条目必须在位。
    /// 无库文件 → `Error::Locked`；有库文件但条目不在 → `Error::KeyringMissing`（路线图 4c：
    /// 两种缺失分开报，前端据此分岔「初始化向导」与「不可恢复，去备份」）。
    /// 两条失败路径都**绝不生成新主密钥、绝不回写文件**。
    /// 与 `open_or_create`「无条目即铸新钥并 `save()`」的语义严格互斥：后者只用于首启建库；
    /// 若拿它做二次启动的静默解锁，一旦 keyring 条目被清（换机/重装/凭据管理器清理），
    /// 它会铸一把新主密钥并覆写 `vault.json`，令既有密文永久不可解（静默数据毁灭）。
    pub fn unlock_with_keyring(data_dir: &Path) -> Result<Store, Error> {
        let path = data_dir.join("vault.json");
        let bytes = std::fs::read(&path).map_err(|_| Error::Locked)?; // 无库文件 → Locked（不建库）
                                                                      // 解析结果在这里被丢弃，权威快照由下方 `adopt_snapshot` 在闸门内重取。
                                                                      // 但这次解析**不能省**：它是「库不存在 / 格式不兼容」的前置闸门，必须在
                                                                      // `vault_guard` 之前跑完 —— 否则一个尚无保险库的目录会先被落下一个 vault.lock，
                                                                      // 而 app 正是靠这条 `Locked` 决定要不要显示初始化向导。
        parse_vault(&bytes)?;
        let entry = keyring_entry()?;
        let key_b64 = match entry.get_password() {
            Ok(s) => Zeroizing::new(s),
            Err(keyring::Error::NoEntry) => return Err(Error::KeyringMissing), // 无条目 → KeyringMissing（不铸新钥；与无库文件的 Locked 分开）
            Err(e) => return Err(Error::Keyring(e.to_string())),
        };
        // 本路径**只读**凭据库，故不经 `master_key_from_keyring`（那是「取或铸」，会写回）：
        // 拿它做二次启动的静默解锁正是上面注记里点名禁止的那件事。共用的只有解码这一段。
        let key = decode_master_key(&key_b64)?;
        let lock = vault_guard(data_dir)?; // 同 unlock_with_passphrase：先读后锁，不动既有错误语义
                                           // 重读失败仍归 `Locked`：本路径对「读不到库文件」的既有口径就是 Locked（不建库），
                                           // 不能因为多了一次重读就换成 Storage —— app 靠这条错误决定是否显示初始化向导。
        let (file, epoch) = Self::adopt_snapshot(&lock, &path, |_| Error::Locked)?;
        Ok(Store {
            path,
            key,
            file,
            lock,
            epoch: std::sync::atomic::AtomicU64::new(epoch),
        }) // 只读打开：不 save()、不 arm_passphrase
    }

    pub fn put(
        &mut self,
        kind: SecretKind,
        label: String,
        secret: Zeroizing<Vec<u8>>,
    ) -> Result<u64, Error> {
        let prev_next_id = self.file.next_id; // P1-17 回滚锚点，见下方 save 失败分支
        self.file.next_id += 1;
        let id = self.file.next_id;
        // S8：kind/label 一并进 AAD —— 之后任何人在 vault.json 里改动它们，`get` 都会报 Integrity
        let sealed = crypto::seal(
            &self.key,
            crypto::Aad {
                record_id: id,
                version: 1,
                kind: kind.aad_tag(),
                label: &label,
            },
            &secret,
        )?;
        self.file.records.insert(
            id,
            Record {
                id,
                kind,
                label,
                version: 1,
                sealed,
            },
        );
        // P1-17：save 失败必须把内存**整个**退回调用前 —— 连 next_id 一起。
        // 内存里的 `file` 是 `get`/`list` 的唯一数据源，它与磁盘一旦分叉就再也对不回来：
        // 用户看到「已保存」、界面上凭据在列，重启后那条记录凭空消失（静默丢数据）。
        // 反过来 `delete` 不回滚则是已删凭据在重启后复活。两者都属于「无声的错」，
        // 比当场报一个 `Err` 恶劣得多 —— 报错用户还能重试，分叉只能等到下次重启才发现。
        if let Err(e) = self.save() {
            self.file.records.remove(&id);
            self.file.next_id = prev_next_id;
            return Err(e);
        }
        Ok(id)
    }

    /// AAD 的 `record_id` 取**查表用的键**（`id`），不取记录自报的 `r.id`。
    ///
    /// S8 把 kind/label 一并绑进 AAD，是为了「谁在 vault.json 里改这两个字段，`get` 就报
    /// Integrity」。但历史实现里 AAD 的四个分量**全部**读自那条记录对象自身 ——
    /// 于是把整条记录（含它自报的 id）从一个键搬到另一个键，四个分量原封不动跟着走，
    /// AAD 完全对得上，密文照常解开。攻击者不需要读懂密文，只要在 JSON 里挪一挪位置，
    /// 就能让「读取 3 号凭据」返回 7 号凭据的明文（例如把测试机的口令挪到生产机的槽位，
    /// 或反过来把生产密钥喂给一台受控主机）。
    ///
    /// 改用查表键之后，`record_id` 变成**调用方问的那个 id**，不再来自被篡改的文件内容，
    /// 挪位当场变成 AAD 不匹配。`parse_vault` 里另有一道 `键 == r.id` 的一致性闸门，
    /// 两道防线各自独立：这里防的是内存中构造出的错位，那里防的是磁盘上写好的错位。
    pub fn get(&self, id: u64) -> Result<Zeroizing<Vec<u8>>, Error> {
        Ok(self.get_typed(id)?.1)
    }

    /// 同 [`Store::get`]，但一并返回记录声明的**用途**（审计2 #20）。
    ///
    /// 存在的理由是调用方需要 kind 才能决定拿这份字节干什么，而 `list()` + `get()` 两次查表
    /// 拿到的 kind 与明文**可以来自不同的读**：中间任何一次 `save`/`delete`/重载都会让两者
    /// 对不上，于是「按 A 的类别使用 B 的材料」——恰恰是本次要消灭的那类混淆。这里一次查表、
    /// 一次解封，kind 与明文同源。
    ///
    /// 更强的一层来自 AAD：`kind` 已被绑进 AEAD 的附加数据，所以返回的这个 kind 是
    /// **被认证过的**。谁在 `vault.json` 里把一条记录的 `kind` 从 `password` 改成
    /// `private_key`，这里不是悄悄换个用途，而是当场 `Integrity` 报错——篡改类别
    /// 不能成为一条重定向凭据用途的路径。
    pub fn get_typed(&self, id: u64) -> Result<(SecretKind, Zeroizing<Vec<u8>>), Error> {
        let r = self.file.records.get(&id).ok_or(Error::NotFound(id))?;
        let plain = crypto::open(
            &self.key,
            crypto::Aad {
                record_id: id,
                version: r.version,
                kind: r.kind.aad_tag(),
                label: &r.label,
            },
            &r.sealed,
        )?;
        Ok((r.kind, plain))
    }

    pub fn delete(&mut self, id: u64) -> Result<(), Error> {
        let removed = self.file.records.remove(&id).ok_or(Error::NotFound(id))?;
        // P1-17：落盘失败即把整条记录塞回去（BTreeMap 按 id 排序，插回原 id 即原位）。
        // 不回滚的话本进程后续 `get(id)` 报 NotFound、磁盘上却仍留着密文，
        // 重启后这条「已删除」的凭据原样复活 —— 用户以为撤销掉的授权其实一直在。
        if let Err(e) = self.save() {
            self.file.records.insert(id, removed);
            return Err(e);
        }
        Ok(())
    }

    /// 这份库是否设过应用口令（审计2 #28）。
    ///
    /// 没有口令 = 主密钥**只**在系统 keyring 里。这不是一个性能或便利的取舍：keyring 条目
    /// 一旦消失（重装系统、换机器、凭据管理器被清理、Linux 上 Secret Service 重置），
    /// vault.json 里的每一条密文都永久解不开——没有任何补救手段，因为世界上再没有第二份
    /// 主密钥。UI 必须能问出这件事，才能在用户还来得及的时候把话说清楚。
    pub fn has_passphrase(&self) -> bool {
        self.file.passphrase_phc.is_some()
    }

    /// 导出一份**可离机恢复**的备份，返回文件名（审计2 #27 / 发布门槛第 5 条）。
    ///
    /// 没设应用口令就**拒绝导出**，而不是导出一份注定打不开的文件。理由是备份的全部意义
    /// 在于「这台机器没了之后还拿得回来」：主密钥只在本机 keyring 里时，拷走 vault.json
    /// 得到的是一份任何人（包括机主自己）都无法解开的密文——它唯一的作用是让用户以为
    /// 自己已经备份过了。给出一份假的安全感，比明说「还不能备份」危险得多。
    ///
    /// 写进 `<data_dir>/vault-backups/` 这个固定目录，**不接受调用方指定路径**。
    /// 前端能指定路径，就等于渲染进程手里有了一个「往任意路径写任意内容」的原语，
    /// 而这里写出去的恰好是整个密文库；固定目录把这个面完全去掉。用户要把备份带走，
    /// 用系统文件管理器从这个目录拷即可（UI 会把绝对路径显示出来）。
    pub fn export_backup(&self) -> Result<String, Error> {
        if !self.has_passphrase() {
            return Err(Error::Storage(
                "尚未设置应用密码：主密钥只存在于本机系统 keyring 中，导出的备份在任何其他机器上都无法解开。\
                 请先设置应用密码，再导出备份"
                    .to_string(),
            ));
        }
        let dir = backup_dir(self.data_dir());
        create_private_dir(&dir)?;
        let name = free_backup_name(&dir)?;
        let json =
            serde_json::to_vec_pretty(&self.file).map_err(|e| Error::Storage(e.to_string()))?;
        // write_atomic 建临时文件时就带 0600（见其 S10 注记）：备份与 vault.json 同等敏感，
        // 走同一条写路径而不是另写一个 fs::write，省得权限收紧这件事在这里被漏掉。
        write_atomic(&dir.join(&name), &json)?;
        Ok(name)
    }

    fn data_dir(&self) -> &Path {
        // `path` 恒为 `<data_dir>/vault.json`，三条打开路径都是这么拼的
        self.path.parent().unwrap_or(Path::new("."))
    }

    /// 列出备份目录里的候选文件。目录不存在 = 还没备份过，返回空表而不是报错。
    ///
    /// 不校验内容是否真是保险库：那需要读进来解析，而这个列表要在每次开对话框时刷新。
    /// 真正的把关在 `restore_backup`——它在动现有库之前会把整份备份解析、验口令、解主密钥
    /// 走一遍。这里只负责「有哪些文件可选」。
    pub fn list_backups(data_dir: &Path) -> Result<Vec<BackupEntry>, Error> {
        let dir = backup_dir(data_dir);
        let rd = match std::fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => {
                return Err(Error::Storage(format!(
                    "无法读取备份目录 {}：{e}",
                    dir.display()
                )))
            }
        };
        let mut out = Vec::new();
        for entry in rd.flatten() {
            let Ok(meta) = entry.metadata() else { continue };
            if !meta.is_file() {
                continue;
            }
            let Some(name) = entry.file_name().to_str().map(|s| s.to_string()) else {
                continue;
            };
            if backup_file_name(&name).is_err() {
                continue;
            }
            out.push(BackupEntry {
                name,
                bytes: meta.len(),
                modified_unix: meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
            });
        }
        // 新的在前：恢复时用户要找的几乎总是最近那一份
        out.sort_by(|a, b| {
            b.modified_unix
                .cmp(&a.modified_unix)
                .then(a.name.cmp(&b.name))
        });
        Ok(out)
    }

    /// 用备份顶替现有保险库；返回被顶替的那份库的留档路径，**目标目录本就没有库时为 `None`**
    ///（审计2 #27 / 发布门槛第 5 条）。
    ///
    /// 返回 `Option` 而不是恒返一个路径：换机器后恢复到空目录是主用场景，此时并没有
    /// 什么东西被顶替。恒返路径会让 UI 显示一句「原有保险库已留档为 …」，指向一个
    /// 并不存在的文件——用户会去找它。
    ///
    /// 顺序是这条命令的全部要害：**先把备份完整验通，一个字节都不写；验通了才动现有库。**
    ///   ① 解析（格式版本、记录错位、计数器倒退三道闸门与正常打开完全同口径）
    ///   ② 用用户给的口令验 PHC 并真的解出 32 字节主密钥（`master_from_passphrase`，
    ///      与 `unlock_with_passphrase` 是同一段代码）
    ///   ③ 现有 vault.json **复制**一份留档（不是移走：复制失败也不会让用户暂时没有库）
    ///   ④ 原子替换
    /// 任何一步失败，现有库都还在原地、还能用原来的口令打开。
    ///
    /// 「恢复」是这个程序里唯一一个会让用户失去凭据的操作，它必须**不可能**把库换成一份
    /// 打不开的东西——所以验证不是走个过场，而是真的把主密钥解出来。
    pub fn restore_backup(
        data_dir: &Path,
        name: &str,
        passphrase: &str,
    ) -> Result<Option<PathBuf>, Error> {
        let name = backup_file_name(name)?;
        let src = backup_dir(data_dir).join(name);
        let bytes = std::fs::read(&src)
            .map_err(|e| Error::Storage(format!("无法读取备份 {}：{e}", src.display())))?;
        let file = parse_vault(&bytes)?;
        let master = Self::master_from_passphrase(&file, passphrase)?;
        use base64::Engine;
        let master_b64 =
            Zeroizing::new(base64::engine::general_purpose::STANDARD.encode(&master[..]));

        let path = data_dir.join("vault.json");
        let lock = vault_guard(data_dir)?;
        // 与 `save()` 同一把闸门：替换期间不许有别的 save 交错进来，
        // 且替换完必须推进世代号——在世的每个 `Store` 实例手里的快照此刻全部作废，
        // 它们后续的 `save()` 会被丢更新比对拦下（fail-closed），不会把旧内容写回去。
        let mut gate = lock
            .save_gate
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let prev_master_b64 = keyring_master_b64()?;
        let displaced = if path.exists() {
            let to = path.with_file_name(format!(
                "vault.json.replaced-{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0)
            ));
            // copy 而非 rename：中途失败时现有库仍在原位可用。
            // Unix 上 `fs::copy` 连权限位一起复制，源是 0600，留档也就是 0600。
            std::fs::copy(&path, &to).map_err(|e| {
                Error::Storage(format!(
                    "无法为现有保险库留档到 {}：{e}（已中止恢复，现有保险库未被改动）",
                    to.display()
                ))
            })?;
            // 留档文件光有密文没用：解它的主密钥就在下一步被顶掉的那个 keyring 条目里。
            // 先把旧主密钥挪进一个专用条目，`vault.json.replaced-*` 才是真能打开的东西，
            // 否则「恢复」这个动作会连带把用户原来那份库变成永久解不开的密文。
            if let Some(prev) = prev_master_b64.as_deref() {
                stash_replaced_master(prev)?;
            }
            Some(to)
        } else {
            None
        };
        // keyring 必须跟着换：主密钥存在 OS 凭据库里，而备份来自另一台机器（或另一次
        // 初始化）时它自带一把不同的主密钥。不换的话，恢复表面成功，下次启动走静默
        // keyring 解锁却是**每一条记录都 Integrity**——用户看到的是「备份把库恢复坏了」。
        //
        // 放在写盘**之前**，是为了让失败可回滚：此刻磁盘还没动，
        // 这一步失败就干净中止；写盘失败则把 keyring 退回 `prev_master_b64`。
        // 反过来（先写盘后换钥）中间失败会留下「库是新的、钥是旧的」，无从收拾。
        set_keyring_master(&master_b64)?;
        if let Err(e) = write_atomic(&path, &bytes) {
            let rollback = match prev_master_b64.as_deref() {
                Some(prev) => set_keyring_master(prev),
                // 恢复前本就没有 keyring 条目：删掉本次写进去的，回到「没有」这个原状
                None => delete_keyring_master(),
            };
            if let Err(re) = rollback {
                return Err(Error::Keyring(format!(
                    "恢复写盘失败（{e}），且系统 keyring 中的主密钥未能退回原值（{re}）：\
                     现有保险库文件未被改动，但静默解锁此刻会失败，请用应用密码解锁"
                )));
            }
            return Err(e);
        }
        *gate += 1;
        Ok(displaced)
    }

    /// 同 `get`：对外报的 id 取查表键。列表里的 id 是用户随后拿去调 `get`/`delete` 的凭证，
    /// 它必须与「按这个键能取到这条记录」严格同义，不能是记录自报的值。
    pub fn list(&self) -> Vec<RecordMeta> {
        self.file
            .records
            .iter()
            .map(|(id, r)| RecordMeta {
                id: *id,
                kind: r.kind,
                label: r.label.clone(),
            })
            .collect()
    }

    fn save(&self) -> Result<(), Error> {
        // P1-17：进程内串行化。同一个库同时被两个 `Store` 实例写（app 的 vault_init 重入、
        // 或异步任务与 UI 命令并发）时，两次「建临时文件 → 写 → rename」若交错，
        // 结果由 rename 的完成顺序决定 —— 后者会把前者整份内容覆盖掉，且**没有任何报错**。
        // 锁中毒（某次 save 中途 panic）不该让保险库从此无法保存：临时文件路线下
        // 目标文件永远是「上一个完整版本」，恢复用锁是安全的。
        let mut gate = self
            .lock
            .save_gate
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // 丢更新拦截（见 `Store::epoch`）：闸门保证「不交错」，拦不住「基于旧快照整份覆盖」。
        // 拒绝而不是合并 —— 合并两份快照需要逐记录的三方比对与冲突裁决，而这里每一条都是
        // 用户凭据，猜错的代价是静默丢一把私钥；报错至少把决定权交回给人。
        let based_on = self.epoch.load(std::sync::atomic::Ordering::SeqCst);
        if *gate != based_on {
            return Err(Error::Storage(format!(
                "保险库已被本进程内的另一个实例改写（磁盘为第 {} 代，本实例基于第 {based_on} 代）：\
                 继续保存会把那些改动整份覆盖掉，已拒绝。请重新解锁保险库后重试",
                *gate
            )));
        }
        let json =
            serde_json::to_vec_pretty(&self.file).map_err(|e| Error::Storage(e.to_string()))?;
        write_atomic(&self.path, &json)?;
        // 只有真正落盘成功才推进世代号：写失败时磁盘仍是上一代，
        // 此处若先加再写，本实例会以为自己是最新的，下一次保存反而绕过比对。
        *gate += 1;
        self.epoch.store(*gate, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
}

fn backup_dir(data_dir: &Path) -> PathBuf {
    data_dir.join(BACKUP_DIR)
}

/// 建备份目录，并在 Unix 上收到 `0700`。
///
/// 目录里放的是**整份密文库**，含口令 PHC 与 KDF 盐——离线爆破所需的全部原料。
/// 权限口径必须与 vault.json 本身一致（S10 / 审计2 #31），不能交给 umask 碰运气：
/// 多数发行版默认 umask 给出的是 0755，同机其他账户可以直接列目录并拷走。
/// 对**已存在**的目录也照样收紧一次——「上个版本建的目录是 0755」正是最可能出现的情形，
/// 而 `create_dir_all` 碰到既有目录时一个权限位都不会动。
fn create_private_dir(dir: &Path) -> Result<(), Error> {
    std::fs::create_dir_all(dir)
        .map_err(|e| Error::Storage(format!("无法创建备份目录 {}：{e}", dir.display())))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| Error::Storage(format!("无法收紧备份目录权限 {}：{e}", dir.display())))?;
    }
    Ok(())
}

/// 备份文件名的**唯一**校验入口（审计2 #27）。
///
/// 「恢复备份」这条命令的文件名来自渲染进程，而它会被直接拼到 `<data_dir>/vault-backups/`
/// 后面。不校验的话，`../../../.ssh/id_rsa` 这样的名字就把「恢复备份」变成了「拿任意路径
/// 的文件当保险库读」：即便读不通会报错，报错本身也已经是一个「这个路径存不存在、是不是
/// 合法 JSON」的探针，而这条命令又是特权侧（app 进程）在执行。所以走白名单——前缀、
/// 后缀、字符集、长度全部钉死，凡不是本程序自己导出的名字一律拒绝。
///
/// 返回原串而不是 `bool`，是为了让调用方**只能**拿到校验过的那一份去拼路径；
/// 返回 bool 的写法允许「校验了 a、却用 b 去拼」，那种错一眼看不出来。
fn backup_file_name(name: &str) -> Result<&str, Error> {
    const PREFIX: &str = "vault-backup-";
    const SUFFIX: &str = ".json";
    let bad = |why: &str| Error::Storage(format!("备份文件名不合法（{why}）：{name}"));
    if name.len() > 128 {
        return Err(bad("过长"));
    }
    // 长度下限先判：否则下面的切片在「前缀与后缀重叠」的短名上会越界 panic
    if name.len() < PREFIX.len() + SUFFIX.len()
        || !name.starts_with(PREFIX)
        || !name.ends_with(SUFFIX)
    {
        return Err(bad("必须形如 vault-backup-<时间戳>.json"));
    }
    // 前后缀都是纯 ASCII，故这两个下标必落在字符边界上
    let stem = &name[PREFIX.len()..name.len() - SUFFIX.len()];
    if stem.is_empty() || !stem.bytes().all(|b| b.is_ascii_digit() || b == b'-') {
        return Err(bad("时间戳部分只能由数字与连字符组成"));
    }
    Ok(name)
}

/// 挑一个当前目录里尚未占用的备份名。
///
/// 秒级时间戳会撞——同一秒内导出两次、或系统时钟被回拨——撞上就带序号往后排。
/// **绝不覆盖已有备份**：用户连点两次「导出」不该让上一份就此消失，而备份恰恰是
/// 他手里最后一根绳子。
fn free_backup_name(dir: &Path) -> Result<String, Error> {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    for n in 0..1000u32 {
        let name = if n == 0 {
            format!("vault-backup-{secs}.json")
        } else {
            format!("vault-backup-{secs}-{n}.json")
        };
        if !dir.join(&name).exists() {
            return Ok(name);
        }
    }
    Err(Error::Storage(format!(
        "{} 下同一秒内的备份名已用尽（已有 1000 份）",
        dir.display()
    )))
}

/// 临时文件名：`vault.json.<pid>.<纳秒>.<序号>.tmp`。
///
/// P1-17：历史实现用固定名 `vault.json.tmp`。固定名有两处致命性：
///   · 同机两个实例（或同进程两次 save）同时写同一个临时文件，各写一半、
///     两次 rename 分别把对方的半成品搬到最终路径 —— 得到一个 JSON 语法都不成立的库；
///   · 上一次崩溃留下的残骸会被 `create_new` 直接拒绝（若用 `write` 则被静默复用）。
/// 命名口径与仓库既有先例一致（`crates/sshengine/tests/transfer.rs::temp_path`）：
/// pid 防跨进程、纳秒防同进程连续两次、序号防 Windows 上时钟粒度粗（可达 15 ms）
/// 导致的同刻撞名 —— 只靠纳秒在 Windows 上是不够的。
fn tmp_path_for(dest: &Path) -> PathBuf {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let mut name = dest.file_name().unwrap_or_default().to_os_string();
    name.push(format!(
        ".{}.{}.{}.tmp",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0), // 时钟早于纪元只是让防撞退化，不该让保存失败
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    dest.with_file_name(name)
}

/// 「写临时文件 → fsync → rename」的原子替换。任一步失败都保证：
/// 目标路径仍是**上一个完整版本**，且不留临时残骸。
///
/// P1-17：`sync_all` 不可省。`fs::write` + `rename` 只保证「换名这件事」相对于文件系统
/// 元数据是原子的，**不保证内容已经落到介质**：崩溃/掉电后完全可能出现
/// 「vault.json 已指向新 inode、内容却是全零/半截」—— 用户的全部凭据换成一个零长度文件。
/// 顺序必须是 sync_all 在 rename 之前，否则同样是在给窗口期背书。
fn write_atomic(dest: &Path, bytes: &[u8]) -> Result<(), Error> {
    let tmp = tmp_path_for(dest);
    // 失败即清残骸：临时文件里是**完整的密文库**（含 PHC 与盐），留在盘上等于多一份可被
    // 拷走的副本；而且下次 save 用的是新名字，这份残骸永远不会被复用，只会越积越多。
    let cleanup = |e: Error| -> Error {
        let _ = std::fs::remove_file(&tmp);
        e
    };
    let mut opts = OpenOptions::new();
    opts.write(true).create_new(true);
    // S10（med）：收权必须在 rename **之前**——先改名再收权会留下一个「最终路径已就位、
    // 权限仍是 umask 默认（多数发行版 0644）」的可读窗口。多用户主机上同机其他账户
    // 在该窗口内即可拷走整个密文库连同 PHC/盐（离线爆破的全部原料）。
    // P1-17 收紧一步：权限直接在 `open` 时给定，连「文件已建好、内容还没写、权限仍是默认」
    // 这个更早的窗口也一并消除。Windows 侧 %APPDATA% 默认继承「仅本用户 + SYSTEM」ACL，无需额外处理。
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts
        .open(&tmp)
        .map_err(|e| Error::Storage(format!("无法创建保险库临时文件 {}：{e}", tmp.display())))?;
    f.write_all(bytes)
        .map_err(|e| cleanup(Error::Storage(format!("写入 {} 失败：{e}", tmp.display()))))?;
    // 数据 + 元数据一起刷；`sync_data` 不够，新建文件的长度属于元数据
    f.sync_all()
        .map_err(|e| cleanup(Error::Storage(format!("{} 落盘失败：{e}", tmp.display()))))?;
    drop(f); // Windows 上句柄未关时 rename 会吃到共享冲突，必须先关
    std::fs::rename(&tmp, dest).map_err(|e| {
        cleanup(Error::Storage(format!(
            "无法将 {} 换名为 {}：{e}",
            tmp.display(),
            dest.display()
        )))
    })?;
    // 父目录 fsync：rename 本身也是一次元数据改动，不刷目录项的话崩溃后可能回到旧名。
    //
    // 刻意**不**把它的失败向上报：此刻 rename 已经成功，内存与磁盘内容是一致的，
    // 再返回 Err 会触发调用方的 P1-17 回滚，把内存退回旧值 —— 制造出一个方向相反的分叉
    //（磁盘新、内存旧），比「元数据可能晚几秒才持久」严重得多。
    #[cfg(unix)]
    if let Some(parent) = dest.parent() {
        let _ = File::open(parent).and_then(|d| d.sync_all());
    }
    // Windows 无「目录 fsync」这一说：目录不是可打开来同步的对象，rename 的元数据
    // 持久性由 NTFS 日志（USN/事务日志）保证，无需（也无法）在用户态额外触发。
    Ok(())
}

#[cfg(test)]
mod tests {
    /// 免解锁判据：没设口令的库答 false（2026-08-31 静默解锁的判据源）。
    ///
    /// 这条判据决定「要不要拿解锁框去烦用户」。答错的两个方向都很贵：
    /// 答成 true = 没设口令的人每次启动仍被弹框（用户报的就是这个）；
    /// 答成 false = 设过口令的人被自动解开，他要的那道锁被绕开一层。
    #[test]
    fn a_vault_without_a_passphrase_reports_no_passphrase_needed() {
        let dir = unit_tempdir("np-probe");
        write_probe_vault(&dir, None);
        assert!(
            !file_needs_passphrase(&dir).expect("读判据"),
            "没设应用口令的库必须答「不需要口令」——否则用户每次启动都要看那个空框"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 设过口令的库答 true：用户要的是有一道锁，判据不许说反。
    #[test]
    fn a_passphrase_protected_vault_reports_that_it_needs_one() {
        let dir = unit_tempdir("pp-probe");
        // PHC 串的**内容**与本判据无关（只看这一栏在不在），故用一个形状合法的常量，
        // 不去真的派生——派生要走 keyring，而 keyring 条目与数据目录无关，
        // 单测碰它就是在动开发者本机那条生产条目（对抗审计报出，已核实）。
        write_probe_vault(&dir, Some("$argon2id$v=19$m=19456,t=2,p=1$c2FsdA$aGFzaA"));
        assert!(
            file_needs_passphrase(&dir).expect("读判据"),
            "设过应用口令的库必须答「需要口令」——判据说反就等于替用户把锁开了"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 库文件不存在 → 答 false，且**判据本身绝不建库**。
    ///
    /// 「不建库」这半条是安全要害：`unlock_with_keyring` 与 `open_or_create`
    /// 语义互斥，在没有 keyring 条目的机器上误走后者会铸一把新主密钥、
    /// 把既有密文永久锁死（本文件对此的原话是「静默数据毁灭」）。
    #[test]
    fn probing_a_missing_vault_neither_fails_nor_creates_one() {
        let dir = unit_tempdir("missing-probe");
        assert!(!file_needs_passphrase(&dir).expect("空目录该答 false"));
        assert!(
            !dir.join("vault.json").exists(),
            "判据函数不得建库——它只读文件头"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 判据夹具：手写一份**形状合法**的 vault.json。
    ///
    /// 刻意不经 `Store::open_or_create`：那条路会读/写系统 keyring，而 keyring 条目由
    /// `(SERVICE, USER)` 唯一确定、**与 data_dir 无关**——单测跑一次就动了本机
    /// 生产库用的同一条条目，条目缺失时还会铸一把新主密钥写回。
    /// `file_needs_passphrase` 只看 `passphrase_phc` 这一栏，手写 JSON 覆盖面等价。
    fn write_probe_vault(dir: &std::path::Path, phc: Option<&str>) {
        let json = serde_json::json!({
            // 用常量而不是字面量：格式版本升级时夹具跟着走，
            // 不会变成一份「本程序已不支持」的假库（第一次就踩到了）。
            "format": VAULT_FORMAT,
            "next_id": 0,
            "passphrase_phc": phc,
            "kdf_salt": serde_json::Value::Null,
            "master_sealed": serde_json::Value::Null,
            "records": {},
        });
        std::fs::write(
            dir.join("vault.json"),
            serde_json::to_vec(&json).expect("夹具序列化"),
        )
        .expect("写夹具");
    }

    use super::*;

    /// S14（med）：主密钥字段必须自带零化语义。
    ///
    /// 安全 Rust 无法断言「已释放的内存确已清零」（读已释放内存是 UB），所以历史上
    /// `impl Drop for Store { self.key.zeroize() }` 被删掉也不会让任何测试变红 ——
    /// 一条纯靠人眼守护的安全不变式。改用类型携带保证后，本函数把它降级成**编译期**问题：
    /// 只要有人把 `key` 换回裸 `[u8; 32]`（或任何不擦除的类型），这里就编译失败。
    #[allow(dead_code)]
    fn master_key_field_must_self_zeroize(s: &Store) -> &Zeroizing<[u8; 32]> {
        &s.key
    }

    /// P1-17 配套：`Store` 必须保持 `Send + Sync`。
    ///
    /// app 侧把它装在 `Arc<tokio::sync::Mutex<Option<Store>>>` 里跨任务传递
    ///（session 装配、断线 watchdog、自动重连循环各持一份）。本次给结构体加了
    /// `Arc<VaultGuard>` 字段 —— 只要有人日后往守卫里塞进一个 `Rc`/`Cell`
    /// 之类的非 Sync 成员，破坏就会以「app crate 里一长串 tokio 泛型错误」的形式
    /// 在**下游**炸开，归因成本极高。在源头钉一条编译期断言，让它就地失败。
    #[allow(dead_code)]
    fn store_must_stay_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Store>();
    }

    // ── 装置 ────────────────────────────────────────────────────────────────

    /// 唯一键必须带纳秒、且用 `create_dir`（已存在即重来）。
    /// 口径与 `tests/common/mod.rs` 的 S22 注记一致：Windows 积极回收 pid，
    /// 「pid + 进程内计数器」会把前人的终态目录原样交回 —— 对 vault 尤其致命，
    /// 目录里若已躺着一份别人主密钥封的 `vault.json`，本节的断言测的就不是本次行为。
    fn unit_tempdir(tag: &str) -> PathBuf {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let base = std::env::temp_dir();
        loop {
            let p = base.join(format!(
                "fs-vault-unit-{tag}-{}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("系统时钟早于 UNIX 纪元")
                    .as_nanos(),
                SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            ));
            match std::fs::create_dir(&p) {
                Ok(()) => return p,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => panic!("创建测试临时目录失败 {}: {e}", p.display()),
            }
        }
    }

    /// 不经 keyring 直接拼一个 `Store`。
    ///
    /// 本节要钉的是**本模块内部的取值口径**（AAD 锚在哪、世代号怎么推进），
    /// 主密钥从哪来与之无关；而拉起 mock keyring 会把一个进程全局开关引进单元测试
    /// 二进制（其代价见 `tests/common/mod.rs` 的 S16 注记：静默换钥导致的随机 Integrity）。
    /// 真实加载路径由 `tests/robustness.rs` 与 `tests/aad_binding.rs` 覆盖。
    fn raw_store(dir: &Path) -> Store {
        Store {
            path: dir.join("vault.json"),
            key: Zeroizing::new([0x5au8; 32]),
            file: VaultFile::default(),
            lock: vault_guard(dir).unwrap(),
            epoch: std::sync::atomic::AtomicU64::new(0),
        }
    }

    fn put(s: &mut Store, label: &str, secret: &[u8]) -> u64 {
        s.put(
            SecretKind::Password,
            label.to_string(),
            Zeroizing::new(secret.to_vec()),
        )
        .unwrap()
    }

    // ── 陈旧锁判定（P0：macOS 每次启动被自己锁死）────────────────────────────

    /// 判定必须只认「锁文件里的 pid 已经不在了」这一种情形。
    ///
    /// 三条否定分支各自对应一次真实事故的反面：判活说存活却回收 → 两个实例同时整文件
    /// 重写同一个库；内容不是 pid（空文件 = 抢到锁但还没来得及写 pid 就被杀）却回收 →
    /// 同上。故除「解析出 pid 且判定为死」外一律不回收。
    #[test]
    fn stale_lock_is_reclaimed_only_when_the_owner_is_gone() {
        assert!(
            lock_is_stale("4321\n", |_| false),
            "持有者已死却不回收：SIGKILL/掉电留下的锁文件会把用户永久挡在自己的保险库外"
        );
        assert!(
            !lock_is_stale("4321\n", |_| true),
            "持有者还活着就回收锁 —— 两个实例会同时整文件重写同一个库"
        );
        assert!(!lock_is_stale("", |_| false), "空锁文件不得当作陈旧锁回收");
        assert!(
            !lock_is_stale("not-a-pid", |_| false),
            "内容不可解析时不得回收"
        );
    }

    /// 判活谓词收到的必须是**解析后的 pid**，而不是原始文本里的任何别的数字。
    #[test]
    fn stale_lock_probes_the_pid_written_in_the_file() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let seen = AtomicU32::new(0);
        let stale = lock_is_stale("  90210  \n", |pid| {
            seen.store(pid, Ordering::SeqCst);
            false
        });
        assert!(stale);
        assert_eq!(
            seen.load(Ordering::SeqCst),
            90210,
            "锁文件的 pid 未被正确解析（前后空白/换行必须容忍：写入方带了 \\n）"
        );
    }

    /// `kill(pid, 0)` 判活的真实行为。**只能在 unix 上运行** —— 本仓日常开发环境是
    /// Windows，这条断言在这里连编译都不会发生，其绿灯只能来自 CI 的 unix 作业。
    /// 这正是把判定逻辑抽成上面那条可移植测试的原因。
    #[cfg(unix)]
    #[test]
    fn pid_probe_distinguishes_self_from_a_reaped_child() {
        assert!(pid_is_alive(std::process::id()), "本进程必然活着");
        assert!(
            pid_is_alive(0),
            "pid 0 在 kill(2) 里是「本进程组」，绝不能拿去探活，必须直接判活"
        );
        let mut child = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg("exit 0")
            .spawn()
            .expect("无法拉起子进程");
        let pid = child.id();
        // 必须 wait：僵尸进程尚未被回收，`kill(pid, 0)` 对它仍返回 0（判活）
        child.wait().expect("wait 失败");
        assert!(
            !pid_is_alive(pid),
            "已回收的子进程仍被判为存活 —— 陈旧锁将永不回收（历史实现在非 Linux 上恒判存活）"
        );
    }

    // ── AAD 锚点（P1：记录错位可读到别人的密文）──────────────────────────────

    /// `get`/`list` 的 id 必须取**查表键**，不能取记录自报的 `r.id`。
    ///
    /// 这里刻意在内存里制造错位，绕过 `parse_vault` 那道闸门：两道防线要各自独立可证，
    /// 只测其中一道的话，另一道被删掉时测试照绿。
    #[test]
    fn get_anchors_aad_on_the_map_key_not_the_record_field() {
        let dir = unit_tempdir("aad-anchor");
        let mut s = raw_store(&dir);
        let a = put(&mut s, "a", b"AAA");
        let b = put(&mut s, "b", b"BBB");
        assert_eq!(&*s.get(a).unwrap(), b"AAA", "对照：错位前必须能正常取回");

        // 把 a 的整条记录（含它自报的 id）搬到 b 的键位下 —— 历史实现的 AAD 四个分量
        // 全部读自记录对象，于是它们原封不动跟着搬家，AAD 完全对得上，密文照常解开。
        let moved = s.file.records.remove(&a).unwrap();
        assert_eq!(
            moved.id, a,
            "取材失效：记录自报 id 与键本就不符，本测试会假绿"
        );
        s.file.records.insert(b, moved);

        assert!(
            matches!(s.get(b), Err(Error::Integrity)),
            "AAD 仍锚在记录自报的 id 上：把一条记录挪个键位就能让「读 b」返回 a 的明文"
        );
        assert_eq!(
            s.list().iter().map(|m| m.id).collect::<Vec<_>>(),
            vec![b],
            "list 报的 id 必须是查表键（用户拿它去调 get/delete）"
        );
    }

    // ── vault.json 入口一致性闸门 ────────────────────────────────────────────

    /// 控制组 + 错位拦截。控制组不可省：没有它，下面的断言可能只是在证明
    /// 「serde 往返有损」，而不是「闸门在起作用」。
    #[test]
    fn parse_vault_rejects_a_displaced_record() {
        let dir = unit_tempdir("displaced");
        let mut s = raw_store(&dir);
        let a = put(&mut s, "a", b"AAA");
        let b = put(&mut s, "b", b"BBB");
        let honest = serde_json::to_vec(&s.file).unwrap();
        assert!(
            parse_vault(&honest).is_ok(),
            "原样往返即被拒 —— 本测试的取材有问题"
        );

        // 对调两条记录的键位：max_id 与 next_id 不变，故只可能触发「错位」这一条闸门
        let (ra, rb) = (
            s.file.records.remove(&a).unwrap(),
            s.file.records.remove(&b).unwrap(),
        );
        s.file.records.insert(a, rb);
        s.file.records.insert(b, ra);
        let doctored = serde_json::to_vec(&s.file).unwrap();

        match parse_vault(&doctored).err() {
            Some(Error::Storage(m)) if m.contains("错位") => {}
            other => panic!("错位的 vault.json 未被拒绝：{other:?}"),
        }
    }

    /// `next_id` 倒退是丢更新的典型残迹：放任不管，下一条新记录会复用一个仍在文件里的 id。
    #[test]
    fn parse_vault_rejects_a_rewound_counter() {
        let dir = unit_tempdir("rewound");
        let mut s = raw_store(&dir);
        put(&mut s, "a", b"AAA");
        s.file.next_id = 0;
        let doctored = serde_json::to_vec(&s.file).unwrap();

        match parse_vault(&doctored).err() {
            Some(Error::Storage(m)) if m.contains("倒退") => {}
            other => panic!("next_id 倒退的 vault.json 未被拒绝：{other:?}"),
        }
    }

    // ── 丢更新（P1：基于旧快照的整份覆盖）────────────────────────────────────

    /// 两个实例并存时，基于旧快照的写必须**当场被拒**。
    ///
    /// 闸门（`save_gate`）只保证两次写不交错，拦不住「后写者拿一份不含对方记录的快照
    /// 整份覆盖回去」—— 那才是数据真正消失的地方。
    #[test]
    fn save_refuses_a_stale_snapshot() {
        let dir = unit_tempdir("epoch");
        let mut a = raw_store(&dir);
        let mut b = raw_store(&dir); // 与 a 同代（都基于「空库」这份快照）
        put(&mut a, "a", b"AAA"); // 磁盘推进一代

        let err = b
            .put(
                SecretKind::ApiKey,
                "b".into(),
                Zeroizing::new(b"BBB".to_vec()),
            )
            .unwrap_err();
        match err {
            Error::Storage(m) if m.contains("另一个实例") => {}
            other => panic!("陈旧快照的保存未被拒绝：{other:?}"),
        }
        // 拒绝之后 a 仍应能继续写：世代比对针对的是「过期的那一份」，不是把库整个冻住
        put(&mut a, "a2", b"A2");
    }

    // ── 单实例闸门（审计2 #12）────────────────────────────────────────────────
    //
    // 本节两条测试都**不**调用 `release_process_locks()`：那是个进程全局动作，会把并行跑着的
    // 其他测试的注册表条目一并清掉（Unix 上连它们的锁文件都删掉），于是这里的「清理」会变成
    // 别处的随机失败。每条用例都用自己的纳秒级临时目录，条目留着即可（见 `guard_registry`
    // 对条目数量上界的注记）。

    /// `lock_data_dir` 在启动最早期抢锁之后，保险库自己那次 `vault_guard` 必须仍然拿得到。
    ///
    /// 这是整条修复最容易反噬的地方：抢锁提前了，但保险库解锁时还会再取一次守卫。
    /// 那一发若真去开第二把独占句柄，Windows 上就是共享冲突、Unix 上就是
    /// AlreadyExists 且「pid 还活着」——**用户被自己锁在门外**，症状与修复前那个
    /// macOS 事故（`VaultGuard::drop` 死代码）一模一样，只是原因换了一个。
    ///
    /// 免疫力来自注册表按**规范化路径**做键并返回克隆，故断言的是「同一把」（`Arc::ptr_eq`），
    /// 不只是「都成功」：换成每次新建一个守卫也能让「都成功」这类断言全绿。
    #[test]
    fn lock_data_dir_is_reentrant_so_the_vault_can_still_open_afterwards() {
        let dir = unit_tempdir("reentrant-lock");
        lock_data_dir(&dir).expect("全新目录上的启动期抢锁必须成功");
        lock_data_dir(&dir).expect("重复调用必须幂等：app 侧调一次，但它没有理由是脆的");
        let a = vault_guard(&dir).expect("保险库随后再取守卫必须复用，而不是二次抢锁");
        let b = vault_guard(&dir).expect("复用必须稳定，而不是碰巧第一次");
        assert!(
            Arc::ptr_eq(&a, &b),
            "同一目录必须始终是同一把守卫，否则 save_gate 也就不是同一把锁了"
        );
        // 走一遍真实取值口径：`raw_store` 内部就调 `vault_guard`，回归时这里会直接 panic。
        let s = raw_store(&dir);
        assert!(
            Arc::ptr_eq(&s.lock, &a),
            "Store 持有的必须还是那一把，而不是另开的一把"
        );
    }

    /// 第二个**进程**必须拿不到同一个数据目录（审计2 #12 的前半条修复）。
    ///
    /// 直接调 `VaultGuard::acquire` 绕过进程内注册表——那正是第二个进程会做的事，
    /// 而注册表按定义是进程内的，走它就测不到跨进程语义。真去 fork 一个子进程则要求
    /// 有编译产物，会把这条单测变成一个依赖构建顺序的集成测试。
    ///
    /// 文案一并钉住：惰性抢锁时代第二个实例撞锁的时机是用户**输完主口令之后**，
    /// 报出来的是「解锁失败（密码错误或 keyring/文件异常）」——与真实原因毫不相干，
    /// 用户会去怀疑自己的密码。指向真实原因是这条修复的一半价值。
    #[test]
    fn a_second_instance_cannot_take_the_same_data_dir() {
        let dir = unit_tempdir("second-instance");
        lock_data_dir(&dir).expect("第一个实例必须抢得到");
        match VaultGuard::acquire(&dir) {
            Ok(_) => panic!(
                "同一数据目录被抢到了第二把锁：单实例闸门形同虚设，\
                 两个进程会同时写同一个 fs.db、同一份日志、乃至同一个 .fspart（审计2 #12）"
            ),
            Err(Error::Storage(msg)) => assert!(
                msg.contains("另一个 future-shell 实例"),
                "失败文案必须指向真实原因，实得：{msg}"
            ),
            Err(other) => panic!("失败类型应为 Storage（app 侧只透传它的文案），实得 {other:?}"),
        }
    }
}
