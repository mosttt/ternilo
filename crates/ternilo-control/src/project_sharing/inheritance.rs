use serde::Serialize;
use serde_json::json;
use sqlx::Row;
use ternilo_protocol::{HarnessError, TenantId, UserId, WorkspaceId};
use ternilo_storage::{Json, Transaction, database_error, lock};

use crate::{
    ControlStore, ControlUser, ResourceAccessSource, ResourceAccessSourceKind, ResourceAction,
    ResourceKind, ResourcePermissions, resource_access_in,
    store::{append_audit, to_i64},
};

#[derive(Clone, Debug, Serialize)]
pub struct ProjectSharingInheritance {
    pub project_id: String,
    pub project_name: String,
    pub enabled: bool,
    pub can_change: bool,
}

impl ControlStore {
    pub async fn workspace_project_sharing(
        &self,
        actor: &ControlUser,
        tenant: &TenantId,
        workspace: &WorkspaceId,
    ) -> Result<ProjectSharingInheritance, HarnessError> {
        let mut tx = self.database.tenant_transaction(tenant).await?;
        let access = resource_access_in(
            &mut tx,
            &actor.user_id,
            tenant,
            ResourceKind::Workspace,
            workspace.as_str(),
        )
        .await?;
        access.require(ResourceAction::View)?;
        let mut inheritance = inheritance_in(&mut tx, tenant, workspace).await?;
        inheritance.can_change &= access.can_manage_sharing;
        tx.commit().await.map_err(database_error)?;
        Ok(inheritance)
    }

    pub async fn set_workspace_project_sharing(
        &self,
        actor: &ControlUser,
        tenant: &TenantId,
        workspace: &WorkspaceId,
        enabled: bool,
        now: u64,
    ) -> Result<ProjectSharingInheritance, HarnessError> {
        let mut tx = self.database.tenant_transaction(tenant).await?;
        lock(
            &mut tx,
            &format!("resource-shares:{tenant}:workspace:{workspace}"),
        )
        .await?;
        resource_access_in(
            &mut tx,
            &actor.user_id,
            tenant,
            ResourceKind::Workspace,
            workspace.as_str(),
        )
        .await?
        .require(ResourceAction::ManageSharing)?;
        crate::account_store::require_team_in(&mut tx, tenant).await?;
        let mut inheritance = inheritance_in(&mut tx, tenant, workspace).await?;
        if enabled {
            super::grants::require_multi_user(&mut tx).await?;
            sqlx::query("INSERT INTO control_workspace_project_sharing (tenant_id,workspace_id,enabled_by,enabled_at_ms) VALUES ($1,$2,$3,$4) ON CONFLICT (tenant_id,workspace_id) DO NOTHING")
                .bind(tenant.as_str()).bind(workspace.as_str()).bind(actor.user_id.as_str()).bind(to_i64(now,"sharing timestamp")?)
                .execute(&mut *tx).await.map_err(database_error)?;
        } else {
            sqlx::query("DELETE FROM control_workspace_project_sharing WHERE tenant_id=$1 AND workspace_id=$2")
                .bind(tenant.as_str()).bind(workspace.as_str()).execute(&mut *tx).await.map_err(database_error)?;
        }
        append_audit(
            &mut tx,
            tenant,
            Some(&actor.user_id),
            "user",
            "workspace.project_sharing",
            "workspace",
            workspace.as_str(),
            "success",
            json!({"project_id":inheritance.project_id,"enabled":enabled}),
            now,
        )
        .await?;
        tx.commit().await.map_err(database_error)?;
        inheritance.enabled = enabled;
        inheritance.can_change = true;
        Ok(inheritance)
    }
}

async fn inheritance_in(
    tx: &mut Transaction,
    tenant: &TenantId,
    workspace: &WorkspaceId,
) -> Result<ProjectSharingInheritance, HarnessError> {
    let row = sqlx::query("SELECT w.project_id,p.name,t.kind AS space_kind,CAST(EXISTS(SELECT 1 FROM control_workspace_project_sharing i WHERE i.tenant_id=w.tenant_id AND i.workspace_id=w.workspace_id) AS INTEGER) AS enabled
        FROM control_workspaces w JOIN control_projects p ON p.tenant_id=w.tenant_id AND p.project_id=w.project_id
        JOIN control_tenants t ON t.tenant_id=w.tenant_id
        WHERE w.tenant_id=$1 AND w.workspace_id=$2")
        .bind(tenant.as_str()).bind(workspace.as_str()).fetch_optional(&mut **tx).await.map_err(database_error)?
        .ok_or_else(|| HarnessError::invalid("workspace does not exist"))?;
    Ok(ProjectSharingInheritance {
        project_id: row.try_get("project_id").map_err(database_error)?,
        project_name: row.try_get("name").map_err(database_error)?,
        enabled: row.try_get::<i64, _>("enabled").map_err(database_error)? != 0,
        can_change: row
            .try_get::<String, _>("space_kind")
            .map_err(database_error)?
            == "team",
    })
}

pub(crate) async fn sources_in(
    tx: &mut Transaction,
    actor: &UserId,
    tenant: &TenantId,
    workspace: &str,
) -> Result<Vec<ResourceAccessSource>, HarnessError> {
    let rows = sqlx::query("SELECT 'direct_user' AS source_kind,p.project_id,p.name AS project_name,CAST(NULL AS TEXT) AS group_id,CAST(NULL AS TEXT) AS group_name,s.permissions_json
        FROM control_workspace_project_sharing i
        JOIN control_workspaces w ON w.tenant_id=i.tenant_id AND w.workspace_id=i.workspace_id
        JOIN control_projects p ON p.tenant_id=w.tenant_id AND p.project_id=w.project_id
        JOIN control_project_user_shares s ON s.tenant_id=p.tenant_id AND s.project_id=p.project_id AND s.grantee_user_id=$2
        WHERE i.tenant_id=$1 AND i.workspace_id=$3
        UNION ALL
        SELECT 'group',p.project_id,p.name,g.group_id,g.name,s.permissions_json
        FROM control_workspace_project_sharing i
        JOIN control_workspaces w ON w.tenant_id=i.tenant_id AND w.workspace_id=i.workspace_id
        JOIN control_projects p ON p.tenant_id=w.tenant_id AND p.project_id=w.project_id
        JOIN control_project_group_shares s ON s.tenant_id=p.tenant_id AND s.project_id=p.project_id
        JOIN control_permission_groups g ON g.tenant_id=s.tenant_id AND g.group_id=s.group_id
        JOIN control_permission_group_members m ON m.tenant_id=s.tenant_id AND m.group_id=s.group_id AND m.user_id=$2
        WHERE i.tenant_id=$1 AND i.workspace_id=$3 ORDER BY source_kind,group_id")
        .bind(tenant.as_str()).bind(actor.as_str()).bind(workspace).fetch_all(&mut **tx).await.map_err(database_error)?;
    rows.iter()
        .map(|row| {
            Ok(ResourceAccessSource {
                kind: if row
                    .try_get::<String, _>("source_kind")
                    .map_err(database_error)?
                    == "group"
                {
                    ResourceAccessSourceKind::Group
                } else {
                    ResourceAccessSourceKind::DirectUser
                },
                resource_kind: ResourceKind::Project,
                resource_id: row.try_get("project_id").map_err(database_error)?,
                resource_name: Some(row.try_get("project_name").map_err(database_error)?),
                group_id: row.try_get("group_id").map_err(database_error)?,
                group_name: row.try_get("group_name").map_err(database_error)?,
                permissions: row
                    .try_get::<Json<ResourcePermissions>, _>("permissions_json")
                    .map_err(database_error)?
                    .0,
            })
        })
        .collect()
}
