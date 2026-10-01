mod events;
mod provenance;
mod session_deletion;
mod uploads;

pub(crate) use session_deletion::purge_deleted_session_mappings;
pub(crate) use uploads::session_deleted;

use crate::EdgeSessionMetadata;
use serde_json::Value;
use sqlx::{AnyPool, Row};
use ternilo_protocol::{HarnessError, SessionEvent, SessionEventKind, SessionId, TenantId};
use ternilo_storage::{Database, Json};
use ternilo_transport::{ExecutorHello, ExecutorId, SessionCursor};

#[derive(Clone)]
pub struct EdgeStore {
    database: Database,
    pool: AnyPool,
}

impl EdgeStore {
    pub(crate) fn from_database(database: Database) -> Self {
        Self {
            pool: database.pool().clone(),
            database,
        }
    }

    #[must_use]
    pub fn database(&self) -> &Database {
        &self.database
    }

    pub async fn health(&self) -> Result<(), HarnessError> {
        sqlx::query("SELECT 1")
            .execute(&self.pool)
            .await
            .map_err(database_error)?;
        Ok(())
    }

    pub async fn register_executor(
        &self,
        tenant_id: &TenantId,
        hello: &ExecutorHello,
        last_seen_at_ms: u64,
    ) -> Result<(), HarnessError> {
        tenant_id.validate()?;
        hello.validate()?;
        let mut transaction = self.transaction(tenant_id).await?;
        sqlx::query(
            "INSERT INTO control_edge_executors
                (tenant_id, executor_id, hello_json, last_seen_at_ms)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (tenant_id, executor_id) DO UPDATE SET
                hello_json = EXCLUDED.hello_json,
                last_seen_at_ms = EXCLUDED.last_seen_at_ms",
        )
        .bind(tenant_id.as_str())
        .bind(hello.executor_id.as_str())
        .bind(to_json(hello, "executor hello")?)
        .bind(to_i64(last_seen_at_ms, "executor last-seen timestamp")?)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        transaction.commit().await.map_err(database_error)
    }

    /// Recheck the original credential; a re-enrollment cannot revive an older connection.
    pub async fn require_node_credential(
        &self,
        principal: &crate::NodePrincipal,
    ) -> Result<(), HarnessError> {
        let mut tx = self.transaction(&principal.scope.tenant_id).await?;
        let active: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM control_node_credentials credential JOIN control_executors executor ON executor.tenant_id = credential.tenant_id AND executor.executor_id = credential.executor_id JOIN control_users account ON account.user_id = executor.owner_user_id LEFT JOIN control_computer_management m ON m.tenant_id=executor.tenant_id AND m.executor_id=executor.executor_id WHERE credential.tenant_id = $1 AND credential.executor_id = $2 AND credential.credential_id = $3 AND executor.owner_user_id = $4 AND credential.revoked_at_ms IS NULL AND executor.state <> 'revoked' AND account.status = 'active' AND m.suspended_at_ms IS NULL AND m.removed_at_ms IS NULL")
            .bind(principal.scope.tenant_id.as_str()).bind(principal.executor_id.as_str())
            .bind(&principal.credential_id).bind(principal.scope.user_id.as_str())
            .fetch_one(&mut *tx).await.map_err(database_error)?;
        if active != 1 {
            return Err(HarnessError::policy(
                "node credential is invalid or revoked",
            ));
        }
        tx.commit().await.map_err(database_error)
    }

    pub async fn mark_executor_seen(
        &self,
        tenant_id: &TenantId,
        executor_id: &ExecutorId,
        last_seen_at_ms: u64,
    ) -> Result<(), HarnessError> {
        validate_route(tenant_id, executor_id)?;
        let mut transaction = self.transaction(tenant_id).await?;
        let changed = sqlx::query(
            "UPDATE control_edge_executors SET last_seen_at_ms = $3
             WHERE tenant_id = $1 AND executor_id = $2",
        )
        .bind(tenant_id.as_str())
        .bind(executor_id.as_str())
        .bind(to_i64(last_seen_at_ms, "executor last-seen timestamp")?)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?
        .rows_affected();
        if changed != 1 {
            return Err(HarnessError::policy("edge executor is not registered"));
        }
        transaction.commit().await.map_err(database_error)
    }

    pub async fn event_cursors(
        &self,
        tenant_id: &TenantId,
        executor_id: &ExecutorId,
    ) -> Result<Vec<SessionCursor>, HarnessError> {
        validate_route(tenant_id, executor_id)?;
        let mut transaction = self.transaction(tenant_id).await?;
        let rows = sqlx::query(
            "SELECT session_id, MAX(seq) AS last_seq FROM control_edge_events
             WHERE tenant_id = $1 AND executor_id = $2
             GROUP BY session_id ORDER BY session_id",
        )
        .bind(tenant_id.as_str())
        .bind(executor_id.as_str())
        .fetch_all(&mut *transaction)
        .await
        .map_err(database_error)?;
        transaction.commit().await.map_err(database_error)?;
        rows.into_iter()
            .map(|row| {
                Ok(SessionCursor {
                    session_id: SessionId::new(
                        row.try_get::<String, _>("session_id")
                            .map_err(database_error)?,
                    ),
                    last_seq: Some(from_i64(
                        row.try_get("last_seq").map_err(database_error)?,
                        "session event sequence",
                    )?),
                })
            })
            .collect()
    }

    pub async fn delete_events(
        &self,
        tenant_id: &TenantId,
        executor_id: &ExecutorId,
        session_id: &SessionId,
    ) -> Result<(), HarnessError> {
        validate_route(tenant_id, executor_id)?;
        session_id.validate()?;
        let mut transaction = self.transaction(tenant_id).await?;
        sqlx::query(
            "DELETE FROM control_edge_events
             WHERE tenant_id = $1 AND executor_id = $2 AND session_id = $3",
        )
        .bind(tenant_id.as_str())
        .bind(executor_id.as_str())
        .bind(session_id.as_str())
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        transaction.commit().await.map_err(database_error)
    }

    async fn transaction(
        &self,
        tenant_id: &TenantId,
    ) -> Result<ternilo_storage::Transaction, HarnessError> {
        self.database.tenant_transaction(tenant_id).await
    }
}

fn validate_route(tenant_id: &TenantId, executor_id: &ExecutorId) -> Result<(), HarnessError> {
    tenant_id.validate()?;
    executor_id.validate()
}

fn to_i64(value: u64, label: &str) -> Result<i64, HarnessError> {
    i64::try_from(value)
        .map_err(|_| HarnessError::invalid(format!("{label} exceeds PostgreSQL BIGINT")))
}

fn from_i64(value: i64, label: &str) -> Result<u64, HarnessError> {
    u64::try_from(value)
        .map_err(|_| HarnessError::execution(format!("database contains invalid {label}")))
}

fn to_json<T: serde::Serialize>(value: &T, label: &str) -> Result<Json<Value>, HarnessError> {
    serde_json::to_value(value)
        .map(Json)
        .map_err(|error| HarnessError::execution(format!("encode {label}: {error}")))
}

fn from_json<T: serde::de::DeserializeOwned>(
    value: Json<Value>,
    label: &str,
) -> Result<T, HarnessError> {
    serde_json::from_value(value.0)
        .map_err(|error| HarnessError::execution(format!("decode {label}: {error}")))
}

fn database_error(error: impl std::fmt::Display) -> HarnessError {
    HarnessError::execution(format!("edge event store failed: {error}"))
}
