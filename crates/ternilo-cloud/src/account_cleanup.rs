use std::{collections::BTreeSet, time::Duration};

use sqlx::{Row, any::AnyRow};
use ternilo_control::{AccountRecord, AccountStatusAction, ControlStore, ControlUser};
use ternilo_protocol::{HarnessError, RunId, TenantId, UserId};
use ternilo_storage::{Backend, Transaction, backend, database_error, set_tenant_scope};

use crate::CloudStore;

impl CloudStore {
    /// Close access and cancel accepted work atomically, preserving its resource ownership.
    pub async fn set_account_status(
        &self,
        actor: &ControlUser,
        user_id: &UserId,
        action: AccountStatusAction,
        status_revision: u64,
        now_ms: u64,
    ) -> Result<AccountRecord, HarnessError> {
        for attempt in 0..8 {
            let mut tx = self.database.begin().await?;
            if let Some(account) =
                set_status_and_cancel_in(&mut tx, actor, user_id, action, status_revision, now_ms)
                    .await?
            {
                tx.commit().await.map_err(database_error)?;
                return Ok(account);
            }
            tx.rollback().await.map_err(database_error)?;
            if attempt < 7 {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        }
        Err(HarnessError::conflict(
            "account tasks are changing; retry the account status action",
        ))
    }
}

async fn set_status_and_cancel_in(
    tx: &mut Transaction,
    actor: &ControlUser,
    user_id: &UserId,
    action: AccountStatusAction,
    status_revision: u64,
    now_ms: u64,
) -> Result<Option<AccountRecord>, HarnessError> {
    let account =
        ControlStore::set_account_status_in(tx, actor, user_id, action, status_revision, now_ms)
            .await?;
    if action == AccountStatusAction::Unban {
        return Ok(Some(account));
    }
    let query = match backend(tx) {
        Backend::Postgres => {
            "SELECT tenant_id, session_id, run_id FROM ternilo_cloud_account_run_scopes($1, $2)"
        }
        Backend::Sqlite => {
            "SELECT tenant_id, session_id, run_id FROM cloud_runs
             WHERE $1 IS NOT NULL AND actor_user_id = $2
               AND state IN ('queued', 'leased', 'running', 'cancel_requested')
             ORDER BY tenant_id, session_id, run_id"
        }
    };
    let scopes = sqlx::query(query)
        .bind(actor.user_id.as_str())
        .bind(user_id.as_str())
        .fetch_all(&mut **tx)
        .await
        .map_err(database_error)?;
    if backend(tx) == Backend::Postgres {
        let sessions = scopes
            .iter()
            .map(|scope| {
                Ok((
                    scope
                        .try_get::<String, _>("tenant_id")
                        .map_err(database_error)?,
                    scope
                        .try_get::<String, _>("session_id")
                        .map_err(database_error)?,
                ))
            })
            .collect::<Result<BTreeSet<_>, HarnessError>>()?;
        for (tenant, session) in sessions {
            set_tenant_scope(tx, &TenantId::new(&tenant)).await?;
            // Acquire every session before releasing any reservation. Never wait
            // while holding earlier sessions: parent/child work can lock those
            // sessions in the opposite order.
            let locked: Option<String> = sqlx::query_scalar(
                "SELECT session_id FROM cloud_sessions WHERE tenant_id=$1 AND session_id=$2 FOR UPDATE SKIP LOCKED",
            )
            .bind(&tenant)
            .bind(&session)
            .fetch_optional(&mut **tx)
            .await
            .map_err(database_error)?;
            if locked.is_none() {
                return Ok(None);
            }
        }
        if !try_lock_cancellation_rows_in(tx, &scopes, user_id).await? {
            return Ok(None);
        }
    }
    for scope in scopes {
        let tenant = TenantId::new(
            scope
                .try_get::<String, _>("tenant_id")
                .map_err(database_error)?,
        );
        let run = RunId::new(
            scope
                .try_get::<String, _>("run_id")
                .map_err(database_error)?,
        );
        set_tenant_scope(tx, &tenant).await?;
        // An editor can transfer a queued run's execution authorization between
        // discovery and our session lock. Preserve work now authorized by someone else.
        let accepted_actor: Option<String> = sqlx::query_scalar(
            "SELECT actor_user_id FROM cloud_runs WHERE tenant_id=$1 AND run_id=$2",
        )
        .bind(tenant.as_str())
        .bind(run.as_str())
        .fetch_optional(&mut **tx)
        .await
        .map_err(database_error)?;
        if accepted_actor.as_deref() != Some(user_id.as_str()) {
            continue;
        }
        CloudStore::cancel_run_in(tx, &tenant, &run, Some(&actor.user_id), now_ms).await?;
    }
    Ok(Some(account))
}

/// Never retain sessions while waiting for rows an admission or queue removal already owns.
async fn try_lock_cancellation_rows_in(
    tx: &mut Transaction,
    scopes: &[AnyRow],
    actor_id: &UserId,
) -> Result<bool, HarnessError> {
    let mut reservations = BTreeSet::new();
    for scope in scopes {
        let tenant: String = scope.try_get("tenant_id").map_err(database_error)?;
        let run: String = scope.try_get("run_id").map_err(database_error)?;
        set_tenant_scope(tx, &TenantId::new(&tenant)).await?;
        let locked = sqlx::query(
            "SELECT state, quota_reservation_id FROM cloud_runs
             WHERE tenant_id=$1 AND run_id=$2 AND actor_user_id=$3
               AND state IN ('queued', 'leased', 'running', 'cancel_requested')
             FOR UPDATE SKIP LOCKED",
        )
        .bind(&tenant)
        .bind(&run)
        .bind(actor_id.as_str())
        .fetch_optional(&mut **tx)
        .await
        .map_err(database_error)?;
        let Some(locked) = locked else {
            let still_targeted: i64 = sqlx::query_scalar(
                "SELECT CAST(EXISTS(SELECT 1 FROM cloud_runs
                 WHERE tenant_id=$1 AND run_id=$2 AND actor_user_id=$3
                   AND state IN ('queued', 'leased', 'running', 'cancel_requested')) AS INTEGER)",
            )
            .bind(&tenant)
            .bind(&run)
            .bind(actor_id.as_str())
            .fetch_one(&mut **tx)
            .await
            .map_err(database_error)?;
            if still_targeted != 0 {
                return Ok(false);
            }
            continue;
        };
        if matches!(
            locked
                .try_get::<String, _>("state")
                .map_err(database_error)?
                .as_str(),
            "queued" | "leased"
        ) {
            reservations.insert((
                tenant,
                locked
                    .try_get::<String, _>("quota_reservation_id")
                    .map_err(database_error)?,
            ));
        }
    }
    for (tenant, reservation) in reservations {
        set_tenant_scope(tx, &TenantId::new(&tenant)).await?;
        let locked: Option<String> = sqlx::query_scalar(
            "SELECT reservation_id FROM control_quota_reservations
             WHERE tenant_id=$1 AND reservation_id=$2 AND state='active'
             FOR UPDATE SKIP LOCKED",
        )
        .bind(&tenant)
        .bind(&reservation)
        .fetch_optional(&mut **tx)
        .await
        .map_err(database_error)?;
        if locked.is_none() {
            let still_active: i64 = sqlx::query_scalar(
                "SELECT CAST(EXISTS(SELECT 1 FROM control_quota_reservations
                 WHERE tenant_id=$1 AND reservation_id=$2 AND state='active') AS INTEGER)",
            )
            .bind(&tenant)
            .bind(&reservation)
            .fetch_one(&mut **tx)
            .await
            .map_err(database_error)?;
            if still_active != 0 {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

/// Recheck admission while holding the status mutation's account lock until commit.
pub(crate) async fn require_active_actor_in(
    tx: &mut Transaction,
    actor_id: &UserId,
) -> Result<(), HarnessError> {
    actor_id.validate()?;
    if backend(tx) == Backend::Postgres {
        // Callers can already own session or budget locks. Never wait for an
        // account mutation that may need those locks to cancel accepted work.
        let acquired: i64 = sqlx::query_scalar(
            "SELECT CAST(pg_try_advisory_xact_lock_shared(hashtextextended($1, 0)) AS INTEGER)",
        )
        .bind(format!("ternilo:account-role:{actor_id}"))
        .fetch_one(&mut **tx)
        .await
        .map_err(database_error)?;
        if acquired == 0 {
            return Err(HarnessError::conflict(
                "account access is changing; retry after reauthentication",
            ));
        }
    }
    let active: Option<String> =
        sqlx::query_scalar("SELECT status FROM control_users WHERE user_id = $1")
            .bind(actor_id.as_str())
            .fetch_optional(&mut **tx)
            .await
            .map_err(database_error)?;
    if active.as_deref() != Some("active") {
        return Err(HarnessError::policy("account is not active"));
    }
    Ok(())
}
