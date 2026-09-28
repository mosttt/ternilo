use super::{
    HarnessError, Row, TenantId, Transaction, WorkspaceId, database_error, set_tenant, to_i64,
};

pub(crate) struct WorkspaceOccupant<'a> {
    pub family_id: &'a str,
    pub worker_id: &'a str,
    pub worker_generation: u64,
}

/// The caller holds the storage gate until both the epoch and its member are committed.
pub(crate) async fn admit_epoch_in(
    tx: &mut Transaction,
    storage: &str,
    tenant: &TenantId,
    workspace: &WorkspaceId,
    occupant: WorkspaceOccupant<'_>,
    now: u64,
) -> Result<Option<u64>, HarnessError> {
    set_tenant(tx, tenant).await?;
    let row = sqlx::query(
        "SELECT MAX(occupation_epoch) AS occupation_epoch,
            COALESCE(SUM(CASE WHEN family_id<>$4 OR worker_id<>$5 OR worker_generation<>$6
                OR state<>'held' OR NOT EXISTS (
                    SELECT 1 FROM cloud_runs run WHERE run.tenant_id=occupancy.tenant_id
                    AND run.run_id=occupancy.run_id AND run.lease_token=occupancy.lease_token
                    AND run.lease_owner=occupancy.worker_id AND run.lease_expires_at_ms>$7
                    AND run.state IN ('leased','running','cancel_requested'))
                THEN 1 ELSE 0 END),0) AS conflicts
         FROM cloud_workspace_occupancy occupancy
         WHERE storage_id=$1 AND tenant_id=$2 AND workspace_id=$3 AND state<>'released'",
    )
    .bind(storage)
    .bind(tenant.as_str())
    .bind(workspace.as_str())
    .bind(occupant.family_id)
    .bind(occupant.worker_id)
    .bind(to_i64(occupant.worker_generation, "worker generation")?)
    .bind(to_i64(now, "occupation admission time")?)
    .fetch_one(&mut **tx)
    .await
    .map_err(database_error)?;
    if row.try_get::<i64, _>("conflicts").map_err(database_error)? != 0 {
        return Ok(None);
    }
    let existing: Option<i64> = row.try_get("occupation_epoch").map_err(database_error)?;
    let epoch = match existing {
        Some(epoch) => epoch,
        None => sqlx::query_scalar(
            "INSERT INTO cloud_workspace_epochs(storage_id,tenant_id,workspace_id,occupation_epoch)
             VALUES($1,$2,$3,1) ON CONFLICT(storage_id,tenant_id,workspace_id) DO UPDATE
             SET occupation_epoch=cloud_workspace_epochs.occupation_epoch+1
             WHERE cloud_workspace_epochs.occupation_epoch<9223372036854775807
             RETURNING occupation_epoch",
        )
        .bind(storage)
        .bind(tenant.as_str())
        .bind(workspace.as_str())
        .fetch_optional(&mut **tx)
        .await
        .map_err(database_error)?
        .ok_or_else(|| HarnessError::execution("workspace occupation epoch is exhausted"))?,
    };
    crate::store::from_i64(epoch, "workspace occupation epoch").map(Some)
}
