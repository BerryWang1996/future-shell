//! MCP 传输快照。事件泵更新，查询端读取，不再消费引擎事件通道。
use fs_sshengine::transfer::{TransferEvent, TransferManager, TransferState};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::{Mutex, Weak};
use std::time::{Duration, Instant};

#[derive(Clone, Serialize)]
pub struct Snapshot {
    pub transfer_id: String,
    pub session_id: String,
    pub direction: String,
    pub state: String,
    pub bytes_done: u64,
    pub bytes_total: Option<u64>,
    pub error: Option<String>,
    pub verification: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_relative_path: Option<String>,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    pub terminal: Option<TerminalMetadata>,
}

impl Snapshot {
    pub fn hide_paths(&mut self) {
        self.local_relative_path = None;
        self.remote_path = None;
        self.error = None;
        if let Some(metadata) = self.terminal.as_mut() {
            metadata.partial_relative_path = None;
        }
    }
}

#[derive(Clone, Serialize)]
pub struct TerminalMetadata {
    transport: &'static str,
    scope: &'static str,
    peer_identity_verified: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    partial_relative_path: Option<String>,
}
impl TerminalMetadata {
    pub fn zmodem(partial_relative_path: Option<String>) -> Self {
        Self {
            transport: "terminal_zmodem",
            scope: "interactive_terminal",
            peer_identity_verified: false,
            partial_relative_path,
        }
    }
}

pub enum TerminalOutcome {
    Completed,
    Cancelled(String),
    Failed(String),
}

/// 取消绑定原后端实例；ZMODEM 不通过 session_id 回查当前 tap。
#[derive(Clone)]
pub enum TaskBackend {
    Sftp {
        engine_id: u64,
        manager: Weak<TransferManager>,
    },
    Zmodem {
        cancel: tokio::sync::watch::Sender<bool>,
    },
}

pub struct Task {
    pub snapshot: Snapshot,
    pub profile_id: String,
    pub generation: u64,
    pub backend: TaskBackend,
    pub settled_at: Option<Instant>,
}

impl Task {
    pub fn terminal(&self) -> bool {
        matches!(
            self.snapshot.state.as_str(),
            "completed" | "failed" | "cancelled"
        )
    }
}

#[derive(Default)]
pub struct TaskRegistry {
    tasks: Mutex<HashMap<String, Task>>,
}

impl TaskRegistry {
    fn prune(tasks: &mut HashMap<String, Task>) {
        tasks.retain(|_, t| {
            t.settled_at
                .is_none_or(|at| at.elapsed() < Duration::from_secs(3600))
        });
        let mut terminal: Vec<_> = tasks
            .iter()
            .filter_map(|(id, t)| t.settled_at.map(|at| (at, id.clone())))
            .collect();
        terminal.sort_unstable();
        let overflow = terminal.len().saturating_sub(256);
        for (_, id) in terminal.into_iter().take(overflow) {
            tasks.remove(&id);
        }
    }

    pub fn register(&self, task: Task) {
        let mut tasks = self.tasks.lock().unwrap();
        Self::prune(&mut tasks);
        tasks.insert(task.snapshot.transfer_id.clone(), task);
    }

    pub fn remove(&self, handle: &str) {
        self.tasks.lock().unwrap().remove(handle);
    }

    pub fn inspect<T>(&self, handle: &str, read: impl FnOnce(&Task) -> T) -> Option<T> {
        let mut tasks = self.tasks.lock().unwrap();
        Self::reconcile_terminal(&mut tasks);
        Self::prune(&mut tasks);
        tasks.get(handle).map(read)
    }

    pub fn list(&self) -> Vec<(String, Snapshot)> {
        let mut tasks = self.tasks.lock().unwrap();
        Self::reconcile_terminal(&mut tasks);
        Self::prune(&mut tasks);
        let mut result: Vec<_> = tasks
            .values()
            .map(|t| (t.profile_id.clone(), t.snapshot.clone()))
            .collect();
        result.sort_by(|a, b| a.1.transfer_id.cmp(&b.1.transfer_id));
        result
    }

    fn reconcile_terminal(tasks: &mut HashMap<String, Task>) {
        for task in tasks.values_mut() {
            if !task.terminal()
                && matches!(&task.backend, TaskBackend::Zmodem {cancel} if cancel.is_closed())
            {
                task.snapshot.state = "failed".into();
                task.snapshot.error = Some("终端传输驱动结束，结果未知；请核对目标文件".into());
                task.snapshot.verification = "unverified".into();
                task.settled_at = Some(Instant::now());
            }
        }
    }

    /// 原 owner 是唯一协议事实来源；取消请求本身不推翻已经提交的 Receipt。
    pub fn terminal_update(
        &self,
        handle: &str,
        generation: u64,
        done: u64,
        total: Option<u64>,
        outcome: Option<TerminalOutcome>,
        local: Option<String>,
        partial: Option<String>,
    ) -> Option<Snapshot> {
        let mut tasks = self.tasks.lock().unwrap();
        let task = tasks.get_mut(handle)?;
        if task.generation != generation
            || !matches!(task.backend, TaskBackend::Zmodem { .. })
            || task.terminal()
        {
            return None;
        }
        task.snapshot.bytes_done = done;
        task.snapshot.bytes_total = total;
        if local.is_some() {
            task.snapshot.local_relative_path = local;
        }
        if let Some(metadata) = task.snapshot.terminal.as_mut() {
            if outcome.is_some() || partial.is_some() {
                metadata.partial_relative_path = partial;
            }
        }
        if let Some(outcome) = outcome {
            let (state, verification, error) = match outcome {
                TerminalOutcome::Completed => ("completed", "protocol_confirmed", None),
                TerminalOutcome::Cancelled(error) => ("cancelled", "unverified", Some(error)),
                TerminalOutcome::Failed(error) => ("failed", "unverified", Some(error)),
            };
            task.snapshot.state = state.into();
            task.snapshot.verification = verification.into();
            task.snapshot.error = error;
            task.settled_at = Some(Instant::now());
        } else if task.snapshot.state != "cancel_requested" {
            task.snapshot.state = "running".into();
        }
        Some(task.snapshot.clone())
    }

    pub fn event(&self, event: &TransferEvent) -> Option<Snapshot> {
        let mut tasks = self.tasks.lock().unwrap();
        let task = tasks.values_mut().find(
            |t| matches!(&t.backend, TaskBackend::Sftp { engine_id, .. } if *engine_id == event.id),
        )?;
        if task.terminal() {
            return None;
        }
        task.snapshot.bytes_done = event.bytes_done;
        if event.bytes_total > 0
            || matches!(event.state, TransferState::Running | TransferState::Done)
        {
            task.snapshot.bytes_total = Some(event.bytes_total);
        }
        task.snapshot.state = match &event.state {
            TransferState::Running if task.snapshot.state == "cancel_requested" => {
                "cancel_requested"
            }
            TransferState::Running => "running",
            TransferState::Retrying { .. } if task.snapshot.state == "cancel_requested" => {
                "cancel_requested"
            }
            TransferState::Retrying { .. } => "retrying",
            TransferState::Done => "completed",
            TransferState::Failed(error) => {
                task.snapshot.error = Some(error.clone());
                "failed"
            }
            TransferState::Cancelled => "cancelled",
        }
        .into();
        if task.terminal() {
            if task.snapshot.state != "completed" {
                task.snapshot.verification = "unverified".into();
            }
            task.settled_at = Some(Instant::now());
            return Some(task.snapshot.clone());
        }
        None
    }

    pub fn stream_ended(&self, session_id: &str, generation: u64) -> Vec<Snapshot> {
        let mut tasks = self.tasks.lock().unwrap();
        let mut settled = Vec::new();
        for task in tasks.values_mut().filter(|t| {
            matches!(t.backend, TaskBackend::Sftp { .. })
                && t.snapshot.session_id == session_id
                && t.generation == generation
                && !t.terminal()
        }) {
            task.snapshot.state = "failed".into();
            task.snapshot.error =
                Some("传输事件流结束，未取得终态。请核对目标文件后再决定是否重试".into());
            task.snapshot.verification = "unverified".into();
            task.settled_at = Some(Instant::now());
            settled.push(task.snapshot.clone());
        }
        Self::prune(&mut tasks);
        settled
    }

    pub fn verification(
        &self,
        id: u64,
        result: &fs_sshengine::verify::VerifyOutcome,
    ) -> Option<Snapshot> {
        let mut tasks = self.tasks.lock().unwrap();
        if let Some(task) = tasks.values_mut().find(|t| {
            matches!(&t.backend, TaskBackend::Sftp { engine_id, .. } if *engine_id == id)
                && t.snapshot.state == "completed"
        }) {
            task.snapshot.verification = match result {
                fs_sshengine::verify::VerifyOutcome::Sha256Match => "sha256_match",
                fs_sshengine::verify::VerifyOutcome::SizeOnlyMatch => "size_only_match",
                fs_sshengine::verify::VerifyOutcome::Mismatch => "mismatch",
                fs_sshengine::verify::VerifyOutcome::Unverified => "unverified",
            }
            .into();
            return Some(task.snapshot.clone());
        }
        None
    }

    /// 第二次核对原 handle、代次及取消通道，绝不改投当前 tap。
    pub fn cancel_zmodem(
        &self,
        handle: &str,
        generation: u64,
        cancel: &tokio::sync::watch::Sender<bool>,
    ) -> Result<(), String> {
        let mut tasks = self.tasks.lock().unwrap();
        let task = tasks.get_mut(handle).ok_or("终端传输任务不可用")?;
        if task.generation != generation
            || !matches!(&task.backend, TaskBackend::Zmodem { cancel: original } if original.same_channel(cancel))
        {
            return Err("终端传输任务已变化".into());
        }
        if task.terminal() {
            return Ok(());
        }
        if cancel.send(true).is_err() {
            let error = "终端传输驱动不可用，取消结果未知；请核对远端后再决定是否重试";
            task.snapshot.state = "failed".into();
            task.snapshot.verification = "unverified".into();
            task.snapshot.error = Some(error.into());
            task.settled_at = Some(Instant::now());
            return Err(error.into());
        }
        Ok(())
    }

    pub fn request_cancel(&self, handle: &str) -> Option<(bool, TaskBackend, u64)> {
        let mut tasks = self.tasks.lock().unwrap();
        Self::prune(&mut tasks);
        let task = tasks.get_mut(handle)?;
        let terminal = task.terminal();
        if !terminal {
            task.snapshot.state = "cancel_requested".into();
        }
        Some((terminal, task.backend.clone(), task.generation))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(id: u64, generation: u64) -> Task {
        Task {
            snapshot: Snapshot {
                transfer_id: id.to_string(),
                session_id: "session".into(),
                direction: "up".into(),
                state: "queued".into(),
                bytes_done: 0,
                bytes_total: None,
                error: None,
                verification: "pending".into(),
                remote_path: Some("/synthetic/remote".into()),
                local_relative_path: None,
                terminal: None,
            },
            profile_id: "profile".into(),
            generation,
            backend: TaskBackend::Sftp {
                engine_id: id,
                manager: Weak::new(),
            },
            settled_at: None,
        }
    }

    fn event(id: u64, state: TransferState) -> TransferEvent {
        TransferEvent {
            id,
            state,
            bytes_done: 4,
            bytes_total: 4,
        }
    }

    #[test]
    fn instant_completion_is_retained_and_cancel_does_not_reverse_it() {
        let registry = TaskRegistry::default();
        registry.register(task(1, 1));
        let completed = registry.event(&event(1, TransferState::Done)).unwrap();
        assert_eq!(completed.state, "completed");
        assert!(registry.request_cancel("1").unwrap().0);
        assert!(registry
            .event(&event(1, TransferState::Cancelled))
            .is_none());
        assert_eq!(
            registry.inspect("1", |t| t.snapshot.state.clone()).unwrap(),
            "completed"
        );
    }

    #[test]
    fn cancellation_remains_requested_until_authoritative_terminal() {
        let registry = TaskRegistry::default();
        registry.register(task(1, 1));
        assert!(!registry.request_cancel("1").unwrap().0);
        registry.event(&event(1, TransferState::Retrying { attempt: 1 }));
        registry.event(&event(1, TransferState::Running));
        assert_eq!(
            registry.inspect("1", |t| t.snapshot.state.clone()).unwrap(),
            "cancel_requested"
        );
        let terminal = registry.event(&event(1, TransferState::Cancelled)).unwrap();
        assert_eq!(terminal.state, "cancelled");
        assert_eq!(terminal.verification, "unverified");
    }

    #[test]
    fn old_stream_termination_never_settles_new_generation() {
        let registry = TaskRegistry::default();
        registry.register(task(1, 1));
        registry.register(task(2, 2));
        registry.stream_ended("session", 1);
        assert_eq!(
            registry.inspect("1", |t| t.snapshot.state.clone()).unwrap(),
            "failed"
        );
        assert_eq!(
            registry.inspect("2", |t| t.snapshot.state.clone()).unwrap(),
            "queued"
        );
    }

    #[test]
    fn cache_bounds_only_terminal_tasks_and_expiry_hides_known_handles() {
        let registry = TaskRegistry::default();
        registry.register(task(1, 1));
        for id in 2..=260 {
            let mut t = task(id, 1);
            t.snapshot.state = "completed".into();
            t.settled_at = Some(Instant::now());
            registry.register(t);
        }
        assert_eq!(registry.list().len(), 257);
        assert!(registry.inspect("1", |_| ()).is_some());
        let mut expired = task(999, 1);
        expired.snapshot.state = "failed".into();
        expired.settled_at = Some(Instant::now() - Duration::from_secs(3601));
        registry.register(expired);
        assert!(registry.inspect("999", |_| ()).is_none());
        assert!(registry.request_cancel("missing").is_none());
    }
    fn zmodem_task(handle: &str, generation: u64) -> (Task, tokio::sync::watch::Receiver<bool>) {
        let (cancel, receiver) = tokio::sync::watch::channel(false);
        let mut task = task(0, generation);
        task.snapshot.transfer_id = handle.into();
        task.backend = TaskBackend::Zmodem { cancel };
        (task, receiver)
    }

    #[test]
    fn sftp_stream_end_does_not_settle_zmodem_in_same_session_generation() {
        let registry = TaskRegistry::default();
        let (zmodem, _receiver) = zmodem_task("zmodem", 1);
        registry.register(zmodem);
        registry.register(task(0, 1));
        let settled = registry.stream_ended("session", 1);
        assert_eq!(settled.len(), 1);
        assert_eq!(settled[0].transfer_id, "0");
        assert_eq!(
            registry
                .inspect("zmodem", |t| t.snapshot.state.clone())
                .unwrap(),
            "queued"
        );
    }

    #[test]
    fn missing_zmodem_receiver_settles_unknown_instead_of_pending_forever() {
        let registry = TaskRegistry::default();
        let (zmodem, receiver) = zmodem_task("zmodem", 1);
        registry.register(zmodem);
        drop(receiver);
        let (_, backend, generation) = registry.request_cancel("zmodem").unwrap();
        let TaskBackend::Zmodem { cancel } = backend else {
            panic!("wrong backend")
        };
        assert!(registry
            .cancel_zmodem("zmodem", generation, &cancel)
            .unwrap_err()
            .contains("未知"));
        registry
            .inspect("zmodem", |t| {
                assert_eq!(t.snapshot.state, "failed");
                assert_eq!(t.snapshot.verification, "unverified");
                assert!(t.snapshot.error.as_ref().unwrap().contains("未知"));
                assert!(t.settled_at.is_some());
            })
            .unwrap();
    }

    #[test]
    fn sftp_events_and_verification_never_match_zmodem() {
        let registry = TaskRegistry::default();
        let (zmodem, _receiver) = zmodem_task("zmodem", 1);
        registry.register(zmodem);
        assert!(registry.event(&event(0, TransferState::Done)).is_none());
        assert_eq!(
            registry
                .inspect("zmodem", |t| t.snapshot.state.clone())
                .unwrap(),
            "queued"
        );
        let (mut completed, _completed_receiver) = zmodem_task("done", 1);
        completed.snapshot.state = "completed".into();
        completed.snapshot.verification = "protocol_confirmed".into();
        completed.settled_at = Some(Instant::now());
        registry.register(completed);
        let outcome = fs_sshengine::verify::VerifyOutcome::Sha256Match;
        assert!(registry.verification(0, &outcome).is_none());
        registry.register(task(0, 1));
        assert_eq!(
            registry
                .event(&event(0, TransferState::Done))
                .unwrap()
                .transfer_id,
            "0"
        );
        assert_eq!(
            registry.verification(0, &outcome).unwrap().verification,
            "sha256_match"
        );
        assert_eq!(
            registry
                .inspect("done", |t| t.snapshot.verification.clone())
                .unwrap(),
            "protocol_confirmed"
        );
    }

    #[test]
    fn zmodem_cancel_is_bound_idempotent_and_only_requests_exit() {
        let registry = TaskRegistry::default();
        let (old, old_receiver) = zmodem_task("old", 1);
        let (new, new_receiver) = zmodem_task("new", 2);
        registry.register(old);
        registry.register(new);
        for _ in 0..2 {
            let (terminal, backend, generation) = registry.request_cancel("old").unwrap();
            assert!(!terminal);
            let TaskBackend::Zmodem { cancel } = backend else {
                panic!("wrong backend")
            };
            registry.cancel_zmodem("old", generation, &cancel).unwrap();
        }
        assert!(*old_receiver.borrow());
        assert!(!*new_receiver.borrow());
        assert_eq!(
            registry
                .inspect("old", |t| t.snapshot.state.clone())
                .unwrap(),
            "cancel_requested"
        );
        assert!(registry.inspect("old", |t| t.settled_at).unwrap().is_none());
    }

    #[test]
    fn stale_cancel_cannot_signal_replacement_even_with_same_generation() {
        let registry = TaskRegistry::default();
        let (old, old_receiver) = zmodem_task("same-handle", 1);
        registry.register(old);
        let (_, backend, generation) = registry.request_cancel("same-handle").unwrap();
        let TaskBackend::Zmodem { cancel } = backend else {
            panic!("wrong backend")
        };
        let (replacement, replacement_receiver) = zmodem_task("same-handle", 1);
        registry.register(replacement);
        assert!(registry
            .cancel_zmodem("same-handle", generation, &cancel)
            .is_err());
        assert!(!*old_receiver.borrow());
        assert!(!*replacement_receiver.borrow());
        assert_eq!(
            registry
                .inspect("same-handle", |t| t.snapshot.state.clone())
                .unwrap(),
            "queued"
        );
        assert!(registry.cancel_zmodem("same-handle", 2, &cancel).is_err());
    }

    #[test]
    fn terminal_zmodem_cancel_does_not_signal_or_reverse_completion() {
        let registry = TaskRegistry::default();
        let (mut completed, receiver) = zmodem_task("done", 1);
        let TaskBackend::Zmodem { cancel } = completed.backend.clone() else {
            panic!("wrong backend")
        };
        completed.snapshot.state = "completed".into();
        completed.snapshot.verification = "protocol_confirmed".into();
        completed.settled_at = Some(Instant::now());
        registry.register(completed);
        assert!(registry.request_cancel("done").unwrap().0);
        registry.cancel_zmodem("done", 1, &cancel).unwrap();
        assert!(!*receiver.borrow());
        drop(receiver);
        registry.cancel_zmodem("done", 1, &cancel).unwrap();
        assert_eq!(
            registry
                .inspect("done", |t| t.snapshot.state.clone())
                .unwrap(),
            "completed"
        );
    }

    #[test]
    fn sftp_snapshot_wire_shape_remains_unchanged() {
        let registry = TaskRegistry::default();
        registry.register(task(1, 1));
        let completed = registry.event(&event(1, TransferState::Done)).unwrap();
        assert_eq!(
            serde_json::to_value(completed).unwrap(),
            serde_json::json!({
                "transfer_id":"1", "session_id":"session", "direction":"up",
                "state":"completed", "bytes_done":4, "bytes_total":4, "error":null,
                "verification":"pending", "remote_path":"/synthetic/remote"
            })
        );
    }
    #[test]
    fn closed_terminal_receiver_is_reconciled_by_queries_without_cancel() {
        for list in [false, true] {
            let registry = TaskRegistry::default();
            let (task, receiver) = zmodem_task("lost", 1);
            registry.register(task);
            drop(receiver);
            if list {
                assert_eq!(registry.list()[0].1.state, "failed");
            }
            registry
                .inspect("lost", |t| {
                    assert_eq!(t.snapshot.state, "failed");
                    assert_eq!(t.snapshot.verification, "unverified");
                    assert!(t.settled_at.is_some());
                })
                .unwrap();
        }
    }

    #[test]
    fn terminal_updates_bind_backend_generation_and_preserve_final_receipt() {
        let registry = TaskRegistry::default();
        let (mut task, receiver) = zmodem_task("terminal", 7);
        task.snapshot.terminal = Some(TerminalMetadata::zmodem(Some(
            ".incoming/original.part".into(),
        )));
        registry.register(task);
        assert!(registry
            .terminal_update("terminal", 6, 0, None, None, None, None)
            .is_none());
        registry.register(super::tests::task(1, 7));
        assert!(registry
            .terminal_update("1", 7, 0, None, None, None, None)
            .is_none());
        registry.request_cancel("terminal"); // request 可以晚于最后一次取消检查，但早于 durable Receipt 登记。
        let completed = registry
            .terminal_update(
                "terminal",
                7,
                4,
                Some(4),
                Some(TerminalOutcome::Completed),
                Some("file".into()),
                None,
            )
            .unwrap();
        let wire = serde_json::to_value(completed).unwrap();
        assert_eq!(wire["transport"], "terminal_zmodem");
        assert_eq!(wire["scope"], "interactive_terminal");
        assert_eq!(wire["peer_identity_verified"], false);
        assert_eq!(wire["verification"], "protocol_confirmed");
        assert!(wire.get("terminal").is_none());
        assert!(
            wire.get("partial_relative_path").is_none(),
            "success clears old partial evidence"
        );
        drop(receiver);
        assert!(registry
            .terminal_update(
                "terminal",
                7,
                0,
                None,
                Some(TerminalOutcome::Failed("late".into())),
                None,
                None
            )
            .is_none());
        assert_eq!(
            registry
                .inspect("terminal", |t| t.snapshot.state.clone())
                .unwrap(),
            "completed"
        );
    }

    #[test]
    fn failed_terminal_keeps_recoverable_relative_paths_and_late_success_is_ignored() {
        let registry = TaskRegistry::default();
        let (mut task, _receiver) = zmodem_task("terminal", 1);
        task.snapshot.terminal = Some(TerminalMetadata::zmodem(None));
        registry.register(task);
        let result = registry
            .terminal_update(
                "terminal",
                1,
                3,
                None,
                Some(TerminalOutcome::Failed(
                    "cleanup failed; target may exist".into(),
                )),
                Some("file".into()),
                Some(".incoming/test.part".into()),
            )
            .unwrap();
        let wire = serde_json::to_value(result).unwrap();
        assert_eq!(wire["local_relative_path"], "file");
        assert_eq!(wire["partial_relative_path"], ".incoming/test.part");
        assert!(registry
            .terminal_update(
                "terminal",
                1,
                3,
                Some(3),
                Some(TerminalOutcome::Completed),
                None,
                None
            )
            .is_none());
    }
    #[test]
    fn revoked_snapshot_hides_old_and_partial_paths_but_keeps_transport_facts() {
        let mut snapshot = task(1, 1).snapshot;
        snapshot.local_relative_path = Some("final".into());
        snapshot.error = Some("path detail".into());
        snapshot.terminal = Some(TerminalMetadata::zmodem(Some(
            ".incoming/private.part".into(),
        )));
        let allowed = serde_json::to_value(&snapshot).unwrap();
        assert_eq!(allowed["partial_relative_path"], ".incoming/private.part");
        snapshot.hide_paths();
        let revoked = serde_json::to_value(snapshot).unwrap();
        for field in [
            "remote_path",
            "local_relative_path",
            "partial_relative_path",
        ] {
            assert!(revoked.get(field).is_none());
        }
        assert!(revoked["error"].is_null());
        assert_eq!(revoked["transport"], "terminal_zmodem");
        assert_eq!(allowed["local_relative_path"], "final");
    }
}
