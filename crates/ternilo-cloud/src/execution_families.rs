//! Immutable cooperation identities are independent of visible session and run history.

use crate::store::{database_error, set_tenant, to_i64};
use ternilo_protocol::{HarnessError, SessionId, TenantId, UserId, WorkspaceId};
use ternilo_storage::{Transaction, set_user_scope};

/// Register only a newly inserted session. Reusing a deleted identity is a conflict.
pub(crate) async fn ensure_root_in(
    tx: &mut Transaction,
    tenant: &TenantId,
    session: &SessionId,
    owner: &UserId,
    workspace: &WorkspaceId,
    now: u64,
) -> Result<(), HarnessError> {
    insert_in(tx, tenant, session, owner, workspace, session.as_str(), now).await
}

/// Only the canonical subagent creation transaction may inherit an execution family.
pub(crate) async fn ensure_child_in(
    tx: &mut Transaction,
    tenant: &TenantId,
    session: &SessionId,
    owner: &UserId,
    workspace: &WorkspaceId,
    parent: &SessionId,
    now: u64,
) -> Result<(), HarnessError> {
    let family = family_for_session_in(tx, tenant, parent, owner, workspace).await?;
    insert_in(tx, tenant, session, owner, workspace, &family, now).await
}

async fn insert_in(
    tx: &mut Transaction,
    tenant: &TenantId,
    session: &SessionId,
    owner: &UserId,
    workspace: &WorkspaceId,
    family: &str,
    now: u64,
) -> Result<(), HarnessError> {
    set_tenant(tx, tenant).await?;
    set_user_scope(tx, owner).await?;
    let inserted = sqlx::query(
        "INSERT INTO cloud_execution_families
            (tenant_id,session_id,owner_user_id,workspace_id,family_id,created_at_ms)
         VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT(tenant_id,session_id) DO NOTHING",
    )
    .bind(tenant.as_str())
    .bind(session.as_str())
    .bind(owner.as_str())
    .bind(workspace.as_str())
    .bind(family)
    .bind(to_i64(now, "family creation time")?)
    .execute(&mut **tx)
    .await
    .map_err(database_error)?
    .rows_affected();
    if inserted != 1 {
        return Err(HarnessError::conflict(
            "cloud session identity has already been used",
        ));
    }
    Ok(())
}

pub(crate) async fn family_for_session_in(
    tx: &mut Transaction,
    tenant: &TenantId,
    session: &SessionId,
    owner: &UserId,
    workspace: &WorkspaceId,
) -> Result<String, HarnessError> {
    set_tenant(tx, tenant).await?;
    set_user_scope(tx, owner).await?;
    sqlx::query_scalar(
        "SELECT family_id FROM cloud_execution_families
         WHERE tenant_id=$1 AND session_id=$2 AND owner_user_id=$3 AND workspace_id=$4",
    )
    .bind(tenant.as_str())
    .bind(session.as_str())
    .bind(owner.as_str())
    .bind(workspace.as_str())
    .fetch_optional(&mut **tx)
    .await
    .map_err(database_error)?
    .ok_or_else(|| HarnessError::policy("workspace session family is unavailable"))
}
