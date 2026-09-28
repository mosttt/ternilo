mod pressure;

use serde::{Deserialize, Serialize};
use ternilo_protocol::HarnessError;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerCapacity {
    pub max_active_runs: u32,
    pub max_resident_runs: u32,
}

impl Default for WorkerCapacity {
    fn default() -> Self {
        Self {
            max_active_runs: 4,
            max_resident_runs: 16,
        }
    }
}

impl WorkerCapacity {
    pub fn validate(self) -> Result<(), HarnessError> {
        if self.max_active_runs == 0 || self.max_resident_runs < self.max_active_runs {
            return Err(HarnessError::invalid(
                "Worker capacity requires positive active runs and resident runs at least as large",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum RunAdmission {
    Ready { admission_epoch: u64 },
    Pending,
}

use crate::{
    CloudStore, CloudWorkerIdentity, RunLease, StartedRun,
    store::{database_error, from_i64, to_i64},
};
use sqlx::{Row, any::AnyRow};
use std::collections::BTreeMap;
use ternilo_protocol::{AcceptedSubagentRun, RunId, SessionId, TenantId, UserId};
use ternilo_storage::{
    Backend, Json, Transaction, for_update, lock, set_tenant_scope, set_user_scope,
};
use ternilo_transport::ExecutorId;

const MAX_WAIT_DEPENDENCIES: usize = 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionPhase {
    Claimed,
    Active,
    Parked,
    ResumePending,
    Cleanup,
    Lost,
    Released,
}

impl ExecutionPhase {
    fn parse(value: &str) -> Result<Self, HarnessError> {
        match value {
            "claimed" => Ok(Self::Claimed),
            "active" => Ok(Self::Active),
            "parked" => Ok(Self::Parked),
            "resume_pending" => Ok(Self::ResumePending),
            "cleanup" => Ok(Self::Cleanup),
            "lost" => Ok(Self::Lost),
            "released" => Ok(Self::Released),
            _ => Err(HarnessError::execution("invalid execution admission phase")),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RunExecutionStatus {
    pub tenant_id: TenantId,
    pub run_id: RunId,
    pub session_id: SessionId,
    pub owner_user_id: UserId,
    pub actor_user_id: UserId,
    pub storage_id: String,
    pub worker_id: ExecutorId,
    pub worker_generation: u64,
    pub lease_token: u64,
    pub writer_fencing_token: u64,
    pub phase: ExecutionPhase,
    pub admission_epoch: u64,
    pub activity_revision: u64,
    pub parked_revision: u64,
}

struct WorkerSlot {
    id: ExecutorId,
    generation: u64,
    capacity: WorkerCapacity,
}

pub(crate) async fn pool_gate(tx: &mut Transaction, storage: &str) -> Result<(), HarnessError> {
    lock(tx, &format!("ternilo:execution-storage:{storage}")).await
}

pub(crate) async fn worker_pool(
    tx: &mut Transaction,
    worker: &str,
) -> Result<String, HarnessError> {
    CloudStore::worker_storage_in(tx, &ExecutorId::new(worker)).await
}

async fn worker_slot(
    tx: &mut Transaction,
    worker: &str,
    now: u64,
) -> Result<WorkerSlot, HarnessError> {
    let row=sqlx::query("SELECT generation,max_active_runs,max_resident_runs,hello_json FROM cloud_workers WHERE worker_id=$1 AND lease_expires_at_ms>$2")
        .bind(worker).bind(to_i64(now,"worker capacity time")?).fetch_optional(&mut **tx).await.map_err(database_error)?
        .ok_or_else(||HarnessError::policy("Worker must have a current registered generation before claiming execution"))?;
    let hello = row
        .try_get::<Json<ternilo_transport::ExecutorHello>, _>("hello_json")
        .map_err(database_error)?
        .0;
    if !hello
        .capabilities
        .contains(&ternilo_transport::ExecutorCapability::CloudRun)
    {
        return Err(HarnessError::policy(
            "Worker does not advertise Cloud run execution",
        ));
    }
    decode_worker_slot(worker, &row)
}

fn decode_worker_slot(worker: &str, row: &AnyRow) -> Result<WorkerSlot, HarnessError> {
    Ok(WorkerSlot {
        id: ExecutorId::new(worker),
        generation: from_i64(
            row.try_get("generation").map_err(database_error)?,
            "worker generation",
        )?,
        capacity: WorkerCapacity {
            max_active_runs: u32::try_from(
                row.try_get::<i64, _>("max_active_runs")
                    .map_err(database_error)?,
            )
            .map_err(|_| HarnessError::execution("invalid active capacity"))?,
            max_resident_runs: u32::try_from(
                row.try_get::<i64, _>("max_resident_runs")
                    .map_err(database_error)?,
            )
            .map_err(|_| HarnessError::execution("invalid resident capacity"))?,
        },
    })
}

async fn usage_in(
    tx: &mut Transaction,
    tenant: &TenantId,
    worker: &str,
) -> Result<(i64, i64, i64), HarnessError> {
    let sql = match ternilo_storage::backend(tx) {
        Backend::Postgres => "SELECT * FROM ternilo_cloud_execution_usage($1,$2)",
        Backend::Sqlite => {
            "SELECT (SELECT COUNT(*) FROM cloud_run_execution WHERE tenant_id=$1 AND phase IN ('claimed','active')) AS tenant_active,(SELECT COUNT(*) FROM cloud_run_execution WHERE worker_id=$2 AND phase IN ('claimed','active')) AS worker_active,(SELECT COUNT(*) FROM cloud_run_execution WHERE worker_id=$2 AND phase<>'released') AS worker_resident"
        }
    };
    let row = sqlx::query(sql)
        .bind(tenant.as_str())
        .bind(worker)
        .fetch_one(&mut **tx)
        .await
        .map_err(database_error)?;
    Ok((
        row.try_get("tenant_active").map_err(database_error)?,
        row.try_get("worker_active").map_err(database_error)?,
        row.try_get("worker_resident").map_err(database_error)?,
    ))
}

pub(crate) fn decode_status(row: &AnyRow) -> Result<RunExecutionStatus, HarnessError> {
    Ok(RunExecutionStatus {
        tenant_id: TenantId::new(
            row.try_get::<String, _>("tenant_id")
                .map_err(database_error)?,
        ),
        run_id: RunId::new(row.try_get::<String, _>("run_id").map_err(database_error)?),
        session_id: SessionId::new(
            row.try_get::<String, _>("session_id")
                .map_err(database_error)?,
        ),
        owner_user_id: UserId::new(
            row.try_get::<String, _>("owner_user_id")
                .map_err(database_error)?,
        ),
        actor_user_id: UserId::new(
            row.try_get::<String, _>("actor_user_id")
                .map_err(database_error)?,
        ),
        storage_id: row.try_get("storage_id").map_err(database_error)?,
        worker_id: ExecutorId::new(
            row.try_get::<String, _>("worker_id")
                .map_err(database_error)?,
        ),
        worker_generation: from_i64(
            row.try_get("worker_generation").map_err(database_error)?,
            "worker generation",
        )?,
        lease_token: from_i64(
            row.try_get("lease_token").map_err(database_error)?,
            "execution lease token",
        )?,
        writer_fencing_token: from_i64(
            row.try_get("writer_fencing_token")
                .map_err(database_error)?,
            "execution writer fence",
        )?,
        phase: ExecutionPhase::parse(&row.try_get::<String, _>("phase").map_err(database_error)?)?,
        admission_epoch: from_i64(
            row.try_get("admission_epoch").map_err(database_error)?,
            "admission epoch",
        )?,
        activity_revision: from_i64(
            row.try_get("activity_revision").map_err(database_error)?,
            "activity revision",
        )?,
        parked_revision: from_i64(
            row.try_get("parked_revision").map_err(database_error)?,
            "parked revision",
        )?,
    })
}

async fn owner_scope(
    tx: &mut Transaction,
    tenant: &TenantId,
    owner: &UserId,
) -> Result<(), HarnessError> {
    set_tenant_scope(tx, tenant).await?;
    set_user_scope(tx, owner).await
}

pub(crate) async fn reconcile_pool_in(
    tx: &mut Transaction,
    storage: &str,
    now: u64,
) -> Result<(), HarnessError> {
    let timestamp = to_i64(now, "execution reconciliation time")?;
    let sql = match ternilo_storage::backend(tx) {
        Backend::Postgres => "SELECT * FROM ternilo_cloud_execution_invalid($1,$2)",
        Backend::Sqlite => include_str!("queries/execution_invalid.sql"),
    };
    let invalid = sqlx::query(sql)
        .bind(storage)
        .bind(timestamp)
        .fetch_all(&mut **tx)
        .await
        .map_err(database_error)?;
    for row in invalid {
        let status = decode_status(&row)?;
        owner_scope(tx, &status.tenant_id, &status.owner_user_id).await?;
        let phase = if status.phase == ExecutionPhase::Claimed {
            "released"
        } else {
            "lost"
        };
        sqlx::query("UPDATE cloud_run_execution SET phase=$4,updated_at_ms=$5,released_at_ms=CASE WHEN $4='released' THEN $5 ELSE NULL END WHERE tenant_id=$1 AND run_id=$2 AND lease_token=$3 AND phase NOT IN ('lost','released')")
            .bind(status.tenant_id.as_str()).bind(status.run_id.as_str()).bind(to_i64(status.lease_token,"execution lease")?).bind(phase).bind(timestamp).execute(&mut **tx).await.map_err(database_error)?;
        if status.phase == ExecutionPhase::Claimed {
            // Start and reconciliation share the storage gate. No process may touch
            // the workspace before Start commits, so this claim has no writers.
            crate::workspace_occupancy::release_unstarted_in(
                tx,
                &RunLease {
                    tenant_id: status.tenant_id,
                    run_id: status.run_id,
                    lease_token: status.lease_token,
                    writer_fencing_token: 0,
                },
                status.worker_id.as_str(),
                now,
            )
            .await?;
        } else {
            crate::workspace_occupancy::mark_cleanup_in(
                tx,
                &status.tenant_id,
                &status.run_id,
                status.lease_token,
                now,
            )
            .await?;
        }
    }
    Ok(())
}

pub(crate) async fn claim_in(
    tx: &mut Transaction,
    row: &AnyRow,
    worker: &str,
    storage: &str,
    now: u64,
) -> Result<bool, HarnessError> {
    let tenant = TenantId::new(
        row.try_get::<String, _>("tenant_id")
            .map_err(database_error)?,
    );
    let owner = UserId::new(
        row.try_get::<String, _>("user_id")
            .map_err(database_error)?,
    );
    owner_scope(tx, &tenant, &owner).await?;
    let slot = worker_slot(tx, worker, now).await?;
    let limit: i32 = sqlx::query_scalar(for_update(
        tx,
        "SELECT max_concurrent_runs FROM control_quotas WHERE tenant_id=$1",
        "SELECT max_concurrent_runs FROM control_quotas WHERE tenant_id=$1 FOR UPDATE",
    ))
    .bind(tenant.as_str())
    .fetch_one(&mut **tx)
    .await
    .map_err(database_error)?;
    let (tenant_active, worker_active, resident) = usage_in(tx, &tenant, worker).await?;
    if tenant_active >= i64::from(limit)
        || worker_active >= i64::from(slot.capacity.max_active_runs)
        || resident >= i64::from(slot.capacity.max_resident_runs)
    {
        return Ok(false);
    }
    let lease = row
        .try_get::<i64, _>("lease_token")
        .map_err(database_error)?
        .checked_add(1)
        .ok_or_else(|| HarnessError::execution("execution lease overflow"))?;
    let run_id = RunId::new(row.try_get::<String, _>("run_id").map_err(database_error)?);
    let session_id = SessionId::new(
        row.try_get::<String, _>("session_id")
            .map_err(database_error)?,
    );
    let workspace_id = ternilo_protocol::WorkspaceId::new(
        row.try_get::<String, _>("workspace_id")
            .map_err(database_error)?,
    );
    let family_id = crate::execution_families::family_for_session_in(
        tx,
        &tenant,
        &session_id,
        &owner,
        &workspace_id,
    )
    .await?;
    let root_id: String =
        sqlx::query_scalar("SELECT root_id FROM cloud_storage_roots WHERE storage_id=$1")
            .bind(storage)
            .fetch_one(&mut **tx)
            .await
            .map_err(database_error)?;
    let epoch = crate::workspace_occupancy::admit_epoch_in(
        tx,
        storage,
        &tenant,
        &workspace_id,
        crate::workspace_occupancy::WorkspaceOccupant {
            family_id: &family_id,
            worker_id: worker,
            worker_generation: slot.generation,
        },
        now,
    )
    .await?;
    let Some(occupation_epoch) = epoch else {
        return Ok(false);
    };
    let ticket = crate::workspace_occupancy::WorkspaceUseTicket {
        storage_id: storage.to_owned(),
        root_id,
        tenant_id: tenant.clone(),
        workspace_id,
        family_id,
        worker_id: worker.to_owned(),
        worker_generation: slot.generation,
        occupation_epoch,
        run_id: run_id.clone(),
        lease_token: from_i64(lease, "execution lease")?,
    };
    crate::workspace_occupancy::occupy_in(tx, &ticket, now).await?;
    sqlx::query("INSERT INTO cloud_run_execution(tenant_id,run_id,lease_token,session_id,owner_user_id,actor_user_id,storage_id,worker_id,worker_generation,phase,created_at_ms,updated_at_ms) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,'claimed',$10,$10)")
        .bind(tenant.as_str()).bind(row.try_get::<String,_>("run_id").map_err(database_error)?).bind(lease).bind(row.try_get::<String,_>("session_id").map_err(database_error)?).bind(owner.as_str())
        .bind(row.try_get::<String,_>("actor_user_id").map_err(database_error)?).bind(storage).bind(worker).bind(to_i64(slot.generation,"worker generation")?).bind(to_i64(now,"execution admission time")?)
        .execute(&mut **tx).await.map_err(database_error)?;
    Ok(true)
}

pub(crate) async fn started_in(
    tx: &mut Transaction,
    run: &crate::CloudRunClaim,
    worker: &str,
    fence: i64,
    now: u64,
) -> Result<(), HarnessError> {
    owner_scope(tx, &run.tenant_id, &run.spec.metadata.user_id).await?;
    let slot = worker_slot(tx, worker, now).await?;
    crate::workspace_occupancy::require_ticket_in(tx, run).await?;
    let changed=sqlx::query("UPDATE cloud_run_execution SET phase='active',writer_fencing_token=$4,updated_at_ms=$5 WHERE tenant_id=$1 AND run_id=$2 AND lease_token=$3 AND worker_id=$6 AND worker_generation=$7 AND phase='claimed'")
        .bind(run.tenant_id.as_str()).bind(run.run_id.as_str()).bind(to_i64(run.lease_token,"execution lease")?).bind(fence).bind(to_i64(now,"execution start time")?).bind(worker).bind(to_i64(slot.generation,"worker generation")?)
        .execute(&mut **tx).await.map_err(database_error)?.rows_affected();
    if changed != 1 {
        return Err(HarnessError::policy(
            "execution admission is no longer current",
        ));
    }
    Ok(())
}

pub(crate) async fn released_in(
    tx: &mut Transaction,
    tenant: &TenantId,
    run: &RunId,
    lease: u64,
    owner: &UserId,
    now: u64,
) -> Result<(), HarnessError> {
    owner_scope(tx, tenant, owner).await?;
    sqlx::query("UPDATE cloud_run_execution SET phase='released',updated_at_ms=$4,released_at_ms=COALESCE(released_at_ms,$4) WHERE tenant_id=$1 AND run_id=$2 AND lease_token=$3")
        .bind(tenant.as_str()).bind(run.as_str()).bind(to_i64(lease,"execution lease")?).bind(to_i64(now,"resident release time")?).execute(&mut **tx).await.map_err(database_error)?;
    Ok(())
}

pub(crate) async fn mark_cleanup_in(
    tx: &mut Transaction,
    tenant: &TenantId,
    run: &RunId,
    lease: u64,
    owner: &UserId,
    now: u64,
) -> Result<(), HarnessError> {
    owner_scope(tx, tenant, owner).await?;
    sqlx::query("UPDATE cloud_run_execution SET phase='cleanup',updated_at_ms=$4 WHERE tenant_id=$1 AND run_id=$2 AND lease_token=$3 AND phase IN ('active','parked','resume_pending','lost')")
        .bind(tenant.as_str()).bind(run.as_str()).bind(to_i64(lease,"execution lease")?).bind(to_i64(now,"resident cleanup time")?).execute(&mut **tx).await.map_err(database_error)?;
    crate::workspace_occupancy::mark_cleanup_in(tx, tenant, run, lease, now).await
}

fn canonical_dependencies(
    dependencies: &[AcceptedSubagentRun],
) -> Result<Vec<AcceptedSubagentRun>, HarnessError> {
    if dependencies.is_empty() || dependencies.len() > MAX_WAIT_DEPENDENCIES {
        return Err(HarnessError::invalid(
            "park requires 1 to 1024 accepted dependencies",
        ));
    }
    let mut entries = BTreeMap::new();
    for dependency in dependencies {
        dependency.validate()?;
        if entries
            .insert(dependency.run_id.clone(), dependency.session_id.clone())
            .is_some()
        {
            return Err(HarnessError::invalid(
                "park dependencies must identify distinct runs",
            ));
        }
    }
    Ok(entries
        .into_iter()
        .map(|(run_id, session_id)| AcceptedSubagentRun { session_id, run_id })
        .collect())
}

async fn admission_in(
    tx: &mut Transaction,
    run: &StartedRun,
    worker: &str,
    now: u64,
) -> Result<RunExecutionStatus, HarnessError> {
    let current = crate::store::require_writer_in(tx, run, worker, Some(now)).await?;
    let now_i64 = to_i64(now, "execution time")?;
    if current
        .try_get::<String, _>("state")
        .map_err(database_error)?
        != "running"
        || current
            .try_get::<Option<i64>, _>("lease_expires_at_ms")
            .map_err(database_error)?
            .is_none_or(|expiry| expiry <= now_i64)
    {
        return Err(HarnessError::policy(
            "only the current running execution can change admission",
        ));
    }
    let row = sqlx::query(
        "SELECT * FROM cloud_run_execution WHERE tenant_id=$1 AND run_id=$2 AND lease_token=$3",
    )
    .bind(run.claim.tenant_id.as_str())
    .bind(run.claim.run_id.as_str())
    .bind(to_i64(run.claim.lease_token, "execution lease")?)
    .fetch_optional(&mut **tx)
    .await
    .map_err(database_error)?
    .ok_or_else(|| HarnessError::policy("execution admission does not exist"))?;
    let status = decode_status(&row)?;
    let slot = worker_slot(tx, worker, now).await?;
    if status.worker_id != slot.id
        || status.worker_generation != slot.generation
        || status.writer_fencing_token != run.fencing_token
        || matches!(
            status.phase,
            ExecutionPhase::Lost
                | ExecutionPhase::Released
                | ExecutionPhase::Claimed
                | ExecutionPhase::Cleanup
        )
    {
        return Err(HarnessError::policy(
            "execution admission generation is no longer current",
        ));
    }
    Ok(status)
}

async fn dependencies_in(
    tx: &mut Transaction,
    status: &RunExecutionStatus,
) -> Result<Vec<AcceptedSubagentRun>, HarnessError> {
    let rows=sqlx::query("SELECT child_session_id,child_run_id FROM cloud_run_wait_dependencies WHERE tenant_id=$1 AND parent_run_id=$2 AND parent_lease_token=$3 ORDER BY child_run_id")
        .bind(status.tenant_id.as_str()).bind(status.run_id.as_str()).bind(to_i64(status.lease_token,"dependency lease")?).fetch_all(&mut **tx).await.map_err(database_error)?;
    rows.iter()
        .map(|row| {
            Ok(AcceptedSubagentRun {
                session_id: SessionId::new(
                    row.try_get::<String, _>("child_session_id")
                        .map_err(database_error)?,
                ),
                run_id: RunId::new(
                    row.try_get::<String, _>("child_run_id")
                        .map_err(database_error)?,
                ),
            })
        })
        .collect()
}

impl CloudStore {
    pub async fn run_execution(
        &self,
        tenant: &TenantId,
        owner: &UserId,
        run: &RunId,
    ) -> Result<Option<RunExecutionStatus>, HarnessError> {
        let mut tx = self.owner_transaction(tenant, owner).await?;
        let row=sqlx::query("SELECT * FROM cloud_run_execution WHERE tenant_id=$1 AND owner_user_id=$2 AND run_id=$3 ORDER BY lease_token DESC LIMIT 1")
            .bind(tenant.as_str()).bind(owner.as_str()).bind(run.as_str()).fetch_optional(&mut *tx).await.map_err(database_error)?;
        tx.commit().await.map_err(database_error)?;
        row.as_ref().map(decode_status).transpose()
    }

    pub async fn park_run(
        &self,
        run: &StartedRun,
        worker: &str,
        dependencies: &[AcceptedSubagentRun],
        activity_revision: u64,
        now: u64,
    ) -> Result<u64, HarnessError> {
        if activity_revision == 0 {
            return Err(HarnessError::invalid("activity revision must be positive"));
        }
        let dependencies = canonical_dependencies(dependencies)?;
        let mut tx = self.admission_transaction().await?;
        let storage = worker_pool(&mut tx, worker).await?;
        pool_gate(&mut tx, &storage).await?;
        let status = admission_in(&mut tx, run, worker, now).await?;
        if activity_revision == status.activity_revision && status.phase == ExecutionPhase::Parked {
            if dependencies_in(&mut tx, &status).await? != dependencies {
                return Err(HarnessError::conflict(
                    "a repeated park revision must retain its accepted dependencies",
                ));
            }
            tx.commit().await.map_err(database_error)?;
            return Ok(status.parked_revision);
        }
        if activity_revision <= status.activity_revision
            || !matches!(
                status.phase,
                ExecutionPhase::Active | ExecutionPhase::Parked
            )
        {
            return Err(HarnessError::conflict(
                "execution activity changed before parking",
            ));
        }
        for dependency in &dependencies {
            crate::run_lineage::require_accepted_dependency_in(
                &mut tx,
                run,
                &dependency.session_id,
                &dependency.run_id,
            )
            .await?;
        }
        let revision = status
            .parked_revision
            .checked_add(1)
            .ok_or_else(|| HarnessError::execution("park revision overflow"))?;
        sqlx::query("DELETE FROM cloud_run_wait_dependencies WHERE tenant_id=$1 AND parent_run_id=$2 AND parent_lease_token=$3")
            .bind(status.tenant_id.as_str()).bind(status.run_id.as_str()).bind(to_i64(status.lease_token,"dependency lease")?).execute(&mut *tx).await.map_err(database_error)?;
        for dependency in &dependencies {
            sqlx::query("INSERT INTO cloud_run_wait_dependencies(tenant_id,parent_run_id,parent_lease_token,owner_user_id,activity_revision,child_session_id,child_run_id) VALUES($1,$2,$3,$4,$5,$6,$7)")
                .bind(status.tenant_id.as_str()).bind(status.run_id.as_str()).bind(to_i64(status.lease_token,"dependency lease")?).bind(status.owner_user_id.as_str()).bind(to_i64(activity_revision,"activity revision")?)
                .bind(dependency.session_id.as_str()).bind(dependency.run_id.as_str()).execute(&mut *tx).await.map_err(database_error)?;
        }
        sqlx::query("UPDATE cloud_run_execution SET phase='parked',activity_revision=$4,parked_revision=$5,updated_at_ms=$6 WHERE tenant_id=$1 AND run_id=$2 AND lease_token=$3")
            .bind(status.tenant_id.as_str()).bind(status.run_id.as_str()).bind(to_i64(status.lease_token,"execution lease")?).bind(to_i64(activity_revision,"activity revision")?).bind(to_i64(revision,"park revision")?).bind(to_i64(now,"park time")?)
            .execute(&mut *tx).await.map_err(database_error)?;
        tx.commit().await.map_err(database_error)?;
        Ok(revision)
    }

    pub async fn resume_run(
        &self,
        run: &StartedRun,
        worker: &str,
        activity_revision: u64,
        parked_revision: u64,
        now: u64,
    ) -> Result<RunAdmission, HarnessError> {
        let mut tx = self.admission_transaction().await?;
        let storage = worker_pool(&mut tx, worker).await?;
        pool_gate(&mut tx, &storage).await?;
        reconcile_pool_in(&mut tx, &storage, now).await?;
        let status = admission_in(&mut tx, run, worker, now).await?;
        if activity_revision < status.activity_revision || parked_revision != status.parked_revision
        {
            return Err(HarnessError::conflict(
                "execution activity changed before resuming",
            ));
        }
        if status.phase == ExecutionPhase::Active {
            if activity_revision > status.activity_revision {
                sqlx::query("UPDATE cloud_run_execution SET activity_revision=$4,updated_at_ms=$5 WHERE tenant_id=$1 AND run_id=$2 AND lease_token=$3")
                    .bind(status.tenant_id.as_str()).bind(status.run_id.as_str()).bind(to_i64(status.lease_token,"execution lease")?)
                    .bind(to_i64(activity_revision,"activity revision")?).bind(to_i64(now,"activity time")?).execute(&mut *tx).await.map_err(database_error)?;
            }
            tx.commit().await.map_err(database_error)?;
            return Ok(RunAdmission::Ready {
                admission_epoch: status.admission_epoch,
            });
        }
        let limit: i32 = sqlx::query_scalar(for_update(
            &tx,
            "SELECT max_concurrent_runs FROM control_quotas WHERE tenant_id=$1",
            "SELECT max_concurrent_runs FROM control_quotas WHERE tenant_id=$1 FOR UPDATE",
        ))
        .bind(status.tenant_id.as_str())
        .fetch_one(&mut *tx)
        .await
        .map_err(database_error)?;
        let slot = worker_slot(&mut tx, worker, now).await?;
        let (tenant_active, worker_active, _) =
            usage_in(&mut tx, &status.tenant_id, worker).await?;
        let granted = tenant_active < i64::from(limit)
            && worker_active < i64::from(slot.capacity.max_active_runs);
        let epoch = if granted {
            status
                .admission_epoch
                .checked_add(1)
                .ok_or_else(|| HarnessError::execution("admission epoch overflow"))?
        } else {
            status.admission_epoch
        };
        sqlx::query("UPDATE cloud_run_execution SET phase=$4,activity_revision=$5,admission_epoch=$6,updated_at_ms=$7 WHERE tenant_id=$1 AND run_id=$2 AND lease_token=$3")
            .bind(status.tenant_id.as_str()).bind(status.run_id.as_str()).bind(to_i64(status.lease_token,"execution lease")?).bind(if granted {"active"}else{"resume_pending"}).bind(to_i64(activity_revision,"activity revision")?)
            .bind(to_i64(epoch,"admission epoch")?).bind(to_i64(now,"resume time")?).execute(&mut *tx).await.map_err(database_error)?;
        if granted {
            sqlx::query("DELETE FROM cloud_run_wait_dependencies WHERE tenant_id=$1 AND parent_run_id=$2 AND parent_lease_token=$3")
                .bind(status.tenant_id.as_str()).bind(status.run_id.as_str()).bind(to_i64(status.lease_token,"dependency lease")?).execute(&mut *tx).await.map_err(database_error)?;
        }
        tx.commit().await.map_err(database_error)?;
        Ok(if granted {
            RunAdmission::Ready {
                admission_epoch: epoch,
            }
        } else {
            RunAdmission::Pending
        })
    }

    /// Cleanup acknowledgement cannot authorize a run or alter a newer lease generation.
    pub async fn release_resident(
        &self,
        lease: &RunLease,
        identity: &CloudWorkerIdentity,
        worker_generation: u64,
        now: u64,
    ) -> Result<(), HarnessError> {
        if identity.generation != worker_generation {
            return Err(HarnessError::policy(
                "resident cleanup belongs to another Worker generation",
            ));
        }
        let mut tx = self.admission_transaction().await?;
        crate::commands::require_worker_in(&mut tx, identity, now).await?;
        let storage = worker_pool(&mut tx, identity.worker_id.as_str()).await?;
        pool_gate(&mut tx, &storage).await?;
        let sql = match ternilo_storage::backend(&tx) {
            Backend::Postgres => "SELECT * FROM ternilo_cloud_execution_entry($1,$2,$3,$4)",
            Backend::Sqlite => {
                "SELECT * FROM cloud_run_execution WHERE worker_id=$1 AND tenant_id=$2 AND run_id=$3 AND lease_token=$4"
            }
        };
        let row = sqlx::query(sql)
            .bind(identity.worker_id.as_str())
            .bind(lease.tenant_id.as_str())
            .bind(lease.run_id.as_str())
            .bind(to_i64(lease.lease_token, "resident lease")?)
            .fetch_optional(&mut *tx)
            .await
            .map_err(database_error)?
            .ok_or_else(|| {
                HarnessError::policy("resident cleanup does not identify an accepted execution")
            })?;
        let status = decode_status(&row)?;
        if status.worker_generation != worker_generation
            || status.writer_fencing_token != lease.writer_fencing_token
        {
            return Err(HarnessError::policy(
                "resident cleanup does not match the original execution generation",
            ));
        }
        owner_scope(&mut tx, &status.tenant_id, &status.owner_user_id).await?;
        if status.phase != ExecutionPhase::Released {
            let current:i64=sqlx::query_scalar("SELECT COUNT(*) FROM cloud_runs WHERE tenant_id=$1 AND run_id=$2 AND lease_token=$3 AND state IN ('leased','running','cancel_requested') AND lease_expires_at_ms>$4")
                .bind(lease.tenant_id.as_str()).bind(lease.run_id.as_str()).bind(to_i64(lease.lease_token,"resident lease")?).bind(to_i64(now,"resident cleanup time")?).fetch_one(&mut *tx).await.map_err(database_error)?;
            if current != 0 {
                return Err(HarnessError::policy(
                    "finish or release the current run before acknowledging resident cleanup",
                ));
            }
            released_in(
                &mut tx,
                &status.tenant_id,
                &status.run_id,
                status.lease_token,
                &status.owner_user_id,
                now,
            )
            .await?;
            crate::workspace_occupancy::confirm_exit_in(
                &mut tx,
                lease,
                &identity.worker_id,
                identity.generation,
                now,
            )
            .await?;
        }
        tx.commit().await.map_err(database_error)
    }

    pub async fn require_foreground_admission(
        &self,
        run: &StartedRun,
        worker: &str,
        now: u64,
    ) -> Result<(), HarnessError> {
        let mut tx = self.begin().await?;
        let status = admission_in(&mut tx, run, worker, now).await?;
        if status.phase != ExecutionPhase::Active {
            return Err(HarnessError::policy(
                "execution is waiting for foreground admission",
            ));
        }
        tx.commit().await.map_err(database_error)
    }
}

pub(crate) async fn require_active_in(
    tx: &mut Transaction,
    run: &StartedRun,
    worker: &str,
    now: u64,
) -> Result<(), HarnessError> {
    let status = admission_in(tx, run, worker, now).await?;
    if status.phase != ExecutionPhase::Active {
        return Err(HarnessError::policy(
            "execution is waiting for foreground admission",
        ));
    }
    Ok(())
}

/// A lost resident retains physical capacity but cannot regain execution authority.
pub(crate) async fn require_current_in(
    tx: &mut Transaction,
    run: &StartedRun,
    worker: &str,
    now: Option<u64>,
) -> Result<(), HarnessError> {
    let now = now
        .map(|value| to_i64(value, "execution authorization time"))
        .transpose()?;
    let current:i64 = sqlx::query_scalar("SELECT COUNT(*) FROM cloud_run_execution execution JOIN cloud_workers worker ON worker.worker_id=execution.worker_id AND worker.generation=execution.worker_generation WHERE execution.tenant_id=$1 AND execution.run_id=$2 AND execution.lease_token=$3 AND execution.worker_id=$4 AND execution.writer_fencing_token=$5 AND execution.phase IN ('active','parked','resume_pending','cleanup') AND (CAST($6 AS BIGINT) IS NULL OR worker.lease_expires_at_ms>$6)")
        .bind(run.claim.tenant_id.as_str()).bind(run.claim.run_id.as_str()).bind(to_i64(run.claim.lease_token,"execution lease")?)
        .bind(worker).bind(to_i64(run.fencing_token,"writer fence")?).bind(now)
        .fetch_one(&mut **tx).await.map_err(database_error)?;
    if current != 1 {
        return Err(HarnessError::policy(
            "execution admission generation is no longer current",
        ));
    }
    Ok(())
}
