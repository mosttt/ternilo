use ternilo_protocol::{HarnessError, SessionId, TenantId, WorkspaceId};
use ternilo_storage::database_error;
use ternilo_transport::ExecutorId;

fn executor_filter(column: &str, executors: Option<&[ExecutorId]>) -> String {
    match executors {
        None => String::new(),
        Some([]) => format!(" AND {column} IS NULL "),
        Some(ids) => {
            let parameters = (3..3 + ids.len())
                .map(|index| format!("${index}"))
                .collect::<Vec<_>>()
                .join(",");
            format!(" AND ({column} IS NULL OR {column} IN ({parameters})) ")
        }
    }
}

use crate::{
    ControlAction, ControlStore, ControlUser, EdgeSessionRecord, ResourceAction, ResourceKind,
    WorkspaceRecord,
    placement_store::edge_session_from_row,
    resource_access_in,
    store::{require_action, workspace_from_row},
};

impl ControlStore {
    pub async fn list_accessible_workspaces(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
    ) -> Result<Vec<WorkspaceRecord>, HarnessError> {
        self.list_accessible_workspaces_filtered(actor, tenant_id, None)
            .await
    }

    pub async fn list_accessible_workspaces_on_executors(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        executors: &[ExecutorId],
    ) -> Result<Vec<WorkspaceRecord>, HarnessError> {
        self.list_accessible_workspaces_filtered(actor, tenant_id, Some(executors))
            .await
    }

    async fn list_accessible_workspaces_filtered(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        executors: Option<&[ExecutorId]>,
    ) -> Result<Vec<WorkspaceRecord>, HarnessError> {
        let mut transaction = self.database.tenant_transaction(tenant_id).await?;
        require_action(
            &mut transaction,
            tenant_id,
            &actor.user_id,
            ControlAction::TenantRead,
        )
        .await?;
        let sql = format!(
            "SELECT w.* FROM control_workspaces w WHERE w.tenant_id = $1 {}
             AND w.unregistered_at_ms IS NULL AND (w.owner_user_id = $2 OR EXISTS (
                 SELECT 1 FROM control_resource_ownership o WHERE o.tenant_id=w.tenant_id
                 AND o.resource_kind='workspace' AND o.resource_id=w.workspace_id AND o.owner_user_id=$2) OR EXISTS (
                 SELECT 1 FROM control_project_workspace_access p WHERE p.tenant_id=w.tenant_id
                 AND p.workspace_id=w.workspace_id AND p.user_id=$2) OR EXISTS (
                 SELECT 1 FROM control_resource_shares s WHERE s.tenant_id = w.tenant_id
                 AND s.resource_kind = 'workspace' AND s.resource_id = w.workspace_id
                 AND s.grantee_user_id = $2) OR EXISTS (
                 SELECT 1 FROM control_resource_group_shares gs JOIN control_permission_group_members gm
                 ON gm.tenant_id=gs.tenant_id AND gm.group_id=gs.group_id AND gm.user_id=$2
                 WHERE gs.tenant_id=w.tenant_id AND gs.resource_kind='workspace' AND gs.resource_id=w.workspace_id))
             ORDER BY w.created_at_ms, w.workspace_id", executor_filter("w.executor_id",executors)
        );
        let mut query = sqlx::query(sqlx::AssertSqlSafe(sql.as_str()))
            .bind(tenant_id.as_str())
            .bind(actor.user_id.as_str());
        for executor in executors.unwrap_or_default() {
            query = query.bind(executor.as_str());
        }
        let rows = query
            .fetch_all(&mut *transaction)
            .await
            .map_err(database_error)?;
        let mut workspaces = Vec::new();
        for row in &rows {
            let workspace = workspace_from_row(row)?;
            let access = resource_access_in(
                &mut transaction,
                &actor.user_id,
                tenant_id,
                ResourceKind::Workspace,
                workspace.workspace_id.as_str(),
            )
            .await?;
            if access.permissions.view {
                workspaces.push(workspace);
            }
        }
        transaction.commit().await.map_err(database_error)?;
        Ok(workspaces)
    }

    pub async fn resolve_accessible_workspace(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        workspace_id: &WorkspaceId,
    ) -> Result<WorkspaceRecord, HarnessError> {
        let mut transaction = self.database.tenant_transaction(tenant_id).await?;
        resource_access_in(
            &mut transaction,
            &actor.user_id,
            tenant_id,
            ResourceKind::Workspace,
            workspace_id.as_str(),
        )
        .await?
        .require(ResourceAction::View)?;
        let row = sqlx::query("SELECT * FROM control_workspaces WHERE tenant_id = $1 AND workspace_id = $2 AND unregistered_at_ms IS NULL")
            .bind(tenant_id.as_str()).bind(workspace_id.as_str()).fetch_optional(&mut *transaction)
            .await.map_err(database_error)?.ok_or_else(|| HarnessError::invalid("workspace does not exist"))?;
        transaction.commit().await.map_err(database_error)?;
        workspace_from_row(&row)
    }

    /// Resolve only the binding belonging to the authorized session, including
    /// unregistered workspaces and individually shared sessions.
    pub async fn resolve_accessible_session_workspace(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        session_id: &SessionId,
    ) -> Result<WorkspaceRecord, HarnessError> {
        let mut transaction = self.database.tenant_transaction(tenant_id).await?;
        resource_access_in(
            &mut transaction,
            &actor.user_id,
            tenant_id,
            ResourceKind::Session,
            session_id.as_str(),
        )
        .await?
        .require(ResourceAction::View)?;
        let edge_workspace = sqlx::query_scalar::<_, String>(
            "SELECT workspace_id FROM control_edge_sessions WHERE tenant_id=$1 AND session_id=$2",
        )
        .bind(tenant_id.as_str())
        .bind(session_id.as_str())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?;
        let workspace_id = if let Some(workspace_id) = edge_workspace {
            workspace_id
        } else {
            sqlx::query_scalar::<_, String>(
                "SELECT workspace_id FROM cloud_sessions WHERE tenant_id=$1 AND session_id=$2",
            )
            .bind(tenant_id.as_str())
            .bind(session_id.as_str())
            .fetch_one(&mut *transaction)
            .await
            .map_err(database_error)?
        };
        let row =
            sqlx::query("SELECT * FROM control_workspaces WHERE tenant_id=$1 AND workspace_id=$2")
                .bind(tenant_id.as_str())
                .bind(workspace_id)
                .fetch_one(&mut *transaction)
                .await
                .map_err(database_error)?;
        transaction.commit().await.map_err(database_error)?;
        workspace_from_row(&row)
    }

    pub async fn list_accessible_edge_sessions(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
    ) -> Result<Vec<EdgeSessionRecord>, HarnessError> {
        self.list_accessible_edge_sessions_filtered(actor, tenant_id, None)
            .await
    }

    pub async fn list_accessible_edge_sessions_on_executors(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        executors: &[ExecutorId],
    ) -> Result<Vec<EdgeSessionRecord>, HarnessError> {
        self.list_accessible_edge_sessions_filtered(actor, tenant_id, Some(executors))
            .await
    }

    async fn list_accessible_edge_sessions_filtered(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        executors: Option<&[ExecutorId]>,
    ) -> Result<Vec<EdgeSessionRecord>, HarnessError> {
        let mut transaction = self.database.tenant_transaction(tenant_id).await?;
        require_action(
            &mut transaction,
            tenant_id,
            &actor.user_id,
            ControlAction::TenantRead,
        )
        .await?;
        let sql = format!(
            "SELECT e.* FROM control_edge_sessions e WHERE e.tenant_id = $1 {}
             AND (e.owner_user_id = $2 OR EXISTS (
                 SELECT 1 FROM control_resource_ownership o WHERE o.tenant_id=e.tenant_id AND o.owner_user_id=$2
                 AND ((o.resource_kind='session' AND o.resource_id=e.session_id)
                  OR (o.resource_kind='workspace' AND o.resource_id=e.workspace_id))) OR EXISTS (
                 SELECT 1 FROM control_project_workspace_access p WHERE p.tenant_id=e.tenant_id
                 AND p.workspace_id=e.workspace_id AND p.user_id=$2) OR EXISTS (
                 SELECT 1 FROM control_resource_shares s WHERE s.tenant_id = e.tenant_id
                 AND s.grantee_user_id = $2 AND ((s.resource_kind = 'session' AND s.resource_id = e.session_id)
                 OR (s.resource_kind = 'workspace' AND s.resource_id = e.workspace_id))) OR EXISTS (
                 SELECT 1 FROM control_resource_group_shares gs JOIN control_permission_group_members gm
                 ON gm.tenant_id=gs.tenant_id AND gm.group_id=gs.group_id AND gm.user_id=$2
                 WHERE gs.tenant_id=e.tenant_id AND ((gs.resource_kind='session' AND gs.resource_id=e.session_id)
                 OR (gs.resource_kind='workspace' AND gs.resource_id=e.workspace_id))) OR EXISTS (
                 SELECT 1 FROM control_resource_fork_group_sources f WHERE f.tenant_id=e.tenant_id AND f.session_id=e.session_id AND f.user_id=$2))
             ORDER BY e.updated_at_ms DESC, e.session_id", executor_filter("e.executor_id",executors)
        );
        let mut query = sqlx::query(sqlx::AssertSqlSafe(sql.as_str()))
            .bind(tenant_id.as_str())
            .bind(actor.user_id.as_str());
        for executor in executors.unwrap_or_default() {
            query = query.bind(executor.as_str());
        }
        let rows = query
            .fetch_all(&mut *transaction)
            .await
            .map_err(database_error)?;
        let mut sessions = Vec::new();
        for row in &rows {
            let session = edge_session_from_row(row)?;
            let access = resource_access_in(
                &mut transaction,
                &actor.user_id,
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

    pub async fn find_accessible_edge_session(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        session_id: &SessionId,
    ) -> Result<Option<EdgeSessionRecord>, HarnessError> {
        session_id.validate()?;
        let mut transaction = self.database.tenant_transaction(tenant_id).await?;
        require_action(
            &mut transaction,
            tenant_id,
            &actor.user_id,
            ControlAction::TenantRead,
        )
        .await?;
        let row = sqlx::query(
            "SELECT * FROM control_edge_sessions WHERE tenant_id = $1 AND session_id = $2",
        )
        .bind(tenant_id.as_str())
        .bind(session_id.as_str())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?;
        let record = if let Some(row) = row {
            let access = resource_access_in(
                &mut transaction,
                &actor.user_id,
                tenant_id,
                ResourceKind::Session,
                session_id.as_str(),
            )
            .await?;
            if access.permissions.view {
                Some(edge_session_from_row(&row)?)
            } else {
                None
            }
        } else {
            None
        };
        transaction.commit().await.map_err(database_error)?;
        Ok(record)
    }
}
