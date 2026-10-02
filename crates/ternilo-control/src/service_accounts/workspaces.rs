use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::Row;
use ternilo_protocol::{HarnessError, TenantId, UserId, WorkspaceId};
use ternilo_storage::{Json, database_error, lock};

use super::account_in;
use crate::{
    ControlAction, ControlStore, ControlUser, PageQuery, ResourceAction, ResourceKind,
    ResourcePermissions, WorkspacePlacement, resource_access_in,
    store::{append_audit, require_action, to_i64},
};

#[derive(Debug, Serialize)]
pub struct ServiceWorkspaceAccess {
    pub workspace_id: WorkspaceId,
    pub name: String,
    pub placement: WorkspacePlacement,
    pub computer_name: Option<String>,
    pub permissions: Option<ResourcePermissions>,
}

#[derive(Debug, Serialize)]
pub struct ServiceWorkspacePage {
    pub workspaces: Vec<ServiceWorkspaceAccess>,
    pub next_cursor: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceWorkspaceUpdate {
    pub permissions: Option<ResourcePermissions>,
    pub expected_permissions: Option<ResourcePermissions>,
}

impl ControlStore {
    pub async fn service_account_workspaces(
        &self,
        actor: &ControlUser,
        tenant: &TenantId,
        id: &UserId,
        query: &PageQuery,
    ) -> Result<ServiceWorkspacePage, HarnessError> {
        let (pattern, cursor) = query.parameters()?;
        let mut tx = self.service_management(actor, tenant).await?;
        account_in(&mut tx, tenant, id).await?;
        let rows = sqlx::query("SELECT w.workspace_id,w.name,w.placement,m.display_name AS computer_name,s.permissions_json
            FROM control_workspaces w
            LEFT JOIN control_resource_ownership o ON o.tenant_id=w.tenant_id AND o.resource_kind='workspace' AND o.resource_id=w.workspace_id
            LEFT JOIN control_computer_management m ON m.tenant_id=w.tenant_id AND m.executor_id=w.executor_id
            LEFT JOIN control_resource_shares s ON s.tenant_id=w.tenant_id AND s.resource_kind='workspace' AND s.resource_id=w.workspace_id AND s.grantee_user_id=$3
            WHERE w.tenant_id=$1 AND COALESCE(o.owner_user_id,w.owner_user_id)=$2 AND w.unregistered_at_ms IS NULL
              AND (CAST($4 AS TEXT) IS NULL OR LOWER(w.name) LIKE $4 ESCAPE '!')
              AND (CAST($5 AS TEXT) IS NULL OR w.workspace_id>$5)
            ORDER BY w.workspace_id LIMIT $6")
            .bind(tenant.as_str()).bind(actor.user_id.as_str()).bind(id.as_str()).bind(pattern).bind(cursor).bind(i64::from(query.limit)+1)
            .fetch_all(&mut *tx).await.map_err(database_error)?;
        let mut workspaces = rows
            .iter()
            .map(|row| {
                Ok(ServiceWorkspaceAccess {
                    workspace_id: WorkspaceId::new(
                        row.try_get::<String, _>("workspace_id")
                            .map_err(database_error)?,
                    ),
                    name: row.try_get("name").map_err(database_error)?,
                    placement: WorkspacePlacement::parse(
                        &row.try_get::<String, _>("placement")
                            .map_err(database_error)?,
                    )?,
                    computer_name: row.try_get("computer_name").map_err(database_error)?,
                    permissions: row
                        .try_get::<Option<Json<ResourcePermissions>>, _>("permissions_json")
                        .map_err(database_error)?
                        .map(|value| value.0),
                })
            })
            .collect::<Result<Vec<_>, HarnessError>>()?;
        let next_cursor = query.finish(&mut workspaces, |entry| entry.workspace_id.to_string());
        tx.commit().await.map_err(database_error)?;
        Ok(ServiceWorkspacePage {
            workspaces,
            next_cursor,
        })
    }

    pub async fn set_service_workspace_access(
        &self,
        actor: &ControlUser,
        tenant: &TenantId,
        id: &UserId,
        workspace: &WorkspaceId,
        update: &ServiceWorkspaceUpdate,
        now_ms: u64,
    ) -> Result<(), HarnessError> {
        workspace.validate()?;
        if update
            .permissions
            .is_some_and(|permissions| !permissions.view)
        {
            return Err(HarnessError::invalid(
                "workspace authorization must allow viewing",
            ));
        }
        let mut tx = self.service_management(actor, tenant).await?;
        account_in(&mut tx, tenant, id).await?;
        lock(
            &mut tx,
            &format!("resource-shares:{tenant}:workspace:{workspace}"),
        )
        .await?;
        let access = resource_access_in(
            &mut tx,
            &actor.user_id,
            tenant,
            ResourceKind::Workspace,
            workspace.as_str(),
        )
        .await?;
        access.require(ResourceAction::ManageSharing)?;
        if &access.owner_user_id == id {
            return Err(HarnessError::invalid(
                "the workspace owner already has access",
            ));
        }
        let previous = sqlx::query("SELECT permissions_json FROM control_resource_shares WHERE tenant_id=$1 AND resource_kind='workspace' AND resource_id=$2 AND grantee_user_id=$3")
            .bind(tenant.as_str()).bind(workspace.as_str()).bind(id.as_str()).fetch_optional(&mut *tx).await.map_err(database_error)?
            .map(|row|row.try_get::<Json<ResourcePermissions>,_>("permissions_json").map(|value|value.0).map_err(database_error)).transpose()?;
        if previous != update.expected_permissions {
            return Err(HarnessError::conflict(
                "workspace authorization changed; reload before saving",
            ));
        }
        if let Some(permissions) = update.permissions {
            require_action(&mut tx, tenant, id, ControlAction::TenantRead).await?;
            sqlx::query("INSERT INTO control_resource_shares(tenant_id,resource_kind,resource_id,grantee_user_id,permissions_json,granted_by,created_at_ms,updated_at_ms) VALUES($1,'workspace',$2,$3,$4,$5,$6,$6) ON CONFLICT(tenant_id,resource_kind,resource_id,grantee_user_id) DO UPDATE SET permissions_json=EXCLUDED.permissions_json,granted_by=EXCLUDED.granted_by,updated_at_ms=EXCLUDED.updated_at_ms")
                .bind(tenant.as_str()).bind(workspace.as_str()).bind(id.as_str()).bind(Json(permissions)).bind(actor.user_id.as_str()).bind(to_i64(now_ms,"service workspace grant")?)
                .execute(&mut *tx).await.map_err(database_error)?;
        } else {
            sqlx::query("DELETE FROM control_resource_shares WHERE tenant_id=$1 AND resource_kind='workspace' AND resource_id=$2 AND grantee_user_id=$3")
                .bind(tenant.as_str()).bind(workspace.as_str()).bind(id.as_str()).execute(&mut *tx).await.map_err(database_error)?;
        }
        append_audit(
            &mut tx,
            tenant,
            Some(&actor.user_id),
            "user",
            if update.permissions.is_some() {
                "resource.share"
            } else {
                "resource.unshare"
            },
            "workspace",
            workspace.as_str(),
            "success",
            json!({"grantee_user_id":id,"permissions":update.permissions}),
            now_ms,
        )
        .await?;
        tx.commit().await.map_err(database_error)
    }
}
