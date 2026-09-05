//! HTTP 传输层与端到端调用（M2 出口第 2 项的最后一段）。
//!
//! # 为什么要一个可注入的 trait
//!
//! [`wire`](crate::wire) 已经把「构造什么、怎么解析」做成了纯函数。本模块只剩一件事：
//! 把那份 [`HttpRequestSpec`] 真的发出去。但这一件事必须能在**不发包**的前提下被测试——
//! 否则 provider 调用的整条路径（配置校验 → 构造 → 发送 → 状态码映射 → 解析）
//! 就只有两端有测试、中间那段接线没有。而接线恰恰是最容易错的地方
//! （忘记把状态码交给 `map_error`、把 429 当成正常响应去解析、把 `Retry-After` 丢掉）。
//!
//! 所以 [`Transport`] 是个 trait，生产用 [`ReqwestTransport`]，测试用假的。
//!
//! # 超时不是可选项
//!
//! 一个不回包的 provider 不能让 UI 永久转圈。[`ReqwestTransport`] 带**硬超时**，
//! 且区分「连接超时」与「整体超时」：前者短（连不上要快点告诉用户），
//! 后者长（模型生成本来就慢）。
//!
//! 这与 `fs_sshengine` 给每段认证都设预算是同一条纪律——那次的教训是
//! agent 探测漏在预算之外，一个本机的坏管道就能让整次连接永远停在「正在认证」。

use crate::provider::{ProviderConfig, ProviderError};
use crate::wire::{self, ChatRequest, ChatResponse, HttpRequestSpec};
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

/// 建立连接的预算。短——连不上要快点告诉用户，而不是让他等一分钟。
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// 一次请求从发出到收完的总预算。
///
/// 长，因为模型生成本来就慢（长回答几十秒是正常的）。但**必须有**：
/// 没有上限的请求会让 UI 永久转圈，而用户唯一能做的是杀进程。
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(180);

/// 一次 HTTP 响应，只保留分级判定需要的那几项。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    /// `Retry-After` 头（秒）。限流提示要用它给出「等多久」。
    pub retry_after_secs: Option<u64>,
    pub body: String,
}

impl HttpResponse {
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// 发请求的能力。
///
/// 返回装箱 future 而不是用 `async fn`：`async fn` in trait 不是 dyn 兼容的，
/// 而这里需要 `dyn Transport`（app 层持有一个，测试换成假的）。
pub trait Transport: Send + Sync {
    fn send<'a>(
        &'a self,
        spec: &'a HttpRequestSpec,
    ) -> Pin<Box<dyn Future<Output = Result<HttpResponse, ProviderError>> + Send + 'a>>;
}

/// 生产实现。
pub struct ReqwestTransport {
    client: reqwest::Client,
}

impl ReqwestTransport {
    /// 建一个带超时的客户端。
    ///
    /// 失败返回 `Err`：建不出客户端通常意味着 TLS 后端初始化失败（比如系统信任库读不到），
    /// 那时该明确报错，而不是留一个每次调用都失败的对象。
    pub fn new() -> Result<Self, ProviderError> {
        let client = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            // 不跟随重定向：AI API 不该重定向，而跟随重定向会把认证头带到
            // 一个我们没打算认证的主机上去（这是 SSRF 类问题的经典入口）。
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| ProviderError::Transport {
                detail: format!("HTTP 客户端初始化失败：{e}"),
            })?;
        Ok(Self { client })
    }
}

impl Transport for ReqwestTransport {
    fn send<'a>(
        &'a self,
        spec: &'a HttpRequestSpec,
    ) -> Pin<Box<dyn Future<Output = Result<HttpResponse, ProviderError>> + Send + 'a>> {
        Box::pin(async move {
            // 按 spec.method 分派，**不写死 POST**。
            //
            // 第一版这里是 `self.client.post(...)`，而结构体上明明有个 `method` 字段
            // ——字段声称可配置、实现写死，是最难发现的那种不一致：加一个 GET 调用方
            // 时它会静默地发成 POST，而对端多半回 405，于是看起来像「对方接口不对」。
            let mut rb = match spec.method {
                "GET" => self.client.get(&spec.url),
                "POST" => self.client.post(&spec.url),
                other => {
                    return Err(ProviderError::Transport {
                        detail: format!("不支持的 HTTP 方法 {other}"),
                    })
                }
            };
            for (k, v) in &spec.headers {
                rb = rb.header(k.as_str(), v.as_str());
            }
            let resp =
                rb.body(spec.body.clone())
                    .send()
                    .await
                    .map_err(|e| ProviderError::Transport {
                        // 刻意只带 e 的显示串，不带 spec：spec 里有 key
                        detail: e.to_string(),
                    })?;
            let status = resp.status().as_u16();
            let retry_after_secs = resp
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.trim().parse::<u64>().ok());
            let body = resp.text().await.map_err(|e| ProviderError::Transport {
                detail: format!("读取响应体失败：{e}"),
            })?;
            Ok(HttpResponse {
                status,
                retry_after_secs,
                body,
            })
        })
    }
}

/// 完整的一次非流式调用：校验 → 构造 → 发送 → 状态码映射 → 解析。
///
/// 这个函数存在的意义就是把那五步的**接线**放在一处并让它可测。
/// 最容易错的两处：
/// - 拿到非 2xx 却仍去 `parse_response`（于是错误体被当成回答解析，得到空文本或
///   一个 `MalformedResponse`，而真正的原因 401 被丢掉了）；
/// - 把 `Retry-After` 丢掉（于是限流提示只能说「稍后再试」而说不出「等 30 秒」）。
pub async fn chat(
    cfg: &ProviderConfig,
    req: &ChatRequest,
    transport: &dyn Transport,
) -> Result<ChatResponse, ProviderError> {
    let spec = wire::build_request(cfg, req)?;
    let resp = transport.send(&spec).await?;
    if !resp.is_success() {
        return Err(wire::map_error(
            resp.status,
            &resp.body,
            resp.retry_after_secs,
        ));
    }
    wire::parse_response(cfg.kind, &resp.body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::ProviderKind;
    use std::sync::Mutex;

    /// 记录收到的请求、按脚本回响应。
    struct FakeTransport {
        seen: Mutex<Vec<HttpRequestSpec>>,
        reply: Result<HttpResponse, ProviderError>,
    }

    impl FakeTransport {
        fn ok(status: u16, body: &str) -> Self {
            Self {
                seen: Mutex::new(Vec::new()),
                reply: Ok(HttpResponse {
                    status,
                    retry_after_secs: None,
                    body: body.into(),
                }),
            }
        }
        fn with_retry_after(status: u16, body: &str, secs: u64) -> Self {
            Self {
                seen: Mutex::new(Vec::new()),
                reply: Ok(HttpResponse {
                    status,
                    retry_after_secs: Some(secs),
                    body: body.into(),
                }),
            }
        }
        fn failing(e: ProviderError) -> Self {
            Self {
                seen: Mutex::new(Vec::new()),
                reply: Err(e),
            }
        }
    }

    impl Transport for FakeTransport {
        fn send<'a>(
            &'a self,
            spec: &'a HttpRequestSpec,
        ) -> Pin<Box<dyn Future<Output = Result<HttpResponse, ProviderError>> + Send + 'a>>
        {
            self.seen.lock().unwrap().push(spec.clone());
            let r = self.reply.clone();
            Box::pin(async move { r })
        }
    }

    fn cfg(kind: ProviderKind) -> ProviderConfig {
        ProviderConfig {
            kind,
            base_url: kind.suggested_base_url().into(),
            model: "m".into(),
            api_key: kind.needs_api_key().then(|| "k".to_string()),
            allow_screen_context: false,
        }
    }

    fn req() -> ChatRequest {
        ChatRequest {
            system: "s".into(),
            user: "u".into(),
            max_tokens: 64,
            stream: false,
        }
    }

    const ANTHROPIC_OK: &str =
        r#"{"content":[{"type":"text","text":"ok"}],"stop_reason":"end_turn"}"#;

    #[tokio::test]
    async fn a_successful_call_returns_the_parsed_answer() {
        let t = FakeTransport::ok(200, ANTHROPIC_OK);
        let r = chat(&cfg(ProviderKind::Anthropic), &req(), &t)
            .await
            .unwrap();
        assert_eq!(r.text, "ok");
        // 请求真的构造并发出去了
        assert_eq!(t.seen.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_non_2xx_never_reaches_the_response_parser() {
        // 这是本模块最容易错的一处：拿到 401 却仍去解析响应体，
        // 于是真正的原因被丢掉、用户看到的是「响应无法解析」。
        let t = FakeTransport::ok(401, r#"{"error":{"message":"bad key"}}"#);
        let e = chat(&cfg(ProviderKind::Anthropic), &req(), &t)
            .await
            .unwrap_err();
        assert!(
            matches!(e, ProviderError::Unauthorized { .. }),
            "401 映射成了 {e:?}"
        );
    }

    #[tokio::test]
    async fn every_error_status_maps_through_rather_than_parsing() {
        for (status, want) in [
            (401u16, "Unauthorized"),
            (403, "Unauthorized"),
            (404, "ModelNotFound"),
            (429, "RateLimited"),
            (500, "ServerError"),
            (503, "ServerError"),
        ] {
            let t = FakeTransport::ok(status, "{}");
            let e = chat(&cfg(ProviderKind::Anthropic), &req(), &t)
                .await
                .unwrap_err();
            let got = format!("{e:?}");
            assert!(
                got.starts_with(want),
                "HTTP {status} 期望 {want}，实得 {got}"
            );
        }
    }

    #[tokio::test]
    async fn retry_after_survives_the_round_trip() {
        // 丢了它，限流提示只能说「稍后再试」而说不出「等 30 秒」
        let t = FakeTransport::with_retry_after(429, "{}", 30);
        let e = chat(&cfg(ProviderKind::Anthropic), &req(), &t)
            .await
            .unwrap_err();
        assert_eq!(
            e,
            ProviderError::RateLimited {
                retry_after_secs: Some(30),
                detail: "{}".into()
            }
        );
        assert!(e.user_message().contains("30"));
    }

    #[tokio::test]
    async fn a_bad_config_fails_before_any_packet_leaves() {
        // 出站面承诺的一部分：地址没填时不该有任何请求发出
        let mut c = cfg(ProviderKind::Anthropic);
        c.base_url = String::new();
        let t = FakeTransport::ok(200, ANTHROPIC_OK);
        let e = chat(&c, &req(), &t).await.unwrap_err();
        assert!(matches!(e, ProviderError::BadConfig(_)), "{e:?}");
        assert!(
            t.seen.lock().unwrap().is_empty(),
            "配置不合法时仍然发出了请求"
        );
    }

    #[tokio::test]
    async fn transport_errors_surface_as_transport_not_malformed() {
        // 连不上与「响应看不懂」给用户的建议完全不同
        let t = FakeTransport::failing(ProviderError::Transport {
            detail: "connection refused".into(),
        });
        let e = chat(&cfg(ProviderKind::Ollama), &req(), &t)
            .await
            .unwrap_err();
        assert!(matches!(e, ProviderError::Transport { .. }), "{e:?}");
        assert!(e.is_retryable());
    }

    #[tokio::test]
    async fn the_request_carries_the_auth_header_for_cloud_providers() {
        for kind in [ProviderKind::Anthropic, ProviderKind::OpenAiCompatible] {
            let t = FakeTransport::ok(200, ANTHROPIC_OK);
            // Anthropic 的样本对 OpenAI 解析会失败，这里只看请求形状
            let _ = chat(&cfg(kind), &req(), &t).await;
            let seen = t.seen.lock().unwrap();
            let spec = seen.first().expect("没发出请求");
            assert!(
                spec.headers
                    .iter()
                    .any(|(k, _)| k == "x-api-key" || k == "authorization"),
                "{kind} 的请求没带认证头：{:?}",
                spec.headers
            );
        }
    }

    #[tokio::test]
    async fn ollama_requests_carry_no_credentials() {
        let t = FakeTransport::ok(200, r#"{"message":{"content":"ok"},"done":true}"#);
        chat(&cfg(ProviderKind::Ollama), &req(), &t).await.unwrap();
        let seen = t.seen.lock().unwrap();
        let spec = seen.first().unwrap();
        assert!(
            !spec
                .headers
                .iter()
                .any(|(k, _)| k.eq_ignore_ascii_case("authorization")
                    || k.eq_ignore_ascii_case("x-api-key")),
            "本地 Ollama 的请求带了认证头：{:?}",
            spec.headers
        );
    }

    #[test]
    fn timeouts_are_set_and_ordered_sensibly() {
        // 连接预算必须短于总预算：反过来意味着「连不上」也要等满总预算才报错
        assert!(CONNECT_TIMEOUT < REQUEST_TIMEOUT);
        // 两者都必须有上限——没有上限的请求让 UI 永久转圈，
        // 而用户唯一能做的是杀进程（同 fs_sshengine 给每段认证设预算的教训）
        assert!(CONNECT_TIMEOUT.as_secs() > 0 && CONNECT_TIMEOUT.as_secs() <= 30);
        assert!(REQUEST_TIMEOUT.as_secs() >= 60 && REQUEST_TIMEOUT.as_secs() <= 600);
    }

    #[test]
    fn the_real_client_builds_and_does_not_follow_redirects() {
        // 跟随重定向会把认证头带到一个我们没打算认证的主机上去（SSRF 类问题的经典入口）。
        // 这里只能证明客户端建得出来；「不跟随」由 builder 上那一行保证，
        // 其行为无法在不发包的前提下断言——如实记录。
        assert!(ReqwestTransport::new().is_ok());
    }

    #[test]
    fn the_method_field_is_actually_honoured() {
        // 字段声称可配置、实现写死 POST，是最难发现的那种不一致：
        // 加一个 GET 调用方时它会静默发成 POST，对端回 405，看起来像「对方接口不对」。
        // 这里只能证明「不认识的方法会明确报错」——GET/POST 的真实分派要发包才看得到，
        // 如实记录这条局限。
        let t = ReqwestTransport::new().unwrap();
        let spec = HttpRequestSpec {
            method: "DELETE",
            url: "http://127.0.0.1:1/x".into(),
            headers: vec![],
            body: String::new(),
        };
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let e = rt.block_on(t.send(&spec)).unwrap_err();
        match e {
            ProviderError::Transport { detail } => {
                assert!(detail.contains("DELETE"), "没点名是哪个方法：{detail}")
            }
            other => panic!("期望 Transport 错误，实得 {other:?}"),
        }
    }

    #[test]
    fn success_range_is_2xx_only() {
        for s in [200u16, 201, 204, 299] {
            assert!(HttpResponse {
                status: s,
                retry_after_secs: None,
                body: String::new()
            }
            .is_success());
        }
        for s in [100u16, 199, 300, 301, 400, 500] {
            assert!(!HttpResponse {
                status: s,
                retry_after_secs: None,
                body: String::new()
            }
            .is_success());
        }
    }
}
