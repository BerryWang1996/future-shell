//! UI 确认队列（`ConfirmPort` 的真实现）+ Agent 监督器（run 表 + 急停令牌）。
//!
//! # 确认是**阻塞等待**，不是「弹一下就走」
//!
//! 总设计 §4.5 的确认契约：⚠ 工具裁决为 write/dangerous 时，调用阻塞等待 GUI
//! 确认队列。这里的 `confirm()` 返回一个 future，它在三种事件下才完成：
//! 用户应答、到点（60s）、上下文消亡（会话没了 / 急停）。
//!
//! 「弹出后立刻返回一个默认值」是最容易写出来的版本，也是最危险的：
//! 那等于把「需确认」悄悄降级成「自动执行」。

use fs_ai::agent::confirm::{AskAnswer, ConfirmAnswer, ConfirmTicket};
use fs_ai::agent::ports::{AbortSignal, ConfirmPort};
use serde::Serialize;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::oneshot;

/// 前端要显示的一条待确认项（`agent:confirm` 事件载荷）。
#[derive(Debug, Clone, Serialize)]
pub struct PendingConfirm {
    pub request_id: u64,
    pub run_id: String,
    /// 已脱敏的动作摘要。
    pub action: String,
    /// `read_only` / `write` / `dangerous`（稳定串）。
    pub tier: String,
    /// true ⇒ 长按或二次输入（§4.3 的手势成本）。
    pub strong: bool,
    pub rules: Vec<String>,
    pub details: Vec<String>,
    /// 等待上限（秒）——前端据此画倒计时。
    pub deadline_secs: u64,
}

/// 一条待答的问题（`agent:ask` 事件载荷）。
#[derive(Debug, Clone, Serialize)]
pub struct PendingAsk {
    pub request_id: u64,
    pub run_id: String,
    pub question: String,
    pub deadline_secs: u64,
}

/// 急停令牌：`AtomicBool` + `Notify`。
///
/// 两者都要：`requested()` 给 admit 的瞬时快照（不阻塞），`fired()` 给
/// select! 的等待端。只有 bool 的话等待端要轮询；只有 Notify 的话
/// 「按停发生在等待开始之前」这一拍会丢。
pub struct AbortToken {
    flag: AtomicBool,
    notify: tokio::sync::Notify,
}

impl Default for AbortToken {
    fn default() -> Self {
        Self {
            flag: AtomicBool::new(false),
            notify: tokio::sync::Notify::new(),
        }
    }
}

impl AbortToken {
    pub fn set(&self) {
        self.flag.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }
}

impl AbortSignal for AbortToken {
    fn requested(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }
    fn fired<'a>(&'a self) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        Box::pin(async move {
            // 先查一次再等：置位发生在 fired() 之前时，notified() 不会自己醒。
            // 这个顺序写反就是「按了停但没反应」，且只在竞速下复现。
            if self.requested() {
                return;
            }
            self.notify.notified().await;
        })
    }
}

/// 全局监督器：run 表 + 待答表。放进 `AppState`。
#[derive(Default)]
pub struct AgentSupervisor {
    runs: Mutex<HashMap<String, Arc<AbortToken>>>,
    confirms: Mutex<HashMap<u64, oneshot::Sender<ConfirmAnswer>>>,
    asks: Mutex<HashMap<u64, oneshot::Sender<AskAnswer>>>,
    next_id: std::sync::atomic::AtomicU64,
}

impl AgentSupervisor {
    pub fn register(&self, run_id: &str) -> Arc<AbortToken> {
        let tok = Arc::new(AbortToken::default());
        self.runs
            .lock()
            .unwrap()
            .insert(run_id.to_string(), tok.clone());
        tok
    }

    pub fn unregister(&self, run_id: &str) {
        self.runs.lock().unwrap().remove(run_id);
        // 该 run 的未决确认一并撤回：run 都结束了，那些票据没有归宿。
        // 不撤的话前端会留着一个点了没反应的对话框。
        self.confirms.lock().unwrap().clear();
        self.asks.lock().unwrap().clear();
    }

    /// 急停。找不到 run 返回 false（前端据此知道「那个 run 已经结束了」）。
    pub fn abort(&self, run_id: &str) -> bool {
        match self.runs.lock().unwrap().get(run_id) {
            Some(t) => {
                t.set();
                true
            }
            None => false,
        }
    }

    pub fn is_running(&self, run_id: &str) -> bool {
        self.runs.lock().unwrap().contains_key(run_id)
    }

    fn next_request_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// 前端应答一条确认。返回 false = 这条已经不在等了（超时/撤回/重复应答）。
    pub fn answer_confirm(&self, request_id: u64, approved: bool) -> bool {
        match self.confirms.lock().unwrap().remove(&request_id) {
            Some(tx) => tx.send(ConfirmAnswer::Answered(approved)).is_ok(),
            None => false,
        }
    }

    /// 前端回答一条提问。
    pub fn answer_ask(&self, request_id: u64, text: String) -> bool {
        match self.asks.lock().unwrap().remove(&request_id) {
            Some(tx) => tx.send(AskAnswer::Answered(text)).is_ok(),
            None => false,
        }
    }
}

/// 事件回调：`(事件名, 载荷)`。
type EmitFn = Arc<dyn Fn(&str, serde_json::Value) + Send + Sync>;

/// 事件出口。
///
/// 做成**具名方法**而不是裸闭包，有两个理由：
/// ① 读代码的人看到 `sink.emit("agent:confirm", …)` 就知道这是一个事件发送点；
/// ② 事件契约守卫（`event-contract.test.ts`）扫的是 `.emit(` 这个形状——
///    藏在闭包调用 `(self.emit)(…)` 里的发送点它看不见，而**守卫看不见的
///    发送点就是守卫的盲区**：那个事件有没有人监听将不再有人管。
pub struct EventSink(pub EmitFn);

impl EventSink {
    pub fn emit(&self, name: &str, payload: serde_json::Value) {
        (self.0)(name, payload);
    }
}

/// `ConfirmPort` 的真实现：入队 → 发事件 → 阻塞等待三选一。
pub struct UiConfirmPort {
    pub supervisor: Arc<AgentSupervisor>,
    pub abort: Arc<AbortToken>,
    pub run_id: String,
    /// 事件出口（Tauri AppHandle 的 emit 封装；测试可注入记录器）。
    pub sink: EventSink,
}

impl ConfirmPort for UiConfirmPort {
    fn confirm<'a>(
        &'a self,
        t: ConfirmTicket,
    ) -> Pin<Box<dyn Future<Output = ConfirmAnswer> + Send + 'a>> {
        Box::pin(async move {
            let id = self.supervisor.next_request_id();
            let (tx, rx) = oneshot::channel();
            self.supervisor.confirms.lock().unwrap().insert(id, tx);
            let payload = PendingConfirm {
                request_id: id,
                run_id: self.run_id.clone(),
                action: t.display.as_str().to_string(),
                tier: t.tier.as_str().to_string(),
                strong: t.strong,
                rules: t.rules.iter().map(|r| r.to_string()).collect(),
                details: t.details.clone(),
                deadline_secs: t.deadline.as_secs(),
            };
            self.sink.emit(
                "agent:confirm",
                serde_json::to_value(&payload).unwrap_or_default(),
            );

            // 三选一：应答 / 到点 / 急停。**到点用 CONFIRM_TIMEOUT 而不是自己拍**
            // ——同一个上限在 fs_policy 里，票据也带着它。
            tokio::select! {
                r = rx => r.unwrap_or(ConfirmAnswer::ContextGone),
                _ = tokio::time::sleep(t.deadline) => {
                    self.supervisor.confirms.lock().unwrap().remove(&id);
                    ConfirmAnswer::TimedOut
                }
                _ = self.abort.fired() => {
                    self.supervisor.confirms.lock().unwrap().remove(&id);
                    // 急停 ⇒ ContextGone；停因由驱动层按 abort 旗标换算成 UserAbort
                    // （而不是 SessionAbandoned——「是人按的停」与「窗口没了」
                    // 导向不同的后续）。
                    ConfirmAnswer::ContextGone
                }
            }
        })
    }

    fn ask_user<'a>(
        &'a self,
        question: &'a str,
    ) -> Pin<Box<dyn Future<Output = AskAnswer> + Send + 'a>> {
        Box::pin(async move {
            let id = self.supervisor.next_request_id();
            let (tx, rx) = oneshot::channel();
            self.supervisor.asks.lock().unwrap().insert(id, tx);
            let payload = PendingAsk {
                request_id: id,
                run_id: self.run_id.clone(),
                question: question.to_string(),
                deadline_secs: fs_policy::CONFIRM_TIMEOUT.as_secs(),
            };
            self.sink.emit(
                "agent:ask",
                serde_json::to_value(&payload).unwrap_or_default(),
            );
            tokio::select! {
                r = rx => r.unwrap_or(AskAnswer::ContextGone),
                _ = tokio::time::sleep(fs_policy::CONFIRM_TIMEOUT) => {
                    self.supervisor.asks.lock().unwrap().remove(&id);
                    AskAnswer::TimedOut
                }
                _ = self.abort.fired() => {
                    self.supervisor.asks.lock().unwrap().remove(&id);
                    AskAnswer::ContextGone
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noop_emit() -> EventSink {
        EventSink(Arc::new(|_, _| {}))
    }

    /// 急停令牌：**先置位、后等待**也必须立即醒。
    ///
    /// `Notify::notified()` 只唤醒「已经在等」的一方；置位发生在 `fired()` 之前时
    /// 它不会自己醒。所以 `fired()` 里先查一次 flag——这个顺序写反就是
    /// 「按了停但没反应」，且只在竞速下复现（最难查的那类）。
    #[tokio::test]
    async fn an_abort_set_before_waiting_still_wakes_the_waiter() {
        let tok = Arc::new(AbortToken::default());
        tok.set(); // 先按停
                   // 再等：必须立即完成，不能挂住
        tokio::time::timeout(std::time::Duration::from_millis(200), tok.fired())
            .await
            .expect("先置位后等待也该立即醒——否则急停在竞速下会丢一拍");
    }

    /// 确认队列：应答把 future 唤醒并带回结果。
    #[tokio::test]
    async fn answering_a_confirmation_resolves_the_waiting_future() {
        let sup = Arc::new(AgentSupervisor::default());
        let abort = Arc::new(AbortToken::default());
        let port = UiConfirmPort {
            supervisor: sup.clone(),
            abort,
            run_id: "r1".into(),
            sink: noop_emit(),
        };
        let ticket = ConfirmTicket {
            id: 1,
            display: fs_ai::agent::record::RedactedAction::new("touch /tmp/x", &[]),
            tier: fs_policy::Tier::Write,
            strong: false,
            rules: vec![],
            details: vec![],
            deadline: std::time::Duration::from_secs(5),
        };
        let sup2 = sup.clone();
        let answering = tokio::spawn(async move {
            // 等入队后再应答（request_id 从 1 开始）
            for _ in 0..50 {
                if sup2.answer_confirm(1, true) {
                    return true;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            false
        });
        let ans = port.confirm(ticket).await;
        assert!(matches!(ans, ConfirmAnswer::Answered(true)));
        assert!(answering.await.unwrap(), "应答该找到那条待确认项");
    }

    /// 急停让等待中的确认收敛为 ContextGone（而不是挂到 60 秒）。
    #[tokio::test]
    async fn aborting_resolves_a_pending_confirmation_immediately() {
        let sup = Arc::new(AgentSupervisor::default());
        let abort = Arc::new(AbortToken::default());
        let port = UiConfirmPort {
            supervisor: sup,
            abort: abort.clone(),
            run_id: "r1".into(),
            sink: noop_emit(),
        };
        let ticket = ConfirmTicket {
            id: 1,
            display: fs_ai::agent::record::RedactedAction::new("rm -rf /", &[]),
            tier: fs_policy::Tier::Dangerous,
            strong: true,
            // 上限故意给大：要证明的是「急停让它提前收敛」，不是「等到点」
            deadline: std::time::Duration::from_secs(600),
            rules: vec![],
            details: vec![],
        };
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            abort.set();
        });
        let ans = tokio::time::timeout(std::time::Duration::from_secs(2), port.confirm(ticket))
            .await
            .expect("急停该让确认立即收敛，而不是挂到 deadline");
        assert!(matches!(ans, ConfirmAnswer::ContextGone));
    }

    /// 重复应答/迟到应答返回 false——不能让第二次点击「复活」一条已收敛的票据。
    #[test]
    fn answering_twice_reports_the_second_one_as_stale() {
        let sup = AgentSupervisor::default();
        assert!(!sup.answer_confirm(999, true), "不存在的票据该报 false");
    }

    /// 急停一个不存在的 run 返回 false（前端据此知道它已经结束了）。
    #[test]
    fn aborting_an_unknown_run_is_reported_not_swallowed() {
        let sup = AgentSupervisor::default();
        assert!(!sup.abort("no-such-run"));
        let _ = sup.register("r1");
        assert!(sup.abort("r1"));
        assert!(sup.is_running("r1"));
        sup.unregister("r1");
        assert!(!sup.is_running("r1"));
    }
}
