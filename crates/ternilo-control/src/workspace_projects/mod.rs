use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::Row;
use ternilo_protocol::{HarnessError, TenantId, WorkspaceId};
use ternilo_storage::{Database, Transaction, database_error, lock};

use crate::{
    ControlStore, ControlUser, ResourceAction, ResourceKind, resource_access_in,
    store::{append_audit, from_i64, to_i64},
    types::require_bounded,
};

#[derive(Clone, Debug, Serialize)]
pub struct WorkspaceProject {
    pub project_id: String,
    pub project_name: String,
    pub execution_project_id: String,
    pub revision: u64,
    pub inherit_project_sharing: bool,
    pub can_move: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceProjectMove {
    pub project_id: String,
    pub expected_project_id: String,
    pub expected_revision: u64,
    pub inherit_project_sharing: bool,
}

pub(crate) async fn initialize(database: &Database) -> Result<(), HarnessError> {
    database
        .initialize(
            "workspace_projects",
            1,
            include_str!("schema.sql"),
            include_str!("postgres.sql"),
        )
        .await
}

pub(crate) async fn project_in(
    tx: &mut Transaction,
    tenant: &TenantId,
    workspace: &str,
) -> Result<String, HarnessError> {
    sqlx::query_scalar(
        "SELECT COALESCE(m.project_id,w.project_id) FROM control_workspaces w
         LEFT JOIN control_workspace_projects m ON m.tenant_id=w.tenant_id AND m.workspace_id=w.workspace_id
         WHERE w.tenant_id=$1 AND w.workspace_id=$2",
    )
    .bind(tenant.as_str())
    .bind(workspace)
    .fetch_optional(&mut **tx)
    .await
    .map_err(database_error)?
    .ok_or_else(|| HarnessError::invalid("workspace does not exist"))
}

async fn snapshot_in(
    tx: &mut Transaction,
    tenant: &TenantId,
    workspace: &WorkspaceId,
    can_move: bool,
) -> Result<WorkspaceProject, HarnessError> {
    let row = sqlx::query(
        "SELECT COALESCE(m.project_id,w.project_id) AS project_id,p.name,
            w.project_id AS execution_project_id,COALESCE(m.revision,0) AS revision,
            CAST(EXISTS(SELECT 1 FROM control_workspace_project_sharing s
                WHERE s.tenant_id=w.tenant_id AND s.workspace_id=w.workspace_id) AS INTEGER) AS inherit_sharing
         FROM control_workspaces w
         LEFT JOIN control_workspace_projects m ON m.tenant_id=w.tenant_id AND m.workspace_id=w.workspace_id
         JOIN control_projects p ON p.tenant_id=w.tenant_id AND p.project_id=COALESCE(m.project_id,w.project_id)
         WHERE w.tenant_id=$1 AND w.workspace_id=$2 AND w.unregistered_at_ms IS NULL",
    )
    .bind(tenant.as_str())
    .bind(workspace.as_str())
    .fetch_optional(&mut **tx)
    .await
    .map_err(database_error)?
    .ok_or_else(|| HarnessError::conflict("workspace is no longer registered"))?;
    Ok(WorkspaceProject {
        project_id: row.try_get("project_id").map_err(database_error)?,
        project_name: row.try_get("name").map_err(database_error)?,
        execution_project_id: row
            .try_get("execution_project_id")
            .map_err(database_error)?,
        revision: from_i64(
            row.try_get("revision").map_err(database_error)?,
            "project revision",
        )?,
        inherit_project_sharing: row
            .try_get::<i64, _>("inherit_sharing")
            .map_err(database_error)?
            != 0,
        can_move,
    })
}

impl ControlStore {
    /// Project assignments are joined only onto resources already authorized by the caller.
    pub async fn workspace_management_projects(
        &self,
        tenant: &TenantId,
    ) -> Result<BTreeMap<WorkspaceId, String>, HarnessError> {
        let mut tx = self.database.tenant_read_transaction(tenant).await?;
        let rows = sqlx::query(
            "SELECT workspace_id,project_id FROM control_workspace_projects WHERE tenant_id=$1",
        )
        .bind(tenant.as_str())
        .fetch_all(&mut *tx)
        .await
        .map_err(database_error)?;
        let result = rows
            .into_iter()
            .map(|row| {
                Ok((
                    WorkspaceId::new(
                        row.try_get::<String, _>("workspace_id")
                            .map_err(database_error)?,
                    ),
                    row.try_get("project_id").map_err(database_error)?,
                ))
            })
            .collect();
        tx.commit().await.map_err(database_error)?;
        result
    }

    pub async fn workspace_project(
        &self,
        actor: &ControlUser,
        tenant: &TenantId,
        workspace: &WorkspaceId,
    ) -> Result<WorkspaceProject, HarnessError> {
        let mut tx = self.database.tenant_read_transaction(tenant).await?;
        let access = resource_access_in(
            &mut tx,
            &actor.user_id,
            tenant,
            ResourceKind::Workspace,
            workspace.as_str(),
        )
        .await?;
        access.require(ResourceAction::View)?;
        let result = snapshot_in(
            &mut tx,
            tenant,
            workspace,
            access.is_owner && access.permissions.configure,
        )
        .await?;
        tx.commit().await.map_err(database_error)?;
        Ok(result)
    }

    pub async fn move_workspace_project(
        &self,
        actor: &ControlUser,
        tenant: &TenantId,
        workspace: &WorkspaceId,
        input: &WorkspaceProjectMove,
        now_ms: u64,
    ) -> Result<WorkspaceProject, HarnessError> {
        workspace.validate()?;
        require_bounded(&input.project_id, "target project id", 128)?;
        require_bounded(&input.expected_project_id, "previous project id", 128)?;
        let mut tx = self.database.tenant_transaction(tenant).await?;
        lock(&mut tx, &format!("ternilo:project-delete:{tenant}")).await?;
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
        access.require(ResourceAction::Delete)?;
        let previous = snapshot_in(&mut tx, tenant, workspace, true).await?;
        if previous.project_id != input.expected_project_id
            || previous.revision != input.expected_revision
        {
            return Err(HarnessError::conflict(
                "workspace project changed; refresh before moving",
            ));
        }
        if previous.project_id == input.project_id {
            return Err(HarnessError::invalid("choose a different project"));
        }
        let target: Option<String> = sqlx::query_scalar(
            "SELECT name FROM control_projects WHERE tenant_id=$1 AND project_id=$2",
        )
        .bind(tenant.as_str())
        .bind(&input.project_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(database_error)?;
        if target.is_none() {
            return Err(HarnessError::invalid(
                "target project does not exist in this space",
            ));
        }
        let name: String = sqlx::query_scalar(
            "SELECT name FROM control_workspaces WHERE tenant_id=$1 AND workspace_id=$2",
        )
        .bind(tenant.as_str())
        .bind(workspace.as_str())
        .fetch_one(&mut *tx)
        .await
        .map_err(database_error)?;
        crate::resource_ownership::require_workspace_name_in(
            &mut tx,
            tenant,
            &access.owner_user_id,
            &input.project_id,
            &name,
            Some(workspace.as_str()),
        )
        .await?;
        let revision = previous
            .revision
            .checked_add(1)
            .ok_or_else(|| HarnessError::conflict("workspace project revision exhausted"))?;
        sqlx::query("INSERT INTO control_workspace_projects (tenant_id,workspace_id,project_id,revision,updated_at_ms)
            VALUES ($1,$2,$3,$4,$5) ON CONFLICT (tenant_id,workspace_id) DO UPDATE SET
            project_id=EXCLUDED.project_id,revision=EXCLUDED.revision,updated_at_ms=EXCLUDED.updated_at_ms")
            .bind(tenant.as_str()).bind(workspace.as_str()).bind(&input.project_id)
            .bind(to_i64(revision,"project revision")?).bind(to_i64(now_ms,"project move timestamp")?)
            .execute(&mut *tx).await.map_err(database_error)?;
        crate::project_sharing::set_inheritance_in(
            &mut tx,
            actor,
            tenant,
            workspace,
            input.inherit_project_sharing,
            now_ms,
        )
        .await?;
        append_audit(&mut tx, tenant, Some(&actor.user_id), "user", "workspace.move_project", "workspace", workspace.as_str(), "success",
            json!({"previous_project_id":previous.project_id,"project_id":input.project_id,
                "execution_project_id":previous.execution_project_id,"previous_revision":previous.revision,
                "revision":revision,"inherit_project_sharing":input.inherit_project_sharing}), now_ms).await?;
        let result = snapshot_in(&mut tx, tenant, workspace, true).await?;
        tx.commit().await.map_err(database_error)?;
        Ok(result)
    }
}
