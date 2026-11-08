use serde::{Deserialize, Serialize};

use crate::HarnessError;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkspaceRequest {
    Info,
    List {
        path: String,
    },
    Read {
        path: String,
    },
    Download {
        path: String,
        offset: u64,
        version: Option<FileDownloadVersion>,
    },
    Open {
        app_id: String,
    },
}

impl WorkspaceRequest {
    pub fn validate(&self) -> Result<(), HarnessError> {
        match self {
            Self::Info => Ok(()),
            Self::List { path } | Self::Read { path } | Self::Download { path, .. } => {
                if path.len() > 4096
                    || path.contains('\0')
                    || path.contains('\\')
                    || path.starts_with('/')
                    || path.split('/').any(|part| part == ".." || part == ".")
                {
                    return Err(HarnessError::invalid(
                        "workspace paths must be root-relative without traversal",
                    ));
                }
                if matches!(self, Self::Download { offset, version: None, .. } if *offset != 0) {
                    return Err(HarnessError::invalid(
                        "resuming a workspace download requires its file version",
                    ));
                }
                Ok(())
            }
            Self::Open { app_id } => {
                if app_id.is_empty()
                    || app_id.len() > 80
                    || !app_id
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                {
                    return Err(HarnessError::invalid("invalid desktop application ID"));
                }
                Ok(())
            }
        }
    }
}

pub const WORKSPACE_DOWNLOAD_CHUNK_BYTES: u64 = 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileDownloadVersion {
    pub bytes: u64,
    pub revision: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileDownloadChunk {
    pub offset: u64,
    pub version: FileDownloadVersion,
    pub content_base64: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkspaceApplication {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkspaceBrowserInfo {
    pub root: String,
    pub can_browse: bool,
    pub applications: Vec<WorkspaceApplication>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<SandboxReadiness>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SandboxReadiness {
    pub backend: String,
    pub status: SandboxReadinessStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub install_command: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxReadinessStatus {
    Ready,
    Missing,
    Unusable,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkspaceEntry {
    pub name: String,
    pub kind: WorkspaceEntryKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceEntryKind {
    Directory,
    File,
    Other,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkspaceDirectory {
    pub path: String,
    pub entries: Vec<WorkspaceEntry>,
    pub truncated: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkspacePreview {
    pub path: String,
    pub bytes: u64,
    pub media_type: String,
    pub encoding: String,
    pub content: String,
    pub truncated: bool,
}
