//! `ModelPort` 的真实现：多轮请求 → transport → 解析 → 重试。
//!
//! 与 `ai_cmd::call_model`（M2 的单发路径）分开：那条走 `build_request`（system+user、
//! 无 tools）并且只取 `text`；Agent 要的是 `build_conversation_request` 与完整的
//! `ChatResponse`（tool_calls 与 usage 都是驱动循环的输入）。
//!
//! 两条路径共用 transport 与 parse——**不共用请求构造**，因为那是两个契约。

use fs_ai::agent::budget::MAX_PROVIDER_ATTEMPTS;
use fs_ai::agent::history::ConversationRequest;
use fs_ai::agent::ports::{AbortSignal, ModelPort};
use fs_ai::provider::{ProviderConfig, ProviderError};
use fs_ai::{wire, ChatResponse};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

pub struct HttpModelPort {
    abort: Arc<dyn AbortSignal>,
    /// 上一次请求体的字节数——token 保守上界的输入半边（`settle_turn` 用）。
    last_bound: AtomicU64,
}

impl HttpModelPort {
    pub fn new(abort: Arc<dyn AbortSignal>) -> Self {
        Self {
            abort,
            last_bound: AtomicU64::new(0),
        }
    }
}

impl ModelPort for HttpModelPort {
    fn turn<'a>(
        &'a self,
        cfg: &'a ProviderConfig,
        req: &'a ConversationRequest,
    ) -> Pin<Box<dyn Future<Output = Result<ChatResponse, ProviderError>> + Send + 'a>> {
        Box::pin(async move {
            let spec = wire::build_conversation_request(cfg, req)?;
            // 保守上界 = 输入半边（请求体字节数，每 token ≥ 1 字节）
            //          + 输出半边（该回 max_tokens）。**两半都在这里加完**，
            // 调用方（`run.rs` 的 settle）原样取 `last_request_bound()`，不再加。
            //
            // 注意别照着旧注释改：它曾写「那半边由调用方在 settle 时加，这里只记
            // 体长」，而代码一直是两半都加——照那句话去 settle 里补一次就是双计，
            // 预算会以两倍速度耗尽。交叉审计逮到的这处注释与代码矛盾。
            self.last_bound.store(
                spec.body.len() as u64 + req.max_tokens as u64,
                Ordering::SeqCst,
            );
            let transport = fs_ai::transport::ReqwestTransport::new()?;

            let mut attempt: u8 = 0;
            loop {
                attempt += 1;
                // 每次尝试都 select! 急停：等一个 60 秒的模型回合期间按停，
                // 不该等它回来才生效。
                let send = fs_ai::transport::Transport::send(&transport, &spec);
                let resp = tokio::select! {
                    r = send => r,
                    _ = self.abort.fired() => {
                        // 急停：drop 掉 HTTP future（reqwest 取消安全，无资源悬挂）。
                        // 用 Transport 错误冒泡——驱动层会把它换算成停因。
                        return Err(ProviderError::Transport { detail: "已急停".into() });
                    }
                };
                match resp {
                    Ok(r) if r.is_success() => {
                        return wire::parse_response(cfg.kind, &r.body);
                    }
                    Ok(r) => {
                        let e = wire::map_error(r.status, &r.body, r.retry_after_secs);
                        if e.is_retryable() && attempt < MAX_PROVIDER_ATTEMPTS {
                            // 退避：Retry-After 优先（限流时服务端给的那个数
                            // 比我们猜的准），否则线性 1s/2s。
                            let secs = match &e {
                                ProviderError::RateLimited {
                                    retry_after_secs, ..
                                } => retry_after_secs.unwrap_or(attempt as u64),
                                _ => attempt as u64,
                            };
                            tokio::time::sleep(std::time::Duration::from_secs(secs)).await;
                            continue;
                        }
                        return Err(e);
                    }
                    Err(e) => {
                        if e.is_retryable() && attempt < MAX_PROVIDER_ATTEMPTS {
                            tokio::time::sleep(std::time::Duration::from_secs(attempt as u64))
                                .await;
                            continue;
                        }
                        return Err(e);
                    }
                }
            }
        })
    }

    fn last_request_bound(&self) -> u64 {
        self.last_bound.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 重试上限来自 fs_ai 的常量（不在这里另拍一个数）。
    #[test]
    fn the_retry_cap_comes_from_the_shared_constant() {
        assert_eq!(MAX_PROVIDER_ATTEMPTS, 3);
    }
}
