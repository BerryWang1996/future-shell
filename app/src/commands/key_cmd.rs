//! 密钥 / 代理管理器（M4b 出口第 13 项）。
//!
//! 出口原文：「密钥列表浏览（**仅指纹/类型/注释**）+ agent 传输开关生效，
//! 单测断言私钥材料不出现在 UI 数据载荷」。
//!
//! # 那条「不出现在载荷里」怎么保证
//!
//! 不靠序列化时过滤，靠**类型**：`fs_sshengine::keyinfo::KeyInfo` 只有三个 `String`，
//! 装不下密钥材料。于是「私钥泄漏到 UI」在这条路径上不是一个需要小心避免的错误，
//! 而是一件写不出来的事——过滤式的做法只要有人加个字段就会破，且破时没有信号。
//!
//! 本文件里唯一碰得到真实材料的地方是 `describe_private_key(&pem, None)` 那一行，
//! 它的返回类型就是上面那个三字段结构；`pem` 是个局部变量，出了那一行就没了。

use crate::state::AppState;
use fs_sshengine::keyinfo::{describe_private_key, KeyInfo, KeyInfoError};
use serde::Serialize;
use std::sync::Arc;
use tauri::State;

// 「agent 传输开关」**不在这里**，也不该在这里。
//
// 起草时这个文件里有过一个 `ssh.useAgent` 全局 settings 键。它是错的：Profile 上已经有
// `auth.allow_agent`（ProfileDialog 的勾选框 → `allow_agent` → `CredentialSetBuilder::agent()`
// → `Method::Agent` 进候选），而再加一个全局开关只会制造一个没人答得上来的问题——
// 全局关、这个 Profile 开，到底用不用 agent？
//
// 出口原文说的「agent 传输开关生效」由那条既有的链承担，本文件末尾有一条守卫钉住它：
// 那条链跨 4 个文件，此前没有任何东西在看它，而断在中间任何一处的表现都是
// 「agent 里明明有可用密钥却总是登不上」——不报错、不告警。

/// 列表里的一条。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KeyEntry {
    /// 来源：`vault` 或 `agent`。
    ///
    /// 两者的处置完全不同——Vault 里的删得掉，agent 里的要去 `ssh-add -d`。
    /// 不分来源的话，用户会在列表里找一个删不掉的条目的删除按钮。
    pub source: String,
    /// Vault 记录 id（agent 来源为 `None`）。
    pub id: Option<u64>,
    /// Vault 里的标签（agent 来源为 `None`，它只有注释）。
    pub label: Option<String>,
    /// `SHA256:...`。读不出来时为 `None`（见 `status`）。
    pub fingerprint: Option<String>,
    pub algorithm: Option<String>,
    pub comment: Option<String>,
    /// `ok` / `encrypted` / `not-a-key`。
    ///
    /// **`encrypted` 不是错误**：加密私钥本来就该是加密的。界面上要显示成一个
    /// 中性的「已加密」而不是红色失败——把正常状态画成失败，会让用户去修一件没坏的事。
    pub status: String,
}

impl KeyEntry {
    fn from_vault(id: u64, label: String, info: Result<KeyInfo, KeyInfoError>) -> Self {
        match info {
            Ok(k) => Self {
                source: "vault".into(),
                id: Some(id),
                label: Some(label),
                fingerprint: Some(k.fingerprint),
                algorithm: Some(k.algorithm),
                comment: Some(k.comment),
                status: "ok".into(),
            },
            Err(e) => Self {
                source: "vault".into(),
                id: Some(id),
                label: Some(label),
                fingerprint: None,
                algorithm: None,
                comment: None,
                status: match e {
                    KeyInfoError::Encrypted => "encrypted",
                    KeyInfoError::NotAKey => "not-a-key",
                }
                .into(),
            },
        }
    }

    fn from_agent(k: KeyInfo) -> Self {
        Self {
            source: "agent".into(),
            id: None,
            label: None,
            fingerprint: Some(k.fingerprint),
            algorithm: Some(k.algorithm),
            comment: Some(k.comment),
            status: "ok".into(),
        }
    }
}

/// 一次列举的结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KeyListing {
    pub keys: Vec<KeyEntry>,
    /// agent 不可用时的原因（一句给人看的话）。可用时为 `None`。
    ///
    /// **不把它变成空列表**：空列表会被读成「agent 在跑但里面没有密钥」，
    /// 那是完全不同的处境——前者要去启动 agent，后者要去 `ssh-add`。
    pub agent_error: Option<String>,
    /// Vault 是否已解锁。锁着时 `keys` 里没有 vault 来源的条目。
    ///
    /// 同理不静默：一个锁着的 Vault 与一个空 Vault 在列表上长得一样，
    /// 而用户会以为自己的密钥丢了。
    pub vault_locked: bool,
}

/// 列出所有密钥的可公开信息。
#[tauri::command]
pub async fn key_list(state: State<'_, Arc<AppState>>) -> Result<KeyListing, String> {
    let mut keys = Vec::new();

    // ── Vault 侧 ──
    let vault_locked = {
        let guard = state.vault.lock().await;
        match guard.as_ref() {
            None => true,
            Some(store) => {
                for m in store.list() {
                    // 只看私钥。密码与 API key 不属于「密钥管理器」，
                    // 混进来只会让这个列表变成第二个 Vault 管理界面。
                    if m.kind != fs_vault::SecretKind::PrivateKey {
                        continue;
                    }
                    // 材料只在这一小段里存在：解出来 → 立刻换成三字段结构 → 丢弃。
                    // `Zeroizing` 出作用域时擦除。
                    let info = match store.get(m.id) {
                        Ok(bytes) => match std::str::from_utf8(&bytes) {
                            Ok(pem) => describe_private_key(pem, None),
                            // 不是 UTF-8 ⇒ 不可能是 PEM。这里**不打印**任何字节。
                            Err(_) => Err(KeyInfoError::NotAKey),
                        },
                        // 取不出来（记录损坏）与「不是密钥」在界面上是同一件事：
                        // 这一条读不了。区分它们要把 Vault 的内部错误摊给用户，没有价值。
                        Err(_) => Err(KeyInfoError::NotAKey),
                    };
                    keys.push(KeyEntry::from_vault(m.id, m.label, info));
                }
                false
            }
        }
    };

    // ── agent 侧 ──
    //
    // agent 探测有超时（sshengine 侧的 AGENT_PROBE_TIMEOUT），所以这里不会挂死。
    // 但它确实会花几百毫秒——放在 Vault 之后，让列表的主体先算出来。
    let agent_error = match fs_sshengine::agent::list_agent_keys().await {
        Ok(list) => {
            keys.extend(list.into_iter().map(KeyEntry::from_agent));
            None
        }
        Err(e) => Some(e.to_string()),
    };

    Ok(KeyListing {
        keys,
        agent_error,
        vault_locked,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info() -> KeyInfo {
        KeyInfo {
            fingerprint: "SHA256:abc".into(),
            algorithm: "ssh-ed25519".into(),
            comment: "me@box".into(),
        }
    }

    /// **出口点名的那一条**，在 IPC 这一层再钉一遍。
    ///
    /// `keyinfo.rs` 已经证明了那个结构装不下材料；这里证明的是**本文件的包装**
    /// 没有把材料重新塞回去——`KeyEntry` 比 `KeyInfo` 多了 4 个字段，
    /// 而多出来的字段正是这类泄漏最常见的来路。
    #[test]
    fn no_key_material_in_the_listing_payload() {
        const PEM_MARKER: &str = "-----BEGIN OPENSSH PRIVATE KEY-----";
        let listing = KeyListing {
            keys: vec![
                KeyEntry::from_vault(1, "我的密钥".into(), Ok(info())),
                KeyEntry::from_vault(2, "加密的".into(), Err(KeyInfoError::Encrypted)),
                KeyEntry::from_agent(info()),
            ],
            agent_error: None,
            vault_locked: false,
        };
        let json = serde_json::to_string(&listing).unwrap();
        assert!(!json.contains(PEM_MARKER), "{json}");
        assert!(!json.contains("PRIVATE KEY"), "{json}");
        // 反向对照：确实有内容（否则空载荷会让上面两条恒真）
        assert!(json.contains("SHA256:abc"));
        assert!(json.contains("我的密钥"));
    }

    #[test]
    fn the_entry_struct_has_no_field_that_could_hold_material() {
        // 加了第 7 个字段时这条会红，逼着人回答「那个字段会不会带出材料」。
        const SRC: &str = include_str!("key_cmd.rs");
        let head = SRC.find("pub struct KeyEntry {").expect("找不到 KeyEntry");
        let start = head + SRC[head..].find('{').expect("没有左花括号") + 1;
        let body = &SRC[start..start + SRC[start..].find("\n}").expect("结构没闭合")];
        let fields: Vec<&str> = body
            .lines()
            .filter(|l| l.trim_start().starts_with("pub "))
            .collect();
        assert_eq!(fields.len(), 7, "KeyEntry 的字段变了：{fields:?}");
        // 每个字段都只装 String / Option<String> / Option<u64>——都装不下 PEM
        for f in &fields {
            assert!(
                f.contains("String") || f.contains("u64"),
                "出现了可能装得下材料的字段：{f}"
            );
        }
    }

    #[test]
    fn encrypted_is_a_status_not_a_failure() {
        // 加密私钥读不出指纹是**正常状态**。界面据 status 分辨：`encrypted` 要画成
        // 中性的「已加密」，画成红色失败会让用户去修一件没坏的事。
        let e = KeyEntry::from_vault(1, "k".into(), Err(KeyInfoError::Encrypted));
        assert_eq!(e.status, "encrypted");
        assert_eq!(e.fingerprint, None);
        // 但条目本身还在列表里——不显示的话，用户会以为这把密钥不见了
        assert_eq!(e.label.as_deref(), Some("k"));
        assert_eq!(e.id, Some(1));
    }

    #[test]
    fn the_two_sources_are_distinguishable() {
        // Vault 里的删得掉，agent 里的要去 `ssh-add -d`。不分来源的话，
        // 用户会在列表里找一个删不掉的条目的删除按钮。
        assert_eq!(
            KeyEntry::from_vault(1, "k".into(), Ok(info())).source,
            "vault"
        );
        assert_eq!(KeyEntry::from_agent(info()).source, "agent");
        // agent 条目没有 Vault id——有 id 的话界面会给它一个删不掉的删除按钮
        assert_eq!(KeyEntry::from_agent(info()).id, None);
    }

    /// 「agent 传输开关生效」——钉住那条跨 4 个文件的链。
    ///
    /// ProfileDialog 的勾选框 → `profile.auth.allow_agent` → `CredentialSetBuilder::agent()`
    /// → `creds.agent` → `Method::Agent` 进候选。
    ///
    /// 断在中间任何一处的表现都一样：**agent 里明明有可用密钥却总是登不上**，
    /// 不报错、不告警（`auth.rs` 的 `Method::Agent` 文档注专门写过这个静默失效）。
    /// 这条链此前没有任何东西在看，而它恰好是这次要证明「开关生效」的那一条。
    #[test]
    fn the_agent_toggle_is_wired_all_the_way_through() {
        // ① 前端有控件，且写的是 allow_agent
        const DLG: &str = include_str!("../../../frontend/src/components/ProfileDialog.svelte");
        assert!(
            DLG.contains("allowAgent"),
            "ProfileDialog 没有 agent 勾选框了"
        );
        assert!(
            DLG.contains("allow_agent: allowAgent"),
            "勾选框不再写进 allow_agent"
        );

        // ② 后端据 allow_agent 把 agent 加进凭据集
        const CONNECT: &str = include_str!("../../../crates/sshengine/src/connect.rs");
        assert!(
            CONNECT.contains("if profile.auth.allow_agent {"),
            "allow_agent 不再决定凭据集"
        );

        // ③ 凭据集里有 agent 时，Method::Agent 才进候选。
        //    这一步是最容易被「优化」掉的：协议层没有 agent 这个方法名，
        //    看起来像是多余的一行。
        assert!(
            CONNECT.contains("out.push(Method::Agent)"),
            "Method::Agent 不再被补进候选——agent 认证会静默失效"
        );
        assert!(
            CONNECT.contains("if creds.agent &&"),
            "补回 Method::Agent 的条件不再看 creds.agent"
        );

        // ④ 真正执行 agent 认证的分支还在
        assert!(
            CONNECT.contains("Method::Agent => agent_auth("),
            "Method::Agent 选中后没有执行体"
        );
    }
}
