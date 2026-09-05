//! 三家 provider 的报文构造与解析（总设计 §8.1「Provider 适配器：录制回放，
//! 流式解析、tool call 解析、错误映射」）。
//!
//! # 为什么把「构造/解析」与「发请求」分开
//!
//! 本模块是**纯函数**：给它配置与请求，它返回一份 [`HttpRequestSpec`]；
//! 给它状态码与响应体，它返回结果或错误。它不知道 HTTP 客户端的存在。
//!
//! 这样分层是因为 bug 全在这一侧。三家的报文形状彼此不同，且各有陷阱：
//!
//! | | 认证头 | 路径 | 流式协议 | 错误体 |
//! |---|---|---|---|---|
//! | Anthropic | `x-api-key` + `anthropic-version` | `/v1/messages` | SSE，事件类型在 **data 的 `type` 字段**里 | `{"type":"error","error":{...}}` |
//! | OpenAI 兼容 | `Authorization: Bearer` | `/chat/completions` | SSE，以 `data: [DONE]` 收尾 | `{"error":{...}}` |
//! | Ollama | 无 | `/api/chat` | **NDJSON，不是 SSE** | `{"error":"..."}` |
//!
//! 最容易写错的是最后一列的 Ollama：它每行就是一个裸 JSON 对象，没有 `data: ` 前缀。
//! 拿 SSE 解析器去读它，每一行都会被当成「不认识的行」而静默跳过——
//! 表现是「请求成功但回答是空的」，而不是报错。
//!
//! # key 绝不进日志
//!
//! [`HttpRequestSpec`] 手写了 `Debug`，把认证头的值打成掩码。
//! 派生 `Debug` 的话，任何一句 `tracing::debug!(?spec)` 就把 key 写进了日志文件——
//! 而日志会被打包进取证包、贴进工单。

use crate::provider::{ProviderConfig, ProviderError, ProviderKind};
use serde_json::{json, Value};

/// 一次待发的 HTTP 请求，与具体客户端无关。
#[derive(Clone, PartialEq, Eq)]
pub struct HttpRequestSpec {
    /// 恒为 `POST`（三家的聊天接口都是）。留成字段而不是写死，是为了让签名不必将来改。
    pub method: &'static str,
    pub url: String,
    pub headers: Vec<(String, String)>,
    /// JSON 正文。
    pub body: String,
}

/// 值必须打掩码的请求头（小写比较）。
const SECRET_HEADERS: &[&str] = &["authorization", "x-api-key", "api-key", "x-goog-api-key"];

fn is_secret_header(name: &str) -> bool {
    SECRET_HEADERS.contains(&name.to_ascii_lowercase().as_str())
}

impl std::fmt::Debug for HttpRequestSpec {
    /// 手写而非派生：派生会把 API key 原样打出来，而 `tracing::debug!(?spec)`
    /// 这一句就足以把 key 写进日志文件——日志会被打包进取证包、贴进工单。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let headers: Vec<(&str, &str)> = self
            .headers
            .iter()
            .map(|(k, v)| {
                (
                    k.as_str(),
                    if is_secret_header(k) {
                        "<redacted>"
                    } else {
                        v.as_str()
                    },
                )
            })
            .collect();
        f.debug_struct("HttpRequestSpec")
            .field("method", &self.method)
            .field("url", &self.url)
            .field("headers", &headers)
            // 正文里没有 key，但可能有屏幕内容；只记长度
            .field("body_len", &self.body.len())
            .finish()
    }
}

/// 一次聊天请求（provider 无关）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatRequest {
    pub system: String,
    pub user: String,
    pub max_tokens: u32,
    pub stream: bool,
}

/// 模型返回的一次工具调用。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// 参数的 JSON **文本**，不是解析后的值。
    ///
    /// 保持文本形态是有意的：OpenAI 那边给的本来就是一个 JSON 字符串
    /// （`function.arguments` 是 string），而流式场景下它是**分片到达**的，
    /// 只有拼完整才能解析。提前解析会让分片路径无法复用同一个类型。
    pub arguments_json: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
}

/// 非流式响应。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ChatResponse {
    pub text: String,
    pub tool_calls: Vec<ToolCall>,
    pub stop_reason: Option<String>,
    pub usage: Option<Usage>,
}

/// 流式响应里的一个事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamEvent {
    /// 一段正文增量。
    TextDelta(String),
    /// 一次工具调用开始（拿到了 id 与名字）。
    ToolCallStart { id: String, name: String },
    /// 工具参数的增量（分片到达）。
    ToolArgsDelta(String),
    /// 流结束。
    Done,
    /// 这一行不携带事件：SSE 的空行、`event:` 行、注释行，或 provider 的心跳。
    ///
    /// 显式一支而不是返回 `Option::None`：`None` 会诱使调用方写
    /// `if let Some(e) = …`，于是「解析失败」和「这行本来就没内容」走同一条路。
    Ignored,
}

// ══════════════════════════════════════════════════════════════════════
// 请求构造
// ══════════════════════════════════════════════════════════════════════

/// 拼接基地址与路径，避免重复段。
///
/// 用户填的地址五花八门：带不带尾斜杠、带不带 `/v1`。
/// `https://api.anthropic.com/v1` + `/v1/messages` = `/v1/v1/messages` 是最常见的一种手滑，
/// 而它的表现是 404——用户会以为是模型名写错了。
fn join_url(base: &str, path: &str) -> String {
    let base = base.trim().trim_end_matches('/');
    let path = path.trim_start_matches('/');
    // path 的首段若已是 base 的末段，就不重复它
    if let Some((first, rest)) = path.split_once('/') {
        if base
            .to_ascii_lowercase()
            .ends_with(&format!("/{}", first.to_ascii_lowercase()))
        {
            return format!("{base}/{rest}");
        }
    }
    format!("{base}/{path}")
}

/// 构造一次请求。
pub fn build_request(
    cfg: &ProviderConfig,
    req: &ChatRequest,
) -> Result<HttpRequestSpec, ProviderError> {
    cfg.validate().map_err(ProviderError::BadConfig)?;
    let key = cfg.api_key.as_deref().unwrap_or("").trim().to_string();
    let mut headers = vec![("content-type".to_string(), "application/json".to_string())];

    let (url, body) = match cfg.kind {
        ProviderKind::Anthropic => {
            headers.push(("x-api-key".into(), key));
            // 版本头是必填的，且**钉死**而不是跟随「最新」：
            // Anthropic 用它做破坏性变更的隔离，跟随最新等于把自己暴露给未来的改动。
            headers.push(("anthropic-version".into(), ANTHROPIC_VERSION.into()));
            let body = json!({
                "model": cfg.model,
                "max_tokens": req.max_tokens,
                "system": req.system,
                "messages": [{ "role": "user", "content": req.user }],
                "stream": req.stream,
            });
            (join_url(&cfg.base_url, "/v1/messages"), body)
        }
        ProviderKind::OpenAiCompatible => {
            headers.push(("authorization".into(), format!("Bearer {key}")));
            let body = json!({
                "model": cfg.model,
                "max_tokens": req.max_tokens,
                "messages": [
                    { "role": "system", "content": req.system },
                    { "role": "user", "content": req.user },
                ],
                "stream": req.stream,
            });
            (join_url(&cfg.base_url, "/chat/completions"), body)
        }
        ProviderKind::Ollama => {
            // 不加认证头：Ollama 跑在本机、不认证。加一个空的 Bearer 反而会让
            // 前面挂了反代的部署行为诡异。
            let body = json!({
                "model": cfg.model,
                "messages": [
                    { "role": "system", "content": req.system },
                    { "role": "user", "content": req.user },
                ],
                "stream": req.stream,
                // Ollama 的上限参数在 options 里，名字也不同
                "options": { "num_predict": req.max_tokens },
            });
            (join_url(&cfg.base_url, "/api/chat"), body)
        }
    };

    Ok(HttpRequestSpec {
        method: "POST",
        url,
        headers,
        body: serde_json::to_string(&body).map_err(|e| ProviderError::MalformedResponse {
            detail: format!("请求体序列化失败：{e}"),
        })?,
    })
}

/// Anthropic 的 API 版本头。**钉死**，不跟随「最新」。
pub const ANTHROPIC_VERSION: &str = "2023-06-01";

// ══════════════════════════════════════════════════════════════════════
// 错误映射
// ══════════════════════════════════════════════════════════════════════

/// 从响应体里尽力抠出一句人类可读的错误说明。
///
/// 三家的错误体形状都不同，且都可能压根不是 JSON（网关返回 HTML 错误页很常见）。
/// 抠不出来就退回截断的原文——**截断**是必要的：错误体可能是一整页 HTML。
fn error_detail(body: &str) -> String {
    const MAX: usize = 400;
    let trimmed = |s: &str| -> String {
        let s = s.trim();
        if s.chars().count() <= MAX {
            s.to_string()
        } else {
            let cut: String = s.chars().take(MAX).collect();
            format!("{cut}…")
        }
    };
    match serde_json::from_str::<Value>(body) {
        Ok(v) => {
            // Anthropic / OpenAI: {"error":{"message":"..."}}；Ollama: {"error":"..."}
            let m = v
                .get("error")
                .and_then(|e| {
                    e.get("message")
                        .and_then(Value::as_str)
                        .or_else(|| e.as_str())
                })
                .or_else(|| v.get("message").and_then(Value::as_str));
            match m {
                Some(m) => trimmed(m),
                None => trimmed(body),
            }
        }
        Err(_) => trimmed(body),
    }
}

/// 上下文超长的判据。
///
/// 三家的状态码都不专属（Anthropic 用 400，OpenAI 用 400 带 `context_length_exceeded`），
/// 所以只能按错误文本认。这条判据**刻意宽松**：认错的代价是给用户一句更具体的建议，
/// 认不出的代价是他看到一句无用的「请求无效」。
fn looks_like_context_overflow(detail: &str) -> bool {
    let d = detail.to_ascii_lowercase();
    [
        "context length",
        "context_length_exceeded",
        "maximum context",
        "too many tokens",
        "prompt is too long",
        "reduce the length",
        "exceeds the maximum",
    ]
    .iter()
    .any(|k| d.contains(k))
}

/// 把 HTTP 状态码 + 响应体映射成 [`ProviderError`]。
///
/// `retry_after` 来自 `Retry-After` 响应头（秒），没有就传 `None`。
pub fn map_error(status: u16, body: &str, retry_after: Option<u64>) -> ProviderError {
    let detail = error_detail(body);
    match status {
        401 | 403 => ProviderError::Unauthorized { detail },
        404 => ProviderError::ModelNotFound { detail },
        429 => ProviderError::RateLimited {
            retry_after_secs: retry_after,
            detail,
        },
        400 | 413 | 422 if looks_like_context_overflow(&detail) => {
            ProviderError::ContextTooLong { detail }
        }
        // 400 里也藏着「模型不存在」——Ollama 就是这么回的
        400 | 422 if detail.to_ascii_lowercase().contains("not found") => {
            ProviderError::ModelNotFound { detail }
        }
        500..=599 => ProviderError::ServerError { status, detail },
        s => ProviderError::UnexpectedStatus { status: s, detail },
    }
}

// ══════════════════════════════════════════════════════════════════════
// 非流式响应解析
// ══════════════════════════════════════════════════════════════════════

fn malformed(what: &str) -> ProviderError {
    ProviderError::MalformedResponse {
        detail: what.to_string(),
    }
}

/// 解析非流式响应。
pub fn parse_response(kind: ProviderKind, body: &str) -> Result<ChatResponse, ProviderError> {
    let v: Value =
        serde_json::from_str(body).map_err(|e| malformed(&format!("响应体不是 JSON：{e}")))?;
    match kind {
        ProviderKind::Anthropic => parse_anthropic(&v),
        ProviderKind::OpenAiCompatible => parse_openai(&v),
        ProviderKind::Ollama => parse_ollama(&v),
    }
}

fn parse_anthropic(v: &Value) -> Result<ChatResponse, ProviderError> {
    // 有些部署在 200 里塞错误体
    if v.get("type").and_then(Value::as_str) == Some("error") {
        return Err(ProviderError::UnexpectedStatus {
            status: 200,
            detail: error_detail(&v.to_string()),
        });
    }
    let blocks = v
        .get("content")
        .and_then(Value::as_array)
        .ok_or_else(|| malformed("Anthropic 响应缺少 content 数组"))?;
    let mut text = String::new();
    let mut tool_calls = Vec::new();
    for b in blocks {
        match b.get("type").and_then(Value::as_str) {
            Some("text") => {
                text.push_str(b.get("text").and_then(Value::as_str).unwrap_or_default());
            }
            Some("tool_use") => tool_calls.push(ToolCall {
                id: b
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .into(),
                name: b
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .into(),
                arguments_json: b
                    .get("input")
                    .map(Value::to_string)
                    .unwrap_or_else(|| "{}".into()),
            }),
            // 未知块类型跳过而不是报错：新版 API 会加块类型，
            // 为此报错等于让一次协议升级把功能整个打死
            _ => {}
        }
    }
    Ok(ChatResponse {
        text,
        tool_calls,
        stop_reason: v
            .get("stop_reason")
            .and_then(Value::as_str)
            .map(str::to_string),
        usage: v.get("usage").map(|u| Usage {
            input_tokens: u.get("input_tokens").and_then(Value::as_u64).unwrap_or(0) as u32,
            output_tokens: u.get("output_tokens").and_then(Value::as_u64).unwrap_or(0) as u32,
        }),
    })
}

fn parse_openai(v: &Value) -> Result<ChatResponse, ProviderError> {
    if v.get("error").is_some() {
        return Err(ProviderError::UnexpectedStatus {
            status: 200,
            detail: error_detail(&v.to_string()),
        });
    }
    let choice = v
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .ok_or_else(|| malformed("OpenAI 兼容响应缺少 choices[0]"))?;
    let msg = choice
        .get("message")
        .ok_or_else(|| malformed("OpenAI 兼容响应缺少 choices[0].message"))?;
    // content 可以是 null（纯 tool call 的回复），那是合法的
    let text = msg
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let tool_calls = msg
        .get("tool_calls")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|t| ToolCall {
                    id: t
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .into(),
                    name: t
                        .get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .into(),
                    // 这里本来就是**字符串**形态的 JSON，不是对象
                    arguments_json: t
                        .get("function")
                        .and_then(|f| f.get("arguments"))
                        .and_then(Value::as_str)
                        .unwrap_or("{}")
                        .into(),
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(ChatResponse {
        text: text.to_string(),
        tool_calls,
        stop_reason: choice
            .get("finish_reason")
            .and_then(Value::as_str)
            .map(str::to_string),
        usage: v.get("usage").map(|u| Usage {
            input_tokens: u.get("prompt_tokens").and_then(Value::as_u64).unwrap_or(0) as u32,
            output_tokens: u
                .get("completion_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0) as u32,
        }),
    })
}

fn parse_ollama(v: &Value) -> Result<ChatResponse, ProviderError> {
    if let Some(e) = v.get("error") {
        let detail = e.as_str().unwrap_or("").to_string();
        // Ollama 在 200 里回 `{"error":"model 'x' not found"}`
        return Err(if detail.to_ascii_lowercase().contains("not found") {
            ProviderError::ModelNotFound { detail }
        } else {
            ProviderError::UnexpectedStatus {
                status: 200,
                detail,
            }
        });
    }
    let msg = v
        .get("message")
        .ok_or_else(|| malformed("Ollama 响应缺少 message"))?;
    let text = msg
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let tool_calls = msg
        .get("tool_calls")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .enumerate()
                .map(|(i, t)| ToolCall {
                    // Ollama 不给 id，用序号补一个稳定的
                    id: format!("ollama-{i}"),
                    name: t
                        .get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .into(),
                    // Ollama 的 arguments 是**对象**而不是字符串
                    arguments_json: t
                        .get("function")
                        .and_then(|f| f.get("arguments"))
                        .map(Value::to_string)
                        .unwrap_or_else(|| "{}".into()),
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(ChatResponse {
        text: text.to_string(),
        tool_calls,
        stop_reason: v
            .get("done_reason")
            .and_then(Value::as_str)
            .map(str::to_string),
        usage: Some(Usage {
            input_tokens: v
                .get("prompt_eval_count")
                .and_then(Value::as_u64)
                .unwrap_or(0) as u32,
            output_tokens: v.get("eval_count").and_then(Value::as_u64).unwrap_or(0) as u32,
        }),
    })
}

// ══════════════════════════════════════════════════════════════════════
// 流式解析
// ══════════════════════════════════════════════════════════════════════

/// 解析流里的**一行**。
///
/// 按行而不是按块：调用方负责把字节流切成行（provider 的分块边界与行边界无关，
/// 一个 TCP 包里可能有半行）。切行这件事与协议无关，放在这里会让本函数必须持有状态。
pub fn parse_stream_line(kind: ProviderKind, line: &str) -> Result<StreamEvent, ProviderError> {
    let line = line.trim_end_matches(['\r', '\n']);
    match kind {
        ProviderKind::Ollama => parse_ollama_stream_line(line),
        ProviderKind::Anthropic => parse_sse_line(line, parse_anthropic_stream_data),
        ProviderKind::OpenAiCompatible => parse_sse_line(line, parse_openai_stream_data),
    }
}

/// SSE 的行层：剥掉 `data: ` 前缀，处理空行/注释/`event:` 行。
fn parse_sse_line(
    line: &str,
    f: impl Fn(&Value) -> Result<StreamEvent, ProviderError>,
) -> Result<StreamEvent, ProviderError> {
    let t = line.trim();
    if t.is_empty() || t.starts_with(':') {
        // 空行是事件分隔，`:` 开头是 SSE 注释（很多网关用它做心跳）
        return Ok(StreamEvent::Ignored);
    }
    // `event: xxx` 行不带负荷；负荷全在 `data:` 里
    let Some(payload) = t.strip_prefix("data:") else {
        return Ok(StreamEvent::Ignored);
    };
    let payload = payload.trim();
    if payload == "[DONE]" {
        return Ok(StreamEvent::Done);
    }
    if payload.is_empty() {
        return Ok(StreamEvent::Ignored);
    }
    let v: Value =
        serde_json::from_str(payload).map_err(|e| malformed(&format!("流式负荷不是 JSON：{e}")))?;
    f(&v)
}

fn parse_anthropic_stream_data(v: &Value) -> Result<StreamEvent, ProviderError> {
    // Anthropic 的事件类型在 data 的 `type` 字段里，不在 SSE 的 `event:` 行上
    // ——只看 `event:` 行的实现会拿不到任何内容。
    match v.get("type").and_then(Value::as_str) {
        Some("content_block_delta") => {
            let d = v.get("delta");
            match d.and_then(|d| d.get("type")).and_then(Value::as_str) {
                Some("text_delta") => Ok(StreamEvent::TextDelta(
                    d.and_then(|d| d.get("text"))
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                )),
                Some("input_json_delta") => Ok(StreamEvent::ToolArgsDelta(
                    d.and_then(|d| d.get("partial_json"))
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                )),
                _ => Ok(StreamEvent::Ignored),
            }
        }
        Some("content_block_start") => {
            let b = v.get("content_block");
            if b.and_then(|b| b.get("type")).and_then(Value::as_str) == Some("tool_use") {
                Ok(StreamEvent::ToolCallStart {
                    id: b
                        .and_then(|b| b.get("id"))
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .into(),
                    name: b
                        .and_then(|b| b.get("name"))
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .into(),
                })
            } else {
                Ok(StreamEvent::Ignored)
            }
        }
        Some("message_stop") => Ok(StreamEvent::Done),
        Some("error") => Err(ProviderError::UnexpectedStatus {
            status: 200,
            detail: error_detail(&v.to_string()),
        }),
        // ping / message_start / message_delta / content_block_stop 等
        _ => Ok(StreamEvent::Ignored),
    }
}

fn parse_openai_stream_data(v: &Value) -> Result<StreamEvent, ProviderError> {
    if v.get("error").is_some() {
        return Err(ProviderError::UnexpectedStatus {
            status: 200,
            detail: error_detail(&v.to_string()),
        });
    }
    let Some(delta) = v
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .and_then(|c| c.get("delta"))
    else {
        return Ok(StreamEvent::Ignored);
    };
    if let Some(t) = delta.get("content").and_then(Value::as_str) {
        if !t.is_empty() {
            return Ok(StreamEvent::TextDelta(t.to_string()));
        }
    }
    if let Some(tc) = delta
        .get("tool_calls")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
    {
        let name = tc
            .get("function")
            .and_then(|f| f.get("name"))
            .and_then(Value::as_str);
        // 首片带 name 与 id，后续片只带 arguments
        if let Some(name) = name {
            return Ok(StreamEvent::ToolCallStart {
                id: tc
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .into(),
                name: name.to_string(),
            });
        }
        if let Some(a) = tc
            .get("function")
            .and_then(|f| f.get("arguments"))
            .and_then(Value::as_str)
        {
            return Ok(StreamEvent::ToolArgsDelta(a.to_string()));
        }
    }
    Ok(StreamEvent::Ignored)
}

/// Ollama 的流是 **NDJSON**：每行一个裸 JSON 对象，没有 `data: ` 前缀。
///
/// 这是三家里最容易写错的一处。拿 SSE 解析器去读它，每行都会因为「没有 data: 前缀」
/// 而被判为 `Ignored`——表现是「请求成功但回答是空的」，不是报错。
fn parse_ollama_stream_line(line: &str) -> Result<StreamEvent, ProviderError> {
    let t = line.trim();
    if t.is_empty() {
        return Ok(StreamEvent::Ignored);
    }
    let v: Value =
        serde_json::from_str(t).map_err(|e| malformed(&format!("Ollama 流式行不是 JSON：{e}")))?;
    if let Some(e) = v.get("error").and_then(Value::as_str) {
        return Err(ProviderError::UnexpectedStatus {
            status: 200,
            detail: e.to_string(),
        });
    }
    if v.get("done").and_then(Value::as_bool) == Some(true) {
        return Ok(StreamEvent::Done);
    }
    let text = v
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    if text.is_empty() {
        return Ok(StreamEvent::Ignored);
    }
    Ok(StreamEvent::TextDelta(text.to_string()))
}

/// 把一整段流（多行）折叠成一个 [`ChatResponse`]。
///
/// 调用方通常边收边显示，但这个函数让「录制回放」测试能用一整段样本
/// 与非流式结果做对比——两条路径解析同一次回答应得到同样的文本，
/// 而那正是流式解析最容易出错的地方（丢首片、丢末片、把 `[DONE]` 当内容）。
pub fn fold_stream(kind: ProviderKind, body: &str) -> Result<ChatResponse, ProviderError> {
    let mut out = ChatResponse::default();
    let mut pending: Option<(String, String, String)> = None; // (id, name, args)
    for line in body.lines() {
        match parse_stream_line(kind, line)? {
            StreamEvent::TextDelta(t) => out.text.push_str(&t),
            StreamEvent::ToolCallStart { id, name } => {
                if let Some((i, n, a)) = pending.take() {
                    out.tool_calls.push(ToolCall {
                        id: i,
                        name: n,
                        arguments_json: a,
                    });
                }
                pending = Some((id, name, String::new()));
            }
            StreamEvent::ToolArgsDelta(a) => {
                if let Some((_, _, args)) = pending.as_mut() {
                    args.push_str(&a);
                }
            }
            StreamEvent::Done => break,
            StreamEvent::Ignored => {}
        }
    }
    if let Some((i, n, a)) = pending.take() {
        out.tool_calls.push(ToolCall {
            id: i,
            name: n,
            arguments_json: if a.is_empty() { "{}".into() } else { a },
        });
    }
    Ok(out)
}

// ══════════════════════════════════════════════════════════════════════
// 请求侧：多轮工具对话（M3 Agent，实现规格 §2 wire 增量）
// ══════════════════════════════════════════════════════════════════════

/// 组装一次**多轮工具对话**请求（Agent 的每个模型回合）。
///
/// 与 [`build_request`] 是兄弟而不是改造：M2 的单发契约（system + user、无 tools）
/// 保持唯一实现不动——动它会让 M2 的回放测试语义跟着漂。
///
/// 序列化形状与解析侧（`parse_anthropic` / `parse_openai` / `parse_ollama`）
/// **严格镜像**，parity 测试把「请求发出去的形状」与「回包解析认的形状」对拍
/// ——两侧各写一份迟早漂，漂了的表现是模型说「我不知道你在说什么」而这边
/// 看不出任何错误。
///
/// 非流式：Agent 一律 `stream:false`——三家非流式都回 usage（token 预算的账本），
/// 流式 `fold_stream` 不填 usage，会系统性触发保守上界。
pub fn build_conversation_request(
    cfg: &ProviderConfig,
    req: &crate::agent::history::ConversationRequest,
) -> Result<HttpRequestSpec, ProviderError> {
    cfg.validate().map_err(ProviderError::BadConfig)?;
    let key = cfg.api_key.as_deref().unwrap_or("").trim().to_string();
    let mut headers = vec![("content-type".to_string(), "application/json".to_string())];

    let (url, body) = match cfg.kind {
        ProviderKind::Anthropic => {
            headers.push(("x-api-key".into(), key));
            headers.push(("anthropic-version".into(), ANTHROPIC_VERSION.into()));
            let body = json!({
                "model": cfg.model,
                "max_tokens": req.max_tokens,
                "system": req.system,
                "messages": req.messages.iter().map(anthropic_message).collect::<Vec<_>>(),
                "tools": req.tools.iter().map(|t| json!({
                    "name": t.name,
                    "description": t.description,
                    "input_schema": serde_json::from_str::<Value>(&t.input_schema_json)
                        .unwrap_or_else(|_| json!({"type":"object"})),
                })).collect::<Vec<_>>(),
                "stream": false,
            });
            (join_url(&cfg.base_url, "/v1/messages"), body)
        }
        ProviderKind::OpenAiCompatible => {
            headers.push(("authorization".into(), format!("Bearer {key}")));
            let mut messages = vec![json!({ "role": "system", "content": req.system })];
            messages.extend(req.messages.iter().map(openai_message));
            let body = json!({
                "model": cfg.model,
                "max_tokens": req.max_tokens,
                "messages": messages,
                "tools": req.tools.iter().map(|t| json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": serde_json::from_str::<Value>(&t.input_schema_json)
                            .unwrap_or_else(|_| json!({"type":"object"})),
                    },
                })).collect::<Vec<_>>(),
                "stream": false,
            });
            (join_url(&cfg.base_url, "/chat/completions"), body)
        }
        ProviderKind::Ollama => {
            // 不加认证头（与 build_request 同理：Ollama 本机不认证，空 Bearer
            // 会让前面挂反代的部署行为诡异）。
            let mut messages = vec![json!({ "role": "system", "content": req.system })];
            messages.extend(req.messages.iter().map(ollama_message));
            let body = json!({
                "model": cfg.model,
                "messages": messages,
                "tools": req.tools.iter().map(|t| json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": serde_json::from_str::<Value>(&t.input_schema_json)
                            .unwrap_or_else(|_| json!({"type":"object"})),
                    },
                })).collect::<Vec<_>>(),
                "stream": false,
                "options": { "num_predict": req.max_tokens },
            });
            (join_url(&cfg.base_url, "/api/chat"), body)
        }
    };

    Ok(HttpRequestSpec {
        method: "POST",
        url,
        headers,
        body: serde_json::to_string(&body).map_err(|e| ProviderError::MalformedResponse {
            detail: format!("请求体序列化失败：{e}"),
        })?,
    })
}

/// Anthropic 一条消息的形状。工具结果是 **user 角色的 tool_result 内容块**——
/// 不是独立的 role（Anthropic 协议没有 tool 角色），这是与 OpenAI 最大的分叉，
/// 也是 parity 测试重点对拍的一格。`is_error` 是协议真字段，原样带。
fn anthropic_message(m: &crate::agent::history::ChatMessage) -> Value {
    use crate::agent::history::ChatMessage as M;
    match m {
        M::User(t) => json!({ "role": "user", "content": t }),
        M::UserAnswer(t) => json!({ "role": "user", "content": t }),
        M::Assistant { text, tool_calls } => {
            let mut blocks = vec![];
            if !text.is_empty() {
                blocks.push(json!({"type":"text","text":text}));
            }
            for c in tool_calls {
                // arguments_json 是文本；input 是**对象**。parse_anthropic 读 input。
                let input: Value =
                    serde_json::from_str(&c.arguments_json).unwrap_or_else(|_| json!({}));
                blocks.push(json!({
                    "type": "tool_use",
                    "id": c.id,
                    "name": c.name,
                    "input": input,
                }));
            }
            json!({ "role": "assistant", "content": blocks })
        }
        M::ToolResult {
            call_id,
            content,
            is_error,
        } => json!({
            "role": "user",
            "content": [{
                "type": "tool_result",
                "tool_use_id": call_id,
                "content": content,
                "is_error": is_error,
            }],
        }),
    }
}

/// OpenAI 一条消息的形状：tool 角色独立存在；assistant 的 tool_calls 里
/// `function.arguments` 是**字符串**（parse_openai 读的就是字符串）。
///
/// is_error 的落法：OpenAI 的 tool 消息**没有**结构化错误字段，标进 content
/// 头部——发明一个协议里不存在的字段，严格网关会整包拒收，那比错误标记
/// 丢失糟得多（模型丢了整段上下文）。
fn openai_message(m: &crate::agent::history::ChatMessage) -> Value {
    use crate::agent::history::ChatMessage as M;
    match m {
        M::User(t) | M::UserAnswer(t) => json!({ "role": "user", "content": t }),
        M::Assistant { text, tool_calls } => json!({
            "role": "assistant",
            "content": if text.is_empty() { Value::Null } else { json!(text) },
            "tool_calls": tool_calls.iter().map(|c| json!({
                "id": c.id,
                "type": "function",
                "function": { "name": c.name, "arguments": c.arguments_json },
            })).collect::<Vec<_>>(),
        }),
        M::ToolResult {
            call_id,
            content,
            is_error,
        } => {
            let content = if *is_error {
                format!("[工具调用失败] {content}")
            } else {
                content.clone()
            };
            json!({
                "role": "tool",
                "tool_call_id": call_id,
                "content": content,
            })
        }
    }
}

/// Ollama 一条消息的形状：`function.arguments` 是**对象**（parse_ollama 读对象）。
/// is_error 同样标进 content 头部（Ollama 也无结构化错误字段）。
fn ollama_message(m: &crate::agent::history::ChatMessage) -> Value {
    use crate::agent::history::ChatMessage as M;
    match m {
        M::User(t) | M::UserAnswer(t) => json!({ "role": "user", "content": t }),
        M::Assistant { text, tool_calls } => json!({
            "role": "assistant",
            "content": text,
            "tool_calls": tool_calls.iter().map(|c| json!({
                "function": {
                    "name": c.name,
                    // 对象，不是字符串——与 OpenAI 的分叉点
                    "arguments": serde_json::from_str::<Value>(&c.arguments_json)
                        .unwrap_or_else(|_| json!({})),
                },
            })).collect::<Vec<_>>(),
        }),
        M::ToolResult {
            call_id,
            content,
            is_error,
        } => {
            let content = if *is_error {
                format!("[工具调用失败] {content}")
            } else {
                content.clone()
            };
            json!({
                "role": "tool",
                // Ollama 的 tool 消息没有 tool_call_id 字段；content 是它能带的
                // 全部。id 并进 content 头部（服务端按序匹配工具调用）。
                "content": format!("[{call_id}] {content}"),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::ProviderConfig;

    fn cfg(kind: ProviderKind, base: &str) -> ProviderConfig {
        ProviderConfig {
            kind,
            base_url: base.into(),
            model: "test-model".into(),
            api_key: kind.needs_api_key().then(|| "secret-key-123".to_string()),
            allow_screen_context: false,
        }
    }

    fn req() -> ChatRequest {
        ChatRequest {
            system: "你是运维助手".into(),
            user: "查看 nginx 状态".into(),
            max_tokens: 1024,
            stream: false,
        }
    }

    // ── 请求构造 ──

    #[test]
    fn anthropic_request_shape() {
        let s = build_request(
            &cfg(ProviderKind::Anthropic, "https://api.anthropic.com"),
            &req(),
        )
        .unwrap();
        assert_eq!(s.method, "POST");
        assert_eq!(s.url, "https://api.anthropic.com/v1/messages");
        // 版本头必填且钉死
        assert!(s
            .headers
            .iter()
            .any(|(k, v)| k == "anthropic-version" && v == ANTHROPIC_VERSION));
        // 认证走 x-api-key 而不是 Bearer
        assert!(s
            .headers
            .iter()
            .any(|(k, v)| k == "x-api-key" && v == "secret-key-123"));
        assert!(!s.headers.iter().any(|(k, _)| k == "authorization"));
        let b: Value = serde_json::from_str(&s.body).unwrap();
        // system 是**顶层字段**，不是一条 message——写成 message 会被静默忽略
        assert_eq!(b["system"], "你是运维助手");
        assert_eq!(b["messages"][0]["role"], "user");
        assert_eq!(b["max_tokens"], 1024);
    }

    #[test]
    fn openai_request_shape() {
        let s = build_request(
            &cfg(ProviderKind::OpenAiCompatible, "https://api.openai.com/v1"),
            &req(),
        )
        .unwrap();
        assert_eq!(s.url, "https://api.openai.com/v1/chat/completions");
        assert!(s
            .headers
            .iter()
            .any(|(k, v)| k == "authorization" && v == "Bearer secret-key-123"));
        let b: Value = serde_json::from_str(&s.body).unwrap();
        // 与 Anthropic 相反：system 是 messages 里的第一条
        assert_eq!(b["messages"][0]["role"], "system");
        assert_eq!(b["messages"][1]["role"], "user");
    }

    #[test]
    fn ollama_request_shape_has_no_auth_header() {
        let s =
            build_request(&cfg(ProviderKind::Ollama, "http://localhost:11434"), &req()).unwrap();
        assert_eq!(s.url, "http://localhost:11434/api/chat");
        assert!(
            !s.headers.iter().any(|(k, _)| is_secret_header(k)),
            "本地 Ollama 不该带认证头：{:?}",
            s.headers
        );
        let b: Value = serde_json::from_str(&s.body).unwrap();
        // 上限参数在 options.num_predict，不是 max_tokens
        assert_eq!(b["options"]["num_predict"], 1024);
    }

    #[test]
    fn url_join_does_not_duplicate_a_version_segment() {
        // 用户把 `/v1` 填进 base 是最常见的写法，重复拼会 404，
        // 而用户会以为是模型名写错了
        for base in [
            "https://api.anthropic.com/v1",
            "https://api.anthropic.com/v1/",
            "https://api.anthropic.com",
            "https://api.anthropic.com/",
        ] {
            let s = build_request(&cfg(ProviderKind::Anthropic, base), &req()).unwrap();
            assert_eq!(
                s.url, "https://api.anthropic.com/v1/messages",
                "base={base} 拼出了 {}",
                s.url
            );
        }
    }

    #[test]
    fn stream_flag_is_carried_into_the_body() {
        for kind in [
            ProviderKind::Anthropic,
            ProviderKind::OpenAiCompatible,
            ProviderKind::Ollama,
        ] {
            let mut r = req();
            r.stream = true;
            let s = build_request(&cfg(kind, kind.suggested_base_url()), &r).unwrap();
            let b: Value = serde_json::from_str(&s.body).unwrap();
            assert_eq!(b["stream"], true, "{kind} 没把 stream 带进请求体");
        }
    }

    #[test]
    fn an_invalid_config_is_rejected_before_any_request_is_built() {
        let mut c = cfg(ProviderKind::Anthropic, "");
        c.base_url = String::new();
        assert!(matches!(
            build_request(&c, &req()),
            Err(ProviderError::BadConfig(_))
        ));
    }

    #[test]
    fn debug_never_prints_the_api_key() {
        // `tracing::debug!(?spec)` 一句就足以把 key 写进日志文件，
        // 而日志会被打包进取证包、贴进工单
        for kind in [ProviderKind::Anthropic, ProviderKind::OpenAiCompatible] {
            let s = build_request(&cfg(kind, kind.suggested_base_url()), &req()).unwrap();
            let d = format!("{s:?}");
            assert!(
                !d.contains("secret-key-123"),
                "{kind} 的 Debug 泄露了 key：{d}"
            );
            assert!(d.contains("<redacted>"), "{kind}: {d}");
        }
    }

    #[test]
    fn debug_does_not_print_the_body_which_may_hold_screen_content() {
        let s = build_request(&cfg(ProviderKind::Ollama, "http://x:1"), &req()).unwrap();
        let d = format!("{s:?}");
        assert!(!d.contains("查看 nginx 状态"), "Debug 打出了请求正文：{d}");
        assert!(d.contains("body_len"));
    }

    // ── 非流式响应（录制回放）──

    const ANTHROPIC_OK: &str = r#"{
      "id":"msg_01","type":"message","role":"assistant","model":"claude",
      "content":[{"type":"text","text":"用 systemctl status nginx 看。"}],
      "stop_reason":"end_turn","usage":{"input_tokens":42,"output_tokens":17}
    }"#;

    const OPENAI_OK: &str = r#"{
      "id":"chatcmpl-1","object":"chat.completion","created":1,"model":"gpt",
      "choices":[{"index":0,"message":{"role":"assistant","content":"用 systemctl status nginx 看。"},"finish_reason":"stop"}],
      "usage":{"prompt_tokens":42,"completion_tokens":17,"total_tokens":59}
    }"#;

    const OLLAMA_OK: &str = r#"{
      "model":"llama3","created_at":"2026-08-23T00:00:00Z",
      "message":{"role":"assistant","content":"用 systemctl status nginx 看。"},
      "done":true,"done_reason":"stop","prompt_eval_count":42,"eval_count":17
    }"#;

    #[test]
    fn all_three_parse_the_same_answer_out_of_their_own_shape() {
        let want = "用 systemctl status nginx 看。";
        for (kind, body) in [
            (ProviderKind::Anthropic, ANTHROPIC_OK),
            (ProviderKind::OpenAiCompatible, OPENAI_OK),
            (ProviderKind::Ollama, OLLAMA_OK),
        ] {
            let r = parse_response(kind, body).unwrap();
            assert_eq!(r.text, want, "{kind}");
            let u = r.usage.unwrap_or_else(|| panic!("{kind} 没解析出 usage"));
            assert_eq!((u.input_tokens, u.output_tokens), (42, 17), "{kind}");
        }
    }

    #[test]
    fn tool_calls_parse_from_all_three_shapes() {
        // 三家的 arguments 形态不同：Anthropic 是对象、OpenAI 是**字符串**、Ollama 是对象。
        // 统一成 JSON 文本，因为流式下它是分片到达的，只有拼完整才能解析。
        let a = parse_response(
            ProviderKind::Anthropic,
            r#"{"content":[{"type":"tool_use","id":"toolu_1","name":"run_command","input":{"cmd":"ls"}}]}"#,
        )
        .unwrap();
        assert_eq!(a.tool_calls.len(), 1);
        assert_eq!(a.tool_calls[0].name, "run_command");
        assert_eq!(a.tool_calls[0].id, "toolu_1");
        assert!(a.tool_calls[0].arguments_json.contains("\"ls\""));

        let o = parse_response(
            ProviderKind::OpenAiCompatible,
            r#"{"choices":[{"message":{"role":"assistant","content":null,
                "tool_calls":[{"id":"call_1","type":"function","function":{"name":"run_command","arguments":"{\"cmd\":\"ls\"}"}}]},
                "finish_reason":"tool_calls"}]}"#,
        )
        .unwrap();
        assert_eq!(o.tool_calls.len(), 1);
        assert_eq!(o.tool_calls[0].name, "run_command");
        assert!(o.tool_calls[0].arguments_json.contains("\"ls\""));
        assert_eq!(o.text, "", "content 为 null 是纯工具调用的合法形态");

        let l = parse_response(
            ProviderKind::Ollama,
            r#"{"message":{"role":"assistant","content":"",
                "tool_calls":[{"function":{"name":"run_command","arguments":{"cmd":"ls"}}}]},"done":true}"#,
        )
        .unwrap();
        assert_eq!(l.tool_calls.len(), 1);
        assert_eq!(l.tool_calls[0].name, "run_command");
        assert!(l.tool_calls[0].arguments_json.contains("\"ls\""));
    }

    #[test]
    fn unknown_content_block_types_are_skipped_not_fatal() {
        // 新版 API 会加块类型。为此报错等于让一次协议升级把功能整个打死。
        let r = parse_response(
            ProviderKind::Anthropic,
            r#"{"content":[{"type":"thinking","thinking":"…"},{"type":"text","text":"答案"}]}"#,
        )
        .unwrap();
        assert_eq!(r.text, "答案");
    }

    #[test]
    fn malformed_bodies_map_to_malformed_not_server_error() {
        // 这一支意味着「我们的解析或对方的协议」有问题，不是对方挂了。
        // 给用户的建议完全不同（前者报 bug，后者重试）。
        for (kind, body) in [
            (ProviderKind::Anthropic, "not json at all"),
            (ProviderKind::Anthropic, r#"{"no_content":1}"#),
            (ProviderKind::OpenAiCompatible, r#"{"choices":[]}"#),
            (ProviderKind::Ollama, r#"{"no_message":1}"#),
        ] {
            let e = parse_response(kind, body).unwrap_err();
            assert!(
                matches!(e, ProviderError::MalformedResponse { .. }),
                "{kind} / {body} 映射成了 {e:?}"
            );
            assert!(!e.is_retryable());
        }
    }

    #[test]
    fn an_error_body_returned_with_http_200_is_still_an_error() {
        // 有些网关把错误塞进 200。当成正常响应解析会得到一个空回答。
        for (kind, body) in [
            (
                ProviderKind::Anthropic,
                r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#,
            ),
            (
                ProviderKind::OpenAiCompatible,
                r#"{"error":{"message":"bad","type":"invalid_request_error"}}"#,
            ),
        ] {
            assert!(parse_response(kind, body).is_err(), "{kind}");
        }
        // Ollama 的 `{"error":"…not found"}` 要映射到 ModelNotFound 而不是泛型错
        let e =
            parse_response(ProviderKind::Ollama, r#"{"error":"model 'x' not found"}"#).unwrap_err();
        assert!(matches!(e, ProviderError::ModelNotFound { .. }), "{e:?}");
    }

    // ── 错误映射 ──

    #[test]
    fn http_status_maps_to_actionable_errors() {
        type Check = fn(&ProviderError) -> bool;
        let cases: Vec<(u16, &str, Check)> = vec![
            (401, r#"{"error":{"message":"invalid api key"}}"#, |e| {
                matches!(e, ProviderError::Unauthorized { .. })
            }),
            (403, "{}", |e| {
                matches!(e, ProviderError::Unauthorized { .. })
            }),
            (404, "{}", |e| {
                matches!(e, ProviderError::ModelNotFound { .. })
            }),
            (429, "{}", |e| {
                matches!(e, ProviderError::RateLimited { .. })
            }),
            (500, "{}", |e| {
                matches!(e, ProviderError::ServerError { .. })
            }),
            (503, "{}", |e| {
                matches!(e, ProviderError::ServerError { .. })
            }),
            (418, "{}", |e| {
                matches!(e, ProviderError::UnexpectedStatus { .. })
            }),
        ];
        for (status, body, ok) in cases {
            let e = map_error(status, body, None);
            assert!(ok(&e), "HTTP {status} 映射成了 {e:?}");
        }
    }

    #[test]
    fn retry_after_is_carried_through() {
        let e = map_error(429, "{}", Some(42));
        assert_eq!(
            e,
            ProviderError::RateLimited {
                retry_after_secs: Some(42),
                detail: "{}".into()
            }
        );
        assert!(e.user_message().contains("42"), "提示里该带上等待秒数");
    }

    #[test]
    fn context_overflow_is_recognised_out_of_a_generic_400() {
        // 三家的状态码都不专属，只能按文本认。认错的代价是一句更具体的建议，
        // 认不出的代价是用户看到一句无用的「请求无效」。
        for body in [
            r#"{"error":{"message":"This model's maximum context length is 8192 tokens"}}"#,
            r#"{"error":{"message":"prompt is too long: 300000 tokens > 200000"}}"#,
            r#"{"error":{"code":"context_length_exceeded","message":"context_length_exceeded"}}"#,
        ] {
            let e = map_error(400, body, None);
            assert!(
                matches!(e, ProviderError::ContextTooLong { .. }),
                "{body} 映射成了 {e:?}"
            );
        }
        // 反向对照：普通 400 不该被认成超长
        let e = map_error(400, r#"{"error":{"message":"invalid role"}}"#, None);
        assert!(!matches!(e, ProviderError::ContextTooLong { .. }), "{e:?}");
    }

    #[test]
    fn an_html_error_page_does_not_blow_up_or_flood_the_message() {
        // 网关返回整页 HTML 很常见
        let html = format!("<html><body>{}</body></html>", "x".repeat(10_000));
        let e = map_error(502, &html, None);
        assert!(matches!(e, ProviderError::ServerError { status: 502, .. }));
        if let ProviderError::ServerError { detail, .. } = &e {
            assert!(
                detail.chars().count() <= 401,
                "错误详情没有截断：{}",
                detail.len()
            );
        }
    }

    // ── 流式解析（录制回放）──

    const ANTHROPIC_STREAM: &str = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\"}}\n",
        "\n",
        ": heartbeat\n",
        "event: content_block_start\n",
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n",
        "\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"用 \"}}\n",
        "\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"systemctl\"}}\n",
        "\n",
        "event: message_stop\n",
        "data: {\"type\":\"message_stop\"}\n",
    );

    const OPENAI_STREAM: &str = concat!(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"\"}}]}\n",
        "\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"用 \"}}]}\n",
        "\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"systemctl\"}}]}\n",
        "\n",
        "data: [DONE]\n",
    );

    const OLLAMA_STREAM: &str = concat!(
        "{\"model\":\"llama3\",\"message\":{\"role\":\"assistant\",\"content\":\"用 \"},\"done\":false}\n",
        "{\"model\":\"llama3\",\"message\":{\"role\":\"assistant\",\"content\":\"systemctl\"},\"done\":false}\n",
        "{\"model\":\"llama3\",\"message\":{\"role\":\"assistant\",\"content\":\"\"},\"done\":true,\"done_reason\":\"stop\"}\n",
    );

    #[test]
    fn all_three_streams_fold_to_the_same_text() {
        for (kind, body) in [
            (ProviderKind::Anthropic, ANTHROPIC_STREAM),
            (ProviderKind::OpenAiCompatible, OPENAI_STREAM),
            (ProviderKind::Ollama, OLLAMA_STREAM),
        ] {
            let r = fold_stream(kind, body).unwrap();
            assert_eq!(r.text, "用 systemctl", "{kind} 的流式折叠结果不对");
        }
    }

    #[test]
    fn ollama_stream_is_ndjson_not_sse() {
        // 三家里最容易写错的一处：拿 SSE 解析器读 NDJSON，每行都因「没有 data: 前缀」
        // 被判为 Ignored，表现是「请求成功但回答是空的」而不是报错。
        let line = r#"{"message":{"content":"hi"},"done":false}"#;
        assert_eq!(
            parse_stream_line(ProviderKind::Ollama, line).unwrap(),
            StreamEvent::TextDelta("hi".into())
        );
        // 反向对照：同一行按 SSE 解析会被忽略——正是那个静默失败的形状
        assert_eq!(
            parse_stream_line(ProviderKind::OpenAiCompatible, line).unwrap(),
            StreamEvent::Ignored
        );
    }

    #[test]
    fn anthropic_event_type_comes_from_the_data_payload_not_the_event_line() {
        // 只看 `event:` 行的实现拿不到任何内容
        let only_event_line = "event: content_block_delta";
        assert_eq!(
            parse_stream_line(ProviderKind::Anthropic, only_event_line).unwrap(),
            StreamEvent::Ignored,
            "`event:` 行本身不带负荷"
        );
        let data =
            r#"data: {"type":"content_block_delta","delta":{"type":"text_delta","text":"x"}}"#;
        assert_eq!(
            parse_stream_line(ProviderKind::Anthropic, data).unwrap(),
            StreamEvent::TextDelta("x".into())
        );
    }

    #[test]
    fn sse_blank_lines_comments_and_done_are_handled() {
        for kind in [ProviderKind::Anthropic, ProviderKind::OpenAiCompatible] {
            for line in ["", "   ", ": keep-alive", "event: whatever", "data:"] {
                assert_eq!(
                    parse_stream_line(kind, line).unwrap(),
                    StreamEvent::Ignored,
                    "{kind} / {line:?}"
                );
            }
        }
        assert_eq!(
            parse_stream_line(ProviderKind::OpenAiCompatible, "data: [DONE]").unwrap(),
            StreamEvent::Done
        );
        assert_eq!(
            parse_stream_line(ProviderKind::Anthropic, r#"data: {"type":"message_stop"}"#).unwrap(),
            StreamEvent::Done
        );
    }

    #[test]
    fn done_is_not_treated_as_content() {
        // 把 `[DONE]` 当内容是流式解析的经典错误，表现是回答末尾多一段 "[DONE]"
        let r = fold_stream(ProviderKind::OpenAiCompatible, OPENAI_STREAM).unwrap();
        assert!(!r.text.contains("DONE"), "{}", r.text);
    }

    #[test]
    fn streamed_tool_calls_reassemble_from_fragments() {
        // OpenAI 把 arguments 分片送：首片带 name/id，后续片只带 arguments
        let stream = concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"run_command\",\"arguments\":\"\"}}]}}]}\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"cmd\\\"\"}}]}}]}\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\":\\\"ls\\\"}\"}}]}}]}\n",
            "data: [DONE]\n",
        );
        let r = fold_stream(ProviderKind::OpenAiCompatible, stream).unwrap();
        assert_eq!(r.tool_calls.len(), 1);
        assert_eq!(r.tool_calls[0].name, "run_command");
        assert_eq!(r.tool_calls[0].id, "call_1");
        // 拼完整之后必须是合法 JSON——这正是「保持文本形态直到拼完」的理由
        let v: Value = serde_json::from_str(&r.tool_calls[0].arguments_json)
            .expect("拼起来的 arguments 不是合法 JSON");
        assert_eq!(v["cmd"], "ls");
    }

    #[test]
    fn anthropic_streamed_tool_calls_reassemble() {
        let stream = concat!(
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"run_command\"}}\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"cmd\\\"\"}}\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\":\\\"ls\\\"}\"}}\n",
            "data: {\"type\":\"message_stop\"}\n",
        );
        let r = fold_stream(ProviderKind::Anthropic, stream).unwrap();
        assert_eq!(r.tool_calls.len(), 1);
        assert_eq!(r.tool_calls[0].name, "run_command");
        let v: Value = serde_json::from_str(&r.tool_calls[0].arguments_json).unwrap();
        assert_eq!(v["cmd"], "ls");
    }

    #[test]
    fn a_stream_error_event_surfaces_as_an_error() {
        let e = parse_stream_line(
            ProviderKind::Anthropic,
            r#"data: {"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#,
        )
        .unwrap_err();
        assert!(matches!(e, ProviderError::UnexpectedStatus { .. }), "{e:?}");

        let e = parse_stream_line(ProviderKind::Ollama, r#"{"error":"boom"}"#).unwrap_err();
        assert!(matches!(e, ProviderError::UnexpectedStatus { .. }), "{e:?}");
    }

    #[test]
    fn a_malformed_stream_line_is_an_error_not_silently_dropped() {
        // 静默跳过会让「回答少了一段」变成一个查不出来的问题
        for kind in [
            ProviderKind::Anthropic,
            ProviderKind::OpenAiCompatible,
            ProviderKind::Ollama,
        ] {
            let line = match kind {
                ProviderKind::Ollama => "{not json",
                _ => "data: {not json",
            };
            let e = parse_stream_line(kind, line).unwrap_err();
            assert!(
                matches!(e, ProviderError::MalformedResponse { .. }),
                "{kind}: {e:?}"
            );
        }
    }

    #[test]
    fn streamed_and_non_streamed_paths_agree() {
        // 同一次回答走两条路径应得到同样的文本。流式解析最容易在这里出错
        // （丢首片、丢末片、把 [DONE] 当内容）。
        for (kind, non_stream, stream) in [
            (ProviderKind::Anthropic, ANTHROPIC_OK, ANTHROPIC_STREAM),
            (ProviderKind::OpenAiCompatible, OPENAI_OK, OPENAI_STREAM),
            (ProviderKind::Ollama, OLLAMA_OK, OLLAMA_STREAM),
        ] {
            let a = parse_response(kind, non_stream).unwrap().text;
            let b = fold_stream(kind, stream).unwrap().text;
            // 两份样本的内容刻意不同（流式那份只到 "systemctl"），
            // 所以比的是「流式那份是非流式那份的前缀」
            assert!(
                a.starts_with(&b),
                "{kind}：流式 {b:?} 不是非流式 {a:?} 的前缀"
            );
            assert!(!b.is_empty(), "{kind} 的流式结果是空的");
        }
    }

    // ── T-20：多轮工具对话的请求形状（三家 parity）────────────────────

    mod conversation {
        use super::*;

        fn conv_req() -> crate::agent::history::ConversationRequest {
            use crate::agent::history::{ChatMessage as M, Conversation};
            let conv = Conversation {
                system: "SYS".into(),
                messages: vec![
                    M::User("排查磁盘".into()),
                    M::Assistant {
                        text: "我先看占用".into(),
                        tool_calls: vec![crate::ToolCall {
                            id: "call-1".into(),
                            name: "run_command".into(),
                            arguments_json: r#"{"command":"df -h"}"#.into(),
                        }],
                    },
                    M::ToolResult {
                        call_id: "call-1".into(),
                        content: r#"{"stdout":"/ 82%","exit_code":0}"#.into(),
                        is_error: false,
                    },
                ],
            };
            conv.build(crate::agent::tool::TOOL_MANIFEST, &[])
        }

        fn body_of(spec: &HttpRequestSpec) -> serde_json::Value {
            serde_json::from_str(&spec.body).expect("请求体该是合法 JSON")
        }

        /// 三家 URL/头/stream 与单发请求同构（同一 join_url、同一认证头、恒非流式）。
        #[test]
        fn conversation_urls_and_headers_match_the_single_shot_contract() {
            let req = conv_req();
            let a = build_conversation_request(
                &cfg(ProviderKind::Anthropic, "https://api.anthropic.com"),
                &req,
            )
            .unwrap();
            assert_eq!(a.url, "https://api.anthropic.com/v1/messages");
            assert!(a
                .headers
                .iter()
                .any(|(k, v)| k == "anthropic-version" && v == ANTHROPIC_VERSION));

            let o = build_conversation_request(
                &cfg(ProviderKind::OpenAiCompatible, "https://api.openai.com"),
                &req,
            )
            .unwrap();
            assert_eq!(o.url, "https://api.openai.com/chat/completions");
            assert!(o
                .headers
                .iter()
                .any(|(k, v)| k == "authorization" && v.starts_with("Bearer ")));

            let l = build_conversation_request(
                &cfg(ProviderKind::Ollama, "http://127.0.0.1:11434"),
                &req,
            )
            .unwrap();
            assert_eq!(l.url, "http://127.0.0.1:11434/api/chat");
            // Agent 恒非流式（usage 是 token 预算的账本）
            for s in [&a, &o, &l] {
                let b = body_of(s);
                assert_eq!(b["stream"], false, "Agent 回合必须非流式");
            }
        }

        /// **Anthropic 镜像**：assistant 的 tool_use 块（id/name/input 对象）+
        /// user 的 tool_result 块（tool_use_id/content/is_error 真字段）。
        /// 与 parse_anthropic 读的字段一一对拍。
        #[test]
        fn anthropic_tool_round_trip_mirrors_the_parser() {
            let req = conv_req();
            let spec = build_conversation_request(
                &cfg(ProviderKind::Anthropic, "https://api.anthropic.com"),
                &req,
            )
            .unwrap();
            let b = body_of(&spec);
            assert_eq!(b["system"], "SYS", "system 顶层");
            let msgs = b["messages"].as_array().unwrap();
            assert_eq!(msgs.len(), 3);
            // assistant：text 块 + tool_use 块
            let blocks = msgs[1]["content"].as_array().unwrap();
            assert_eq!(blocks[0]["type"], "text");
            assert_eq!(blocks[0]["text"], "我先看占用");
            assert_eq!(blocks[1]["type"], "tool_use");
            assert_eq!(blocks[1]["id"], "call-1");
            assert_eq!(blocks[1]["name"], "run_command");
            assert_eq!(
                blocks[1]["input"]["command"], "df -h",
                "input 是对象不是字符串"
            );
            // tool result：user 角色的 tool_result 块
            assert_eq!(msgs[2]["role"], "user", "Anthropic 无 tool 角色");
            let tr = &msgs[2]["content"][0];
            assert_eq!(tr["type"], "tool_result");
            assert_eq!(tr["tool_use_id"], "call-1");
            assert_eq!(tr["is_error"], false);
        }

        /// **OpenAI 镜像**：assistant 的 tool_calls[].function.arguments 是**字符串**；
        /// tool 结果走独立 tool 角色 + tool_call_id。与 parse_openai 一一对拍。
        #[test]
        fn openai_tool_round_trip_mirrors_the_parser() {
            let req = conv_req();
            let spec = build_conversation_request(
                &cfg(ProviderKind::OpenAiCompatible, "https://api.openai.com"),
                &req,
            )
            .unwrap();
            let b = body_of(&spec);
            let msgs = b["messages"].as_array().unwrap();
            assert_eq!(msgs[0]["role"], "system", "system 是第一条消息");
            let tc = &msgs[2]["tool_calls"][0];
            assert_eq!(tc["id"], "call-1");
            assert_eq!(tc["type"], "function");
            assert_eq!(tc["function"]["name"], "run_command");
            assert!(
                tc["function"]["arguments"].is_string(),
                "arguments 必须是字符串——parse_openai 读的就是字符串"
            );
            let tr = &msgs[3];
            assert_eq!(tr["role"], "tool");
            assert_eq!(tr["tool_call_id"], "call-1");
        }

        /// **Ollama 镜像**：tool_calls[].function.arguments 是**对象**（与 OpenAI 分叉）。
        #[test]
        fn ollama_tool_arguments_is_an_object_not_a_string() {
            let req = conv_req();
            let spec = build_conversation_request(
                &cfg(ProviderKind::Ollama, "http://127.0.0.1:11434"),
                &req,
            )
            .unwrap();
            let b = body_of(&spec);
            let msgs = b["messages"].as_array().unwrap();
            let tc = &msgs[2]["tool_calls"][0];
            assert_eq!(tc["function"]["name"], "run_command");
            assert!(
                tc["function"]["arguments"].is_object(),
                "Ollama 的 arguments 是对象——parse_ollama 读对象"
            );
            assert_eq!(tc["function"]["arguments"]["command"], "df -h");
            // 上限参数在 options 里（与单发同构）
            assert_eq!(b["options"]["num_predict"], req.max_tokens);
        }

        /// **T-21：回喂的 Observation 形状与契约字段同名**——MCP command.run 与
        /// Agent 工具结果同一形状（一份契约）。exec.rs 的
        /// observation_serializes_with_the_contract_field_names 钉的是 Observation
        /// 自己；这里钉「这份序列化就是回喂内容」这一环。
        #[test]
        fn tool_result_content_is_the_observation_contract_shape() {
            let obs = crate::exec::Observation {
                stdout: "o".into(),
                stderr: "e".into(),
                exit_code: Some(2),
                truncated: true,
                timed_out: false,
            };
            let content = serde_json::to_string(&obs).unwrap();
            for k in ["stdout", "stderr", "exit_code", "truncated", "timed_out"] {
                assert!(content.contains(k), "Observation 序列化缺字段 {k}");
            }
            // 这份串作为 ToolResult.content 走完 Anthropic 的序列化路径不变形
            use crate::agent::history::{ChatMessage as M, Conversation};
            let conv = Conversation {
                system: "s".into(),
                messages: vec![M::ToolResult {
                    call_id: "c1".into(),
                    content,
                    is_error: false,
                }],
            };
            let req = conv.build(crate::agent::tool::TOOL_MANIFEST, &[]);
            let a = body_of(
                &build_conversation_request(
                    &cfg(ProviderKind::Anthropic, "https://api.anthropic.com"),
                    &req,
                )
                .unwrap(),
            );
            let got = a["messages"][0]["content"][0]["content"].as_str().unwrap();
            assert!(
                got.contains("\"exit_code\":2"),
                "Observation 串过一遍序列化没变形：{got}"
            );
        }

        /// is_error 的三家落法：Anthropic 真字段；OpenAI/Ollama 标进 content 头部
        /// （**不发明协议里不存在的字段**——严格网关会整包拒收，那比错误标记
        /// 丢失糟得多：模型丢了整段上下文）。
        #[test]
        fn is_error_lands_where_each_protocol_can_carry_it() {
            use crate::agent::history::{ChatMessage as M, Conversation};
            let conv = Conversation {
                system: "s".into(),
                messages: vec![M::ToolResult {
                    call_id: "c1".into(),
                    content: "boom".into(),
                    is_error: true,
                }],
            };
            let req = conv.build(crate::agent::tool::TOOL_MANIFEST, &[]);
            let a = body_of(
                &build_conversation_request(
                    &cfg(ProviderKind::Anthropic, "https://api.anthropic.com"),
                    &req,
                )
                .unwrap(),
            );
            assert_eq!(
                a["messages"][0]["content"][0]["is_error"], true,
                "Anthropic 真字段"
            );

            let o = body_of(
                &build_conversation_request(
                    &cfg(ProviderKind::OpenAiCompatible, "https://api.openai.com"),
                    &req,
                )
                .unwrap(),
            );
            let c = o["messages"][1]["content"].as_str().unwrap();
            assert!(
                c.contains("工具调用失败"),
                "OpenAI 该在 content 头部标记：{c}"
            );
            assert!(c.contains("boom"));

            let l = body_of(
                &build_conversation_request(
                    &cfg(ProviderKind::Ollama, "http://127.0.0.1:11434"),
                    &req,
                )
                .unwrap(),
            );
            let lc = l["messages"][1]["content"].as_str().unwrap();
            assert!(
                lc.contains("工具调用失败") && lc.contains("[c1]"),
                "Ollama 的 content 带 id 与错误标：{lc}"
            );
        }

        /// tools 数组从 manifest 生成：三家都带全量工具，且 schema 落在**那家协议
        /// 认的位置**（Anthropic 顶层 `input_schema`；OpenAI/Ollama 嵌在
        /// `function.parameters`——第一版测试在顶层找后两者，自己错了）。
        #[test]
        fn tools_arrays_carry_every_manifest_entry() {
            let req = conv_req();
            for (kind, base) in [
                (ProviderKind::Anthropic, "https://api.anthropic.com"),
                (ProviderKind::OpenAiCompatible, "https://api.openai.com"),
                (ProviderKind::Ollama, "http://127.0.0.1:11434"),
            ] {
                let b = body_of(&build_conversation_request(&cfg(kind, base), &req).unwrap());
                let tools = b["tools"].as_array().unwrap();
                assert_eq!(
                    tools.len(),
                    crate::agent::tool::TOOL_MANIFEST.len(),
                    "{kind:?}"
                );
                for t in tools {
                    // Anthropic 顶层 input_schema；OpenAI/Ollama 嵌在 function.parameters
                    let schema = if kind == ProviderKind::Anthropic {
                        &t["input_schema"]
                    } else {
                        &t["function"]["parameters"]
                    };
                    assert!(schema.is_object(), "{kind:?} 的 schema 该是对象：{t}");
                }
            }
        }
    }
}
