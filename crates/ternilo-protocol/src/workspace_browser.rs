use serde::{Deserialize, Serialize};

use crate::HarnessError;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkspaceRequest {
    Info,
    List { path: String },
    Read { path: String },
    Open { app_id: String },
}

impl WorkspaceRequest {
    pub fn validate(&self) -> Result<(), HarnessError> {
        match self {
            Self::Info => Ok(()),
            Self::List { path } | Self::Read { path } => {
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
