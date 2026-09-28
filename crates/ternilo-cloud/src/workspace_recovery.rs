//! Recovery acknowledges physical exit without restoring an expired execution's authority.

use serde::{Deserialize, Serialize};
use ternilo_protocol::{HarnessError, RunId, TenantId};
use ternilo_storage::{Backend, Transaction, set_tenant_scope, set_user_scope};

use crate::{
    CloudStore, CloudWorkerIdentity, ExecutionPhase, RunExecutionStatus, RunLease,
    WorkspaceUseTicket,
    execution_admission::{decode_status, pool_gate, reconcile_pool_in, released_in},
    store::{database_error, to_i64},
};

const PAGE_SIZE: usize = 32;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceRecoveryTicket {
    pub workspace: WorkspaceUseTicket,
    pub writer_fencing_token: u64,
}

impl WorkspaceRecoveryTicket {
    fn lease(&self) -> RunLease {
        RunLease {
            tenant_id: self.workspace.tenant_id.clone(),
            run_id: self.workspace.run_id.clone(),
            lease_token: self.workspace.lease_token,
            writer_fencing_token: self.writer_fencing_token,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceRecoveryCursor {
    pub tenant_id: TenantId,
    pub run_id: RunId,
    pub lease_token: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceRecoveryPage {
    pub candidates: Vec<WorkspaceRecoveryTicket>,
    pub next: Option<WorkspaceRecoveryCursor>,
}

impl CloudStore {
    pub async fn workspace_recovery_candidates(
        &self,
        identity: &CloudWorkerIdentity,
        after: Option<&WorkspaceRecoveryCursor>,
        now: u64,
    ) -> Result<WorkspaceRecoveryPage, HarnessError> {
        if let Some(after) = after {
            after.tenant_id.validate()?;
            after.run_id.validate()?;
            if after.lease_token == 0 {
                return Err(HarnessError::invalid(
                    "workspace recovery cursor is invalid",
                ));
            }
        }
        let mut tx = self.admission_transaction().await?;
        let storage = recovery_pool_in(&mut tx, identity, now).await?;
        reconcile_pool_in(&mut tx, &storage, now).await?;
        let sql = match ternilo_storage::backend(&tx) {
            Backend::Postgres => "SELECT * FROM ternilo_cloud_workspace_recovery($1,$2,$3,$4,$5)",
            Backend::Sqlite => include_str!("queries/workspace_recovery.sql"),
        };
        let rows = sqlx::query(sql)
            .bind(&storage)
            .bind(after.map_or("", |cursor| cursor.tenant_id.as_str()))
            .bind(after.map_or("", |cursor| cursor.run_id.as_str()))
            .bind(to_i64(
                after.map_or(0, |cursor| cursor.lease_token),
                "recovery cursor",
            )?)
            .bind(to_i64(now, "recovery time")?)
            .fetch_all(&mut *tx)
            .await
            .map_err(database_error)?;
        let mut candidates = Vec::with_capacity(rows.len());
        for row in rows {
            let status = decode_status(&row)?;
            set_tenant_scope(&mut tx, &status.tenant_id).await?;
            set_user_scope(&mut tx, &status.owner_user_id).await?;
            let lease = RunLease {
                tenant_id: status.tenant_id,
                run_id: status.run_id,
                lease_token: status.lease_token,
                writer_fencing_token: status.writer_fencing_token,
            };
            let workspace = crate::workspace_occupancy::ticket_for_run_in(&mut tx, &lease)
                .await?
                .ok_or_else(|| HarnessError::execution("recovery occupancy disappeared"))?;
            candidates.push(WorkspaceRecoveryTicket {
                workspace,
                writer_fencing_token: status.writer_fencing_token,
            });
        }
        let next = if candidates.len() == PAGE_SIZE {
            candidates.last().map(|ticket| WorkspaceRecoveryCursor {
                tenant_id: ticket.workspace.tenant_id.clone(),
                run_id: ticket.workspace.run_id.clone(),
                lease_token: ticket.workspace.lease_token,
            })
        } else {
            None
        };
        tx.commit().await.map_err(database_error)?;
        Ok(WorkspaceRecoveryPage { candidates, next })
    }

    /// Only the registered trusted supervisor may attest that the exact old writers stopped.
    pub async fn confirm_workspace_recovery(
        &self,
        identity: &CloudWorkerIdentity,
        ticket: &WorkspaceRecoveryTicket,
        now: u64,
    ) -> Result<(), HarnessError> {
        ticket.workspace.validate()?;
        let mut tx = self.admission_transaction().await?;
        let storage = recovery_pool_in(&mut tx, identity, now).await?;
        if storage != ticket.workspace.storage_id {
            return Err(HarnessError::policy(
                "recovery Worker belongs to another storage",
            ));
        }
        let status = recovery_execution_in(&mut tx, ticket).await?;
        set_tenant_scope(&mut tx, &status.tenant_id).await?;
        set_user_scope(&mut tx, &status.owner_user_id).await?;
        let lease = ticket.lease();
        let canonical = crate::workspace_occupancy::historical_ticket_in(&mut tx, &lease).await?;
        if status.storage_id != storage
            || status.worker_generation != ticket.workspace.worker_generation
            || status.writer_fencing_token != ticket.writer_fencing_token
            || canonical.as_ref() != Some(&ticket.workspace)
        {
            return Err(HarnessError::policy(
                "workspace recovery ticket is stale or forged",
            ));
        }
        // Historical equality is checked before the idempotent path, even after a new epoch.
        if status.phase == ExecutionPhase::Released {
            tx.commit().await.map_err(database_error)?;
            return Ok(());
        }
        if !matches!(status.phase, ExecutionPhase::Lost | ExecutionPhase::Cleanup) {
            return Err(HarnessError::policy(
                "workspace recovery requires a retired execution",
            ));
        }
        require_retired_run_in(&mut tx, &lease, now).await?;
        released_in(
            &mut tx,
            &status.tenant_id,
            &status.run_id,
            status.lease_token,
            &status.owner_user_id,
            now,
        )
        .await?;
        // The canonical original generation is used only to acknowledge its physical exit.
        crate::workspace_occupancy::confirm_exit_in(
            &mut tx,
            &lease,
            &status.worker_id,
            status.worker_generation,
            now,
        )
        .await?;
        ternilo_control::ControlStore::record_workspace_recovery_in(
            &mut tx,
            &status.tenant_id,
            &status.run_id,
            serde_json::json!({
                "reporter_worker_id": identity.worker_id,
                "reporter_generation": identity.generation,
                "original_worker_id": status.worker_id,
                "original_generation": status.worker_generation,
                "storage_id": storage,
                "workspace_id": ticket.workspace.workspace_id,
                "occupation_epoch": ticket.workspace.occupation_epoch,
                "lease_token": status.lease_token,
                "writer_fencing_token": status.writer_fencing_token,
            }),
            now,
        )
        .await?;
        tx.commit().await.map_err(database_error)
    }
}

async fn recovery_pool_in(
    tx: &mut Transaction,
    identity: &CloudWorkerIdentity,
    now: u64,
) -> Result<String, HarnessError> {
    crate::commands::require_worker_in(tx, identity, now).await?;
    let storage = CloudStore::worker_storage_in(tx, &identity.worker_id).await?;
    pool_gate(tx, &storage).await?;
    let valid: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM cloud_worker_credentials credential
         JOIN cloud_storage_roots root ON root.storage_id=credential.storage_id
             AND root.root_id=credential.root_id
         WHERE credential.worker_id=$1 AND credential.revoked_at_ms IS NULL",
    )
    .bind(identity.worker_id.as_str())
    .fetch_one(&mut **tx)
    .await
    .map_err(database_error)?;
    if valid != 1 {
        return Err(HarnessError::policy(
            "recovery Worker has no registered storage root",
        ));
    }
    Ok(storage)
}

async fn recovery_execution_in(
    tx: &mut Transaction,
    ticket: &WorkspaceRecoveryTicket,
) -> Result<RunExecutionStatus, HarnessError> {
    let sql = match ternilo_storage::backend(tx) {
        Backend::Postgres => "SELECT * FROM ternilo_cloud_execution_entry($1,$2,$3,$4)",
        Backend::Sqlite => {
            "SELECT * FROM cloud_run_execution WHERE worker_id=$1 AND tenant_id=$2 AND run_id=$3 AND lease_token=$4"
        }
    };
    let row = sqlx::query(sql)
        .bind(&ticket.workspace.worker_id)
        .bind(ticket.workspace.tenant_id.as_str())
        .bind(ticket.workspace.run_id.as_str())
        .bind(to_i64(ticket.workspace.lease_token, "recovery lease")?)
        .fetch_optional(&mut **tx)
        .await
        .map_err(database_error)?
        .ok_or_else(|| HarnessError::policy("workspace recovery execution is unknown"))?;
    decode_status(&row)
}

async fn require_retired_run_in(
    tx: &mut Transaction,
    lease: &RunLease,
    now: u64,
) -> Result<(), HarnessError> {
    let current: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM cloud_runs WHERE tenant_id=$1 AND run_id=$2 AND lease_token=$3
         AND state IN ('leased','running','cancel_requested') AND lease_expires_at_ms>$4",
    )
    .bind(lease.tenant_id.as_str())
    .bind(lease.run_id.as_str())
    .bind(to_i64(lease.lease_token, "recovery lease")?)
    .bind(to_i64(now, "recovery time")?)
    .fetch_one(&mut **tx)
    .await
    .map_err(database_error)?;
    if current != 0 {
        return Err(HarnessError::policy("workspace execution has not retired"));
    }
    Ok(())
}
