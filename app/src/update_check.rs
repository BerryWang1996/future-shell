//! 手动版本检查（M4b 出口「updater」条，按 2026-08-23 的用户裁决改写）。
//!
//! # 为什么不是 tauri-plugin-updater
//!
//! 出口原文要求「mock 更新服务器走通『下载→签名校验→替换』」，那需要
//! `tauri-plugin-updater`。而它与本产品的出站面承诺直接冲突——README「出站面」节写着
//! **零遥测没有例外**，而 updater 插件会在后台主动去厂商的 manifest 地址查版本，
//! 那是标准的 phone home。`egress-contract.test.ts` 把它列在 `TELEMETRY_CRATES` 里，
//! 任何 crate 都不允许引入。
//!
//! 用户裁决：**只做手动检查**。于是本模块的形状是：
//!
//! | | tauri-plugin-updater | 本模块 |
//! |---|---|---|
//! | 何时联网 | 启动时/定时后台查 | **只有用户点「检查更新」那一下** |
//! | 地址 | 打包时写进配置 | **用户自己填**，不填就永不联网 |
//! | 下载 | 自动下载 | 不下载——给出链接，用户自己去 |
//! | 替换 | 自动替换二进制 | 不替换 |
//!
//! 「不下载不替换」不只是省事：自动替换一个正在运行的二进制需要提权、需要处理
//! 「替换到一半断电」，而那两件事的失败模式是**用户的工具打不开了**。
//! 对一个运维要靠它去救火的程序，这个代价不值得。
//!
//! # 不新增任何 HTTP 面
//!
//! 本模块**不自己持有 HTTP 客户端**，而是复用 `fs_ai::Transport`。
//! 于是 `reqwest` 仍然只出现在 `crates/ai` 一个 crate 里，
//! 出站面门禁的 `HTTP_ALLOWED` 一个字都不用改。

use fs_ai::provider::ProviderError;
use fs_ai::transport::Transport;
use fs_ai::wire::HttpRequestSpec;

/// 语义版本。只认 `major.minor.patch[-pre]`，不认 build metadata（`+xxx`）——
/// 本仓的版本号门禁（`check-version-consistency.sh`）本来就只接受纯三段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemVer {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    /// 预发布标识（`-beta.1`）。`None` 表示正式版。
    pub pre: Option<String>,
}

impl SemVer {
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim().trim_start_matches('v');
        // build metadata 直接丢弃（semver 规定它不参与比较）
        let s = s.split('+').next()?;
        let (core, pre) = match s.split_once('-') {
            Some((c, p)) if !p.is_empty() => (c, Some(p.to_string())),
            Some(_) => return None, // `1.2.3-` 是畸形
            None => (s, None),
        };
        let mut it = core.split('.');
        let major = it.next()?.parse().ok()?;
        let minor = it.next()?.parse().ok()?;
        let patch = it.next()?.parse().ok()?;
        if it.next().is_some() {
            return None; // 四段不是 semver
        }
        Some(Self {
            major,
            minor,
            patch,
            pre,
        })
    }
}

impl PartialOrd for SemVer {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for SemVer {
    /// semver 的排序规则，其中**预发布版小于同号正式版**。
    ///
    /// 这一条是这里最容易写错的：`0.4.1-beta` 必须小于 `0.4.1`。
    /// 按字符串比较会得到相反的结果（`"0.4.1-beta" > "0.4.1"`，因为前者更长），
    /// 于是程序会在用户已经装了正式版时提示他「有新版本 0.4.1-beta」。
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        (self.major, self.minor, self.patch)
            .cmp(&(other.major, other.minor, other.patch))
            .then_with(|| match (&self.pre, &other.pre) {
                (None, None) => Ordering::Equal,
                // 有预发布标识的一方**更小**
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (Some(a), Some(b)) => a.cmp(b),
            })
    }
}

impl std::fmt::Display for SemVer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if let Some(p) = &self.pre {
            write!(f, "-{p}")?;
        }
        Ok(())
    }
}

/// 更新清单的形状。字段全部可选除了 `version`——
/// 宽容解析：多一个不认识的字段不该让整次检查失败。
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct UpdateManifest {
    pub version: String,
    #[serde(default)]
    pub notes: Option<String>,
    /// 发布页或下载页的链接。**由用户在浏览器里打开**，本程序不下载。
    #[serde(default)]
    pub url: Option<String>,
}

/// 一次检查的结论。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UpdateStatus {
    /// 用户没填检查地址。**这一支意味着一个包都没发出去。**
    NotConfigured,
    UpToDate {
        current: String,
    },
    Available {
        current: String,
        latest: String,
        notes: Option<String>,
        /// 给用户在浏览器里打开的链接。可能没有。
        url: Option<String>,
    },
    /// 检查本身失败了（连不上、清单看不懂）。
    ///
    /// 与「已是最新」严格分开：把失败显示成「已是最新」会让用户以为自己在最新版上，
    /// 而实际上程序根本没查成。
    Failed {
        detail: String,
    },
}

impl UpdateStatus {
    pub fn message(&self) -> String {
        match self {
            Self::NotConfigured => {
                "没有配置更新检查地址，本程序不会主动联网。要检查更新请在 设置 → 更新 里填入地址，或直接去项目发布页看。".into()
            }
            Self::UpToDate { current } => format!("已是最新版本（{current}）。"),
            Self::Available { current, latest, .. } => {
                format!("有新版本 {latest}（当前 {current}）。本程序不会自动下载或替换，请到发布页手动获取。")
            }
            Self::Failed { detail } => format!("检查更新失败：{detail}"),
        }
    }
}

/// 一个 URL 能不能交给系统浏览器打开。能则返回它，否则 `None`。
///
/// # 为什么这道关必须存在
///
/// [`UpdateStatus::Available::url`] 的值来自**远端 JSON**，前端拿到后交给 `openExternal`
/// ——在 Windows 上那最终是一次 `ShellExecute`。不校验 scheme 的话，一份清单里写
/// `"url": "file:///C:/Windows/System32/..."`（或任何已注册的自定义协议）就等于让远端
/// 决定本机打开什么。本程序确实不下载、不安装，但「把用户引到哪儿去」同样是一步可被劫持
/// 的动作，而它看起来毫无风险——按钮上写的是「打开发布页」。
///
/// 限死 https 而非放行 http，与 `settings_cmd` 里 `update.manifestUrl` 的白名单校验同口径：
/// 明文 http 的清单可被中间人整份换掉，那时「发布页」就是攻击者的钓鱼页。
///
/// 顺带挡掉内嵌控制字符（`\r\n` 之类）——它们在某些 URL 处理路径上能造成截断或注入，
/// 且合法 URL 里本就不该有。
fn sanitize_external_url(u: &str) -> Option<String> {
    let u = u.trim();
    if u.len() > 2048 || u.is_empty() {
        return None;
    }
    if u.chars().any(|c| c.is_control() || c == ' ') {
        return None;
    }
    // scheme 大小写不敏感（`HTTPS://` 合法），其余部分原样保留
    if !u.to_ascii_lowercase().starts_with("https://") {
        return None;
    }
    // `https://` 之后必须真有主机名，否则 `https://` 这七个字符自己就能过关
    if u[8..].is_empty() {
        return None;
    }
    Some(u.to_string())
}

/// 手动检查一次。
///
/// `manifest_url` 为 `None` 或空 → 直接返回 [`UpdateStatus::NotConfigured`]，
/// **一个包都不发**。这是「不点就零流量」承诺的落点，且有测试断言 transport 没被调用。
pub async fn check(
    manifest_url: Option<&str>,
    current_version: &str,
    transport: &dyn Transport,
) -> UpdateStatus {
    let Some(url) = manifest_url.map(str::trim).filter(|u| !u.is_empty()) else {
        return UpdateStatus::NotConfigured;
    };
    if sanitize_external_url(url).is_none() {
        return UpdateStatus::Failed {
            detail: "更新检查地址必须是 https:// 开头的普通 URL".into(),
        };
    }
    let Some(current) = SemVer::parse(current_version) else {
        return UpdateStatus::Failed {
            detail: format!("本程序自己的版本号 `{current_version}` 不是合法 semver"),
        };
    };

    let spec = HttpRequestSpec {
        method: "GET",
        url: url.to_string(),
        headers: vec![("accept".into(), "application/json".into())],
        body: String::new(),
    };
    let resp = match transport.send(&spec).await {
        Ok(r) => r,
        Err(ProviderError::Transport { detail }) => {
            return UpdateStatus::Failed {
                detail: format!("连不上更新检查地址：{detail}"),
            }
        }
        Err(e) => {
            return UpdateStatus::Failed {
                detail: e.to_string(),
            }
        }
    };
    if !resp.is_success() {
        return UpdateStatus::Failed {
            detail: format!("更新检查地址返回 HTTP {}", resp.status),
        };
    }
    let manifest: UpdateManifest = match serde_json::from_str(&resp.body) {
        Ok(m) => m,
        Err(e) => {
            return UpdateStatus::Failed {
                detail: format!("更新清单不是预期的 JSON 形状：{e}"),
            }
        }
    };
    let Some(latest) = SemVer::parse(&manifest.version) else {
        return UpdateStatus::Failed {
            detail: format!("更新清单里的版本号 `{}` 不是合法 semver", manifest.version),
        };
    };
    if latest > current {
        UpdateStatus::Available {
            current: current.to_string(),
            latest: latest.to_string(),
            // 备注同样来自远端，限长后原样带出。**不解析 Markdown、不当 HTML 渲染**——
            // 前端把它当纯文本显示。截断处补省略号，免得半句话看着像完整的一句。
            notes: manifest.notes.map(|n| {
                if n.chars().count() > 2000 {
                    n.chars().take(2000).collect::<String>() + "…"
                } else {
                    n
                }
            }),
            // 过不了这道关就当作「没有链接」，而不是让整次检查失败：
            // 「有新版本」这件事仍然是真的，用户仍该被告知，只是不给可点的按钮。
            url: manifest.url.as_deref().and_then(sanitize_external_url),
        }
    } else {
        UpdateStatus::UpToDate {
            current: current.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fs_ai::transport::HttpResponse;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Fake {
        calls: AtomicUsize,
        reply: Result<HttpResponse, ProviderError>,
    }

    impl Fake {
        fn body(body: &str) -> Self {
            Self {
                calls: AtomicUsize::new(0),
                reply: Ok(HttpResponse {
                    status: 200,
                    retry_after_secs: None,
                    body: body.into(),
                }),
            }
        }
        fn status(status: u16) -> Self {
            Self {
                calls: AtomicUsize::new(0),
                reply: Ok(HttpResponse {
                    status,
                    retry_after_secs: None,
                    body: "{}".into(),
                }),
            }
        }
        fn dead() -> Self {
            Self {
                calls: AtomicUsize::new(0),
                reply: Err(ProviderError::Transport {
                    detail: "connection refused".into(),
                }),
            }
        }
        fn n(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    impl Transport for Fake {
        fn send<'a>(
            &'a self,
            _spec: &'a HttpRequestSpec,
        ) -> Pin<Box<dyn Future<Output = Result<HttpResponse, ProviderError>> + Send + 'a>>
        {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let r = self.reply.clone();
            Box::pin(async move { r })
        }
    }

    // ── semver ──

    #[test]
    fn semver_parses_the_shapes_we_accept() {
        assert_eq!(
            SemVer::parse("0.4.1"),
            Some(SemVer {
                major: 0,
                minor: 4,
                patch: 1,
                pre: None
            })
        );
        // 前导 v 与 build metadata 都能吃
        assert_eq!(SemVer::parse("v1.2.3").unwrap().to_string(), "1.2.3");
        assert_eq!(SemVer::parse("1.2.3+abc").unwrap().to_string(), "1.2.3");
        assert_eq!(
            SemVer::parse("0.4.1-beta.2").unwrap().pre.as_deref(),
            Some("beta.2")
        );
    }

    #[test]
    fn semver_rejects_malformed_versions() {
        for bad in ["", "1", "1.2", "1.2.3.4", "a.b.c", "1.2.x", "1.2.3-"] {
            assert_eq!(SemVer::parse(bad), None, "{bad:?} 该被拒");
        }
    }

    #[test]
    fn a_prerelease_sorts_below_the_matching_release() {
        // 这是最容易写错的一条：按字符串比较会得到相反结果
        // （"0.4.1-beta" > "0.4.1"，因为前者更长），
        // 于是程序会在用户已装正式版时提示他「有新版本 0.4.1-beta」。
        let beta = SemVer::parse("0.4.1-beta").unwrap();
        let rel = SemVer::parse("0.4.1").unwrap();
        assert!(beta < rel, "预发布版必须小于同号正式版");
        assert!(rel > beta);
        // 同为预发布时按标识排
        assert!(SemVer::parse("1.0.0-alpha").unwrap() < SemVer::parse("1.0.0-beta").unwrap());
    }

    #[test]
    fn semver_ordering_is_numeric_not_lexicographic() {
        // 字符串比较会说 "0.10.0" < "0.9.0"
        assert!(SemVer::parse("0.9.0").unwrap() < SemVer::parse("0.10.0").unwrap());
        assert!(SemVer::parse("0.4.9").unwrap() < SemVer::parse("0.4.10").unwrap());
        assert!(SemVer::parse("0.4.1").unwrap() < SemVer::parse("1.0.0").unwrap());
    }

    // ── 「不点就零流量」──

    #[tokio::test]
    async fn without_a_configured_url_not_a_single_packet_goes_out() {
        // 这是承诺的落点：不配就永不联网
        for url in [None, Some(""), Some("   ")] {
            let t = Fake::body("{}");
            let s = check(url, "0.4.0", &t).await;
            assert_eq!(s, UpdateStatus::NotConfigured, "url={url:?}");
            assert_eq!(t.n(), 0, "url={url:?} 时竟然发了请求");
        }
    }

    #[tokio::test]
    async fn a_non_http_url_is_rejected_before_any_request() {
        let t = Fake::body("{}");
        let s = check(Some("file:///etc/passwd"), "0.4.0", &t).await;
        assert!(matches!(s, UpdateStatus::Failed { .. }), "{s:?}");
        assert_eq!(t.n(), 0, "非 http 地址仍然发了请求");
    }

    // ── 检查结果 ──

    #[tokio::test]
    async fn a_newer_version_is_reported_with_a_link_but_never_downloaded() {
        let t = Fake::body(
            r#"{"version":"0.5.0","notes":"修了几个 bug","url":"https://example.com/releases"}"#,
        );
        let s = check(Some("https://example.com/latest.json"), "0.4.0", &t).await;
        match &s {
            UpdateStatus::Available {
                current,
                latest,
                notes,
                url,
            } => {
                assert_eq!(current, "0.4.0");
                assert_eq!(latest, "0.5.0");
                assert_eq!(notes.as_deref(), Some("修了几个 bug"));
                assert_eq!(url.as_deref(), Some("https://example.com/releases"));
            }
            other => panic!("{other:?}"),
        }
        // 只发了一次请求——检查而已，没有下载
        assert_eq!(t.n(), 1);
        assert!(
            s.message().contains("不会自动下载"),
            "提示要说清本程序不下载：{}",
            s.message()
        );
    }

    #[tokio::test]
    async fn the_same_or_older_version_reports_up_to_date() {
        for latest in ["0.4.0", "0.3.9", "0.4.0-beta"] {
            let t = Fake::body(&format!(r#"{{"version":"{latest}"}}"#));
            let s = check(Some("https://x/latest.json"), "0.4.0", &t).await;
            assert!(
                matches!(s, UpdateStatus::UpToDate { .. }),
                "latest={latest} 时报了 {s:?}"
            );
        }
    }

    #[tokio::test]
    async fn a_failed_check_is_never_shown_as_up_to_date() {
        // 把失败显示成「已是最新」会让用户以为自己在最新版上，
        // 而实际上程序根本没查成
        type Mk = Box<dyn Fn() -> Fake>;
        let cases: Vec<(&str, Mk)> = vec![
            ("连不上", Box::new(Fake::dead)),
            ("HTTP 500", Box::new(|| Fake::status(500))),
            ("HTTP 404", Box::new(|| Fake::status(404))),
            ("坏 JSON", Box::new(|| Fake::body("{not json"))),
            ("缺 version", Box::new(|| Fake::body(r#"{"notes":"x"}"#))),
            (
                "version 不是 semver",
                Box::new(|| Fake::body(r#"{"version":"最新版"}"#)),
            ),
        ];
        for (name, mk) in cases {
            let t = mk();
            let s = check(Some("https://x/latest.json"), "0.4.0", &t).await;
            assert!(
                matches!(s, UpdateStatus::Failed { .. }),
                "{name} 的结果是 {s:?}，不是 Failed"
            );
            assert!(!matches!(s, UpdateStatus::UpToDate { .. }), "{name}");
        }
    }

    #[tokio::test]
    async fn an_unknown_extra_field_does_not_fail_the_check() {
        // 宽容解析：多一个不认识的字段不该让整次检查失败
        let t = Fake::body(r#"{"version":"0.5.0","future_field":123,"platforms":{"win":{}}}"#);
        let s = check(Some("https://x/latest.json"), "0.4.0", &t).await;
        assert!(matches!(s, UpdateStatus::Available { .. }), "{s:?}");
    }

    #[tokio::test]
    async fn our_own_broken_version_string_is_reported_as_such() {
        let t = Fake::body(r#"{"version":"0.5.0"}"#);
        let s = check(Some("https://x/latest.json"), "not-a-version", &t).await;
        match s {
            UpdateStatus::Failed { detail } => {
                assert!(detail.contains("本程序自己的版本号"), "{detail}")
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(t.n(), 0, "自己版本号就不合法时不该发请求");
    }

    #[test]
    fn every_status_message_is_actionable() {
        let all = [
            UpdateStatus::NotConfigured,
            UpdateStatus::UpToDate {
                current: "0.4.0".into(),
            },
            UpdateStatus::Available {
                current: "0.4.0".into(),
                latest: "0.5.0".into(),
                notes: None,
                url: None,
            },
            UpdateStatus::Failed { detail: "x".into() },
        ];
        for s in &all {
            assert!(s.message().chars().count() >= 8, "{s:?}");
        }
        // 「没配」这一条必须说清「本程序不会主动联网」——那是承诺本身
        assert!(UpdateStatus::NotConfigured
            .message()
            .contains("不会主动联网"));
    }

    #[test]
    fn there_is_no_download_or_replace_capability_in_this_module() {
        // 「不下载不替换」是设计约束，不是「暂时还没做」。
        // 自动替换一个正在运行的二进制需要提权、需要处理「替换到一半断电」，
        // 而那两件事的失败模式是**用户的工具打不开了**——对一个要靠它救火的程序不值得。
        //
        // 用源码断言把这条钉住：本模块不得出现下载/写文件/执行的字眼。
        const SRC: &str = include_str!("update_check.rs");
        let prod = &SRC[..SRC.find("\n#[cfg(test)]").expect("本文件必须有测试段")];
        for banned in [
            "std::fs::write",
            "std::fs::File",
            "Command::new",
            "std::process",
        ] {
            assert!(
                !prod.contains(banned),
                "本模块出现了 `{banned}`——它只该做检查，不该下载或替换"
            );
        }
    }

    // ── 发布页链接的净化（这一段挡的是「远端 JSON 决定本机打开什么」）──

    #[test]
    fn only_https_urls_survive_sanitization() {
        for good in [
            "https://example.com/releases",
            "https://example.com/a?b=c#d",
            "HTTPS://EXAMPLE.COM/X", // scheme 大小写不敏感
        ] {
            assert_eq!(
                sanitize_external_url(good).as_deref(),
                Some(good.trim()),
                "{good} 本该通过"
            );
        }
        for bad in [
            "http://example.com/releases", // 明文：清单可被中间人整份换掉
            "file:///C:/Windows/System32/calc.exe",
            "javascript:alert(1)",
            "ms-msdt:/id",  // Windows 上已注册的自定义协议
            "vscode://x/y", // 任何第三方协议同理
            "//example.com/releases",
            "example.com/releases",
            "https://",                        // 只有 scheme，没有主机
            "  https://example.com/a b  ",     // 内嵌空格
            "https://example.com/a\r\nHost:x", // CRLF
            "https://example.com/\u{0}",
            "",
            "   ",
        ] {
            assert_eq!(sanitize_external_url(bad), None, "{bad:?} 本该被挡下");
        }
        // 长度上限：2048 过、2049 不过
        let host = "https://e.com/";
        let ok = format!("{host}{}", "a".repeat(2048 - host.len()));
        assert_eq!(ok.len(), 2048);
        assert!(sanitize_external_url(&ok).is_some());
        assert!(sanitize_external_url(&format!("{ok}a")).is_none());
    }

    #[tokio::test]
    async fn a_hostile_url_in_the_manifest_becomes_no_link_at_all() {
        // 「有新版本」仍然要告诉用户——只是不给一个可点的按钮。
        // 若这里改成让整次检查 Failed，一份带坏链接的清单就能让用户永远看不到
        // 「有新版本」这个事实，那是把可用性交给了对方。
        let t = Fake::body(r#"{"version":"0.5.0","url":"file:///C:/Windows/System32/calc.exe"}"#);
        let s = check(Some("https://x/latest.json"), "0.4.0", &t).await;
        match s {
            UpdateStatus::Available { latest, url, .. } => {
                assert_eq!(latest, "0.5.0");
                assert_eq!(url, None, "非 https 链接不该出现在给前端的结果里");
            }
            other => panic!("应报有新版本，实得 {other:?}"),
        }
    }

    #[tokio::test]
    async fn an_https_url_in_the_manifest_is_passed_through_verbatim() {
        // 反向对照：上一条若因 sanitize 恒返 None 而通过，这一条会红。
        let t = Fake::body(r#"{"version":"0.5.0","url":"https://example.com/releases/0.5.0"}"#);
        let s = check(Some("https://x/latest.json"), "0.4.0", &t).await;
        match s {
            UpdateStatus::Available { url, .. } => {
                assert_eq!(url.as_deref(), Some("https://example.com/releases/0.5.0"));
            }
            other => panic!("应报有新版本，实得 {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_plain_http_check_url_is_rejected_before_any_request() {
        // 与 settings_cmd 的白名单校验同口径。两处若走散，用户能存进库的地址
        // 会有一部分在检查时才被拒——错误出现在点按钮的那一刻，而不是填写的那一刻。
        let t = Fake::body("{}");
        let s = check(Some("http://example.com/latest.json"), "0.4.0", &t).await;
        assert!(matches!(s, UpdateStatus::Failed { .. }), "{s:?}");
        assert_eq!(t.n(), 0, "明文 http 地址仍然发了请求");
    }

    #[tokio::test]
    async fn overlong_release_notes_are_truncated() {
        // 备注是远端给的任意长文本，会被显示出来。不限长的话，一份 10 MB 的
        // notes 会原样穿过 IPC 进到界面里。
        let long = "啊".repeat(5000);
        let t = Fake::body(&format!(r#"{{"version":"0.5.0","notes":"{long}"}}"#));
        let s = check(Some("https://x/latest.json"), "0.4.0", &t).await;
        match s {
            UpdateStatus::Available { notes, .. } => {
                let n = notes.expect("应带备注");
                assert_eq!(n.chars().count(), 2001, "2000 字 + 省略号");
                assert!(n.ends_with('…'));
            }
            other => panic!("应报有新版本，实得 {other:?}"),
        }
    }

    #[tokio::test]
    async fn short_release_notes_are_untouched() {
        // 反向对照：上一条若因「总是截断」而通过，这一条会红。
        let t = Fake::body(r#"{"version":"0.5.0","notes":"修了几个 bug"}"#);
        let s = check(Some("https://x/latest.json"), "0.4.0", &t).await;
        match s {
            UpdateStatus::Available { notes, .. } => {
                assert_eq!(notes.as_deref(), Some("修了几个 bug"));
            }
            other => panic!("应报有新版本，实得 {other:?}"),
        }
    }
}
