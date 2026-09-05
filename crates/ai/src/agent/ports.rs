//! 注入面集中在 trait 定义处。`run.rs` 只面向 trait 编程，测试给 fake——
//! 时钟、通道、确认、审计、HTTP 全部可注入，离线测试不需要真 SSH、真模型、真时间。

use crate::agent::confirm::{AskAnswer, ConfirmAnswer, ConfirmTicket};
use crate::agent::record::StepRecord;
use crate::agent::tool::Tool;
use crate::exec::{Clock, Observation};
use crate::{ChatResponse, ProviderConfig, ProviderError};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// 急停信号。trait（std-only）：app 层用 AtomicBool + `tokio::sync::Notify` 实现，
/// 测试用旗标实现。dyn 兼容（boxed-future，与 `fs_ai::Transport` 同款手法——
/// AsyncFnInTrait 不能 dyn 是仓库既有事实）。
pub trait AbortSignal: Send + Sync {
    /// 瞬时快照。`admit_*` 每次重查，不闩「未请求」。
    fn requested(&self) -> bool;
    /// 进 `select!` 的等待端（app 层由 Notify 唤醒；测试里可以永远 pending）。
    fn fired<'a>(&'a self) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>>;
}

/// 急停感知时钟：置位后 `elapsed` 报 `u64::MAX` ⇒ `run_command` 自己的
/// 读前超时检查立即走 `timed_out` 臂 ⇒ kill → close。
///
/// 急停因此**不需要任何新的通道回收代码**，也不可能写出第二份——
/// kill-before-close、单一返回点、partial_output 三条不变式全部继承
/// `exec::run_command` 已被变异钉死的行为（实现规格 §7）。
pub struct RunClock {
    signal: Arc<dyn AbortSignal>,
    start: std::sync::Mutex<std::time::Instant>,
}

impl RunClock {
    pub fn new(signal: Arc<dyn AbortSignal>) -> Self {
        Self {
            signal,
            start: std::sync::Mutex::new(std::time::Instant::now()),
        }
    }
    /// ExecPort 每条命令前重置——每命令独立计时，上一条的耗时不算进这一条。
    pub fn reset(&self) {
        *self.start.lock().unwrap() = std::time::Instant::now();
    }
}

impl Clock for RunClock {
    fn elapsed_secs(&self) -> u64 {
        if self.signal.requested() {
            u64::MAX
        } else {
            self.start.lock().unwrap().elapsed().as_secs()
        }
    }
}

/// 通道计数面（出口 2「通道计数归零」的运行时层断言对象）。
///
/// 登记唯一入口 `lease()`，注销唯一出口 `Drop`——没有 `forget()` / `unregister()`。
/// 注意分工：Drop 只兜底**计数**；通道 close 的保证在 `run_command` 的单一返回点
/// （exec.rs 已被三路断言 + 变异钉死），两个保证各自由为它设计的机制持有。
#[derive(Default)]
pub struct ChannelRegistry {
    open: AtomicUsize,
}

impl ChannelRegistry {
    pub fn count(&self) -> usize {
        self.open.load(Ordering::SeqCst)
    }
    pub fn lease(self: &Arc<Self>) -> ChannelLease {
        self.open.fetch_add(1, Ordering::SeqCst);
        ChannelLease {
            registry: Arc::clone(self),
        }
    }
}

pub struct ChannelLease {
    registry: Arc<ChannelRegistry>,
}

impl Drop for ChannelLease {
    fn drop(&mut self) {
        // panic 展开也跑 Drop：即使驱动循环 panic，计数也会归零——
        // 「泄漏计数」不该是 panic 的次生灾害。
        self.registry.open.fetch_sub(1, Ordering::SeqCst);
    }
}

/// 模型回合端口。实现（app 层 / 测试 fake）内部完成：
/// `build_conversation_request` → `transport::chat`（非流式——三家都回 usage，
/// 流式 fold_stream 不填 usage，本批不碰流式）→ `is_retryable` 重试
/// （≤ [`crate::agent::budget::MAX_PROVIDER_ATTEMPTS`]，每次尝试 select! abort）。
pub trait ModelPort: Send + Sync {
    fn turn<'a>(
        &'a self,
        cfg: &'a ProviderConfig,
        req: &'a super::history::ConversationRequest,
    ) -> Pin<Box<dyn Future<Output = Result<ChatResponse, ProviderError>> + Send + 'a>>;
    /// 请求体字节长（token 保守上界的输入半边），`settle_turn` 用。
    fn last_request_bound(&self) -> u64;
}

/// 执行端口。`fs_ai::exec::run_command` 的全仓唯一生产调用点在它的 app 层实现里。
///
/// CurrentTerminal 模式也在实现里组合：`wrap_for_terminal` → 收集 →
/// `strip_command_echo`（先）→ `parse_sentinel`（后）。
///
/// **注意它并不注入用户 PTY**（此处旧注写的是「注入用户 PTY」，与实现不符，
/// 2026-08-28 订正）：实现同样走独立的 exec 通道，没有 TTY。
/// 完整记述见 `fs_ai::exec` 的模块头。
pub trait ExecPort: Send + Sync {
    fn run<'a>(
        &'a self,
        tool: &'a Tool,
        clock: &'a RunClock,
    ) -> Pin<Box<dyn Future<Output = Observation> + Send + 'a>>;
    /// 远端读：sftp 读，套同一 `Observation` 形状（stdout 装内容、exit_code 0/None）。
    fn read_remote_file<'a>(
        &'a self,
        path: &'a str,
        max_bytes: usize,
    ) -> Pin<Box<dyn Future<Output = Observation> + Send + 'a>>;
    fn list_remote_dir<'a>(
        &'a self,
        path: &'a str,
    ) -> Pin<Box<dyn Future<Output = Observation> + Send + 'a>>;
    fn sftp_get<'a>(
        &'a self,
        remote: &'a str,
        local_dest: &'a str,
    ) -> Pin<Box<dyn Future<Output = Observation> + Send + 'a>>;
    fn sftp_put<'a>(
        &'a self,
        local_src: &'a str,
        remote_dest: &'a str,
    ) -> Pin<Box<dyn Future<Output = Observation> + Send + 'a>>;
    /// 外部 MCP 工具调用（M3 出口 6）：把 `Tool::ExternalMcp` 转给挂载的外部
    /// server。返回同一 `Observation` 形状——stdout 装工具结果文本，失败时
    /// stderr 装原因、exit_code 非 0。
    ///
    /// **分级不在这条路上**：外部工具的危险级由 [`crate::agent::gate`] 的
    /// `floor_at_write` 在进执行之前就定好（Write 起步、工具级可提级、不可降级），
    /// 这里只负责「已放行的调用真的发出去」。
    fn external_mcp<'a>(
        &'a self,
        server_id: &'a str,
        tool: &'a str,
        arguments_json: &'a str,
    ) -> Pin<Box<dyn Future<Output = Observation> + Send + 'a>>;
}

/// 确认端口：UI 确认队列与 ask_user 的注入面。
///
/// 实现内部要 `select!` 急停（→ `ContextGone` 由驱动层换算成 UserAbort 或
/// SessionAbandoned）与会话存亡。
pub trait ConfirmPort: Send + Sync {
    fn confirm<'a>(
        &'a self,
        t: ConfirmTicket,
    ) -> Pin<Box<dyn Future<Output = ConfirmAnswer> + Send + 'a>>;
    fn ask_user<'a>(
        &'a self,
        question: &'a str,
    ) -> Pin<Box<dyn Future<Output = AskAnswer> + Send + 'a>>;
}

/// 审计端口。`Err` ⇒ 驱动循环 `stop(AuditWriteFailed)`。
///
/// 刻意与 `ai_cmd::audit_ai` 的 warn-only 不同：一次性建议失败可以放过，
/// **自主步无记录地继续是链上缺口**。dispatch 内联 await ⇒ 下一次模型调用
/// 结构上发生在落库之后（审计先行，由控制流保证，不靠注释）。
pub trait AuditPort: Send + Sync {
    fn append<'a>(
        &'a self,
        rec: StepRecord,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    /// RunClock：急停置位后报 MAX，未置位时报真实流逝（0）。
    #[test]
    fn the_run_clock_jumps_to_max_when_the_signal_fires() {
        struct Flag(AtomicBool);
        impl AbortSignal for Flag {
            fn requested(&self) -> bool {
                self.0.load(Ordering::SeqCst)
            }
            fn fired<'a>(&'a self) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
                Box::pin(std::future::pending())
            }
        }
        let flag = Arc::new(Flag(AtomicBool::new(false)));
        let clock = RunClock::new(flag.clone());
        assert_eq!(clock.elapsed_secs(), 0, "未置位时报真实值");
        flag.0.store(true, Ordering::SeqCst);
        assert_eq!(
            clock.elapsed_secs(),
            u64::MAX,
            "置位后跳 MAX——run_command 的超时臂自己会接管"
        );
    }

    /// 通道租约：成对归零；panic 展开也归零；没有忘记归还的口子。
    #[test]
    fn channel_leases_return_to_zero_on_every_path() {
        let reg = Arc::new(ChannelRegistry::default());
        {
            let _a = reg.lease();
            let _b = reg.lease();
            assert_eq!(reg.count(), 2);
        } // 双双 Drop
        assert_eq!(reg.count(), 0);
        // panic 展开路径
        let reg2 = Arc::new(ChannelRegistry::default());
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _lease = reg2.lease();
            assert_eq!(reg2.count(), 1);
            panic!("boom");
        }));
        assert!(result.is_err());
        assert_eq!(
            reg2.count(),
            0,
            "panic 展开也跑 Drop——计数泄漏不该是 panic 的次生灾害"
        );
    }
}
