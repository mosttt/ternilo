use crate::{
    ControlStore, ControlUser, NodeModelPrincipal, PageQuery,
    crypto::random_identifier,
    store::{from_i64, to_i64},
};
use serde::Serialize;
use sqlx::Row;
use ternilo_protocol::{
    ComputerModelAttempt, HarnessError, RunModelBinding, RunModelSnapshot, TenantId, UserId,
};
use ternilo_storage::{Database, Transaction, database_error, set_tenant_scope};

#[derive(Serialize)]
pub struct ComputerModelAttemptRecord {
    pub attempt: u32,
    pub started_at_ms: u64,
    pub finished_at_ms: Option<u64>,
    pub report: Option<ComputerModelAttempt>,
}

#[derive(Serialize)]
pub struct ComputerModelRequestRecord {
    pub request_id: String,
    pub session_id: String,
    pub run_id: String,
    pub execution_executor_id: String,
    pub source_executor_id: String,
    pub execution_computer_name: String,
    pub source_computer_name: String,
    pub actor_user_id: UserId,
    pub model_owner_user_id: UserId,
    pub resource_owner_user_id: UserId,
    pub snapshot: RunModelSnapshot,
    pub state: String,
    pub error_code: Option<String>,
    pub created_at_ms: u64,
    pub attempts: Vec<ComputerModelAttemptRecord>,
}

#[derive(Serialize)]
pub struct ComputerModelRequestPage {
    pub requests: Vec<ComputerModelRequestRecord>,
    pub next_cursor: Option<String>,
}

pub(crate) async fn initialize(database: &Database) -> Result<(), HarnessError> {
    database
        .initialize(
            "computer_models",
            1,
            include_str!("schema.sql"),
            include_str!("postgres.sql"),
        )
        .await
}

impl ControlStore {
    pub async fn computer_model_source_name(
        &self,
        binding: &RunModelBinding,
    ) -> Result<String, HarnessError> {
        let RunModelBinding::ComputerProvider {
            tenant_id,
            executor_id,
            ..
        } = binding
        else {
            return Err(HarnessError::invalid("expected source computer"));
        };
        let mut tx = self.database.tenant_transaction(tenant_id).await?;
        self.require_computer_model_source_in(&mut tx, binding)
            .await?;
        let name: String = sqlx::query_scalar("SELECT COALESCE(m.display_name,e.executor_id) FROM control_executors e LEFT JOIN control_computer_management m ON m.tenant_id=e.tenant_id AND m.executor_id=e.executor_id WHERE e.tenant_id=$1 AND e.executor_id=$2")
            .bind(tenant_id.as_str()).bind(executor_id).fetch_one(&mut *tx).await.map_err(database_error)?;
        tx.commit().await.map_err(database_error)?;
        Ok(name)
    }

    pub async fn renew_computer_model_request_in(
        &self,
        tx: &mut Transaction,
        tenant: &TenantId,
        id: &str,
        now: u64,
    ) -> Result<(), HarnessError> {
        set_tenant_scope(tx, tenant).await?;
        let changed = sqlx::query("UPDATE control_computer_model_requests SET updated_at_ms=$3 WHERE tenant_id=$1 AND request_id=$2 AND state='pending' AND updated_at_ms >= $4")
            .bind(tenant.as_str()).bind(id).bind(to_i64(now, "model request heartbeat")?).bind(to_i64(now.saturating_sub(60_000), "model request expiry")?).execute(&mut **tx).await.map_err(database_error)?.rows_affected();
        if changed != 1 {
            return Err(HarnessError::policy(
                "computer model request is no longer active",
            ));
        }
        Ok(())
    }

    pub async fn list_computer_model_requests(
        &self,
        actor: &ControlUser,
        tenant: &TenantId,
        query: &PageQuery,
        now: u64,
    ) -> Result<ComputerModelRequestPage, HarnessError> {
        let (pattern, cursor) = query.parameters()?;
        let cursor = cursor
            .as_deref()
            .map(|value| {
                let (time, id) = value
                    .split_once(':')
                    .ok_or_else(|| HarnessError::invalid("invalid computer model cursor"))?;
                let time: i64 = time
                    .parse()
                    .map_err(|_| HarnessError::invalid("invalid computer model cursor"))?;
                Ok::<_, HarnessError>((time, id))
            })
            .transpose()?;
        let mut tx = self.database.tenant_transaction(tenant).await?;
        crate::store::require_action(
            &mut tx,
            tenant,
            &actor.user_id,
            crate::ControlAction::TenantRead,
        )
        .await?;
        // A lost Server/Node connection is never replayed; retain unknown usage.
        sqlx::query("UPDATE control_computer_model_requests SET state='failed',error_code='connection_lost' WHERE tenant_id=$1 AND state='pending' AND updated_at_ms<$2")
            .bind(tenant.as_str()).bind(to_i64(now.saturating_sub(60_000), "model request expiry")?).execute(&mut *tx).await.map_err(database_error)?;
        let rows = sqlx::query("SELECT r.*,COALESCE(e.display_name,r.execution_executor_id) AS execution_computer_name,COALESCE(s.display_name,r.source_executor_id) AS source_computer_name FROM control_computer_model_requests r LEFT JOIN control_computer_management e ON e.tenant_id=r.tenant_id AND e.executor_id=r.execution_executor_id LEFT JOIN control_computer_management s ON s.tenant_id=r.tenant_id AND s.executor_id=r.source_executor_id WHERE r.tenant_id=$1 AND (r.actor_user_id=$2 OR r.model_owner_user_id=$2) AND (CAST($3 AS TEXT) IS NULL OR LOWER(r.request_id) LIKE $3 ESCAPE '!' OR LOWER(r.snapshot_json) LIKE $3 ESCAPE '!') AND (CAST($4 AS BIGINT) IS NULL OR r.created_at_ms<$4 OR (r.created_at_ms=$4 AND r.request_id<$5)) ORDER BY r.created_at_ms DESC,r.request_id DESC LIMIT $6")
            .bind(tenant.as_str()).bind(actor.user_id.as_str()).bind(pattern).bind(cursor.map(|(time, _)| time)).bind(cursor.map(|(_, id)| id)).bind(i64::from(query.limit) + 1)
            .fetch_all(&mut *tx).await.map_err(database_error)?;
        let mut requests = Vec::new();
        for row in rows {
            let id: String = row.try_get("request_id").map_err(database_error)?;
            let attempts = sqlx::query("SELECT * FROM control_computer_model_attempts WHERE tenant_id=$1 AND request_id=$2 ORDER BY attempt")
                .bind(tenant.as_str()).bind(&id).fetch_all(&mut *tx).await.map_err(database_error)?
                .into_iter().map(|row| {
                    Ok(ComputerModelAttemptRecord {
                        attempt: u32::try_from(row.try_get::<i64, _>("attempt").map_err(database_error)?).map_err(|_| HarnessError::execution("invalid stored attempt"))?,
                        started_at_ms: from_i64(row.try_get("started_at_ms").map_err(database_error)?, "attempt timestamp")?,
                        finished_at_ms: row.try_get::<Option<i64>, _>("finished_at_ms").map_err(database_error)?.map(|value| from_i64(value, "attempt timestamp")).transpose()?,
                        report: row.try_get::<Option<String>, _>("report_json").map_err(database_error)?.map(|value| serde_json::from_str(&value).map_err(|_| HarnessError::execution("invalid stored model report"))).transpose()?,
                    })
                }).collect::<Result<Vec<_>, HarnessError>>()?;
            requests.push(ComputerModelRequestRecord {
                request_id: id,
                session_id: row.try_get("session_id").map_err(database_error)?,
                run_id: row.try_get("run_id").map_err(database_error)?,
                execution_executor_id: row
                    .try_get("execution_executor_id")
                    .map_err(database_error)?,
                source_executor_id: row.try_get("source_executor_id").map_err(database_error)?,
                execution_computer_name: row
                    .try_get("execution_computer_name")
                    .map_err(database_error)?,
                source_computer_name: row
                    .try_get("source_computer_name")
                    .map_err(database_error)?,
                actor_user_id: UserId::new(
                    row.try_get::<String, _>("actor_user_id")
                        .map_err(database_error)?,
                ),
                model_owner_user_id: UserId::new(
                    row.try_get::<String, _>("model_owner_user_id")
                        .map_err(database_error)?,
                ),
                resource_owner_user_id: UserId::new(
                    row.try_get::<String, _>("resource_owner_user_id")
                        .map_err(database_error)?,
                ),
                snapshot: serde_json::from_str(
                    &row.try_get::<String, _>("snapshot_json")
                        .map_err(database_error)?,
                )
                .map_err(|_| HarnessError::execution("invalid stored model snapshot"))?,
                state: row.try_get("state").map_err(database_error)?,
                error_code: row.try_get("error_code").map_err(database_error)?,
                created_at_ms: from_i64(
                    row.try_get("created_at_ms").map_err(database_error)?,
                    "request timestamp",
                )?,
                attempts,
            });
        }
        let next_cursor = query.finish(&mut requests, |value| {
            format!("{}:{}", value.created_at_ms, value.request_id)
        });
        tx.commit().await.map_err(database_error)?;
        Ok(ComputerModelRequestPage {
            requests,
            next_cursor,
        })
    }

    pub async fn require_computer_model_source_in(
        &self,
        tx: &mut Transaction,
        binding: &RunModelBinding,
    ) -> Result<(), HarnessError> {
        let RunModelBinding::ComputerProvider {
            tenant_id,
            owner_user_id,
            executor_id,
            ..
        } = binding
        else {
            return Err(HarnessError::policy("expected a computer model binding"));
        };
        binding.validate()?;
        crate::account_store::require_active_account_in(tx, owner_user_id).await?;
        set_tenant_scope(tx, tenant_id).await?;
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM control_executors e JOIN control_memberships u ON u.tenant_id=e.tenant_id AND u.user_id=e.owner_user_id LEFT JOIN control_computer_management m ON m.tenant_id=e.tenant_id AND m.executor_id=e.executor_id WHERE e.tenant_id=$1 AND e.executor_id=$2 AND e.owner_user_id=$3 AND e.state<>'revoked' AND m.suspended_at_ms IS NULL AND m.removed_at_ms IS NULL AND EXISTS (SELECT 1 FROM control_node_credentials c WHERE c.tenant_id=e.tenant_id AND c.executor_id=e.executor_id AND c.revoked_at_ms IS NULL)")
            .bind(tenant_id.as_str()).bind(executor_id).bind(owner_user_id.as_str()).fetch_one(&mut **tx).await.map_err(database_error)?;
        if count != 1 {
            return Err(HarnessError::policy(
                "the source computer authorization is unavailable",
            ));
        }
        Ok(())
    }

    pub async fn accept_computer_model_request_in(
        &self,
        tx: &mut Transaction,
        principal: &NodeModelPrincipal,
        request_key: &str,
        payload_hash: &str,
        max_attempts: u32,
        now: u64,
    ) -> Result<String, crate::ModelAccessError> {
        self.require_computer_model_source_in(tx, &principal.snapshot.binding)
            .await?;
        let RunModelBinding::ComputerProvider {
            tenant_id,
            owner_user_id,
            executor_id,
            ..
        } = &principal.snapshot.binding
        else {
            unreachable!()
        };
        if tenant_id != &principal.tenant_id
            || !(1..=8).contains(&max_attempts)
            || request_key.is_empty()
            || request_key.len() > 128
            || payload_hash.len() != 64
        {
            return Err(HarnessError::invalid("invalid computer model request authority").into());
        }
        let existing: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM control_computer_model_requests WHERE tenant_id=$1 AND credential_id=$2 AND session_id=$3 AND run_id=$4 AND request_key=$5")
            .bind(tenant_id.as_str()).bind(&principal.credential_id).bind(principal.session_id.as_str()).bind(principal.run_id.as_str()).bind(request_key)
            .fetch_one(&mut **tx).await.map_err(database_error)?;
        if existing != 0 {
            return Err(HarnessError::conflict(
                "computer model request was already accepted; it cannot be replayed",
            )
            .into());
        }
        let execution: String = sqlx::query_scalar(
            "SELECT executor_id FROM control_edge_sessions WHERE tenant_id=$1 AND session_id=$2",
        )
        .bind(tenant_id.as_str())
        .bind(principal.session_id.as_str())
        .fetch_one(&mut **tx)
        .await
        .map_err(database_error)?;
        crate::model_traffic::check_admission(tx, &principal.actor_user_id, now).await?;
        let id = random_identifier("cmr");
        sqlx::query("INSERT INTO control_computer_model_requests(tenant_id,request_id,credential_id,session_id,run_id,request_key,payload_hash,execution_executor_id,source_executor_id,actor_user_id,model_owner_user_id,resource_owner_user_id,snapshot_json,max_attempts,state,created_at_ms,updated_at_ms) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,'pending',$15,$15)")
            .bind(tenant_id.as_str()).bind(&id).bind(&principal.credential_id).bind(principal.session_id.as_str()).bind(principal.run_id.as_str()).bind(request_key).bind(payload_hash).bind(execution).bind(executor_id)
            .bind(principal.actor_user_id.as_str()).bind(owner_user_id.as_str()).bind(principal.resource_owner_user_id.as_str())
            .bind(serde_json::to_string(&principal.snapshot).map_err(|_| HarnessError::invalid("invalid model snapshot"))?).bind(i64::from(max_attempts)).bind(to_i64(now, "computer model timestamp")?)
            .execute(&mut **tx).await.map_err(database_error)?;
        Ok(id)
    }

    pub async fn begin_computer_model_attempt_in(
        &self,
        tx: &mut Transaction,
        principal: &NodeModelPrincipal,
        id: &str,
        attempt: u32,
        now: u64,
    ) -> Result<(), HarnessError> {
        self.require_computer_model_source_in(tx, &principal.snapshot.binding)
            .await?;
        set_tenant_scope(tx, &principal.tenant_id).await?;
        let allowed: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM control_computer_model_requests WHERE tenant_id=$1 AND request_id=$2 AND credential_id=$3 AND session_id=$4 AND run_id=$5 AND actor_user_id=$6 AND state='pending' AND max_attempts >= $7")
            .bind(principal.tenant_id.as_str()).bind(id).bind(&principal.credential_id).bind(principal.session_id.as_str()).bind(principal.run_id.as_str()).bind(principal.actor_user_id.as_str()).bind(i64::from(attempt))
            .fetch_one(&mut **tx).await.map_err(database_error)?;
        if allowed != 1 || attempt == 0 {
            return Err(HarnessError::policy(
                "computer model attempt is not authorized",
            ));
        }
        if attempt > 1 {
            let previous: Option<String> = sqlx::query_scalar("SELECT report_json FROM control_computer_model_attempts WHERE tenant_id=$1 AND request_id=$2 AND attempt=$3")
                .bind(principal.tenant_id.as_str()).bind(id).bind(i64::from(attempt - 1)).fetch_optional(&mut **tx).await.map_err(database_error)?.flatten();
            if !previous.as_deref().and_then(|value| serde_json::from_str::<ComputerModelAttempt>(value).ok()).is_some_and(|value| matches!(value, ComputerModelAttempt::Finished { error_code: Some(code), .. } if code != ternilo_protocol::ErrorCode::Cancelled)) {
                return Err(HarnessError::policy("computer model retry requires a failed prior attempt"));
            }
        }
        sqlx::query("INSERT INTO control_computer_model_attempts(tenant_id,request_id,attempt,started_at_ms) VALUES($1,$2,$3,$4)")
            .bind(principal.tenant_id.as_str()).bind(id).bind(i64::from(attempt)).bind(to_i64(now, "computer model timestamp")?).execute(&mut **tx).await.map_err(database_error)?;
        Ok(())
    }

    /// Retain reports for accepted attempts even when access is subsequently revoked.
    pub async fn finish_computer_model_attempt(
        &self,
        tenant: &TenantId,
        id: &str,
        report: &ComputerModelAttempt,
        now: u64,
    ) -> Result<(), HarnessError> {
        let ComputerModelAttempt::Finished {
            attempt,
            upstream_request_id,
            ..
        } = report
        else {
            return Err(HarnessError::invalid("expected finished model attempt"));
        };
        if upstream_request_id
            .as_ref()
            .is_some_and(|id| id.len() > 512)
        {
            return Err(HarnessError::invalid(
                "upstream request identifier is too long",
            ));
        }
        let mut tx = self.database.tenant_transaction(tenant).await?;
        let changed = sqlx::query("UPDATE control_computer_model_attempts SET report_json=$4,finished_at_ms=$5 WHERE tenant_id=$1 AND request_id=$2 AND attempt=$3 AND report_json IS NULL")
            .bind(tenant.as_str()).bind(id).bind(i64::from(*attempt)).bind(serde_json::to_string(report).map_err(|_| HarnessError::invalid("invalid model attempt report"))?).bind(to_i64(now, "computer model timestamp")?)
            .execute(&mut *tx).await.map_err(database_error)?.rows_affected();
        if changed != 1 {
            return Err(HarnessError::conflict(
                "model attempt was not started or was already reported",
            ));
        }
        tx.commit().await.map_err(database_error)
    }

    pub async fn finish_computer_model_request(
        &self,
        tenant: &TenantId,
        id: &str,
        state: &str,
        error: Option<&str>,
        now: u64,
    ) -> Result<(), HarnessError> {
        if !matches!(state, "completed" | "failed" | "cancelled") {
            return Err(HarnessError::invalid(
                "invalid computer model request outcome",
            ));
        }
        let mut tx = self.database.tenant_transaction(tenant).await?;
        sqlx::query("UPDATE control_computer_model_requests SET state=$3,error_code=$4,updated_at_ms=$5 WHERE tenant_id=$1 AND request_id=$2 AND state='pending'")
            .bind(tenant.as_str()).bind(id).bind(state).bind(error).bind(to_i64(now, "computer model timestamp")?).execute(&mut *tx).await.map_err(database_error)?;
        tx.commit().await.map_err(database_error)
    }
}
