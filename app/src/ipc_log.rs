//! IPC 追踪层：把「用户做了什么、报了什么错」写进日志文件。
//!
//! 背景（用户真踩过）：全仓命令层此前**一条日志都不打**。前端 `invoke("session_open", …)`
//! 里的参数名写错 / 缺必填字段时，Tauri 在**进入任何命令函数之前**就返回 `missing required
//! key X`，这个错只变成前端一句 toast，文件日志里什么都没有——事后对着空日志根本无从查起。
//!
//! 这一层的职责是**边界记账**：
//! - `ipc:invoke`：每一条 IPC 调用（命令名 + 脱敏后的实参）在进入分发前落盘；
//! - `ipc:done`：分发返回后落盘（含耗时，用来抓「某个命令卡住」）。
//!
//! 为什么结果不在此层记：Tauri v2 的异步命令经 `InvokeResolver::respond_async` 在另一任务里
//! 结算，`invoke_handler` 包装闭包拿不到那个返回值（`Invoke` 的 `resolver` 字段虽 pub，但
//! 观测结果的 responder 藏在私有字段里）。命令内部自身的错误字符串仍会原样走到前端 toast，
//! 关键的「调用点 + 实参」已经由 `ipc:invoke` 全覆盖——缺字段/键名写错这类错误，光看实参
//! 就能定位。异步命令内部的失败，由各命令自身已有的 `tracing::error!` 与 connect 链路的
//! 日志承担。
//!
//! 安全（P1-19/20/23 + secrets.rs 同口径）：实参落盘前必须过 [`redact`]，口令/私钥/剪贴板/
//! 终端输入/导入原文/环境变量一律掩成 `<redacted>`。宁可多掩一个无害字段，不可漏一个秘密。

use serde_json::Value;
use tauri::ipc::{Invoke, InvokeBody};
use tauri::Runtime;

/// 命中即整值掩掉的键名（大小写不敏感的子串匹配，覆盖 camelCase / snake_case 两套）。
/// 维护原则：新增一个含秘密的 IPC 参数，必须在这里加一条，并补 [`redact`] 的单测。
const SENSITIVE_KEYS: &[&str] = &[
    "password",
    "passphrase",
    "secret",  // vault 的 secretB64 / secret_b64
    "datab64", // term_input 的终端输入（base64，可能含用户敲的口令）
    "data_b64",
    "responses",     // auth_respond 的 kbd-interactive 应答（用户敲的口令/OTP）
    "text",          // clipboard_write 的剪贴板内容
    "content",       // hostkey_import / profiles_import 的整份原文
    "current",       // vault_change_passphrase 的旧口令
    "newpassphrase", // vault_change_passphrase 的新口令
    "private_key",   // 私钥 PEM
    "env",           // Profile.env：环境变量里躺着 API key/口令是常态，整对象掩掉
];

/// 把一条 IPC 实参脱敏成可落盘的字符串。Raw 载荷（非 JSON）只记字节数。
pub fn redact(body: &InvokeBody) -> String {
    match body {
        InvokeBody::Json(v) => redact_value(v).to_string(),
        InvokeBody::Raw(bytes) => format!("<raw {} bytes>", bytes.len()),
    }
}

fn redact_value(v: &Value) -> Value {
    match v {
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, val) in map {
                // 归一化：去掉 `_`/`-`/空格再比大小写——`privateKey`/`private_key`/`private-key`
                // 三种拼法都落到同一串，避免「清单里是 snake、载荷里是 camel」的漏掩。
                let key = normalize(k);
                if SENSITIVE_KEYS.iter().any(|s| key.contains(&normalize(s))) {
                    out.insert(k.clone(), Value::String("<redacted>".into()));
                } else {
                    out.insert(k.clone(), redact_value(val));
                }
            }
            Value::Object(out)
        }
        Value::Array(arr) => Value::Array(arr.iter().map(redact_value).collect()),
        other => other.clone(),
    }
}

/// 键名归一化：只保留 ASCII 字母数字、统一小写。见 [`redact_value`]。
fn normalize(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// 高频流控命令：不是「用户动作」，而是每帧/每 tick 都被前端驱动的内部回传。
/// `term_ack` 每帧一次（batch_interval 16 ms 下 ~60/s、大输出更密），`term_resize`
/// 随窗口拖拽连续触发——逐条 info 日志会在持续输出下写 ~120 行/s，把 8 MiB
/// 轮转日志在几分钟内卷走，顺带吞掉背压/滞留诊断行（审计3）。改记 debug（默认
/// `future_shell_app=info` 过滤掉），需排查时用 `RUST_LOG=future_shell_app::ipc=debug` 打开。
const NOISY_COMMANDS: &[&str] = &["term_ack", "term_resize"];

fn is_noisy(command: &str) -> bool {
    NOISY_COMMANDS.contains(&command)
}

/// 包装 `generate_handler!` 产物：在分发前后各打一条日志（noisy 命令记 debug、其余记 info）。
/// 直接替换 `.invoke_handler(tauri::generate_handler![…])` 里的内层即可，
/// 命令注册列表零改动——新增命令自动被覆盖，不会「加了一个命令忘了加日志」。
pub fn log_invoke<R: Runtime>(
    handler: impl Fn(Invoke<R>) -> bool + Send + Sync + 'static,
) -> impl Fn(Invoke<R>) -> bool + Send + Sync + 'static {
    move |invoke| {
        let command = invoke.message.command().to_string();
        let args = redact(invoke.message.payload());
        let started = std::time::Instant::now();
        // tracing 的 event!/debug!/info! 宏要求常量 level（E0435），不能传变量，故以二分支择级。
        if is_noisy(&command) {
            tracing::debug!(
                target: "future_shell_app::ipc",
                command = %command,
                args = %args,
                "ipc:invoke"
            );
            let handled = handler(invoke);
            tracing::debug!(
                target: "future_shell_app::ipc",
                command = %command,
                elapsed_ms = started.elapsed().as_millis() as u64,
                "ipc:done"
            );
            handled
        } else {
            tracing::info!(
                target: "future_shell_app::ipc",
                command = %command,
                args = %args,
                "ipc:invoke"
            );
            let handled = handler(invoke);
            tracing::info!(
                target: "future_shell_app::ipc",
                command = %command,
                elapsed_ms = started.elapsed().as_millis() as u64,
                "ipc:done"
            );
            handled
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{is_noisy, redact, redact_value, NOISY_COMMANDS, SENSITIVE_KEYS};
    use serde_json::json;

    fn r(v: serde_json::Value) -> String {
        redact_value(&v).to_string()
    }

    #[test]
    fn secrets_are_masked_case_insensitively() {
        let payload = json!({
            "passphrase": "hunter2",
            "password": "hunter2",
            "secretB64": "aGVsbG8=",
            "secret_b64": "aGVsbG8=",
            "dataB64": "bHMK",
            "responses": ["pw", "otp"],
            "text": "clipboard secret",
            "content": "ssh-rsa AAAA…",
            "current": "oldpw",
            "newPassphrase": "newpw",
            "privateKey": "-----BEGIN RSA PRIVATE KEY-----",
        });
        let out = r(payload);
        for needle in [
            "hunter2",
            "aGVsbG8=",
            "bHMK",
            "clipboard secret",
            "ssh-rsa",
            "oldpw",
            "newpw",
            "PRIVATE KEY",
        ] {
            assert!(!out.contains(needle), "秘密「{needle}」泄漏进日志：{out}");
        }
        assert_eq!(
            out.matches("<redacted>").count(),
            11,
            "11 个敏感键都应被掩：{out}"
        );
    }

    #[test]
    fn env_map_is_wholly_masked() {
        let out = r(json!({ "env": { "API_TOKEN": "sk-123", "PATH": "/usr/bin" } }));
        assert!(!out.contains("sk-123"), "env 里的秘密泄漏：{out}");
        assert!(!out.contains("/usr/bin"), "env 整对象应掩掉：{out}");
    }

    #[test]
    fn benign_fields_survive_for_diagnosis() {
        let out = r(
            json!({ "sessionId": "abc-123", "host": "10.0.0.1", "port": 22, "path": "/etc/hosts" }),
        );
        assert!(out.contains("abc-123"), "sessionId 丢了：{out}");
        assert!(out.contains("10.0.0.1"), "host 丢了：{out}");
        assert!(out.contains("/etc/hosts"), "path 丢了：{out}");
    }

    #[test]
    fn nested_and_array_secrets_are_masked() {
        let out = r(json!({
            "profile": { "auth": { "vault_record": 1 }, "jump": [ { "host": "j1", "auth": { "passphrase_vault_record": 7 } } ] }
        }));
        assert!(
            out.contains("\"vault_record\":1"),
            "数字凭据 id 应保留：{out}"
        );
        assert!(out.contains("j1"), "跳板 host 应保留：{out}");
    }

    #[test]
    fn raw_body_logs_only_byte_count() {
        use tauri::ipc::InvokeBody;
        let body = InvokeBody::Raw(vec![0u8; 42]);
        assert_eq!(redact(&body), "<raw 42 bytes>");
    }

    /// 敏感键清单不能悄悄缩水：每删一条，就是给一类秘密开了日志口子。
    /// 这里钉住「至少覆盖」的最小集，删任一条即红（新增秘密类该加新的，不是替换旧的）。
    #[test]
    fn sensitive_key_list_has_no_regression() {
        for must in [
            "password",
            "passphrase",
            "secret",
            "datab64",
            "responses",
            "text",
            "content",
            "current",
            "env",
            "private_key",
        ] {
            assert!(
                SENSITIVE_KEYS.iter().any(|k| k == &must),
                "敏感键「{must}」从清单里消失了"
            );
        }
    }

    /// 高频流控命令归入 noisy（记 debug），用户动作/一次性命令仍记 info。
    #[test]
    fn noisy_commands_are_debug_others_are_info() {
        assert!(is_noisy("term_ack"), "term_ack 每帧一次，必须降为 debug");
        assert!(
            is_noisy("term_resize"),
            "term_resize 随窗口拖拽连发，降 debug"
        );
        assert!(!is_noisy("term_input"), "term_input 是用户输入，仍记 info");
        assert!(
            !is_noisy("session_open"),
            "session_open 是一次性动作，仍记 info"
        );
    }

    /// noisy 清单不能悄悄缩水：term_ack 是最凶的洪泛源（每帧一次），删了它审计3 的日志洪泛即复发。
    #[test]
    fn noisy_command_list_has_no_regression() {
        assert!(
            NOISY_COMMANDS.contains(&"term_ack"),
            "term_ack 从 noisy 清单里消失了"
        );
    }
}
