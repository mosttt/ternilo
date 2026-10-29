use crate::{
    ControlStore, ControlUser, NodePrincipal,
    computer_management::require_computer_in,
    store::{from_i64, to_i64},
};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use ternilo_protocol::{ClientUpdateInfo, HarnessError, TenantId};
use ternilo_storage::{Database, Json, Transaction, database_error};
use ternilo_transport::{
    ClientUpdateCommand, ClientUpdateRequest, ExecutorId, MAINTENANCE_LEASE_MS, MaintenanceCommand,
    MaintenanceHello, MaintenanceOperation, MaintenanceReply,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MaintenanceState {
    pub hello: MaintenanceHello,
    pub last_seen_at_ms: u64,
}
impl MaintenanceState {
    #[must_use]
    pub fn online(&self, now: u64) -> bool {
        self.last_seen_at_ms.saturating_add(MAINTENANCE_LEASE_MS) > now
    }
}
pub(crate) async fn initialize(database: &Database) -> Result<(), HarnessError> {
    database
        .initialize(
            "client_maintenance",
            1,
            include_str!("schema.sql"),
            include_str!("postgres.sql"),
        )
        .await
}
async fn state_in(
    tx: &mut Transaction,
    tenant: &TenantId,
    executor: &ExecutorId,
) -> Result<Option<MaintenanceState>, HarnessError> {
    let row = sqlx::query("SELECT hello_json,last_seen_at_ms FROM control_client_maintenance WHERE tenant_id=$1 AND executor_id=$2")
        .bind(tenant.as_str()).bind(executor.as_str()).fetch_optional(&mut **tx).await.map_err(database_error)?;
    row.map(|row| {
        Ok(MaintenanceState {
            hello: row
                .try_get::<Json<MaintenanceHello>, _>("hello_json")
                .map_err(database_error)?
                .0,
            last_seen_at_ms: from_i64(
                row.try_get("last_seen_at_ms").map_err(database_error)?,
                "maintenance last seen",
            )?,
        })
    })
    .transpose()
}
impl ControlStore {
    pub async fn maintenance_state(
        &self,
        actor: &ControlUser,
        tenant: &TenantId,
        executor: &ExecutorId,
    ) -> Result<Option<MaintenanceState>, HarnessError> {
        let mut tx = self.database().tenant_transaction(tenant).await?;
        require_computer_in(&mut tx, actor, tenant, executor, true, false).await?;
        let result = state_in(&mut tx, tenant, executor).await?;
        tx.commit().await.map_err(database_error)?;
        Ok(result)
    }
    pub async fn maintenance_hello(
        &self,
        node: &NodePrincipal,
        hello: &MaintenanceHello,
        now: u64,
    ) -> Result<(), HarnessError> {
        hello.validate()?;
        if hello.executor_id != node.executor_id {
            return Err(HarnessError::policy(
                "maintenance identity does not match its credential",
            ));
        }
        let tenant = &node.scope.tenant_id;
        let mut tx = self.database().tenant_transaction(tenant).await?;
        ternilo_storage::lock(
            &mut tx,
            &format!("computer-management:{tenant}:{}", node.executor_id),
        )
        .await?;
        crate::edge_store::require_node_credential_in(&mut tx, node).await?;
        sqlx::query("INSERT INTO control_client_maintenance (tenant_id,executor_id,credential_id,instance_nonce,hello_json,last_seen_at_ms) VALUES ($1,$2,$3,$4,$5,$6) ON CONFLICT(tenant_id,executor_id) DO UPDATE SET credential_id=EXCLUDED.credential_id,instance_nonce=EXCLUDED.instance_nonce,hello_json=EXCLUDED.hello_json,last_seen_at_ms=EXCLUDED.last_seen_at_ms")
            .bind(tenant.as_str()).bind(node.executor_id.as_str()).bind(&node.credential_id).bind(&hello.instance_nonce).bind(Json(hello)).bind(to_i64(now,"maintenance time")?).execute(&mut *tx).await.map_err(database_error)?;
        crate::client_updates::update_job_progress(&mut tx, tenant, &node.executor_id, &hello.info)
            .await?;
        tx.commit().await.map_err(database_error)
    }
    /// Polling carries no application RPCs. Every delivery rechecks the enrolled credential.
    pub async fn maintenance_poll(
        &self,
        node: &NodePrincipal,
        nonce: &str,
        info: &ClientUpdateInfo,
        reply: Option<&MaintenanceReply>,
        now: u64,
    ) -> Result<Option<MaintenanceCommand>, HarnessError> {
        info.validate()?;
        let tenant = &node.scope.tenant_id;
        let executor = &node.executor_id;
        let mut tx = self.database().tenant_transaction(tenant).await?;
        ternilo_storage::lock(&mut tx, &format!("computer-management:{tenant}:{executor}")).await?;
        crate::edge_store::require_node_credential_in(&mut tx, node).await?;
        let mut state = state_in(&mut tx, tenant, executor)
            .await?
            .ok_or_else(|| HarnessError::policy("maintenance handshake is required"))?;
        if state.hello.instance_nonce != nonce
            || state.hello.info.version != info.version
            || state.hello.info.target != info.target
        {
            return Err(HarnessError::policy(
                "maintenance instance was replaced or its build identity changed",
            ));
        }
        let mut info = info.clone();
        if let (Some(previous), Some(next)) = (&state.hello.info.progress, &info.progress)
            && previous.id == next.id
            && previous.revision > next.revision
        {
            info.progress = Some(previous.clone());
        }
        state.hello.info = info.clone();
        let changed = sqlx::query("UPDATE control_client_maintenance SET hello_json=$3,last_seen_at_ms=$4 WHERE tenant_id=$1 AND executor_id=$2 AND credential_id=$5")
            .bind(tenant.as_str()).bind(executor.as_str()).bind(Json(&state.hello)).bind(to_i64(now,"maintenance time")?).bind(&node.credential_id).execute(&mut *tx).await.map_err(database_error)?.rows_affected();
        if changed != 1 {
            return Err(HarnessError::policy("maintenance credential was replaced"));
        }
        crate::client_updates::update_job_progress(&mut tx, tenant, executor, &info).await?;
        if let Some(reply) = reply {
            if reply.id.len() > 100
                || reply.error.as_ref().is_some_and(|s| s.len() > 4096)
                || reply.info.is_some() == reply.error.is_some()
            {
                return Err(HarnessError::invalid("invalid maintenance reply"));
            }
            if let Some(info) = &reply.info {
                info.validate()?;
                if info.version != state.hello.info.version
                    || info.target != state.hello.info.target
                {
                    return Err(HarnessError::policy(
                        "maintenance reply changed its build identity",
                    ));
                }
            }
            sqlx::query("UPDATE control_maintenance_commands SET reply_json=$4 WHERE tenant_id=$1 AND executor_id=$2 AND command_id=$3 AND reply_json IS NULL AND expires_at_ms>$5")
                .bind(tenant.as_str()).bind(executor.as_str()).bind(&reply.id).bind(Json(reply)).bind(to_i64(now,"maintenance reply time")?).execute(&mut *tx).await.map_err(database_error)?;
        }
        let command = next_command(&mut tx, tenant, executor, now).await?;
        tx.commit().await.map_err(database_error)?;
        Ok(command)
    }
    pub async fn enqueue_maintenance_command(
        &self,
        actor: &ControlUser,
        tenant: &TenantId,
        executor: &ExecutorId,
        operation: MaintenanceOperation,
        now: u64,
    ) -> Result<String, HarnessError> {
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
                "this computer cannot accept maintenance commands",
            ));
        }
        if state_in(&mut tx, tenant, executor)
            .await?
            .is_none_or(|state| !state.online(now))
        {
            return Err(HarnessError::unavailable(
                "computer maintenance channel is offline",
            ));
        }
        let id = crate::crypto::random_identifier("mnt");
        sqlx::query("DELETE FROM control_maintenance_commands WHERE tenant_id=$1 AND executor_id=$2 AND expires_at_ms<=$3")
            .bind(tenant.as_str()).bind(executor.as_str()).bind(to_i64(now,"maintenance time")?).execute(&mut *tx).await.map_err(database_error)?;
        sqlx::query("INSERT INTO control_maintenance_commands (tenant_id,executor_id,command_id,operation_json,created_at_ms,expires_at_ms) VALUES ($1,$2,$3,$4,$5,$6)")
            .bind(tenant.as_str()).bind(executor.as_str()).bind(&id).bind(Json(operation)).bind(to_i64(now,"maintenance time")?).bind(to_i64(now.saturating_add(60_000),"maintenance expiry")?).execute(&mut *tx).await.map_err(database_error)?;
        tx.commit().await.map_err(database_error)?;
        Ok(id)
    }
    pub async fn take_maintenance_reply(
        &self,
        tenant: &TenantId,
        executor: &ExecutorId,
        id: &str,
    ) -> Result<Option<MaintenanceReply>, HarnessError> {
        let mut tx = self.database().tenant_transaction(tenant).await?;
        let reply = sqlx::query_scalar::<_, Option<Json<MaintenanceReply>>>("SELECT reply_json FROM control_maintenance_commands WHERE tenant_id=$1 AND executor_id=$2 AND command_id=$3")
            .bind(tenant.as_str()).bind(executor.as_str()).bind(id).fetch_optional(&mut *tx).await.map_err(database_error)?.flatten().map(|r|r.0);
        if reply.is_some() {
            sqlx::query("DELETE FROM control_maintenance_commands WHERE tenant_id=$1 AND executor_id=$2 AND command_id=$3").bind(tenant.as_str()).bind(executor.as_str()).bind(id).execute(&mut *tx).await.map_err(database_error)?;
        }
        tx.commit().await.map_err(database_error)?;
        Ok(reply)
    }
}
async fn next_command(
    tx: &mut Transaction,
    tenant: &TenantId,
    executor: &ExecutorId,
    now: u64,
) -> Result<Option<MaintenanceCommand>, HarnessError> {
    let row = sqlx::query("SELECT command_id,operation_json,expires_at_ms FROM control_maintenance_commands WHERE tenant_id=$1 AND executor_id=$2 AND reply_json IS NULL AND expires_at_ms>$3 ORDER BY created_at_ms,command_id LIMIT 1")
        .bind(tenant.as_str()).bind(executor.as_str()).bind(to_i64(now,"maintenance time")?).fetch_optional(&mut **tx).await.map_err(database_error)?;
    let Some(row) = row else {
        return Ok(None);
    };
    let operation = row
        .try_get::<Json<MaintenanceOperation>, _>("operation_json")
        .map_err(database_error)?
        .0;
    let command = match operation {
        MaintenanceOperation::Status => ClientUpdateCommand::Status,
        MaintenanceOperation::Cancel { id } => ClientUpdateCommand::Cancel { id },
        MaintenanceOperation::Start { id } => {
            let job = crate::client_updates::read_job(tx, tenant, executor)
                .await?
                .filter(|job| {
                    job.progress.id == id
                        && !job.progress.phase.terminal()
                        && job.expires_at_ms > now
                });
            let Some(job) = job else {
                let reply = MaintenanceReply {
                    id: row.try_get("command_id").map_err(database_error)?,
                    info: None,
                    error: Some("update operation is no longer active".into()),
                };
                sqlx::query("UPDATE control_maintenance_commands SET reply_json=$4 WHERE tenant_id=$1 AND executor_id=$2 AND command_id=$3")
                    .bind(tenant.as_str()).bind(executor.as_str()).bind(&reply.id).bind(Json(reply.clone())).execute(&mut **tx).await.map_err(database_error)?;
                return Ok(None);
            };
            // Mint a delivery-scoped grant; the database stores only its hash.
            let token = format!(
                "{:032x}{:032x}",
                rand::random::<u128>(),
                rand::random::<u128>()
            );
            sqlx::query("UPDATE control_client_updates SET download_token_hash=$3,download_expires_at_ms=$4 WHERE tenant_id=$1 AND executor_id=$2")
                .bind(tenant.as_str()).bind(executor.as_str()).bind(crate::client_updates::digest_token(&token)).bind(to_i64(now.saturating_add(30*60*1000),"download expiry")?).execute(&mut **tx).await.map_err(database_error)?;
            ClientUpdateCommand::Start {
                request: ClientUpdateRequest {
                    id,
                    manifest: job.manifest,
                    signature: job.signature,
                    download_token: token,
                    server_version: env!("CARGO_PKG_VERSION").into(),
                    expires_at_ms: job.expires_at_ms,
                },
            }
        }
    };
    Ok(Some(MaintenanceCommand {
        id: row.try_get("command_id").map_err(database_error)?,
        expires_at_ms: from_i64(
            row.try_get("expires_at_ms").map_err(database_error)?,
            "command expiry",
        )?,
        command,
    }))
}
