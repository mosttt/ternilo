use serde_json::json;
use sqlx::Row;
use ternilo_protocol::{HarnessError, TenantId, UserId};
use ternilo_storage::{Json, Transaction, database_error, lock};

use crate::{
    ControlAction, ControlStore, ControlUser, GrantPage, PageQuery, ResourceAction, ResourceKind,
    ResourcePermissions, ShareSubject, SharedGrant, resource_access_in,
    store::{append_audit, from_i64, require_action, to_i64},
};

impl ControlStore {
    pub(crate) async fn set_project_user_share(
        &self,
        actor: &ControlUser,
        tenant: &TenantId,
        project: &str,
        grantee: &UserId,
        permissions: Option<ResourcePermissions>,
        now: u64,
    ) -> Result<(), HarnessError> {
        let mut tx = self.database.tenant_transaction(tenant).await?;
        manage_in(&mut tx, actor, tenant, project, permissions.is_some()).await?;
        grantee.validate()?;
        if let Some(permissions) = permissions {
            require_action(&mut tx, tenant, grantee, ControlAction::TenantRead).await?;
            sqlx::query("INSERT INTO control_project_user_shares (tenant_id,project_id,grantee_user_id,permissions_json,granted_by,created_at_ms,updated_at_ms)
                VALUES ($1,$2,$3,$4,$5,$6,$6) ON CONFLICT (tenant_id,project_id,grantee_user_id) DO UPDATE SET
                permissions_json=EXCLUDED.permissions_json,granted_by=EXCLUDED.granted_by,updated_at_ms=EXCLUDED.updated_at_ms")
                .bind(tenant.as_str()).bind(project).bind(grantee.as_str()).bind(Json(permissions)).bind(actor.user_id.as_str()).bind(to_i64(now,"share timestamp")?)
                .execute(&mut *tx).await.map_err(database_error)?;
        } else {
            sqlx::query("DELETE FROM control_project_user_shares WHERE tenant_id=$1 AND project_id=$2 AND grantee_user_id=$3")
                .bind(tenant.as_str()).bind(project).bind(grantee.as_str()).execute(&mut *tx).await.map_err(database_error)?;
        }
        append_audit(
            &mut tx,
            tenant,
            Some(&actor.user_id),
            "user",
            if permissions.is_some() {
                "project.share"
            } else {
                "project.unshare"
            },
            "project",
            project,
            "success",
            json!({"grantee_user_id":grantee,"permissions":permissions}),
            now,
        )
        .await?;
        tx.commit().await.map_err(database_error)
    }

    pub(crate) async fn set_project_group_share(
        &self,
        actor: &ControlUser,
        tenant: &TenantId,
        project: &str,
        group: &str,
        permissions: Option<ResourcePermissions>,
        now: u64,
    ) -> Result<(), HarnessError> {
        let mut tx = self.database.tenant_transaction(tenant).await?;
        manage_in(&mut tx, actor, tenant, project, permissions.is_some()).await?;
        crate::group_store::group_in(&mut tx, tenant, group).await?;
        if let Some(permissions) = permissions {
            sqlx::query("INSERT INTO control_project_group_shares (tenant_id,project_id,group_id,permissions_json,granted_by,created_at_ms,updated_at_ms)
                VALUES ($1,$2,$3,$4,$5,$6,$6) ON CONFLICT (tenant_id,project_id,group_id) DO UPDATE SET
                permissions_json=EXCLUDED.permissions_json,granted_by=EXCLUDED.granted_by,updated_at_ms=EXCLUDED.updated_at_ms")
                .bind(tenant.as_str()).bind(project).bind(group).bind(Json(permissions)).bind(actor.user_id.as_str()).bind(to_i64(now,"share timestamp")?)
                .execute(&mut *tx).await.map_err(database_error)?;
        } else {
            sqlx::query("DELETE FROM control_project_group_shares WHERE tenant_id=$1 AND project_id=$2 AND group_id=$3")
                .bind(tenant.as_str()).bind(project).bind(group).execute(&mut *tx).await.map_err(database_error)?;
        }
        append_audit(
            &mut tx,
            tenant,
            Some(&actor.user_id),
            "user",
            if permissions.is_some() {
                "project.group_share"
            } else {
                "project.group_unshare"
            },
            "project",
            project,
            "success",
            json!({"group_id":group,"permissions":permissions}),
            now,
        )
        .await?;
        tx.commit().await.map_err(database_error)
    }

    pub(crate) async fn list_project_shares(
        &self,
        actor: &ControlUser,
        tenant: &TenantId,
        project: &str,
        query: &PageQuery,
    ) -> Result<GrantPage, HarnessError> {
        let (pattern, cursor) = query.parameters()?;
        let mut tx = self.database.tenant_transaction(tenant).await?;
        resource_access_in(
            &mut tx,
            &actor.user_id,
            tenant,
            ResourceKind::Project,
            project,
        )
        .await?
        .require(ResourceAction::View)?;
        let rows = sqlx::query("SELECT * FROM (
            SELECT 'user' AS subject_kind,s.grantee_user_id AS subject_id,u.username AS name,s.permissions_json,s.created_at_ms,s.updated_at_ms,
                'user:' || s.grantee_user_id AS page_key,LOWER(u.username || ' ' || u.user_id) AS search_text
            FROM control_project_user_shares s JOIN control_users u ON u.user_id=s.grantee_user_id
            WHERE s.tenant_id=$1 AND s.project_id=$2
            UNION ALL
            SELECT 'group',g.group_id,g.name,s.permissions_json,s.created_at_ms,s.updated_at_ms,'group:' || g.group_id,LOWER(g.name)
            FROM control_project_group_shares s JOIN control_permission_groups g ON g.tenant_id=s.tenant_id AND g.group_id=s.group_id
            WHERE s.tenant_id=$1 AND s.project_id=$2
        ) grants WHERE (CAST($3 AS TEXT) IS NULL OR search_text LIKE $3 ESCAPE '!')
          AND (CAST($4 AS TEXT) IS NULL OR page_key>$4) ORDER BY page_key LIMIT $5")
            .bind(tenant.as_str()).bind(project).bind(pattern).bind(cursor).bind(i64::from(query.limit)+1)
            .fetch_all(&mut *tx).await.map_err(database_error)?;
        let mut shares = Vec::with_capacity(rows.len());
        for row in rows {
            let id: String = row.try_get("subject_id").map_err(database_error)?;
            let subject = if row
                .try_get::<String, _>("subject_kind")
                .map_err(database_error)?
                == "group"
            {
                ShareSubject::Group {
                    group: crate::group_store::group_in(&mut tx, tenant, &id).await?,
                }
            } else {
                ShareSubject::User {
                    user: ControlUser {
                        user_id: UserId::new(id),
                        username: row.try_get("name").map_err(database_error)?,
                    },
                }
            };
            shares.push(SharedGrant {
                subject,
                permissions: row
                    .try_get::<Json<ResourcePermissions>, _>("permissions_json")
                    .map_err(database_error)?
                    .0,
                created_at_ms: from_i64(
                    row.try_get("created_at_ms").map_err(database_error)?,
                    "share timestamp",
                )?,
                updated_at_ms: from_i64(
                    row.try_get("updated_at_ms").map_err(database_error)?,
                    "share timestamp",
                )?,
                inherited: false,
            });
        }
        let next_cursor = query.finish(&mut shares, |share| match &share.subject {
            ShareSubject::User { user } => format!("user:{}", user.user_id),
            ShareSubject::Group { group } => format!("group:{}", group.group_id),
        });
        tx.commit().await.map_err(database_error)?;
        Ok(GrantPage {
            shares,
            next_cursor,
        })
    }
}

async fn manage_in(
    tx: &mut Transaction,
    actor: &ControlUser,
    tenant: &TenantId,
    project: &str,
    granting: bool,
) -> Result<(), HarnessError> {
    lock(tx, &format!("resource-shares:{tenant}:project:{project}")).await?;
    resource_access_in(tx, &actor.user_id, tenant, ResourceKind::Project, project)
        .await?
        .require(ResourceAction::ManageSharing)?;
    if granting {
        require_multi_user(tx).await?;
    }
    Ok(())
}

pub(super) async fn require_multi_user(tx: &mut Transaction) -> Result<(), HarnessError> {
    let mode: String =
        sqlx::query_scalar("SELECT mode FROM control_instance_settings WHERE singleton=1")
            .fetch_one(&mut **tx)
            .await
            .map_err(database_error)?;
    if mode != "multi_user" {
        return Err(HarnessError::policy(
            "multi-user mode is required to share resources",
        ));
    }
    Ok(())
}
