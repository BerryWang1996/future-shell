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
    ManagementError::new(code, error.to_string())
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

async fn allowed(state: &AppState, profile_id: &str) -> bool {
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
        .map_err(failed)?;
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
                    snapshot.local_relative_path = None;
                    snapshot.remote_path = None;
                    snapshot.error = None;
                }
                serde_json::to_value(snapshot).map_err(failed)
            }
            ManagedTool::Cancel { transfer_id } => {
                let (terminal, id, manager, _) = self
                    .state
                    .mcp_tasks
                    .request_cancel(transfer_id)
                    .ok_or_else(unavailable)?;
                if !terminal {
                    let manager = manager.upgrade().ok_or_else(unavailable)?;
                    manager.cancel(id).await;
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
        match tool {
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
        let _guard = live.operation_gate.lock().await;
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
                    .map_err(|(error, partial)| ManagementError {
                        code: remote_failed_ref(&error, "FS_MCP_COPY_FAILED"),
                        message: error.to_string(),
                        partial: Some(partial),
                    })?
            }
            ManagedTool::Delete {
                path, recursive, ..
            } => fs_sshengine::managed_fileops::delete(ops.as_ref(), path, *recursive)
                .await
                .map_err(|(error, partial)| ManagementError {
                    code: remote_failed_ref(&error, "FS_MCP_DELETE_FAILED"),
                    message: error.to_string(),
                    partial: Some(partial),
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
}
