use super::{
    ControlAction, ControlStore, ControlUser, HarnessError, ProjectRecord, Row, TenantId,
    append_audit, database_error, from_i64, json, require_action, require_bounded, set_tenant,
};

impl ControlStore {
    pub async fn rename_project(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        project_id: &str,
        name: &str,
        now_ms: u64,
    ) -> Result<ProjectRecord, HarnessError> {
        tenant_id.validate()?;
        require_bounded(project_id, "project id", 128)?;
        require_bounded(name, "project name", 256)?;
        let name = name.trim();
        let mut transaction = self.database.begin().await?;
        set_tenant(&mut transaction, tenant_id).await?;
        require_action(
            &mut transaction,
            tenant_id,
            &actor.user_id,
            ControlAction::ProjectManage,
        )
        .await?;
        let row = sqlx::query(
            "UPDATE control_projects SET name = $3
             WHERE tenant_id = $1 AND project_id = $2 RETURNING created_at_ms",
        )
        .bind(tenant_id.as_str())
        .bind(project_id)
        .bind(name)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?
        .ok_or_else(|| HarnessError::invalid("project does not exist"))?;
        let created_at_ms = from_i64(
            row.try_get("created_at_ms").map_err(database_error)?,
            "project timestamp",
        )?;
        append_audit(
            &mut transaction,
            tenant_id,
            Some(&actor.user_id),
            "user",
            "project.rename",
            "project",
            project_id,
            "success",
            json!({ "name": name }),
            now_ms,
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(ProjectRecord {
            tenant_id: tenant_id.clone(),
            project_id: project_id.to_owned(),
            name: name.to_owned(),
            created_at_ms,
        })
    }

    pub async fn delete_project(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        project_id: &str,
        now_ms: u64,
    ) -> Result<(), HarnessError> {
        tenant_id.validate()?;
        require_bounded(project_id, "project id", 128)?;
        let mut transaction = self.database.begin().await?;
        set_tenant(&mut transaction, tenant_id).await?;
        require_action(
            &mut transaction,
            tenant_id,
            &actor.user_id,
            ControlAction::ProjectManage,
        )
        .await?;
        ternilo_storage::lock(
            &mut transaction,
            &format!("ternilo:project-delete:{tenant_id}"),
        )
        .await?;
        let name = sqlx::query_scalar::<_, String>(ternilo_storage::for_update(
            &transaction,
            "SELECT name FROM control_projects WHERE tenant_id = $1 AND project_id = $2",
            "SELECT name FROM control_projects WHERE tenant_id = $1 AND project_id = $2 FOR UPDATE",
        ))
        .bind(tenant_id.as_str())
        .bind(project_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?
        .ok_or_else(|| HarnessError::invalid("project does not exist"))?;
        ensure_project_deletable(&mut transaction, tenant_id, project_id).await?;
        sqlx::query("DELETE FROM control_projects WHERE tenant_id = $1 AND project_id = $2")
            .bind(tenant_id.as_str())
            .bind(project_id)
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
        append_audit(
            &mut transaction,
            tenant_id,
            Some(&actor.user_id),
            "user",
            "project.delete",
            "project",
            project_id,
            "success",
            json!({ "name": name }),
            now_ms,
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(())
    }
}

async fn ensure_project_deletable(
    transaction: &mut ternilo_storage::Transaction,
    tenant_id: &TenantId,
    project_id: &str,
) -> Result<(), HarnessError> {
    crate::model_store::scope(transaction).await?;
    let reason = sqlx::query_scalar::<_, String>(
        "SELECT CASE
            WHEN EXISTS (SELECT 1 FROM control_account_spaces
                WHERE personal_tenant_id = $1 AND default_project_id = $2)
                THEN 'personal default project cannot be deleted'
            WHEN (SELECT COUNT(*) FROM control_projects WHERE tenant_id = $1) <= 1
                THEN 'the last project in a space cannot be deleted'
            WHEN EXISTS (SELECT 1 FROM control_workspaces WHERE tenant_id = $1 AND project_id = $2)
                OR EXISTS (SELECT 1 FROM control_executor_enrollments WHERE tenant_id = $1 AND project_id = $2)
                OR EXISTS (SELECT 1 FROM control_executors WHERE tenant_id = $1 AND project_id = $2)
                OR EXISTS (SELECT 1 FROM control_secrets WHERE tenant_id = $1 AND project_id = $2)
                OR EXISTS (SELECT 1 FROM control_secret_heads WHERE tenant_id = $1 AND project_id = $2)
                OR EXISTS (SELECT 1 FROM control_model_requests WHERE tenant_id = $1 AND project_id = $2)
                THEN 'project has associated resources and cannot be deleted'
            ELSE '' END",
    )
    .bind(tenant_id.as_str())
    .bind(project_id)
    .fetch_one(&mut **transaction)
    .await
    .map_err(database_error)?;
    if reason.is_empty() {
        Ok(())
    } else {
        Err(HarnessError::conflict(reason))
    }
}
