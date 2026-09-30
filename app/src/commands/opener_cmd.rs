use tauri::AppHandle;
use tauri_plugin_opener::OpenerExt;

/// 可安全落日志的外链摘要（审计 P1-19）。
///
/// 完整 URL **不可入日志**：终端里被 Ctrl+点击的链接极常见地带着一次性凭据——
/// CI 的 `?token=…`、对象存储的预签名 URL、SSO 回调的 `#access_token=…`、
/// 甚至 `https://user:pass@host/`。日志会滚动 7 天、会被打包进用户反馈的诊断包、
/// 会被同机上的其他进程读走，把这些东西写进去等于给凭据额外做了一份长期副本。
///
/// 故只保留 scheme 与规范化 host（必要时含端口）：定位「用户去过哪台主机」够用了，
/// 而 path/query/fragment/userinfo 一律不留。
pub struct UrlLogSummary {
    pub scheme: &'static str,
    /// http(s) 为小写 host（带非默认端口时形如 `host:8443`）；mailto 恒为空串
    ///（邮箱地址本身是个人数据，且对排障无价值）。
    pub host: String,
}

/// 外链白名单校验 + 日志摘要提取：仅 http/https/mailto（spec §2.8）。
/// Rust 侧独立于 capability 作用域把关，拒绝 file://、ftp://、javascript:、tel: 等一切非白名单 scheme。
///
/// 相较原实现（只看 `//` 前缀与长度）收紧到「必须存在合法 host」：
/// - authority 段含 `@` 一律拒绝。userinfo 既是凭据泄漏载体，也是经典钓鱼构造
///   （`https://www.bank.com@evil.example/`——用户看到的是前半段，浏览器去的是 evil.example）；
///   我们没有任何正当理由需要放行它。
/// - host 必须非空且只含 host 允许的字符，端口必须是合法 u16。`https://[::1]/` 之类 IPv6
///   字面量按 RFC 3986 的方括号形式识别，不与端口冒号混淆。
pub fn parse_external_url(url: &str) -> Result<UrlLogSummary, String> {
    let Some((scheme, rest)) = url.split_once(':') else {
        return Err("missing scheme".to_string());
    };
    // scheme 首字符必须是字母（RFC 3986 §3.1）；其余允许 ALPHA / DIGIT / "+" / "-" / "."
    let mut sc = scheme.chars();
    match sc.next() {
        Some(c) if c.is_ascii_alphabetic() => {}
        _ => return Err("invalid scheme".to_string()),
    }
    if sc.any(|c| !c.is_ascii_alphanumeric() && c != '+' && c != '-' && c != '.') {
        return Err("invalid scheme".to_string());
    }
    match scheme.to_ascii_lowercase().as_str() {
        "http" | "https" => {
            // 必须是层级 URL（//host/…），拒绝 http:evil.com、https:// 等畸形写法
            let Some(after) = rest.strip_prefix("//") else {
                return Err("hierarchical http(s) URL required".to_string());
            };
            // authority 止于第一个 '/'、'?' 或 '#'
            let authority = match after.find(['/', '?', '#']) {
                Some(i) => &after[..i],
                None => after,
            };
            let host = validate_authority(authority)?;
            Ok(UrlLogSummary {
                scheme: if scheme.eq_ignore_ascii_case("https") {
                    "https"
                } else {
                    "http"
                },
                host,
            })
        }
        // mailto 无 authority；地址不入日志（见 UrlLogSummary 文档注）
        "mailto" if !rest.trim().is_empty() => Ok(UrlLogSummary {
            scheme: "mailto",
            host: String::new(),
        }),
        "mailto" => Err("empty mailto target".to_string()),
        _ => Err("scheme not allowed".to_string()),
    }
}

/// 校验 authority 段并返回可落日志的 `host[:port]`（小写）。
fn validate_authority(authority: &str) -> Result<String, String> {
    if authority.is_empty() {
        return Err("missing host".to_string());
    }
    if authority.contains('@') {
        // userinfo：钓鱼构造 + 凭据泄漏载体，一律拒绝（不做「剥掉再放行」——
        // 用户看到的和实际打开的不一致本身就是不该放行的信号）
        return Err("userinfo in authority is not allowed".to_string());
    }
    // IPv6 字面量：`[::1]` / `[::1]:8443`
    let (host, port) = if let Some(rest) = authority.strip_prefix('[') {
        let Some(end) = rest.find(']') else {
            return Err("malformed IPv6 host".to_string());
        };
        let h = &rest[..end];
        let tail = &rest[end + 1..];
        let p = match tail {
            "" => None,
            t => Some(t.strip_prefix(':').ok_or("malformed IPv6 host")?),
        };
        if h.is_empty()
            || h.chars()
                .any(|c| !c.is_ascii_hexdigit() && c != ':' && c != '.')
        {
            return Err("malformed IPv6 host".to_string());
        }
        (format!("[{}]", h.to_ascii_lowercase()), p)
    } else {
        match authority.rsplit_once(':') {
            Some((h, p)) => (h.to_ascii_lowercase(), Some(p)),
            None => (authority.to_ascii_lowercase(), None),
        }
    };
    if host.is_empty() {
        return Err("missing host".to_string());
    }
    if !host.starts_with('[')
        && host
            .chars()
            .any(|c| !(c.is_ascii_alphanumeric() || c == '-' || c == '.' || c == '_'))
    {
        return Err("invalid host".to_string());
    }
    match port {
        None => Ok(host),
        // 空端口（`http://host:/x`）语义上等价于默认端口，但更可能是拼错，fail-closed 拒掉
        Some(p) => match p.parse::<u16>() {
            Ok(n) if n > 0 => Ok(format!("{host}:{n}")),
            _ => Err("invalid port".to_string()),
        },
    }
}

/// 经系统默认浏览器打开外链（前端 web-links addon Ctrl+点击触发，spec §2.8）。
#[tauri::command]
pub fn open_external(url: String, app: AppHandle) -> Result<(), String> {
    let summary = parse_external_url(&url)?;
    // 审计 P1-19：只记 scheme + host，绝不记 path/query/fragment/userinfo。
    tracing::info!(
        target: "future_shell_app::opener",
        scheme = summary.scheme,
        host = %summary.host,
        "open external url"
    );
    app.opener()
        .open_url(url, None::<&str>)
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    // 校验与日志摘要提取合一（审计 P1-19）：单测直接打 `parse_external_url`，
    // 既钉住白名单，也钉住「摘要里不得出现 path/query/fragment」。
    use super::parse_external_url;
    use super::parse_external_url as v;

    #[test]
    fn allows_whitelisted_schemes() {
        assert!(v("http://example.com").is_ok());
        assert!(v("https://example.com/x?q=1#frag").is_ok());
        assert!(v("mailto:ops@example.com").is_ok());
        assert!(v("https://example.com:8443/x").is_ok());
        assert!(v("https://[::1]:8443/x").is_ok());
    }

    #[test]
    fn rejects_foreign_and_malformed() {
        for bad in [
            "file:///etc/passwd",
            "ftp://host/x",
            "javascript:alert(1)",
            "vbscript:msgbox(1)",
            "tel:10086", // opener 默认集允许 tel，白名单不收
            "ssh://host",
            "http:example.com", // 非层级
            "https://",         // 缺 host
            "//example.com",    // 无 scheme
            "example.com",      // 无 scheme
            "",
            "mailto:",              // 空目标
            "https:///path",        // authority 为空
            "https://host:/x",      // 空端口
            "https://host:99999/x", // 端口越界
            "https://ho st/x",      // host 含非法字符
            "https://[::1/x",       // IPv6 未闭合
            "1http://example.com",  // scheme 首字符非字母
        ] {
            assert!(v(bad).is_err(), "must reject: {bad}");
        }
    }

    /// 审计 P1-19：authority 里的 userinfo 既泄漏凭据也是钓鱼构造，必须拒绝。
    /// 注意 `https://www.bank.com@evil.example/` 的真实目标是 evil.example。
    #[test]
    fn rejects_userinfo_in_authority() {
        for bad in [
            "https://user:pass@example.com/",
            "http://token@example.com/x",
            "https://www.bank.com@evil.example/login",
        ] {
            assert!(v(bad).is_err(), "must reject userinfo: {bad}");
        }
    }

    /// 审计 P1-19：日志摘要只保留 scheme + host，路径/查询/片段一律不带出来——
    /// 这三处正是一次性 token 的常驻位置。
    #[test]
    fn log_summary_drops_path_query_fragment() {
        let s =
            parse_external_url("https://Example.COM:8443/a/b?token=SECRET#access=SECRET2").unwrap();
        assert_eq!(s.scheme, "https");
        assert_eq!(s.host, "example.com:8443");
        assert!(!s.host.contains("SECRET"));
        // mailto 连地址都不留
        let m = parse_external_url("mailto:ops@example.com").unwrap();
        assert_eq!(m.scheme, "mailto");
        assert_eq!(m.host, "");
    }
}

/// 在系统文件管理器中显示诊断日志目录（不存在先创建）。
/// R119：本命令走 Rust 侧 `OpenerExt`，插件作用域只校验 webview → `commands.rs` 的 IPC 路径，
/// 故 capability 不授 `opener:*` 亦可用（Task 17 已按最小权限删除该项，勿因本命令回加）。
#[tauri::command]
pub fn reveal_log_dir(app: tauri::AppHandle) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    // 审计 P1-21：数据目录解析失败即上抛（命令返回 Result<_, String>），不再经会 panic 的垫片。
    let dir = crate::state::data_dir_or_err()?.join("logs");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    app.opener()
        .reveal_item_in_dir(dir)
        .map_err(|e| e.to_string())
}

/// 打开**会话转录**目录（M4a「打开会话目录入口」，UI 规格 §2.1）。
///
/// 与 [`reveal_log_dir`]（应用自身的 tracing 日志）是两个不同的目录，必须分开：
/// 用户点「打开会话日志目录」时想看的是自己那些转录文件，而不是程序的排障日志；
/// 指到同一处会让他在一堆 `futureshell.log` 里找 `web-01_2026….log`。
///
/// 目录取**配置里那一个**（settings 键 `session.log` 的 dir），空则回落数据目录下
/// `logs/sessions`——与 `AppState::build_session_log` 的回落逐字同源。若两处走散，
/// 这个入口就会打开一个空目录而转录文件躺在别处，是典型的「功能在、但指错地方」。
/// 目录不存在即创建：首次开启转录后、第一次连接前点它，目录本来就还没建。
#[tauri::command]
pub async fn reveal_session_log_dir(
    app: tauri::AppHandle,
    state: tauri::State<'_, std::sync::Arc<crate::state::AppState>>,
) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    // 目录解析走 state 的唯一实现（`session_log_dir`）：这条回落规则曾在此处与
    // `build_session_log` 各写一份，两处一旦走散，这个入口就会打开一个空目录而
    // 转录文件躺在别处——功能在、指错地方，且不报错。
    let dir = state.session_log_dir_resolved().await;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    app.opener()
        .reveal_item_in_dir(dir)
        .map_err(|e| e.to_string())
}

/// 打开**录屏**目录（M4a 会话录制的落点）。
///
/// 与上面那条分开是因为它们是两个真实存在的不同目录：转录（会话日志）可配、
/// 落在 `session.log.dir` 或 `data_dir/logs/sessions`；录屏不可配、恒在
/// `data_dir/recordings`（`record_cmd::recordings_dir`）。两者永不相交。
///
/// 补这个入口是交叉审计的结果：出口原文写「对已落盘**日志/录屏**目录生效」，
/// 而此前全仓只有会话日志一个入口，录屏那一半没有任何打开方式——用户录了屏
/// 却找不到文件在哪。这正是 `reveal_session_log_dir` 那段注释里写的
/// 「功能在、指错地方，且不报错」在另一半上的复发。
#[tauri::command]
pub async fn reveal_recordings_dir(
    app: tauri::AppHandle,
    state: tauri::State<'_, std::sync::Arc<crate::state::AppState>>,
) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    // 目录解析与 record_cmd 同源：那边写文件、这边打开，不能各拼各的路径。
    let dir = crate::commands::record_cmd::recordings_dir_of(&state);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    app.opener()
        .reveal_item_in_dir(dir)
        .map_err(|e| e.to_string())
}
