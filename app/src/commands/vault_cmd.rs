use crate::state::AppState;
use base64::Engine;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tauri::{Emitter, State};
use zeroize::Zeroizing;

/// Vault 最后一次被使用的时刻（UNIX 秒；审计 P1-16）。
///
/// 缺陷原状：SettingsDialog 写 `vault.autoLockMinutes`，代码库里**没有任何读取方**——
/// 用户在设置里选了「5 分钟后自动锁定」，Vault 实际永不锁定。这是安全承诺与真实行为不一致，
/// 比根本没有这个选项更危险：用户会据此放心地把机器留在解锁状态离开工位。
///
/// 放在模块内的 `static` 而非 `AppState`：`AppState` 由并行改动方持有，跨文件加字段会冲突；
/// 而「最后活动时刻」进程内全局唯一（Vault 本就只有一把），全局量与事实同构，不损失语义。
static LAST_ACTIVITY: AtomicU64 = AtomicU64::new(0);

/// 自动锁定巡检周期。取 20 s 而非「按剩余时间精确定时」：设置可随时改、活动时刻随时刷新，
/// 精确定时器要为每次变化重排一次，复杂度全用在把误差从 20 s 压到 0 上——而这个功能的
/// 语义本就是「大约 N 分钟没动就锁」，多等不到半分钟没有安全意义上的差别。
const AUTO_LOCK_POLL: std::time::Duration = std::time::Duration::from_secs(20);

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 记一次 Vault 活动（审计 P1-16）。所有会读写 Vault 的路径都要打这一下：
/// vault_* 命令自不必说，`VaultSecrets::secret()`（认证时按需取密）同样算——
/// 漏掉它会让一场持续几分钟的交互式认证被自动锁定从中间截断。
pub fn touch_vault_activity() {
    LAST_ACTIVITY.store(now_secs(), Ordering::Relaxed);
}

/// 解析 `vault.autoLockMinutes` 的持久值：0 / 缺省 / 解析失败一律返回 None（= 不自动锁定）。
///
/// 两种编码都认，不是为了宽容而宽容——前端 `settingSet` 走 `JSON.stringify(value)`，
/// 而 SettingsDialog 里这一项绑的是 `<select>`（值天然是字符串），于是库里落的是 `"5"`
/// 这种**带引号的 JSON 字符串**而非裸数字 `5`；只认 `Value::Number` 会让界面上选的每一个
/// 时长都被读成「未配置」，功能整个失效且毫无报错。裸串分支则兜历史写入。
///
/// 全部失败路径都收敛到 None（不锁）而非某个默认时长：这里的失败意味着「不知道用户要什么」，
/// 替用户擅自选一个时长会让 Vault 在用户没要求过的时刻锁掉，正在进行的认证被从中间截断。
fn parse_auto_lock_minutes(raw: &str) -> Option<u64> {
    let minutes = serde_json::from_str::<serde_json::Value>(raw)
        .ok()
        .and_then(|v| match v {
            serde_json::Value::Number(n) => n.as_u64(),
            serde_json::Value::String(s) => s.trim().parse::<u64>().ok(),
            _ => None,
        })
        .or_else(|| raw.trim().parse::<u64>().ok())?;
    (minutes > 0).then_some(minutes)
}

/// 闲置是否已达自动锁定阈值。
///
/// `saturating_mul` 而非 `minutes * 60`：`minutes` 直接来自设置库里的 u64，没有任何上界。
/// 界面上那个 `<select>` 给不出大值，但 `settings_set` 是通用 IPC（任意键任意串），
/// 库文件也可被手工编辑或损坏，于是 `minutes` 取到 `u64::MAX / 60` 以上是可达的。
/// 一旦溢出，两种构建各错各的、且**都无声**：
///   - debug（`cargo test` 走的就是它）：算术溢出 panic。panic 发生在 `spawn` 出去的任务里，
///     任务就地死掉，主流程一无所知——自动锁定在本进程余下的生命里彻底停摆，
///     而设置界面照旧显示「5 分钟后锁定」。
///   - release（overflow-checks 默认关）：回绕成一个小数，阈值几乎必然小于 idle，
///     于是每轮巡检都锁一次，用户刚解锁就被锁，且看不出原因。
///
/// 饱和后阈值为 `u64::MAX`，`idle` 永远小于它 → 判为「不锁」。这与设置项自身的
/// 「0 = 从不」同向：用户填了个荒谬的大数，其意图最接近的解释就是「别锁」。
fn should_auto_lock(idle_secs: u64, minutes: u64) -> bool {
    idle_secs >= minutes.saturating_mul(60)
}

/// 读 `vault.autoLockMinutes`（解析见 [`parse_auto_lock_minutes`]）。
async fn auto_lock_minutes(state: &AppState) -> Option<u64> {
    let raw = fs_connmgr::SettingsRepo::new(state.db.pool())
        .get("vault.autoLockMinutes")
        .await
        .ok()??;
    parse_auto_lock_minutes(&raw)
}

/// 自动锁定巡检任务（审计 P1-16）：由 `lib.rs` 的 setup 启动一次，随进程存活。
///
/// 每轮只做三件事：Vault 没解锁 → 跳过；设置为 0/缺省 → 跳过；闲置超时 → 置回 None 并
/// emit `vault:locked`。设置每轮重读，用户在 UI 上改完即刻生效，不需要重启。
///
/// 锁定是**幂等**的：即使与手动 `vault_lock` 撞上也只是重复置 None。事件照发——
/// 前端据此关掉可能开着的凭据选取器并提示「已自动锁定」，否则用户会看到一堆「vault 未解锁」
/// 的报错却不知道发生了什么。
pub fn spawn_auto_lock_watcher(state: Arc<AppState>) {
    touch_vault_activity(); // 以启动时刻起算，避免进程刚起就因 LAST_ACTIVITY=0 判成「闲置了 55 年」
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(AUTO_LOCK_POLL).await;
            let Some(minutes) = auto_lock_minutes(&state).await else {
                continue;
            };
            let idle = now_secs().saturating_sub(LAST_ACTIVITY.load(Ordering::Relaxed));
            if !should_auto_lock(idle, minutes) {
                continue;
            }
            let mut guard = state.vault.lock().await;
            if guard.is_none() {
                continue; // 本就锁着，不重复发事件
            }
            *guard = None;
            drop(guard);
            tracing::info!(idle_secs = idle, minutes, "Vault 闲置超时，已自动锁定");
            let _ = state.app.emit(
                "vault:locked",
                serde_json::json!({ "reason": "auto_lock", "idleMinutes": minutes }),
            );
        }
    });
}

#[tauri::command]
pub async fn vault_status(state: State<'_, Arc<AppState>>) -> Result<bool, String> {
    Ok(state.vault.lock().await.is_some())
}

#[tauri::command]
pub async fn vault_init(
    passphrase: Option<String>,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    // spec §3.2（用毕即擦）：口令入参于命令入口即刻包入 Zeroizing，各返回路径随 drop 清零（兑现 L764 类型审计委托「app 层命令侧自行约束」）
    let passphrase = passphrase.map(Zeroizing::new);
    let store =
        fs_vault::Store::open_or_create(&state.data_dir, passphrase.as_ref().map(|p| p.as_str()))
            .map_err(|e| e.to_string())?;
    *state.vault.lock().await = Some(store);
    // 审计 P1-16：Vault 被真实使用，刷新闲置计时
    touch_vault_activity();
    Ok(())
}

#[tauri::command]
pub async fn vault_unlock(
    passphrase: Option<String>,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    // spec §3.2（用毕即擦）：口令入参于命令入口即刻包入 Zeroizing，各返回路径随 drop 清零（兑现 L764 类型审计委托「app 层命令侧自行约束」）
    let passphrase = passphrase.map(Zeroizing::new);
    // R112：无口令路径按「库文件是否已存在」二分——已存在 → `unlock_with_keyring`（只读打开，
    // keyring 条目缺失即 KeyringMissing，绝不铸新主密钥覆写既有密文）；不存在 → `open_or_create` 首启建库。
    // 二者语义互斥，混用即静默数据毁灭（详见 fs_vault::Store::unlock_with_keyring 文档注）。
    let store = match passphrase {
        Some(p) => fs_vault::Store::unlock_with_passphrase(&state.data_dir, &p),
        None if state.data_dir.join("vault.json").exists() => {
            fs_vault::Store::unlock_with_keyring(&state.data_dir)
        }
        None => fs_vault::Store::open_or_create(&state.data_dir, None),
    }
    .map_err(|e| e.to_string())?;
    *state.vault.lock().await = Some(store);
    // 审计 P1-16：Vault 被真实使用，刷新闲置计时
    touch_vault_activity();
    Ok(())
}

/// 启动时的**静默解锁**（2026-08-31 用户裁定：解锁框默认不该出现）。
///
/// # 为什么要有这条
///
/// 没设应用口令的库，主密钥就在系统 keyring 里——`unlock_with_keyring` 一步就
/// 打开，不需要用户做任何事。可此前没有人自动调它：用户想用凭据 → 发现锁着 →
/// 点状态栏锁图标 → 看到一个写着「应用密码（未设置则留空）」的框 → 留空 →
/// 点解锁。一个**对他毫无意义**的框，每次启动都要走一遍。
///
/// # 两类用户分开对待（这条是本函数的全部要点）
///
/// · **设过应用口令** → 本函数**什么都不做**，返回 `Ok(false)`。用户先前明确
///   要过「Vault 当作应用锁」，自动解开就是把他自己要的东西拆掉。
///   **注意**（2026-09-01 对抗审计核实）：今天的「应用锁」并不是一道真闸——
///   `Store::unlock_with_keyring` 不读 `passphrase_phc`，解锁框里留空点「解锁」
///   就能打开设过密码的库（现存绿测 tests/backup_restore_keyring.rs:76-86 正
///   给这个行为背书）。本函数不自动解开设过口令的库，是**不去扩大**那个缺口，
///   不等于那道锁是严的。补真闸是行为变更，需产品裁定，不混在本次改动里；
/// · **没设** → `unlock_with_keyring` 静默解开，返回 `Ok(true)`。
///
/// 判据取 `fs_vault::file_needs_passphrase`（读库文件头，不解锁、不碰 keyring）——
/// 现成的 `Store::has_passphrase` 是 `&self`，要先有解开的 Store 才问得到，
/// 用在这里是鸡生蛋。
///
/// # 失败一律静默
///
/// keyring 条目不在（换了机器、被凭据管理器清理、Linux 上 Secret Service 没起）
/// 时返回 `Ok(false)`，**不报错、不弹框**：这是一次「顺手试试」，失败只意味着
/// 回到原来的手动路径；把它做成硬错误，等于让每个 keyring 不可用的环境在启动
/// 时吃一个看不懂的红框。真正需要凭据的时候，认证链路自己会弹框问
/// （见 connect.rs 的降级路径）。
///
/// **绝不建库**：库文件不存在时直接返回 `Ok(false)`。`unlock_with_keyring` 与
/// `open_or_create` 语义互斥，混用会铸新主密钥覆写既有密文（store.rs 的文档
/// 用「静默数据毁灭」形容它）——本函数只走前者，一步都不越界。
#[tauri::command]
pub async fn vault_auto_unlock(state: State<'_, Arc<AppState>>) -> Result<bool, String> {
    auto_unlock_inner(&state).await
}

/// [`vault_auto_unlock`] 的主体。抽出来是因为**启动路径也要用**：Tauri 命令的
/// `State<'_, Arc<AppState>>` 拿不到裸 `&AppState`，而 setup 里只有 Arc。
/// 两条路走同一份逻辑，不各写一遍（各写一遍就会分叉，而分叉的那一半没人测）。
pub async fn auto_unlock_inner(state: &AppState) -> Result<bool, String> {
    // 已经解开了：无事可做（重复调用安全——启动路径与前端都可能调）
    if state.vault.lock().await.is_some() {
        return Ok(true);
    }
    // 库不存在 → 不建，交给用户第一次存凭据时走 setup
    if !state.data_dir.join("vault.json").exists() {
        return Ok(false);
    }
    match fs_vault::file_needs_passphrase(&state.data_dir) {
        // 设过口令：用户的意图是「要有一道锁」，本函数就不去替他开
        // （那道锁今天挡得住多少是另一回事，见本函数文档的注意事项）
        Ok(true) => Ok(false),
        Ok(false) => match fs_vault::Store::unlock_with_keyring(&state.data_dir) {
            Ok(store) => {
                *state.vault.lock().await = Some(store);
                touch_vault_activity();
                tracing::info!("Vault 已静默解锁（该库未设应用口令，主密钥取自系统 keyring）");
                Ok(true)
            }
            Err(e) => {
                // keyring 拿不到主密钥：回到手动路径，不打扰用户
                tracing::info!("Vault 静默解锁未成功（{e}）——回落到手动解锁");
                Ok(false)
            }
        },
        Err(e) => {
            // 库文件坏了/读不出：fail-closed，交给手动路径去报真正的错
            tracing::warn!("读取 Vault 口令状态失败（{e}）——不做静默解锁");
            Ok(false)
        }
    }
}

#[tauri::command]
pub async fn vault_put_secret(
    kind: String,
    label: String,
    secret_b64: String,
    state: State<'_, Arc<AppState>>,
) -> Result<u64, String> {
    // 总设计 §3.2「用毕即擦」/ Global Constraints：base64 编码态同为秘密材料母体（可直接解码还原），
    // 只擦解码产物会留编码母体在堆上——命令入口即刻包入 Zeroizing，各返回路径随 drop 清零。
    // 与同文件 vault_init / vault_unlock 的口令路径同口径（L763 类型审计委托之对偶）。
    let secret_b64 = Zeroizing::new(secret_b64);
    // 解码产物同样即刻受保护，见 `decode_secret` 的文档注（审计 P2：此前它是裸 Vec，
    // 活过了下面的「vault 未解锁」早退与 `state.vault.lock().await` 这个取消点）。
    let bytes = decode_secret(&secret_b64)?;
    // 审计2 #20：认不出的类别串**当场拒绝**，不再 `_ => ApiKey` 静默回落。
    // 类别决定这份材料日后被拿去干什么（口令上网、私钥解码、还是根本不给 SSH 用），
    // 把一个拼错的串悄悄变成一个具体用途，等于让输入错误改写安全语义。
    let kind = fs_vault::SecretKind::from_aad_tag(&kind).ok_or_else(|| {
        format!(
            "未知的凭据类别 {kind:?}；可用值：{}",
            fs_vault::SecretKind::ALL.map(|k| k.aad_tag()).join("、")
        )
    })?;
    // 审计 P1-16：Vault 被真实使用，刷新闲置计时
    touch_vault_activity();
    let mut vault = state.vault.lock().await;
    let store = vault.as_mut().ok_or("vault 未解锁")?;
    store.put(kind, label, bytes).map_err(|e| e.to_string())
}

/// base64 解码为**受保护**的明文字节（审计 P2「vault_put_secret 明文未清零」）。
///
/// 返回 `Zeroizing<Vec<u8>>` 而非裸 `Vec<u8>`：原实现让解码产物在裸 `Vec` 里活过了一个 `?`
/// ——`vault.as_mut().ok_or("vault 未解锁")?`。而「vault 未解锁」不是罕见分支：手动 `vault_lock`、
/// 闲置自动锁定（`spawn_auto_lock_watcher`）都会置 None，ProfileDialog 的保存按钮在调用前又不查
/// vault 状态，于是锁定态下每点一次「保存」，堆上就留下一份完整的私钥明文。
/// 此外 `state.vault.lock().await` 是个 await 点，命令 future 在此被取消同样只 drop 不清零。
///
/// 把 `Zeroizing` 提到解码处，上述全部早退与取消路径的清零由 Drop 一次性覆盖，
/// 不再依赖「记得在每条返回路径上手动擦」——那种依赖迟早会被下一次改动破坏。
/// 失败路径无需额外处理：base64 的 `DecodeError` 只带下标与那一个非法符号，不含解码产物。
fn decode_secret(secret_b64: &str) -> Result<Zeroizing<Vec<u8>>, String> {
    base64::engine::general_purpose::STANDARD
        .decode(secret_b64)
        .map(Zeroizing::new)
        .map_err(|e| e.to_string())
}

/// 凭据元数据 DTO（i3②：`RecordMeta` 不经 IPC 直出 core 边界——Task 4 L605 注记之对偶实现；永不回 secret 材料，spec §3.2）。
#[derive(serde::Serialize)]
pub struct RecordMetaDto {
    pub id: u64,
    pub kind: String,
    pub label: String,
}

/// 列出凭据元数据（ProfileDialog 认证页签选取器数据源；kind 映射为小写串，与 `vault_put_secret` 入参对偶）。
#[tauri::command]
pub async fn vault_list_secrets(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<RecordMetaDto>, String> {
    // 审计 P1-16：Vault 被真实使用，刷新闲置计时
    touch_vault_activity();
    let guard = state.vault.lock().await;
    let store = guard.as_ref().ok_or("vault 未解锁")?;
    Ok(store
        .list()
        .into_iter()
        // 出边界的串取 `aad_tag()` 本身，而不是又抄一份 match：抄的那份与 `from_aad_tag`
        // 迟早对不上，而对不上的现象是「列表里显示的类别」与「保存时认的类别」分叉。
        .map(|m| RecordMetaDto {
            id: m.id,
            kind: m.kind.aad_tag().to_string(),
            label: m.label,
        })
        .collect())
}

/// 锁定 Vault（i14②：此后 vault_put_secret / vault_list_secrets / vault_copy_to_clipboard 均回「vault 未解锁」）。
#[tauri::command]
pub async fn vault_lock(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    *state.vault.lock().await = None;
    Ok(())
}

/// vault.json 是否存在（供前端判定 VaultDialog 呈现 setup 首启向导还是 unlock 密码输入；不泄露敏感信息）。
#[tauri::command]
pub fn vault_has_file(state: State<'_, Arc<AppState>>) -> bool {
    state.data_dir.join("vault.json").exists()
}

/// 库**文件**有没有设应用密码——**不要求已解锁**（对偶 [`vault_has_passphrase`]：那条要先有
/// 解开的 Store，而需要问这个问题的时刻恰恰是解不开的时刻）。
///
/// 给解锁框用（路线图 4c「keyring 条目缺失时解锁框要说真话」，2026-09-02）：解锁失败时，
/// 「没设口令 + keyring 条目不在」= 库里凭据永久不可恢复，与「设了口令、留空走不通」是两件事，
/// 文案必须分开——分开的依据只能从库文件头读，那正是 `fs_vault::file_needs_passphrase` 做的事
/// （不碰 keyring、不解密文）。
///
/// 返回 `None` 表示还没有库文件；读不出/解析不了按错误上抛（前端据此退回通用文案，不下结论）。
#[tauri::command]
pub fn vault_file_has_passphrase(state: State<'_, Arc<AppState>>) -> Result<Option<bool>, String> {
    if !state.data_dir.join("vault.json").exists() {
        return Ok(None);
    }
    fs_vault::file_needs_passphrase(&state.data_dir)
        .map(Some)
        .map_err(|e| e.to_string())
}

/// 删除一条凭据（审计2 #27）。
///
/// `Store::delete` 早就写好了，只是从来没接到 IPC 上——于是用户存错一条私钥、
/// 或者某台服务器下线之后，那份密文就永远躺在 vault.json 里，界面上没有任何入口
/// 能把它拿掉。「存得进、删不掉」对一个凭据管理器是结构性的缺陷，不是缺个按钮。
#[tauri::command]
pub async fn vault_delete_secret(
    record_id: u64,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    // 审计 P1-16：Vault 被真实使用，刷新闲置计时
    touch_vault_activity();
    let mut vault = state.vault.lock().await;
    let store = vault.as_mut().ok_or("vault 未解锁")?;
    store.delete(record_id).map_err(|e| e.to_string())
}

/// 这份库有没有设应用密码（审计2 #28）。
///
/// 没设 = 主密钥**只**在本机 OS 凭据库里，条目一旦消失（重装、换机、凭据管理器被清理）
/// 全部凭据永久解不开，且没有任何补救手段。UI 必须问得到这件事，才能在用户还来得及的
/// 时候把话说清楚，而不是等他真的换机器时才发现。
#[tauri::command]
pub async fn vault_has_passphrase(state: State<'_, Arc<AppState>>) -> Result<bool, String> {
    let vault = state.vault.lock().await;
    let store = vault.as_ref().ok_or("vault 未解锁")?;
    Ok(store.has_passphrase())
}

/// 设置或修改应用密码（审计2 #27「轮换」/ #28「补设」）。
///
/// `current` 为 `None` 表示「本来就没有」——`Store::change_passphrase` 会自己核对这一点：
/// 库里已有口令而调用方不给旧口令，它返回 `Locked`，不会让人绕过验证直接改密。
/// 这里刻意不在 app 层再判一次，那会变成两处口径，而两处口径迟早分叉。
#[tauri::command]
pub async fn vault_change_passphrase(
    current: Option<String>,
    new_passphrase: String,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    // spec §3.2（用毕即擦）：两个口令入参于命令入口即刻包入 Zeroizing，与 vault_init 同口径
    let current = current.map(Zeroizing::new);
    let new_passphrase = Zeroizing::new(new_passphrase);
    if new_passphrase.is_empty() {
        return Err("新的应用密码不能为空".into());
    }
    // 审计 P1-16：Vault 被真实使用，刷新闲置计时
    touch_vault_activity();
    let mut vault = state.vault.lock().await;
    let store = vault.as_mut().ok_or("vault 未解锁")?;
    store
        .change_passphrase(current.as_ref().map(|p| p.as_str()), &new_passphrase)
        .map_err(|e| e.to_string())
}

/// 备份目录的绝对路径（供 UI 显示，让用户知道去哪儿把备份拷走）。
///
/// 取 `state.data_dir` + `fs_vault::BACKUP_DIR` 而不是另写一个字面量：这个目录名
/// 在 core 侧是备份读写的唯一依据，两边各写一份迟早对不上，而对不上的现象是
/// 「界面告诉你去 A 目录找，文件却写在 B 目录」。
#[tauri::command]
pub fn vault_backup_dir(state: State<'_, Arc<AppState>>) -> String {
    state
        .data_dir
        .join(fs_vault::BACKUP_DIR)
        .to_string_lossy()
        .into_owned()
}

/// 导出一份备份，返回文件名（审计2 #27 / 发布门槛第 5 条）。
#[tauri::command]
pub async fn vault_create_backup(state: State<'_, Arc<AppState>>) -> Result<String, String> {
    // 审计 P1-16：Vault 被真实使用，刷新闲置计时
    touch_vault_activity();
    let vault = state.vault.lock().await;
    let store = vault.as_ref().ok_or("vault 未解锁")?;
    let name = store.export_backup().map_err(|e| e.to_string())?;
    // 只记文件名，不记路径内容也不记任何库内容
    tracing::info!(backup = %name, "保险库备份已导出");
    Ok(name)
}

/// 备份条目 DTO（与 `RecordMetaDto` 同一口径：core 类型不经 IPC 直出边界）。
#[derive(serde::Serialize)]
pub struct BackupEntryDto {
    pub name: String,
    pub bytes: u64,
    pub modified_unix: u64,
}

/// 列出可恢复的备份。
///
/// **不要求已解锁**：需要恢复的时刻，恰恰就是打不开库的时刻。要求先解锁等于把
/// 这条恢复路径锁在了它自己要解决的那个问题后面。列表里只有文件名与大小，
/// 不含任何库内容。
#[tauri::command]
pub fn vault_list_backups(state: State<'_, Arc<AppState>>) -> Result<Vec<BackupEntryDto>, String> {
    fs_vault::Store::list_backups(&state.data_dir)
        .map(|v| {
            v.into_iter()
                .map(|b| BackupEntryDto {
                    name: b.name,
                    bytes: b.bytes,
                    modified_unix: b.modified_unix,
                })
                .collect()
        })
        .map_err(|e| e.to_string())
}

/// 用备份顶替现有保险库；返回被顶替那份库的留档文件名（原本没有库时为 `None`）。
///
/// 同样**不要求已解锁**（理由同 `vault_list_backups`）。成功之后必须把内存里那份
/// `Store` 丢掉并广播 `vault:locked`：它持有的是恢复前的快照，留着不但读到的是旧内容，
/// 下一次 `put` 还会被丢更新比对拒绝，用户看到的是一条莫名其妙的「已被改写」——
/// 而那其实是我们自己没收拾干净。
#[tauri::command]
pub async fn vault_restore_backup(
    name: String,
    passphrase: String,
    state: State<'_, Arc<AppState>>,
) -> Result<Option<String>, String> {
    // spec §3.2（用毕即擦）：口令入参于命令入口即刻包入 Zeroizing，与 vault_init 同口径
    let passphrase = Zeroizing::new(passphrase);
    touch_vault_activity();
    // 全程持有这把锁：恢复期间不许别的 vault 命令插进来（它们手里的快照正在作废），
    // 也不许两次恢复并发。
    let mut vault = state.vault.lock().await;
    let displaced = fs_vault::Store::restore_backup(&state.data_dir, &name, &passphrase)
        .map_err(|e| e.to_string())?;
    *vault = None;
    drop(vault);
    let displaced = displaced.and_then(|p| {
        p.file_name()
            .and_then(|n| n.to_str())
            .map(|s| s.to_string())
    });
    tracing::info!(
        backup = %name,
        displaced = displaced.as_deref().unwrap_or("<无>"),
        "保险库已从备份恢复，内存中的解锁状态已作废"
    );
    let _ = state
        .app
        .emit("vault:locked", serde_json::json!({ "reason": "restored" }));
    Ok(displaced)
}

/// 复制凭据到剪贴板（spec §3.2/§6.2；F19）：明文只在 core 进程内短暂存在——
/// core 侧解密单条 → 写系统剪贴板 → 立即 zeroize 明文 → 定时清除（设置键
/// `security.clipboardClear` = `{"enabled":true,"seconds":30}`：默认开、时长可配、
/// 整项可关，逐字对齐总设计 §3.2 / UI 规格 §2.12）仅凭内容哈希比对清除。
/// 明文永不进 IPC、永不进 tracing、永不出 core 进程；前端只传 record_id。
#[tauri::command]
pub async fn vault_copy_to_clipboard(
    record_id: u64,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    use zeroize::Zeroize;
    // 审计 P1-16：Vault 被真实使用，刷新闲置计时
    touch_vault_activity();
    // ① core 侧解密单条记录（Zeroizing，离开作用域即擦除）
    //    R114：明文必须由 `Zeroizing<String>` 而非裸 `String` 持有——③ 处两个 `?` 早退时，
    //    裸 String 的默认 drop 不清零，尾部的手动 `text.zeroize()` 又走不到，明文会留在堆上
    //    （剪贴板初始化失败/写入失败是可触发的常规错误路径，非罕见分支）。
    //
    //    注意 `Zeroizing::new` **不能**包在整个块外面：块内的 `?` 直接从函数返回，
    //    构造器压根没被调用，保护不到任何东西——这正是审计 P2 在原实现里点出的洞。
    //    改为块内自己产出 `Zeroizing<String>`，每条早退路径都在保护之下。
    let text: Zeroizing<String> = {
        let guard = state.vault.lock().await;
        let store = guard.as_ref().ok_or("vault 未解锁")?;
        let secret = store.get(record_id).map_err(|e| e.to_string())?; // Zeroizing<Vec<u8>>
        secret_to_clipboard_text(&secret)?
        // secret（Zeroizing）随块结束 drop → 解密字节即擦
    };
    // ② 仅持有写入内容的 SHA-256 用于后续比对（定时任务不持明文）
    let expected = sha256_hex(text.as_bytes());
    // ③ 写系统剪贴板（任一 `?` 早退亦随 text 的 Zeroizing drop 清零）
    {
        let mut cb = arboard::Clipboard::new().map_err(|e| e.to_string())?;
        cb.set_text(text.as_str()).map_err(|e| e.to_string())?;
    }
    // ④ 明文即刻释放：Zeroizing 的 Drop 保证清零，显式 drop 只为缩短驻留窗口
    drop(text);
    // ⑤ 定时清除（总设计 §3.2 / UI 规格 §2.12：默认开、默认 30s 时长可配、整项可关）。
    //    设置键 `security.clipboardClear` = JSON {"enabled":true,"seconds":30}；禁用则不起定时器（本次复制照常）。
    //    清除时重读剪贴板算哈希，仍等于所写内容才清除（避免覆盖用户后续复制）
    let cfg = {
        let repo = fs_connmgr::SettingsRepo::new(state.db.pool());
        // `.ok().flatten()`：读库出错与「键不存在」在此合流，都交给 parse 的安全默认。
        // 二者确实该同等对待——两种情况下我们同样不知道用户配了什么，而未知时的正确行为只有一个。
        parse_clipboard_clear(
            repo.get("security.clipboardClear")
                .await
                .ok()
                .flatten()
                .as_deref(),
        )
    };
    if !cfg.enabled {
        return Ok(());
    }
    let clip_seconds = cfg.seconds;
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(clip_seconds)).await;
        if let Ok(mut cb) = arboard::Clipboard::new() {
            if let Ok(mut current) = cb.get_text() {
                let same = sha256_hex(current.as_bytes()) == expected;
                current.zeroize();
                if same {
                    let _ = cb.clear();
                }
            }
        }
    });
    Ok(())
}

/// 把凭据字节转成可写进剪贴板的文本，明文全程处于 `Zeroizing` 之下（审计 P2）。
///
/// 关键在于**借用校验**。`String::from_utf8` 要按值吃掉一个 `Vec`，失败时 `FromUtf8Error`
/// 原样持有它并按默认 Drop 释放（不清零）；原实现还先 `secret.to_vec()` 复制了一份**不受任何
/// 保护**的明文交给它，于是每一次失败调用都在堆上留下一份未擦的私钥。
/// `str::from_utf8` 只借 `&[u8]`：失败时 `Utf8Error` 里只有下标，明文一个字节都不会离开 `secret`
/// （而 `secret` 本身是调用方的 `Zeroizing`）。
///
/// 错误串是自拟的领域说法，不是 std 那句「invalid utf-8 sequence of N bytes from index M」：
/// 这条路径的真实含义是「这条凭据是二进制（多半是 DER 私钥），本就不该往文本剪贴板里放」，
/// std 那句话会把用户引向「文件编码坏了」。串里刻意不带任何原文字节与下标——它要经 IPC 回前端，
/// 还可能被用户截图贴进工单。
///
/// 也**不能**改用 `String::from_utf8_lossy` 把它糊过去：那会把私钥里的非法字节替换成 U+FFFD
/// 再放进剪贴板，用户拿到的是一份悄悄损坏、却看不出损坏的密钥。
fn secret_to_clipboard_text(secret: &[u8]) -> Result<Zeroizing<String>, String> {
    let text = std::str::from_utf8(secret)
        .map_err(|_| "该凭据不是文本（可能是二进制私钥），无法复制到剪贴板".to_string())?;
    Ok(Zeroizing::new(text.to_owned()))
}

/// 剪贴板定时清除配置（设置键 `security.clipboardClear`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ClipboardClear {
    enabled: bool,
    seconds: u64,
}

/// 未配置 / 配置损坏时的取值：**开启、30 秒**（总设计 §3.2 / UI 规格 §2.12 的规定默认）。
const CLIPBOARD_CLEAR_DEFAULT: ClipboardClear = ClipboardClear {
    enabled: true,
    seconds: 30,
};

/// `seconds` 的上界（1 小时）。
///
/// 无上界时这个值会原样进 `tokio::time::sleep(Duration::from_secs(..))`，而 tokio 1.53.1 的
/// `sleep` 对**溢出值不 panic**：
/// ```text
/// match Instant::now().checked_add(duration) {
///     Some(deadline) => Sleep::new_timeout(deadline, location),
///     None => Sleep::new_timeout(Instant::far_future(), location),   // ← 悄悄换成"永不"
/// }
/// ```
/// 于是一个越界的 `seconds` 的后果是：定时清除**永远不触发**，而设置界面上那个复选框仍然
/// 显示为开启。复制出去的口令 / 私钥无限期留在系统剪贴板里，任何能读剪贴板的进程都拿得走——
/// 正是本功能存在的理由。不必到溢出量级也一样：`seconds` 取一年就已经等价于"永不"。
/// 这是"安全承诺与实际行为不符"，与审计 P1-16（autoLockMinutes 只写不读）同族。
///
/// 可达性同 [`should_auto_lock`] 的论证：界面上那个 `<select>` 最大只给到 300 秒，但
/// `settings_set` 是通用 IPC（任意键任意串），设置库文件也可被手工编辑或损坏。
///
/// 越界取**钳到上界**而非退回默认 30 秒：写了个大数的用户，其意图最接近的解释是"尽量久一点"，
/// 钳位顺着这个方向、同时把暴露窗口约束回可执行的范围。要彻底关掉有明确的开关
/// （`enabled: false`，界面上就是那个复选框），不必靠一个荒谬的时长来表达。
/// 1 小时是界面最大预设（300 秒）的 12 倍，留足余量，且与 `crates/terminal/src/flow.rs`
/// 的 `MAX_BATCH_INTERVAL` 取同一量级——那里对 tokio 的同一个饱和陷阱已经做过一次钳位。
const CLIPBOARD_CLEAR_MAX_SECONDS: u64 = 3600;

/// 解析 `security.clipboardClear` 的持久值。
///
/// 每一条失败路径都落到「开启」而不是「关闭」，这是刻意的**失败方向**选择：
/// 失败落到关闭，用户复制出来的口令/私钥就无限期躺在系统剪贴板里，任何一个能读剪贴板的
/// 进程（浏览器页面里的 `paste` 事件、剪贴板历史工具、远程协助软件）都拿得走——
/// 这正是本功能存在的理由，而配置读不出来时恰恰是最该保守的时候。
/// 失败落到开启，最坏后果是把用户想留着的一段剪贴板内容清掉；而且连这个都不会发生——
/// 清除前会重算哈希，只有内容仍是本次写进去的那份才动手（见 `vault_copy_to_clipboard` ⑤）。
/// 一边是不可挽回的凭据泄露，一边是可挽回的小麻烦。
///
/// `seconds` 过滤掉 0：0 秒意味着写进剪贴板的同一瞬间就清掉，用户根本来不及粘贴——
/// 功能表现为「复制没反应」，而这不可能是任何人的本意。要关就用 `enabled: false`
/// （界面上就是那个复选框），语义明确且可回读。
/// 上界同理但方向相反，理由见 [`CLIPBOARD_CLEAR_MAX_SECONDS`]：过大等价于「永不清除」，
/// 而这个失败方向恰好是本功能要防的那件事。
fn parse_clipboard_clear(raw: Option<&str>) -> ClipboardClear {
    let Some(cfg) = raw.and_then(|v| serde_json::from_str::<serde_json::Value>(v).ok()) else {
        return CLIPBOARD_CLEAR_DEFAULT;
    };
    ClipboardClear {
        enabled: cfg
            .get("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(CLIPBOARD_CLEAR_DEFAULT.enabled),
        seconds: cfg
            .get("seconds")
            .and_then(|v| v.as_u64())
            .filter(|s| *s > 0)
            .map(|s| s.min(CLIPBOARD_CLEAR_MAX_SECONDS))
            .unwrap_or(CLIPBOARD_CLEAR_DEFAULT.seconds),
    }
}

/// SHA-256 十六进制摘要（仅用于剪贴板内容比对，非安全边界）。
fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    digest.iter().fold(String::with_capacity(64), |mut acc, b| {
        use std::fmt::Write;
        let _ = write!(&mut acc, "{b:02x}");
        acc
    })
}

// 测试模块置于文件末尾：clippy 的 `items_after_test_module` 会把「测试模块之后还有产品代码」
// 判为错误——那种排布下新加的函数很容易被误写进 `#[cfg(test)]` 的作用域，不参与发布构建。
#[cfg(test)]
mod tests {
    use super::*;

    /// 明文一进本进程就必须交给 `Zeroizing`，往后所有早退（vault 未解锁 / await 点被取消）
    /// 的清零由 Drop 覆盖。**清零本身不可测**：读已释放堆页是 UB，没有确定性的观测手段——
    /// 由 `decode_secret` 的返回类型在编译期保证。这里能钉的是「解码结果逐字正确」，
    /// 保证上提 `Zeroizing` 没有顺手改坏语义。
    #[test]
    fn decode_secret_roundtrips_private_key_bytes() {
        let pem = b"-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaA==\n";
        let b64 = base64::engine::general_purpose::STANDARD.encode(pem);
        let out = decode_secret(&b64).expect("合法 base64 必须能解码");
        assert_eq!(&out[..], &pem[..]);
    }

    /// 非法 base64 报错而非 panic / 静默截断（前端可能发来任意串）。
    #[test]
    fn decode_secret_rejects_malformed_base64() {
        assert!(decode_secret("这不是 base64").is_err());
    }

    /// 文本凭据照常放行，且内容逐字不变——复制进剪贴板的必须与库里的一致，
    /// 多一个字节少一个字节都是「用户拿这份口令登不上去」。
    #[test]
    fn secret_to_clipboard_text_passes_utf8_through_verbatim() {
        let out = secret_to_clipboard_text("p@ssw0rd·口令 🙂".as_bytes()).expect("合法 UTF-8");
        assert_eq!(&*out, "p@ssw0rd·口令 🙂");
    }

    /// 二进制凭据（DER 私钥等）必须**报错**，绝不能 lossy 转换后照写剪贴板：
    /// 那会把非法字节替换成 U+FFFD 再放进去，用户拿到一份悄悄损坏、却看不出损坏的密钥。
    #[test]
    fn secret_to_clipboard_text_rejects_binary_instead_of_lossy_converting() {
        let der = [0x30u8, 0x82, 0x04, 0xa4, 0x02, 0x01, 0x00, 0xff, 0xfe];
        let err = secret_to_clipboard_text(&der).expect_err("二进制凭据必须报错");
        assert!(err.contains("不是文本"), "错误串应说明原因，实得：{err}");
    }

    /// 错误串会经 IPC 回到前端、进 toast、可能被截图贴进工单，里面**一个明文字节都不许有**。
    /// 钉的是一类很自然的「顺手加点调试信息」改动：`format!("{e}: {secret:?}")` 之类。
    #[test]
    fn secret_to_clipboard_text_error_carries_no_plaintext() {
        // 前缀是合法 UTF-8 且足够特征化，末尾接一个非法字节触发失败
        let mut secret = b"SUPER-SECRET-KEY-MATERIAL".to_vec();
        secret.push(0xff);
        let err = secret_to_clipboard_text(&secret).expect_err("非 UTF-8 必须报错");
        assert!(
            !err.contains("SUPER-SECRET-KEY-MATERIAL"),
            "错误串泄露了明文：{err}"
        );
        // `{:?}` 打字节切片会是十进制数组（…, 255），`{:x?}` 则是 ff——两种写法都堵上
        assert!(
            !err.contains("255") && !err.contains("ff"),
            "错误串泄露了原始字节：{err}"
        );
    }

    // ── vault.autoLockMinutes（审计 P1-16）────────────────────────────────
    // P1-16 的整改把「设置项只写不读」接上了后端巡检，但整改本身此前零测试覆盖：
    // 一旦解析这段被改坏，界面照旧显示「5 分钟后锁定」，Vault 实际永不锁定，
    // 而这个偏差**没有任何可观测信号**——用户只有在机器被人动过之后才会知道。

    /// 前端真实写入的形态：`settingSet` 走 `JSON.stringify(value)`，而这一项在
    /// SettingsDialog 里绑的是 `<select>`（值是字符串），落库即 `"5"`（含引号）。
    /// 这条是整个功能的主路径——它一断，界面上选的每个时长都被读成「未配置」。
    #[test]
    fn auto_lock_accepts_the_quoted_string_the_frontend_actually_writes() {
        assert_eq!(parse_auto_lock_minutes("\"5\""), Some(5));
        assert_eq!(parse_auto_lock_minutes("\"30\""), Some(30));
    }

    /// 裸数字与裸串（历史写入/手工编辑）同样要认。
    ///
    /// `"05"` 这一条是专门用来压住 `or_else` 那个兜底分支的：`15` 和 ` 15 ` 本身就是合法 JSON
    /// （serde_json 允许前后空白），走的仍是 `Value::Number`，**根本到不了 or_else**。
    /// 真正只有 or_else 才认的是「不是合法 JSON、但 Rust 能 parse 成 u64」的形态——
    /// JSON 数字禁止前导零，而手工编辑配置写成 `05` 是很自然的事。
    /// 没有这一条，那个分支就是无人覆盖的死代码，删掉它也不会有任何测试变红。
    #[test]
    fn auto_lock_accepts_bare_number_and_bare_string() {
        assert_eq!(parse_auto_lock_minutes("15"), Some(15));
        assert_eq!(parse_auto_lock_minutes(" 15 "), Some(15));
        assert_eq!(parse_auto_lock_minutes("05"), Some(5));
    }

    /// 0 = 从不锁定，是界面上真实存在的选项（`<option value="0">`），不是异常值。
    /// 若把 0 当成「解析出了 0 分钟」而按 0 秒阈值走，用户选了「从不」反而变成**立即锁定**——
    /// 语义完全颠倒，且是用户主动选择的那一项出问题。
    #[test]
    fn auto_lock_zero_means_never_not_immediately() {
        assert_eq!(parse_auto_lock_minutes("0"), None);
        assert_eq!(parse_auto_lock_minutes("\"0\""), None);
    }

    /// 无法解析 → None（不锁），而不是替用户挑一个时长。
    #[test]
    fn auto_lock_unparseable_means_never() {
        for raw in [
            "", "abc", "\"abc\"", "{}", "[]", "null", "true", "-5", "1.5",
        ] {
            assert_eq!(parse_auto_lock_minutes(raw), None, "输入：{raw:?}");
        }
    }

    /// 阈值判定的常规刻度：边界取 `>=`，恰好到点即锁。
    #[test]
    fn should_auto_lock_at_and_after_the_threshold() {
        assert!(!should_auto_lock(299, 5), "4分59秒不该锁");
        assert!(should_auto_lock(300, 5), "恰好 5 分钟就该锁");
        assert!(should_auto_lock(301, 5));
    }

    /// **溢出**：`minutes` 无上界地来自设置库，`minutes * 60` 会溢出。
    /// debug 构建下算术溢出直接 panic，而这段跑在 `spawn` 出去的巡检任务里——
    /// 任务就地死掉，主流程毫无察觉，自动锁定在本进程余下的生命里彻底停摆。
    /// release 构建（overflow-checks 默认关）则回绕成一个小阈值，变成每轮都锁。
    /// 本用例同时钉住这两种失效：`saturating_mul` 下阈值饱和为 u64::MAX，判为不锁。
    #[test]
    fn should_auto_lock_saturates_instead_of_overflowing() {
        assert!(!should_auto_lock(u64::MAX - 1, u64::MAX));
        assert!(!should_auto_lock(3600, u64::MAX / 59));
        // 饱和的边界情形：idle 取到 u64::MAX 时与饱和阈值相等，按 `>=` 判为锁。
        // 这不是缺陷——u64::MAX 秒的闲置（约 5850 亿年）不可能出现，
        // 写在这里只为固定住行为，防止后来者把 `>=` 改成 `>` 时以为无人依赖。
        assert!(should_auto_lock(u64::MAX, u64::MAX));
    }

    // ── security.clipboardClear（总设计 §3.2 / UI 规格 §2.12）────────────

    /// 前端真实写入的形态：`JSON.stringify({enabled, seconds})`。
    #[test]
    fn clipboard_clear_reads_what_the_frontend_writes() {
        assert_eq!(
            parse_clipboard_clear(Some(r#"{"enabled":true,"seconds":60}"#)),
            ClipboardClear {
                enabled: true,
                seconds: 60
            }
        );
        assert_eq!(
            parse_clipboard_clear(Some(r#"{"enabled":false,"seconds":15}"#)),
            ClipboardClear {
                enabled: false,
                seconds: 15
            }
        );
    }

    /// 用户显式关掉（复选框取消）必须被尊重——这是「整项可关」的规格承诺。
    /// 与下面那组「读不出来就默认开」不同：这里读得出来，且读到的就是「关」。
    #[test]
    fn clipboard_clear_respects_an_explicit_disable() {
        assert!(!parse_clipboard_clear(Some(r#"{"enabled":false,"seconds":30}"#)).enabled);
    }

    /// 缺省 / 损坏 / 类型不对 —— 一律**开启**。
    /// 方向是安全性的核心：落到关闭意味着口令无限期留在系统剪贴板里，
    /// 而配置读不出来恰恰是最该保守的时刻。逐个列出真实会遇到的破损形态。
    #[test]
    fn clipboard_clear_fails_towards_enabled_on_every_broken_input() {
        let broken = [
            None,                             // 键不存在（首次运行）/ 读库出错
            Some(""),                         // 空值
            Some("not json"),                 // 损坏
            Some("null"),                     // JSON 合法但不是对象
            Some("[]"),                       // 同上
            Some(r#""{\"enabled\":false}""#), // 双重编码：整体是字符串，get() 取不到键
            Some(r#"{"seconds":45}"#),        // 缺 enabled
            Some(r#"{"enabled":"false"}"#),   // enabled 是字符串而非 bool
            Some(r#"{"enabled":0}"#),         // enabled 是数字
        ];
        for raw in broken {
            assert!(
                parse_clipboard_clear(raw).enabled,
                "破损配置必须落到「开启」，输入：{raw:?}"
            );
        }
    }

    /// 时长破损时落到 30 秒（规格默认），不是 0、不是无穷。
    #[test]
    fn clipboard_clear_falls_back_to_thirty_seconds() {
        for raw in [
            r#"{"enabled":true}"#,                // 缺 seconds
            r#"{"enabled":true,"seconds":0}"#,    // 0 秒 = 写进去同刻即清，来不及粘贴
            r#"{"enabled":true,"seconds":-1}"#,   // 负数
            r#"{"enabled":true,"seconds":1.5}"#,  // 小数
            r#"{"enabled":true,"seconds":"30"}"#, // 字符串
            r#"{"enabled":true,"seconds":null}"#,
        ] {
            assert_eq!(parse_clipboard_clear(Some(raw)).seconds, 30, "输入：{raw}");
        }
    }

    /// 关闭态下的 seconds 仍要解析出来：用户取消勾选后 SettingsDialog 会把
    /// `{enabled:false, seconds:<原值>}` 整个写回，重新勾选时要能回读到原来的时长。
    /// 若这里把关闭态的 seconds 归零/丢弃，用户的时长设置会在每次开关切换后被悄悄重置。
    #[test]
    fn clipboard_clear_keeps_seconds_while_disabled() {
        assert_eq!(
            parse_clipboard_clear(Some(r#"{"enabled":false,"seconds":300}"#)).seconds,
            300
        );
    }

    /// 越界时长必须钳到上界，否则「定时清除」显示为开启而实际永不触发。
    /// 界面预设范围内的值不得被这道钳位动到——钳位若顺手改了 300 秒，用户选的 5 分钟
    /// 会变成别的数，而这种"修好了但顺手改坏了旁边一格"没有任何别的断言拦得住。
    #[test]
    fn clipboard_clear_clamps_absurd_durations() {
        for (raw, want) in [
            (r#"{"enabled":true,"seconds":18446744073709551615}"#, 3600), // u64::MAX
            (r#"{"enabled":true,"seconds":31536000}"#, 3600),             // 一年 ≈ 永不
            (r#"{"enabled":true,"seconds":3601}"#, 3600),                 // 刚过界
            (r#"{"enabled":true,"seconds":3600}"#, 3600),                 // 边界本身不动
            (r#"{"enabled":true,"seconds":300}"#, 300),                   // 界面最大预设
            (r#"{"enabled":true,"seconds":15}"#, 15),                     // 界面最小预设
        ] {
            assert_eq!(
                parse_clipboard_clear(Some(raw)).seconds,
                want,
                "输入：{raw}"
            );
        }
    }

    /// 钳位为什么是这个上界：钳完的值必须是计时器**真能表示**的时刻。
    ///
    /// 这条钉的是因果链本身，不是复述常量。tokio 1.53.1 的 `sleep` 在
    /// `Instant::now().checked_add(d)` 返回 `None` 时不 panic，而是换成 `Instant::far_future()`
    /// ——即"永不"。所以判据是 `checked_add` 有没有值：越界输入原样传下去会落进 `None` 那条
    /// 分支（定时清除静默失效），钳位后的值必须落进 `Some`。
    #[test]
    fn clamped_duration_is_representable_but_the_raw_one_is_not() {
        let now = std::time::Instant::now();
        assert!(
            now.checked_add(std::time::Duration::from_secs(u64::MAX))
                .is_none(),
            "u64::MAX 秒若还能表示，那 tokio 就不会走 far_future 分支，本钳位的前提不成立"
        );
        let clamped =
            parse_clipboard_clear(Some(r#"{"enabled":true,"seconds":18446744073709551615}"#))
                .seconds;
        assert!(
            now.checked_add(std::time::Duration::from_secs(clamped))
                .is_some(),
            "钳位后仍不可表示 = 清除依旧永不触发，钳了等于没钳"
        );
    }

    /// 结构钉：定时器的时长必须来自 `parse_clipboard_clear` 的产物，不能绕过钳位另取一路。
    ///
    /// 为什么只能钉结构：`vault_copy_to_clipboard` 要活的 `AppState`（含 sqlite 池）和真实
    /// 系统剪贴板，单元测试里起不来；上面那批用例钉的是**解析器**，解析器再对也挡不住调用点
    /// 改成直接读原始 JSON。这条钉的就是那一步——它是本次修复唯一没有行为测试覆盖的一环。
    #[test]
    fn the_clipboard_timer_takes_its_duration_from_the_parsed_config() {
        let src = include_str!("vault_cmd.rs");
        let body = {
            let start = src
                .find("pub async fn vault_copy_to_clipboard(")
                .expect("函数改名了：这条钉子已失去锚点，请同步更新而不是删掉");
            let rest = &src[start..];
            // 顶层 `}` 顶格出现，即函数结束。
            &rest[..rest.find("\n}\n").expect("函数体没有顶格收尾") + 2]
        };
        assert_eq!(
            body.matches("parse_clipboard_clear(").count(),
            1,
            "复制路径必须且只能经由解析器拿配置"
        );
        assert_eq!(
            body.matches("let clip_seconds = cfg.seconds;").count(),
            1,
            "时长必须取自解析出的 cfg，取原始值就绕过了上界钳位"
        );
        assert_eq!(
            body.matches("sleep(").count(),
            1,
            "复制路径只该有一处定时；多一处就有一处没走钳位"
        );
        assert!(
            body.contains("tokio::time::sleep(std::time::Duration::from_secs(clip_seconds)).await"),
            "定时器的实参必须是 clip_seconds 本身，中间不得再算一道"
        );
    }

    /* --------------------------------------------------------------- *
     * M1 出口「Vault：…锁定后 secret 不可读」的 **IPC 那一半**
     * --------------------------------------------------------------- */

    /// 本文件源码。锁定态的语义是 `state.vault == None`，而它变成用户可见行为，
    /// 靠的是每条读写秘密的命令自己写一句 `.ok_or("vault 未解锁")?`——
    /// 那是**逐条重复的人工纪律**，加一条新命令时漏掉不会有任何东西报错。
    /// **只取生产段**：在 `#[cfg(test)]` 处截断。否则本模块自己的断言文本
    /// （比如下面那张文案变体清单）会被当成生产代码扫进来，门禁自己把自己判红。
    fn production_src() -> &'static str {
        const RAW: &str = include_str!("vault_cmd.rs");
        let cut = RAW
            .find(
                "
#[cfg(test)]",
            )
            .expect("本文件必须有 #[cfg(test)] 段");
        &RAW[..cut]
    }

    /// 把源码切成「一条 `#[tauri::command]` 到下一条之间」的块。
    fn command_blocks() -> Vec<(String, &'static str)> {
        let mut out = Vec::new();
        let marker = "#[tauri::command]";
        let src = production_src();
        let starts: Vec<usize> = src.match_indices(marker).map(|(i, _)| i).collect();
        for (n, &s) in starts.iter().enumerate() {
            let e = starts.get(n + 1).copied().unwrap_or(src.len());
            let block = &src[s..e];
            // 取函数名：块内第一处 `fn <name>`
            let name = block
                .find("fn ")
                .map(|i| {
                    block[i + 3..]
                        .chars()
                        .take_while(|c| c.is_alphanumeric() || *c == '_')
                        .collect::<String>()
                })
                .unwrap_or_default();
            out.push((name, block));
        }
        out
    }

    /// 取出块里 `state.vault` 那把锁的绑定名（无绑定即 None）。
    fn vault_guard_binding(block: &str) -> Option<String> {
        let i = block.find("= state.vault.lock()")?;
        let name: String = block[..i]
            .trim_end()
            .rsplit(|c: char| c.is_whitespace())
            .next()?
            .to_string();
        (!name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_')).then_some(name)
    }

    /// 凡是从 `state.vault` 里把 `Store` 取出来用的命令，都必须带解锁守卫。
    ///
    /// 反过来不成立、也不该成立：`vault_status` / `vault_has_file` / `vault_lock` /
    /// `vault_init` / `vault_unlock` / `vault_backup_dir` / `vault_list_backups` /
    /// `vault_restore_backup` 都**必须**在锁定态下可用（否则用户锁上之后连解锁和
    /// 从备份恢复都做不了）。所以判据不是「人人有守卫」，而是「碰 Store 的都有」。
    #[test]
    fn every_command_that_touches_the_store_has_the_unlock_guard() {
        let blocks = command_blocks();
        // 做空防护①：解析器失效（切不出块）时下面的循环零轮，恒绿
        assert!(
            blocks.len() >= 15,
            "只解析出 {} 条 #[tauri::command]，解析器已失效（本文件当前有 15 条）",
            blocks.len()
        );

        let mut touching = Vec::new();
        for (name, block) in &blocks {
            // 绑定变量名不统一（有的 `let mut vault`、有的 `let guard`），而块里还有别的
            // Option（`passphrase.as_ref()`），所以既不能按变量名认、也不能按 `.as_ref()` 认
            // ——vault_init 正是这样被误判过。改为**先取出这个块里 vault 锁的绑定名**，
            // 再看有没有对**那个名字**拆 Option：
            //   `let mut vault = state.vault.lock().await;` → 绑定名 vault
            //   `*state.vault.lock().await = None;`         → 无绑定名（vault_lock/init/unlock）
            //   vault_restore_backup 有绑定名但从不拆 Option（只把整个解锁态置 None）
            let binds_store = vault_guard_binding(block).is_some_and(|g| {
                block.contains(&format!("{g}.as_mut()")) || block.contains(&format!("{g}.as_ref()"))
            });
            if !binds_store {
                continue;
            }
            touching.push(name.clone());
            assert!(
                block.contains(r#"ok_or("vault 未解锁")"#),
                "命令 `{name}` 取用了内存中的 Store 却没有解锁守卫：\
                 锁定态下它会拿着 None 走下去（unwrap/panic 或读到陈旧句柄），\
                 「锁定后 secret 不可读」在这一条上不成立"
            );
        }

        // 做空防护②：识别器失效（比如 Store 的取用改成了别的写法）时上面同样零轮
        assert!(
            touching.len() >= 7,
            "只认出 {} 条取用 Store 的命令（当前应有 7 条：{:?}）——识别器已与实现脱节，\
             此门禁已不再检查任何东西",
            touching.len(),
            touching
        );

        // 做空防护③：反向——必须确实存在**不带**守卫的命令，否则「碰 Store 的才要守卫」
        // 这条规则退化成「人人都要」，而那会把 vault_unlock 自己也判死
        assert!(
            blocks.len() > touching.len(),
            "所有命令都被判为取用 Store：识别器过宽，规则已失去区分力"
        );
    }

    /// 守卫的错误串是**前端判据**：VaultDialog 靠它决定弹解锁框还是报错。
    /// 各写各的（"vault未解锁" / "Vault 未解锁" / "locked"）会让前端只认得其中一种。
    #[test]
    fn the_unlock_guard_message_is_one_single_string() {
        let n = production_src().matches(r#"ok_or("vault 未解锁")"#).count();
        assert!(n >= 7, "解锁守卫只出现 {n} 次，取材已失效");
        // 任何别的大小写/空格变体都不许出现
        for variant in ["vault未解锁", "Vault 未解锁", "vault 未 解锁"] {
            assert!(
                !production_src().contains(variant),
                "出现了解锁守卫文案的变体「{variant}」：前端按单一文案判据，变体会漏判"
            );
        }
    }
}
