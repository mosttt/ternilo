use serde::{Deserialize, Serialize};
use serde_json::Value;
use ternilo_cloud::{CloudSessionRecord, CloudSessionState};
use ternilo_control::{
    EdgeSessionRecord, ExecutorRecord, ResourceAccess, WorkspacePlacement, WorkspaceRecord,
};
use ternilo_protocol::{
    Attachment, PermissionPreset, PluginEntry, SessionId, SessionIdentity, SessionMode,
    SubagentSessionMetadata, UserId, WorkspaceId,
};
use ternilo_transport::ExecutorId;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CreateSessionRequest {
    pub(crate) workspace_id: WorkspaceId,
    pub(crate) session_id: Option<String>,
    pub(crate) agent_id: Option<String>,
    pub(crate) agent_preset: Option<String>,
    pub(crate) permissions: Option<PermissionPreset>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UpdateSessionRequest {
    pub(crate) title: Option<String>,
    pub(crate) permissions: Option<PermissionPreset>,
    pub(crate) model: Option<Value>,
    pub(crate) model_token_limit: Option<u64>,
    pub(crate) agent_preset: Option<String>,
    pub(crate) profile_plugins: Option<Vec<PluginEntry>>,
    pub(crate) mode: Option<SessionMode>,
}

impl UpdateSessionRequest {
    pub(crate) fn has_changes(&self) -> bool {
        self.title.is_some()
            || self.permissions.is_some()
            || self.model.is_some()
            || self.model_token_limit.is_some()
            || self.agent_preset.is_some()
            || self.profile_plugins.is_some()
            || self.mode.is_some()
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TurnRequest {
    pub(crate) input: String,
    pub(crate) run_id: Option<String>,
    #[serde(default)]
    pub(crate) attachments: Vec<Attachment>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ResolveAttachmentRequest {
    pub(crate) attachment: Attachment,
}

#[derive(Deserialize)]
pub(crate) struct PendingQuestionQuery {
    pub(crate) session_id: SessionId,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DefaultModelTargetQuery {
    pub(crate) session_id: Option<SessionId>,
    pub(crate) workspace_id: Option<WorkspaceId>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AnswerQuestionRequest {
    pub(crate) selected: Vec<String>,
    pub(crate) custom: Option<String>,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ForkSessionRequest {
    pub(crate) at_seq: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FeedbackRequest {
    pub(crate) target_seq: u64,
    pub(crate) expected_revision: u64,
    pub(crate) rating: Option<ternilo_protocol::FeedbackRating>,
    pub(crate) note: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CommandFeedbackRequest {
    pub(crate) text: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SubagentFollowupRequest {
    pub(crate) message: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RenameWorkspaceRequest {
    pub(crate) title: String,
}

#[derive(Deserialize)]
pub(crate) struct SearchQuery {
    pub(crate) query: String,
    pub(crate) limit: Option<u32>,
    #[serde(default)]
    pub(crate) online_computers_only: bool,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DirectoryQuery {
    pub(crate) path: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CreateDirectoryRequest {
    pub(crate) parent: String,
    pub(crate) name: String,
}

#[derive(Serialize)]
pub(crate) struct ExecutionTarget {
    pub(crate) executor_id: ExecutorId,
    pub(crate) display_name: Option<String>,
    pub(crate) project_id: Option<String>,
    pub(crate) state: String,
    pub(crate) connected: bool,
    pub(crate) last_seen_at_ms: Option<u64>,
}

impl ExecutionTarget {
    pub(crate) fn from_record(record: ExecutorRecord, connected: bool) -> Self {
        Self {
            executor_id: record.executor_id,
            display_name: record.management.display_name,
            project_id: record.project_id,
            state: record.state,
            connected,
            last_seen_at_ms: record.last_seen_at_ms,
        }
    }
}

#[derive(Clone, Serialize)]
pub(crate) struct WorkbenchWorkspace {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) access: Option<ResourceAccess>,
    pub(crate) workspace_id: WorkspaceId,
    pub(crate) path: String,
    pub(crate) title: String,
    pub(crate) created_at_ms: u64,
    pub(crate) updated_at_ms: u64,
    pub(crate) placement: WorkspacePlacement,
    pub(crate) node_id: Option<ExecutorId>,
    pub(crate) node_name: Option<String>,
    pub(crate) status: String,
    pub(crate) project_id: String,
    pub(crate) owner_user_id: UserId,
}

impl WorkbenchWorkspace {
    pub(crate) fn with_access(mut self, access: ResourceAccess) -> Self {
        self.owner_user_id = access.owner_user_id.clone();
        self.access = Some(access);
        self
    }

    pub(crate) fn from_record(record: WorkspaceRecord, connected: bool) -> Self {
        let status = match record.placement {
            WorkspacePlacement::Cloud => "ready",
            WorkspacePlacement::LocalNode if connected => "online",
            WorkspacePlacement::LocalNode => "offline",
        }
        .to_owned();
        let path = match record.placement {
            WorkspacePlacement::Cloud => format!("云端 / {}", record.name),
            WorkspacePlacement::LocalNode => format!("此电脑 / {}", record.name),
        };
        Self {
            access: None,
            workspace_id: record.workspace_id,
            path,
            title: record.name,
            created_at_ms: record.created_at_ms,
            updated_at_ms: record.updated_at_ms,
            placement: record.placement,
            node_id: record.executor_id,
            node_name: None,
            status,
            project_id: record.project_id,
            owner_user_id: record.owner_user_id,
        }
    }
}

#[derive(Clone, Serialize)]
pub(crate) struct WorkbenchSession {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) access: Option<ResourceAccess>,
    pub(crate) identity: SessionIdentity,
    pub(crate) workspace_id: WorkspaceId,
    pub(crate) placement: WorkspacePlacement,
    pub(crate) workspace_path: String,
    pub(crate) parent_session_id: Option<SessionId>,
    pub(crate) subagent: Option<SubagentSessionMetadata>,
    pub(crate) title: String,
    pub(crate) archived_at_ms: Option<u64>,
    pub(crate) blank: bool,
    pub(crate) permissions: PermissionPreset,
    pub(crate) model: Value,
    pub(crate) model_token_limit: Option<u64>,
    pub(crate) agent_preset: String,
    pub(crate) preset_plugins: Vec<PluginEntry>,
    pub(crate) profile_plugins: Vec<PluginEntry>,
    pub(crate) mode: SessionMode,
    pub(crate) created_at_ms: u64,
    pub(crate) updated_at_ms: u64,
}

impl WorkbenchSession {
    pub(crate) fn with_access(mut self, access: ResourceAccess) -> Self {
        self.access = Some(access);
        self
    }

    pub(crate) fn cloud(session: CloudSessionRecord, workspace_path: String) -> Self {
        let blank = session.state == CloudSessionState::Idle && session.last_seq.is_none();
        Self {
            access: None,
            identity: SessionIdentity {
                tenant_id: session.tenant_id,
                user_id: session.user_id,
                agent_id: session.agent_id,
                session_id: session.session_id,
            },
            workspace_id: session.workspace_id,
            placement: WorkspacePlacement::Cloud,
            workspace_path,
            parent_session_id: session.parent_session_id,
            subagent: session.subagent,
            title: session.title,
            archived_at_ms: session.archived_at_ms,
            blank,
            permissions: session.permissions,
            model_token_limit: Some(session.reserved_model_tokens),
            model: serde_json::to_value(session.model.as_ref().map_or(
                ternilo_protocol::DefaultModelSelection::ProfileDefault,
                ternilo_protocol::RunModelSnapshot::selection,
            ))
            .expect("model selections serialize"),
            agent_preset: session.agent_preset,
            preset_plugins: Vec::new(),
            profile_plugins: session.profile_plugins,
            mode: session.mode,
            created_at_ms: session.created_at_ms,
            updated_at_ms: session.updated_at_ms,
        }
    }

    pub(crate) fn edge(session: EdgeSessionRecord, workspace_path: String) -> Self {
        let metadata = session.metadata;
        Self {
            access: None,
            identity: SessionIdentity {
                tenant_id: session.tenant_id,
                user_id: session.owner_user_id,
                agent_id: ternilo_protocol::AgentId::new("local-node"),
                session_id: session.session_id,
            },
            workspace_id: session.workspace_id,
            placement: WorkspacePlacement::LocalNode,
            workspace_path,
            parent_session_id: metadata.parent_session_id,
            subagent: metadata.subagent,
            title: metadata.title,
            archived_at_ms: metadata.archived_at_ms,
            blank: metadata.blank,
            permissions: metadata.permissions,
            model: metadata.model,
            model_token_limit: None,
            agent_preset: metadata.agent_preset,
            preset_plugins: metadata.preset_plugins,
            profile_plugins: metadata.profile_plugins,
            mode: metadata.mode,
            created_at_ms: metadata.created_at_ms,
            updated_at_ms: metadata.updated_at_ms.max(session.updated_at_ms),
        }
    }
}

#[derive(Serialize)]
pub(crate) struct WorkbenchState {
    pub(crate) workspaces: Vec<WorkbenchWorkspace>,
    pub(crate) sessions: Vec<WorkbenchSession>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NodeSessionSnapshot {
    pub(crate) identity: SessionIdentity,
    pub(crate) workspace_id: WorkspaceId,
    /// Parsed to enforce the Node response shape, but never copied into
    /// Control storage or a browser response.
    pub(crate) workspace_path: String,
    #[serde(default)]
    pub(crate) parent_session_id: Option<SessionId>,
    #[serde(default)]
    pub(crate) subagent: Option<SubagentSessionMetadata>,
    pub(crate) title: String,
    #[serde(default)]
    pub(crate) archived_at_ms: Option<u64>,
    #[serde(default)]
    pub(crate) blank: bool,
    pub(crate) permissions: PermissionPreset,
    pub(crate) model: Value,
    #[serde(default, rename = "server_model")]
    pub(crate) _server_model: Option<ternilo_protocol::RunModelSnapshot>,
    pub(crate) agent_preset: String,
    #[serde(default)]
    pub(crate) preset_plugins: Vec<PluginEntry>,
    #[serde(default)]
    pub(crate) profile_plugins: Vec<PluginEntry>,
    pub(crate) mode: SessionMode,
    pub(crate) created_at_ms: u64,
    pub(crate) updated_at_ms: u64,
}

impl NodeSessionSnapshot {
    pub(crate) fn metadata(
        &self,
        parent_session_id: Option<SessionId>,
    ) -> ternilo_control::EdgeSessionMetadata {
        ternilo_control::EdgeSessionMetadata {
            parent_session_id,
            subagent: self.subagent.clone(),
            title: self.title.clone(),
            archived_at_ms: self.archived_at_ms,
            blank: self.blank,
            permissions: self.permissions,
            model: self.model.clone(),
            server_model: None,
            agent_preset: self.agent_preset.clone(),
            preset_plugins: self.preset_plugins.clone(),
            profile_plugins: self.profile_plugins.clone(),
            mode: self.mode,
            created_at_ms: self.created_at_ms,
            updated_at_ms: self.updated_at_ms,
        }
    }
}

#[cfg(test)]
mod tests {
    use ternilo_protocol::{AgentId, SubagentId, SubagentTranscriptKind, TenantId, UserId};

    use super::*;

    #[test]
    fn cloud_workbench_session_preserves_subagent_metadata() {
        let metadata = SubagentSessionMetadata {
            subagent_id: SubagentId::new("researcher"),
            provider: "in-process".to_owned(),
            transcript_kind: SubagentTranscriptKind::Conversation,
        };
        let session = CloudSessionRecord {
            tenant_id: TenantId::new("tenant"),
            session_id: SessionId::new("child"),
            user_id: UserId::new("user"),
            project_id: "project".to_owned(),
            workspace_id: WorkspaceId::new("workspace"),
            parent_session_id: Some(SessionId::new("root")),
            subagent: Some(metadata.clone()),
            agent_id: AgentId::new("agent"),
            title: "Researcher".to_owned(),
            archived_at_ms: None,
            state: CloudSessionState::Idle,
            execution: None,
            permissions: PermissionPreset::WorkspaceWrite,
            model: None,
            reserved_model_tokens: 100,
            agent_preset: "standard".to_owned(),
            profile_plugins: Vec::new(),
            mode: SessionMode::Execute,
            last_seq: None,
            created_at_ms: 1,
            updated_at_ms: 1,
        };

        assert_eq!(
            WorkbenchSession::cloud(session, "Cloud".to_owned()).subagent,
            Some(metadata)
        );
    }
}
