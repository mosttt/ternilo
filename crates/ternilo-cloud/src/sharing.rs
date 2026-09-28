use std::time::Duration;

use ternilo_control::{ControlStore, ResourceAction, ResourceKind, resource_access_in};
use ternilo_protocol::{
    HarnessError, RunId, SessionId, SessionSubmissionRequest, TenantId, UserId,
};
use ternilo_storage::{Transaction, database_error, set_user_scope};

use crate::{
    CloudRunState, CloudSessionRecord, CloudStore, CloudSubmissionReceipt, CompiledRun,
    store::decode_session,
};

/// Resolve an authenticated actor to the existing resource owner in the same
/// transaction that reads or changes the canonical resource.
pub(crate) async fn session_owner_in(
    transaction: &mut Transaction,
    tenant_id: &TenantId,
    actor_id: &UserId,
    session_id: &SessionId,
    action: ResourceAction,
) -> Result<UserId, HarnessError> {
    let access = resource_access_in(
        transaction,
        actor_id,
        tenant_id,
        ResourceKind::Session,
        session_id.as_str(),
    )
    .await?;
    access.require(action)?;
    set_user_scope(transaction, &access.owner_user_id).await?;
    Ok(access.owner_user_id)
}

pub(crate) async fn audit_session_in(
    transaction: &mut Transaction,
    tenant_id: &TenantId,
    actor_id: &UserId,
    session_id: &SessionId,
    owner_id: &UserId,
    action: ResourceAction,
    now_ms: u64,
) -> Result<(), HarnessError> {
    ControlStore::record_resource_action_in(
        transaction,
        actor_id,
        tenant_id,
        ResourceKind::Session,
        session_id.as_str(),
        owner_id,
        action,
        now_ms,
    )
    .await
}

pub(crate) async fn require_workspace_read_in(
    transaction: &mut Transaction,
    tenant_id: &TenantId,
    actor_id: &UserId,
    session_id: &SessionId,
) -> Result<(), HarnessError> {
    let workspace: String = sqlx::query_scalar(
        "SELECT workspace_id FROM cloud_sessions WHERE tenant_id=$1 AND session_id=$2",
    )
    .bind(tenant_id.as_str())
    .bind(session_id.as_str())
    .fetch_one(&mut **transaction)
    .await
    .map_err(database_error)?;
    resource_access_in(
        transaction,
        actor_id,
        tenant_id,
        ResourceKind::Workspace,
        &workspace,
    )
    .await?
    .require(ResourceAction::View)
}

impl CloudStore {
    pub async fn session_command_as(
        &self,
        tenant_id: &TenantId,
        actor_id: &UserId,
        session_id: &SessionId,
        command_id: &ternilo_transport::CommandId,
    ) -> Result<Option<crate::CloudSessionCommandRecord>, HarnessError> {
        let mut transaction = self.tenant_transaction(tenant_id).await?;
        let owner_id = session_owner_in(
            &mut transaction,
            tenant_id,
            actor_id,
            session_id,
            ResourceAction::View,
        )
        .await?;
        let row = sqlx::query("SELECT * FROM cloud_session_commands WHERE tenant_id=$1 AND user_id=$2 AND session_id=$3 AND command_id=$4")
            .bind(tenant_id.as_str()).bind(owner_id.as_str()).bind(session_id.as_str()).bind(command_id.as_str())
            .fetch_optional(&mut *transaction).await.map_err(database_error)?;
        let record = row
            .as_ref()
            .map(crate::commands::decode_command_record)
            .transpose()?;
        if record.as_ref().is_some_and(|record| {
            matches!(
                record.command.body,
                ternilo_transport::ExecutorCommandBody::Application {
                    request: ternilo_transport::ApplicationOperation::SessionWorkspace { .. }
                }
            )
        }) {
            require_workspace_read_in(&mut transaction, tenant_id, actor_id, session_id).await?;
        }
        transaction.commit().await.map_err(database_error)?;
        Ok(record)
    }

    pub async fn wait_for_session_command_reply_as(
        &self,
        tenant_id: &TenantId,
        actor_id: &UserId,
        session_id: &SessionId,
        command_id: &ternilo_transport::CommandId,
        timeout: Duration,
        poll_interval: Duration,
    ) -> Result<Option<ternilo_transport::CommandReply>, HarnessError> {
        if poll_interval.is_zero() {
            return Err(HarnessError::invalid(
                "command reply poll interval must be positive",
            ));
        }
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let Some(record) = self
                .session_command_as(tenant_id, actor_id, session_id, command_id)
                .await?
            else {
                return Ok(None);
            };
            if let Some(reply) = record.reply {
                return Ok(Some(reply));
            }
            if tokio::time::Instant::now() >= deadline {
                return Ok(None);
            }
            tokio::time::sleep(
                poll_interval.min(deadline.saturating_duration_since(tokio::time::Instant::now())),
            )
            .await;
        }
    }

    pub async fn find_accessible_session(
        &self,
        tenant_id: &TenantId,
        actor_id: &UserId,
        session_id: &SessionId,
    ) -> Result<Option<CloudSessionRecord>, HarnessError> {
        self.find_accessible_session_in(tenant_id, actor_id, session_id, false)
            .await
    }

    /// Resolve the visible canonical record for deletion, including archived sessions.
    /// The delete transaction must still authorize the current actor's Delete permission.
    pub async fn find_accessible_session_for_deletion(
        &self,
        tenant_id: &TenantId,
        actor_id: &UserId,
        session_id: &SessionId,
    ) -> Result<Option<CloudSessionRecord>, HarnessError> {
        self.find_accessible_session_in(tenant_id, actor_id, session_id, true)
            .await
    }

    async fn find_accessible_session_in(
        &self,
        tenant_id: &TenantId,
        actor_id: &UserId,
        session_id: &SessionId,
        include_archived: bool,
    ) -> Result<Option<CloudSessionRecord>, HarnessError> {
        session_id.validate()?;
        let mut transaction = self.tenant_transaction(tenant_id).await?;
        let row = sqlx::query(
            "SELECT * FROM cloud_sessions WHERE tenant_id = $1 AND session_id = $2
             AND (CAST($3 AS BIGINT) = 1 OR archived_at_ms IS NULL)",
        )
        .bind(tenant_id.as_str())
        .bind(session_id.as_str())
        .bind(i64::from(include_archived))
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?;
        let session = if let Some(row) = row {
            let access = resource_access_in(
                &mut transaction,
                actor_id,
                tenant_id,
                ResourceKind::Session,
                session_id.as_str(),
            )
            .await?;
            access
                .permissions
                .view
                .then(|| decode_session(&row))
                .transpose()?
        } else {
            None
        };
        transaction.commit().await.map_err(database_error)?;
        Ok(session)
    }

    pub async fn list_accessible_sessions(
        &self,
        tenant_id: &TenantId,
        actor_id: &UserId,
        limit: u32,
    ) -> Result<Vec<CloudSessionRecord>, HarnessError> {
        self.list_accessible_sessions_by_archive(tenant_id, actor_id, limit, false)
            .await
    }

    pub async fn list_accessible_archived_sessions(
        &self,
        tenant_id: &TenantId,
        actor_id: &UserId,
        limit: u32,
    ) -> Result<Vec<CloudSessionRecord>, HarnessError> {
        self.list_accessible_sessions_by_archive(tenant_id, actor_id, limit, true)
            .await
    }

    async fn list_accessible_sessions_by_archive(
        &self,
        tenant_id: &TenantId,
        actor_id: &UserId,
        limit: u32,
        archived: bool,
    ) -> Result<Vec<CloudSessionRecord>, HarnessError> {
        if !(1..=1_000).contains(&limit) {
            return Err(HarnessError::invalid(
                "cloud session limit must be between 1 and 1000",
            ));
        }
        let mut transaction = self.tenant_transaction(tenant_id).await?;
        let rows = sqlx::query(
            "SELECT session.* FROM cloud_sessions AS session WHERE session.tenant_id = $1
             AND ((CAST($4 AS BIGINT) = 1 AND session.archived_at_ms IS NOT NULL)
               OR (CAST($4 AS BIGINT) = 0 AND session.archived_at_ms IS NULL))
             AND (session.user_id = $2 OR EXISTS (
                 SELECT 1 FROM control_project_workspace_access p WHERE p.tenant_id=session.tenant_id
                 AND p.workspace_id=session.workspace_id AND p.user_id=$2) OR EXISTS (
                 SELECT 1 FROM control_resource_shares AS grant_record
                 WHERE grant_record.tenant_id = session.tenant_id AND grant_record.grantee_user_id = $2
                 AND ((grant_record.resource_kind = 'session' AND grant_record.resource_id = session.session_id)
                 OR (grant_record.resource_kind = 'workspace' AND grant_record.resource_id = session.workspace_id)))
               OR EXISTS (
                   SELECT 1 FROM control_resource_group_shares AS group_grant
                   JOIN control_permission_group_members AS group_member
                     ON group_member.tenant_id = group_grant.tenant_id AND group_member.group_id = group_grant.group_id
                   WHERE group_grant.tenant_id = session.tenant_id AND group_member.user_id = $2
                     AND ((group_grant.resource_kind = 'session' AND group_grant.resource_id = session.session_id)
                     OR (group_grant.resource_kind = 'workspace' AND group_grant.resource_id = session.workspace_id)))
               OR EXISTS (
                   SELECT 1 FROM control_resource_fork_group_sources AS source
                   JOIN control_resource_group_shares AS group_grant
                     ON group_grant.tenant_id = source.tenant_id AND group_grant.resource_kind = source.source_resource_kind
                     AND group_grant.resource_id = source.source_resource_id AND group_grant.group_id = source.group_id
                   JOIN control_permission_group_members AS group_member
                     ON group_member.tenant_id = source.tenant_id AND group_member.group_id = source.group_id
                     AND group_member.user_id = source.user_id
                   WHERE source.tenant_id = session.tenant_id AND source.session_id = session.session_id
                     AND source.user_id = $2))
             ORDER BY session.updated_at_ms DESC, session.session_id LIMIT $3",
        )
        .bind(tenant_id.as_str())
        .bind(actor_id.as_str())
        .bind(i64::from(limit))
        .bind(i64::from(archived))
        .fetch_all(&mut *transaction)
        .await
        .map_err(database_error)?;
        let mut sessions = Vec::with_capacity(rows.len());
        for row in rows {
            let session = decode_session(&row)?;
            let access = resource_access_in(
                &mut transaction,
                actor_id,
                tenant_id,
                ResourceKind::Session,
                session.session_id.as_str(),
            )
            .await?;
            if access.permissions.view {
                sessions.push(session);
            }
        }
        transaction.commit().await.map_err(database_error)?;
        Ok(sessions)
    }

    pub async fn enqueue_session_submission_as(
        &self,
        actor_id: &UserId,
        compiled: &CompiledRun,
        request: &SessionSubmissionRequest,
        now_ms: u64,
    ) -> Result<CloudSubmissionReceipt, HarnessError> {
        let metadata = &compiled.spec.metadata;
        let mut transaction = self.tenant_transaction(&metadata.tenant_id).await?;
        let owner_id = session_owner_in(
            &mut transaction,
            &metadata.tenant_id,
            actor_id,
            &metadata.session_id,
            ResourceAction::Submit,
        )
        .await?;
        if metadata.user_id != owner_id
            || &compiled.actor_user_id != actor_id
            || compiled.authorization_session_id != metadata.session_id
        {
            return Err(HarnessError::policy(
                "shared runs must retain the session owner",
            ));
        }
        if actor_id != &owner_id {
            crate::shared_attachments::require_session_attachment_references_in(
                &mut transaction,
                &metadata.tenant_id,
                &metadata.session_id,
                &request.attachments,
            )
            .await?;
        }
        crate::store::lock_session_in(&mut transaction, &metadata.tenant_id, &metadata.session_id)
            .await?;
        let reservation = ControlStore::reserve_quota_for_owner_in(
            &mut transaction,
            actor_id,
            &owner_id,
            &metadata.tenant_id,
            Some(metadata.run_id.as_str()),
            compiled.reserved_model_tokens,
            Duration::from_hours(24),
            now_ms,
        )
        .await?;
        let receipt = Self::enqueue_session_submission_in(
            &mut transaction,
            compiled,
            &reservation.reservation_id,
            request,
            now_ms,
        )
        .await?;
        audit_session_in(
            &mut transaction,
            &metadata.tenant_id,
            actor_id,
            &metadata.session_id,
            &owner_id,
            ResourceAction::Submit,
            now_ms,
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(receipt)
    }

    pub async fn cancel_run_as(
        &self,
        tenant_id: &TenantId,
        actor_id: &UserId,
        session_id: &SessionId,
        run_id: &RunId,
        now_ms: u64,
    ) -> Result<CloudRunState, HarnessError> {
        let mut transaction = self.tenant_transaction(tenant_id).await?;
        let owner_id = session_owner_in(
            &mut transaction,
            tenant_id,
            actor_id,
            session_id,
            ResourceAction::Stop,
        )
        .await?;
        let bound = sqlx::query_scalar::<_, String>(
            "SELECT session_id FROM cloud_runs WHERE tenant_id = $1 AND run_id = $2",
        )
        .bind(tenant_id.as_str())
        .bind(run_id.as_str())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?;
        if bound.as_deref() != Some(session_id.as_str()) {
            return Err(HarnessError::policy(
                "run does not belong to the authorized session",
            ));
        }
        let state =
            Self::cancel_run_in(&mut transaction, tenant_id, run_id, Some(actor_id), now_ms)
                .await?;
        audit_session_in(
            &mut transaction,
            tenant_id,
            actor_id,
            session_id,
            &owner_id,
            ResourceAction::Stop,
            now_ms,
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(state)
    }
}
