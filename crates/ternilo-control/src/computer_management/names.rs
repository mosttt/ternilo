use sqlx::Row;
use ternilo_protocol::{HarnessError, TenantId};
use ternilo_storage::{Database, Transaction, database_error};
use ternilo_transport::ExecutorId;

use crate::store::to_i64;

pub(crate) async fn initialize(database: &Database) -> Result<(), HarnessError> {
    database
        .initialize(
            "computer_names",
            1,
            include_str!("names.sql"),
            include_str!("names-postgres.sql"),
        )
        .await
}

pub(crate) fn validate(name: &str) -> Result<&str, HarnessError> {
    let name = name.trim();
    if name.is_empty()
        || name.len() > 512
        || name.chars().count() > 128
        || name.chars().any(char::is_control)
    {
        return Err(HarnessError::invalid(
            "computer name must contain 1 to 128 characters without control characters",
        ));
    }
    Ok(name)
}

pub(crate) async fn reserve_in(
    tx: &mut Transaction,
    tenant: &TenantId,
    executor: &ExecutorId,
    owner: &str,
    name: &str,
    expiry: Option<u64>,
    now_ms: u64,
) -> Result<(), HarnessError> {
    let name = validate(name)?;
    ternilo_storage::lock(tx, &format!("computer-name:{tenant}:{owner}:{name}")).await?;
    let now = to_i64(now_ms, "computer name timestamp")?;
    sqlx::query("DELETE FROM control_computer_names WHERE tenant_id=$1 AND reserved_until_ms<=$2 AND NOT EXISTS (SELECT 1 FROM control_executors e WHERE e.tenant_id=control_computer_names.tenant_id AND e.executor_id=control_computer_names.executor_id)")
        .bind(tenant.as_str()).bind(now).execute(&mut **tx).await.map_err(database_error)?;
    sqlx::query("UPDATE control_computer_names SET name_key=NULL,reserved_until_ms=NULL WHERE tenant_id=$1 AND reserved_until_ms<=$2 AND EXISTS (SELECT 1 FROM control_computer_management m WHERE m.tenant_id=control_computer_names.tenant_id AND m.executor_id=control_computer_names.executor_id AND m.removed_at_ms IS NOT NULL)")
        .bind(tenant.as_str()).bind(now).execute(&mut **tx).await.map_err(database_error)?;
    let conflict: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM control_computer_names WHERE tenant_id=$1 AND owner_user_id=$2 AND name_key=$3 AND executor_id<>$4")
        .bind(tenant.as_str()).bind(owner).bind(name).bind(executor.as_str()).fetch_one(&mut **tx).await.map_err(database_error)?;
    let existing: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM control_executors e LEFT JOIN control_computer_management m ON m.tenant_id=e.tenant_id AND m.executor_id=e.executor_id WHERE e.tenant_id=$1 AND e.owner_user_id=$2 AND e.executor_id<>$4 AND m.removed_at_ms IS NULL AND COALESCE(m.display_name,e.executor_id)=$3 AND NOT EXISTS (SELECT 1 FROM control_computer_names n WHERE n.tenant_id=e.tenant_id AND n.executor_id=e.executor_id)")
        .bind(tenant.as_str()).bind(owner).bind(name).bind(executor.as_str()).fetch_one(&mut **tx).await.map_err(database_error)?;
    if conflict != 0 || existing != 0 {
        return Err(HarnessError::conflict(
            "computer name is already registered by this account in this space",
        ));
    }
    let written = sqlx::query("INSERT INTO control_computer_names(tenant_id,executor_id,owner_user_id,name,name_key,reserved_until_ms) VALUES($1,$2,$3,$4,$4,$5) ON CONFLICT(tenant_id,executor_id) DO UPDATE SET name=EXCLUDED.name,name_key=EXCLUDED.name_key,reserved_until_ms=EXCLUDED.reserved_until_ms WHERE control_computer_names.owner_user_id=EXCLUDED.owner_user_id")
        .bind(tenant.as_str()).bind(executor.as_str()).bind(owner).bind(name)
        .bind(expiry.map(|value| to_i64(value,"computer name expiry")).transpose()?)
        .execute(&mut **tx).await.map_err(database_error)?;
    if written.rows_affected() != 1 {
        return Err(HarnessError::policy(
            "computer identity belongs to another account",
        ));
    }
    Ok(())
}

pub(crate) async fn activate_in(
    tx: &mut Transaction,
    tenant: &TenantId,
    executor: &ExecutorId,
) -> Result<String, HarnessError> {
    let row = sqlx::query("UPDATE control_computer_names SET reserved_until_ms=NULL WHERE tenant_id=$1 AND executor_id=$2 RETURNING name")
        .bind(tenant.as_str()).bind(executor.as_str()).fetch_one(&mut **tx).await.map_err(database_error)?;
    row.try_get("name").map_err(database_error)
}
