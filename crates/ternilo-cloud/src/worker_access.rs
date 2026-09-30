use std::{
    fmt::Write as _,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::random;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{Row, any::AnyRow};
use ternilo_protocol::{HarnessError, SessionId, TenantId, UserId};
use ternilo_storage::{Json, Transaction, for_update, set_tenant_scope, set_user_scope};
use ternilo_transport::ExecutorId;

use crate::{
    ClaimedCloudSessionCommand, CloudRunClaim, CloudStore, CloudWorkerIdentity, CommandLease,
    RunLease, StartedRun, WorkerConfiguration, WorkerRegisterRequest, commands,
    store::{database_error, from_i64, to_i64},
};

#[derive(Clone)]
pub(crate) struct WorkerAccess {
    token_hash: String,
    identity: CloudWorkerIdentity,
    run: Option<RunLease>,
    command: Option<CommandLease>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkerCredentialRecord {
    pub worker_id: ExecutorId,
    pub storage_id: String,
    pub registered: bool,
    pub online: bool,
    pub last_seen_at_ms: Option<u64>,
    pub lease_expires_at_ms: Option<u64>,
    pub created_at_ms: u64,
    pub revoked_at_ms: Option<u64>,
}

#[derive(Serialize, Deserialize)]
pub struct WorkerCredentialGrant {
    pub worker_id: ExecutorId,
    pub storage_id: String,
    pub token: String,
}

impl CloudStore {
    /// Capacity transitions acquire their pool gate before canonical run row locks.
    pub(crate) async fn admission_transaction(&self) -> Result<Transaction, HarnessError> {
        let mut store = self.clone();
        if let Some(access) = &mut store.worker_access {
            access.run = None;
        }
        store.begin().await
    }

    pub(crate) async fn begin(&self) -> Result<Transaction, HarnessError> {
        let mut transaction = self.database.begin().await?;
        if let Some(access) = &self.worker_access {
            credential_in(
                &mut transaction,
                &access.token_hash,
                Some(&access.identity.worker_id),
            )
            .await?;
            let now = current_time_ms()?;
            commands::require_worker_in(&mut transaction, &access.identity, now).await?;
            if let Some(run) = &access.run {
                canonical_run_in(&mut transaction, &access.identity, run, now, true).await?;
            }
            if let Some(command) = &access.command {
                canonical_command_in(&mut transaction, &access.identity, command, now).await?;
            }
        }
        Ok(transaction)
    }

    #[expect(
        clippy::too_many_lines,
        reason = "Keep canonical worker, resource authority and execution reservation checks together in one transaction."
    )]
    pub async fn authorize_workload_model_in(
        &self,
        transaction: &mut Transaction,
        worker_token: &str,
        identity: &CloudWorkerIdentity,
        lease: &RunLease,
        requested_binding: &ternilo_protocol::RunModelBinding,
        now_ms: u64,
    ) -> Result<(StartedRun, ternilo_control::WorkloadModelPrincipal), HarnessError> {
        credential_in(
            transaction,
            &token_hash(worker_token),
            Some(&identity.worker_id),
        )
        .await?;
        commands::require_worker_in(transaction, identity, now_ms).await?;
        let run = canonical_run_in(transaction, identity, lease, now_ms, true).await?;
        crate::execution_admission::require_active_in(
            transaction,
            &run,
            identity.worker_id.as_str(),
            now_ms,
        )
        .await?;
        let metadata = &run.claim.spec.metadata;
        let access = ternilo_control::resource_access_in(
            transaction,
            &run.claim.actor_user_id,
            &metadata.tenant_id,
            ternilo_control::ResourceKind::Session,
            run.claim.authorization_session_id.as_str(),
        )
        .await?;
        access.require(ternilo_control::ResourceAction::Submit)?;
        if access.owner_user_id != metadata.user_id {
            return Err(HarnessError::policy(
                "workload authority no longer belongs to its resource owner",
            ));
        }
        if run.claim.authorization_session_id != metadata.session_id {
            let lineage: i64 = sqlx::query_scalar(
                "WITH RECURSIVE lineage(session_id, parent_session_id, subagent_metadata) AS (
                    SELECT session_id, parent_session_id, subagent_metadata FROM cloud_sessions
                    WHERE tenant_id=$1 AND user_id=$2 AND workspace_id=$3 AND session_id=$4
                    UNION
                    SELECT parent.session_id, parent.parent_session_id, parent.subagent_metadata
                    FROM cloud_sessions AS parent JOIN lineage AS child ON child.parent_session_id=parent.session_id
                    WHERE parent.tenant_id=$1 AND parent.user_id=$2 AND parent.workspace_id=$3
                      AND child.subagent_metadata IS NOT NULL
                 ) SELECT COUNT(*) FROM lineage WHERE session_id=$5"
            ).bind(metadata.tenant_id.as_str()).bind(metadata.user_id.as_str())
                .bind(metadata.workspace_id.as_str()).bind(metadata.session_id.as_str())
                .bind(run.claim.authorization_session_id.as_str())
                .fetch_one(&mut **transaction).await.map_err(database_error)?;
            if lineage != 1 {
                return Err(HarnessError::policy(
                    "workload has no canonical subagent lineage to its authorization session",
                ));
            }
        }
        let snapshot = crate::profile_model_snapshot(&run.claim.spec.profile)?
            .ok_or_else(|| HarnessError::policy("workload has no configured model"))?;
        if &snapshot.binding != requested_binding
            || snapshot.binding.beneficiary_user_id() != &metadata.user_id
        {
            return Err(HarnessError::policy(
                "requested model does not match the accepted workload binding",
            ));
        }
        let reservation = sqlx::query(
            "SELECT reservation.reservation_id, reservation.user_id, reservation.reserved_model_tokens
             FROM cloud_runs AS run JOIN control_quota_reservations AS reservation
               ON reservation.tenant_id=run.tenant_id AND reservation.reservation_id=run.quota_reservation_id
             WHERE run.tenant_id=$1 AND run.run_id=$2 AND run.state='running'
               AND reservation.state='active' AND reservation.run_id=run.run_id"
        ).bind(metadata.tenant_id.as_str()).bind(metadata.run_id.as_str())
            .fetch_optional(&mut **transaction).await.map_err(database_error)?
            .ok_or_else(|| HarnessError::policy("workload is cancelled or has no active execution reservation"))?;
        let execution_owner_user_id = UserId::new(
            reservation
                .try_get::<String, _>("user_id")
                .map_err(database_error)?,
        );
        if execution_owner_user_id != metadata.user_id {
            return Err(HarnessError::policy(
                "execution reservation does not belong to the workload resource owner",
            ));
        }
        let principal = ternilo_control::WorkloadModelPrincipal {
            tenant_id: metadata.tenant_id.clone(),
            project_id: metadata
                .project_id
                .clone()
                .ok_or_else(|| HarnessError::policy("workload has no canonical project"))?,
            workspace_id: metadata.workspace_id.clone(),
            session_id: metadata.session_id.clone(),
            authorization_session_id: run.claim.authorization_session_id.clone(),
            run_id: metadata.run_id.clone(),
            actor_user_id: run.claim.actor_user_id.clone(),
            resource_owner_user_id: metadata.user_id.clone(),
            execution_owner_user_id,
            execution_reservation_id: reservation
                .try_get("reservation_id")
                .map_err(database_error)?,
            worker_id: identity.worker_id.as_str().to_owned(),
            worker_generation: identity.generation,
            lease_token: lease.lease_token,
            writer_fencing_token: lease.writer_fencing_token,
            model: snapshot.binding,
            run_token_limit: from_i64(
                reservation
                    .try_get("reserved_model_tokens")
                    .map_err(database_error)?,
                "workload token ceiling",
            )?,
        };
        Ok((run, principal))
    }

    pub(crate) async fn tenant_transaction(
        &self,
        tenant: &TenantId,
    ) -> Result<Transaction, HarnessError> {
        let mut transaction = self.begin().await?;
        set_tenant_scope(&mut transaction, tenant).await?;
        Ok(transaction)
    }

    pub(crate) async fn owner_transaction(
        &self,
        tenant: &TenantId,
        user: &UserId,
    ) -> Result<Transaction, HarnessError> {
        let mut transaction = self.tenant_transaction(tenant).await?;
        set_user_scope(&mut transaction, user).await?;
        Ok(transaction)
    }

    pub async fn release_unused_quota_reservation(
        &self,
        actor: &ternilo_control::ControlUser,
        tenant_id: &TenantId,
        reservation_id: &str,
        now_ms: u64,
    ) -> Result<(), HarnessError> {
        let mut tx = self.database.begin().await?;
        set_tenant_scope(&mut tx, tenant_id).await?;
        ternilo_control::ControlStore::workload_model_budget_in(&mut tx, tenant_id, reservation_id)
            .await?;
        // Lock the reservation before testing allocation; enqueue takes the same lock before inserting a Run.
        let owned: i64 = sqlx::query_scalar(for_update(&tx,
            "SELECT COUNT(*) FROM control_quota_reservations WHERE tenant_id=$1 AND reservation_id=$2 AND user_id=$3",
            "SELECT 1 FROM control_quota_reservations WHERE tenant_id=$1 AND reservation_id=$2 AND user_id=$3 FOR UPDATE"
        )).bind(tenant_id.as_str()).bind(reservation_id).bind(actor.user_id.as_str())
            .fetch_optional(&mut *tx).await.map_err(database_error)?.unwrap_or(0);
        if owned != 1 {
            return Err(HarnessError::policy(
                "quota reservation is not owned by this user",
            ));
        }
        let attached: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM cloud_runs WHERE tenant_id=$1 AND quota_reservation_id=$2",
        )
        .bind(tenant_id.as_str())
        .bind(reservation_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(database_error)?;
        if attached != 0 {
            return Err(HarnessError::policy(
                "allocated workload reservations must be released through execution cancellation",
            ));
        }
        ternilo_control::ControlStore::release_unallocated_workload_quota_in(
            &mut tx,
            actor,
            tenant_id,
            reservation_id,
            now_ms,
        )
        .await?;
        tx.commit().await.map_err(database_error)
    }

    pub async fn create_worker_credential(
        &self,
        worker_id: &ExecutorId,
        storage_id: &str,
        now_ms: u64,
    ) -> Result<WorkerCredentialGrant, HarnessError> {
        worker_id.validate()?;
        validate_storage_id(storage_id)?;
        let token = format!("ter_w_{}", URL_SAFE_NO_PAD.encode(random::<[u8; 32]>()));
        let mut transaction = self.begin().await?;
        let changed = sqlx::query("INSERT INTO cloud_worker_credentials(worker_id,token_hash,storage_id,root_id,created_at_ms,revoked_at_ms) VALUES($1,$2,$3,NULL,$4,NULL) ON CONFLICT(worker_id) DO NOTHING")
            .bind(worker_id.as_str()).bind(token_hash(&token)).bind(storage_id).bind(to_i64(now_ms,"worker credential time")?)
            .execute(&mut *transaction).await.map_err(database_error)?.rows_affected();
        if changed != 1 {
            return Err(HarnessError::conflict(
                "Worker id is already registered; credentials are never silently replaced",
            ));
        }
        transaction.commit().await.map_err(database_error)?;
        Ok(WorkerCredentialGrant {
            worker_id: worker_id.clone(),
            storage_id: storage_id.to_owned(),
            token,
        })
    }

    pub async fn worker_credentials(&self) -> Result<Vec<WorkerCredentialRecord>, HarnessError> {
        let now = current_time_ms()?;
        let rows = sqlx::query("SELECT credential.*, worker.last_seen_at_ms, worker.lease_expires_at_ms FROM cloud_worker_credentials AS credential LEFT JOIN cloud_workers AS worker ON worker.worker_id=credential.worker_id ORDER BY credential.worker_id")
            .fetch_all(&self.pool).await.map_err(database_error)?;
        rows.into_iter()
            .map(|row| {
                let revoked_at_ms = row
                    .try_get::<Option<i64>, _>("revoked_at_ms")
                    .map_err(database_error)?
                    .map(|value| from_i64(value, "worker revocation time"))
                    .transpose()?;
                let last_seen_at_ms = row
                    .try_get::<Option<i64>, _>("last_seen_at_ms")
                    .map_err(database_error)?
                    .map(|value| from_i64(value, "worker heartbeat time"))
                    .transpose()?;
                let lease_expires_at_ms = row
                    .try_get::<Option<i64>, _>("lease_expires_at_ms")
                    .map_err(database_error)?
                    .map(|value| from_i64(value, "worker lease expiry"))
                    .transpose()?;
                Ok(WorkerCredentialRecord {
                    worker_id: ExecutorId::new(
                        row.try_get::<String, _>("worker_id")
                            .map_err(database_error)?,
                    ),
                    storage_id: row.try_get("storage_id").map_err(database_error)?,
                    registered: row
                        .try_get::<Option<String>, _>("root_id")
                        .map_err(database_error)?
                        .is_some(),
                    online: revoked_at_ms.is_none()
                        && lease_expires_at_ms.is_some_and(|expiry| expiry > now),
                    last_seen_at_ms,
                    lease_expires_at_ms,
                    created_at_ms: from_i64(
                        row.try_get("created_at_ms").map_err(database_error)?,
                        "worker creation time",
                    )?,
                    revoked_at_ms,
                })
            })
            .collect()
    }

    pub async fn revoke_worker_credential(
        &self,
        worker_id: &ExecutorId,
        now_ms: u64,
    ) -> Result<(), HarnessError> {
        worker_id.validate()?;
        let mut transaction = self.begin().await?;
        // Credential is locked first everywhere, including generation replacement and run writes.
        let present = sqlx::query(for_update(
            &transaction,
            "SELECT worker_id FROM cloud_worker_credentials WHERE worker_id=$1",
            "SELECT worker_id FROM cloud_worker_credentials WHERE worker_id=$1 FOR UPDATE",
        ))
        .bind(worker_id.as_str())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?;
        if present.is_none() {
            return Err(HarnessError::invalid("Worker credential does not exist"));
        }
        sqlx::query("UPDATE cloud_worker_credentials SET revoked_at_ms=COALESCE(revoked_at_ms,$2) WHERE worker_id=$1")
            .bind(worker_id.as_str()).bind(to_i64(now_ms,"worker revocation time")?).execute(&mut *transaction).await.map_err(database_error)?;
        sqlx::query("UPDATE cloud_workers SET lease_expires_at_ms=$2 WHERE worker_id=$1")
            .bind(worker_id.as_str())
            .bind(to_i64(now_ms, "worker revocation time")?)
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
        crate::store::drain_worker_runs_in(&mut transaction, worker_id.as_str(), now_ms).await?;
        transaction.commit().await.map_err(database_error)
    }

    pub async fn worker_configuration(
        &self,
        token: &str,
    ) -> Result<WorkerConfiguration, HarnessError> {
        let mut transaction = self.database.begin().await?;
        let credential = credential_in(&mut transaction, &token_hash(token), None).await?;
        let storage_id: String = credential.try_get("storage_id").map_err(database_error)?;
        let registered_root = sqlx::query_scalar::<_, String>(
            "SELECT root_id FROM cloud_storage_roots WHERE storage_id=$1",
        )
        .bind(&storage_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?;
        let credential_root: Option<String> =
            credential.try_get("root_id").map_err(database_error)?;
        if registered_root
            .as_ref()
            .zip(credential_root.as_ref())
            .is_some_and(|(storage, credential)| storage != credential)
        {
            return Err(HarnessError::policy(
                "Worker credential does not match its registered storage root",
            ));
        }
        let result = WorkerConfiguration {
            worker_id: ExecutorId::new(
                credential
                    .try_get::<String, _>("worker_id")
                    .map_err(database_error)?,
            ),
            storage_id,
            expected_root_id: registered_root.or(credential_root),
        };
        transaction.commit().await.map_err(database_error)?;
        Ok(result)
    }

    pub async fn register_authenticated_worker(
        &self,
        token: &str,
        request: &WorkerRegisterRequest,
        lease: Duration,
        now_ms: u64,
    ) -> Result<CloudWorkerIdentity, HarnessError> {
        request.capacity.validate()?;
        validate_storage_id(&request.storage_id)?;
        validate_storage_id(&request.root_id)?;
        request.hello.validate()?;
        let mut transaction = self.database.begin().await?;
        let credential = credential_in(
            &mut transaction,
            &token_hash(token),
            Some(&request.hello.executor_id),
        )
        .await?;
        if credential
            .try_get::<String, _>("storage_id")
            .map_err(database_error)?
            != request.storage_id
        {
            return Err(HarnessError::policy(
                "Worker storage does not match its credential",
            ));
        }
        let root: Option<String> = credential.try_get("root_id").map_err(database_error)?;
        if root.as_ref().is_some_and(|root| root != &request.root_id) {
            return Err(HarnessError::policy(
                "Worker data root differs from its registered persistent volume",
            ));
        }
        sqlx::query("INSERT INTO cloud_storage_roots(storage_id,root_id,registered_at_ms) VALUES($1,$2,$3) ON CONFLICT(storage_id) DO NOTHING")
            .bind(&request.storage_id).bind(&request.root_id).bind(to_i64(now_ms,"storage registration time")?)
            .execute(&mut *transaction).await.map_err(database_error)?;
        let storage_root: String =
            sqlx::query_scalar("SELECT root_id FROM cloud_storage_roots WHERE storage_id=$1")
                .bind(&request.storage_id)
                .fetch_one(&mut *transaction)
                .await
                .map_err(database_error)?;
        if storage_root != request.root_id {
            return Err(HarnessError::policy(
                "Workers assigned to the same storage must use the same persistent volume",
            ));
        }
        sqlx::query(
            "UPDATE cloud_worker_credentials SET root_id=$2 WHERE worker_id=$1 AND root_id IS NULL",
        )
        .bind(request.hello.executor_id.as_str())
        .bind(&request.root_id)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        let previous = sqlx::query_scalar::<_, String>(for_update(
            &transaction,
            "SELECT instance_nonce FROM cloud_workers WHERE worker_id=$1",
            "SELECT instance_nonce FROM cloud_workers WHERE worker_id=$1 FOR UPDATE",
        ))
        .bind(request.hello.executor_id.as_str())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?;
        crate::execution_admission::pool_gate(&mut transaction, &request.storage_id).await?;
        if previous
            .as_ref()
            .is_some_and(|nonce| nonce != &request.hello.instance_nonce)
        {
            crate::store::drain_worker_runs_in(
                &mut transaction,
                request.hello.executor_id.as_str(),
                now_ms,
            )
            .await?;
        }
        let identity =
            commands::register_worker_in(&mut transaction, &request.hello, lease, now_ms).await?;
        sqlx::query(
            "UPDATE cloud_workers SET max_active_runs=$2,max_resident_runs=$3 WHERE worker_id=$1",
        )
        .bind(identity.worker_id.as_str())
        .bind(i64::from(request.capacity.max_active_runs))
        .bind(i64::from(request.capacity.max_resident_runs))
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        transaction.commit().await.map_err(database_error)?;
        Ok(identity)
    }

    pub async fn authenticated_worker(
        &self,
        token: &str,
        identity: &CloudWorkerIdentity,
        now_ms: u64,
    ) -> Result<Self, HarnessError> {
        let hash = token_hash(token);
        let mut transaction = self.database.begin().await?;
        credential_in(&mut transaction, &hash, Some(&identity.worker_id)).await?;
        commands::require_worker_in(&mut transaction, identity, now_ms).await?;
        transaction.commit().await.map_err(database_error)?;
        let mut store = self.clone();
        store.worker_access = Some(WorkerAccess {
            token_hash: hash,
            identity: identity.clone(),
            run: None,
            command: None,
        });
        Ok(store)
    }

    pub async fn authorized_run(
        &self,
        lease: &RunLease,
        now_ms: u64,
    ) -> Result<(Self, StartedRun), HarnessError> {
        let access = self
            .worker_access
            .as_ref()
            .ok_or_else(|| HarnessError::policy("Worker authentication is required"))?;
        let mut transaction = self.begin().await?;
        let run = canonical_run_in(&mut transaction, &access.identity, lease, now_ms, true).await?;
        transaction.commit().await.map_err(database_error)?;
        let mut store = self.clone();
        store
            .worker_access
            .as_mut()
            .expect("authenticated Worker")
            .run = Some(lease.clone());
        Ok((store, run))
    }

    pub async fn authorized_claim(
        &self,
        lease: &RunLease,
        now_ms: u64,
    ) -> Result<CloudRunClaim, HarnessError> {
        let access = self
            .worker_access
            .as_ref()
            .ok_or_else(|| HarnessError::policy("Worker authentication is required"))?;
        let mut transaction = self.begin().await?;
        let run =
            canonical_run_in(&mut transaction, &access.identity, lease, now_ms, false).await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(run.claim)
    }

    pub async fn authorized_command(
        &self,
        lease: &CommandLease,
        now_ms: u64,
    ) -> Result<(Self, ClaimedCloudSessionCommand), HarnessError> {
        let access = self
            .worker_access
            .as_ref()
            .ok_or_else(|| HarnessError::policy("Worker authentication is required"))?;
        let mut transaction = self.begin().await?;
        let command =
            canonical_command_in(&mut transaction, &access.identity, lease, now_ms).await?;
        transaction.commit().await.map_err(database_error)?;
        let mut store = self.clone();
        store
            .worker_access
            .as_mut()
            .expect("authenticated Worker")
            .command = Some(lease.clone());
        Ok((store, command))
    }

    /// A lost acknowledgement can be retried only by the completing generation.
    pub async fn complete_authenticated_command(
        &self,
        lease: &CommandLease,
        outcome: ternilo_transport::CommandOutcome,
        now_ms: u64,
    ) -> Result<(), HarnessError> {
        let access = self
            .worker_access
            .as_ref()
            .ok_or_else(|| HarnessError::policy("Worker authentication is required"))?;
        let mut transaction = self.begin().await?;
        let row = commands::command_in(
            &mut transaction,
            &lease.tenant_id,
            &lease.user_id,
            &lease.command_id,
        )
        .await?
        .ok_or_else(|| HarnessError::policy("Worker command lease is not available"))?;
        if row.try_get::<String, _>("state").map_err(database_error)? == "completed" {
            let reply = row
                .try_get::<Option<Json<ternilo_transport::CommandReply>>, _>("reply_json")
                .map_err(database_error)?;
            let same_generation = row
                .try_get::<Option<String>, _>("lease_owner")
                .map_err(database_error)?
                .as_deref()
                == Some(access.identity.worker_id.as_str())
                && row
                    .try_get::<Option<i64>, _>("worker_generation")
                    .map_err(database_error)?
                    == Some(to_i64(access.identity.generation, "worker generation")?)
                && row
                    .try_get::<i64, _>("attempt_count")
                    .map_err(database_error)?
                    == i64::from(lease.attempt_count);
            if same_generation && reply.is_some_and(|reply| reply.0.outcome == outcome) {
                transaction.commit().await.map_err(database_error)?;
                return Ok(());
            }
            return Err(HarnessError::policy(
                "Worker command completion differs from its recorded result or generation",
            ));
        }
        canonical_command_in(&mut transaction, &access.identity, lease, now_ms).await?;
        let reply = ternilo_transport::CommandReply {
            command_id: lease.command_id.clone(),
            completed_at_ms: now_ms,
            outcome,
        };
        commands::finish_command_in(&mut transaction, &row, "completed", Some(&reply), now_ms)
            .await?;
        transaction.commit().await.map_err(database_error)
    }
}

async fn credential_in(
    transaction: &mut Transaction,
    hash: &str,
    worker: Option<&ExecutorId>,
) -> Result<AnyRow, HarnessError> {
    let row=sqlx::query(for_update(transaction,"SELECT * FROM cloud_worker_credentials WHERE token_hash=$1 AND revoked_at_ms IS NULL","SELECT * FROM cloud_worker_credentials WHERE token_hash=$1 AND revoked_at_ms IS NULL FOR UPDATE"))
        .bind(hash).fetch_optional(&mut **transaction).await.map_err(database_error)?
        .ok_or_else(|| HarnessError::policy("Worker credential is invalid or revoked"))?;
    if worker.is_some_and(|worker| {
        row.try_get::<String, _>("worker_id").ok().as_deref() != Some(worker.as_str())
    }) {
        return Err(HarnessError::policy(
            "Worker identity does not match its credential",
        ));
    }
    Ok(row)
}

async fn canonical_run_in(
    transaction: &mut Transaction,
    worker: &CloudWorkerIdentity,
    lease: &RunLease,
    now: u64,
    started: bool,
) -> Result<StartedRun, HarnessError> {
    lease.tenant_id.validate()?;
    lease.run_id.validate()?;
    set_tenant_scope(transaction, &lease.tenant_id).await?;
    let row = sqlx::query("SELECT * FROM cloud_runs WHERE tenant_id=$1 AND run_id=$2")
        .bind(lease.tenant_id.as_str())
        .bind(lease.run_id.as_str())
        .fetch_optional(&mut **transaction)
        .await
        .map_err(database_error)?
        .ok_or_else(|| HarnessError::policy("Worker run lease is not available"))?;
    let state: String = row.try_get("state").map_err(database_error)?;
    let now_i64 = to_i64(now, "run lease time")?;
    if row
        .try_get::<Option<String>, _>("lease_owner")
        .map_err(database_error)?
        .as_deref()
        != Some(worker.worker_id.as_str())
        || from_i64(
            row.try_get("lease_token").map_err(database_error)?,
            "run lease token",
        )? != lease.lease_token
        || row
            .try_get::<Option<i64>, _>("lease_expires_at_ms")
            .map_err(database_error)?
            .is_none_or(|expiry| expiry <= now_i64)
        || (started && !matches!(state.as_str(), "running" | "cancel_requested"))
        || (!started && state != "leased")
    {
        return Err(HarnessError::policy(
            "Worker run lease is no longer current",
        ));
    }
    let spec = row
        .try_get::<Json<ternilo_protocol::RunSpec>, _>("spec")
        .map_err(database_error)?
        .0;
    let digest: Vec<u8> = row.try_get("spec_digest").map_err(database_error)?;
    let run = StartedRun {
        claim: CloudRunClaim {
            provenance: crate::input_provenance::stored_provenance(&row)?,
            actor_user_id: UserId::new(
                row.try_get::<String, _>("actor_user_id")
                    .map_err(database_error)?,
            ),
            authorization_session_id: SessionId::new(
                row.try_get::<String, _>("authorization_session_id")
                    .map_err(database_error)?,
            ),
            tenant_id: lease.tenant_id.clone(),
            run_id: lease.run_id.clone(),
            session_id: SessionId::new(
                row.try_get::<String, _>("session_id")
                    .map_err(database_error)?,
            ),
            workspace_use: crate::workspace_occupancy::ticket_for_run_in(transaction, lease)
                .await?
                .ok_or_else(|| HarnessError::execution("cloud run has no workspace occupancy"))?,
            lease_token: lease.lease_token,
            spec,
            spec_digest: digest
                .try_into()
                .map_err(|_| HarnessError::execution("invalid stored run digest"))?,
        },
        fencing_token: lease.writer_fencing_token,
        prior_events: Vec::new(),
    };
    if started {
        crate::store::require_writer_in(transaction, &run, worker.worker_id.as_str(), Some(now))
            .await?;
    }
    Ok(run)
}

async fn canonical_command_in(
    transaction: &mut Transaction,
    worker: &CloudWorkerIdentity,
    lease: &CommandLease,
    now: u64,
) -> Result<ClaimedCloudSessionCommand, HarnessError> {
    let row = commands::command_in(
        transaction,
        &lease.tenant_id,
        &lease.user_id,
        &lease.command_id,
    )
    .await?
    .ok_or_else(|| HarnessError::policy("Worker command lease is not available"))?;
    let command = commands::decode_claimed_command(&row)?;
    if command.attempt_count != lease.attempt_count
        || !commands::command_owned(&row, worker, now)?
        || !commands::target_is_current(transaction, &row, worker, now).await?
    {
        return Err(HarnessError::policy(
            "Worker command lease is no longer current",
        ));
    }
    Ok(command)
}

fn token_hash(token: &str) -> String {
    Sha256::digest(token.as_bytes())
        .iter()
        .fold(String::with_capacity(64), |mut text, byte| {
            write!(text, "{byte:02x}").expect("String formatting succeeds");
            text
        })
}

fn validate_storage_id(value: &str) -> Result<(), HarnessError> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(HarnessError::invalid(
            "Worker storage and root ids must use 1 to 128 ASCII letters, digits, dots, hyphens or underscores",
        ));
    }
    Ok(())
}

fn current_time_ms() -> Result<u64, HarnessError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| HarnessError::execution("system clock is before Unix epoch"))?
        .as_millis()
        .try_into()
        .map_err(|_| HarnessError::execution("system time exceeds u64"))
}

impl CloudStore {
    pub async fn authorized_telemetry(
        &self,
        lease: &crate::TelemetryLease,
    ) -> Result<crate::ClaimedCloudTelemetry, HarnessError> {
        let access = self
            .worker_access
            .as_ref()
            .ok_or_else(|| HarnessError::policy("Worker authentication is required"))?;
        let mut transaction = self
            .owner_transaction(&lease.tenant_id, &lease.user_id)
            .await?;
        let row=sqlx::query("SELECT item.*, session.agent_id FROM cloud_telemetry_outbox AS item JOIN cloud_sessions AS session ON session.tenant_id=item.tenant_id AND session.session_id=item.session_id WHERE item.tenant_id=$1 AND item.user_id=$2 AND item.occurrence_id=$3 AND item.lease_owner=$4 AND item.worker_generation=$5 AND item.attempt_count=$6 AND item.state='inflight'")
            .bind(lease.tenant_id.as_str()).bind(lease.user_id.as_str()).bind(&lease.occurrence_id).bind(access.identity.worker_id.as_str())
            .bind(to_i64(access.identity.generation,"Worker generation")?).bind(i64::from(lease.attempt_count))
            .fetch_optional(&mut *transaction).await.map_err(database_error)?
            .ok_or_else(||HarnessError::policy("Worker telemetry lease is not available"))?;
        let result = crate::ClaimedCloudTelemetry {
            identity: ternilo_protocol::SessionIdentity {
                tenant_id: lease.tenant_id.clone(),
                user_id: lease.user_id.clone(),
                session_id: SessionId::new(
                    row.try_get::<String, _>("session_id")
                        .map_err(database_error)?,
                ),
                agent_id: ternilo_protocol::AgentId::new(
                    row.try_get::<String, _>("agent_id")
                        .map_err(database_error)?,
                ),
            },
            occurrence_id: lease.occurrence_id.clone(),
            from_seq: from_i64(
                row.try_get("from_seq").map_err(database_error)?,
                "telemetry start",
            )?,
            to_seq: from_i64(
                row.try_get("to_seq").map_err(database_error)?,
                "telemetry end",
            )?,
            attempt_count: lease.attempt_count,
            events: Vec::new(),
        };
        transaction.commit().await.map_err(database_error)?;
        Ok(result)
    }
}
