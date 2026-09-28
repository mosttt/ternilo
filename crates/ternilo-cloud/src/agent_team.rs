use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::random;
use sha2::{Digest as _, Sha256};
use sqlx::Row;
use ternilo_protocol::{
    AgentTeamId, AgentTeamMember, AgentTeamMemberId, AgentTeamMemberRole, AgentTeamMessage,
    AgentTeamMessageId, AgentTeamMessageSend, AgentTeamSnapshot, AgentTeamTask,
    AgentTeamTaskCreate, AgentTeamTaskId, AgentTeamTaskReplace, AgentTeamTaskStatus, HarnessError,
    SessionId, SubagentSessionMetadata, TenantId, UserId,
};

use crate::{
    CloudStore,
    commands::set_owner_scope,
    store::{database_error, from_i64, to_i64},
};

#[derive(Clone)]
struct TeamSession {
    session_id: SessionId,
    parent_session_id: Option<SessionId>,
    subagent: Option<SubagentSessionMetadata>,
    title: String,
}

#[derive(Clone)]
struct TeamScope {
    team_id: AgentTeamId,
    root_session_id: SessionId,
    current_member_id: AgentTeamMemberId,
    members: Vec<AgentTeamMember>,
}

impl TeamScope {
    fn contains_member(&self, member: &AgentTeamMemberId) -> bool {
        self.members.iter().any(|candidate| &candidate.id == member)
    }
}

impl CloudStore {
    pub async fn agent_team_snapshot(
        &self,
        tenant_id: &TenantId,
        actor_id: &UserId,
        session_id: &SessionId,
    ) -> Result<AgentTeamSnapshot, HarnessError> {
        let user_id = actor_id;
        validate_scope(tenant_id, user_id, session_id)?;
        let mut transaction = self.begin().await?;
        if self.worker_access.is_none()
            && self.database.backend() == ternilo_storage::Backend::Postgres
        {
            sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
                .execute(&mut *transaction)
                .await
                .map_err(database_error)?;
        }
        let owner_id = crate::sharing::session_owner_in(
            &mut transaction,
            tenant_id,
            actor_id,
            session_id,
            ternilo_control::ResourceAction::View,
        )
        .await?;
        let user_id = &owner_id;
        set_owner_scope(&mut transaction, tenant_id, user_id).await?;
        let scope =
            load_team_scope(&mut transaction, tenant_id, user_id, session_id, false).await?;
        let tasks = load_tasks(&mut transaction, tenant_id, user_id, &scope.team_id).await?;
        let rows = sqlx::query(
            "SELECT message_id, from_member_id, to_member_id, content,
                    created_at_ms, read_at_ms
             FROM cloud_agent_team_messages
             WHERE tenant_id = $1 AND user_id = $2 AND team_id = $3
               AND (from_member_id = $4 OR to_member_id = $4)
             ORDER BY created_at_ms, message_id",
        )
        .bind(tenant_id.as_str())
        .bind(user_id.as_str())
        .bind(scope.team_id.as_str())
        .bind(scope.current_member_id.as_str())
        .fetch_all(&mut *transaction)
        .await
        .map_err(database_error)?;
        let messages = rows
            .iter()
            .map(decode_message)
            .collect::<Result<Vec<_>, _>>()?;
        let snapshot = AgentTeamSnapshot {
            team_id: scope.team_id,
            current_member_id: scope.current_member_id,
            members: scope.members,
            tasks,
            messages,
        };
        snapshot.validate()?;
        transaction.commit().await.map_err(database_error)?;
        Ok(snapshot)
    }

    pub async fn create_agent_team_task(
        &self,
        tenant_id: &TenantId,
        actor_id: &UserId,
        session_id: &SessionId,
        request: AgentTeamTaskCreate,
        now_ms: u64,
    ) -> Result<AgentTeamTask, HarnessError> {
        let user_id = actor_id;
        validate_scope(tenant_id, user_id, session_id)?;
        request.validate()?;
        let now = to_i64(now_ms, "cloud Agent Team task creation timestamp")?;
        let mut transaction = self.begin().await?;
        let owner_id = crate::sharing::session_owner_in(
            &mut transaction,
            tenant_id,
            actor_id,
            session_id,
            ternilo_control::ResourceAction::Submit,
        )
        .await?;
        let user_id = &owner_id;
        set_owner_scope(&mut transaction, tenant_id, user_id).await?;
        let scope = locked_team_scope(&mut transaction, tenant_id, user_id, session_id).await?;
        validate_owner(&scope, request.owner.as_ref())?;
        validate_dependencies_exist(
            &mut transaction,
            tenant_id,
            user_id,
            &scope.team_id,
            &request.dependencies,
        )
        .await?;
        let task_id = AgentTeamTaskId::new(format!(
            "task-{}",
            URL_SAFE_NO_PAD.encode(random::<[u8; 16]>())
        ));
        sqlx::query(
            "INSERT INTO cloud_agent_team_tasks
                (tenant_id, user_id, team_id, task_id, subject, description, status,
                 owner_member_id, revision, created_at_ms, updated_at_ms)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 1, $9, $9)",
        )
        .bind(tenant_id.as_str())
        .bind(user_id.as_str())
        .bind(scope.team_id.as_str())
        .bind(task_id.as_str())
        .bind(&request.subject)
        .bind(&request.description)
        .bind(status_as_str(request.status))
        .bind(request.owner.as_ref().map(AgentTeamMemberId::as_str))
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        insert_dependencies(
            &mut transaction,
            tenant_id,
            user_id,
            &scope.team_id,
            &task_id,
            &request.dependencies,
        )
        .await?;
        crate::sharing::audit_session_in(
            &mut transaction,
            tenant_id,
            actor_id,
            session_id,
            user_id,
            ternilo_control::ResourceAction::Submit,
            now_ms,
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(AgentTeamTask {
            id: task_id,
            subject: request.subject,
            description: request.description,
            status: request.status,
            dependencies: request.dependencies,
            owner: request.owner,
            revision: 1,
            created_at_ms: now_ms,
            updated_at_ms: now_ms,
        })
    }

    #[expect(
        clippy::too_many_lines,
        reason = "Keep authorization, canonical state and audit changes in one atomic operation."
    )]
    pub async fn replace_agent_team_task(
        &self,
        tenant_id: &TenantId,
        actor_id: &UserId,
        session_id: &SessionId,
        task_id: &AgentTeamTaskId,
        request: AgentTeamTaskReplace,
        now_ms: u64,
    ) -> Result<AgentTeamTask, HarnessError> {
        let user_id = actor_id;
        validate_scope(tenant_id, user_id, session_id)?;
        task_id.validate()?;
        request.validate()?;
        if request
            .dependencies
            .iter()
            .any(|dependency| dependency == task_id)
        {
            return Err(HarnessError::invalid(
                "Agent Team task cannot depend on itself",
            ));
        }
        let now = to_i64(now_ms, "cloud Agent Team task update timestamp")?;
        let mut transaction = self.begin().await?;
        let owner_id = crate::sharing::session_owner_in(
            &mut transaction,
            tenant_id,
            actor_id,
            session_id,
            ternilo_control::ResourceAction::Submit,
        )
        .await?;
        let user_id = &owner_id;
        set_owner_scope(&mut transaction, tenant_id, user_id).await?;
        let scope = locked_team_scope(&mut transaction, tenant_id, user_id, session_id).await?;
        validate_owner(&scope, request.owner.as_ref())?;
        let row = sqlx::query(ternilo_storage::for_update(
            &transaction,
            "SELECT revision, created_at_ms
             FROM cloud_agent_team_tasks
             WHERE tenant_id = $1 AND user_id = $2 AND team_id = $3 AND task_id = $4",
            "SELECT revision, created_at_ms
             FROM cloud_agent_team_tasks
             WHERE tenant_id = $1 AND user_id = $2 AND team_id = $3 AND task_id = $4
             FOR UPDATE",
        ))
        .bind(tenant_id.as_str())
        .bind(user_id.as_str())
        .bind(scope.team_id.as_str())
        .bind(task_id.as_str())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?
        .ok_or_else(|| HarnessError::invalid("unknown Agent Team task"))?;
        let current_revision = from_i64(
            row.try_get("revision").map_err(database_error)?,
            "cloud Agent Team task revision",
        )?;
        if current_revision != request.expected_revision {
            return Err(HarnessError::conflict(format!(
                "Agent Team task revision is {current_revision}; expected {}",
                request.expected_revision
            )));
        }
        let created_at_ms = from_i64(
            row.try_get("created_at_ms").map_err(database_error)?,
            "cloud Agent Team task creation timestamp",
        )?;
        validate_dependencies_exist(
            &mut transaction,
            tenant_id,
            user_id,
            &scope.team_id,
            &request.dependencies,
        )
        .await?;
        let mut graph =
            load_dependency_graph(&mut transaction, tenant_id, user_id, &scope.team_id).await?;
        graph.insert(task_id.clone(), request.dependencies.clone());
        validate_acyclic(&graph)?;
        let revision = current_revision
            .checked_add(1)
            .ok_or_else(|| HarnessError::conflict("Agent Team task revision is exhausted"))?;
        let changed = sqlx::query(
            "UPDATE cloud_agent_team_tasks
             SET subject = $5, description = $6, status = $7, owner_member_id = $8,
                 revision = $9, updated_at_ms = $10
             WHERE tenant_id = $1 AND user_id = $2 AND team_id = $3 AND task_id = $4
               AND revision = $11",
        )
        .bind(tenant_id.as_str())
        .bind(user_id.as_str())
        .bind(scope.team_id.as_str())
        .bind(task_id.as_str())
        .bind(&request.subject)
        .bind(&request.description)
        .bind(status_as_str(request.status))
        .bind(request.owner.as_ref().map(AgentTeamMemberId::as_str))
        .bind(to_i64(revision, "cloud Agent Team task revision")?)
        .bind(now)
        .bind(to_i64(
            request.expected_revision,
            "cloud Agent Team expected task revision",
        )?)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?
        .rows_affected();
        if changed != 1 {
            return Err(HarnessError::conflict(
                "Agent Team task changed while it was being replaced",
            ));
        }
        sqlx::query(
            "DELETE FROM cloud_agent_team_task_dependencies
             WHERE tenant_id = $1 AND user_id = $2 AND team_id = $3 AND task_id = $4",
        )
        .bind(tenant_id.as_str())
        .bind(user_id.as_str())
        .bind(scope.team_id.as_str())
        .bind(task_id.as_str())
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        insert_dependencies(
            &mut transaction,
            tenant_id,
            user_id,
            &scope.team_id,
            task_id,
            &request.dependencies,
        )
        .await?;
        crate::sharing::audit_session_in(
            &mut transaction,
            tenant_id,
            actor_id,
            session_id,
            user_id,
            ternilo_control::ResourceAction::Submit,
            now_ms,
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(AgentTeamTask {
            id: task_id.clone(),
            subject: request.subject,
            description: request.description,
            status: request.status,
            dependencies: request.dependencies,
            owner: request.owner,
            revision,
            created_at_ms,
            updated_at_ms: now_ms,
        })
    }

    #[expect(
        clippy::too_many_lines,
        reason = "Validate team ownership, dependencies and revision before the audited deletion commits."
    )]
    pub async fn delete_agent_team_task(
        &self,
        tenant_id: &TenantId,
        actor_id: &UserId,
        session_id: &SessionId,
        task_id: &AgentTeamTaskId,
        expected_revision: u64,
    ) -> Result<(), HarnessError> {
        let user_id = actor_id;
        validate_scope(tenant_id, user_id, session_id)?;
        task_id.validate()?;
        if expected_revision == 0 {
            return Err(HarnessError::invalid(
                "Agent Team task expected revision must be positive",
            ));
        }
        let now_ms = from_i64(
            chrono::Utc::now().timestamp_millis(),
            "cloud task deletion timestamp",
        )?;
        let mut transaction = self.begin().await?;
        let owner_id = crate::sharing::session_owner_in(
            &mut transaction,
            tenant_id,
            actor_id,
            session_id,
            ternilo_control::ResourceAction::Submit,
        )
        .await?;
        let user_id = &owner_id;
        set_owner_scope(&mut transaction, tenant_id, user_id).await?;
        let scope = locked_team_scope(&mut transaction, tenant_id, user_id, session_id).await?;
        let revision = sqlx::query_scalar::<_, i64>(ternilo_storage::for_update(
            &transaction,
            "SELECT revision
             FROM cloud_agent_team_tasks
             WHERE tenant_id = $1 AND user_id = $2 AND team_id = $3 AND task_id = $4",
            "SELECT revision
             FROM cloud_agent_team_tasks
             WHERE tenant_id = $1 AND user_id = $2 AND team_id = $3 AND task_id = $4
             FOR UPDATE",
        ))
        .bind(tenant_id.as_str())
        .bind(user_id.as_str())
        .bind(scope.team_id.as_str())
        .bind(task_id.as_str())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?
        .ok_or_else(|| HarnessError::invalid("unknown Agent Team task"))?;
        let revision = from_i64(revision, "cloud Agent Team task revision")?;
        if revision != expected_revision {
            return Err(HarnessError::conflict(format!(
                "Agent Team task revision is {revision}; expected {expected_revision}"
            )));
        }
        let referenced = sqlx::query_scalar::<_, i64>(
            "SELECT CAST(EXISTS(
                SELECT 1 FROM cloud_agent_team_task_dependencies
                WHERE tenant_id = $1 AND user_id = $2 AND team_id = $3
                  AND dependency_task_id = $4
             ) AS INTEGER)",
        )
        .bind(tenant_id.as_str())
        .bind(user_id.as_str())
        .bind(scope.team_id.as_str())
        .bind(task_id.as_str())
        .fetch_one(&mut *transaction)
        .await
        .map_err(database_error)?
            != 0;
        if referenced {
            return Err(HarnessError::conflict(
                "Agent Team task is still required by another task",
            ));
        }
        let changed = sqlx::query(
            "DELETE FROM cloud_agent_team_tasks
             WHERE tenant_id = $1 AND user_id = $2 AND team_id = $3 AND task_id = $4
               AND revision = $5",
        )
        .bind(tenant_id.as_str())
        .bind(user_id.as_str())
        .bind(scope.team_id.as_str())
        .bind(task_id.as_str())
        .bind(to_i64(
            expected_revision,
            "cloud Agent Team expected task revision",
        )?)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?
        .rows_affected();
        if changed != 1 {
            return Err(HarnessError::conflict(
                "Agent Team task changed while it was being deleted",
            ));
        }
        crate::sharing::audit_session_in(
            &mut transaction,
            tenant_id,
            actor_id,
            session_id,
            user_id,
            ternilo_control::ResourceAction::Submit,
            now_ms,
        )
        .await?;
        transaction.commit().await.map_err(database_error)
    }

    pub async fn send_agent_team_message(
        &self,
        tenant_id: &TenantId,
        actor_id: &UserId,
        session_id: &SessionId,
        request: AgentTeamMessageSend,
        now_ms: u64,
    ) -> Result<AgentTeamMessage, HarnessError> {
        let user_id = actor_id;
        validate_scope(tenant_id, user_id, session_id)?;
        request.validate()?;
        let now = to_i64(now_ms, "cloud Agent Team message timestamp")?;
        let mut transaction = self.begin().await?;
        let owner_id = crate::sharing::session_owner_in(
            &mut transaction,
            tenant_id,
            actor_id,
            session_id,
            ternilo_control::ResourceAction::Submit,
        )
        .await?;
        let user_id = &owner_id;
        set_owner_scope(&mut transaction, tenant_id, user_id).await?;
        let scope = locked_team_scope(&mut transaction, tenant_id, user_id, session_id).await?;
        if !scope.contains_member(&request.to) {
            return Err(HarnessError::invalid(
                "Agent Team message recipient is not a Team member",
            ));
        }
        let message_id = AgentTeamMessageId::new(format!(
            "message-{}",
            URL_SAFE_NO_PAD.encode(random::<[u8; 16]>())
        ));
        sqlx::query(
            "INSERT INTO cloud_agent_team_messages
                (tenant_id, user_id, team_id, message_id, from_member_id, to_member_id,
                 content, created_at_ms, read_at_ms)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, NULL)",
        )
        .bind(tenant_id.as_str())
        .bind(user_id.as_str())
        .bind(scope.team_id.as_str())
        .bind(message_id.as_str())
        .bind(scope.current_member_id.as_str())
        .bind(request.to.as_str())
        .bind(&request.content)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        crate::sharing::audit_session_in(
            &mut transaction,
            tenant_id,
            actor_id,
            session_id,
            user_id,
            ternilo_control::ResourceAction::Submit,
            now_ms,
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(AgentTeamMessage {
            id: message_id,
            from: scope.current_member_id,
            to: request.to,
            content: request.content,
            created_at_ms: now_ms,
            read_at_ms: None,
        })
    }

    pub async fn mark_agent_team_message_read(
        &self,
        tenant_id: &TenantId,
        actor_id: &UserId,
        session_id: &SessionId,
        message_id: &AgentTeamMessageId,
        now_ms: u64,
    ) -> Result<AgentTeamMessage, HarnessError> {
        let user_id = actor_id;
        validate_scope(tenant_id, user_id, session_id)?;
        message_id.validate()?;
        let now = to_i64(now_ms, "cloud Agent Team message read timestamp")?;
        let mut transaction = self.begin().await?;
        let owner_id = crate::sharing::session_owner_in(
            &mut transaction,
            tenant_id,
            actor_id,
            session_id,
            ternilo_control::ResourceAction::Submit,
        )
        .await?;
        let user_id = &owner_id;
        set_owner_scope(&mut transaction, tenant_id, user_id).await?;
        let scope = locked_team_scope(&mut transaction, tenant_id, user_id, session_id).await?;
        let row = sqlx::query(ternilo_storage::for_update(
            &transaction,
            "SELECT message_id, from_member_id, to_member_id, content,
                    created_at_ms, read_at_ms
             FROM cloud_agent_team_messages
             WHERE tenant_id = $1 AND user_id = $2 AND team_id = $3
               AND message_id = $4 AND to_member_id = $5",
            "SELECT message_id, from_member_id, to_member_id, content,
                    created_at_ms, read_at_ms
             FROM cloud_agent_team_messages
             WHERE tenant_id = $1 AND user_id = $2 AND team_id = $3
               AND message_id = $4 AND to_member_id = $5
             FOR UPDATE",
        ))
        .bind(tenant_id.as_str())
        .bind(user_id.as_str())
        .bind(scope.team_id.as_str())
        .bind(message_id.as_str())
        .bind(scope.current_member_id.as_str())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?
        .ok_or_else(|| HarnessError::invalid("unknown Agent Team inbox message"))?;
        let mut message = decode_message(&row)?;
        if message.read_at_ms.is_none() {
            sqlx::query(
                "UPDATE cloud_agent_team_messages
                 SET read_at_ms = $6
                 WHERE tenant_id = $1 AND user_id = $2 AND team_id = $3
                   AND message_id = $4 AND to_member_id = $5",
            )
            .bind(tenant_id.as_str())
            .bind(user_id.as_str())
            .bind(scope.team_id.as_str())
            .bind(message_id.as_str())
            .bind(scope.current_member_id.as_str())
            .bind(now)
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
            message.read_at_ms = Some(now_ms);
        }
        crate::sharing::audit_session_in(
            &mut transaction,
            tenant_id,
            actor_id,
            session_id,
            user_id,
            ternilo_control::ResourceAction::Submit,
            now_ms,
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(message)
    }
}

fn validate_scope(
    tenant_id: &TenantId,
    user_id: &UserId,
    session_id: &SessionId,
) -> Result<(), HarnessError> {
    tenant_id.validate()?;
    user_id.validate()?;
    session_id.validate()
}

async fn locked_team_scope(
    transaction: &mut ternilo_storage::Transaction,
    tenant_id: &TenantId,
    user_id: &UserId,
    session_id: &SessionId,
) -> Result<TeamScope, HarnessError> {
    let initial = load_team_scope(transaction, tenant_id, user_id, session_id, false).await?;
    let locked = sqlx::query_scalar::<_, String>(ternilo_storage::for_update(
        transaction,
        "SELECT session_id
         FROM cloud_sessions
         WHERE tenant_id = $1 AND user_id = $2 AND session_id = $3",
        "SELECT session_id
         FROM cloud_sessions
         WHERE tenant_id = $1 AND user_id = $2 AND session_id = $3
         FOR UPDATE",
    ))
    .bind(tenant_id.as_str())
    .bind(user_id.as_str())
    .bind(initial.root_session_id.as_str())
    .fetch_optional(&mut **transaction)
    .await
    .map_err(database_error)?;
    if locked.is_none() {
        return Err(HarnessError::invalid(
            "cloud Agent Team Session does not exist for this owner",
        ));
    }
    let scope = load_team_scope(transaction, tenant_id, user_id, session_id, true).await?;
    if scope.root_session_id != initial.root_session_id {
        return Err(HarnessError::conflict(
            "cloud Agent Team ancestry changed during the operation",
        ));
    }
    Ok(scope)
}

async fn load_team_scope(
    transaction: &mut ternilo_storage::Transaction,
    tenant_id: &TenantId,
    user_id: &UserId,
    current_id: &SessionId,
    lock_sessions: bool,
) -> Result<TeamScope, HarnessError> {
    let query = if lock_sessions
        && ternilo_storage::backend(transaction) == ternilo_storage::Backend::Postgres
    {
        "SELECT session_id, parent_session_id, subagent_metadata, title
         FROM cloud_sessions
         WHERE tenant_id = $1 AND user_id = $2
         ORDER BY session_id
         FOR SHARE"
    } else {
        "SELECT session_id, parent_session_id, subagent_metadata, title
         FROM cloud_sessions
         WHERE tenant_id = $1 AND user_id = $2
         ORDER BY session_id"
    };
    let rows = sqlx::query(query)
        .bind(tenant_id.as_str())
        .bind(user_id.as_str())
        .fetch_all(&mut **transaction)
        .await
        .map_err(database_error)?;
    let sessions = rows
        .into_iter()
        .map(|row| {
            let subagent = row
                .try_get::<Option<ternilo_storage::Json<SubagentSessionMetadata>>, _>(
                    "subagent_metadata",
                )
                .map_err(database_error)?
                .map(|value| value.0);
            if let Some(metadata) = &subagent {
                metadata.subagent_id.validate()?;
                if metadata.provider.trim().is_empty() {
                    return Err(HarnessError::execution(
                        "cloud Subagent Session has an empty provider",
                    ));
                }
            }
            Ok(TeamSession {
                session_id: SessionId::new(
                    row.try_get::<String, _>("session_id")
                        .map_err(database_error)?,
                ),
                parent_session_id: row
                    .try_get::<Option<String>, _>("parent_session_id")
                    .map_err(database_error)?
                    .map(SessionId::new),
                subagent,
                title: row.try_get("title").map_err(database_error)?,
            })
        })
        .collect::<Result<Vec<_>, HarnessError>>()?;
    team_scope_from_sessions(&sessions, tenant_id, user_id, current_id)
}

fn team_scope_from_sessions(
    sessions: &[TeamSession],
    tenant_id: &TenantId,
    user_id: &UserId,
    current_id: &SessionId,
) -> Result<TeamScope, HarnessError> {
    let by_id = sessions
        .iter()
        .enumerate()
        .map(|(index, session)| (session.session_id.clone(), index))
        .collect::<BTreeMap<_, _>>();
    let mut root_index = *by_id.get(current_id).ok_or_else(|| {
        HarnessError::invalid("cloud Agent Team Session does not exist for this owner")
    })?;
    let mut ancestors = BTreeSet::new();
    while sessions[root_index].subagent.is_some() {
        if !ancestors.insert(sessions[root_index].session_id.clone()) {
            return Err(HarnessError::execution(
                "cyclic cloud Subagent Session ancestry",
            ));
        }
        let parent_id = sessions[root_index]
            .parent_session_id
            .as_ref()
            .ok_or_else(|| {
                HarnessError::execution("cloud Subagent Session has no persisted parent")
            })?;
        root_index = *by_id.get(parent_id).ok_or_else(|| {
            HarnessError::execution("cloud Subagent Session parent is not owner-scoped")
        })?;
    }
    let root = &sessions[root_index];
    let team_id = AgentTeamId::new(format!(
        "team-{}",
        digest_id(&[
            tenant_id.as_str(),
            user_id.as_str(),
            root.session_id.as_str(),
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
        .remove(current_id)
        .ok_or_else(|| HarnessError::execution("current cloud Session is not in its Agent Team"))?;
    Ok(TeamScope {
        team_id,
        root_session_id: root.session_id.clone(),
        current_member_id,
        members,
    })
}

fn collect_members(
    sessions: &[TeamSession],
    session: &TeamSession,
    team_id: &AgentTeamId,
    parent_member_id: Option<AgentTeamMemberId>,
    visited: &mut BTreeSet<SessionId>,
    members: &mut Vec<AgentTeamMember>,
    member_by_session: &mut BTreeMap<SessionId, AgentTeamMemberId>,
) -> Result<(), HarnessError> {
    if !visited.insert(session.session_id.clone()) {
        return Err(HarnessError::execution(
            "cyclic cloud Agent Team Session tree",
        ));
    }
    let member_id = AgentTeamMemberId::new(format!(
        "member-{}",
        digest_id(&[team_id.as_str(), session.session_id.as_str()])
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
    member_by_session.insert(session.session_id.clone(), member_id.clone());
    members.push(member);
    let mut children = sessions
        .iter()
        .filter(|candidate| {
            candidate.subagent.is_some()
                && candidate.parent_session_id.as_ref() == Some(&session.session_id)
        })
        .collect::<Vec<_>>();
    children.sort_by(|left, right| left.session_id.cmp(&right.session_id));
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
    digest.finalize()[..16]
        .iter()
        .fold(String::with_capacity(32), |mut output, byte| {
            write!(output, "{byte:02x}").expect("writing to String cannot fail");
            output
        })
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

async fn load_tasks(
    transaction: &mut ternilo_storage::Transaction,
    tenant_id: &TenantId,
    user_id: &UserId,
    team_id: &AgentTeamId,
) -> Result<Vec<AgentTeamTask>, HarnessError> {
    let dependency_rows = sqlx::query(
        "SELECT task_id, dependency_task_id
         FROM cloud_agent_team_task_dependencies
         WHERE tenant_id = $1 AND user_id = $2 AND team_id = $3
         ORDER BY task_id, dependency_task_id",
    )
    .bind(tenant_id.as_str())
    .bind(user_id.as_str())
    .bind(team_id.as_str())
    .fetch_all(&mut **transaction)
    .await
    .map_err(database_error)?;
    let mut dependencies = BTreeMap::<AgentTeamTaskId, Vec<AgentTeamTaskId>>::new();
    for row in dependency_rows {
        let task = AgentTeamTaskId::new(
            row.try_get::<String, _>("task_id")
                .map_err(database_error)?,
        );
        let dependency = AgentTeamTaskId::new(
            row.try_get::<String, _>("dependency_task_id")
                .map_err(database_error)?,
        );
        dependencies.entry(task).or_default().push(dependency);
    }
    let rows = sqlx::query(
        "SELECT task_id, subject, description, status, owner_member_id,
                revision, created_at_ms, updated_at_ms
         FROM cloud_agent_team_tasks
         WHERE tenant_id = $1 AND user_id = $2 AND team_id = $3
         ORDER BY created_at_ms, task_id",
    )
    .bind(tenant_id.as_str())
    .bind(user_id.as_str())
    .bind(team_id.as_str())
    .fetch_all(&mut **transaction)
    .await
    .map_err(database_error)?;
    rows.iter()
        .map(|row| {
            let id = AgentTeamTaskId::new(
                row.try_get::<String, _>("task_id")
                    .map_err(database_error)?,
            );
            Ok(AgentTeamTask {
                dependencies: dependencies.remove(&id).unwrap_or_default(),
                id,
                subject: row.try_get("subject").map_err(database_error)?,
                description: row.try_get("description").map_err(database_error)?,
                status: status_from_str(
                    &row.try_get::<String, _>("status").map_err(database_error)?,
                )?,
                owner: row
                    .try_get::<Option<String>, _>("owner_member_id")
                    .map_err(database_error)?
                    .map(AgentTeamMemberId::new),
                revision: from_i64(
                    row.try_get("revision").map_err(database_error)?,
                    "cloud Agent Team task revision",
                )?,
                created_at_ms: from_i64(
                    row.try_get("created_at_ms").map_err(database_error)?,
                    "cloud Agent Team task creation timestamp",
                )?,
                updated_at_ms: from_i64(
                    row.try_get("updated_at_ms").map_err(database_error)?,
                    "cloud Agent Team task update timestamp",
                )?,
            })
        })
        .collect()
}

fn decode_message(row: &sqlx::any::AnyRow) -> Result<AgentTeamMessage, HarnessError> {
    Ok(AgentTeamMessage {
        id: AgentTeamMessageId::new(
            row.try_get::<String, _>("message_id")
                .map_err(database_error)?,
        ),
        from: AgentTeamMemberId::new(
            row.try_get::<String, _>("from_member_id")
                .map_err(database_error)?,
        ),
        to: AgentTeamMemberId::new(
            row.try_get::<String, _>("to_member_id")
                .map_err(database_error)?,
        ),
        content: row.try_get("content").map_err(database_error)?,
        created_at_ms: from_i64(
            row.try_get("created_at_ms").map_err(database_error)?,
            "cloud Agent Team message timestamp",
        )?,
        read_at_ms: row
            .try_get::<Option<i64>, _>("read_at_ms")
            .map_err(database_error)?
            .map(|value| from_i64(value, "cloud Agent Team message read timestamp"))
            .transpose()?,
    })
}

async fn validate_dependencies_exist(
    transaction: &mut ternilo_storage::Transaction,
    tenant_id: &TenantId,
    user_id: &UserId,
    team_id: &AgentTeamId,
    dependencies: &[AgentTeamTaskId],
) -> Result<(), HarnessError> {
    for dependency in dependencies {
        let exists = sqlx::query_scalar::<_, i64>(
            "SELECT CAST(EXISTS(
                SELECT 1 FROM cloud_agent_team_tasks
                WHERE tenant_id = $1 AND user_id = $2 AND team_id = $3 AND task_id = $4
             ) AS INTEGER)",
        )
        .bind(tenant_id.as_str())
        .bind(user_id.as_str())
        .bind(team_id.as_str())
        .bind(dependency.as_str())
        .fetch_one(&mut **transaction)
        .await
        .map_err(database_error)?
            != 0;
        if !exists {
            return Err(HarnessError::invalid(format!(
                "unknown Agent Team task dependency {}",
                dependency.as_str()
            )));
        }
    }
    Ok(())
}

async fn insert_dependencies(
    transaction: &mut ternilo_storage::Transaction,
    tenant_id: &TenantId,
    user_id: &UserId,
    team_id: &AgentTeamId,
    task_id: &AgentTeamTaskId,
    dependencies: &[AgentTeamTaskId],
) -> Result<(), HarnessError> {
    for dependency in dependencies {
        sqlx::query(
            "INSERT INTO cloud_agent_team_task_dependencies
                (tenant_id, user_id, team_id, task_id, dependency_task_id)
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(tenant_id.as_str())
        .bind(user_id.as_str())
        .bind(team_id.as_str())
        .bind(task_id.as_str())
        .bind(dependency.as_str())
        .execute(&mut **transaction)
        .await
        .map_err(database_error)?;
    }
    Ok(())
}

async fn load_dependency_graph(
    transaction: &mut ternilo_storage::Transaction,
    tenant_id: &TenantId,
    user_id: &UserId,
    team_id: &AgentTeamId,
) -> Result<BTreeMap<AgentTeamTaskId, Vec<AgentTeamTaskId>>, HarnessError> {
    let task_rows = sqlx::query_scalar::<_, String>(
        "SELECT task_id FROM cloud_agent_team_tasks
         WHERE tenant_id = $1 AND user_id = $2 AND team_id = $3",
    )
    .bind(tenant_id.as_str())
    .bind(user_id.as_str())
    .bind(team_id.as_str())
    .fetch_all(&mut **transaction)
    .await
    .map_err(database_error)?;
    let mut graph = task_rows
        .into_iter()
        .map(|task| (AgentTeamTaskId::new(task), Vec::new()))
        .collect::<BTreeMap<_, _>>();
    let rows = sqlx::query(
        "SELECT task_id, dependency_task_id
         FROM cloud_agent_team_task_dependencies
         WHERE tenant_id = $1 AND user_id = $2 AND team_id = $3",
    )
    .bind(tenant_id.as_str())
    .bind(user_id.as_str())
    .bind(team_id.as_str())
    .fetch_all(&mut **transaction)
    .await
    .map_err(database_error)?;
    for row in rows {
        let task = AgentTeamTaskId::new(
            row.try_get::<String, _>("task_id")
                .map_err(database_error)?,
        );
        let dependency = AgentTeamTaskId::new(
            row.try_get::<String, _>("dependency_task_id")
                .map_err(database_error)?,
        );
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
            "invalid Agent Team task status {status:?} in PostgreSQL"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use ternilo_protocol::{SubagentId, SubagentTranscriptKind};

    use super::*;

    fn root(id: &str) -> TeamSession {
        TeamSession {
            session_id: SessionId::new(id),
            parent_session_id: None,
            subagent: None,
            title: "Lead".to_owned(),
        }
    }

    fn child(id: &str, parent: &str, subagent_id: &str) -> TeamSession {
        TeamSession {
            session_id: SessionId::new(id),
            parent_session_id: Some(SessionId::new(parent)),
            subagent: Some(SubagentSessionMetadata {
                subagent_id: SubagentId::new(subagent_id),
                provider: "in-process".to_owned(),
                transcript_kind: SubagentTranscriptKind::Conversation,
            }),
            title: subagent_id.to_owned(),
        }
    }

    #[test]
    fn canonical_descendants_share_opaque_ids_and_ordinary_forks_are_isolated() {
        let root_session = root("internal-root-session");
        let child_session = child(
            "internal-child-session",
            "internal-root-session",
            "researcher",
        );
        let grandchild = child(
            "internal-grandchild-session",
            "internal-child-session",
            "reviewer",
        );
        let mut ordinary_fork = root("internal-fork-session");
        ordinary_fork.parent_session_id = Some(SessionId::new("internal-root-session"));
        let sessions = vec![root_session, child_session, grandchild, ordinary_fork];
        let tenant = TenantId::new("tenant");
        let user = UserId::new("user");

        let root_scope = team_scope_from_sessions(
            &sessions,
            &tenant,
            &user,
            &SessionId::new("internal-root-session"),
        )
        .unwrap();
        let child_scope = team_scope_from_sessions(
            &sessions,
            &tenant,
            &user,
            &SessionId::new("internal-child-session"),
        )
        .unwrap();
        let grandchild_scope = team_scope_from_sessions(
            &sessions,
            &tenant,
            &user,
            &SessionId::new("internal-grandchild-session"),
        )
        .unwrap();
        assert_eq!(root_scope.team_id, child_scope.team_id);
        assert_eq!(root_scope.team_id, grandchild_scope.team_id);
        assert_eq!(root_scope.members.len(), 3);
        assert!(
            root_scope
                .members
                .iter()
                .all(|member| !member.id.as_str().contains("internal"))
        );
        assert!(!root_scope.team_id.as_str().contains("internal"));

        let fork_scope = team_scope_from_sessions(
            &sessions,
            &tenant,
            &user,
            &SessionId::new("internal-fork-session"),
        )
        .unwrap();
        assert_ne!(root_scope.team_id, fork_scope.team_id);
        assert_eq!(fork_scope.members.len(), 1);
    }

    #[test]
    fn dependency_cycles_are_rejected() {
        let task_a = AgentTeamTaskId::new("task-a");
        let task_b = AgentTeamTaskId::new("task-b");
        let graph = BTreeMap::from([
            (task_a.clone(), vec![task_b.clone()]),
            (task_b, vec![task_a]),
        ]);
        assert!(validate_acyclic(&graph).is_err());
    }
}
