//! 连接失败的**结构化出错面**（2026-09-01，路线图 4c「认证失败面板带出路」）。
//!
//! # 为什么不再只回一个 `String`
//!
//! `session_open` 此前把 `fs_sshengine::Error` 用 `to_string()` 压成一句话回给前端。
//! 那句话长这样：
//!
//! ```text
//! auth failed; tried: []; server allows: ["publickey"]; notes: ["本连接未配置任何…"]
//! ```
//!
//! 用户三次撞到同一处（2026-08-31/09-01）：**信息全在里面，可他什么也做不了**——
//! 前端拿到的是一坨文本，无法据此决定「该给一颗什么按钮」。要让失败面板挂出
//! 「选一条私钥 / 用 Agent 重试 / 重试连接」，前端必须**结构化地**知道：服务器通告了
//! 什么、我们试过什么、失败属于哪一类、这条连接有没有跳板。
//!
//! # `summary` 是唯一必填的语义
//!
//! `summary` = `Error` 的 Display 原文，**一字不改**。它喂三处：标签的 errorText、
//! toast、文件日志。原文里的 `server allows: ["publickey"]` 对懂 SSH 的人是决定性
//! 信息，人话化那一层在**产生错误的地方**做（引擎的 notes 已经是中文指引），
//! 不在这里二次改写。`connect-state.test.ts` 里「errorText 含原始错误」那条判据
//! 靠它继续成立。
//!
//! # `remaining` 里**没有** `"agent"`
//!
//! `Method::Agent` 是本地合成的——协议层它就是 `publickey`，服务器从不通告
//! 「agent」。把它原样透给前端，面板会渲染出「服务器只接受 publickey / agent」这种
//! 服务器根本没说过的话。`auth.rs::material_mismatch_note` 已经为同一个理由过滤过
//! 一次，这里照抄（该坑在 DTO 层会原样复发，故单独有判据钉住）。
//!
//! # `category` 是给前端分流用的字符串
//!
//! 不做成 Rust 枚举 + serde：它只在这一处产生、只被前端 `switch`，一个 `&'static str`
//! 的 `match` 更直白。取值集合与 `types.ts` 的 `ConnectFailure.category` 联合类型由
//! `enum-contract.test.ts` 逐字对齐——两侧任何一边多一个/少一个都会红。

use fs_connmgr::Profile;
use fs_sshengine::Error;
use serde::Serialize;

/// 一次连接失败的全部可分流信息。**Err 直接过 IPC**：Tauri 把它序列化成 JS 侧的
/// rejection 值，前端用 `summary` 是否为 string 判断拿到的是结构体还是老式字符串。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ConnectFailure {
    /// `Error` 的 Display 原文。喂 errorText / toast / 日志，一字不改。
    pub summary: String,
    /// 分类：见 [`category_of`]。前端据此决定挂哪些按钮。
    pub category: &'static str,
    /// 已尝试过的认证方法（协议名）。空 = 一次都没发起。
    pub tried: Vec<String>,
    /// 服务器**仍在通告**的方法（协议名，已过滤 `agent`）。空 = 服务器没通告任何方法，
    /// 或失败根本不在认证阶段——前端此时**不得**渲染「服务器只接受 X」这句。
    pub remaining: Vec<String>,
    /// 引擎给的诊断句（中文，可直接展示；含 agent 管道路径之类的本地细节，
    /// 前端应折叠而不是全摊）。
    pub notes: Vec<String>,
    pub host: String,
    pub port: u16,
    pub username: String,
    /// 主机密钥 SHA256 指纹（`observed_facts`，未连过为空串）。面板顶部固定显示——
    /// 防「在 A 机的面板上挑了 B 机的私钥」。
    pub fingerprint: String,
    /// 有跳板。错误里不带「失败在第几跳」，前端此时禁掉改档案的一键动作，
    /// 只留「配置认证」与「重试」。
    pub has_jump: bool,
}

/// 错误分类。取值集合由 `enum-contract.test.ts` 与 `types.ts` 逐字对齐。
///
/// 只分「前端处置方式不同」的类：auth 有认证按钮；secret_locked 指向解锁保险库；
/// host_key 不该给认证按钮（那是信任问题）；timeout / connect 只有「重试」有意义；
/// 其余归 other。**不分得更细**——每多一档，前端就多一个没人测的分支。
pub fn category_of(e: &Error) -> &'static str {
    match e {
        Error::Auth { .. } => "auth",
        Error::SecretSourceLocked => "secret_locked",
        Error::HostKey(_) => "host_key",
        Error::Timeout { .. } | Error::Poisoned(_) => "timeout",
        Error::Connect(_) => "connect",
        _ => "other",
    }
}

impl ConnectFailure {
    /// 从引擎错误 + 档案拼出结构化失败。
    pub fn from_error(e: &Error, profile: &Profile) -> Self {
        let (tried, remaining, notes) = match e {
            Error::Auth {
                tried,
                remaining,
                notes,
            } => (
                tried.clone(),
                // 见模块头：agent 是本地合成的方法名，服务器从不通告它。
                remaining
                    .iter()
                    .filter(|m| m.as_str() != "agent")
                    .cloned()
                    .collect(),
                notes.clone(),
            ),
            _ => (Vec::new(), Vec::new(), Vec::new()),
        };
        let facts = fs_sshengine::connect::observed_facts(&profile.host, profile.port);
        Self {
            summary: e.to_string(),
            category: category_of(e),
            tried,
            remaining,
            notes,
            host: profile.host.clone(),
            port: profile.port,
            username: profile.username.clone(),
            fingerprint: facts.fingerprint_sha256,
            has_jump: !profile.jump.is_empty(),
        }
    }

    /// 非引擎错误（装配阶段的 `String`、spawn_blocking 的 JoinError）：只有 summary
    /// 有意义，归 other。
    pub fn other(summary: impl Into<String>, profile: &Profile) -> Self {
        Self {
            summary: summary.into(),
            category: "other",
            tried: Vec::new(),
            remaining: Vec::new(),
            notes: Vec::new(),
            host: profile.host.clone(),
            port: profile.port,
            username: profile.username.clone(),
            fingerprint: String::new(),
            has_jump: !profile.jump.is_empty(),
        }
    }

    /// 连档案都还没取到（id 不合法 / 查库失败）：没有主机信息可填。
    pub fn bare(summary: impl Into<String>) -> Self {
        Self {
            summary: summary.into(),
            category: "other",
            tried: Vec::new(),
            remaining: Vec::new(),
            notes: Vec::new(),
            host: String::new(),
            port: 0,
            username: String::new(),
            fingerprint: String::new(),
            has_jump: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(jump: usize) -> Profile {
        Profile {
            id: uuid::Uuid::nil(),
            name: "t".into(),
            group_path: None,
            host: "10.0.0.9".into(),
            port: 2222,
            username: "root".into(),
            protocol: fs_connmgr::Protocol::Ssh,
            auth: Default::default(),
            jump: (0..jump)
                .map(|i| fs_connmgr::JumpHop {
                    host: format!("j{i}"),
                    port: 22,
                    username: "u".into(),
                    ..Default::default()
                })
                .collect(),
            host_key_policy: Default::default(),
            host_key_pins: vec![],
            env: Default::default(),
            term: Default::default(),
            sftp: Default::default(),
            ai_policy: Default::default(),
            serial: Default::default(),
        }
    }

    /// 用户真机那条原样进来，出去的是能分流的结构：类别 auth、服务器通告原样、
    /// 诊断句原样、summary 与 Display **逐字相同**（errorText 判据靠它）。
    #[test]
    fn an_auth_failure_is_carried_structurally_and_verbatim() {
        let e = Error::Auth {
            tried: vec![],
            remaining: vec!["publickey".into()],
            notes: vec!["本连接未配置任何可用的认证凭据…".into()],
        };
        let f = ConnectFailure::from_error(&e, &profile(0));
        assert_eq!(f.category, "auth");
        assert_eq!(f.remaining, vec!["publickey"]);
        assert!(f.tried.is_empty());
        assert_eq!(f.notes.len(), 1);
        assert_eq!(
            f.summary,
            e.to_string(),
            "summary 必须是 Display 原文，一字不改"
        );
        assert_eq!(
            (f.host.as_str(), f.port, f.username.as_str()),
            ("10.0.0.9", 2222, "root")
        );
        assert!(!f.has_jump);
    }

    /// `agent` 是本地合成的方法名，服务器从不通告它——透给前端就会渲染出
    /// 「服务器只接受 publickey / agent」这种服务器没说过的话。
    /// auth.rs 的 material_mismatch_note 已为同一理由过滤过一次；DTO 层会原样复发，故钉住。
    #[test]
    fn agent_never_reaches_the_frontend_in_remaining() {
        let e = Error::Auth {
            tried: vec![],
            remaining: vec!["publickey".into(), "agent".into(), "password".into()],
            notes: vec![],
        };
        let f = ConnectFailure::from_error(&e, &profile(0));
        assert_eq!(f.remaining, vec!["publickey", "password"]);
    }

    /// 分类表：每一档对应前端一种处置。多一档就多一个没人测的分支，少一档就
    /// 有一类失败被塞进 other 失去按钮——两边都靠 enum-contract 与本测试守。
    #[test]
    fn categories_map_each_error_family_once() {
        let cases: Vec<(Error, &str)> = vec![
            (
                Error::Auth {
                    tried: vec![],
                    remaining: vec![],
                    notes: vec![],
                },
                "auth",
            ),
            (Error::SecretSourceLocked, "secret_locked"),
            (Error::HostKey("refused".into()), "host_key"),
            (
                Error::Timeout {
                    stage: "kex".into(),
                    secs: 10,
                },
                "timeout",
            ),
            (Error::Poisoned("dead".into()), "timeout"),
            (Error::Connect("no route".into()), "connect"),
            (Error::Ssh("misc".into()), "other"),
            (Error::Import("x".into()), "other"),
        ];
        for (e, want) in cases {
            assert_eq!(category_of(&e), want, "{e}");
        }
    }

    /// 非认证类失败不得带出空壳的认证字段以外的东西：tried/remaining/notes 全空，
    /// 前端据 `remaining.is_empty()` 决定**不**渲染「服务器只接受 X」。
    #[test]
    fn non_auth_failures_carry_no_auth_fields() {
        let f = ConnectFailure::from_error(&Error::Connect("refused".into()), &profile(0));
        assert_eq!(f.category, "connect");
        assert!(f.tried.is_empty() && f.remaining.is_empty() && f.notes.is_empty());
    }

    /// 有跳板时 has_jump 为真：前端据此禁掉「改档案的一键动作」。
    #[test]
    fn jump_hosts_are_reported() {
        assert!(ConnectFailure::from_error(&Error::Connect("x".into()), &profile(2)).has_jump);
        assert!(ConnectFailure::other("assembly failed", &profile(1)).has_jump);
        assert!(!ConnectFailure::bare("bad id").has_jump);
    }

    /// 过线形状：字段名就是前端 `types.ts` 里那几个（snake_case 原样，Tauri 不转
    /// 结构体字段名）。这条测的是序列化键集，不是值。
    #[test]
    fn wire_shape_has_exactly_the_documented_keys() {
        let f = ConnectFailure::bare("x");
        let v = serde_json::to_value(&f).unwrap();
        let mut keys: Vec<_> = v.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(
            keys,
            vec![
                "category",
                "fingerprint",
                "has_jump",
                "host",
                "notes",
                "port",
                "remaining",
                "summary",
                "tried",
                "username"
            ]
        );
    }
}
