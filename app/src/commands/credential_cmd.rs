//! 连接凭据的归属模型（2026-08-28）。
//!
//! # 默认专属，共享是显式动作
//!
//! Vault 记录此前是一个**全局池**：连接属性里的下拉框把所有记录摊开让用户
//! 挑。那个形状在诱导共享——凭据看起来是「先建池子再挑一条」，而不是
//! 「这个连接自己的密码」。用户要的是反过来（原话）：
//!
//! > 每一个连接我希望都有一个自己的凭据管理，除非另行设置，用户才可以把
//! > 凭据抽出来同步到多个连接设置里
//!
//! 于是：
//! · **专属**（默认）：用户在连接属性里输密码 → 存成一条只归这个连接的
//!   Vault 记录。它不出现在任何其他连接的选择器里。
//! · **共享**：用户对某条专属凭据点「设为共享」并起名 → 它进
//!   `shared_credentials` 登记表，从此其他连接可以引用它。
//!
//! # 为什么归属信息存在应用库而不是 Vault
//!
//! 「是否共享、叫什么名字」是**应用层的组织方式**，不是秘密材料的属性。
//! Vault 的记录格式受 AAD 认证约束（改字段要动加密结构 + 迁移 + 备份兼容）；
//! 而这张登记表泄露了也不泄露任何秘密，丢了也只是回到「都不共享」这个
//! 安全默认。见 migrations/0009_shared_credentials.sql 的记述。
//!
//! # 一致性口径
//!
//! 列表一律拿登记表与 `vault_list_secrets` **求交集**：Vault 里已删的记录
//! 不会因为登记表还留着行而出现在选择器里。反向的孤儿行由 unshare 与
//! profile_delete 清理；漏了也只是一行死数据，不影响正确性。

use crate::state::AppState;
use std::sync::Arc;
use tauri::State;

/// 共享凭据的一条（给选择器用）。
#[derive(serde::Serialize)]
pub struct SharedCredential {
    pub record_id: u64,
    /// 用户起的名字（如「生产机 root」）。**不是** vault 记录的 label——
    /// 那个是自动生成的 `user@host`，对共享场景没有意义。
    pub name: String,
    /// 记录类别（password / private_key / api_key / 未知串）。
    ///
    /// **必须带上**：后端按记录自己声明的类别分流，类别不对一律硬错误、
    /// 不静默降级（审计2 #20）。选择器要在**选之前**就把用不了的置灰——
    /// 否则用户只能在连接时收到一句「已拒绝」，而那时他既看不到类别、
    /// 也不知道该改哪一栏。Vault 未解锁时读不到类别，此处为 None，
    /// 前端按「未知」处理（fail-closed，与它对未知类别的既有口径一致）。
    pub kind: Option<String>,
}

/// 记录类别 → IPC 线上串。
///
/// **必须与 `vault_list_secrets` 的映射逐字一致**：前端用同一套
/// `usableAsCredential` 判定可用性，两处串不一样就会出现「同一条记录在
/// 一个选择器里能选、在另一个里置灰」这种没人能理解的现象。
fn kind_wire_tag(k: fs_vault::SecretKind) -> &'static str {
    match k {
        fs_vault::SecretKind::Password => "password",
        fs_vault::SecretKind::PrivateKey => "private_key",
        fs_vault::SecretKind::ApiKey => "api_key",
    }
}

/// 列出可被其他连接引用的凭据。
///
/// 只列**显式共享**的那些：专属凭据（用户在某个连接里输的密码）不在此列，
/// 这正是「默认每个连接一份自己的」的兑现处。
#[tauri::command]
pub async fn credentials_shared_list(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<SharedCredential>, String> {
    let rows: Vec<(i64, String)> =
        sqlx::query_as("SELECT record_id, name FROM shared_credentials ORDER BY name")
            .fetch_all(state.db.pool())
            .await
            .map_err(|e| e.to_string())?;

    // 与 Vault 求交集：库里已经删掉的记录不该还出现在选择器里
    // （选了它，连接时会得到一句「vault record N 不存在」，而用户
    //  完全不知道自己选的是个幽灵）。
    // 类别与存活一次取齐（同一把锁下）：类别用于选择器置灰，存活用于
    // 过滤幽灵记录。Vault 未解锁时两者都拿不到——此时**返回登记的全部**
    // 而不是空列表（空列表会让用户以为共享凭据丢了），类别记为 None。
    let meta: Option<std::collections::HashMap<u64, String>> = {
        crate::commands::vault_cmd::touch_vault_activity();
        state.vault.try_lock().ok().and_then(|guard| {
            guard.as_ref().map(|store| {
                store
                    .list()
                    .into_iter()
                    .map(|m| (m.id, kind_wire_tag(m.kind).to_string()))
                    .collect()
            })
        })
    };

    Ok(rows
        .into_iter()
        .filter(|(id, _)| meta.as_ref().is_none_or(|m| m.contains_key(&(*id as u64))))
        .map(|(record_id, name)| SharedCredential {
            record_id: record_id as u64,
            name,
            kind: meta
                .as_ref()
                .and_then(|m| m.get(&(record_id as u64)).cloned()),
        })
        .collect())
}

/// 把一条凭据「抽出来」共享给其他连接。
///
/// 幂等：已经共享过的再点一次只是改名。
#[tauri::command]
pub async fn credential_share(
    record_id: u64,
    name: String,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    let name = name.trim();
    if name.is_empty() {
        // 名字是共享凭据在**别处**的唯一身份（其他连接的选择器里只看得到它）。
        // 空名字会让选择器出现一个无法辨认的条目——那比不让共享更糟。
        return Err("请给这条共享凭据起个名字（其他连接的选择器里只显示这个名字）".into());
    }
    sqlx::query(
        "INSERT INTO shared_credentials (record_id, name) VALUES (?1, ?2)
         ON CONFLICT(record_id) DO UPDATE SET name = excluded.name",
    )
    .bind(record_id as i64)
    .bind(name)
    .execute(state.db.pool())
    .await
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// 取消共享。**不删 Vault 记录**——它可能还被某个连接当作专属凭据在用。
///
/// 取消之后，其他连接的选择器里不再出现它；已经引用了它的连接**保持原样**
/// （引用的是 vault 记录 id，不是登记表）。这是刻意的：取消共享不该把别人
/// 正在用的连接弄坏，那是「删除凭据」才该有的后果。
#[tauri::command]
pub async fn credential_unshare(
    record_id: u64,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    sqlx::query("DELETE FROM shared_credentials WHERE record_id = ?1")
        .bind(record_id as i64)
        .execute(state.db.pool())
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// 某条记录是不是共享的（连接属性打开时用来决定单选停在哪一档）。
#[tauri::command]
pub async fn credential_is_shared(
    record_id: u64,
    state: State<'_, Arc<AppState>>,
) -> Result<Option<String>, String> {
    let name: Option<String> =
        sqlx::query_scalar("SELECT name FROM shared_credentials WHERE record_id = ?1")
            .bind(record_id as i64)
            .fetch_optional(state.db.pool())
            .await
            .map_err(|e| e.to_string())?;
    Ok(name)
}
