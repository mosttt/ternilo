use std::{collections::BTreeSet, path::PathBuf};

use serde::{Deserialize, Serialize};
use ternilo_protocol::{
    HarnessError, PermissionPreset, PluginEntry, SessionIdentity, SessionMode,
    SubagentSessionMetadata, WorkspaceId, validate_agent_preset_id,
};
use tokio::sync::Mutex;

use crate::persistence::atomic_replace;

const STATE_VERSION: u32 = 1;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Workspace {
    pub workspace_id: WorkspaceId,
    pub path: String,
    pub title: String,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalSession {
    pub identity: SessionIdentity,
    pub workspace_id: WorkspaceId,
    /// Canonical working directory captured when the session is created.
    /// Workspace registration is only sidebar organization and may be removed
    /// without changing this immutable execution binding.
    pub workspace_path: String,
    #[serde(default)]
    pub parent_session_id: Option<ternilo_protocol::SessionId>,
    #[serde(default)]
    pub subagent: Option<SubagentSessionMetadata>,
    pub title: String,
    #[serde(default)]
    pub archived_at_ms: Option<u64>,
    #[serde(default)]
    pub blank: bool,
    #[serde(default)]
    pub permissions: PermissionPreset,
    #[serde(default)]
    pub model: ModelSelection,
    #[serde(default)]
    pub server_model: Option<ternilo_protocol::RunModelSnapshot>,
    #[serde(default = "default_agent_preset")]
    pub agent_preset: String,
    #[serde(default)]
    pub preset_plugins: Vec<PluginEntry>,
    #[serde(default)]
    pub profile_plugins: Vec<PluginEntry>,
    #[serde(default)]
    pub mode: SessionMode,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

fn default_agent_preset() -> String {
    crate::DEFAULT_AGENT_PRESET.to_owned()
}

pub use ternilo_protocol::DefaultModelSelection as ModelSelection;

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct LocalStateSnapshot {
    pub workspaces: Vec<Workspace>,
    pub sessions: Vec<LocalSession>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StateDocument {
    schema_version: u32,
    workspaces: Vec<Workspace>,
    sessions: Vec<LocalSession>,
}

impl Default for StateDocument {
    fn default() -> Self {
        Self {
            schema_version: STATE_VERSION,
            workspaces: Vec::new(),
            sessions: Vec::new(),
        }
    }
}

pub struct LocalState {
    root: PathBuf,
    state_path: PathBuf,
    document: Mutex<StateDocument>,
}

impl LocalState {
    pub async fn open(root: PathBuf) -> Result<Self, HarnessError> {
        crate::prepare_data_dir(&root)?;
        let state_path = root.join("data/state.json");
        let document = match tokio::fs::read(&state_path).await {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|error| {
                HarnessError::execution(format!("parse {}: {error}", state_path.display()))
            })?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => StateDocument::default(),
            Err(error) => {
                return Err(HarnessError::execution(format!(
                    "read {}: {error}",
                    state_path.display()
                )));
            }
        };
        validate_document(&document)?;
        Ok(Self {
            root,
            state_path,
            document: Mutex::new(document),
        })
    }

    #[must_use]
    pub fn sessions_dir(&self) -> PathBuf {
        self.root.join("data/sessions")
    }

    #[must_use]
    pub(crate) fn search_index_path(&self) -> PathBuf {
        self.root.join("cache/session-search.sqlite3")
    }

    #[must_use]
    pub(crate) fn projection_cache_path(&self) -> PathBuf {
        self.root.join("cache/session-projections.sqlite3")
    }

    pub async fn snapshot(&self) -> LocalStateSnapshot {
        let document = self.document.lock().await;
        LocalStateSnapshot {
            workspaces: document.workspaces.clone(),
            sessions: document.sessions.clone(),
        }
    }

    pub async fn workspace(&self, id: &WorkspaceId) -> Option<Workspace> {
        self.document
            .lock()
            .await
            .workspaces
            .iter()
            .find(|workspace| &workspace.workspace_id == id)
            .cloned()
    }

    pub async fn workspace_by_path(&self, path: &str) -> Option<Workspace> {
        self.document
            .lock()
            .await
            .workspaces
            .iter()
            .find(|workspace| workspace.path == path)
            .cloned()
    }

    pub async fn session(&self, id: &str) -> Option<LocalSession> {
        self.document
            .lock()
            .await
            .sessions
            .iter()
            .find(|session| session.identity.session_id.as_str() == id)
            .cloned()
    }

    pub async fn insert_workspace(&self, workspace: Workspace) -> Result<(), HarnessError> {
        let mut guard = self.document.lock().await;
        if guard.workspaces.iter().any(|current| {
            current.workspace_id == workspace.workspace_id || current.path == workspace.path
        }) {
            return Err(HarnessError::invalid("workspace already exists"));
        }
        let mut next = guard.clone();
        next.workspaces.push(workspace);
        self.persist(&next).await?;
        *guard = next;
        Ok(())
    }

    pub async fn insert_session(&self, session: LocalSession) -> Result<(), HarnessError> {
        self.insert_session_inner(session, true).await
    }

    /// Persist a child copied from an existing session. Unlike a user-created
    /// blank session, a fork may retain a binding whose sidebar registration
    /// has since been removed.
    pub async fn insert_forked_session(&self, session: LocalSession) -> Result<(), HarnessError> {
        self.insert_session_inner(session, false).await
    }

    async fn insert_session_inner(
        &self,
        session: LocalSession,
        require_registered_workspace: bool,
    ) -> Result<(), HarnessError> {
        let mut guard = self.document.lock().await;
        if guard
            .sessions
            .iter()
            .any(|current| current.identity.session_id == session.identity.session_id)
        {
            return Err(HarnessError::invalid("session already exists"));
        }
        if require_registered_workspace {
            let workspace = guard
                .workspaces
                .iter()
                .find(|workspace| workspace.workspace_id == session.workspace_id)
                .ok_or_else(|| HarnessError::invalid("session workspace does not exist"))?;
            if workspace.path != session.workspace_path {
                return Err(HarnessError::invalid(
                    "session workspace path does not match its registration",
                ));
            }
        } else {
            let parent_id = session.parent_session_id.as_ref().ok_or_else(|| {
                HarnessError::invalid("a forked session requires a parent session")
            })?;
            let parent = guard
                .sessions
                .iter()
                .find(|candidate| &candidate.identity.session_id == parent_id)
                .ok_or_else(|| HarnessError::invalid("fork parent session does not exist"))?;
            if parent.workspace_id != session.workspace_id
                || parent.workspace_path != session.workspace_path
            {
                return Err(HarnessError::invalid(
                    "fork must retain its parent workspace binding",
                ));
            }
        }
        let mut next = guard.clone();
        next.sessions.push(session);
        self.persist(&next).await?;
        *guard = next;
        Ok(())
    }

    pub async fn touch_session(
        &self,
        id: &str,
        title: Option<String>,
        updated_at_ms: u64,
    ) -> Result<LocalSession, HarnessError> {
        let mut guard = self.document.lock().await;
        let mut next = guard.clone();
        let session = next
            .sessions
            .iter_mut()
            .find(|session| session.identity.session_id.as_str() == id)
            .ok_or_else(|| HarnessError::invalid(format!("unknown session {id:?}")))?;
        if let Some(title) = title {
            session.title = title;
        }
        session.updated_at_ms = updated_at_ms;
        let result = session.clone();
        self.persist(&next).await?;
        *guard = next;
        Ok(result)
    }

    pub async fn set_generated_title(
        &self,
        id: &str,
        title: String,
        updated_at_ms: u64,
    ) -> Result<bool, HarnessError> {
        let mut guard = self.document.lock().await;
        let current = guard
            .sessions
            .iter()
            .find(|session| session.identity.session_id.as_str() == id)
            .ok_or_else(|| HarnessError::invalid(format!("unknown session {id:?}")))?;
        if current.title != "New session" {
            return Ok(false);
        }
        let mut next = guard.clone();
        let session = next
            .sessions
            .iter_mut()
            .find(|session| session.identity.session_id.as_str() == id)
            .expect("session was found in the source document");
        session.title = title;
        session.updated_at_ms = updated_at_ms;
        self.persist(&next).await?;
        *guard = next;
        Ok(true)
    }

    pub async fn mark_session_started(
        &self,
        id: &str,
        updated_at_ms: u64,
    ) -> Result<LocalSession, HarnessError> {
        let mut guard = self.document.lock().await;
        let current = guard
            .sessions
            .iter()
            .find(|session| session.identity.session_id.as_str() == id)
            .ok_or_else(|| HarnessError::invalid(format!("unknown session {id:?}")))?;
        if !current.blank {
            return Ok(current.clone());
        }
        let mut next = guard.clone();
        let session = next
            .sessions
            .iter_mut()
            .find(|session| session.identity.session_id.as_str() == id)
            .expect("session was found in the source document");
        session.blank = false;
        session.updated_at_ms = updated_at_ms;
        let result = session.clone();
        self.persist(&next).await?;
        *guard = next;
        Ok(result)
    }

    pub async fn archive_session(
        &self,
        id: &str,
        archived_at_ms: u64,
    ) -> Result<LocalSession, HarnessError> {
        let mut guard = self.document.lock().await;
        let current = guard
            .sessions
            .iter()
            .find(|session| session.identity.session_id.as_str() == id)
            .ok_or_else(|| HarnessError::invalid(format!("unknown session {id:?}")))?;
        if current.archived_at_ms.is_some() {
            return Ok(current.clone());
        }
        let mut next = guard.clone();
        let session = next
            .sessions
            .iter_mut()
            .find(|session| session.identity.session_id.as_str() == id)
            .expect("session was found in the source document");
        session.archived_at_ms = Some(archived_at_ms);
        let result = session.clone();
        self.persist(&next).await?;
        *guard = next;
        Ok(result)
    }

    pub async fn restore_session(&self, id: &str) -> Result<LocalSession, HarnessError> {
        let mut guard = self.document.lock().await;
        let current = guard
            .sessions
            .iter()
            .find(|session| session.identity.session_id.as_str() == id)
            .ok_or_else(|| HarnessError::invalid(format!("unknown session {id:?}")))?;
        if current.archived_at_ms.is_none() {
            return Ok(current.clone());
        }
        let mut next = guard.clone();
        let session = next
            .sessions
            .iter_mut()
            .find(|session| session.identity.session_id.as_str() == id)
            .expect("session was found in the source document");
        session.archived_at_ms = None;
        let result = session.clone();
        self.persist(&next).await?;
        *guard = next;
        Ok(result)
    }

    pub async fn rename_workspace(
        &self,
        id: &WorkspaceId,
        title: String,
        updated_at_ms: u64,
    ) -> Result<Workspace, HarnessError> {
        let title = title.trim();
        if title.is_empty() {
            return Err(HarnessError::invalid("workspace title must not be blank"));
        }
        let mut guard = self.document.lock().await;
        let current = guard
            .workspaces
            .iter()
            .find(|workspace| &workspace.workspace_id == id)
            .ok_or_else(|| HarnessError::invalid(format!("unknown workspace {id}")))?;
        if current.title == title {
            return Ok(current.clone());
        }
        if guard
            .workspaces
            .iter()
            .any(|workspace| &workspace.workspace_id != id && workspace.title == title)
        {
            return Err(HarnessError::invalid(format!(
                "workspace title {title:?} is already in use"
            )));
        }
        let mut next = guard.clone();
        let workspace = next
            .workspaces
            .iter_mut()
            .find(|workspace| &workspace.workspace_id == id)
            .expect("workspace was found in the source document");
        workspace.title = title.to_owned();
        workspace.updated_at_ms = updated_at_ms;
        let result = workspace.clone();
        self.persist(&next).await?;
        *guard = next;
        Ok(result)
    }

    /// Remove only the workspace registration. Sessions keep their immutable
    /// `workspace_path` and event logs, so callers can surface them as
    /// ungrouped and resume them after a restart.
    pub async fn unregister_workspace(&self, id: &WorkspaceId) -> Result<Workspace, HarnessError> {
        let mut guard = self.document.lock().await;
        let mut next = guard.clone();
        let index = next
            .workspaces
            .iter()
            .position(|workspace| &workspace.workspace_id == id)
            .ok_or_else(|| HarnessError::invalid(format!("unknown workspace {id}")))?;
        let removed = next.workspaces.remove(index);
        self.persist(&next).await?;
        *guard = next;
        Ok(removed)
    }

    pub async fn replace_session(
        &self,
        id: &str,
        replacement: LocalSession,
    ) -> Result<LocalSession, HarnessError> {
        let mut guard = self.document.lock().await;
        let mut next = guard.clone();
        let session = next
            .sessions
            .iter_mut()
            .find(|session| session.identity.session_id.as_str() == id)
            .ok_or_else(|| HarnessError::invalid(format!("unknown session {id:?}")))?;
        if replacement.identity != session.identity
            || replacement.workspace_id != session.workspace_id
            || replacement.workspace_path != session.workspace_path
            || replacement.created_at_ms != session.created_at_ms
        {
            return Err(HarnessError::invalid(
                "session update cannot change identity, workspace binding, or creation time",
            ));
        }
        replacement.clone_into(session);
        self.persist(&next).await?;
        *guard = next;
        Ok(replacement)
    }

    /// Change only the session mode when it still matches `expected`. This is
    /// used by the reviewed plan exit so a concurrent settings write cannot
    /// be replaced with a stale whole-session snapshot.
    pub async fn transition_mode(
        &self,
        id: &str,
        expected: SessionMode,
        mode: SessionMode,
        updated_at_ms: u64,
    ) -> Result<bool, HarnessError> {
        let mut guard = self.document.lock().await;
        let current = guard
            .sessions
            .iter()
            .find(|session| session.identity.session_id.as_str() == id)
            .ok_or_else(|| HarnessError::invalid(format!("unknown session {id:?}")))?;
        if current.mode != expected {
            return Ok(false);
        }
        let mut next = guard.clone();
        let session = next
            .sessions
            .iter_mut()
            .find(|session| session.identity.session_id.as_str() == id)
            .expect("session was found in the source document");
        session.mode = mode;
        session.updated_at_ms = updated_at_ms;
        self.persist(&next).await?;
        *guard = next;
        Ok(true)
    }

    pub async fn delete_session(&self, id: &str) -> Result<LocalSession, HarnessError> {
        let mut guard = self.document.lock().await;
        let mut next = guard.clone();
        let index = next
            .sessions
            .iter()
            .position(|session| session.identity.session_id.as_str() == id)
            .ok_or_else(|| HarnessError::invalid(format!("unknown session {id:?}")))?;
        let removed = next.sessions.remove(index);
        self.persist(&next).await?;
        *guard = next;
        Ok(removed)
    }

    async fn persist(&self, document: &StateDocument) -> Result<(), HarnessError> {
        let bytes = serde_json::to_vec_pretty(document)
            .map_err(|error| HarnessError::execution(format!("serialize local state: {error}")))?;
        atomic_replace(&self.state_path, &bytes, false).await
    }
}

fn validate_document(document: &StateDocument) -> Result<(), HarnessError> {
    if document.schema_version != STATE_VERSION {
        return Err(HarnessError::execution(format!(
            "unsupported local state version {}; expected {STATE_VERSION}",
            document.schema_version
        )));
    }
    let mut workspace_ids = BTreeSet::new();
    let mut workspace_paths = BTreeSet::new();
    for workspace in &document.workspaces {
        workspace.workspace_id.validate()?;
        if workspace.path.trim().is_empty()
            || !workspace_ids.insert(workspace.workspace_id.clone())
            || !workspace_paths.insert(workspace.path.as_str())
        {
            return Err(HarnessError::execution(
                "invalid or duplicate workspace in local state",
            ));
        }
    }
    let session_ids = document
        .sessions
        .iter()
        .map(|session| session.identity.session_id.clone())
        .collect::<BTreeSet<_>>();
    if session_ids.len() != document.sessions.len() {
        return Err(HarnessError::execution("duplicate session in local state"));
    }
    for session in &document.sessions {
        session.identity.validate()?;
        session.workspace_id.validate()?;
        validate_model(&session.model)?;
        validate_agent_preset_id(&session.agent_preset)?;
        if session.workspace_path.trim().is_empty()
            || session
                .parent_session_id
                .as_ref()
                .is_some_and(|parent| parent == &session.identity.session_id)
        {
            return Err(HarnessError::execution("invalid session in local state"));
        }
    }
    Ok(())
}

pub(crate) fn validate_model(model: &ModelSelection) -> Result<(), HarnessError> {
    model.validate()
}

#[cfg(test)]
mod tests {
    use ternilo_protocol::{AgentId, SessionId, TenantId, UserId};

    use super::*;

    fn session(workspace_id: WorkspaceId, workspace_path: &str) -> LocalSession {
        LocalSession {
            server_model: None,
            identity: SessionIdentity {
                tenant_id: TenantId::new("local"),
                user_id: UserId::new("local-user"),
                agent_id: AgentId::new("standard"),
                session_id: SessionId::new("session"),
            },
            workspace_id,
            workspace_path: workspace_path.to_owned(),
            parent_session_id: None,
            subagent: None,
            title: "New session".to_owned(),
            archived_at_ms: None,
            blank: true,
            permissions: PermissionPreset::WorkspaceWrite,
            model: ModelSelection::ProfileDefault,
            agent_preset: "standard".to_owned(),
            preset_plugins: Vec::new(),
            profile_plugins: Vec::new(),
            mode: SessionMode::Execute,
            created_at_ms: 10,
            updated_at_ms: 10,
        }
    }

    #[tokio::test]
    async fn blank_and_archive_state_are_durable() {
        let directory = tempfile::tempdir().unwrap();
        let workspace_id = WorkspaceId::new("workspace");
        let state = LocalState::open(directory.path().to_path_buf())
            .await
            .unwrap();
        state
            .insert_workspace(Workspace {
                workspace_id: workspace_id.clone(),
                path: directory.path().to_string_lossy().into_owned(),
                title: "workspace".to_owned(),
                created_at_ms: 10,
                updated_at_ms: 10,
            })
            .await
            .unwrap();
        state
            .insert_session(session(workspace_id, directory.path().to_str().unwrap()))
            .await
            .unwrap();

        let started = state.mark_session_started("session", 20).await.unwrap();
        assert!(!started.blank);
        assert_eq!(started.updated_at_ms, 20);
        let archived = state.archive_session("session", 30).await.unwrap();
        assert_eq!(archived.archived_at_ms, Some(30));
        assert_eq!(archived.updated_at_ms, 20);
        assert_eq!(
            state
                .archive_session("session", 40)
                .await
                .unwrap()
                .archived_at_ms,
            Some(30)
        );
        drop(state);

        let reopened = LocalState::open(directory.path().to_path_buf())
            .await
            .unwrap();
        let restored = reopened.session("session").await.unwrap();
        assert!(!restored.blank);
        assert_eq!(restored.archived_at_ms, Some(30));
    }

    #[tokio::test]
    async fn generated_title_is_compare_and_set_against_manual_rename() {
        let directory = tempfile::tempdir().unwrap();
        let workspace_id = WorkspaceId::new("workspace");
        let state = LocalState::open(directory.path().to_path_buf())
            .await
            .unwrap();
        state
            .insert_workspace(Workspace {
                workspace_id: workspace_id.clone(),
                path: directory.path().to_string_lossy().into_owned(),
                title: "workspace".to_owned(),
                created_at_ms: 10,
                updated_at_ms: 10,
            })
            .await
            .unwrap();
        state
            .insert_session(session(workspace_id, directory.path().to_str().unwrap()))
            .await
            .unwrap();

        assert!(
            state
                .set_generated_title("session", "Generated title".to_owned(), 20)
                .await
                .unwrap()
        );
        let mut renamed = state.session("session").await.unwrap();
        renamed.title = "Manual title".to_owned();
        renamed.updated_at_ms = 30;
        state.replace_session("session", renamed).await.unwrap();
        assert!(
            !state
                .set_generated_title("session", "Late generated title".to_owned(), 40)
                .await
                .unwrap()
        );
        assert_eq!(
            state.session("session").await.unwrap().title,
            "Manual title"
        );
    }

    #[tokio::test]
    async fn workspace_registration_lifecycle_is_durable_without_owning_sessions() {
        let directory = tempfile::tempdir().unwrap();
        let workspace_id = WorkspaceId::new("workspace");
        let state = LocalState::open(directory.path().to_path_buf())
            .await
            .unwrap();
        let workspace = Workspace {
            workspace_id: workspace_id.clone(),
            path: directory.path().to_string_lossy().into_owned(),
            title: "before".to_owned(),
            created_at_ms: 10,
            updated_at_ms: 10,
        };
        state.insert_workspace(workspace).await.unwrap();
        state
            .insert_session(session(
                workspace_id.clone(),
                directory.path().to_str().unwrap(),
            ))
            .await
            .unwrap();

        let renamed = state
            .rename_workspace(&workspace_id, "  after  ".to_owned(), 20)
            .await
            .unwrap();
        assert_eq!(renamed.title, "after");
        assert_eq!(renamed.updated_at_ms, 20);
        state.unregister_workspace(&workspace_id).await.unwrap();
        assert!(state.snapshot().await.workspaces.is_empty());
        assert_eq!(state.snapshot().await.sessions.len(), 1);
        drop(state);

        let reopened = LocalState::open(directory.path().to_path_buf())
            .await
            .unwrap();
        let snapshot = reopened.snapshot().await;
        assert!(snapshot.workspaces.is_empty());
        assert_eq!(snapshot.sessions[0].workspace_id, workspace_id);
        assert_eq!(
            snapshot.sessions[0].workspace_path,
            directory.path().to_str().unwrap()
        );

        let mut child = session(workspace_id.clone(), directory.path().to_str().unwrap());
        child.identity.session_id = SessionId::new("child");
        child.parent_session_id = Some(SessionId::new("session"));
        child.blank = false;
        reopened.insert_forked_session(child).await.unwrap();
        assert_eq!(reopened.snapshot().await.sessions.len(), 2);

        reopened
            .insert_workspace(Workspace {
                workspace_id: WorkspaceId::new("replacement"),
                path: directory.path().to_string_lossy().into_owned(),
                title: "after".to_owned(),
                created_at_ms: 30,
                updated_at_ms: 30,
            })
            .await
            .unwrap();
        assert_eq!(
            reopened.snapshot().await.sessions[0].workspace_id,
            workspace_id
        );
    }

    #[tokio::test]
    async fn workspace_rename_rejects_blank_duplicate_and_unknown_targets() {
        let directory = tempfile::tempdir().unwrap();
        let state = LocalState::open(directory.path().to_path_buf())
            .await
            .unwrap();
        for (id, title, offset) in [("one", "one", 1), ("two", "two", 2)] {
            state
                .insert_workspace(Workspace {
                    workspace_id: WorkspaceId::new(id),
                    path: directory.path().join(id).to_string_lossy().into_owned(),
                    title: title.to_owned(),
                    created_at_ms: offset,
                    updated_at_ms: offset,
                })
                .await
                .unwrap();
        }
        assert!(
            state
                .rename_workspace(&WorkspaceId::new("one"), " ".to_owned(), 3)
                .await
                .is_err()
        );
        assert!(
            state
                .rename_workspace(&WorkspaceId::new("one"), "two".to_owned(), 3)
                .await
                .is_err()
        );
        assert!(
            state
                .unregister_workspace(&WorkspaceId::new("missing"))
                .await
                .is_err()
        );
    }

    #[test]
    fn absent_lifecycle_fields_use_legacy_defaults() {
        let mut value =
            serde_json::to_value(session(WorkspaceId::new("workspace"), "/tmp")).unwrap();
        let object = value.as_object_mut().unwrap();
        object.remove("blank");
        object.remove("archived_at_ms");
        let decoded: LocalSession = serde_json::from_value(value).unwrap();
        assert!(!decoded.blank);
        assert_eq!(decoded.archived_at_ms, None);
    }
}
