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
}

pub struct Task {
    pub snapshot: Snapshot,
    pub profile_id: String,
    pub generation: u64,
    pub engine_id: u64,
    pub manager: Weak<TransferManager>,
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
        Self::prune(&mut tasks);
        tasks.get(handle).map(read)
    }

    pub fn list(&self) -> Vec<(String, Snapshot)> {
        let mut tasks = self.tasks.lock().unwrap();
        Self::prune(&mut tasks);
        let mut result: Vec<_> = tasks
            .values()
            .map(|t| (t.profile_id.clone(), t.snapshot.clone()))
            .collect();
        result.sort_by(|a, b| a.1.transfer_id.cmp(&b.1.transfer_id));
        result
    }

    pub fn event(&self, event: &TransferEvent) -> Option<Snapshot> {
        let mut tasks = self.tasks.lock().unwrap();
        let task = tasks.values_mut().find(|t| t.engine_id == event.id)?;
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
            t.snapshot.session_id == session_id && t.generation == generation && !t.terminal()
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
        if let Some(task) = tasks
            .values_mut()
            .find(|t| t.engine_id == id && t.snapshot.state == "completed")
        {
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

    pub fn request_cancel(&self, handle: &str) -> Option<(bool, u64, Weak<TransferManager>, u64)> {
        let mut tasks = self.tasks.lock().unwrap();
        Self::prune(&mut tasks);
        let task = tasks.get_mut(handle)?;
        let terminal = task.terminal();
        if !terminal {
            task.snapshot.state = "cancel_requested".into();
        }
        Some((
            terminal,
            task.engine_id,
            task.manager.clone(),
            task.generation,
        ))
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
            },
            profile_id: "profile".into(),
            generation,
            engine_id: id,
            manager: Weak::new(),
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
}
