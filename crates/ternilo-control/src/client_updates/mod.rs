use crate::{
    ControlStore, ControlUser, EdgeStore,
    computer_management::require_computer_in,
    crypto::token_hash,
    store::{append_audit, to_i64},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::Row;
use ternilo_protocol::{
    ClientUpdateInfo, ClientUpdatePhase, ClientUpdateProgress, HarnessError, TenantId,
};
use ternilo_storage::{Database, Json, Transaction, database_error};
use ternilo_transport::{ExecutorHello, ExecutorId};

#[derive(Clone, Serialize, Deserialize)]
pub struct ClientUpdateJob {
    pub progress: ClientUpdateProgress,
    pub target: String,
    pub manifest: String,
    pub signature: String,
    pub created_at_ms: u64,
    pub expires_at_ms: u64,
}

pub(crate) async fn initialize(database: &Database) -> Result<(), HarnessError> {
    database
        .initialize(
            "client_updates",
            1,
            include_str!("schema.sql"),
            include_str!("postgres.sql"),
        )
        .await
}

async fn read_job(
    tx: &mut Transaction,
    tenant: &TenantId,
    executor: &ExecutorId,
) -> Result<Option<ClientUpdateJob>, HarnessError> {
    Ok(sqlx::query_scalar::<_, Json<ClientUpdateJob>>(
        "SELECT job_json FROM control_client_updates WHERE tenant_id=$1 AND executor_id=$2",
    )
    .bind(tenant.as_str())
    .bind(executor.as_str())
    .fetch_optional(&mut **tx)
    .await
    .map_err(database_error)?
    .map(|json| json.0))
}
fn digest_token(token: &str) -> String {
    use std::fmt::Write as _;
    token_hash(token)
        .iter()
        .fold(String::with_capacity(64), |mut output, byte| {
            write!(output, "{byte:02x}").expect("string write");
            output
        })
}

impl ControlStore {
    pub async fn client_update_job(
        &self,
        actor: &ControlUser,
        tenant: &TenantId,
        executor: &ExecutorId,
    ) -> Result<Option<ClientUpdateJob>, HarnessError> {
        let mut tx = self.database().tenant_transaction(tenant).await?;
        require_computer_in(&mut tx, actor, tenant, executor, true, false).await?;
        let job = read_job(&mut tx, tenant, executor).await?;
        tx.commit().await.map_err(database_error)?;
        Ok(job)
    }

    /// One owner-authorized operation per computer. The download grant never enters audit JSON.
    pub async fn create_client_update_job(
        &self,
        actor: &ControlUser,
        tenant: &TenantId,
        executor: &ExecutorId,
        job: &ClientUpdateJob,
        token: &str,
    ) -> Result<bool, HarnessError> {
        let mut tx = self.database().tenant_transaction(tenant).await?;
        let (_, _, state) =
            require_computer_in(&mut tx, actor, tenant, executor, true, true).await?;
        let management =
            crate::computer_management::management_in(&mut tx, tenant, executor).await?;
        if state == "revoked"
            || management.suspended_at_ms.is_some()
            || management.removed_at_ms.is_some()
        {
            return Err(HarnessError::policy(
                "this computer cannot accept remote upgrades",
            ));
        }
        if let Some(previous) = read_job(&mut tx, tenant, executor).await? {
            if previous.progress.id == job.progress.id {
                if previous.progress.version != job.progress.version {
                    return Err(HarnessError::conflict(
                        "update identifier was reused for a different release",
                    ));
                }
                return Ok(false);
            }
            let preparation_expired = previous.progress.phase == ClientUpdatePhase::Preparing
                && previous.created_at_ms.saturating_add(15 * 60 * 1000) < job.created_at_ms;
            if !previous.progress.phase.terminal() && !preparation_expired {
                return Err(HarnessError::conflict(
                    "another computer update is still active",
                ));
            }
        }
        sqlx::query("INSERT INTO control_client_updates (tenant_id,executor_id,operation_id,job_json,download_token_hash,download_expires_at_ms) VALUES ($1,$2,$3,$4,$5,$6) ON CONFLICT(tenant_id,executor_id) DO UPDATE SET operation_id=EXCLUDED.operation_id,job_json=EXCLUDED.job_json,download_token_hash=EXCLUDED.download_token_hash,download_expires_at_ms=EXCLUDED.download_expires_at_ms")
            .bind(tenant.as_str()).bind(executor.as_str()).bind(&job.progress.id).bind(Json(job))
            .bind(digest_token(token)).bind(to_i64(job.created_at_ms.saturating_add(30 * 60 * 1000),"download grant expiry")?)
            .execute(&mut *tx).await.map_err(database_error)?;
        append_audit(
            &mut tx,
            tenant,
            Some(&actor.user_id),
            "user",
            "computer.update.requested",
            "executor",
            executor.as_str(),
            "success",
            json!({"id":job.progress.id,"version":job.progress.version,"target":job.target}),
            job.created_at_ms,
        )
        .await?;
        tx.commit().await.map_err(database_error)?;
        Ok(true)
    }

    pub async fn fail_client_update_preparation(
        &self,
        tenant: &TenantId,
        executor: &ExecutorId,
        id: &str,
        error: &str,
    ) -> Result<(), HarnessError> {
        let mut tx = self.database().tenant_transaction(tenant).await?;
        ternilo_storage::lock(&mut tx, &format!("computer-management:{tenant}:{executor}")).await?;
        if let Some(mut job) = read_job(&mut tx, tenant, executor).await?
            && job.progress.id == id
            && job.progress.phase == ClientUpdatePhase::Preparing
        {
            job.progress.phase = ClientUpdatePhase::Failed;
            job.progress.error = Some(error.chars().take(2000).collect());
            sqlx::query("UPDATE control_client_updates SET job_json=$3 WHERE tenant_id=$1 AND executor_id=$2")
                .bind(tenant.as_str()).bind(executor.as_str()).bind(Json(&job)).execute(&mut *tx).await.map_err(database_error)?;
        }
        tx.commit().await.map_err(database_error)
    }

    pub async fn client_update_download(
        &self,
        tenant: &TenantId,
        executor: &ExecutorId,
        id: &str,
        token: &str,
        now: u64,
    ) -> Result<ClientUpdateJob, HarnessError> {
        let mut tx = self.database().tenant_transaction(tenant).await?;
        let row = sqlx::query("SELECT u.job_json FROM control_client_updates u JOIN control_executors e ON e.tenant_id=u.tenant_id AND e.executor_id=u.executor_id JOIN control_users owner ON owner.user_id=e.owner_user_id LEFT JOIN control_computer_management m ON m.tenant_id=e.tenant_id AND m.executor_id=e.executor_id WHERE u.tenant_id=$1 AND u.executor_id=$2 AND u.operation_id=$3 AND u.download_token_hash=$4 AND u.download_expires_at_ms>$5 AND e.state<>'revoked' AND owner.status='active' AND m.suspended_at_ms IS NULL AND m.removed_at_ms IS NULL")
            .bind(tenant.as_str()).bind(executor.as_str()).bind(id).bind(digest_token(token)).bind(to_i64(now,"download time")?)
            .fetch_optional(&mut *tx).await.map_err(database_error)?
            .ok_or_else(|| HarnessError::policy("update download authorization is invalid, expired or revoked"))?;
        let job = row
            .try_get::<Json<ClientUpdateJob>, _>("job_json")
            .map_err(database_error)?
            .0;
        if job.progress.phase.terminal() {
            return Err(HarnessError::policy(
                "update download authorization is no longer active",
            ));
        }
        tx.commit().await.map_err(database_error)?;
        Ok(job)
    }
}

impl EdgeStore {
    /// Compare the persisted connection nonce so a replaced connection cannot overwrite new state.
    pub async fn record_client_update_state(
        &self,
        tenant: &TenantId,
        executor: &ExecutorId,
        nonce: &str,
        info: &ClientUpdateInfo,
    ) -> Result<bool, HarnessError> {
        info.validate()?;
        let mut tx = self.database().tenant_transaction(tenant).await?;
        ternilo_storage::lock(&mut tx, &format!("computer-management:{tenant}:{executor}")).await?;
        let Some(original) = sqlx::query_scalar::<_, String>(
            "SELECT hello_json FROM control_edge_executors WHERE tenant_id=$1 AND executor_id=$2",
        )
        .bind(tenant.as_str())
        .bind(executor.as_str())
        .fetch_optional(&mut *tx)
        .await
        .map_err(database_error)?
        else {
            return Ok(false);
        };
        let mut hello: ExecutorHello = serde_json::from_str(&original)
            .map_err(|_| HarnessError::execution("invalid persisted computer handshake"))?;
        if hello.instance_nonce != nonce {
            return Ok(false);
        }
        let Some(client) = &hello.client else {
            return Err(HarnessError::policy(
                "computer did not declare lifecycle support",
            ));
        };
        if client.version != info.version || client.target != info.target {
            return Err(HarnessError::policy(
                "computer update status changed the connection's build identity",
            ));
        }
        let mut info = info.clone();
        if let (Some(previous), Some(next)) = (&client.progress, &info.progress)
            && previous.id == next.id
            && previous.revision > next.revision
        {
            info.progress = Some(previous.clone());
        }
        hello.client = Some(info.clone());
        let changed = sqlx::query("UPDATE control_edge_executors SET hello_json=$3 WHERE tenant_id=$1 AND executor_id=$2 AND hello_json=$4")
            .bind(tenant.as_str()).bind(executor.as_str()).bind(Json(&hello)).bind(original).execute(&mut *tx).await.map_err(database_error)?.rows_affected() == 1;
        if changed
            && let Some(progress) = &info.progress
            && let Some(mut job) = read_job(&mut tx, tenant, executor).await?
            && job.progress.id == progress.id
            && job.progress.version == progress.version
            && progress.revision > job.progress.revision
        {
            if progress.phase == ClientUpdatePhase::Completed && info.version != progress.version {
                return Err(HarnessError::policy(
                    "computer did not reconnect with the requested version",
                ));
            }
            job.progress = progress.clone();
            sqlx::query("UPDATE control_client_updates SET job_json=$3 WHERE tenant_id=$1 AND executor_id=$2")
                .bind(tenant.as_str()).bind(executor.as_str()).bind(Json(&job)).execute(&mut *tx).await.map_err(database_error)?;
        }
        tx.commit().await.map_err(database_error)?;
        Ok(changed)
    }
}
