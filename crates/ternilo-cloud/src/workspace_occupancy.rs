use crate::RunLease;
use crate::store::{database_error, set_tenant, to_i64};
use sqlx::Row;
use ternilo_protocol::{HarnessError, RunId, TenantId, WorkspaceId};
use ternilo_storage::Transaction;

mod epochs;
pub(crate) use epochs::{WorkspaceOccupant, admit_epoch_in};

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WorkspaceUseTicket {
    pub storage_id: String,
    pub root_id: String,
    pub tenant_id: TenantId,
    pub workspace_id: WorkspaceId,
    pub family_id: String,
    pub worker_id: String,
    pub worker_generation: u64,
    pub occupation_epoch: u64,
    pub run_id: RunId,
    pub lease_token: u64,
}

impl crate::CloudRunClaim {
    pub fn validate_workspace_use(&self) -> Result<(), HarnessError> {
        let ticket = &self.workspace_use;
        let metadata = &self.spec.metadata;
        ticket.validate()?;
        if ticket.tenant_id != self.tenant_id
            || ticket.tenant_id != metadata.tenant_id
            || ticket.workspace_id != metadata.workspace_id
            || ticket.run_id != self.run_id
            || ticket.run_id != metadata.run_id
            || self.session_id != metadata.session_id
            || ticket.lease_token != self.lease_token
        {
            return Err(HarnessError::policy(
                "workspace ticket does not identify this run",
            ));
        }
        Ok(())
    }
}

impl WorkspaceUseTicket {
    pub fn validate(&self) -> Result<(), HarnessError> {
        self.tenant_id.validate()?;
        self.workspace_id.validate()?;
        self.run_id.validate()?;
        if self.storage_id.trim().is_empty()
            || self.root_id.trim().is_empty()
            || self.family_id.trim().is_empty()
            || self.worker_id.trim().is_empty()
            || self.worker_generation == 0
            || self.occupation_epoch == 0
            || self.lease_token == 0
        {
            return Err(HarnessError::invalid("workspace use ticket is incomplete"));
        }
        Ok(())
    }
}

pub(crate) async fn occupy_in(
    tx: &mut Transaction,
    ticket: &WorkspaceUseTicket,
    now: u64,
) -> Result<(), HarnessError> {
    ticket.validate()?;
    set_tenant(tx, &ticket.tenant_id).await?;
    let inserted = sqlx::query(
        "INSERT INTO cloud_workspace_occupancy
            (storage_id,tenant_id,workspace_id,family_id,worker_id,worker_generation,
             occupation_epoch,run_id,lease_token,state,updated_at_ms)
         SELECT $1,$2,$3,$4,$5,$6,$7,$8,$9,'held',$10
         FROM cloud_workspace_epochs
         WHERE storage_id=$1 AND tenant_id=$2 AND workspace_id=$3 AND occupation_epoch=$7",
    )
    .bind(&ticket.storage_id)
    .bind(ticket.tenant_id.as_str())
    .bind(ticket.workspace_id.as_str())
    .bind(&ticket.family_id)
    .bind(&ticket.worker_id)
    .bind(to_i64(ticket.worker_generation, "worker generation")?)
    .bind(to_i64(ticket.occupation_epoch, "occupation epoch")?)
    .bind(ticket.run_id.as_str())
    .bind(to_i64(ticket.lease_token, "lease token")?)
    .bind(to_i64(now, "workspace occupation time")?)
    .execute(&mut **tx)
    .await
    .map_err(database_error)?
    .rows_affected();
    if inserted != 1 {
        return Err(HarnessError::policy(
            "workspace occupation epoch is no longer current",
        ));
    }
    Ok(())
}

pub(crate) async fn require_ticket_in(
    tx: &mut Transaction,
    claim: &crate::CloudRunClaim,
) -> Result<(), HarnessError> {
    claim.validate_workspace_use()?;
    let current = ticket_for_run_in(tx, &claim.into()).await?;
    if current.as_ref() != Some(&claim.workspace_use) {
        return Err(HarnessError::policy(
            "workspace ticket is stale or does not match its claim",
        ));
    }
    Ok(())
}

pub(crate) async fn confirm_exit_in(
    tx: &mut Transaction,
    lease: &RunLease,
    worker: &ternilo_transport::ExecutorId,
    worker_generation: u64,
    now: u64,
) -> Result<(), HarnessError> {
    let changed=sqlx::query("UPDATE cloud_workspace_occupancy SET state='released',exit_confirmed_at_ms=$5,updated_at_ms=$5 WHERE tenant_id=$1 AND run_id=$2 AND lease_token=$3 AND worker_id=$4 AND worker_generation=$6 AND state IN ('held','cleanup')").bind(lease.tenant_id.as_str()).bind(lease.run_id.as_str()).bind(to_i64(lease.lease_token,"lease token")?).bind(worker.as_str()).bind(to_i64(now,"workspace exit time")?).bind(to_i64(worker_generation,"worker generation")?).execute(&mut **tx).await.map_err(database_error)?.rows_affected();
    if changed == 0 {
        return Err(HarnessError::policy(
            "workspace physical exit confirmation is stale or missing",
        ));
    }
    Ok(())
}

pub(crate) async fn ticket_for_run_in(
    tx: &mut Transaction,
    lease: &RunLease,
) -> Result<Option<WorkspaceUseTicket>, HarnessError> {
    let row = sqlx::query(
        "SELECT occupancy.storage_id,occupancy.workspace_id,occupancy.family_id,
                occupancy.worker_id,occupancy.worker_generation,occupancy.occupation_epoch,
                root.root_id
         FROM cloud_workspace_occupancy occupancy
         JOIN cloud_storage_roots root ON root.storage_id=occupancy.storage_id
         JOIN cloud_workspace_epochs epoch ON epoch.storage_id=occupancy.storage_id
             AND epoch.tenant_id=occupancy.tenant_id AND epoch.workspace_id=occupancy.workspace_id
             AND epoch.occupation_epoch=occupancy.occupation_epoch
         WHERE occupancy.tenant_id=$1 AND occupancy.run_id=$2 AND occupancy.lease_token=$3
             AND occupancy.state<>'released'",
    )
    .bind(lease.tenant_id.as_str())
    .bind(lease.run_id.as_str())
    .bind(to_i64(lease.lease_token, "lease token")?)
    .fetch_optional(&mut **tx)
    .await
    .map_err(database_error)?;
    row.as_ref().map(|r| decode_ticket(r, lease)).transpose()
}

pub(crate) async fn historical_ticket_in(
    tx: &mut Transaction,
    lease: &RunLease,
) -> Result<Option<WorkspaceUseTicket>, HarnessError> {
    let row = sqlx::query(
        "SELECT occupancy.*,root.root_id FROM cloud_workspace_occupancy occupancy
         JOIN cloud_storage_roots root ON root.storage_id=occupancy.storage_id
         WHERE occupancy.tenant_id=$1 AND occupancy.run_id=$2 AND occupancy.lease_token=$3",
    )
    .bind(lease.tenant_id.as_str())
    .bind(lease.run_id.as_str())
    .bind(to_i64(lease.lease_token, "recovery lease")?)
    .fetch_optional(&mut **tx)
    .await
    .map_err(database_error)?;
    row.as_ref().map(|r| decode_ticket(r, lease)).transpose()
}

fn decode_ticket(
    r: &sqlx::any::AnyRow,
    lease: &RunLease,
) -> Result<WorkspaceUseTicket, HarnessError> {
    Ok(WorkspaceUseTicket {
        storage_id: r.try_get("storage_id").map_err(database_error)?,
        root_id: r.try_get("root_id").map_err(database_error)?,
        tenant_id: lease.tenant_id.clone(),
        workspace_id: WorkspaceId::new(
            r.try_get::<String, _>("workspace_id")
                .map_err(database_error)?,
        ),
        family_id: r.try_get("family_id").map_err(database_error)?,
        worker_id: r.try_get("worker_id").map_err(database_error)?,
        worker_generation: crate::store::from_i64(
            r.try_get("worker_generation").map_err(database_error)?,
            "worker generation",
        )?,
        occupation_epoch: crate::store::from_i64(
            r.try_get("occupation_epoch").map_err(database_error)?,
            "occupation epoch",
        )?,
        run_id: lease.run_id.clone(),
        lease_token: lease.lease_token,
    })
}

pub(crate) async fn release_unstarted_in(
    tx: &mut Transaction,
    lease: &RunLease,
    worker: &str,
    now: u64,
) -> Result<(), HarnessError> {
    sqlx::query("UPDATE cloud_workspace_occupancy SET state='released',exit_confirmed_at_ms=$4,updated_at_ms=$4 WHERE tenant_id=$1 AND run_id=$2 AND lease_token=$3 AND worker_id=$5 AND state='held'")
        .bind(lease.tenant_id.as_str()).bind(lease.run_id.as_str()).bind(to_i64(lease.lease_token,"lease token")?)
        .bind(to_i64(now,"workspace release time")?).bind(worker)
        .execute(&mut **tx).await.map_err(database_error)?;
    Ok(())
}

pub(crate) async fn mark_cleanup_in(
    tx: &mut Transaction,
    tenant: &TenantId,
    run: &RunId,
    lease: u64,
    now: u64,
) -> Result<(), HarnessError> {
    sqlx::query("UPDATE cloud_workspace_occupancy SET state='cleanup',updated_at_ms=$4 WHERE tenant_id=$1 AND run_id=$2 AND lease_token=$3 AND state='held'")
        .bind(tenant.as_str()).bind(run.as_str()).bind(to_i64(lease, "occupation lease")?)
        .bind(to_i64(now, "occupation cleanup time")?).execute(&mut **tx).await.map_err(database_error)?;
    Ok(())
}
