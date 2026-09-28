use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    future::Future,
    path::Path,
    pin::Pin,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use linorun_core::CallContext;
use sha2::{Digest as _, Sha256};
use ternilo_kernel::AgentTeamProvider;
use ternilo_protocol::{
    AgentTeamId, AgentTeamMember, AgentTeamMemberId, AgentTeamMemberRole, AgentTeamMessage,
    AgentTeamMessageId, AgentTeamMessageSend, AgentTeamSnapshot, AgentTeamTask,
    AgentTeamTaskCreate, AgentTeamTaskId, AgentTeamTaskReplace, AgentTeamTaskStatus, HarnessError,
    SessionId,
};
use tokio::sync::broadcast;
use tokio_rusqlite::{Connection, rusqlite::OptionalExtension as _};

use crate::{
    LocalInvalidationCategory, LocalInvalidationNotification, LocalSession,
    notifications::publish_invalidation, state::LocalState,
};

const SCHEMA: &str = r"
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS agent_team_tasks (
    team_id TEXT NOT NULL,
    task_id TEXT NOT NULL,
    subject TEXT NOT NULL,
    description TEXT NOT NULL,
    status TEXT NOT NULL,
    owner_member_id TEXT,
    revision INTEGER NOT NULL CHECK (revision > 0),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY (team_id, task_id)
);

CREATE TABLE IF NOT EXISTS agent_team_task_dependencies (
    team_id TEXT NOT NULL,
    task_id TEXT NOT NULL,
    dependency_task_id TEXT NOT NULL,
    PRIMARY KEY (team_id, task_id, dependency_task_id),
    CHECK (task_id <> dependency_task_id),
    FOREIGN KEY (team_id, task_id)
        REFERENCES agent_team_tasks(team_id, task_id) ON DELETE CASCADE,
    FOREIGN KEY (team_id, dependency_task_id)
        REFERENCES agent_team_tasks(team_id, task_id) ON DELETE RESTRICT
);

CREATE TABLE IF NOT EXISTS agent_team_messages (
    team_id TEXT NOT NULL,
    message_id TEXT NOT NULL,
    from_member_id TEXT NOT NULL,
    to_member_id TEXT NOT NULL,
    content TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    read_at_ms INTEGER,
    PRIMARY KEY (team_id, message_id)
);

CREATE INDEX IF NOT EXISTS agent_team_tasks_updated
    ON agent_team_tasks(team_id, updated_at_ms, task_id);
CREATE INDEX IF NOT EXISTS agent_team_dependencies_target
    ON agent_team_task_dependencies(team_id, dependency_task_id);
CREATE INDEX IF NOT EXISTS agent_team_mailbox
    ON agent_team_messages(team_id, to_member_id, created_at_ms, message_id);
";

#[derive(Clone)]
pub(crate) struct LocalAgentTeamStore {
    connection: Connection,
}

#[derive(Clone)]
struct TeamScope {
    team_id: AgentTeamId,
    current_member_id: AgentTeamMemberId,
    members: Vec<AgentTeamMember>,
    session_ids: Vec<SessionId>,
}

impl TeamScope {
    fn contains_member(&self, member: &AgentTeamMemberId) -> bool {
        self.members.iter().any(|candidate| &candidate.id == member)
    }
}

pub(crate) struct LocalAgentTeamProvider {
    store: Arc<LocalAgentTeamStore>,
    state: Arc<LocalState>,
    session_id: SessionId,
    invalidations: broadcast::Sender<LocalInvalidationNotification>,
}

impl LocalAgentTeamProvider {
    pub(crate) fn new(
        store: Arc<LocalAgentTeamStore>,
        state: Arc<LocalState>,
        session_id: SessionId,
        invalidations: broadcast::Sender<LocalInvalidationNotification>,
    ) -> Self {
        Self {
            store,
            state,
            session_id,
            invalidations,
        }
    }

    async fn scope(&self) -> Result<TeamScope, HarnessError> {
        team_scope(&self.state.snapshot().await.sessions, &self.session_id)
    }

    fn notify_team(&self, session_ids: &[SessionId], revision: Option<u64>) {
        for session_id in session_ids {
            publish_invalidation(
                &self.invalidations,
                Some(session_id.as_str()),
                LocalInvalidationCategory::AgentTeam,
                revision,
            );
        }
    }

    pub(crate) async fn current_session_ids(&self) -> Result<Vec<SessionId>, HarnessError> {
        Ok(self.scope().await?.session_ids)
    }

    pub(crate) async fn invalidate_current_team(&self, revision: Option<u64>) {
        if let Ok(scope) = self.scope().await {
            self.notify_team(&scope.session_ids, revision);
        }
    }

    pub(crate) async fn current_snapshot(&self) -> Result<AgentTeamSnapshot, HarnessError> {
        self.store.snapshot(self.scope().await?).await
    }

    pub(crate) async fn current_create_task(
        &self,
        request: AgentTeamTaskCreate,
    ) -> Result<AgentTeamTask, HarnessError> {
        let scope = self.scope().await?;
        let session_ids = scope.session_ids.clone();
        let task = self.store.create_task(scope, request).await?;
        self.notify_team(&session_ids, Some(task.revision));
        Ok(task)
    }

    pub(crate) async fn current_replace_task(
        &self,
        task_id: AgentTeamTaskId,
        request: AgentTeamTaskReplace,
    ) -> Result<AgentTeamTask, HarnessError> {
        let scope = self.scope().await?;
        let session_ids = scope.session_ids.clone();
        let task = self.store.replace_task(scope, task_id, request).await?;
        self.notify_team(&session_ids, Some(task.revision));
        Ok(task)
    }

    pub(crate) async fn current_delete_task(
        &self,
        task_id: AgentTeamTaskId,
        expected_revision: u64,
    ) -> Result<(), HarnessError> {
        let scope = self.scope().await?;
        let session_ids = scope.session_ids.clone();
        self.store
            .delete_task(scope, task_id, expected_revision)
            .await?;
        self.notify_team(&session_ids, Some(expected_revision));
        Ok(())
    }

    pub(crate) async fn current_send_message(
        &self,
        request: AgentTeamMessageSend,
    ) -> Result<AgentTeamMessage, HarnessError> {
        let scope = self.scope().await?;
        let session_ids = scope.session_ids.clone();
        let message = self.store.send_message(scope, request).await?;
        self.notify_team(&session_ids, None);
        Ok(message)
    }

    pub(crate) async fn current_mark_message_read(
        &self,
        message_id: AgentTeamMessageId,
    ) -> Result<AgentTeamMessage, HarnessError> {
        let scope = self.scope().await?;
        let session_ids = scope.session_ids.clone();
        let message = self.store.mark_message_read(scope, message_id).await?;
        self.notify_team(&session_ids, None);
        Ok(message)
    }
}

impl AgentTeamProvider for LocalAgentTeamProvider {
    fn snapshot<'a>(
        &'a self,
        _: CallContext<()>,
    ) -> Pin<Box<dyn Future<Output = Result<AgentTeamSnapshot, HarnessError>> + Send + 'a>> {
        Box::pin(async move { self.current_snapshot().await })
    }

    fn create_task<'a>(
        &'a self,
        _: CallContext<()>,
        request: AgentTeamTaskCreate,
    ) -> Pin<Box<dyn Future<Output = Result<AgentTeamTask, HarnessError>> + Send + 'a>> {
        Box::pin(async move { self.current_create_task(request).await })
    }

    fn replace_task<'a>(
        &'a self,
        _: CallContext<()>,
        task_id: AgentTeamTaskId,
        request: AgentTeamTaskReplace,
    ) -> Pin<Box<dyn Future<Output = Result<AgentTeamTask, HarnessError>> + Send + 'a>> {
        Box::pin(async move { self.current_replace_task(task_id, request).await })
    }

    fn delete_task<'a>(
        &'a self,
        _: CallContext<()>,
        task_id: AgentTeamTaskId,
        expected_revision: u64,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(async move { self.current_delete_task(task_id, expected_revision).await })
    }

    fn send_message<'a>(
        &'a self,
        _: CallContext<()>,
        request: AgentTeamMessageSend,
    ) -> Pin<Box<dyn Future<Output = Result<AgentTeamMessage, HarnessError>> + Send + 'a>> {
        Box::pin(async move { self.current_send_message(request).await })
    }

    fn mark_message_read<'a>(
        &'a self,
        _: CallContext<()>,
        message_id: AgentTeamMessageId,
    ) -> Pin<Box<dyn Future<Output = Result<AgentTeamMessage, HarnessError>> + Send + 'a>> {
        Box::pin(async move { self.current_mark_message_read(message_id).await })
    }
}

impl LocalAgentTeamStore {
    pub(crate) async fn open(path: &Path) -> Result<Self, HarnessError> {
        let connection = Connection::open(path).await.map_err(|error| {
            HarnessError::execution(format!(
                "open local Agent Team database {}: {error}",
                path.display()
            ))
        })?;
        set_private_permissions(path).await?;
        connection
            .call(|database| -> Result<(), HarnessError> {
                database
                    .busy_timeout(Duration::from_secs(5))
                    .map_err(sqlite_error)?;
                database
                    .pragma_update(None, "journal_mode", "WAL")
                    .map_err(sqlite_error)?;
                database
                    .pragma_update(None, "synchronous", "FULL")
                    .map_err(sqlite_error)?;
                database.execute_batch(SCHEMA).map_err(sqlite_error)
            })
            .await
            .map_err(worker_error)?;
        Ok(Self { connection })
    }

    async fn snapshot(&self, scope: TeamScope) -> Result<AgentTeamSnapshot, HarnessError> {
        let team_id = scope.team_id.clone();
        let current_member_id = scope.current_member_id.clone();
        let (tasks, messages) = self
            .connection
            .call(move |database| {
                let tasks = load_tasks(database, &team_id)?;
                let mut statement = database
                    .prepare(
                        "SELECT message_id, from_member_id, to_member_id, content,
                                created_at_ms, read_at_ms
                         FROM agent_team_messages
                         WHERE team_id = ?1 AND (from_member_id = ?2 OR to_member_id = ?2)
                         ORDER BY created_at_ms, message_id",
                    )
                    .map_err(sqlite_error)?;
                let rows = statement
                    .query_map([team_id.as_str(), current_member_id.as_str()], |row| {
                        Ok(AgentTeamMessage {
                            id: AgentTeamMessageId::new(row.get::<_, String>(0)?),
                            from: AgentTeamMemberId::new(row.get::<_, String>(1)?),
                            to: AgentTeamMemberId::new(row.get::<_, String>(2)?),
                            content: row.get(3)?,
                            created_at_ms: row.get(4)?,
                            read_at_ms: row.get(5)?,
                        })
                    })
                    .map_err(sqlite_error)?;
                let messages = rows.collect::<Result<Vec<_>, _>>().map_err(sqlite_error)?;
                Ok((tasks, messages))
            })
            .await
            .map_err(worker_error)?;
        let snapshot = AgentTeamSnapshot {
            team_id: scope.team_id,
            current_member_id: scope.current_member_id,
            members: scope.members,
            tasks,
            messages,
        };
        snapshot.validate()?;
        Ok(snapshot)
    }

    async fn create_task(
        &self,
        scope: TeamScope,
        request: AgentTeamTaskCreate,
    ) -> Result<AgentTeamTask, HarnessError> {
        request.validate()?;
        validate_owner(&scope, request.owner.as_ref())?;
        let team_id = scope.team_id;
        let now = now_ms()?;
        self.connection
            .call(move |database| {
                let transaction = database.transaction().map_err(sqlite_error)?;
                validate_dependencies_exist(&transaction, &team_id, &request.dependencies)?;
                let random: String = transaction
                    .query_row("SELECT lower(hex(randomblob(16)))", [], |row| row.get(0))
                    .map_err(sqlite_error)?;
                let task_id = AgentTeamTaskId::new(format!("task-{random}"));
                transaction
                    .execute(
                        "INSERT INTO agent_team_tasks
                         (team_id, task_id, subject, description, status, owner_member_id,
                          revision, created_at_ms, updated_at_ms)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, ?7, ?7)",
                        tokio_rusqlite::rusqlite::params![
                            team_id.as_str(),
                            task_id.as_str(),
                            request.subject,
                            request.description,
                            status_as_str(request.status),
                            request.owner.as_ref().map(AgentTeamMemberId::as_str),
                            now,
                        ],
                    )
                    .map_err(sqlite_error)?;
                replace_dependencies(&transaction, &team_id, &task_id, &request.dependencies)?;
                transaction.commit().map_err(sqlite_error)?;
                Ok(AgentTeamTask {
                    id: task_id,
                    subject: request.subject,
                    description: request.description,
                    status: request.status,
                    dependencies: request.dependencies,
                    owner: request.owner,
                    revision: 1,
                    created_at_ms: now,
                    updated_at_ms: now,
                })
            })
            .await
            .map_err(worker_error)
    }

    async fn replace_task(
        &self,
        scope: TeamScope,
        task_id: AgentTeamTaskId,
        request: AgentTeamTaskReplace,
    ) -> Result<AgentTeamTask, HarnessError> {
        task_id.validate()?;
        request.validate()?;
        validate_owner(&scope, request.owner.as_ref())?;
        if request.dependencies.iter().any(|id| id == &task_id) {
            return Err(HarnessError::invalid(
                "Agent Team task cannot depend on itself",
            ));
        }
        let team_id = scope.team_id;
        let now = now_ms()?;
        self.connection
            .call(move |database| {
                let transaction = database.transaction().map_err(sqlite_error)?;
                let current = transaction
                    .query_row(
                        "SELECT revision, created_at_ms FROM agent_team_tasks
                         WHERE team_id = ?1 AND task_id = ?2",
                        [team_id.as_str(), task_id.as_str()],
                        |row| Ok((row.get::<_, u64>(0)?, row.get::<_, u64>(1)?)),
                    )
                    .optional()
                    .map_err(sqlite_error)?
                    .ok_or_else(|| HarnessError::invalid("unknown Agent Team task"))?;
                if current.0 != request.expected_revision {
                    return Err(HarnessError::conflict(format!(
                        "Agent Team task revision is {}; expected {}",
                        current.0, request.expected_revision
                    )));
                }
                validate_dependencies_exist(&transaction, &team_id, &request.dependencies)?;
                let mut graph = load_dependency_graph(&transaction, &team_id)?;
                graph.insert(task_id.clone(), request.dependencies.clone());
                validate_acyclic(&graph)?;
                let revision = current.0 + 1;
                transaction
                    .execute(
                        "UPDATE agent_team_tasks
                         SET subject = ?3, description = ?4, status = ?5,
                             owner_member_id = ?6, revision = ?7, updated_at_ms = ?8
                         WHERE team_id = ?1 AND task_id = ?2 AND revision = ?9",
                        tokio_rusqlite::rusqlite::params![
                            team_id.as_str(),
                            task_id.as_str(),
                            request.subject,
                            request.description,
                            status_as_str(request.status),
                            request.owner.as_ref().map(AgentTeamMemberId::as_str),
                            revision,
                            now,
                            request.expected_revision,
                        ],
                    )
                    .map_err(sqlite_error)?;
                replace_dependencies(&transaction, &team_id, &task_id, &request.dependencies)?;
                transaction.commit().map_err(sqlite_error)?;
                Ok(AgentTeamTask {
                    id: task_id,
                    subject: request.subject,
                    description: request.description,
                    status: request.status,
                    dependencies: request.dependencies,
                    owner: request.owner,
                    revision,
                    created_at_ms: current.1,
                    updated_at_ms: now,
                })
            })
            .await
            .map_err(worker_error)
    }

    async fn delete_task(
        &self,
        scope: TeamScope,
        task_id: AgentTeamTaskId,
        expected_revision: u64,
    ) -> Result<(), HarnessError> {
        task_id.validate()?;
        if expected_revision == 0 {
            return Err(HarnessError::invalid(
                "Agent Team task expected revision must be positive",
            ));
        }
        let team_id = scope.team_id;
        self.connection
            .call(move |database| {
                let transaction = database.transaction().map_err(sqlite_error)?;
                let revision = transaction
                    .query_row(
                        "SELECT revision FROM agent_team_tasks
                         WHERE team_id = ?1 AND task_id = ?2",
                        [team_id.as_str(), task_id.as_str()],
                        |row| row.get::<_, u64>(0),
                    )
                    .optional()
                    .map_err(sqlite_error)?
                    .ok_or_else(|| HarnessError::invalid("unknown Agent Team task"))?;
                if revision != expected_revision {
                    return Err(HarnessError::conflict(format!(
                        "Agent Team task revision is {revision}; expected {expected_revision}"
                    )));
                }
                let referenced = transaction
                    .query_row(
                        "SELECT EXISTS(
                            SELECT 1 FROM agent_team_task_dependencies
                            WHERE team_id = ?1 AND dependency_task_id = ?2
                         )",
                        [team_id.as_str(), task_id.as_str()],
                        |row| row.get::<_, bool>(0),
                    )
                    .map_err(sqlite_error)?;
                if referenced {
                    return Err(HarnessError::conflict(
                        "Agent Team task is still required by another task",
                    ));
                }
                transaction
                    .execute(
                        "DELETE FROM agent_team_tasks
                         WHERE team_id = ?1 AND task_id = ?2 AND revision = ?3",
                        tokio_rusqlite::rusqlite::params![
                            team_id.as_str(),
                            task_id.as_str(),
                            expected_revision,
                        ],
                    )
                    .map_err(sqlite_error)?;
                transaction.commit().map_err(sqlite_error)
            })
            .await
            .map_err(worker_error)
    }

    async fn send_message(
        &self,
        scope: TeamScope,
        request: AgentTeamMessageSend,
    ) -> Result<AgentTeamMessage, HarnessError> {
        request.validate()?;
        if !scope.contains_member(&request.to) {
            return Err(HarnessError::invalid(
                "Agent Team message recipient is not a Team member",
            ));
        }
        let team_id = scope.team_id;
        let from = scope.current_member_id;
        let now = now_ms()?;
        self.connection
            .call(move |database| {
                let random: String = database
                    .query_row("SELECT lower(hex(randomblob(16)))", [], |row| row.get(0))
                    .map_err(sqlite_error)?;
                let message_id = AgentTeamMessageId::new(format!("message-{random}"));
                database
                    .execute(
                        "INSERT INTO agent_team_messages
                         (team_id, message_id, from_member_id, to_member_id, content,
                          created_at_ms, read_at_ms)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL)",
                        tokio_rusqlite::rusqlite::params![
                            team_id.as_str(),
                            message_id.as_str(),
                            from.as_str(),
                            request.to.as_str(),
                            request.content,
                            now,
                        ],
                    )
                    .map_err(sqlite_error)?;
                Ok(AgentTeamMessage {
                    id: message_id,
                    from,
                    to: request.to,
                    content: request.content,
                    created_at_ms: now,
                    read_at_ms: None,
                })
            })
            .await
            .map_err(worker_error)
    }

    async fn mark_message_read(
        &self,
        scope: TeamScope,
        message_id: AgentTeamMessageId,
    ) -> Result<AgentTeamMessage, HarnessError> {
        message_id.validate()?;
        let team_id = scope.team_id;
        let current_member_id = scope.current_member_id;
        let now = now_ms()?;
        self.connection
            .call(move |database| {
                let mut message = database
                    .query_row(
                        "SELECT from_member_id, to_member_id, content, created_at_ms, read_at_ms
                         FROM agent_team_messages
                         WHERE team_id = ?1 AND message_id = ?2 AND to_member_id = ?3",
                        [
                            team_id.as_str(),
                            message_id.as_str(),
                            current_member_id.as_str(),
                        ],
                        |row| {
                            Ok(AgentTeamMessage {
                                id: message_id.clone(),
                                from: AgentTeamMemberId::new(row.get::<_, String>(0)?),
                                to: AgentTeamMemberId::new(row.get::<_, String>(1)?),
                                content: row.get(2)?,
                                created_at_ms: row.get(3)?,
                                read_at_ms: row.get(4)?,
                            })
                        },
                    )
                    .optional()
                    .map_err(sqlite_error)?
                    .ok_or_else(|| HarnessError::invalid("unknown Agent Team inbox message"))?;
                if message.read_at_ms.is_none() {
                    database
                        .execute(
                            "UPDATE agent_team_messages SET read_at_ms = ?4
                             WHERE team_id = ?1 AND message_id = ?2 AND to_member_id = ?3",
                            tokio_rusqlite::rusqlite::params![
                                team_id.as_str(),
                                message_id.as_str(),
                                current_member_id.as_str(),
                                now,
                            ],
                        )
                        .map_err(sqlite_error)?;
                    message.read_at_ms = Some(now);
                }
                Ok(message)
            })
            .await
            .map_err(worker_error)
    }
}

fn team_scope(
    sessions: &[LocalSession],
    current_id: &SessionId,
) -> Result<TeamScope, HarnessError> {
    let by_id = sessions
        .iter()
        .map(|session| (session.identity.session_id.clone(), session))
        .collect::<BTreeMap<_, _>>();
    let mut root = *by_id
        .get(current_id)
        .ok_or_else(|| HarnessError::invalid("unknown Agent Team Session"))?;
    let mut ancestors = BTreeSet::new();
    while root.subagent.is_some() {
        if !ancestors.insert(root.identity.session_id.clone()) {
            return Err(HarnessError::execution("cyclic Subagent Session ancestry"));
        }
        let parent_id = root
            .parent_session_id
            .as_ref()
            .ok_or_else(|| HarnessError::execution("Subagent Session has no persisted parent"))?;
        root = *by_id
            .get(parent_id)
            .ok_or_else(|| HarnessError::execution("Subagent Session parent is not persisted"))?;
    }

    let team_id = AgentTeamId::new(format!(
        "team-{}",
        digest_id(&[
            root.identity.tenant_id.as_str(),
            root.identity.user_id.as_str(),
            root.identity.session_id.as_str(),
        ])
    ));
    let mut members = Vec::new();
    let mut member_by_session = BTreeMap::new();
    collect_members(
        sessions,
        root,
        &team_id,
        None,
        &mut BTreeSet::new(),
        &mut members,
        &mut member_by_session,
    )?;
    let current_member_id = member_by_session
        .get(current_id)
        .cloned()
        .ok_or_else(|| HarnessError::execution("current Session is not in its Agent Team"))?;
    let session_ids = member_by_session.into_keys().collect();
    Ok(TeamScope {
        team_id,
        current_member_id,
        members,
        session_ids,
    })
}

fn collect_members(
    sessions: &[LocalSession],
    session: &LocalSession,
    team_id: &AgentTeamId,
    parent_member_id: Option<AgentTeamMemberId>,
    visited: &mut BTreeSet<SessionId>,
    members: &mut Vec<AgentTeamMember>,
    member_by_session: &mut BTreeMap<SessionId, AgentTeamMemberId>,
) -> Result<(), HarnessError> {
    if !visited.insert(session.identity.session_id.clone()) {
        return Err(HarnessError::execution("cyclic Agent Team Session tree"));
    }
    let member_id = AgentTeamMemberId::new(format!(
        "member-{}",
        digest_id(&[team_id.as_str(), session.identity.session_id.as_str()])
    ));
    let (subagent_id, provider, role) = match &session.subagent {
        Some(metadata) => (
            Some(metadata.subagent_id.clone()),
            Some(metadata.provider.clone()),
            AgentTeamMemberRole::Subagent,
        ),
        None => (None, None, AgentTeamMemberRole::Lead),
    };
    let member = AgentTeamMember {
        id: member_id.clone(),
        parent_id: parent_member_id,
        subagent_id,
        label: session.title.clone(),
        provider,
        role,
    };
    member.validate()?;
    member_by_session.insert(session.identity.session_id.clone(), member_id.clone());
    members.push(member);

    let mut children = sessions
        .iter()
        .filter(|candidate| {
            candidate.subagent.is_some()
                && candidate.parent_session_id.as_ref() == Some(&session.identity.session_id)
        })
        .collect::<Vec<_>>();
    children.sort_by(|left, right| left.identity.session_id.cmp(&right.identity.session_id));
    for child in children {
        collect_members(
            sessions,
            child,
            team_id,
            Some(member_id.clone()),
            visited,
            members,
            member_by_session,
        )?;
    }
    Ok(())
}

fn digest_id(parts: &[&str]) -> String {
    let mut digest = Sha256::new();
    for part in parts {
        digest.update(part.as_bytes());
        digest.update([0]);
    }
    let mut encoded = String::with_capacity(32);
    for byte in &digest.finalize()[..16] {
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded
}

fn validate_owner(
    scope: &TeamScope,
    owner: Option<&AgentTeamMemberId>,
) -> Result<(), HarnessError> {
    if owner.is_some_and(|owner| !scope.contains_member(owner)) {
        Err(HarnessError::invalid(
            "Agent Team task owner is not a Team member",
        ))
    } else {
        Ok(())
    }
}

fn load_tasks(
    database: &tokio_rusqlite::rusqlite::Connection,
    team_id: &AgentTeamId,
) -> Result<Vec<AgentTeamTask>, HarnessError> {
    let mut dependencies = BTreeMap::<AgentTeamTaskId, Vec<AgentTeamTaskId>>::new();
    let mut dependency_statement = database
        .prepare(
            "SELECT task_id, dependency_task_id FROM agent_team_task_dependencies
             WHERE team_id = ?1 ORDER BY task_id, dependency_task_id",
        )
        .map_err(sqlite_error)?;
    let dependency_rows = dependency_statement
        .query_map([team_id.as_str()], |row| {
            Ok((
                AgentTeamTaskId::new(row.get::<_, String>(0)?),
                AgentTeamTaskId::new(row.get::<_, String>(1)?),
            ))
        })
        .map_err(sqlite_error)?;
    for row in dependency_rows {
        let (task, dependency) = row.map_err(sqlite_error)?;
        dependencies.entry(task).or_default().push(dependency);
    }

    let mut statement = database
        .prepare(
            "SELECT task_id, subject, description, status, owner_member_id,
                    revision, created_at_ms, updated_at_ms
             FROM agent_team_tasks WHERE team_id = ?1
             ORDER BY created_at_ms, task_id",
        )
        .map_err(sqlite_error)?;
    let rows = statement
        .query_map([team_id.as_str()], |row| {
            let task_id = AgentTeamTaskId::new(row.get::<_, String>(0)?);
            let status = row.get::<_, String>(3)?;
            Ok((
                task_id,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                status,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, u64>(5)?,
                row.get::<_, u64>(6)?,
                row.get::<_, u64>(7)?,
            ))
        })
        .map_err(sqlite_error)?;
    let mut tasks = Vec::new();
    for row in rows {
        let (id, subject, description, status, owner, revision, created_at_ms, updated_at_ms) =
            row.map_err(sqlite_error)?;
        tasks.push(AgentTeamTask {
            dependencies: dependencies.remove(&id).unwrap_or_default(),
            id,
            subject,
            description,
            status: status_from_str(&status)?,
            owner: owner.map(AgentTeamMemberId::new),
            revision,
            created_at_ms,
            updated_at_ms,
        });
    }
    Ok(tasks)
}

fn validate_dependencies_exist(
    database: &tokio_rusqlite::rusqlite::Connection,
    team_id: &AgentTeamId,
    dependencies: &[AgentTeamTaskId],
) -> Result<(), HarnessError> {
    for dependency in dependencies {
        let exists = database
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM agent_team_tasks WHERE team_id = ?1 AND task_id = ?2
                 )",
                [team_id.as_str(), dependency.as_str()],
                |row| row.get::<_, bool>(0),
            )
            .map_err(sqlite_error)?;
        if !exists {
            return Err(HarnessError::invalid(format!(
                "unknown Agent Team task dependency {}",
                dependency.as_str()
            )));
        }
    }
    Ok(())
}

fn replace_dependencies(
    database: &tokio_rusqlite::rusqlite::Connection,
    team_id: &AgentTeamId,
    task_id: &AgentTeamTaskId,
    dependencies: &[AgentTeamTaskId],
) -> Result<(), HarnessError> {
    database
        .execute(
            "DELETE FROM agent_team_task_dependencies WHERE team_id = ?1 AND task_id = ?2",
            [team_id.as_str(), task_id.as_str()],
        )
        .map_err(sqlite_error)?;
    for dependency in dependencies {
        database
            .execute(
                "INSERT INTO agent_team_task_dependencies
                 (team_id, task_id, dependency_task_id) VALUES (?1, ?2, ?3)",
                [team_id.as_str(), task_id.as_str(), dependency.as_str()],
            )
            .map_err(sqlite_error)?;
    }
    Ok(())
}

fn load_dependency_graph(
    database: &tokio_rusqlite::rusqlite::Connection,
    team_id: &AgentTeamId,
) -> Result<BTreeMap<AgentTeamTaskId, Vec<AgentTeamTaskId>>, HarnessError> {
    let mut graph = BTreeMap::new();
    let mut task_statement = database
        .prepare("SELECT task_id FROM agent_team_tasks WHERE team_id = ?1")
        .map_err(sqlite_error)?;
    let tasks = task_statement
        .query_map([team_id.as_str()], |row| {
            Ok(AgentTeamTaskId::new(row.get::<_, String>(0)?))
        })
        .map_err(sqlite_error)?;
    for task in tasks {
        graph.insert(task.map_err(sqlite_error)?, Vec::new());
    }
    let mut dependency_statement = database
        .prepare(
            "SELECT task_id, dependency_task_id FROM agent_team_task_dependencies
             WHERE team_id = ?1",
        )
        .map_err(sqlite_error)?;
    let dependencies = dependency_statement
        .query_map([team_id.as_str()], |row| {
            Ok((
                AgentTeamTaskId::new(row.get::<_, String>(0)?),
                AgentTeamTaskId::new(row.get::<_, String>(1)?),
            ))
        })
        .map_err(sqlite_error)?;
    for dependency in dependencies {
        let (task, dependency) = dependency.map_err(sqlite_error)?;
        graph.entry(task).or_default().push(dependency);
    }
    Ok(graph)
}

fn validate_acyclic(
    graph: &BTreeMap<AgentTeamTaskId, Vec<AgentTeamTaskId>>,
) -> Result<(), HarnessError> {
    fn visit(
        task: &AgentTeamTaskId,
        graph: &BTreeMap<AgentTeamTaskId, Vec<AgentTeamTaskId>>,
        visiting: &mut BTreeSet<AgentTeamTaskId>,
        visited: &mut BTreeSet<AgentTeamTaskId>,
    ) -> bool {
        if visited.contains(task) {
            return true;
        }
        if !visiting.insert(task.clone()) {
            return false;
        }
        for dependency in graph.get(task).into_iter().flatten() {
            if !visit(dependency, graph, visiting, visited) {
                return false;
            }
        }
        visiting.remove(task);
        visited.insert(task.clone());
        true
    }

    let mut visiting = BTreeSet::new();
    let mut visited = BTreeSet::new();
    if graph
        .keys()
        .all(|task| visit(task, graph, &mut visiting, &mut visited))
    {
        Ok(())
    } else {
        Err(HarnessError::invalid(
            "Agent Team task dependencies contain a cycle",
        ))
    }
}

const fn status_as_str(status: AgentTeamTaskStatus) -> &'static str {
    match status {
        AgentTeamTaskStatus::Pending => "pending",
        AgentTeamTaskStatus::InProgress => "in_progress",
        AgentTeamTaskStatus::Blocked => "blocked",
        AgentTeamTaskStatus::Completed => "completed",
        AgentTeamTaskStatus::Cancelled => "cancelled",
    }
}

fn status_from_str(status: &str) -> Result<AgentTeamTaskStatus, HarnessError> {
    match status {
        "pending" => Ok(AgentTeamTaskStatus::Pending),
        "in_progress" => Ok(AgentTeamTaskStatus::InProgress),
        "blocked" => Ok(AgentTeamTaskStatus::Blocked),
        "completed" => Ok(AgentTeamTaskStatus::Completed),
        "cancelled" => Ok(AgentTeamTaskStatus::Cancelled),
        _ => Err(HarnessError::execution(format!(
            "invalid Agent Team task status {status:?} in SQLite"
        ))),
    }
}

fn now_ms() -> Result<u64, HarnessError> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| HarnessError::execution(format!("system clock before epoch: {error}")))?;
    elapsed
        .as_millis()
        .try_into()
        .map_err(|_| HarnessError::execution("timestamp does not fit in u64"))
}

#[allow(clippy::needless_pass_by_value)]
fn sqlite_error(error: tokio_rusqlite::rusqlite::Error) -> HarnessError {
    HarnessError::execution(format!("local Agent Team SQLite error: {error}"))
}

#[allow(clippy::needless_pass_by_value)]
fn worker_error(error: tokio_rusqlite::Error<HarnessError>) -> HarnessError {
    match error {
        tokio_rusqlite::Error::Error(error) => error,
        other => HarnessError::execution(format!("local Agent Team worker error: {other}")),
    }
}

#[cfg(unix)]
async fn set_private_permissions(path: &Path) -> Result<(), HarnessError> {
    use std::os::unix::fs::PermissionsExt as _;
    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .await
        .map_err(|error| {
            HarnessError::execution(format!(
                "set private local Agent Team permissions {}: {error}",
                path.display()
            ))
        })
}

#[cfg(not(unix))]
async fn set_private_permissions(_: &Path) -> Result<(), HarnessError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use ternilo_protocol::{
        AgentId, PermissionPreset, SessionIdentity, SessionMode, SubagentId,
        SubagentSessionMetadata, SubagentTranscriptKind, TenantId, UserId, WorkspaceId,
    };

    use super::*;

    fn root_session(id: &str) -> LocalSession {
        LocalSession {
            server_model: None,
            identity: SessionIdentity {
                tenant_id: TenantId::new("tenant"),
                user_id: UserId::new("user"),
                agent_id: AgentId::new("agent"),
                session_id: SessionId::new(id),
            },
            workspace_id: WorkspaceId::new("workspace"),
            workspace_path: "/workspace".to_owned(),
            parent_session_id: None,
            subagent: None,
            title: "Lead".to_owned(),
            archived_at_ms: None,
            blank: false,
            permissions: PermissionPreset::WorkspaceWrite,
            model: crate::ModelSelection::ProfileDefault,
            agent_preset: crate::DEFAULT_AGENT_PRESET.to_owned(),
            preset_plugins: Vec::new(),
            profile_plugins: Vec::new(),
            mode: SessionMode::Execute,
            created_at_ms: 1,
            updated_at_ms: 1,
        }
    }

    fn subagent_session(id: &str, parent: &str, subagent_id: &str) -> LocalSession {
        let mut session = root_session(id);
        session.parent_session_id = Some(SessionId::new(parent));
        session.subagent = Some(SubagentSessionMetadata {
            subagent_id: SubagentId::new(subagent_id),
            provider: "in-process".to_owned(),
            transcript_kind: SubagentTranscriptKind::Conversation,
        });
        session.title = subagent_id.to_owned();
        session
    }

    #[test]
    fn only_canonical_subagent_descendants_share_a_team() {
        let root = root_session("root");
        let child = subagent_session("child", "root", "researcher");
        let grandchild = subagent_session("grandchild", "child", "reviewer");
        let mut ordinary_fork = root_session("fork");
        ordinary_fork.parent_session_id = Some(SessionId::new("root"));
        let sessions = vec![root, child, grandchild, ordinary_fork];

        let root_scope = team_scope(&sessions, &SessionId::new("root")).unwrap();
        let child_scope = team_scope(&sessions, &SessionId::new("child")).unwrap();
        let grandchild_scope = team_scope(&sessions, &SessionId::new("grandchild")).unwrap();
        assert_eq!(root_scope.team_id, child_scope.team_id);
        assert_eq!(root_scope.team_id, grandchild_scope.team_id);
        assert_eq!(root_scope.members.len(), 3);
        assert_ne!(root_scope.current_member_id, child_scope.current_member_id);

        let fork_scope = team_scope(&sessions, &SessionId::new("fork")).unwrap();
        assert_ne!(root_scope.team_id, fork_scope.team_id);
        assert_eq!(fork_scope.members.len(), 1);
    }

    #[tokio::test]
    async fn task_cas_dependencies_and_mailbox_are_durable() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("agent-team.sqlite3");
        let store = LocalAgentTeamStore::open(&path).await.unwrap();
        let scope = team_scope(&[root_session("root")], &SessionId::new("root")).unwrap();
        let member = scope.current_member_id.clone();
        let base = store
            .create_task(
                scope.clone(),
                AgentTeamTaskCreate {
                    subject: "Base".to_owned(),
                    description: String::new(),
                    status: AgentTeamTaskStatus::Completed,
                    dependencies: Vec::new(),
                    owner: Some(member.clone()),
                },
            )
            .await
            .unwrap();
        let dependent = store
            .create_task(
                scope.clone(),
                AgentTeamTaskCreate {
                    subject: "Dependent".to_owned(),
                    description: String::new(),
                    status: AgentTeamTaskStatus::Pending,
                    dependencies: vec![base.id.clone()],
                    owner: Some(member.clone()),
                },
            )
            .await
            .unwrap();
        assert!(
            store
                .delete_task(scope.clone(), base.id.clone(), base.revision)
                .await
                .is_err()
        );
        assert!(
            store
                .replace_task(
                    scope.clone(),
                    dependent.id.clone(),
                    AgentTeamTaskReplace {
                        expected_revision: 99,
                        subject: dependent.subject.clone(),
                        description: dependent.description.clone(),
                        status: dependent.status,
                        dependencies: dependent.dependencies.clone(),
                        owner: dependent.owner.clone(),
                    },
                )
                .await
                .is_err()
        );
        let message = store
            .send_message(
                scope.clone(),
                AgentTeamMessageSend {
                    to: member,
                    content: "done".to_owned(),
                },
            )
            .await
            .unwrap();
        store
            .mark_message_read(scope.clone(), message.id)
            .await
            .unwrap();
        drop(store);

        let reopened = LocalAgentTeamStore::open(&path).await.unwrap();
        let snapshot = reopened.snapshot(scope).await.unwrap();
        assert_eq!(snapshot.tasks.len(), 2);
        assert_eq!(snapshot.messages.len(), 1);
        assert!(snapshot.messages[0].read_at_ms.is_some());
    }
}
