use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::Row;
use ternilo_protocol::{HarnessError, TenantId, UserId};
use ternilo_storage::{Backend, Database, Transaction, database_error, lock};

use crate::{
    CandidatePage, ControlAction, ControlStore, ControlUser, PageQuery, ResourceAction,
    ResourceKind, ShareSubject, resource_access_in,
    store::{append_audit, from_i64, require_action, to_i64},
};

#[derive(Clone, Debug, Serialize)]
pub struct ResourceOwnership {
    pub owner: ControlUser,
    pub revision: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceOwnershipTransfer {
    pub owner_user_id: UserId,
    pub expected_owner_user_id: UserId,
    pub expected_revision: u64,
    pub retain_previous_owner: bool,
}

pub(crate) async fn initialize(database: &Database) -> Result<(), HarnessError> {
    database
        .initialize(
            "resource_ownership",
            1,
            match database.backend() {
                Backend::Sqlite => concat!(
                    include_str!("schema.sql"),
                    include_str!("sqlite_cleanup.sql")
                ),
                Backend::Postgres => include_str!("schema.sql"),
            },
            include_str!("postgres_access.sql"),
        )
        .await
}

/// Management can change; canonical storage, input authors and execution do not.
pub(crate) async fn owner_in(
    tx: &mut Transaction,
    tenant: &TenantId,
    kind: ResourceKind,
    id: &str,
    storage_owner: &UserId,
    workspace: Option<&str>,
) -> Result<(UserId, u64), HarnessError> {
    let row = sqlx::query(
        "SELECT owner_user_id,revision FROM control_resource_ownership
         WHERE tenant_id=$1 AND resource_kind=$2 AND resource_id=$3",
    )
    .bind(tenant.as_str())
    .bind(kind.as_str())
    .bind(id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(database_error)?;
    let row = if row.is_some() {
        row
    } else if let Some(workspace) = workspace {
        // Independent session ownership takes precedence over workspace inheritance.
        sqlx::query(
            "SELECT o.owner_user_id,o.revision FROM control_resource_ownership o
             JOIN control_workspaces w ON w.tenant_id=o.tenant_id AND w.workspace_id=o.resource_id
             WHERE o.tenant_id=$1 AND o.resource_kind='workspace' AND o.resource_id=$2
               AND w.owner_user_id=$3",
        )
        .bind(tenant.as_str())
        .bind(workspace)
        .bind(storage_owner.as_str())
        .fetch_optional(&mut **tx)
        .await
        .map_err(database_error)?
    } else {
        None
    };
    match row {
        Some(row) => Ok((
            UserId::new(
                row.try_get::<String, _>("owner_user_id")
                    .map_err(database_error)?,
            ),
            from_i64(
                row.try_get("revision").map_err(database_error)?,
                "ownership revision",
            )?,
        )),
        None => Ok((storage_owner.clone(), 0)),
    }
}

pub(crate) async fn require_workspace_name_in(
    tx: &mut Transaction,
    tenant: &TenantId,
    owner: &UserId,
    project: &str,
    name: &str,
    excluded: Option<&str>,
) -> Result<(), HarnessError> {
    lock(tx, &format!("workspace-name:{tenant}:{owner}:{project}")).await?;
    let conflict: i64 = sqlx::query_scalar(
        "SELECT CAST(EXISTS(SELECT 1 FROM control_workspaces w
         LEFT JOIN control_resource_ownership o ON o.tenant_id=w.tenant_id
           AND o.resource_kind='workspace' AND o.resource_id=w.workspace_id
         WHERE w.tenant_id=$1 AND w.project_id=$2 AND w.name=$3
           AND w.unregistered_at_ms IS NULL AND COALESCE(o.owner_user_id,w.owner_user_id)=$4
           AND (CAST($5 AS TEXT) IS NULL OR w.workspace_id<>$5)) AS INTEGER)",
    )
    .bind(tenant.as_str())
    .bind(project)
    .bind(name)
    .bind(owner.as_str())
    .bind(excluded)
    .fetch_one(&mut **tx)
    .await
    .map_err(database_error)?;
    if conflict != 0 {
        Err(HarnessError::conflict(
            "workspace name already exists in this project",
        ))
    } else {
        Ok(())
    }
}

impl ControlStore {
    pub async fn resource_transfer_candidates(
        &self,
        actor: &ControlUser,
        tenant: &TenantId,
        kind: ResourceKind,
        id: &str,
        query: &PageQuery,
    ) -> Result<CandidatePage, HarnessError> {
        if kind == ResourceKind::Project {
            return Err(HarnessError::invalid(
                "projects are managed through space roles",
            ));
        }
        let (pattern, cursor) = query.parameters()?;
        let mut tx = self.database.tenant_transaction(tenant).await?;
        crate::account_store::require_team_in(&mut tx, tenant).await?;
        resource_access_in(&mut tx, &actor.user_id, tenant, kind, id)
            .await?
            .require(ResourceAction::Delete)?;
        let rows = sqlx::query(
            "SELECT m.user_id,u.username FROM control_memberships m JOIN control_users u ON u.user_id=m.user_id
             WHERE m.tenant_id=$1 AND m.user_id<>$2 AND m.role IN ('member','admin','owner') AND u.status='active'
               AND (CAST($3 AS TEXT) IS NULL OR LOWER(m.user_id) LIKE $3 ESCAPE '!' OR LOWER(u.username) LIKE $3 ESCAPE '!')
               AND (CAST($4 AS TEXT) IS NULL OR m.user_id>$4) ORDER BY m.user_id LIMIT $5",
        ).bind(tenant.as_str()).bind(actor.user_id.as_str()).bind(pattern).bind(cursor)
            .bind(i64::from(query.limit)+1).fetch_all(&mut *tx).await.map_err(database_error)?;
        let mut users = rows
            .iter()
            .map(|row| {
                Ok(ControlUser {
                    user_id: UserId::new(
                        row.try_get::<String, _>("user_id")
                            .map_err(database_error)?,
                    ),
                    username: row.try_get("username").map_err(database_error)?,
                })
            })
            .collect::<Result<Vec<_>, HarnessError>>()?;
        let next_cursor = query.finish(&mut users, |user| user.user_id.as_str().to_owned());
        tx.commit().await.map_err(database_error)?;
        Ok(CandidatePage {
            candidates: users
                .into_iter()
                .map(|user| ShareSubject::User { user })
                .collect(),
            next_cursor,
        })
    }

    pub async fn resource_ownership(
        &self,
        actor: &ControlUser,
        tenant: &TenantId,
        kind: ResourceKind,
        id: &str,
    ) -> Result<ResourceOwnership, HarnessError> {
        let mut tx = self.database.tenant_transaction(tenant).await?;
        let access = resource_access_in(&mut tx, &actor.user_id, tenant, kind, id).await?;
        access.require(ResourceAction::View)?;
        let result = snapshot_in(&mut tx, &access.owner_user_id, access.ownership_revision).await?;
        tx.commit().await.map_err(database_error)?;
        Ok(result)
    }

    #[expect(
        clippy::too_many_lines,
        reason = "Commit ownership, recipient checks and audit atomically."
    )]
    pub async fn transfer_resource_ownership(
        &self,
        actor: &ControlUser,
        tenant: &TenantId,
        kind: ResourceKind,
        id: &str,
        input: &ResourceOwnershipTransfer,
        now_ms: u64,
    ) -> Result<ResourceOwnership, HarnessError> {
        if kind == ResourceKind::Project {
            return Err(HarnessError::invalid(
                "project management belongs to space roles",
            ));
        }
        input.owner_user_id.validate()?;
        input.expected_owner_user_id.validate()?;
        let mut tx = self.database.tenant_transaction(tenant).await?;
        lock(
            &mut tx,
            &format!("resource-shares:{tenant}:{}:{id}", kind.as_str()),
        )
        .await?;
        if kind == ResourceKind::Session {
            let (_, workspace) =
                crate::sharing_store::resource_owner(&mut tx, tenant, kind, id).await?;
            if let Some(workspace) = workspace {
                // Serialize inherited ownership checks with workspace handoff.
                lock(
                    &mut tx,
                    &format!("resource-shares:{tenant}:workspace:{workspace}"),
                )
                .await?;
            }
        }
        crate::account_store::require_team_in(&mut tx, tenant).await?;
        let access = resource_access_in(&mut tx, &actor.user_id, tenant, kind, id).await?;
        access.require(ResourceAction::View)?;
        if access.owner_user_id != input.expected_owner_user_id
            || access.ownership_revision != input.expected_revision
        {
            return Err(HarnessError::conflict(
                "resource ownership changed; refresh before transferring",
            ));
        }
        access.require(ResourceAction::Delete)?;
        if input.owner_user_id == access.owner_user_id {
            return Err(HarnessError::invalid("choose a different resource owner"));
        }
        require_action(
            &mut tx,
            tenant,
            &input.owner_user_id,
            ControlAction::RunReserve,
        )
        .await?;
        let mode: String =
            sqlx::query_scalar("SELECT mode FROM control_instance_settings WHERE singleton=1")
                .fetch_one(&mut *tx)
                .await
                .map_err(database_error)?;
        if mode != "multi_user" {
            return Err(HarnessError::policy(
                "multi-user mode is required to transfer resources",
            ));
        }
        if kind == ResourceKind::Workspace {
            let workspace = sqlx::query("SELECT project_id,name FROM control_workspaces WHERE tenant_id=$1 AND workspace_id=$2 AND unregistered_at_ms IS NULL")
                .bind(tenant.as_str()).bind(id).fetch_optional(&mut *tx).await.map_err(database_error)?
                .ok_or_else(|| HarnessError::conflict("workspace is no longer registered"))?;
            require_workspace_name_in(
                &mut tx,
                tenant,
                &input.owner_user_id,
                &workspace
                    .try_get::<String, _>("project_id")
                    .map_err(database_error)?,
                &workspace
                    .try_get::<String, _>("name")
                    .map_err(database_error)?,
                Some(id),
            )
            .await?;
        }
        let revision = access
            .ownership_revision
            .checked_add(1)
            .ok_or_else(|| HarnessError::conflict("ownership revision exhausted"))?;
        sqlx::query(
            "INSERT INTO control_resource_ownership
             (tenant_id,resource_kind,resource_id,owner_user_id,revision,updated_at_ms)
             VALUES ($1,$2,$3,$4,$5,$6)
             ON CONFLICT (tenant_id,resource_kind,resource_id) DO UPDATE SET
             owner_user_id=EXCLUDED.owner_user_id,revision=EXCLUDED.revision,updated_at_ms=EXCLUDED.updated_at_ms",
        ).bind(tenant.as_str()).bind(kind.as_str()).bind(id).bind(input.owner_user_id.as_str())
            .bind(to_i64(revision,"ownership revision")?).bind(to_i64(now_ms,"ownership timestamp")?)
            .execute(&mut *tx).await.map_err(database_error)?;
        if input.retain_previous_owner {
            sqlx::query("INSERT INTO control_resource_shares
                (tenant_id,resource_kind,resource_id,grantee_user_id,permissions_json,granted_by,created_at_ms,updated_at_ms)
                VALUES ($1,$2,$3,$4,$5,$4,$6,$6)
                ON CONFLICT (tenant_id,resource_kind,resource_id,grantee_user_id) DO UPDATE SET
                permissions_json=EXCLUDED.permissions_json,granted_by=EXCLUDED.granted_by,updated_at_ms=EXCLUDED.updated_at_ms")
                .bind(tenant.as_str()).bind(kind.as_str()).bind(id).bind(actor.user_id.as_str())
                .bind(ternilo_storage::Json(crate::ResourcePermissions::OWNER))
                .bind(to_i64(now_ms,"share timestamp")?).execute(&mut *tx).await.map_err(database_error)?;
        }
        append_audit(
            &mut tx,
            tenant,
            Some(&actor.user_id),
            "user",
            "resource.transfer",
            kind.as_str(),
            id,
            "success",
            json!({"previous_owner_user_id":access.owner_user_id,
                "owner_user_id":input.owner_user_id,"storage_user_id":access.storage_user_id,
                "previous_revision":access.ownership_revision,"revision":revision,
                "retain_previous_owner":input.retain_previous_owner}),
            now_ms,
        )
        .await?;
        let result = snapshot_in(&mut tx, &input.owner_user_id, revision).await?;
        tx.commit().await.map_err(database_error)?;
        Ok(result)
    }
}

async fn snapshot_in(
    tx: &mut Transaction,
    owner: &UserId,
    revision: u64,
) -> Result<ResourceOwnership, HarnessError> {
    let username: String =
        sqlx::query_scalar("SELECT username FROM control_users WHERE user_id=$1")
            .bind(owner.as_str())
            .fetch_one(&mut **tx)
            .await
            .map_err(database_error)?;
    Ok(ResourceOwnership {
        owner: ControlUser {
            user_id: owner.clone(),
            username,
        },
        revision,
    })
}
