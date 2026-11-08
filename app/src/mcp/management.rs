use crate::state::AppState;
use fs_connmgr::{ProfileRepo, SettingsRepo};
use fs_mcpbridge::managed::{ManagedTool, ManagementError, ManagementPort};
use fs_sshengine::sftp::SftpOps;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub struct McpManagement {
    pub state: Arc<AppState>,
}

fn unavailable() -> ManagementError {
    ManagementError::new(
        "FS_MCP_TARGET_UNAVAILABLE",
        "目标不可用或已变化，请重新查询",
    )
}
fn failed(error: impl std::fmt::Display) -> ManagementError {
    ManagementError::new("FS_MCP_OPERATION_FAILED", error.to_string())
}

fn operation_failed(error: String) -> ManagementError {
    if error == fs_mcpbridge::ports::SESSION_BUSY {
        ManagementError::new("FS_MCP_CONFLICT", error)
    } else {
        failed(error)
    }
}

fn session_failed(error: crate::commands::session_cmd::SessionControlError) -> ManagementError {
    use crate::commands::session_cmd::SessionControlError;
    let code = match &error {
        SessionControlError::Unavailable => "FS_MCP_TARGET_UNAVAILABLE",
        SessionControlError::Busy | SessionControlError::Connected => "FS_MCP_CONFLICT",
        SessionControlError::ConfigurationChanged => "FS_POLICY_FORBIDDEN",
        SessionControlError::Storage(_) => "FS_MCP_OPERATION_FAILED",
    };
    ManagementError::new(code, error.to_string())
}

fn remote_failed(error: fs_sshengine::Error) -> ManagementError {
    let code = remote_failed_ref(&error, "FS_MCP_OPERATION_FAILED");
    ManagementError::new(code, remote_message(&error))
}

fn remote_message(error: &fs_sshengine::Error) -> String {
    let scope = match error {
        fs_sshengine::Error::SftpStatus { code: 2, .. } => {
            "原 SSH 端点的远端路径不存在；请核对路径与 sessions.details 中的端点"
        }
        fs_sshengine::Error::SftpStatus { code: 3, .. } => {
            "原 SSH 端点的远端文件系统拒绝访问；请核对远端文件权限（与 MCP 连接授权不同）"
        }
        fs_sshengine::Error::Unsupported(_) | fs_sshengine::Error::SftpStatus { code: 8, .. } => {
            "原 SSH 端点或客户端不支持该文件操作；请核对端点及服务端能力"
        }
        _ => return error.to_string(),
    };
    format!("{scope}：{error}")
}

fn remote_partial(
    error: fs_sshengine::Error,
    partial: Value,
    fallback: &'static str,
) -> ManagementError {
    ManagementError {
        code: remote_failed_ref(&error, fallback),
        message: remote_message(&error),
        partial: Some(partial),
    }
}

fn remote_failed_ref(error: &fs_sshengine::Error, fallback: &'static str) -> &'static str {
    match error {
        fs_sshengine::Error::Conflict(_) => "FS_MCP_CONFLICT",
        fs_sshengine::Error::Unsupported(_) | fs_sshengine::Error::SftpStatus { code: 8, .. } => {
            "FS_MCP_UNSUPPORTED"
        }
        fs_sshengine::Error::Timeout { .. } | fs_sshengine::Error::Poisoned(_) => "FS_MCP_TIMEOUT",
        fs_sshengine::Error::SftpStatus { code: 2, .. } => "FS_MCP_TARGET_UNAVAILABLE",
        fs_sshengine::Error::SftpStatus { code: 3, .. } => "FS_MCP_REMOTE_PERMISSION_DENIED",
        _ => fallback,
    }
}

pub(crate) fn upload_root_key(profile_id: &str) -> String {
    format!("mcp.uploadRoot.{profile_id}")
}

pub(crate) async fn allowed(state: &AppState, profile_id: &str) -> bool {
    let Ok(id) = uuid::Uuid::parse_str(profile_id) else {
        return false;
    };
    ProfileRepo::new(state.db.pool())
        .get(id)
        .await
        .is_ok_and(|p| p.allows_mcp_ssh())
}

fn open_upload(root: &str, raw: &str) -> Result<(PathBuf, std::fs::File), String> {
    if root.trim().is_empty() {
        return Err("上传目录尚未授权".into());
    }
    let root = Path::new(root)
        .canonicalize()
        .map_err(|_| "上传目录不可用")?;
    if !root.is_dir() {
        return Err("上传目录不可用".into());
    }
    let requested = Path::new(raw);
    let path = if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        root.join(requested)
    };
    let canonical = path.canonicalize().map_err(|_| "无法读取上传源")?;
    if !canonical.starts_with(&root) {
        return Err("上传源超出已授权目录".into());
    }
    if !std::fs::metadata(&canonical)
        .map_err(|_| "无法读取源文件属性")?
        .is_file()
    {
        return Err("上传源必须是普通文件".into());
    }
    let source = std::fs::File::open(&canonical).map_err(|_| "无法打开上传源")?;
    if !source
        .metadata()
        .map_err(|_| "无法读取源文件属性")?
        .is_file()
    {
        return Err("上传源必须是普通文件".into());
    }
    let captured = same_file::Handle::from_file(source.try_clone().map_err(|_| "无法保留源文件")?)
        .map_err(|_| "无法验证源文件身份")?;
    let current = same_file::Handle::from_path(&canonical).map_err(|_| "源文件已变化")?;
    if captured != current
        || !canonical
            .canonicalize()
            .map_err(|_| "源文件已变化")?
            .starts_with(&root)
    {
        return Err("源文件在打开时发生变化".into());
    }
    Ok((canonical, source))
}

fn check_upload_root(expected: &str, current: Option<&str>) -> Result<(), ManagementError> {
    if current == Some(expected) {
        Ok(())
    } else {
        Err(ManagementError::new(
            "FS_MCP_PATH_DENIED",
            "上传目录授权已变化",
        ))
    }
}

async fn terminal_submit_budget(
    handle: &str,
    write: impl std::future::Future<Output = Result<(), ManagementError>>,
) -> Result<(), ManagementError> {
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), write)
        .await
        .unwrap_or_else(|_| {
            Err(ManagementError::new(
                "FS_MCP_TIMEOUT",
                "终端命令提交超时，结果未知；先查询 transfer.status，禁止重放",
            ))
        });
    result.map_err(|mut error| {
        error.partial = Some(json!({"transfer_id":handle,"observe_with":"transfer.status"}));
        error
    })
}

impl McpManagement {
    async fn submit(&self, tool: &ManagedTool, generation: u64) -> Result<Value, ManagementError> {
        use crate::commands::sftp_cmd::{submit_core, McpSubmission, TransferSpec};
        let sid = tool.session_id().ok_or_else(unavailable)?;
        let live = self.state.registry.get(sid).ok_or_else(unavailable)?;
        let handle = uuid::Uuid::new_v4().to_string();
        let mut options = fs_sshengine::transfer::TransferOptions {
            no_replace: true,
            upload_source: None,
        };
        let mut upload_root = None;
        let (direction, local, remote) = match tool {
            ManagedTool::Upload {
                local_path,
                remote_path,
                ..
            } => {
                let value = SettingsRepo::new(self.state.db.pool())
                    .get(&upload_root_key(&live.profile_id))
                    .await
                    .map_err(failed)?
                    .ok_or_else(|| {
                        ManagementError::new(
                            "FS_MCP_PATH_DENIED",
                            "请先在连接编辑器的 AI 页授权本地上传目录",
                        )
                    })?;
                let root: String = serde_json::from_str(&value).map_err(failed)?;
                upload_root = Some(root.clone());
                let raw = local_path.clone();
                let (path, source) = tokio::task::spawn_blocking(move || open_upload(&root, &raw))
                    .await
                    .map_err(failed)?
                    .map_err(|e| ManagementError::new("FS_MCP_PATH_DENIED", e))?;
                options.upload_source = Some(Arc::new(source));
                (
                    "up",
                    path.to_string_lossy().into_owned(),
                    remote_path.clone(),
                )
            }
            ManagedTool::Download { remote_path, .. } => {
                ("down", String::new(), remote_path.clone())
            }
            _ => unreachable!(),
        };
        submit_core(
            TransferSpec {
                session_id: sid.into(),
                direction: direction.into(),
                local,
                remote,
                resume_offset: 0,
                options,
                registration: Some(McpSubmission {
                    handle: handle.clone(),
                    profile_id: live.profile_id.clone(),
                    generation,
                    upload_root,
                }),
            },
            &self.state.app,
            &self.state,
        )
        .await
        .map_err(operation_failed)?;
        Ok(json!({"transfer_id":handle,"accepted":true,"observe_with":"transfer.status"}))
    }

    async fn submit_terminal(
        &self,
        tool: &ManagedTool,
        generation: u64,
    ) -> Result<Value, ManagementError> {
        use super::terminal_command::{download_command, upload_basename, upload_command};
        use crate::zmodem_bridge::{McpReservation, McpTransferIo};
        use crate::zmodem_mcp_io::{DownloadSink, UploadSource};
        let sid = tool.session_id().ok_or_else(unavailable)?;
        let live = self.state.registry.get(sid).ok_or_else(unavailable)?;
        if !self.state.zmodem_config().await.enabled {
            return Err(ManagementError::new(
                "FS_MCP_UNSUPPORTED",
                "请在设置中启用终端内文件传输",
            ));
        }
        let tap = self
            .state
            .zmodem
            .lock()
            .unwrap()
            .get(sid)
            .cloned()
            .ok_or_else(|| {
                ManagementError::new(
                    "FS_MCP_UNSUPPORTED",
                    "该会话未装配终端传输入口，请启用终端内文件传输后重连",
                )
            })?;
        let mut upload_root = None;
        let (command, remote_path, local_relative_path, io) = match tool {
            ManagedTool::TerminalUpload {
                local_path,
                remote_dir,
                ..
            } => {
                let basename = upload_basename(Path::new(local_path))
                    .map_err(|e| ManagementError::new("FS_MCP_PATH_DENIED", e))?
                    .to_owned();
                let command = upload_command(remote_dir)
                    .map_err(|e| ManagementError::new("FS_MCP_PATH_DENIED", e))?;
                let value = SettingsRepo::new(self.state.db.pool())
                    .get(&upload_root_key(&live.profile_id))
                    .await
                    .map_err(failed)?
                    .ok_or_else(|| {
                        ManagementError::new("FS_MCP_PATH_DENIED", "请先授权本地上传目录")
                    })?;
                let root: String = serde_json::from_str(&value).map_err(failed)?;
                upload_root = Some(value);
                let raw = local_path.clone();
                let (_, file) = tokio::task::spawn_blocking(move || open_upload(&root, &raw))
                    .await
                    .map_err(failed)?
                    .map_err(|e| ManagementError::new("FS_MCP_PATH_DENIED", e))?;
                let size = file.metadata().map_err(failed)?.len();
                let source = UploadSource::new(file, basename, size).map_err(failed)?;
                (
                    command,
                    remote_dir.clone(),
                    Some(local_path.clone()),
                    McpTransferIo::Upload(source),
                )
            }
            ManagedTool::TerminalDownload { remote_path, .. } => {
                let plan = download_command(remote_path)
                    .map_err(|e| ManagementError::new("FS_MCP_PATH_DENIED", e))?;
                let root = self.state.download_sandbox_for(sid).await.map_err(failed)?;
                let sink = DownloadSink::new(&root, &plan.basename, &plan.local_basename)
                    .map_err(failed)?;
                (
                    plan.command,
                    remote_path.clone(),
                    Some(plan.local_basename),
                    McpTransferIo::Download {
                        basename: plan.basename,
                        sink,
                        expected: None,
                    },
                )
            }
            _ => unreachable!(),
        };
        let handle = uuid::Uuid::new_v4().to_string();
        let _gate = tap
            .reserve_mcp(
                &self.state,
                McpReservation {
                    handle: handle.clone(),
                    generation,
                    remote_path,
                    local_relative_path,
                    io,
                },
            )
            .await
            .map_err(|e| ManagementError::new("FS_MCP_CONFLICT", e))?;
        // Accepted reservations survive caller cancellation. Every failure preserves its handle.
        let result = async {
            if self.state.registry.generation_of(sid) != Some(generation)
                || !self
                    .state
                    .zmodem
                    .lock()
                    .unwrap()
                    .get(sid)
                    .is_some_and(|t| Arc::ptr_eq(t, &tap))
            {
                return Err(unavailable());
            }
            if !allowed(&self.state, &live.profile_id).await {
                return Err(ManagementError::new(
                    "FS_POLICY_FORBIDDEN",
                    "该连接已撤销 MCP 授权",
                ));
            }
            if let Some(expected_root) = upload_root {
                let current = SettingsRepo::new(self.state.db.pool())
                    .get(&upload_root_key(&live.profile_id))
                    .await
                    .map_err(failed)?;
                check_upload_root(&expected_root, current.as_deref())?;
            }
            let mut bytes = command.into_bytes();
            bytes.push(b'\r');
            terminal_submit_budget(&handle, async {
                let writer = live.write.lock().await;
                if self.state.registry.generation_of(sid) != Some(generation)
                    || !self
                        .state
                        .zmodem
                        .lock()
                        .unwrap()
                        .get(sid)
                        .is_some_and(|t| Arc::ptr_eq(t, &tap))
                {
                    return Err(unavailable());
                }
                tap.mark_mcp_command_attempted(&handle, generation)
                    .map_err(failed)?;
                writer.data_bytes(bytes).await.map_err(failed)
            })
            .await
        }
        .await;
        if let Err(mut error) = result {
            tap.fail_mcp_submission(&handle, generation, &error.message);
            error.partial = Some(json!({"transfer_id":handle,"observe_with":"transfer.status"}));
            return Err(error);
        }
        Ok(json!({"transfer_id":handle,"accepted":true,"observe_with":"transfer.status"}))
    }

    async fn task_call(&self, tool: &ManagedTool) -> Result<Value, ManagementError> {
        match tool {
            ManagedTool::Transfers { session_id } => {
                let mut snapshots = Vec::new();
                for (profile, snapshot) in self.state.mcp_tasks.list() {
                    if session_id
                        .as_ref()
                        .is_none_or(|sid| sid == &snapshot.session_id)
                        && allowed(&self.state, &profile).await
                    {
                        snapshots.push(snapshot);
                    }
                }
                Ok(json!({"transfers":snapshots}))
            }
            ManagedTool::Status { transfer_id } => {
                let (profile, mut snapshot) = self
                    .state
                    .mcp_tasks
                    .inspect(transfer_id, |task| {
                        (task.profile_id.clone(), task.snapshot.clone())
                    })
                    .ok_or_else(unavailable)?;
                if !allowed(&self.state, &profile).await {
                    snapshot.hide_paths();
                }
                serde_json::to_value(snapshot).map_err(failed)
            }
            ManagedTool::Cancel { transfer_id } => {
                let (terminal, backend, generation) = self
                    .state
                    .mcp_tasks
                    .request_cancel(transfer_id)
                    .ok_or_else(unavailable)?;
                if !terminal {
                    match backend {
                        super::tasks::TaskBackend::Sftp { engine_id, manager } => {
                            let manager = manager.upgrade().ok_or_else(unavailable)?;
                            manager.cancel(engine_id).await;
                        }
                        super::tasks::TaskBackend::Zmodem { cancel } => {
                            self.state
                                .mcp_tasks
                                .cancel_zmodem(transfer_id, generation, &cancel)
                                .map_err(|error| {
                                    ManagementError::new("FS_MCP_TARGET_UNAVAILABLE", error)
                                })?;
                        }
                    }
                }
                Ok(
                    json!({"transfer_id":transfer_id,"cancel_requested":!terminal,"already_terminal":terminal}),
                )
            }
            _ => unreachable!(),
        }
    }

    async fn dispatch(
        &self,
        tool: &ManagedTool,
        expected: Option<u64>,
    ) -> Result<Value, ManagementError> {
        if tool.session_id().is_none() {
            return self.task_call(tool).await;
        }
        let sid = tool.session_id().ok_or_else(unavailable)?;
        let generation = expected.ok_or_else(unavailable)?;
        let (actual, live) = self
            .state
            .registry
            .get_with_generation(sid)
            .ok_or_else(unavailable)?;
        if actual != generation {
            return Err(unavailable());
        }
        if !allowed(&self.state, &live.profile_id).await {
            return Err(ManagementError::new(
                "FS_POLICY_FORBIDDEN",
                "该 SSH 连接未允许 MCP 操作，请在连接编辑器核对授权",
            ));
        }
        if !matches!(tool, ManagedTool::Details { .. })
            && crate::zmodem_bridge::mcp_terminal_busy(&self.state, sid)
        {
            return Err(operation_failed(fs_mcpbridge::ports::SESSION_BUSY.into()));
        }
        match tool {
            ManagedTool::TerminalUpload { .. } | ManagedTool::TerminalDownload { .. } => {
                return self.submit_terminal(tool, generation).await
            }
            ManagedTool::Upload { .. } | ManagedTool::Download { .. } => {
                return self.submit(tool, generation).await
            }
            ManagedTool::Close { .. } => {
                crate::commands::session_cmd::close_core(sid, &self.state, Some(generation), true)
                    .await
                    .map_err(session_failed)?;
                return Ok(json!({"closed":true,"session_id":sid}));
            }
            ManagedTool::Reconnect { .. } => {
                let started = crate::commands::session_cmd::reconnect_core(
                    sid,
                    &self.state,
                    Some(generation),
                    true,
                )
                .await
                .map_err(session_failed)?;
                return Ok(
                    json!({"accepted":true,"already_reconnecting":!started,"session_id":sid,"observe_with":"sessions.details"}),
                );
            }
            _ => {}
        }
        let _guard = if matches!(tool, ManagedTool::Details { .. }) {
            live.operation_gate.clone().lock_owned().await
        } else {
            crate::zmodem_bridge::idle_operation_gate(live.operation_gate.clone(), || {
                crate::zmodem_bridge::mcp_terminal_busy(&self.state, sid)
            })
            .await
            .map_err(operation_failed)?
        };
        if self.state.registry.generation_of(sid) != Some(generation) {
            return Err(unavailable());
        }
        if !allowed(&self.state, &live.profile_id).await {
            return Err(ManagementError::new(
                "FS_POLICY_FORBIDDEN",
                "该连接已撤销 MCP 授权",
            ));
        }
        if let ManagedTool::Details { .. } = tool {
            let profile = ProfileRepo::new(self.state.db.pool())
                .get(uuid::Uuid::parse_str(&live.profile_id).map_err(failed)?)
                .await
                .map_err(failed)?;
            return Ok(json!({
                "session_id":sid,"name":profile.name,"endpoint":live.target_endpoint,
                "generation":generation,
                "state":live.connection_state()
            }));
        }
        let ops = self.state.sftp_ops_for(sid).await.map_err(failed)?;
        let result = match tool {
            ManagedTool::Stat { path, .. } => serde_json::to_value(
                fs_sshengine::sftp::describe_entry(ops.as_ref(), path)
                    .await
                    .map_err(remote_failed)?,
            )
            .map_err(failed)?,
            ManagedTool::Walk { path, exclude, .. } => serde_json::to_value(
                fs_sshengine::managed_fileops::walk(ops.as_ref(), path, exclude.as_deref())
                    .await
                    .map_err(remote_failed)?,
            )
            .map_err(failed)?,
            ManagedTool::Mkdir { path, .. } => {
                let path = fs_sshengine::managed_fileops::destination(ops.as_ref(), path)
                    .await
                    .map_err(remote_failed)?;
                fs_sshengine::managed_fileops::require_absent(ops.as_ref(), &path)
                    .await
                    .map_err(remote_failed)?;
                ops.mkdir(&path).await.map_err(remote_failed)?;
                json!({"created":path})
            }
            ManagedTool::Rename { from, to, .. } => {
                let from = fs_sshengine::managed_fileops::destination(ops.as_ref(), from)
                    .await
                    .map_err(remote_failed)?;
                let to = fs_sshengine::managed_fileops::destination(ops.as_ref(), to)
                    .await
                    .map_err(remote_failed)?;
                ops.rename_no_replace(&from, &to)
                    .await
                    .map_err(remote_failed)?;
                json!({"renamed":true,"path":to})
            }
            ManagedTool::Symlink {
                target, link_path, ..
            } => {
                let path = fs_sshengine::managed_fileops::destination(ops.as_ref(), link_path)
                    .await
                    .map_err(remote_failed)?;
                fs_sshengine::managed_fileops::require_absent(ops.as_ref(), &path)
                    .await
                    .map_err(remote_failed)?;
                ops.symlink(target, &path).await.map_err(remote_failed)?;
                json!({"created":path,"target":target})
            }
            ManagedTool::Copy { from, to, .. } => {
                fs_sshengine::managed_fileops::copy(ops.as_ref(), from, to)
                    .await
                    .map_err(|(error, partial)| {
                        remote_partial(error, partial, "FS_MCP_COPY_FAILED")
                    })?
            }
            ManagedTool::Delete {
                path, recursive, ..
            } => fs_sshengine::managed_fileops::delete(ops.as_ref(), path, *recursive)
                .await
                .map_err(|(error, partial)| {
                    remote_partial(error, partial, "FS_MCP_DELETE_FAILED")
                })?,
            _ => unreachable!(),
        };
        Ok(result)
    }
}

impl ManagementPort for McpManagement {
    fn generation(&self, sid: &str) -> Option<u64> {
        self.state.registry.generation_of(sid)
    }
    fn run<'a>(
        &'a self,
        tool: &'a ManagedTool,
        generation: Option<u64>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Value, ManagementError>> + Send + 'a>,
    > {
        Box::pin(self.dispatch(tool, generation))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn terminal_submission_budget_preserves_original_handle_and_never_replays() {
        let gate = tokio::sync::Mutex::new(());
        let held = gate.lock().await;
        let attempts = std::sync::atomic::AtomicUsize::new(0);
        let started = tokio::time::Instant::now();
        let error = terminal_submit_budget("original-handle", async {
            let _write = gate.lock().await;
            attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        })
        .await
        .unwrap_err();
        assert!(started.elapsed() >= std::time::Duration::from_secs(5));
        assert_eq!(error.code, "FS_MCP_TIMEOUT");
        assert_eq!(error.partial.unwrap()["transfer_id"], "original-handle");
        drop(held);
        assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 0);
        let error = terminal_submit_budget("write-error", async { Err(failed("channel closed")) })
            .await
            .unwrap_err();
        assert_eq!(error.partial.unwrap()["transfer_id"], "write-error");
    }

    #[test]
    fn upload_root_recheck_rejects_revocation_or_changed_json_before_command() {
        let captured = serde_json::to_string("/authorized").unwrap();
        assert!(check_upload_root(&captured, Some(&captured)).is_ok());
        for current in [None, Some("null"), Some("\"/another\""), Some("\"\"")] {
            assert_eq!(
                check_upload_root(&captured, current).unwrap_err().code,
                "FS_MCP_PATH_DENIED"
            );
        }
    }

    #[test]
    fn remote_errors_identify_file_scope_and_preserve_partial_results() {
        for (status, code, text) in [
            (2, "FS_MCP_TARGET_UNAVAILABLE", "远端路径不存在"),
            (3, "FS_MCP_REMOTE_PERMISSION_DENIED", "远端文件系统拒绝访问"),
            (8, "FS_MCP_UNSUPPORTED", "不支持该文件操作"),
        ] {
            let error = fs_sshengine::Error::SftpStatus {
                code: status,
                message: "original context".into(),
            };
            let out = remote_failed(error);
            assert_eq!(out.code, code);
            assert!(out.message.contains(text));
            assert!(out.message.contains("original context"));
            for fallback in ["FS_MCP_COPY_FAILED", "FS_MCP_DELETE_FAILED"] {
                let error = fs_sshengine::Error::SftpStatus {
                    code: status,
                    message: "original context".into(),
                };
                let partial = json!({"completed": 2});
                let out = remote_partial(error, partial.clone(), fallback);
                assert_eq!(out.code, code);
                assert!(out.message.contains(text));
                assert!(out.message.contains("original context"));
                assert_eq!(out.partial, Some(partial));
            }
        }
    }

    #[test]
    fn upload_requires_explicit_root_and_pins_the_opened_file() {
        use std::io::Read;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("source");
        std::fs::write(&path, b"original").unwrap();
        assert!(open_upload("", path.to_str().unwrap()).is_err());
        assert!(open_upload(dir.path().to_str().unwrap(), ".").is_err());
        let (_, mut pinned) = open_upload(dir.path().to_str().unwrap(), "source").unwrap();
        std::fs::rename(&path, dir.path().join("old")).unwrap();
        std::fs::write(&path, b"replacement").unwrap();
        let mut data = Vec::new();
        pinned.read_to_end(&mut data).unwrap();
        assert_eq!(data, b"original");
    }

    #[cfg(unix)]
    #[test]
    fn upload_refuses_symlink_escape_and_relative_traversal() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret"), b"outside").unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("escape")).unwrap();
        assert!(open_upload(root.path().to_str().unwrap(), "escape/secret").is_err());
        assert!(open_upload(
            root.path().to_str().unwrap(),
            outside.path().join("secret").to_str().unwrap()
        )
        .is_err());
        assert!(open_upload(root.path().to_str().unwrap(), "../").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn download_parent_validation_precedes_creating_children() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("escape")).unwrap();
        assert!(fs_sshengine::sandbox::prepare_destination(
            root.path(),
            Path::new("escape/new/file")
        )
        .is_err());
        assert!(!outside.path().join("new").exists());
        let good =
            fs_sshengine::sandbox::prepare_destination(root.path(), Path::new("a/b/file")).unwrap();
        assert!(good.parent().unwrap().is_dir());
        assert!(good.starts_with(root.path().canonicalize().unwrap()));
    }
    #[test]
    fn occupied_terminal_is_conflict_not_unavailable_or_forbidden() {
        assert_eq!(
            operation_failed(fs_mcpbridge::ports::SESSION_BUSY.into()).code,
            "FS_MCP_CONFLICT"
        );
        assert_eq!(
            session_failed(crate::commands::session_cmd::SessionControlError::Busy).code,
            "FS_MCP_CONFLICT"
        );
        assert_eq!(
            operation_failed("unrelated IO failure".into()).code,
            "FS_MCP_OPERATION_FAILED"
        );
    }
}
