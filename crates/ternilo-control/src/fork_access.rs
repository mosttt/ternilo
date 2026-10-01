use serde_json::json;
use sqlx::Row;
use ternilo_protocol::{HarnessError, SessionId, TenantId, UserId};
use ternilo_storage::{Json, Transaction, database_error};

use crate::{
    ControlStore, EdgeSessionMetadata, ResourceAccessSourceKind, ResourceAction, ResourceKind,
    ResourcePermissions, resource_access_in,
    store::{append_audit, to_i64},
};

impl ControlStore {
    /// Preserve only the initiating user's effective access to a user-created
    /// fork. Background subagent creation must not call this operation.
    #[expect(
        clippy::too_many_lines,
        reason = "Keep canonical lineage, management inheritance and existing fork grants in the same transaction."
    )]
    pub async fn inherit_fork_access_in(
        transaction: &mut Transaction,
        actor_id: &UserId,
        tenant_id: &TenantId,
        parent_id: &SessionId,
        child_id: &SessionId,
        now_ms: u64,
    ) -> Result<(), HarnessError> {
        let parent = resource_access_in(
            transaction,
            actor_id,
            tenant_id,
            ResourceKind::Session,
            parent_id.as_str(),
        )
        .await?;
        parent.require(ResourceAction::Submit)?;
        let child = resource_access_in(
            transaction,
            actor_id,
            tenant_id,
            ResourceKind::Session,
            child_id.as_str(),
        )
        .await?;
        if parent.storage_user_id != child.storage_user_id
            || !is_user_fork_in(transaction, tenant_id, parent_id, child_id).await?
        {
            return Err(HarnessError::policy(
                "fork access requires the parent's original owner and placement",
            ));
        }
        if parent.is_owner {
            if !child.is_owner {
                sqlx::query(
                    "INSERT INTO control_resource_ownership
                    (tenant_id,resource_kind,resource_id,owner_user_id,revision,updated_at_ms)
                    VALUES ($1,'session',$2,$3,1,$4)",
                )
                .bind(tenant_id.as_str())
                .bind(child_id.as_str())
                .bind(actor_id.as_str())
                .bind(to_i64(now_ms, "fork ownership timestamp")?)
                .execute(&mut **transaction)
                .await
                .map_err(database_error)?;
            }
            return Ok(());
        }
        let mode: String =
            sqlx::query_scalar("SELECT mode FROM control_instance_settings WHERE singleton = 1")
                .fetch_one(&mut **transaction)
                .await
                .map_err(database_error)?;
        if mode != "multi_user" {
            return Err(HarnessError::policy(
                "multi-user mode is required to share a fork",
            ));
        }
        let mut direct = ResourcePermissions::default();
        for source in &parent.sources {
            if source.kind == ResourceAccessSourceKind::DirectUser
                && source.resource_kind != ResourceKind::Project
            {
                direct.combine(source.permissions);
            }
        }
        direct = direct.intersect(parent.permissions);
        if direct.view {
            sqlx::query("INSERT INTO control_resource_shares
                (tenant_id, resource_kind, resource_id, grantee_user_id, permissions_json, granted_by, created_at_ms, updated_at_ms)
                VALUES ($1, 'session', $2, $3, $4, $3, $5, $5)")
                .bind(tenant_id.as_str()).bind(child_id.as_str()).bind(actor_id.as_str())
                .bind(Json(direct)).bind(to_i64(now_ms,"fork share timestamp")?)
                .execute(&mut **transaction).await.map_err(database_error)?;
        }
        // Preserve original group references instead of turning them into a permanent
        // personal grant. Forking again copies these same references, without recursion.
        let mut group_sources = std::collections::BTreeMap::new();
        for source in &parent.sources {
            // Same-workspace forks inherit current project rules; never freeze them as child grants.
            if source.resource_kind == ResourceKind::Project {
                continue;
            }
            if let Some(group_id) = &source.group_id {
                let permissions = group_sources
                    .entry((
                        source.resource_kind.as_str(),
                        source.resource_id.as_str(),
                        group_id.as_str(),
                    ))
                    .or_insert(ResourcePermissions::default());
                permissions.combine(source.permissions.intersect(parent.permissions));
            }
        }
        for ((resource_kind, resource_id, group_id), permissions) in group_sources {
            sqlx::query("INSERT INTO control_resource_fork_group_sources
                (tenant_id,session_id,user_id,source_resource_kind,source_resource_id,group_id,permissions_json,created_at_ms)
                VALUES ($1,$2,$3,$4,$5,$6,$7,$8)")
                .bind(tenant_id.as_str()).bind(child_id.as_str()).bind(actor_id.as_str()).bind(resource_kind).bind(resource_id).bind(group_id)
                .bind(Json(permissions)).bind(to_i64(now_ms,"fork share timestamp")?)
                .execute(&mut **transaction).await.map_err(database_error)?;
        }
        append_audit(
            transaction,
            tenant_id,
            Some(actor_id),
            "user",
            "resource.fork_access",
            "session",
            child_id.as_str(),
            "success",
            json!({
                "parent_session_id": parent_id,
                "owner_user_id": parent.owner_user_id,
                "grantee_user_id": actor_id,
                "permissions": parent.permissions,
            }),
            now_ms,
        )
        .await
        .map(drop)
    }
}

async fn is_user_fork_in(
    transaction: &mut Transaction,
    tenant_id: &TenantId,
    parent_id: &SessionId,
    child_id: &SessionId,
) -> Result<bool, HarnessError> {
    let edge = sqlx::query(
        "SELECT child.metadata_json,
                CAST(child.workspace_id = parent.workspace_id AND child.executor_id = parent.executor_id AS INTEGER) AS same_binding
         FROM control_edge_sessions child JOIN control_edge_sessions parent
           ON parent.tenant_id = child.tenant_id AND parent.session_id = $2
         WHERE child.tenant_id = $1 AND child.session_id = $3",
    ).bind(tenant_id.as_str()).bind(parent_id.as_str()).bind(child_id.as_str())
        .fetch_optional(&mut **transaction).await.map_err(database_error)?;
    if let Some(edge) = edge {
        let metadata = edge
            .try_get::<Json<EdgeSessionMetadata>, _>("metadata_json")
            .map_err(database_error)?
            .0;
        return Ok(edge
            .try_get::<i64, _>("same_binding")
            .map_err(database_error)?
            != 0
            && metadata.parent_session_id.as_ref() == Some(parent_id)
            && metadata.subagent.is_none());
    }
    sqlx::query_scalar::<_, i64>(
        "SELECT CAST(EXISTS(
            SELECT 1 FROM cloud_sessions child JOIN cloud_sessions parent
              ON parent.tenant_id = child.tenant_id AND parent.session_id = $2
            WHERE child.tenant_id = $1 AND child.session_id = $3
              AND child.parent_session_id = parent.session_id
              AND child.workspace_id = parent.workspace_id
              AND child.subagent_metadata IS NULL
         ) AS INTEGER)",
    )
    .bind(tenant_id.as_str())
    .bind(parent_id.as_str())
    .bind(child_id.as_str())
    .fetch_one(&mut **transaction)
    .await
    .map(|exists| exists != 0)
    .map_err(database_error)
}
