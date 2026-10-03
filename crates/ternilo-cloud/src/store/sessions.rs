use sqlx::Row;
use ternilo_protocol::{HarnessError, PermissionPreset, SessionId, TenantId, UserId, WorkspaceId};

use super::{CloudStore, database_error, json_error, mode_str, permission_str, set_tenant, to_i64};
use crate::{CloudSessionDraft, CloudSessionRecord, CloudSessionUpdate};

mod fork;
async fn require_authorized_model_binding(
    transaction: &mut ternilo_storage::Transaction,
    tenant_id: &TenantId,
    session_id: &SessionId,
    update: &CloudSessionUpdate,
    actor_id: &UserId,
) -> Result<(), HarnessError> {
    let Some(Some(snapshot)) = &update.model else {
        return Ok(());
    };
    crate::model_delegation::require_model_owner_in(
        transaction,
        tenant_id,
        ternilo_control::ResourceKind::Session,
        session_id.as_str(),
        &snapshot.binding,
    )
    .await?;
    if snapshot.binding.beneficiary_user_id() == actor_id {
        return Ok(());
    }
    let current = sqlx::query_scalar::<
        _,
        Option<ternilo_storage::Json<ternilo_protocol::RunModelSnapshot>>,
    >(ternilo_storage::for_update(
        transaction,
        "SELECT model_snapshot FROM cloud_sessions WHERE tenant_id=$1 AND session_id=$2",
        "SELECT model_snapshot FROM cloud_sessions WHERE tenant_id=$1 AND session_id=$2 FOR UPDATE",
    ))
    .bind(tenant_id.as_str())
    .bind(session_id.as_str())
    .fetch_one(&mut **transaction)
    .await
    .map_err(database_error)?;
    if !current.is_some_and(|current| current.0.binding == snapshot.binding) {
        return Err(HarnessError::policy(
            "only the model owner may authorize a new model source",
        ));
    }
    Ok(())
}

mod records;
pub(crate) use records::decode_session;
use records::select_session;

impl CloudStore {
    pub async fn create_session(
        &self,
        draft: CloudSessionDraft,
        tenant_id: &TenantId,
        actor_id: &UserId,
        now_ms: u64,
    ) -> Result<CloudSessionRecord, HarnessError> {
        draft.validate()?;
        tenant_id.validate()?;
        actor_id.validate()?;
        let session_id = draft
            .session_id
            .clone()
            .unwrap_or_else(crate::random_session_id);
        let now = to_i64(now_ms, "cloud session creation timestamp")?;
        let mut transaction = self.begin().await?;
        set_tenant(&mut transaction, tenant_id).await?;
        let owner_id = creation_workspace_owner_in(
            &mut transaction,
            tenant_id,
            actor_id,
            &draft.workspace_id,
            &draft.project_id,
        )
        .await?;
        let user_id = &owner_id;
        if let Some(snapshot) = &draft.model {
            crate::model_delegation::require_model_owner_in(
                &mut transaction,
                tenant_id,
                ternilo_control::ResourceKind::Workspace,
                draft.workspace_id.as_str(),
                &snapshot.binding,
            )
            .await?;
        }
        sqlx::query(
            "INSERT INTO cloud_sessions
                (tenant_id, session_id, user_id, project_id, workspace_id, agent_id,
                 title, permissions, model_snapshot, reserved_model_tokens,
                 agent_preset, profile_plugins, mode, state, created_at_ms, updated_at_ms)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, 'idle', $14, $14)",
        )
        .bind(tenant_id.as_str())
        .bind(session_id.as_str())
        .bind(user_id.as_str())
        .bind(&draft.project_id)
        .bind(draft.workspace_id.as_str())
        .bind(draft.agent_id.as_str())
        .bind(&draft.title)
        .bind(permission_str(draft.permissions))
        .bind(draft.model.as_ref().map(ternilo_storage::Json))
        .bind(to_i64(
            draft.reserved_model_tokens,
            "cloud session model token budget",
        )?)
        .bind(&draft.agent_preset)
        .bind(ternilo_storage::Json(&draft.profile_plugins))
        .bind(mode_str(draft.mode))
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        crate::execution_families::ensure_root_in(
            &mut transaction,
            tenant_id,
            &session_id,
            user_id,
            &draft.workspace_id,
            now_ms,
        )
        .await?;
        ternilo_storage::set_user_scope(&mut transaction, user_id).await?;
        crate::telemetry::sync_session_telemetry_in(
            &mut transaction,
            tenant_id,
            user_id,
            &session_id,
            now_ms,
        )
        .await?;
        let row = select_session(&mut transaction, tenant_id, &session_id).await?;
        crate::sharing::audit_session_in(
            &mut transaction,
            tenant_id,
            actor_id,
            &session_id,
            user_id,
            ternilo_control::ResourceAction::Submit,
            now_ms,
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;
        decode_session(&row)
    }

    pub async fn get_session(
        &self,
        tenant_id: &TenantId,
        session_id: &SessionId,
    ) -> Result<CloudSessionRecord, HarnessError> {
        session_id.validate()?;
        let mut transaction = self.begin().await?;
        set_tenant(&mut transaction, tenant_id).await?;
        let row = select_session(&mut transaction, tenant_id, session_id).await?;
        transaction.commit().await.map_err(database_error)?;
        decode_session(&row)
    }

    /// Owner-scoped lookup used by the unified workbench placement resolver.
    /// `None` deliberately covers both a missing identifier and a Session owned
    /// by another user so callers cannot use the resolver as an oracle.
    pub async fn find_owned_session(
        &self,
        tenant_id: &TenantId,
        user_id: &UserId,
        session_id: &SessionId,
    ) -> Result<Option<CloudSessionRecord>, HarnessError> {
        tenant_id.validate()?;
        user_id.validate()?;
        session_id.validate()?;
        let mut transaction = self.begin().await?;
        set_tenant(&mut transaction, tenant_id).await?;
        let row = sqlx::query(
            "SELECT tenant_id, session_id, user_id, project_id, workspace_id, agent_id,
                    parent_session_id, subagent_metadata, title, archived_at_ms, state, permissions,
                    model_snapshot,
                    reserved_model_tokens,
                    agent_preset, profile_plugins, mode, execution, last_seq, created_at_ms, updated_at_ms
             FROM cloud_sessions
             WHERE tenant_id = $1 AND session_id = $2 AND user_id = $3
               AND archived_at_ms IS NULL",
        )
        .bind(tenant_id.as_str())
        .bind(session_id.as_str())
        .bind(user_id.as_str())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?;
        transaction.commit().await.map_err(database_error)?;
        row.as_ref().map(decode_session).transpose()
    }

    pub async fn update_session(
        &self,
        tenant_id: &TenantId,
        session_id: &SessionId,
        actor_id: &UserId,
        update: CloudSessionUpdate,
        now_ms: u64,
    ) -> Result<CloudSessionRecord, HarnessError> {
        let user_id = actor_id;
        session_id.validate()?;
        user_id.validate()?;
        validate_session_update(&update)?;
        let profile_plugins = update
            .profile_plugins
            .as_ref()
            .map(serde_json::to_value)
            .transpose()
            .map_err(json_error)?;
        let now = to_i64(now_ms, "cloud session update timestamp")?;
        let mut transaction = self.begin().await?;
        let owner_id = crate::sharing::session_owner_in(
            &mut transaction,
            tenant_id,
            actor_id,
            session_id,
            ternilo_control::ResourceAction::Configure,
        )
        .await?;
        let user_id = &owner_id;
        set_tenant(&mut transaction, tenant_id).await?;
        require_authorized_model_binding(
            &mut transaction,
            tenant_id,
            session_id,
            &update,
            actor_id,
        )
        .await?;
        if update.agent_preset.is_some() {
            require_unstarted_preset(&mut transaction, tenant_id, session_id).await?;
        }
        let changed = sqlx::query(
            "UPDATE cloud_sessions
             SET title = COALESCE($4, title),
                 permissions = COALESCE($5, permissions),
                 model_snapshot = CASE WHEN $6 != 0 THEN $7 ELSE model_snapshot END,
                 reserved_model_tokens = COALESCE($8, reserved_model_tokens),
                 agent_preset = COALESCE($9, agent_preset),
                 profile_plugins = COALESCE($10, profile_plugins),
                 mode = COALESCE($11, mode),
                 updated_at_ms = $12
             WHERE tenant_id = $1 AND session_id = $2 AND user_id = $3",
        )
        .bind(tenant_id.as_str())
        .bind(session_id.as_str())
        .bind(user_id.as_str())
        .bind(update.title.as_deref())
        .bind(update.permissions.map(permission_str))
        .bind(i64::from(update.model.is_some()))
        .bind(
            update
                .model
                .as_ref()
                .and_then(Option::as_ref)
                .map(ternilo_storage::Json),
        )
        .bind(
            update
                .reserved_model_tokens
                .map(|value| to_i64(value, "cloud session model token budget"))
                .transpose()?,
        )
        .bind(update.agent_preset.as_deref())
        .bind(profile_plugins.map(ternilo_storage::Json))
        .bind(update.mode.map(mode_str))
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?
        .rows_affected();
        if changed != 1 {
            return Err(HarnessError::invalid("cloud session does not exist"));
        }
        ternilo_storage::set_user_scope(&mut transaction, user_id).await?;
        crate::telemetry::sync_session_telemetry_in(
            &mut transaction,
            tenant_id,
            user_id,
            session_id,
            now_ms,
        )
        .await?;
        let row = select_session(&mut transaction, tenant_id, session_id).await?;
        crate::sharing::audit_session_in(
            &mut transaction,
            tenant_id,
            actor_id,
            session_id,
            user_id,
            ternilo_control::ResourceAction::Configure,
            now_ms,
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;
        decode_session(&row)
    }

    pub async fn list_sessions(
        &self,
        tenant_id: &TenantId,
        limit: u32,
    ) -> Result<Vec<CloudSessionRecord>, HarnessError> {
        if limit == 0 || limit > 500 {
            return Err(HarnessError::invalid(
                "cloud session list limit must be between 1 and 500",
            ));
        }
        let mut transaction = self.begin().await?;
        set_tenant(&mut transaction, tenant_id).await?;
        let rows = sqlx::query(
            "SELECT session.tenant_id, session.session_id, session.user_id,
                    session.project_id, session.workspace_id, session.parent_session_id,
                    session.subagent_metadata,
                    session.agent_id, session.archived_at_ms, session.state,
                    session.permissions, session.model_snapshot, session.reserved_model_tokens, session.agent_preset,
                    session.profile_plugins, session.mode, session.execution, session.last_seq,
                    session.created_at_ms, session.updated_at_ms,
                    session.title
             FROM cloud_sessions AS session
             WHERE session.tenant_id = $1 AND session.archived_at_ms IS NULL
             ORDER BY session.updated_at_ms DESC, session.session_id
             LIMIT $2",
        )
        .bind(tenant_id.as_str())
        .bind(i64::from(limit))
        .fetch_all(&mut *transaction)
        .await
        .map_err(database_error)?;
        transaction.commit().await.map_err(database_error)?;
        rows.iter().map(decode_session).collect()
    }

    pub async fn archive_session(
        &self,
        tenant_id: &TenantId,
        actor_id: &UserId,
        session_id: &SessionId,
        now_ms: u64,
    ) -> Result<CloudSessionRecord, HarnessError> {
        let now = to_i64(now_ms, "cloud session archive timestamp")?;
        let mut transaction = self.begin().await?;
        let owner_id = crate::sharing::session_owner_in(
            &mut transaction,
            tenant_id,
            actor_id,
            session_id,
            ternilo_control::ResourceAction::Delete,
        )
        .await?;
        let user_id = &owner_id;
        set_tenant(&mut transaction, tenant_id).await?;
        let row = sqlx::query(
            "UPDATE cloud_sessions
             SET archived_at_ms = $4, updated_at_ms = $4
             WHERE tenant_id = $1 AND session_id = $2 AND user_id = $3
               AND archived_at_ms IS NULL
             RETURNING tenant_id, session_id, user_id, project_id, workspace_id, agent_id,
                       parent_session_id, subagent_metadata, title, archived_at_ms, state, permissions,
                       model_snapshot, reserved_model_tokens,
                       agent_preset, profile_plugins, mode, execution, last_seq, created_at_ms, updated_at_ms",
        )
        .bind(tenant_id.as_str())
        .bind(session_id.as_str())
        .bind(user_id.as_str())
        .bind(now)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?
        .ok_or_else(|| HarnessError::invalid("cloud session does not exist"))?;
        crate::sharing::audit_session_in(
            &mut transaction,
            tenant_id,
            actor_id,
            session_id,
            user_id,
            ternilo_control::ResourceAction::Delete,
            now_ms,
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;
        decode_session(&row)
    }

    pub async fn restore_session(
        &self,
        tenant_id: &TenantId,
        actor_id: &UserId,
        session_id: &SessionId,
        now_ms: u64,
    ) -> Result<CloudSessionRecord, HarnessError> {
        let mut transaction = self.begin().await?;
        let owner_id = crate::sharing::session_owner_in(
            &mut transaction,
            tenant_id,
            actor_id,
            session_id,
            ternilo_control::ResourceAction::Delete,
        )
        .await?;
        set_tenant(&mut transaction, tenant_id).await?;
        let changed = sqlx::query(
            "UPDATE cloud_sessions SET archived_at_ms = NULL
             WHERE tenant_id = $1 AND session_id = $2 AND user_id = $3
               AND archived_at_ms IS NOT NULL",
        )
        .bind(tenant_id.as_str())
        .bind(session_id.as_str())
        .bind(owner_id.as_str())
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?
        .rows_affected();
        let row = select_session(&mut transaction, tenant_id, session_id).await?;
        if changed > 0 {
            crate::sharing::audit_session_in(
                &mut transaction,
                tenant_id,
                actor_id,
                session_id,
                &owner_id,
                ternilo_control::ResourceAction::Delete,
                now_ms,
            )
            .await?;
        }
        transaction.commit().await.map_err(database_error)?;
        decode_session(&row)
    }

    pub async fn delete_session(
        &self,
        tenant_id: &TenantId,
        session_id: &SessionId,
        actor_id: &UserId,
        now_ms: u64,
    ) -> Result<(), HarnessError> {
        let user_id = actor_id;
        session_id.validate()?;
        user_id.validate()?;
        let mut transaction = self.begin().await?;
        let owner_id = crate::sharing::session_owner_in(
            &mut transaction,
            tenant_id,
            actor_id,
            session_id,
            ternilo_control::ResourceAction::Delete,
        )
        .await?;
        let user_id = &owner_id;
        set_tenant(&mut transaction, tenant_id).await?;
        let active = sqlx::query_scalar::<_, i64>(
            "SELECT CAST(EXISTS(
                SELECT 1 FROM cloud_runs
                WHERE tenant_id = $1 AND session_id = $2
                  AND state IN ('queued', 'leased', 'running', 'cancel_requested')
             ) AS INTEGER)",
        )
        .bind(tenant_id.as_str())
        .bind(session_id.as_str())
        .fetch_one(&mut *transaction)
        .await
        .map_err(database_error)?
            != 0;
        if active {
            return Err(HarnessError::policy(
                "a cloud session with an active run cannot be deleted",
            ));
        }
        let deleted = sqlx::query(
            "DELETE FROM cloud_sessions
             WHERE tenant_id = $1 AND session_id = $2 AND user_id = $3",
        )
        .bind(tenant_id.as_str())
        .bind(session_id.as_str())
        .bind(user_id.as_str())
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?
        .rows_affected();
        if deleted != 1 {
            return Err(HarnessError::invalid("cloud session does not exist"));
        }
        sqlx::query("DELETE FROM control_resource_shares WHERE tenant_id=$1 AND resource_kind='session' AND resource_id=$2")
            .bind(tenant_id.as_str()).bind(session_id.as_str())
            .execute(&mut *transaction).await.map_err(database_error)?;
        sqlx::query("DELETE FROM control_resource_group_shares WHERE tenant_id=$1 AND resource_kind='session' AND resource_id=$2")
            .bind(tenant_id.as_str()).bind(session_id.as_str())
            .execute(&mut *transaction).await.map_err(database_error)?;
        sqlx::query(
            "DELETE FROM control_resource_fork_group_sources WHERE tenant_id=$1 AND session_id=$2",
        )
        .bind(tenant_id.as_str())
        .bind(session_id.as_str())
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        sqlx::query("DELETE FROM cloud_runs WHERE tenant_id = $1 AND session_id = $2")
            .bind(tenant_id.as_str())
            .bind(session_id.as_str())
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
        crate::sharing::audit_session_in(
            &mut transaction,
            tenant_id,
            actor_id,
            session_id,
            user_id,
            ternilo_control::ResourceAction::Delete,
            now_ms,
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(())
    }
}

fn require_text(value: &str, label: &str, maximum: usize) -> Result<(), HarnessError> {
    if value.trim().is_empty() || value.len() > maximum || value.chars().any(char::is_control) {
        Err(HarnessError::invalid(format!(
            "{label} must contain 1 to {maximum} bytes without control characters"
        )))
    } else {
        Ok(())
    }
}

async fn creation_workspace_owner_in(
    transaction: &mut ternilo_storage::Transaction,
    tenant_id: &TenantId,
    actor_id: &UserId,
    workspace_id: &WorkspaceId,
    project_id: &str,
) -> Result<UserId, HarnessError> {
    let access = ternilo_control::resource_access_in(
        transaction,
        actor_id,
        tenant_id,
        ternilo_control::ResourceKind::Workspace,
        workspace_id.as_str(),
    )
    .await?;
    access.require(ternilo_control::ResourceAction::Submit)?;
    let user_id = &access.storage_user_id;
    let workspace = sqlx::query(
        "SELECT project_id, owner_user_id, placement
         FROM control_workspaces
         WHERE tenant_id = $1 AND workspace_id = $2
           AND unregistered_at_ms IS NULL",
    )
    .bind(tenant_id.as_str())
    .bind(workspace_id.as_str())
    .fetch_optional(&mut **transaction)
    .await
    .map_err(database_error)?
    .ok_or_else(|| HarnessError::invalid("cloud session workspace does not exist"))?;
    let workspace_project: String = workspace.try_get("project_id").map_err(database_error)?;
    let workspace_owner: String = workspace.try_get("owner_user_id").map_err(database_error)?;
    let placement: String = workspace.try_get("placement").map_err(database_error)?;
    if workspace_project != project_id
        || workspace_owner != user_id.as_str()
        || placement != "cloud"
    {
        return Err(HarnessError::policy(
            "cloud session workspace must belong to the user and requested project",
        ));
    }
    Ok(access.storage_user_id)
}

async fn require_unstarted_preset(
    transaction: &mut ternilo_storage::Transaction,
    tenant_id: &TenantId,
    session_id: &SessionId,
) -> Result<(), HarnessError> {
    let session = sqlx::query(ternilo_storage::for_update(
        transaction,
        "SELECT state, last_seq, parent_session_id FROM cloud_sessions WHERE tenant_id=$1 AND session_id=$2",
        "SELECT state, last_seq, parent_session_id FROM cloud_sessions WHERE tenant_id=$1 AND session_id=$2 FOR UPDATE",
    ))
    .bind(tenant_id.as_str())
    .bind(session_id.as_str())
    .fetch_one(&mut **transaction)
    .await
    .map_err(database_error)?;
    let runs: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM cloud_runs WHERE tenant_id=$1 AND session_id=$2")
            .bind(tenant_id.as_str())
            .bind(session_id.as_str())
            .fetch_one(&mut **transaction)
            .await
            .map_err(database_error)?;
    if session.get::<String, _>("state") != "idle"
        || session.get::<i64, _>("last_seq") >= 0
        || session
            .get::<Option<String>, _>("parent_session_id")
            .is_some()
        || runs > 0
    {
        return Err(HarnessError::conflict(
            "agent preset is locked after the first accepted task; create a new session to use another preset",
        ));
    }
    Ok(())
}

fn validate_session_update(update: &CloudSessionUpdate) -> Result<(), HarnessError> {
    if let Some(title) = update.title.as_deref() {
        require_text(title, "cloud session title", 256)?;
    }
    if update.permissions == Some(PermissionPreset::FullAccess) {
        return Err(HarnessError::policy(
            "cloud sessions cannot grant full host access",
        ));
    }
    if let Some(Some(model)) = &update.model {
        model.validate()?;
    }
    if let Some(tokens) = update.reserved_model_tokens
        && tokens == 0
    {
        return Err(HarnessError::invalid(
            "cloud session model token budget must be positive",
        ));
    }
    if let Some(preset) = update.agent_preset.as_deref() {
        require_text(preset, "cloud session Agent preset", 128)?;
    }
    if update
        .profile_plugins
        .as_ref()
        .is_some_and(|plugins| plugins.len() > 128)
    {
        return Err(HarnessError::invalid(
            "cloud session plugin override count exceeds 128",
        ));
    }
    Ok(())
}
