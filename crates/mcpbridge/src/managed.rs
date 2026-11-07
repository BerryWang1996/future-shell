//! 1.0.5 的受管操作。参数、发现和分级共享同一个类型。
use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "operation", deny_unknown_fields)]
pub enum ManagedTool {
    #[serde(rename = "sftp.upload")]
    Upload {
        session_id: String,
        local_path: String,
        remote_path: String,
    },
    #[serde(rename = "sftp.download")]
    Download {
        session_id: String,
        remote_path: String,
    },
    #[serde(rename = "transfer.list")]
    Transfers { session_id: Option<String> },
    #[serde(rename = "transfer.status")]
    Status { transfer_id: String },
    #[serde(rename = "transfer.cancel")]
    Cancel { transfer_id: String },
    #[serde(rename = "sftp.stat")]
    Stat { session_id: String, path: String },
    #[serde(rename = "sftp.walk")]
    Walk {
        session_id: String,
        path: String,
        exclude: Option<String>,
    },
    #[serde(rename = "sftp.mkdir")]
    Mkdir { session_id: String, path: String },
    #[serde(rename = "sftp.rename")]
    Rename {
        session_id: String,
        from: String,
        to: String,
    },
    #[serde(rename = "sftp.copy")]
    Copy {
        session_id: String,
        from: String,
        to: String,
    },
    #[serde(rename = "sftp.delete")]
    Delete {
        session_id: String,
        path: String,
        #[serde(default)]
        recursive: bool,
    },
    #[serde(rename = "sftp.symlink")]
    Symlink {
        session_id: String,
        target: String,
        link_path: String,
    },
    #[serde(rename = "sessions.details")]
    Details { session_id: String },
    #[serde(rename = "sessions.close")]
    Close { session_id: String },
    #[serde(rename = "sessions.reconnect")]
    Reconnect { session_id: String },
}

impl ManagedTool {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Upload { .. } => "sftp.upload",
            Self::Download { .. } => "sftp.download",
            Self::Transfers { .. } => "transfer.list",
            Self::Status { .. } => "transfer.status",
            Self::Cancel { .. } => "transfer.cancel",
            Self::Stat { .. } => "sftp.stat",
            Self::Walk { .. } => "sftp.walk",
            Self::Mkdir { .. } => "sftp.mkdir",
            Self::Rename { .. } => "sftp.rename",
            Self::Copy { .. } => "sftp.copy",
            Self::Delete { .. } => "sftp.delete",
            Self::Symlink { .. } => "sftp.symlink",
            Self::Details { .. } => "sessions.details",
            Self::Close { .. } => "sessions.close",
            Self::Reconnect { .. } => "sessions.reconnect",
        }
    }

    pub fn session_id(&self) -> Option<&str> {
        match self {
            Self::Transfers { .. } | Self::Status { .. } | Self::Cancel { .. } => None,
            Self::Upload { session_id, .. }
            | Self::Download { session_id, .. }
            | Self::Stat { session_id, .. }
            | Self::Walk { session_id, .. }
            | Self::Mkdir { session_id, .. }
            | Self::Rename { session_id, .. }
            | Self::Copy { session_id, .. }
            | Self::Delete { session_id, .. }
            | Self::Symlink { session_id, .. }
            | Self::Details { session_id }
            | Self::Close { session_id }
            | Self::Reconnect { session_id } => Some(session_id),
        }
    }

    pub fn tier(&self) -> fs_policy::Tier {
        crate::gate::classify_managed(self).tier
    }
}

/// 管理操作的应用边界。代次在确认前捕获，实际执行前复核。
pub trait ManagementPort: Send + Sync {
    fn generation(&self, session_id: &str) -> Option<u64>;
    fn run<'a>(
        &'a self,
        tool: &'a ManagedTool,
        generation: Option<u64>,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<serde_json::Value, ManagementError>>
                + Send
                + 'a,
        >,
    >;
}

#[derive(Debug)]
pub struct ManagementError {
    pub code: &'static str,
    pub message: String,
    pub partial: Option<serde_json::Value>,
}

impl ManagementError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            partial: None,
        }
    }
}

#[cfg(test)]
pub(crate) fn examples() -> Vec<(&'static str, serde_json::Value)> {
    use serde_json::json;
    vec![
        (
            "sftp.upload",
            json!({"session_id":"s1","local_path":"sample.bin","remote_path":"/tmp/sample.bin"}),
        ),
        (
            "sftp.download",
            json!({"session_id":"s1","remote_path":"/tmp/sample.bin"}),
        ),
        ("transfer.list", json!({})),
        ("transfer.status", json!({"transfer_id":"task"})),
        ("transfer.cancel", json!({"transfer_id":"task"})),
        (
            "sftp.stat",
            json!({"session_id":"s1","path":"/tmp/sample.bin"}),
        ),
        ("sftp.walk", json!({"session_id":"s1","path":"/tmp"})),
        ("sftp.mkdir", json!({"session_id":"s1","path":"/tmp/a"})),
        (
            "sftp.rename",
            json!({"session_id":"s1","from":"/tmp/a","to":"/tmp/b"}),
        ),
        (
            "sftp.copy",
            json!({"session_id":"s1","from":"/tmp/a","to":"/tmp/b"}),
        ),
        (
            "sftp.delete",
            json!({"session_id":"s1","path":"/tmp/a","recursive":true}),
        ),
        (
            "sftp.symlink",
            json!({"session_id":"s1","target":"a","link_path":"/tmp/link"}),
        ),
        ("sessions.details", json!({"session_id":"s1"})),
        ("sessions.close", json!({"session_id":"s1"})),
        ("sessions.reconnect", json!({"session_id":"s1"})),
    ]
}
