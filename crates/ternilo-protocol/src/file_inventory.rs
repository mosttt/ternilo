use serde::{Deserialize, Serialize};

use crate::{
    Attachment, HarnessError, RunId, SessionEvent, SessionEventKind, SessionId, SubmissionId,
    UserMessageSource, WorkspaceId,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionFileKind {
    Upload,
    Generated,
}

impl SessionFileKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Upload => "upload",
            Self::Generated => "generated",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileSourceStatus {
    Online,
    Offline,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionFileItem {
    /// Stable within the containing session; each generated version has its own ID.
    pub id: String,
    pub session_id: SessionId,
    pub session_title: String,
    pub session_archived: bool,
    pub workspace_id: WorkspaceId,
    pub workspace_name: String,
    pub kind: SessionFileKind,
    pub name: String,
    pub media_type: String,
    pub path: Option<String>,
    pub occurred_at_ms: u64,
    pub event_seq: Option<u64>,
    pub run_id: RunId,
    pub source_status: FileSourceStatus,
    pub attachment_index: u32,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OfflineFileSource {
    pub workspace_id: WorkspaceId,
    pub executor_id: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionFilePage {
    pub items: Vec<SessionFileItem>,
    pub next_cursor: Option<String>,
    pub offline_sources: Vec<OfflineFileSource>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionFileContent {
    pub name: String,
    pub media_type: String,
    /// Original file bytes, including UTF-8 text, encoded uniformly as base64.
    pub content_base64: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionFileQuery {
    pub workspace_id: Option<WorkspaceId>,
    pub session_id: Option<SessionId>,
    pub kind: Option<SessionFileKind>,
    pub query: Option<String>,
    pub cursor: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: u16,
}

const fn default_limit() -> u16 {
    100
}

impl Default for SessionFileQuery {
    fn default() -> Self {
        Self {
            workspace_id: None,
            session_id: None,
            kind: None,
            query: None,
            cursor: None,
            limit: default_limit(),
        }
    }
}

impl SessionFileQuery {
    pub fn validate(&self) -> Result<(), HarnessError> {
        if !(1..=200).contains(&self.limit) {
            return Err(HarnessError::invalid(
                "file page limit must be between 1 and 200",
            ));
        }
        if let Some(id) = &self.session_id {
            id.validate()?;
        }
        if let Some(id) = &self.workspace_id {
            id.validate()?;
        }
        if self.query.as_ref().is_some_and(|query| query.len() > 256) {
            return Err(HarnessError::invalid(
                "file query must contain at most 256 bytes",
            ));
        }
        self.parsed_cursor()?;
        Ok(())
    }

    pub fn parsed_cursor(&self) -> Result<Option<SessionFileCursor>, HarnessError> {
        self.cursor
            .as_deref()
            .map(|cursor| {
                if cursor.len() > 512 {
                    return Err(HarnessError::invalid("invalid file cursor"));
                }
                let value: SessionFileCursor = serde_json::from_str(cursor)
                    .map_err(|_| HarnessError::invalid("invalid file cursor"))?;
                value.session_id.validate()?;
                SessionFileLocator::parse(&value.file_id)?;
                Ok(value)
            })
            .transpose()
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionFileCursor {
    pub occurred_at_ms: u64,
    pub session_id: SessionId,
    pub file_id: String,
}

impl SessionFileItem {
    #[must_use]
    pub fn cursor(&self) -> SessionFileCursor {
        SessionFileCursor {
            occurred_at_ms: self.occurred_at_ms,
            session_id: self.session_id.clone(),
            file_id: self.id.clone(),
        }
    }
}

impl SessionFileCursor {
    pub fn encode(&self) -> Result<String, HarnessError> {
        serde_json::to_string(self)
            .map_err(|error| HarnessError::execution(format!("encode file cursor: {error}")))
    }
}

pub struct SessionFileReference<'a> {
    pub id: String,
    pub kind: SessionFileKind,
    pub path: Option<&'a str>,
    pub attachment_index: u32,
    pub attachment: &'a Attachment,
    pub occurred_at_ms: u64,
    pub event_seq: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SessionFileLocator {
    Submission {
        submission_id: SubmissionId,
        attachment_index: u32,
    },
    Event {
        kind: SessionFileKind,
        event_seq: u64,
        attachment_index: u32,
    },
}

impl SessionFileLocator {
    pub fn parse(value: &str) -> Result<Self, HarnessError> {
        if let Some(value) = value.strip_prefix("submission-") {
            let (id, index) = value
                .rsplit_once('-')
                .ok_or_else(|| HarnessError::invalid("invalid submission file ID"))?;
            let submission_id = SubmissionId::new(id);
            submission_id.validate()?;
            let attachment_index = index
                .parse()
                .map_err(|_| HarnessError::invalid("invalid file attachment index"))?;
            return Ok(Self::Submission {
                submission_id,
                attachment_index,
            });
        }
        let mut parts = value.split('-');
        let kind = match parts.next() {
            Some("upload") => SessionFileKind::Upload,
            Some("generated") => SessionFileKind::Generated,
            _ => return Err(HarnessError::invalid("invalid session file ID")),
        };
        let event_seq = parts
            .next()
            .and_then(|part| part.parse().ok())
            .ok_or_else(|| HarnessError::invalid("invalid file event sequence"))?;
        let attachment_index = parts
            .next()
            .and_then(|part| part.parse().ok())
            .ok_or_else(|| HarnessError::invalid("invalid file attachment index"))?;
        if parts.next().is_some() || (kind == SessionFileKind::Generated && attachment_index != 0) {
            return Err(HarnessError::invalid("invalid session file ID"));
        }
        Ok(Self::Event {
            kind,
            event_seq,
            attachment_index,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcceptedSessionUpload {
    pub submission_id: SubmissionId,
    pub attachment_index: u32,
    pub created_at_ms: u64,
    pub submitted_run_id: RunId,
    pub attachment: Attachment,
}

impl AcceptedSessionUpload {
    #[must_use]
    pub fn file_id(&self) -> String {
        submission_file_id(&self.submission_id, self.attachment_index)
    }

    #[must_use]
    pub fn metadata(&self) -> AcceptedUploadMetadata {
        AcceptedUploadMetadata {
            submission_id: self.submission_id.clone(),
            attachment_index: self.attachment_index,
            created_at_ms: self.created_at_ms,
            submitted_run_id: self.submitted_run_id.clone(),
            name: self.attachment.name.clone(),
            media_type: self.attachment.media_type.clone(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcceptedUploadMetadata {
    pub submission_id: SubmissionId,
    pub attachment_index: u32,
    pub created_at_ms: u64,
    pub submitted_run_id: RunId,
    pub name: String,
    pub media_type: String,
}

impl AcceptedUploadMetadata {
    pub fn validate(&self) -> Result<(), HarnessError> {
        self.submission_id.validate()?;
        self.submitted_run_id.validate()?;
        if self.attachment_index >= 10
            || self.name.trim().is_empty()
            || self.name.len() > 1024
            || self.media_type.trim().is_empty()
            || self.media_type.len() > 256
        {
            return Err(HarnessError::invalid("invalid accepted upload metadata"));
        }
        Ok(())
    }

    #[must_use]
    pub fn file_id(&self) -> String {
        submission_file_id(&self.submission_id, self.attachment_index)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcceptedUploadChange {
    pub seq: u64,
    pub session_id: SessionId,
    pub kind: AcceptedUploadChangeKind,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum AcceptedUploadChangeKind {
    UploadAccepted { upload: AcceptedUploadMetadata },
    SessionDeleted,
}

#[must_use]
pub fn submission_file_id(submission_id: &SubmissionId, attachment_index: u32) -> String {
    format!("submission-{submission_id}-{attachment_index}")
}

/// Enumerate only durable upload and generated-file facts, never paths mentioned in prose.
#[must_use]
pub fn session_file_references(event: &SessionEvent) -> Vec<SessionFileReference<'_>> {
    match &event.kind {
        SessionEventKind::UserMessage {
            attachments,
            source,
            ..
        } => attachments
            .iter()
            .enumerate()
            .filter_map(|(index, attachment)| {
                let index = u32::try_from(index).ok()?;
                Some(SessionFileReference {
                    id: match source {
                        Some(UserMessageSource::Submission { submission_id, .. }) => {
                            submission_file_id(submission_id, index)
                        }
                        _ => format!("upload-{}-{index}", event.seq),
                    },
                    kind: SessionFileKind::Upload,
                    path: None,
                    attachment_index: index,
                    attachment,
                    occurred_at_ms: match source {
                        Some(UserMessageSource::Submission { created_at_ms, .. }) => *created_at_ms,
                        _ => event.occurred_at_ms,
                    },
                    event_seq: Some(event.seq),
                })
            })
            .collect(),
        SessionEventKind::DeliverableProduced {
            path, attachment, ..
        } => vec![SessionFileReference {
            id: format!("generated-{}-0", event.seq),
            kind: SessionFileKind::Generated,
            path: Some(path),
            attachment_index: 0,
            attachment,
            occurred_at_ms: event.occurred_at_ms,
            event_seq: Some(event.seq),
        }],
        _ => Vec::new(),
    }
}
