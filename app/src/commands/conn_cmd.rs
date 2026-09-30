use crate::state::AppState;
use base64::Engine; // decode 是 trait 方法，不导入即 E0599（vault_cmd.rs 同款）
use std::sync::Arc;
use tauri::State;

/// 一条读不出来的 profiles 行（审计 P2「坏数据被静默隐藏」）。
///
/// `id` 是库里那一列的**原始文本**而非 `Uuid`：坏行的成因之一恰恰是 id 本身不合法，
/// 强行解析成 `Uuid` 就再也指不出「是哪一行」了（与 `fs_connmgr::BadRow` 同一取舍）。
#[derive(serde::Serialize)]
pub struct BadRowDto {
    pub id: String,
    /// 人可读的失败原因（哪一列、坏在哪），直接展示给用户。
    pub error: String,
}

/// 连接列表返回体（审计 P2 收尾）。
///
/// 形状从裸数组改成对象是**故意破坏**前端契约的：`ProfileRepo::list()` 会跳过坏行，
/// 于是用户在 GUI 上看到的是「我那条生产机配置凭空消失了」——既没有提示，也无从判断
/// 是自己删过还是库坏了。诊断只有真正抵达 UI 才算修好，而裸数组里没有任何位置能承载它，
/// 保留旧形状等于让这条审计项永远停在「已实现但用户看不见」。
///
/// 字段用 camelCase（`badRows`）与事件载荷（`sessionId` 等）一致；`profiles` 元素仍是
/// `fs_connmgr::Profile` 的原生序列化形态（snake_case），前端对它的既有读法不变。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileListResult {
    pub profiles: Vec<fs_connmgr::Profile>,
    /// 空数组是常态；非空时前端必须在列表末尾挂可见提示（UI 规格：不得只进 console）。
    pub bad_rows: Vec<BadRowDto>,
}

/// 列出全部连接配置，**连同读不出来的坏行诊断**（审计 P2）。
///
/// 走 `list_with_diagnostics` 而非 `list`：后者把坏行降级成一条日志就扔了，桌面端用户不看
/// stdout。整表读失败（库损坏/无权限）仍旧上抛 Err——那是「列表拿不到」，与「拿到了但缺几行」
/// 是两回事，不能混成一个空数组。
#[tauri::command]
pub async fn profiles_list(state: State<'_, Arc<AppState>>) -> Result<ProfileListResult, String> {
    let (profiles, bad) = fs_connmgr::ProfileRepo::new(state.db.pool())
        .list_with_diagnostics()
        .await
        .map_err(|e| e.to_string())?;
    Ok(ProfileListResult {
        profiles,
        bad_rows: bad
            .into_iter()
            .map(|b| BadRowDto {
                id: b.id,
                error: b.error,
            })
            .collect(),
    })
}

/// 保存 Profile（upsert）。`id` 由服务端补全：`None` = 新建 → 生成 v4；`Some` = 更新 → 解析后覆盖 `profile.id`。
/// 前端可只提交部分字段：缺省字段（auth/jump/host_key_policy/host_key_pins/env/term/sftp/ai_policy）
/// 在反序列化时经 `#[serde(default)]` 回填默认值——**依赖 Task 5 在 `fs_connmgr::Profile` 对应字段标注 `#[serde(default)]`**。
#[tauri::command]
pub async fn profile_save(
    id: Option<String>,
    profile: fs_connmgr::Profile,
    state: State<'_, Arc<AppState>>,
) -> Result<String, String> {
    // 错误串进文件日志：ipc_log 的边界记账只覆盖「调用点 + 实参」，命令内部失败
    // （校验不过、upsert 报错）此前既不落盘也不可见——用户报「保存报错」时日志是空的。
    let result = profile_save_impl(id, profile, state).await;
    if let Err(e) = &result {
        tracing::error!(target: "future_shell_app::conn", error = %e, "profile_save 失败");
    }
    result
}

async fn profile_save_impl(
    id: Option<String>,
    mut profile: fs_connmgr::Profile,
    state: State<'_, Arc<AppState>>,
) -> Result<String, String> {
    profile.id = match id {
        Some(s) => uuid::Uuid::parse_str(&s).map_err(|e| e.to_string())?,
        None => uuid::Uuid::new_v4(),
    };
    // 审计2 #37：保存入口与 JSON 导入共用 `fs_connmgr::Profile::validate`（同一套规则，
    // 两条入口不可能漂移）。校验在 upsert 之前、uuid 解析之后——先让 id 失败报它的错，
    // 再让字段失败报字段的错，两条文案互不污染。
    profile.validate()?;
    let repo = fs_connmgr::ProfileRepo::new(state.db.pool());
    repo.upsert(&profile).await.map_err(|e| e.to_string())?;
    Ok(profile.id.to_string())
}

/// 连接时输口令的「记住（存入 Vault）」（Task 47）。
///
/// 前端在口令弹框勾选「记住」后调用：把用户现场输入的口令存进 Vault（kind=Password，
/// 标签取该档案的 `user@host`），并回写档案的 `auth.vault_record`——下次连接走既有的
/// vault 取凭据路径，不再弹框。登录本身不依赖这一步（前端先 auth_respond 再调这里，
/// 存失败不阻断登录，只影响「下次要不要重输」）。
#[tauri::command]
pub async fn profile_store_password(
    profile_id: String,
    secret_b64: String,
    state: State<'_, Arc<AppState>>,
) -> Result<u64, String> {
    // 与 vault_put_secret 同口径：base64 编码态同为秘密材料母体，命令入口即刻包入 Zeroizing。
    let secret_b64 = zeroize::Zeroizing::new(secret_b64);
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(secret_b64.as_str())
        .map(zeroize::Zeroizing::new)
        .map_err(|e| e.to_string())?;

    let id = uuid::Uuid::parse_str(&profile_id).map_err(|e| e.to_string())?;
    let repo = fs_connmgr::ProfileRepo::new(state.db.pool());
    let mut profile = repo.get(id).await.map_err(|e| e.to_string())?;

    let label = format!("{}@{}", profile.username, profile.host);
    let record_id = {
        let mut vault = state.vault.lock().await;
        let store = vault.as_mut().ok_or("vault 未解锁")?;
        store
            .put(fs_vault::SecretKind::Password, label, bytes)
            .map_err(|e| e.to_string())?
    };

    profile.auth.vault_record = Some(record_id);
    repo.upsert(&profile).await.map_err(|e| e.to_string())?;
    Ok(record_id)
}

/// 设置/更换**本连接专属**的密码（2026-08-28 凭据归属模型）。
///
/// 与 [`profile_store_password`] 的差别是「换」这个字：那条是连接时弹框勾
/// 「记住」的一次性写入，这条是用户在连接属性里**反复改**的入口。
///
/// # 旧记录必须删掉
///
/// 每次改密码都 `store.put(...)` 新建一条而不管旧的，Vault 会随用户改密码
/// 的次数线性长出孤儿记录：它们谁也不引用、在保险库管理里堆成一列
/// `user@host` 分不清谁是谁、备份体积跟着涨、而且**每一条都是一份还能用的
/// 明文口令**——泄露面白白扩大。
///
/// 所以顺序是：新记录先写成功 → profile 改指向 → 再删旧的。反过来（先删后写）
/// 一旦中间失败，用户的连接就指着一个不存在的记录，密码彻底丢了。
///
/// **只删专属的**：旧记录若在 `shared_credentials` 里（用户把它抽出来共享过），
/// 别的连接可能正在用，此时只解除本连接的引用，记录留着。
#[tauri::command]
pub async fn profile_set_own_password(
    profile_id: String,
    secret_b64: String,
    state: State<'_, Arc<AppState>>,
) -> Result<u64, String> {
    let secret_b64 = zeroize::Zeroizing::new(secret_b64);
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(secret_b64.as_str())
        .map(zeroize::Zeroizing::new)
        .map_err(|e| e.to_string())?;

    let id = uuid::Uuid::parse_str(&profile_id).map_err(|e| e.to_string())?;
    let repo = fs_connmgr::ProfileRepo::new(state.db.pool());
    let mut profile = repo.get(id).await.map_err(|e| e.to_string())?;
    let old = profile.auth.vault_record;

    let label = format!("{}@{}", profile.username, profile.host);
    let record_id = {
        let mut vault = state.vault.lock().await;
        let store = vault.as_mut().ok_or("vault 未解锁")?;
        store
            .put(fs_vault::SecretKind::Password, label, bytes)
            .map_err(|e| e.to_string())?
    };

    profile.auth.vault_record = Some(record_id);
    repo.upsert(&profile).await.map_err(|e| e.to_string())?;

    // 收旧记录（见上：新的已经落定，此刻删旧的是安全的）
    if let Some(old_id) = old {
        if old_id != record_id {
            let shared: Option<String> =
                sqlx::query_scalar("SELECT name FROM shared_credentials WHERE record_id = ?1")
                    .bind(old_id as i64)
                    .fetch_optional(state.db.pool())
                    .await
                    .map_err(|e| e.to_string())?;
            if shared.is_none() {
                // 专属旧记录：删掉，不留孤儿明文
                let mut vault = state.vault.lock().await;
                if let Some(store) = vault.as_mut() {
                    if let Err(e) = store.delete(old_id) {
                        // 删不掉不阻断（新密码已生效），但要留痕——
                        // 静默失败会让孤儿悄悄堆积
                        tracing::warn!(old_id, %e, "旧的专属凭据未能删除，Vault 里会留一条孤儿记录");
                    }
                }
            }
        }
    }
    Ok(record_id)
}

#[cfg(test)]
mod tests {
    //! 「缺省字段 JSON → upsert → 回填默认值」用例：前端只提交常规字段，
    //! 反序列化即补齐其余字段默认值（依赖 Task 5 的 `#[serde(default)]` 契约；upsert 落库由 Task 5 repo 测试覆盖）。

    #[test]
    fn partial_profile_json_backfills_defaults() {
        let json = r#"{
            "id": "00000000-0000-0000-0000-000000000000",
            "name": "web-01", "group_path": null,
            "host": "10.0.0.1", "port": 22, "username": "root"
        }"#;
        let p: fs_connmgr::Profile =
            serde_json::from_str(json).expect("缺省字段应被 serde(default) 回填，反序列化不应失败");
        assert!(p.auth.vault_record.is_none());
        assert!(!p.auth.allow_agent);
        assert!(!p.auth.allow_kbd_interactive);
        assert!(!p.auth.kbd_auto_answer_single);
        assert!(matches!(p.host_key_policy, fs_connmgr::HostKeyPolicy::Tofu)); // 默认 TOFU
        assert!(p.jump.is_empty());
        assert!(p.host_key_pins.is_empty());
        assert!(p.env.is_empty());
    }

    /// P2 契约锁定：`profiles_list` 的返回体是**对象**且必带 `badRows`。
    /// 前端据此渲染「N 条配置无法读取」提示；键名一旦漂移，提示就会静默消失，
    /// 而那正是本条审计要修的「坏数据看不见」原状。
    #[test]
    fn profile_list_result_serializes_with_bad_rows() {
        let v = serde_json::to_value(super::ProfileListResult {
            profiles: vec![],
            bad_rows: vec![super::BadRowDto {
                id: "not-a-uuid".into(),
                error: "auth_blob 解析失败".into(),
            }],
        })
        .unwrap();
        assert!(v.get("profiles").unwrap().is_array());
        let bad = v.get("badRows").expect("必须是 camelCase 的 badRows");
        assert_eq!(bad[0]["id"], "not-a-uuid");
        assert_eq!(bad[0]["error"], "auth_blob 解析失败");
    }

    /// 审计2 #37：`profile_save` 必须走与 JSON 导入同源的 `Profile::validate`。
    ///
    /// 两条入口共用同一套规则是不变量；删掉这一行（或换成别家校验）编译照过、行为全对，
    /// 保存入口随即回到「超限字段直接落库」的原状——所以靠源文件看住，且校验必须在 upsert 之前。
    #[test]
    fn profile_save_validates_before_upsert() {
        let src = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/commands/conn_cmd.rs"),
        )
        .expect("读不到本文件");
        // 针与源码行**整行相等**才计数：断言行自身就写有针的文字，子串匹配会把自己数进去。
        let needle = "profile.validate()?;";
        assert_eq!(
            src.lines().filter(|l| l.trim() == needle).count(),
            1,
            "保存入口必须恰好一处调用 Profile::validate"
        );
        let v = src.find(needle).expect("校验调用点不存在");
        let u = src
            .find("repo.upsert(&profile)")
            .expect("upsert 调用点不存在");
        assert!(v < u, "校验必须发生在 upsert 之前");
    }

    /// P1 契约锁定：`hostkey_known_keys` 回传的 `StoredKey` 线格式。
    ///
    /// ProfileDialog 的钉扎选取器逐字读这四个键，并把其中两个原样写进 `host_key_pins`
    /// 提交回 Rust（`HostKeyPin { key_blob, fingerprint_sha256 }`，两个字段都无 default）。
    /// 键名一旦漂移，两头都不会有编译错误：选取器渲染出一排 `undefined`，钉列表存进去的是
    /// 一条内容为空的钉——而空钉在新的 `decide` 下是**恒硬拒**，用户只会看到「连不上」。
    /// 这条断言是这段跨语言契约唯一的看守。
    #[test]
    fn stored_key_wire_shape_matches_pin_picker() {
        let v = serde_json::to_value(fs_sshengine::hostkey::StoredKey {
            key_type: "ssh-ed25519".into(),
            key_blob: "AAAAC3Nza".into(),
            fingerprint_sha256: "SHA256:abc".into(),
            source: "tofu".into(),
        })
        .unwrap();
        assert_eq!(v["key_type"], "ssh-ed25519");
        assert_eq!(v["key_blob"], "AAAAC3Nza");
        assert_eq!(v["fingerprint_sha256"], "SHA256:abc");
        assert_eq!(v["source"], "tofu");
        // 钉的两个字段必须能从这里直接取到——前端不做任何键名映射
        let pin: fs_connmgr::HostKeyPin = serde_json::from_value(serde_json::json!({
            "key_blob": v["key_blob"], "fingerprint_sha256": v["fingerprint_sha256"],
        }))
        .expect("StoredKey 的两个字段应能原样构成一条 HostKeyPin");
        assert_eq!(pin.key_blob, "AAAAC3Nza");
        assert_eq!(pin.fingerprint_sha256, "SHA256:abc");
    }
}

/// 删除连接。**顺带收走它的专属凭据**（2026-08-28 凭据归属模型）。
///
/// 专属凭据的语义就是「只归这个连接」——连接没了，那条记录再没有任何入口
/// 能引用到它，却仍是一份**还能用的明文口令**躺在保险库里：在管理界面堆成
/// 一列分不清谁是谁的 `user@host`，跟着备份走，泄露面白留着。
///
/// **共享凭据不动**：它可能正被别的连接引用，删了会把那些连接弄坏。判据是
/// `shared_credentials` 登记表——在表里的一律留着，只是本连接不再引用它。
///
/// 删凭据失败不阻断删连接：用户的意图是「这条连接我不要了」，为了一条清理
/// 不掉的记录把主动作也否掉是本末倒置。但要留痕，否则孤儿会悄悄堆积。
#[tauri::command]
pub async fn profile_delete(id: String, state: State<'_, Arc<AppState>>) -> Result<(), String> {
    let uuid = uuid::Uuid::parse_str(&id).map_err(|e| e.to_string())?;
    let repo = fs_connmgr::ProfileRepo::new(state.db.pool());

    // 先取出它引用的记录（删完就查不到了）
    let owned_record = match repo.get(uuid).await {
        Ok(p) => p.auth.vault_record,
        // 查不到（已被别处删掉/坏数据）不影响删除本身
        Err(_) => None,
    };

    repo.delete(uuid).await.map_err(|e| e.to_string())?;

    if let Some(record) = owned_record {
        let shared: Option<String> =
            sqlx::query_scalar("SELECT name FROM shared_credentials WHERE record_id = ?1")
                .bind(record as i64)
                .fetch_optional(state.db.pool())
                .await
                .map_err(|e| e.to_string())?;
        if shared.is_none() {
            let mut vault = state.vault.lock().await;
            match vault.as_mut() {
                Some(store) => {
                    if let Err(e) = store.delete(record) {
                        tracing::warn!(record, %e, "连接已删除，但它的专属凭据没能一并删掉（Vault 里留了一条孤儿记录）");
                    }
                }
                // Vault 锁着：删不了。留痕即可——下次用户解锁后可在保险库
                // 管理里手动清；不为此阻断删除，也不静默。
                None => tracing::info!(
                    record,
                    "连接已删除，其专属凭据因 Vault 未解锁而保留（可在保险库管理里手动删除）"
                ),
            }
        }
    }
    Ok(())
}

/// 导出 Profile 为 JSON 字符串（Task 6 `ProfileRepo::export_json` 的 IPC 出口，i20①；明文结构，敏感字段仅 vault 记录 id——spec §3.1）。
/// `ids = None` → 全量；`Some` → 子集（对全量导出数组按 id 过滤后重新序列化，复用同一序列化路径保证导入导出往返同构）。
#[tauri::command]
pub async fn profiles_export(
    ids: Option<Vec<String>>,
    state: State<'_, Arc<AppState>>,
) -> Result<String, String> {
    let json = fs_connmgr::ProfileRepo::new(state.db.pool())
        .export_json()
        .await
        .map_err(|e| e.to_string())?;
    match ids {
        None => Ok(json),
        Some(want) => {
            let arr: Vec<serde_json::Value> =
                serde_json::from_str(&json).map_err(|e| e.to_string())?;
            let kept: Vec<&serde_json::Value> = arr
                .iter()
                .filter(|v| {
                    v.get("id")
                        .and_then(|i| i.as_str())
                        .map(|i| want.iter().any(|w| w == i))
                        .unwrap_or(false)
                })
                .collect();
            serde_json::to_string_pretty(&kept).map_err(|e| e.to_string())
        }
    }
}

/// 导入 Profile JSON（Task 6 `import_json`：id 冲突重新生成 v4；返回导入条数，前端 Toast「导入 N 条」并刷新列表）。
#[tauri::command]
pub async fn profiles_import(
    content: String,
    state: State<'_, Arc<AppState>>,
) -> Result<usize, String> {
    fs_connmgr::ProfileRepo::new(state.db.pool())
        .import_json(&content)
        .await
        .map_err(|e| e.to_string())
}

/// 导入 OpenSSH known_hosts 文本到信任库（source=imported，spec §2.5 / UI §2.4「主机密钥」页签「导入 known_hosts…」）。
/// `TrustStore::import_known_hosts(&str) -> Result<ImportSummary, _>`；`ImportSummary` 需
/// `serde::Serialize` 以经 IPC 回传前端。
///
/// `ImportSummary` 的每一格都要在前端**分开**措辞，合并会说错话：
///   · `imported` / `upgraded`：信任已建立。`upgraded` 是同主机两行密钥按协商顺序择优保留时
///     被顶替掉的那格（审计 P2），与「真有一把没进来」的 `conflicted` 分开。
///   · `duplicate`：本来就在库里（或已记为吊销），无需任何动作。
///   · `revoked`：`@revoked` 行已落进吊销名单，此后**硬拒**这把密钥。
///   · `cert_authority` / `hashed` / `pattern`：本版本**不支持**的三类 known_hosts 语义，
///     这些行没有进来。必须如实说——审计2 #22 里旧实现把它们与 duplicate 一起塞进一个
///     `skipped`，用户无从分辨「都已经在库里了」和「12 行安全信息被丢了」。
///   · `malformed`：格式层面不成立的行。
#[tauri::command]
pub async fn hostkey_import(
    content: String,
    state: State<'_, Arc<AppState>>,
) -> Result<fs_sshengine::hostkey::ImportSummary, String> {
    fs_sshengine::hostkey::TrustStore::new(state.db.pool())
        .import_known_hosts(&content)
        .await
        .map_err(|e| e.to_string())
}

/// 列出信任库里这台 `host:port` 已记录的主机密钥，供 ProfileDialog 的「添加钉扎指纹」选取。
///
/// 审计 P1 的**可达性**那一半：策略下拉里一直有「指纹钉扎」，但 ProfileDialog 只有 `removePin`
/// 没有添加入口，「导入 known_hosts」写的是全局信任库、不写 profile 钉，`import_json` 还会把钉
/// 清空。于是选了钉扎的用户 pins 恒为空——旧实现下这退化成一个每次连接都弹、点了也不收敛的
/// 「密钥已变更」红框（比 Strict 还弱），修好 `decide` 之后则变成恒硬拒。**只修判定不补入口，
/// 等于把这个选项从「假安全」改成「连不上」**，所以两半必须同时落地。
///
/// 钉的来源刻意**只有信任库**，不接受前端自由填写的指纹：钉是「本机亲眼见过并确认过这把公钥」
/// 的断言，而信任库里的每一行都恰好有这个来源（TOFU 首触时用户点过确认，或用户自己导入的
/// known_hosts）。放开手填等于允许一个从聊天记录里粘来的指纹冒充本机认知——那正是
/// `import_json` 剥离 `host_key_pins` 要挡的同一类事（见 connmgr/repo.rs 的导入剥离不变量）。
///
/// 查不到不是错误：新主机本就还没有记录，前端据空列表提示「先连一次以建立信任」即可。
#[tauri::command]
pub async fn hostkey_known_keys(
    host: String,
    port: u16,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<fs_sshengine::hostkey::StoredKey>, String> {
    fs_sshengine::hostkey::TrustStore::new(state.db.pool())
        .lookup(&host, port)
        .await
        .map_err(|e| e.to_string())
}
