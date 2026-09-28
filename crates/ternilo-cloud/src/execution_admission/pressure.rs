use super::{
    Backend, CloudStore, HarnessError, Row, RunId, SessionId, TenantId, Transaction, UserId,
    database_error, owner_scope, pool_gate, reconcile_pool_in, to_i64, worker_pool, worker_slot,
};

impl CloudStore {
    /// Resolve stalls on the Worker holding the execution family. Other Workers
    /// cannot use spare capacity for a family pinned by physical workspace ownership.
    pub async fn resolve_execution_pressure(
        &self,
        worker: &str,
        now: u64,
    ) -> Result<u32, HarnessError> {
        let mut tx = self.admission_transaction().await?;
        let storage = worker_pool(&mut tx, worker).await?;
        pool_gate(&mut tx, &storage).await?;
        reconcile_pool_in(&mut tx, &storage, now).await?;
        crate::workspace_waiting::refresh_in(&mut tx, &storage, now).await?;
        let gate = match ternilo_storage::backend(&tx) {
            Backend::Postgres => "SELECT ternilo_cloud_claim_gate()",
            Backend::Sqlite => {
                "SELECT 1-claims_paused FROM cloud_runtime_control WHERE singleton=1"
            }
        };
        let enabled: i64 = sqlx::query_scalar(gate)
            .fetch_one(&mut *tx)
            .await
            .map_err(database_error)?;
        if enabled == 0 || !worker_is_stalled_in(&mut tx, worker, now).await? {
            tx.commit().await.map_err(database_error)?;
            return Ok(0);
        }
        let sql = match ternilo_storage::backend(&tx) {
            Backend::Postgres => "SELECT blocked FROM ternilo_cloud_execution_pressure_barrier($1)",
            Backend::Sqlite => include_str!("../queries/execution_pressure_barrier.sql"),
        };
        let blocked: i64 = sqlx::query_scalar(sql)
            .bind(worker)
            .fetch_one(&mut *tx)
            .await
            .map_err(database_error)?;
        if blocked != 0 {
            tx.commit().await.map_err(database_error)?;
            return Ok(0);
        }
        let sql = match ternilo_storage::backend(&tx) {
            Backend::Postgres => "SELECT * FROM ternilo_cloud_execution_pressure_candidate($1,$2)",
            Backend::Sqlite => include_str!("../queries/execution_pressure_candidate.sql"),
        };
        let candidate = sqlx::query(sql)
            .bind(worker)
            .bind(to_i64(now, "dependency readiness time")?)
            .fetch_optional(&mut *tx)
            .await
            .map_err(database_error)?;
        let Some(candidate) = candidate else {
            tx.commit().await.map_err(database_error)?;
            return Ok(0);
        };
        let tenant = TenantId::new(
            candidate
                .try_get::<String, _>("tenant_id")
                .map_err(database_error)?,
        );
        let owner = UserId::new(
            candidate
                .try_get::<String, _>("owner_user_id")
                .map_err(database_error)?,
        );
        let child = RunId::new(
            candidate
                .try_get::<String, _>("child_run_id")
                .map_err(database_error)?,
        );
        let session = SessionId::new(
            candidate
                .try_get::<String, _>("child_session_id")
                .map_err(database_error)?,
        );
        owner_scope(&mut tx, &tenant, &owner).await?;
        let failed =
            crate::store::fail_queued_capacity_in(&mut tx, &tenant, &session, &child, now).await?;
        tx.commit().await.map_err(database_error)?;
        Ok(u32::from(failed))
    }
}

async fn worker_is_stalled_in(
    tx: &mut Transaction,
    worker: &str,
    now: u64,
) -> Result<bool, HarnessError> {
    let sql = match ternilo_storage::backend(tx) {
        Backend::Postgres => "SELECT * FROM ternilo_cloud_execution_worker_summary($1)",
        Backend::Sqlite => include_str!("../queries/execution_worker_summary.sql"),
    };
    let summary = sqlx::query(sql)
        .bind(worker)
        .fetch_one(&mut **tx)
        .await
        .map_err(database_error)?;
    let residents: i64 = summary.try_get("residents").map_err(database_error)?;
    let nonparked: i64 = summary.try_get("nonparked").map_err(database_error)?;
    if residents == 0 || nonparked != 0 {
        return Ok(false);
    }
    let slot = worker_slot(tx, worker, now).await?;
    Ok(residents >= i64::from(slot.capacity.max_resident_runs))
}
