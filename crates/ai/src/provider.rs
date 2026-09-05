//! Provider 抽象与配置态（总设计 §4.1 / M2 范围「Provider trait（Anthropic 原生 /
//! OpenAI 兼容 REST / Ollama / 错误提示式未配置态）」）。
//!
//! # 「未配置」是一等状态，不是错误
//!
//! M2 出口第 1 项：「未配置任何模型源时：AI 面板显示配置向导，**不崩溃、不降级为假数据**」。
//! 那句「不降级为假数据」是这个模块最重要的约束——它排除了一整类常见做法：
//! 内置一个默认 endpoint、塞一个试用 key、或者在没有 provider 时返回一段假的示例回答。
//!
//! 所以 [`ProviderState::Unconfigured`] 是枚举里的一支，而不是 `Option<Config>` 的 `None`
//! ——前者迫使每个调用点都显式处理它，后者会被 `unwrap_or_default()` 一笔带过。
//!
//! # 没有内置 endpoint
//!
//! 三种 provider 都要求调用方给出 `base_url`。Anthropic 与 OpenAI 兼容层有众所周知的
//! 官方地址，但**不写进代码**：写进去就意味着「装上就能连出去」，
//! 而 README 的出站面承诺是「只连你自己配置的目标」。
//! 默认地址由 UI 在**用户新建 provider 时**填进输入框（用户可见、可改），
//! 而不是由代码在后台兜底。

/// 三种 provider 形态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProviderKind {
    /// Anthropic 原生 Messages API。
    Anthropic,
    /// OpenAI 兼容的 `/chat/completions`（OpenAI 本体、Azure、各家兼容层、vLLM…）。
    OpenAiCompatible,
    /// 本地 Ollama 的 `/api/chat`。**协议不是 SSE 而是 NDJSON**，见 [`crate::wire`]。
    Ollama,
}

impl ProviderKind {
    /// 稳定串（settings 落库与审计用；**永不改名**）。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::OpenAiCompatible => "openai_compatible",
            Self::Ollama => "ollama",
        }
    }

    pub fn from_str_exact(s: &str) -> Option<Self> {
        match s {
            "anthropic" => Some(Self::Anthropic),
            "openai_compatible" => Some(Self::OpenAiCompatible),
            "ollama" => Some(Self::Ollama),
            _ => None,
        }
    }

    /// 这种 provider 需不需要 API key。
    ///
    /// Ollama 跑在本机、不认证。把它也要求 key 会让本地路径无端多一步，
    /// 而本地路径恰恰是「云 API 不可达环境（中国大陆）」下的一等公民（路线图风险条）。
    pub fn needs_api_key(self) -> bool {
        match self {
            Self::Anthropic | Self::OpenAiCompatible => true,
            Self::Ollama => false,
        }
    }

    /// 建 provider 时给用户**预填**在输入框里的地址。
    ///
    /// 注意这不是「默认值」——它不参与任何请求构造，只是 UI 的初始文本，
    /// 用户看得见、改得动、也可以清空。代码路径上没有任何地方会在
    /// `base_url` 缺失时悄悄用它兜底（那正是 [`ProviderConfig::validate`] 要挡的）。
    pub fn suggested_base_url(self) -> &'static str {
        match self {
            Self::Anthropic => "https://api.anthropic.com",
            Self::OpenAiCompatible => "https://api.openai.com/v1",
            Self::Ollama => "http://localhost:11434",
        }
    }
}

impl std::fmt::Display for ProviderKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 一个已配置好的 provider。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderConfig {
    pub kind: ProviderKind,
    /// 基地址，**必填**（见模块头「没有内置 endpoint」）。
    pub base_url: String,
    /// 模型名，**必填**。没有默认模型：模型名会变、会下线，
    /// 代码里写死一个就是给用户埋一个将来某天突然报 404 的坑。
    pub model: String,
    /// API key。`None` 对 Ollama 是正常的。
    pub api_key: Option<String>,
    /// §4.2 的按 Provider 开关：允许把屏幕内容发给**这一个** provider 吗。
    ///
    /// 按 provider 而不是全局，因为信任度本来就不同：
    /// 发给本机 Ollama 与发给某家云 API 是两个决定。
    pub allow_screen_context: bool,
}

/// 配置校验失败的原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    BaseUrlEmpty,
    BaseUrlNotHttp,
    ModelEmpty,
    ApiKeyMissing,
}

impl ConfigError {
    pub fn message(&self) -> &'static str {
        match self {
            Self::BaseUrlEmpty => "请填写 provider 的地址（本程序不内置任何默认地址）",
            Self::BaseUrlNotHttp => "地址必须以 http:// 或 https:// 开头",
            Self::ModelEmpty => "请填写模型名",
            Self::ApiKeyMissing => "这种 provider 需要 API key",
        }
    }
}

impl ProviderConfig {
    /// 校验这份配置能不能用来发请求。
    ///
    /// 校验在**构造请求之前**做，且失败返回错误而不是「用个默认值继续」——
    /// 后者会导致请求发到一个用户没打算发的地址上去。
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.base_url.trim().is_empty() {
            return Err(ConfigError::BaseUrlEmpty);
        }
        let u = self.base_url.trim().to_ascii_lowercase();
        if !(u.starts_with("http://") || u.starts_with("https://")) {
            return Err(ConfigError::BaseUrlNotHttp);
        }
        if self.model.trim().is_empty() {
            return Err(ConfigError::ModelEmpty);
        }
        if self.kind.needs_api_key()
            && self
                .api_key
                .as_deref()
                .map(str::trim)
                .unwrap_or("")
                .is_empty()
        {
            return Err(ConfigError::ApiKeyMissing);
        }
        Ok(())
    }
}

/// AI 层的整体配置态。
///
/// `Unconfigured` 是一支枚举而非 `Option::None`：它迫使每个调用点显式处理
/// 「还没配」这件事，而 `Option` 会被 `unwrap_or_default()` 一笔带过——
/// 那一笔带过的地方，就是「降级为假数据」的入口。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderState {
    /// 一个 provider 都没有。UI 应显示配置向导。
    Unconfigured,
    /// 有可用的 provider。
    Configured(ProviderConfig),
}

impl ProviderState {
    /// 从一组候选里选出可用的。全都不可用即 `Unconfigured`。
    ///
    /// 不返回「第一个」而是「第一个**校验通过**的」：一份填了一半的配置
    /// （填了 key 忘了模型名）不该让整个功能看起来是可用的，然后在发请求时才报错。
    pub fn pick(candidates: &[ProviderConfig]) -> Self {
        match candidates.iter().find(|c| c.validate().is_ok()) {
            Some(c) => Self::Configured(c.clone()),
            None => Self::Unconfigured,
        }
    }

    pub fn is_configured(&self) -> bool {
        matches!(self, Self::Configured(_))
    }

    pub fn config(&self) -> Option<&ProviderConfig> {
        match self {
            Self::Configured(c) => Some(c),
            Self::Unconfigured => None,
        }
    }
}

/// 一次 provider 调用可能出的错（§8.1「错误映射」）。
///
/// 分这么细是为了让 UI 能给出**可行动**的下一步：401 该去改 key，429 该等一会儿，
/// 404 该换模型名，超长该缩短上下文。一律回一句「请求失败」的话，
/// 用户唯一的下一步就是关掉这个功能。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderError {
    /// 还没配 provider。**不是**传输错误——它在发请求之前就该被拦住。
    NotConfigured,
    /// 配置本身不合法。
    BadConfig(ConfigError),
    /// 401/403：key 不对或没权限。
    Unauthorized { detail: String },
    /// 429：限流。`retry_after_secs` 来自响应头，可能没有。
    RateLimited {
        retry_after_secs: Option<u64>,
        detail: String,
    },
    /// 404 或模型名不存在。
    ModelNotFound { detail: String },
    /// 上下文超长（各家的错误码不同，统一映射到这一支）。
    ContextTooLong { detail: String },
    /// 5xx。
    ServerError { status: u16, detail: String },
    /// 其他 HTTP 状态。
    UnexpectedStatus { status: u16, detail: String },
    /// 连不上、超时、TLS 失败。
    Transport { detail: String },
    /// 连上了、状态码正常，但响应体不是我们能理解的形状。
    ///
    /// 单独一支而不是并进 `ServerError`：这一支意味着**我们的解析或对方的协议**有问题，
    /// 而不是对方挂了。给用户的建议完全不同（前者该报 bug，后者该重试）。
    MalformedResponse { detail: String },
}

impl ProviderError {
    /// 重试有没有意义。
    ///
    /// UI 与将来的 Agent 循环都要用它决定「显示重试按钮吗 / 自动重试吗」。
    /// 把不可重试的错也重试是在浪费用户的时间和对方的配额。
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::RateLimited { .. } | Self::ServerError { .. } | Self::Transport { .. }
        )
    }

    /// 给用户看的一句话，含下一步。
    pub fn user_message(&self) -> String {
        match self {
            Self::NotConfigured => "还没有配置模型源。请到 设置 → AI 添加一个 provider。".into(),
            Self::BadConfig(e) => e.message().into(),
            Self::Unauthorized { .. } => {
                "provider 拒绝了这次请求（认证失败）。请检查 API key 是否正确、是否过期。".into()
            }
            Self::RateLimited {
                retry_after_secs, ..
            } => match retry_after_secs {
                Some(s) => format!("被限流了，请约 {s} 秒后再试。"),
                None => "被限流了，请稍后再试。".into(),
            },
            Self::ModelNotFound { .. } => {
                "provider 说这个模型不存在。请检查模型名（模型会改名、会下线）。".into()
            }
            Self::ContextTooLong { .. } => {
                "上下文太长了。可以关掉「允许发送屏幕上下文」或换一个上下文窗口更大的模型。"
                    .into()
            }
            Self::ServerError { status, .. } => {
                format!("provider 侧出错（HTTP {status}），这通常是对方的临时故障，可以重试。")
            }
            Self::UnexpectedStatus { status, .. } => {
                // 最常见的成因是地址指向了另一个服务（少了 `/v1`、多了一段路径、
                // 或者填成了某个网关的首页），所以指引指向地址而不是「重试」。
                format!(
                    "provider 返回了意料之外的状态码 HTTP {status}。\
                     请检查地址是否指向了正确的 API 路径（最常见的是少了或多了 `/v1` 这类前缀）。"
                )
            }
            Self::Transport { .. } => {
                "连不上 provider。请检查网络、地址是否正确、以及是否需要走代理。".into()
            }
            Self::MalformedResponse { .. } => {
                "provider 的响应无法解析。这多半是本程序的适配器与对方协议版本不匹配，请报告这个问题。"
                    .into()
            }
        }
    }
}

impl std::fmt::Display for ProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.user_message())
    }
}

impl std::error::Error for ProviderError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(kind: ProviderKind) -> ProviderConfig {
        ProviderConfig {
            kind,
            base_url: kind.suggested_base_url().to_string(),
            model: "m".into(),
            api_key: kind.needs_api_key().then(|| "k".to_string()),
            allow_screen_context: false,
        }
    }

    #[test]
    fn unconfigured_is_a_state_not_an_error_value() {
        // M2 出口第 1 项的核心：没配 provider 时不崩溃、不假数据。
        // 它是枚举的一支，于是每个调用点必须显式处理。
        let s = ProviderState::pick(&[]);
        assert_eq!(s, ProviderState::Unconfigured);
        assert!(!s.is_configured());
        assert!(s.config().is_none());
    }

    #[test]
    fn a_half_filled_config_does_not_count_as_configured() {
        // 填了 key 忘了模型名的配置，不该让整个功能看起来可用、
        // 然后在真发请求时才报错
        let mut c = cfg(ProviderKind::Anthropic);
        c.model = String::new();
        assert_eq!(ProviderState::pick(&[c]), ProviderState::Unconfigured);
    }

    #[test]
    fn pick_takes_the_first_valid_not_the_first() {
        let mut bad = cfg(ProviderKind::Anthropic);
        bad.api_key = None;
        let good = cfg(ProviderKind::Ollama);
        let s = ProviderState::pick(&[bad, good.clone()]);
        assert_eq!(s, ProviderState::Configured(good));
    }

    #[test]
    fn no_provider_has_a_built_in_endpoint_used_for_requests() {
        // 「装上就能连出去」是 README 出站面承诺明确排除的。
        // 建议地址只是 UI 预填文本，空 base_url 必须**报错**而不是兜底。
        for k in [
            ProviderKind::Anthropic,
            ProviderKind::OpenAiCompatible,
            ProviderKind::Ollama,
        ] {
            let mut c = cfg(k);
            c.base_url = String::new();
            assert_eq!(
                c.validate(),
                Err(ConfigError::BaseUrlEmpty),
                "{k} 的空地址应报错而不是用建议值兜底"
            );
            // 只有空白也算空
            c.base_url = "   ".into();
            assert_eq!(c.validate(), Err(ConfigError::BaseUrlEmpty));
        }
    }

    #[test]
    fn there_is_no_default_model() {
        // 模型名会改、会下线。代码里写死一个就是给用户埋一个将来突然 404 的坑。
        for k in [
            ProviderKind::Anthropic,
            ProviderKind::OpenAiCompatible,
            ProviderKind::Ollama,
        ] {
            let mut c = cfg(k);
            c.model = "  ".into();
            assert_eq!(c.validate(), Err(ConfigError::ModelEmpty), "{k}");
        }
    }

    #[test]
    fn ollama_needs_no_api_key_but_the_cloud_ones_do() {
        // 本地路径是「云 API 不可达环境」下的一等公民，不该无端多一步
        assert!(!ProviderKind::Ollama.needs_api_key());
        assert!(ProviderKind::Anthropic.needs_api_key());
        assert!(ProviderKind::OpenAiCompatible.needs_api_key());

        let mut o = cfg(ProviderKind::Ollama);
        o.api_key = None;
        assert_eq!(o.validate(), Ok(()));

        let mut a = cfg(ProviderKind::Anthropic);
        a.api_key = Some("   ".into());
        assert_eq!(a.validate(), Err(ConfigError::ApiKeyMissing));
    }

    #[test]
    fn non_http_schemes_are_rejected() {
        // 挡住 `file://`、`ftp://`，也挡住手滑写成 `localhost:11434`（没有 scheme）
        for bad in [
            "localhost:11434",
            "file:///etc/passwd",
            "ftp://x",
            "//x",
            "x",
        ] {
            let mut c = cfg(ProviderKind::Ollama);
            c.base_url = bad.into();
            assert_eq!(
                c.validate(),
                Err(ConfigError::BaseUrlNotHttp),
                "{bad} 应被拒绝"
            );
        }
        // http 与 https 都行（本地 Ollama 就是 http）
        for ok in [
            "http://localhost:11434",
            "https://api.example.com/v1",
            "HTTPS://X",
        ] {
            let mut c = cfg(ProviderKind::Ollama);
            c.base_url = ok.into();
            assert_eq!(c.validate(), Ok(()), "{ok} 应通过");
        }
    }

    #[test]
    fn the_screen_context_switch_is_per_provider_and_defaults_off() {
        // 发给本机 Ollama 与发给某家云 API 是两个决定，所以开关按 provider
        let c = cfg(ProviderKind::Ollama);
        assert!(!c.allow_screen_context, "默认必须是关");
    }

    #[test]
    fn provider_kind_strings_round_trip_and_reject_variants() {
        for k in [
            ProviderKind::Anthropic,
            ProviderKind::OpenAiCompatible,
            ProviderKind::Ollama,
        ] {
            assert_eq!(ProviderKind::from_str_exact(k.as_str()), Some(k));
        }
        for bad in ["Anthropic", "openai", "OpenAI-compatible", "", "claude"] {
            assert_eq!(ProviderKind::from_str_exact(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn only_transient_errors_are_retryable() {
        // 把不可重试的错也重试是在浪费用户的时间和对方的配额
        assert!(ProviderError::RateLimited {
            retry_after_secs: None,
            detail: String::new()
        }
        .is_retryable());
        assert!(ProviderError::ServerError {
            status: 503,
            detail: String::new()
        }
        .is_retryable());
        assert!(ProviderError::Transport {
            detail: String::new()
        }
        .is_retryable());

        for e in [
            ProviderError::NotConfigured,
            ProviderError::Unauthorized {
                detail: String::new(),
            },
            ProviderError::ModelNotFound {
                detail: String::new(),
            },
            ProviderError::ContextTooLong {
                detail: String::new(),
            },
            ProviderError::MalformedResponse {
                detail: String::new(),
            },
            ProviderError::BadConfig(ConfigError::ModelEmpty),
        ] {
            assert!(!e.is_retryable(), "{e:?} 不该被判为可重试");
        }
    }

    #[test]
    fn every_error_tells_the_user_what_to_do_next() {
        // 一律回「请求失败」的话，用户唯一的下一步就是关掉这个功能
        let all = [
            ProviderError::NotConfigured,
            ProviderError::BadConfig(ConfigError::BaseUrlEmpty),
            ProviderError::Unauthorized { detail: "x".into() },
            ProviderError::RateLimited {
                retry_after_secs: Some(30),
                detail: "x".into(),
            },
            ProviderError::RateLimited {
                retry_after_secs: None,
                detail: "x".into(),
            },
            ProviderError::ModelNotFound { detail: "x".into() },
            ProviderError::ContextTooLong { detail: "x".into() },
            ProviderError::ServerError {
                status: 500,
                detail: "x".into(),
            },
            ProviderError::UnexpectedStatus {
                status: 418,
                detail: "x".into(),
            },
            ProviderError::Transport { detail: "x".into() },
            ProviderError::MalformedResponse { detail: "x".into() },
        ];
        for e in &all {
            let m = e.user_message();
            assert!(m.chars().count() >= 8, "{e:?} 的提示太短：{m}");
            // 每条都得给方向：或指出该改什么，或说明能不能重试
            let actionable = ["请", "可以", "检查", "换", "报告", "重试", "稍后", "添加"]
                .iter()
                .any(|k| m.contains(k));
            assert!(actionable, "{e:?} 的提示没有可行动的下一步：{m}");
        }
    }

    #[test]
    fn error_detail_never_leaks_into_the_user_message() {
        // detail 里可能带响应体片段，而那可能含 key 或屏幕内容。
        // 给用户看的那句话只用固定文案，不拼 detail。
        let e = ProviderError::Unauthorized {
            detail: "sk-secret-leaked-here".into(),
        };
        assert!(!e.user_message().contains("sk-secret-leaked-here"));
    }
}
