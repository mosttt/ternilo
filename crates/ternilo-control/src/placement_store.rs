use serde_json::{Value, json};
use sqlx::Row;
use ternilo_protocol::{HarnessError, SessionId, TenantId, UserId, WorkspaceId};
use ternilo_transport::ExecutorId;

use crate::{
    ControlAction, ControlStore, ControlUser, EdgeSessionMetadata, EdgeSessionRecord,
    ResourceAction, ResourceKind, WorkspaceRecord,
    crypto::random_identifier,
    resource_access_in,
    store::{
        append_audit, database_error, from_i64, require_action, set_tenant, to_i64,
        workspace_from_row, workspace_write_error,
    },
    types::require_bounded,
};

impl ControlStore {
    /// Resolve a registered Workspace for the exact authenticated owner.
    /// Tenant administrators do not implicitly gain access to another user's
    /// Harness: the workbench is owner-scoped even when tenant administration
    /// APIs are broader.
    pub async fn resolve_owned_workspace(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        workspace_id: &WorkspaceId,
    ) -> Result<WorkspaceRecord, HarnessError> {
        workspace_id.validate()?;
        let mut transaction = self.database.begin().await?;
        prepare_owner_read(&mut transaction, actor, tenant_id).await?;
        let row = sqlx::query(
            "SELECT tenant_id, workspace_id, project_id, owner_user_id, name,
                    placement, storage, executor_id, executor_workspace_id,
                    created_at_ms, updated_at_ms
             FROM control_workspaces
             WHERE tenant_id = $1 AND workspace_id = $2 AND owner_user_id = $3
               AND unregistered_at_ms IS NULL",
        )
        .bind(tenant_id.as_str())
        .bind(workspace_id.as_str())
        .bind(actor.user_id.as_str())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?
        .ok_or_else(workspace_not_found)?;
        transaction.commit().await.map_err(database_error)?;
        workspace_from_row(&row)
    }

    /// Resolve the immutable binding behind an existing Session. Unlike
    /// `resolve_owned_workspace`, this includes logically unregistered rows so
    /// an Ungrouped Session remains runnable.
    pub async fn resolve_owned_session_workspace(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        workspace_id: &WorkspaceId,
    ) -> Result<WorkspaceRecord, HarnessError> {
        workspace_id.validate()?;
        let mut transaction = self.database.begin().await?;
        prepare_owner_read(&mut transaction, actor, tenant_id).await?;
        let row = sqlx::query(
            "SELECT tenant_id, workspace_id, project_id, owner_user_id, name,
                    placement, storage, executor_id, executor_workspace_id,
                    created_at_ms, updated_at_ms
             FROM control_workspaces
             WHERE tenant_id = $1 AND workspace_id = $2 AND owner_user_id = $3",
        )
        .bind(tenant_id.as_str())
        .bind(workspace_id.as_str())
        .bind(actor.user_id.as_str())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?
        .ok_or_else(workspace_not_found)?;
        transaction.commit().await.map_err(database_error)?;
        workspace_from_row(&row)
    }

    pub async fn rename_owned_workspace(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        workspace_id: &WorkspaceId,
        title: &str,
        now_ms: u64,
    ) -> Result<WorkspaceRecord, HarnessError> {
        workspace_id.validate()?;
        require_bounded(title, "workspace title", 256)?;
        let now = to_i64(now_ms, "workspace rename timestamp")?;
        let mut transaction = self.database.begin().await?;
        set_tenant(&mut transaction, tenant_id).await?;
        ternilo_storage::lock(
            &mut transaction,
            &format!("resource-shares:{tenant_id}:workspace:{workspace_id}"),
        )
        .await?;
        let access = resource_access_in(
            &mut transaction,
            &actor.user_id,
            tenant_id,
            ResourceKind::Workspace,
            workspace_id.as_str(),
        )
        .await?;
        access.require(ResourceAction::Delete)?;
        let project: String = sqlx::query_scalar(
            "SELECT project_id FROM control_workspaces WHERE tenant_id=$1 AND workspace_id=$2",
        )
        .bind(tenant_id.as_str())
        .bind(workspace_id.as_str())
        .fetch_one(&mut *transaction)
        .await
        .map_err(database_error)?;
        crate::resource_ownership::require_workspace_name_in(
            &mut transaction,
            tenant_id,
            &access.owner_user_id,
            &project,
            title,
            Some(workspace_id.as_str()),
        )
        .await?;
        let row = sqlx::query(
            "UPDATE control_workspaces
             SET name = $4, updated_at_ms = $5
             WHERE tenant_id = $1 AND workspace_id = $2 AND owner_user_id = $3
               AND unregistered_at_ms IS NULL
             RETURNING tenant_id, workspace_id, project_id, owner_user_id, name,
                       placement, storage, executor_id, executor_workspace_id,
                       created_at_ms, updated_at_ms",
        )
        .bind(tenant_id.as_str())
        .bind(workspace_id.as_str())
        .bind(access.storage_user_id.as_str())
        .bind(title)
        .bind(now)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(workspace_write_error)?
        .ok_or_else(workspace_not_found)?;
        append_audit(
            &mut transaction,
            tenant_id,
            Some(&actor.user_id),
            "user",
            "workspace.rename",
            "workspace",
            workspace_id.as_str(),
            "success",
            json!({ "title": title }),
            now_ms,
        )
        .await?;
        transaction.commit().await.map_err(workspace_write_error)?;
        workspace_from_row(&row)
    }

    /// Remove a Workspace registration while retaining both the binding row
    /// and every cloud/edge Session that was created from it.
    pub async fn unregister_owned_workspace(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        workspace_id: &WorkspaceId,
        now_ms: u64,
    ) -> Result<(), HarnessError> {
        workspace_id.validate()?;
        let now = to_i64(now_ms, "workspace unregister timestamp")?;
        let mut transaction = self.database.begin().await?;
        set_tenant(&mut transaction, tenant_id).await?;
        ternilo_storage::lock(
            &mut transaction,
            &format!("resource-shares:{tenant_id}:workspace:{workspace_id}"),
        )
        .await?;
        let access = resource_access_in(
            &mut transaction,
            &actor.user_id,
            tenant_id,
            ResourceKind::Workspace,
            workspace_id.as_str(),
        )
        .await?;
        access.require(ResourceAction::Delete)?;
        let changed = sqlx::query(
            "UPDATE control_workspaces
             SET unregistered_at_ms = $4, updated_at_ms = $4
             WHERE tenant_id = $1 AND workspace_id = $2 AND owner_user_id = $3
               AND unregistered_at_ms IS NULL",
        )
        .bind(tenant_id.as_str())
        .bind(workspace_id.as_str())
        .bind(access.storage_user_id.as_str())
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?
        .rows_affected();
        if changed != 1 {
            return Err(workspace_not_found());
        }
        append_audit(
            &mut transaction,
            tenant_id,
            Some(&actor.user_id),
            "user",
            "workspace.unregister",
            "workspace",
            workspace_id.as_str(),
            "success",
            json!({}),
            now_ms,
        )
        .await?;
        transaction.commit().await.map_err(database_error)
    }

    #[allow(clippy::too_many_arguments)]
    #[expect(
        clippy::too_many_lines,
        reason = "Keep placement authorization, the canonical Node mapping, fork access and actor audit in one atomic transaction."
    )]
    pub async fn create_edge_session_mapping(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        workspace_id: &WorkspaceId,
        executor_id: &ExecutorId,
        node_session_id: &SessionId,
        requested_session_id: Option<&SessionId>,
        mut metadata: EdgeSessionMetadata,
        now_ms: u64,
    ) -> Result<EdgeSessionRecord, HarnessError> {
        workspace_id.validate()?;
        executor_id.validate()?;
        node_session_id.validate()?;
        if let Some(session_id) = requested_session_id {
            session_id.validate()?;
        }
        metadata.validate()?;
        let session_id = requested_session_id
            .cloned()
            .unwrap_or_else(|| SessionId::new(random_identifier("ses")));
        let now = to_i64(now_ms, "edge session creation timestamp")?;
        metadata.server_model = None;
        let mut transaction = self.database.begin().await?;
        set_tenant(&mut transaction, tenant_id).await?;
        let mut parent_node_session_id = None;
        let owner_id = if let Some(parent_id) = &metadata.parent_session_id {
            let access = resource_access_in(
                &mut transaction,
                &actor.user_id,
                tenant_id,
                ResourceKind::Session,
                parent_id.as_str(),
            )
            .await?;
            access.require(ResourceAction::Submit)?;
            let parent = sqlx::query("SELECT workspace_id, executor_id, owner_user_id, node_session_id, metadata_json FROM control_edge_sessions WHERE tenant_id=$1 AND session_id=$2")
                .bind(tenant_id.as_str()).bind(parent_id.as_str()).fetch_optional(&mut *transaction).await.map_err(database_error)?.ok_or_else(session_not_found)?;
            let inherited: ternilo_storage::Json<EdgeSessionMetadata> =
                parent.try_get("metadata_json").map_err(database_error)?;
            metadata.server_model = inherited.0.server_model;
            let parent_workspace: String =
                parent.try_get("workspace_id").map_err(database_error)?;
            let parent_executor: String = parent.try_get("executor_id").map_err(database_error)?;
            if parent_workspace != workspace_id.as_str() || parent_executor != executor_id.as_str()
            {
                return Err(HarnessError::policy(
                    "forked edge Session changed its immutable placement binding",
                ));
            }
            parent_node_session_id = Some(SessionId::new(
                parent
                    .try_get::<String, _>("node_session_id")
                    .map_err(database_error)?,
            ));
            access.storage_user_id
        } else {
            let access = resource_access_in(
                &mut transaction,
                &actor.user_id,
                tenant_id,
                ResourceKind::Workspace,
                workspace_id.as_str(),
            )
            .await?;
            access.require(ResourceAction::Submit)?;
            require_local_workspace_binding(
                &mut transaction,
                &access.storage_user_id,
                tenant_id,
                workspace_id,
                executor_id,
            )
            .await?;
            access.storage_user_id
        };
        if crate::edge_store::session_deleted(
            &mut transaction,
            tenant_id,
            executor_id,
            node_session_id,
        )
        .await?
        {
            return Err(HarnessError::policy(
                "deleted Node session identity cannot be reused",
            ));
        }
        let metadata_json = serde_json::to_value(&metadata).map_err(|error| json_error(&error))?;
        sqlx::query(
            "INSERT INTO control_edge_sessions
                (tenant_id, session_id, workspace_id, executor_id, owner_user_id,
                 node_session_id, metadata_json, last_event_seq,
                 created_at_ms, updated_at_ms)
             VALUES ($1, $2, $3, $4, $5, $6, $7, NULL, $8, $8)",
        )
        .bind(tenant_id.as_str())
        .bind(session_id.as_str())
        .bind(workspace_id.as_str())
        .bind(executor_id.as_str())
        .bind(owner_id.as_str())
        .bind(node_session_id.as_str())
        .bind(ternilo_storage::Json(metadata_json))
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(mapping_conflict)?;
        crate::EdgeStore::record_session_provenance_context_in_transaction(
            &mut transaction,
            tenant_id,
            executor_id,
            node_session_id,
            parent_node_session_id.as_ref(),
            metadata
                .subagent
                .as_ref()
                .map(|subagent| &subagent.subagent_id),
        )
        .await?;
        if metadata.subagent.is_none()
            && let Some(parent_id) = &metadata.parent_session_id
        {
            Self::inherit_fork_access_in(
                &mut transaction,
                &actor.user_id,
                tenant_id,
                parent_id,
                &session_id,
                now_ms,
            )
            .await?;
        }
        append_audit(
            &mut transaction,
            tenant_id,
            Some(&actor.user_id),
            "user",
            "edge_session.create",
            "session",
            session_id.as_str(),
            "success",
            json!({ "workspace_id": workspace_id, "placement": "local_node", "owner_user_id": owner_id }),
            now_ms,
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(EdgeSessionRecord {
            tenant_id: tenant_id.clone(),
            session_id,
            workspace_id: workspace_id.clone(),
            executor_id: executor_id.clone(),
            owner_user_id: owner_id.clone(),
            node_session_id: node_session_id.clone(),
            metadata,
            last_event_seq: None,
            created_at_ms: now_ms,
            updated_at_ms: now_ms,
        })
    }

    pub async fn list_owned_edge_sessions(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
    ) -> Result<Vec<EdgeSessionRecord>, HarnessError> {
        let mut transaction = self.database.begin().await?;
        prepare_owner_read(&mut transaction, actor, tenant_id).await?;
        let rows = sqlx::query(
            "SELECT tenant_id, session_id, workspace_id, executor_id, owner_user_id,
                    node_session_id, metadata_json, last_event_seq,
                    created_at_ms, updated_at_ms
             FROM control_edge_sessions
             WHERE tenant_id = $1 AND owner_user_id = $2
             ORDER BY updated_at_ms DESC, session_id",
        )
        .bind(tenant_id.as_str())
        .bind(actor.user_id.as_str())
        .fetch_all(&mut *transaction)
        .await
        .map_err(database_error)?;
        transaction.commit().await.map_err(database_error)?;
        rows.iter().map(edge_session_from_row).collect()
    }

    pub async fn find_owned_edge_session(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        session_id: &SessionId,
    ) -> Result<Option<EdgeSessionRecord>, HarnessError> {
        session_id.validate()?;
        let mut transaction = self.database.begin().await?;
        prepare_owner_read(&mut transaction, actor, tenant_id).await?;
        let row =
            select_owned_edge_session(&mut transaction, &actor.user_id, tenant_id, session_id)
                .await?;
        transaction.commit().await.map_err(database_error)?;
        row.as_ref().map(edge_session_from_row).transpose()
    }

    pub async fn update_edge_session_metadata(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        session_id: &SessionId,
        node_session_id: &SessionId,
        mut metadata: EdgeSessionMetadata,
        now_ms: u64,
    ) -> Result<EdgeSessionRecord, HarnessError> {
        session_id.validate()?;
        node_session_id.validate()?;
        metadata.validate()?;
        let now = to_i64(now_ms, "edge session cache timestamp")?;
        let mut transaction = self.database.begin().await?;
        let access = resource_access_in(
            &mut transaction,
            &actor.user_id,
            tenant_id,
            ResourceKind::Session,
            session_id.as_str(),
        )
        .await?;
        access.require(ResourceAction::View)?;
        let owner_id = &access.storage_user_id;
        let existing: ternilo_storage::Json<EdgeSessionMetadata> = sqlx::query_scalar(ternilo_storage::for_update(&transaction,
            "SELECT metadata_json FROM control_edge_sessions WHERE tenant_id=$1 AND session_id=$2",
            "SELECT metadata_json FROM control_edge_sessions WHERE tenant_id=$1 AND session_id=$2 FOR UPDATE"))
            .bind(tenant_id.as_str()).bind(session_id.as_str()).fetch_one(&mut *transaction).await.map_err(database_error)?;
        metadata.server_model = existing.0.server_model;
        let metadata_json = serde_json::to_value(metadata).map_err(|error| json_error(&error))?;
        let row = sqlx::query(
            "UPDATE control_edge_sessions
             SET metadata_json = $5, updated_at_ms = $6
             WHERE tenant_id = $1 AND session_id = $2 AND owner_user_id = $3
               AND node_session_id = $4
             RETURNING tenant_id, session_id, workspace_id, executor_id, owner_user_id,
                       node_session_id, metadata_json, last_event_seq,
                       created_at_ms, updated_at_ms",
        )
        .bind(tenant_id.as_str())
        .bind(session_id.as_str())
        .bind(owner_id.as_str())
        .bind(node_session_id.as_str())
        .bind(ternilo_storage::Json(metadata_json))
        .bind(now)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?
        .ok_or_else(session_not_found)?;
        transaction.commit().await.map_err(database_error)?;
        edge_session_from_row(&row)
    }

    pub async fn apply_generated_edge_title(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        session_id: &SessionId,
        node_session_id: &SessionId,
        title: &str,
        now_ms: u64,
    ) -> Result<EdgeSessionRecord, HarnessError> {
        session_id.validate()?;
        node_session_id.validate()?;
        require_bounded(title, "generated edge session title", 120)?;
        let now = to_i64(now_ms, "edge generated title timestamp")?;
        let mut transaction = self.database.begin().await?;
        let access = resource_access_in(
            &mut transaction,
            &actor.user_id,
            tenant_id,
            ResourceKind::Session,
            session_id.as_str(),
        )
        .await?;
        access.require(ResourceAction::View)?;
        let owner_id = &access.storage_user_id;
        let row = sqlx::query(ternilo_storage::for_update(&transaction,
            "SELECT tenant_id, session_id, workspace_id, executor_id, owner_user_id, node_session_id, metadata_json, last_event_seq, created_at_ms, updated_at_ms FROM control_edge_sessions WHERE tenant_id = $1 AND session_id = $2 AND owner_user_id = $3",
            "SELECT tenant_id, session_id, workspace_id, executor_id, owner_user_id, node_session_id, metadata_json, last_event_seq, created_at_ms, updated_at_ms FROM control_edge_sessions WHERE tenant_id = $1 AND session_id = $2 AND owner_user_id = $3 FOR UPDATE",
        ))
        .bind(tenant_id.as_str()).bind(session_id.as_str()).bind(owner_id.as_str())
        .fetch_optional(&mut *transaction).await.map_err(database_error)?
        .ok_or_else(session_not_found)?;
        let mut record = edge_session_from_row(&row)?;
        if record.node_session_id == *node_session_id && record.metadata.title == "New session" {
            record.metadata.title = title.to_owned();
            record.metadata.blank = false;
            record.metadata.updated_at_ms = now_ms;
            record.updated_at_ms = now_ms;
            sqlx::query("UPDATE control_edge_sessions SET metadata_json = $4, updated_at_ms = $5 WHERE tenant_id = $1 AND session_id = $2 AND owner_user_id = $3")
                .bind(tenant_id.as_str()).bind(session_id.as_str()).bind(owner_id.as_str())
                .bind(ternilo_storage::Json(&record.metadata)).bind(now)
                .execute(&mut *transaction).await.map_err(database_error)?;
        }
        transaction.commit().await.map_err(database_error)?;
        Ok(record)
    }

    #[expect(
        clippy::too_many_lines,
        reason = "Keep binding checks, events, grants, deletion marker and audit in the same transaction."
    )]
    pub async fn delete_edge_session_mapping(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        session_id: &SessionId,
        node_session_id: &SessionId,
        now_ms: u64,
    ) -> Result<(), HarnessError> {
        session_id.validate()?;
        node_session_id.validate()?;
        let mut transaction = self.database.begin().await?;
        prepare_owner_read(&mut transaction, actor, tenant_id).await?;
        let executor = sqlx::query_scalar::<_, String>(
            "SELECT executor_id FROM control_edge_sessions WHERE tenant_id=$1 AND session_id=$2",
        )
        .bind(tenant_id.as_str())
        .bind(session_id.as_str())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?;
        if let Some(executor) = executor {
            ternilo_storage::lock(
                &mut transaction,
                &format!("node-uploads:{tenant_id}:{executor}"),
            )
            .await?;
            let exists = sqlx::query_scalar::<_, String>(
                "SELECT session_id FROM control_edge_sessions WHERE tenant_id=$1 AND session_id=$2",
            )
            .bind(tenant_id.as_str())
            .bind(session_id.as_str())
            .fetch_optional(&mut *transaction)
            .await
            .map_err(database_error)?;
            if exists.is_none() {
                return transaction.commit().await.map_err(database_error);
            }
        } else {
            return transaction.commit().await.map_err(database_error);
        }
        let access = resource_access_in(
            &mut transaction,
            &actor.user_id,
            tenant_id,
            ResourceKind::Session,
            session_id.as_str(),
        )
        .await?;
        access.require(ResourceAction::Delete)?;
        let mapping = require_owned_edge_session(
            &mut transaction,
            &access.storage_user_id,
            tenant_id,
            session_id,
        )
        .await?;
        let mapped_node: String = mapping.try_get("node_session_id").map_err(database_error)?;
        let executor: String = mapping.try_get("executor_id").map_err(database_error)?;
        if mapped_node != node_session_id.as_str() {
            return Err(session_not_found());
        }
        sqlx::query(
            "DELETE FROM control_edge_events
             WHERE tenant_id = $1 AND executor_id = $2 AND session_id = $3",
        )
        .bind(tenant_id.as_str())
        .bind(executor)
        .bind(node_session_id.as_str())
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        sqlx::query(
            "DELETE FROM control_edge_sessions
             WHERE tenant_id = $1 AND session_id = $2 AND owner_user_id = $3",
        )
        .bind(tenant_id.as_str())
        .bind(session_id.as_str())
        .bind(access.storage_user_id.as_str())
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        for statement in [
            "DELETE FROM control_resource_shares WHERE tenant_id=$1 AND resource_kind='session' AND resource_id=$2",
            "DELETE FROM control_resource_group_shares WHERE tenant_id=$1 AND resource_kind='session' AND resource_id=$2",
            "DELETE FROM control_resource_fork_group_sources WHERE tenant_id=$1 AND session_id=$2",
        ] {
            sqlx::query(statement)
                .bind(tenant_id.as_str())
                .bind(session_id.as_str())
                .execute(&mut *transaction)
                .await
                .map_err(database_error)?;
        }
        append_audit(
            &mut transaction,
            tenant_id,
            Some(&actor.user_id),
            "user",
            "edge_session.delete",
            "session",
            session_id.as_str(),
            "success",
            json!({ "placement": "local_node", "owner_user_id": access.owner_user_id }),
            now_ms,
        )
        .await?;
        transaction.commit().await.map_err(database_error)
    }
}

async fn prepare_owner_read(
    transaction: &mut ternilo_storage::Transaction,
    actor: &ControlUser,
    tenant_id: &TenantId,
) -> Result<(), HarnessError> {
    set_tenant(transaction, tenant_id).await?;
    require_action(
        transaction,
        tenant_id,
        &actor.user_id,
        ControlAction::TenantRead,
    )
    .await
    .map(|_| ())
}

async fn require_local_workspace_binding(
    transaction: &mut ternilo_storage::Transaction,
    owner_id: &UserId,
    tenant_id: &TenantId,
    workspace_id: &WorkspaceId,
    executor_id: &ExecutorId,
) -> Result<(), HarnessError> {
    let exists = sqlx::query_scalar::<_, i64>(
        "SELECT CAST(EXISTS(
            SELECT 1 FROM control_workspaces
            WHERE tenant_id = $1 AND workspace_id = $2 AND owner_user_id = $3
              AND placement = 'local_node' AND executor_id = $4
              AND unregistered_at_ms IS NULL
         ) AS INTEGER)",
    )
    .bind(tenant_id.as_str())
    .bind(workspace_id.as_str())
    .bind(owner_id.as_str())
    .bind(executor_id.as_str())
    .fetch_one(&mut **transaction)
    .await
    .map(|value| value != 0)
    .map_err(database_error)?;
    if exists {
        Ok(())
    } else {
        Err(workspace_not_found())
    }
}

async fn require_owned_edge_session(
    transaction: &mut ternilo_storage::Transaction,
    storage_owner: &UserId,
    tenant_id: &TenantId,
    session_id: &SessionId,
) -> Result<sqlx::any::AnyRow, HarnessError> {
    select_owned_edge_session(transaction, storage_owner, tenant_id, session_id)
        .await?
        .ok_or_else(session_not_found)
}

async fn select_owned_edge_session(
    transaction: &mut ternilo_storage::Transaction,
    storage_owner: &UserId,
    tenant_id: &TenantId,
    session_id: &SessionId,
) -> Result<Option<sqlx::any::AnyRow>, HarnessError> {
    sqlx::query(
        "SELECT tenant_id, session_id, workspace_id, executor_id, owner_user_id,
                node_session_id, metadata_json, last_event_seq,
                created_at_ms, updated_at_ms
         FROM control_edge_sessions
         WHERE tenant_id = $1 AND session_id = $2 AND owner_user_id = $3",
    )
    .bind(tenant_id.as_str())
    .bind(session_id.as_str())
    .bind(storage_owner.as_str())
    .fetch_optional(&mut **transaction)
    .await
    .map_err(database_error)
}

pub(crate) fn edge_session_from_row(
    row: &sqlx::any::AnyRow,
) -> Result<EdgeSessionRecord, HarnessError> {
    let metadata: ternilo_storage::Json<Value> =
        row.try_get("metadata_json").map_err(database_error)?;
    Ok(EdgeSessionRecord {
        tenant_id: TenantId::new(
            row.try_get::<String, _>("tenant_id")
                .map_err(database_error)?,
        ),
        session_id: SessionId::new(
            row.try_get::<String, _>("session_id")
                .map_err(database_error)?,
        ),
        workspace_id: WorkspaceId::new(
            row.try_get::<String, _>("workspace_id")
                .map_err(database_error)?,
        ),
        executor_id: ExecutorId::new(
            row.try_get::<String, _>("executor_id")
                .map_err(database_error)?,
        ),
        owner_user_id: UserId::new(
            row.try_get::<String, _>("owner_user_id")
                .map_err(database_error)?,
        ),
        node_session_id: SessionId::new(
            row.try_get::<String, _>("node_session_id")
                .map_err(database_error)?,
        ),
        metadata: serde_json::from_value(metadata.0).map_err(|error| json_error(&error))?,
        last_event_seq: row
            .try_get::<Option<i64>, _>("last_event_seq")
            .map_err(database_error)?
            .map(|value| from_i64(value, "edge session event sequence"))
            .transpose()?,
        created_at_ms: from_i64(
            row.try_get("created_at_ms").map_err(database_error)?,
            "edge session creation timestamp",
        )?,
        updated_at_ms: from_i64(
            row.try_get("updated_at_ms").map_err(database_error)?,
            "edge session update timestamp",
        )?,
    })
}

fn workspace_not_found() -> HarnessError {
    HarnessError::invalid("workspace does not exist")
}

fn session_not_found() -> HarnessError {
    HarnessError::invalid("session does not exist")
}

fn json_error(error: &serde_json::Error) -> HarnessError {
    HarnessError::execution(format!("edge session metadata JSON failed: {error}"))
}

fn mapping_conflict(error: sqlx::Error) -> HarnessError {
    if error
        .as_database_error()
        .is_some_and(sqlx::error::DatabaseError::is_unique_violation)
    {
        HarnessError::invalid("session mapping already exists")
    } else {
        database_error(error)
    }
}
