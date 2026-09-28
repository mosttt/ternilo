use ternilo_protocol::{HarnessError, TenantId};
use ternilo_storage::{Database, Transaction};
use ternilo_transport::ExecutorId;

use crate::{CloudStore, store};

pub(crate) async fn initialize_database(database: &Database) -> Result<(), HarnessError> {
    database
        .initialize(
            "cloud_workspace_storage",
            1,
            include_str!("schema/workspace_storage.sql"),
            include_str!("schema/workspace_storage_postgres.sql"),
        )
        .await
}

impl CloudStore {
    /// Resolve storage from the authenticated credential, never from a claim payload.
    pub(crate) async fn worker_storage_in(
        transaction: &mut Transaction,
        worker_id: &ExecutorId,
    ) -> Result<String, HarnessError> {
        worker_id.validate()?;
        sqlx::query_scalar::<_, String>(
            "SELECT storage_id FROM cloud_worker_credentials
             WHERE worker_id=$1 AND revoked_at_ms IS NULL",
        )
        .bind(worker_id.as_str())
        .fetch_optional(&mut **transaction)
        .await
        .map_err(store::database_error)?
        .ok_or_else(|| HarnessError::policy("Worker credential is unavailable or revoked"))
    }

    /// Bind all tenant workspaces to one persistent filesystem on their first claim.
    /// A missing or offline worker never releases this assignment.
    pub(crate) async fn ensure_tenant_storage_in(
        transaction: &mut Transaction,
        tenant_id: &TenantId,
        storage_id: &str,
        now_ms: u64,
    ) -> Result<bool, HarnessError> {
        tenant_id.validate()?;
        ExecutorId::new(storage_id).validate()?;
        sqlx::query(
            "INSERT INTO cloud_tenant_storage(tenant_id,storage_id,assigned_at_ms)
             VALUES($1,$2,$3) ON CONFLICT(tenant_id) DO NOTHING",
        )
        .bind(tenant_id.as_str())
        .bind(storage_id)
        .bind(store::to_i64(now_ms, "storage assignment time")?)
        .execute(&mut **transaction)
        .await
        .map_err(store::database_error)?;
        let assigned = sqlx::query_scalar::<_, String>(
            "SELECT storage_id FROM cloud_tenant_storage WHERE tenant_id=$1",
        )
        .bind(tenant_id.as_str())
        .fetch_one(&mut **transaction)
        .await
        .map_err(store::database_error)?;
        Ok(assigned == storage_id)
    }
}
