use fs_sshengine::events::{HostKeyChoice, Prompt, SessionEvents};
use fs_sshengine::hostkey::{Decision, PresentedKey};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter};
use tokio::sync::oneshot;

/// 待决 prompt 的关联键（审计 P1-24）：`(session_id, prompt_id)`。
///
/// 原实现只用 session_id 作键，而**同一 session_id 上可以并存或接连出现多个 prompt**：
/// kbd-interactive 本就是多回合协议（先问密码、再问 OTP）；重连复用同一 session_id；
/// hostkey 与 kbd 也可能在一次拨号里前后脚出现。后到的 prompt 直接 `insert` 覆盖前一枚 tx，
/// 被覆盖者的接收端立刻收到 RecvError 被判成拒绝；更糟的是用户对着「第一个框」输入的应答，
/// 会被投递给「第二个框」的等待者——两次提问语义不同（密码 vs OTP）时这是把密码答给了 OTP。
///
/// prompt_id 是每次发问现取的 UUID nonce，与「这一次提问」一一对应：应答只可能落到它自己
/// 那一回合上，投错的物理可能性被消除，而不是靠时序侥幸。
pub type PromptKey = (String, String);

/// 待决 prompt 的 oneshot tx 表，以 `(session_id, prompt_id)` 为键（审计 P1-24）。
///
/// 表本身仍公开，但**投递一律走 `take_auth`/`take_hostkey`**：那两处封装了「promptId 缺省时
/// 回落到该会话唯一待决 prompt」的过渡兼容规则，绕开它们直接 `remove` 会把该规则复制成多份。
// S289：手写 Default 触发 clippy::derivable_impls——Mutex<HashMap> 各层均有 Default，直接派生
#[derive(Default)]
pub struct PendingPrompts {
    pub auth: Mutex<HashMap<PromptKey, oneshot::Sender<Vec<String>>>>,
    pub hostkey: Mutex<HashMap<PromptKey, oneshot::Sender<HostKeyChoice>>>,
}

impl PendingPrompts {
    /// 取出 kbd-interactive 应答通道（`auth_respond` 投递前调用）。
    pub fn take_auth(
        &self,
        session_id: &str,
        prompt_id: Option<&str>,
    ) -> Result<oneshot::Sender<Vec<String>>, String> {
        take_pending(&self.auth, session_id, prompt_id, "auth")
    }

    /// 取出 host key 裁决通道（`hostkey_decide` 投递前调用）。
    pub fn take_hostkey(
        &self,
        session_id: &str,
        prompt_id: Option<&str>,
    ) -> Result<oneshot::Sender<HostKeyChoice>, String> {
        take_pending(&self.hostkey, session_id, prompt_id, "hostkey")
    }

    /// 清空某会话的全部待决 prompt（session_close / watchdog 收尾路径，F20②）。
    /// 等待中的回调随 tx drop 收到 RecvError → 按拒绝/空应答收尾，连接任务不再挂起。
    /// 键含 prompt_id 后不能再用 `remove(&session_id)`，必须按 session 前缀 retain。
    pub fn clear_session(&self, session_id: &str) {
        self.auth
            .lock()
            .unwrap()
            .retain(|(sid, _), _| sid != session_id);
        self.hostkey
            .lock()
            .unwrap()
            .retain(|(sid, _), _| sid != session_id);
    }
}

/// 按 `(session_id, prompt_id)` 取出待决通道；`prompt_id` 为 None 时走**过渡兼容**回落。
///
/// 回落规则刻意做成「该会话恰好只有一枚待决 prompt 才投递，多于一枚直接报错」：多枚时无从
/// 判断用户答的是哪一枚，随便挑一枚投递恰恰是 P1-24 要消灭的那类错投。宁可让前端收到一条
/// 明确的错误提示，也不能把密码悄悄答给另一个问题。
///
/// **过渡期约定**：`promptId` 现为 `Option`，只为让尚未升级的前端对话框继续可用；
/// 前端接上 `promptId` 透传后，此参数应改为必填、本回落分支应整体删除。
fn take_pending<T>(
    map: &Mutex<HashMap<PromptKey, T>>,
    session_id: &str,
    prompt_id: Option<&str>,
    kind: &str,
) -> Result<T, String> {
    let mut guard = map.lock().unwrap();
    if let Some(pid) = prompt_id {
        return guard
            .remove(&(session_id.to_string(), pid.to_string()))
            .ok_or_else(|| format!("no pending {kind} prompt for id {pid}（已超时或已应答）"));
    }
    // 先在独立作用域里定位唯一键：keys() 借着 guard，必须先还回去才能 remove。
    let key = {
        let mut hits = guard.keys().filter(|(sid, _)| sid == session_id);
        let first = hits.next().cloned();
        let has_more = hits.next().is_some();
        match (first, has_more) {
            (None, _) => return Err(format!("no pending {kind} prompt")),
            (Some(_), true) => {
                return Err(format!(
                    "该会话有多个待决 {kind} prompt，应答必须回传 promptId"
                ))
            }
            (Some(k), false) => k,
        }
    };
    guard
        .remove(&key)
        .ok_or_else(|| format!("no pending {kind} prompt"))
}

pub struct GuiEvents {
    pub app: AppHandle,
    pub session_id: String,
    pub pending: Arc<PendingPrompts>,
    /// 「谁在问」的人读标识，随 `auth:prompt` 下发（审计 P1-24 前端半边）。
    ///
    /// kbd-interactive 的载荷里原本只有 `session_id` 与服务端自报的 `name`/`instruction`——
    /// 前者是 UUID，后者由**对端**提供且常为空串。于是两台机器同时问「Password:」时，
    /// 界面上的两个框逐字相同，用户无从判断自己正在把哪台机器的口令输进去。
    /// 会话身份必须由**本地已知的配置**给出，不能采信对端字符串。
    pub target: String,
    /// 连接档案 id：连接时弹框输口令的「记住（存入 Vault）」需要据此回写 `auth.vault_record`。
    pub profile_id: String,
}

/// `username@host:port`：连接配置里那三项，拼成用户在侧栏里认得出的样子。
///
/// 只取这三项：口令、密钥路径、跳板链一概不进（审计 P1-20 同源约束——
/// 这串标识会随事件进入前端，也可能被用户截图贴进工单）。
pub fn target_label(profile: &fs_connmgr::Profile) -> String {
    format!("{}@{}:{}", profile.username, profile.host, profile.port)
}

/// TOFU / kbd-interactive 人机回路超时：用户长时间不响应即按拒绝/空应答收尾，
/// 避免连接任务永久挂起、重连耗尽 tokio worker（spec §2.1；F20）。
const PROMPT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

impl SessionEvents for GuiEvents {
    fn host_key_decision(
        &self,
        host: &str,
        port: u16,
        presented: &PresentedKey,
        hint: &Decision,
    ) -> HostKeyChoice {
        let (tx, rx) = oneshot::channel();
        let key = (self.session_id.clone(), new_prompt_id());
        self.pending.hostkey.lock().unwrap().insert(key.clone(), tx);
        let emitted = self.app.emit(
            "hostkey:prompt",
            serde_json::json!({
                "session_id": self.session_id,
                // 审计 P1-24：前端必须原样回传 promptId，`hostkey_decide` 据此定位本回合
                "promptId": key.1,
                "host": host, "port": port,
                "key_type": presented.key_type,
                "fingerprint": presented.fingerprint_sha256,
                "kind": match hint { Decision::Changed { .. } => "changed", _ => "tofu" },
                // kind=changed 时附旧指纹（SHA256:…）供 Task 20 HostKeyDialog 新旧对比（spec §2.1 密钥变更红色告警）；
                // tofu 时为 null（信任库无历史记录）。old_fingerprint 由 hostkey 信任库校验产出（Task 7 Decision::Changed）。
                "old_fingerprint": match hint {
                    Decision::Changed { old_fingerprint } => Some(old_fingerprint.clone()),
                    _ => None,
                },
            }),
        );
        // emit 失败 = 事件根本没送到前端（webview 已销毁 / 序列化失败），不会有任何人来应答。
        // 原实现照样进 120 s 等待——用户面对的是一个卡住两分钟、毫无提示的连接过程（审计 P1-24）。
        // 送不出去就没有人机回路可言，立即按拒绝收尾才是诚实的。
        if let Err(e) = emitted {
            tracing::error!(session_id = %self.session_id, error = %e, "hostkey prompt 事件投递失败，按拒绝收尾");
            self.pending.hostkey.lock().unwrap().remove(&key);
            return HostKeyChoice::Refuse;
        }
        self.await_hostkey(rx, key)
    }

    fn kbd_interactive(&self, name: &str, instruction: &str, prompts: &[Prompt]) -> Vec<String> {
        let (tx, rx) = oneshot::channel();
        let key = (self.session_id.clone(), new_prompt_id());
        self.pending.auth.lock().unwrap().insert(key.clone(), tx);
        // name/instruction 原样透传（总设计 §2.1 v3）：载荷键 instruction 对偶 russh 字段 instructions
        let emitted = self.app.emit("auth:prompt", serde_json::json!({
            "session_id": self.session_id,
            // 审计 P1-24：多回合 kbd-interactive 下，应答必须靠 promptId 而非 session_id 归位
            "promptId": key.1,
            // 前端据此在框上标明是哪台机器在问；两台机器同时要口令时这是唯一的分辨依据
            "target": self.target,
            "name": name, "instruction": instruction,
            "prompts": prompts.iter().map(|p| serde_json::json!({"text": p.text, "echo": p.echo})).collect::<Vec<_>>(),
        }));
        if let Err(e) = emitted {
            tracing::error!(session_id = %self.session_id, error = %e, "auth prompt 事件投递失败，按空应答收尾");
            self.pending.auth.lock().unwrap().remove(&key);
            return Vec::new();
        }
        self.await_auth(rx, key)
    }

    fn password_prompt_reason(&self, instruction: &str) -> String {
        self.emit_password_prompt(instruction)
    }

    fn password_prompt(&self) -> String {
        self.emit_password_prompt("该连接未配置口令，请输入登录口令")
    }

    fn status(&self, msg: &str) {
        let _ = self.app.emit(
            "session:status",
            serde_json::json!({"session_id": self.session_id, "message": msg}),
        );
    }
}

/// 每次发问现取的 nonce（审计 P1-24）。UUID v4 而非自增序号：会话可跨进程重连、序号需要
/// 一个谁来维护的问题，nonce 没有这个问题，且天然不可被前端猜测复用。
fn new_prompt_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

impl GuiEvents {
    /// 两个口令询问方法的公共发射体：instruction 参数化（2026-08-31，
    /// 保险库锁着时引擎会传不同的缘由）。
    fn emit_password_prompt(&self, instruction: &str) -> String {
        let (tx, rx) = oneshot::channel();
        let key = (self.session_id.clone(), new_prompt_id());
        self.pending.auth.lock().unwrap().insert(key.clone(), tx);
        // 与 kbd_interactive 共用 `auth:prompt` 通道与 `auth_respond` 应答；多一个 `authKind` 让前端
        // 区分「服务端在问」与「客户端主动要口令」——只有后者显示「记住（存入 Vault）」。
        // 叫 `authKind` 而非 `kind`：`kind` 已被 hostkey:prompt 占用（tofu/changed），
        // 两个不同事件共用一个裸 `kind` 会让 enum-contract 的跨语言门禁把两个契约搅在一起。
        let emitted = self.app.emit(
            "auth:prompt",
            serde_json::json!({
                "session_id": self.session_id,
                "promptId": key.1,
                "authKind": "password",
                "target": self.target,
                // 口令提示语由**本地**给出（不是对端字符串）：这是客户端自己发起的询问
                "name": "口令认证",
                "instruction": instruction,
                "prompts": [{"text": "密码", "echo": false}],
                // 前端「记住」要据此回写 auth.vault_record
                "profileId": self.profile_id,
            }),
        );
        if let Err(e) = emitted {
            tracing::error!(session_id = %self.session_id, error = %e, "password prompt 事件投递失败，按取消收尾");
            self.pending.auth.lock().unwrap().remove(&key);
            return String::new();
        }
        self.await_auth(rx, key)
            .first()
            .cloned()
            .unwrap_or_default()
    }

    /// 在 russh 回调所在线程（runtime worker）上阻塞等待前端裁决：block_in_place 释放 worker，
    /// 其内 Handle::current().block_on 驱动 tokio 定时器使 120s 超时生效；超时或 tx 被
    /// session_close 移除（RecvError）时按 Refuse 收尾并清理 pending（F15/F20①②）。
    /// `key` 随等待一并传入：清理必须精确到本回合那一枚，按 session_id 批量清会连带干掉
    /// 同会话上另一枚仍然有效的待决 prompt（审计 P1-24）。
    fn await_hostkey(&self, rx: oneshot::Receiver<HostKeyChoice>, key: PromptKey) -> HostKeyChoice {
        let pending = self.pending.clone();
        tokio::task::block_in_place(move || {
            tokio::runtime::Handle::current().block_on(async move {
                match tokio::time::timeout(PROMPT_TIMEOUT, rx).await {
                    Ok(Ok(choice)) => choice,
                    Ok(Err(_)) => {
                        // tx 被 session_close 移除（RecvError）：关闭收尾的正常路径，静默按拒绝
                        pending.hostkey.lock().unwrap().remove(&key);
                        HostKeyChoice::Refuse
                    }
                    Err(_) => {
                        // S294：prompt 超时——留日志痕迹，防静默拒绝无痕（复审一波低；一次性 emit 无重放，
                        // 前端错过事件后的唯一出路即此超时，Task 20 可补 pending 查询/重放）
                        tracing::warn!(session_id = %key.0, prompt_id = %key.1, "hostkey prompt timed out; refusing");
                        pending.hostkey.lock().unwrap().remove(&key);
                        HostKeyChoice::Refuse
                    }
                }
            })
        })
    }

    fn await_auth(&self, rx: oneshot::Receiver<Vec<String>>, key: PromptKey) -> Vec<String> {
        let pending = self.pending.clone();
        tokio::task::block_in_place(move || {
            tokio::runtime::Handle::current().block_on(async move {
                match tokio::time::timeout(PROMPT_TIMEOUT, rx).await {
                    Ok(Ok(responses)) => responses,
                    Ok(Err(_)) => {
                        // tx 被 session_close 移除（RecvError）：关闭收尾的正常路径，静默按空应答
                        pending.auth.lock().unwrap().remove(&key);
                        Vec::new()
                    }
                    Err(_) => {
                        // S294：prompt 超时——留日志痕迹，防静默空应答无痕（复审一波低）
                        tracing::warn!(session_id = %key.0, prompt_id = %key.1, "kbd-interactive prompt timed out; empty response");
                        pending.auth.lock().unwrap().remove(&key);
                        Vec::new()
                    }
                }
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 审计 P1-24：同一会话上两枚并存的 prompt 必须各自独立可寻址——
    /// 这正是原实现（键只有 session_id）做不到的：后者会覆盖前者。
    #[test]
    fn two_prompts_on_same_session_coexist_and_route_by_id() {
        let pending = PendingPrompts::default();
        let (tx1, rx1) = oneshot::channel::<Vec<String>>();
        let (tx2, rx2) = oneshot::channel::<Vec<String>>();
        pending
            .auth
            .lock()
            .unwrap()
            .insert(("s".into(), "p1".into()), tx1);
        pending
            .auth
            .lock()
            .unwrap()
            .insert(("s".into(), "p2".into()), tx2);
        // 两枚都在（未被覆盖）
        assert_eq!(pending.auth.lock().unwrap().len(), 2);
        // 缺 promptId 时不得瞎猜：多于一枚一律报错，绝不把应答投给另一回合
        assert!(pending.take_auth("s", None).is_err());
        // 带 promptId 精确投递
        pending
            .take_auth("s", Some("p2"))
            .unwrap()
            .send(vec!["second".into()])
            .unwrap();
        assert_eq!(rx2.blocking_recv().unwrap(), vec!["second".to_string()]);
        // p1 仍待决，未被 p2 的应答波及
        pending
            .take_auth("s", Some("p1"))
            .unwrap()
            .send(vec!["first".into()])
            .unwrap();
        assert_eq!(rx1.blocking_recv().unwrap(), vec!["first".to_string()]);
    }

    /// 过渡兼容：会话上恰好只有一枚待决 prompt 时，不带 promptId 的老前端仍能投递。
    #[test]
    fn single_pending_prompt_falls_back_without_id() {
        let pending = PendingPrompts::default();
        let (tx, rx) = oneshot::channel::<HostKeyChoice>();
        pending
            .hostkey
            .lock()
            .unwrap()
            .insert(("s".into(), "only".into()), tx);
        pending
            .take_hostkey("s", None)
            .unwrap()
            .send(HostKeyChoice::AcceptOnce)
            .unwrap();
        assert!(matches!(
            rx.blocking_recv().unwrap(),
            HostKeyChoice::AcceptOnce
        ));
        // 取空后再取报错，且不会误命中其他会话
        assert!(pending.take_hostkey("s", None).is_err());
    }

    /// `clear_session` 必须只清目标会话：键含 prompt_id 后不能再靠 `remove(&session_id)`，
    /// 漏改会变成「关一个标签把另一个会话的待决 prompt 一起判死」。
    #[test]
    fn clear_session_only_touches_target_session() {
        let pending = PendingPrompts::default();
        let (tx_a, _rx_a) = oneshot::channel::<Vec<String>>();
        let (tx_b, _rx_b) = oneshot::channel::<Vec<String>>();
        pending
            .auth
            .lock()
            .unwrap()
            .insert(("a".into(), "p".into()), tx_a);
        pending
            .auth
            .lock()
            .unwrap()
            .insert(("b".into(), "p".into()), tx_b);
        pending.clear_session("a");
        assert!(pending.take_auth("a", None).is_err());
        assert!(pending.take_auth("b", None).is_ok());
    }

    /// 造一个只写出本用例关心字段的连接配置。
    ///
    /// 走 serde 而非结构体字面量：`Profile` 的可选字段都带 `#[serde(default)]`，
    /// 日后加字段不会把这里编译坏——那种摩擦最后总是以删测试收场。
    fn profile(name: &str, user: &str, host: &str, port: u16) -> fs_connmgr::Profile {
        serde_json::from_value(serde_json::json!({
            "id": "00000000-0000-0000-0000-000000000000",
            "name": name,
            "group_path": null,
            "host": host,
            "port": port,
            "username": user,
        }))
        .expect("测试用 Profile 必须能反序列化（字段名或必填项变了就该在这里炸）")
    }

    /// 身份标识只有 `user@host:port` 这一种写法，且端口必须显式带上。
    ///
    /// 这串字随 `auth:prompt` 进前端，是「口令要输给谁」在界面上的唯一依据
    /// （AuthPromptDialog 的身份行）；22 与 2222 是两台不同的机器，省略端口即抹平差别。
    #[test]
    fn target_label_is_user_at_host_colon_port() {
        assert_eq!(
            target_label(&profile("生产网关", "root", "alpha.example.com", 22)),
            "root@alpha.example.com:22"
        );
        assert_eq!(
            target_label(&profile("生产网关", "root", "alpha.example.com", 2222)),
            "root@alpha.example.com:2222"
        );
    }

    /// 两条并发提问必须**看得出区别**：user/host/port 任意一项不同，标识就得不同。
    ///
    /// 反过来，本地起的显示名（`name`）变了标识不动——用 name 当身份是不行的：
    /// 它可以两条都叫「生产」，而真正决定这份口令去哪台机器的是 user@host:port。
    #[test]
    fn target_label_discriminates_by_wire_destination_not_by_display_name() {
        let base = profile("生产", "root", "alpha.example.com", 22);
        for (case, other) in [
            (
                "换登录用户",
                profile("生产", "ops", "alpha.example.com", 22),
            ),
            ("换主机", profile("生产", "root", "beta.example.com", 22)),
            ("换端口", profile("生产", "root", "alpha.example.com", 2222)),
        ] {
            assert_ne!(target_label(&base), target_label(&other), "{case}");
        }
        let renamed = profile("测试", "root", "alpha.example.com", 22);
        assert_eq!(target_label(&base), target_label(&renamed));
    }
}
