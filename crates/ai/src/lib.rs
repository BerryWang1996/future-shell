//! fs_ai — AI 层：Provider 适配、提示模板、上下文组装（总设计 §4，M2）。
//!
//! # 目前落地的部分
//!
//! - [`redact`]：提示词与审计记录的脱敏。先做它是因为其余每一块都要用——
//!   上下文组装要用、审计落库要用、错误上报要用。

/// `run_command` 契约（总设计 §4.4）：超时/截断/观察值，以及当前终端模式的哨兵。
/// MCP 的 `command.run` 共享同一份——契约有两个实现就等于有两个契约。
pub mod agent;
pub mod context;
pub mod exec;
pub mod provider;
pub mod redact;
pub mod transport;
pub mod wire;

pub use context::{
    assemble, parse_host_facts, AssembledContext, ContextInputs, HostFacts, ProfileFacts,
    ScreenExcerpt, HOST_FACTS_COMMAND,
};
pub use provider::{ConfigError, ProviderConfig, ProviderError, ProviderKind, ProviderState};
pub use redact::{redact, Redacted};
pub use transport::{chat, HttpResponse, ReqwestTransport, Transport};
pub use wire::{
    build_request, fold_stream, map_error, parse_response, parse_stream_line, ChatRequest,
    ChatResponse, HttpRequestSpec, StreamEvent, ToolCall, Usage,
};
