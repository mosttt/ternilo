use serde::{Deserialize, Serialize};
use serde_json::json;
use ternilo_protocol::{HarnessError, TenantId, UserId};
use ternilo_storage::{Backend, Transaction, database_error, lock, set_tenant_scope};

use crate::{
    AccountRecord, AccountStatus, ControlStore, ControlUser, PlatformAction, PlatformRole,
    account_store::{account_in, append_platform_audit, authorize_platform_in},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountStatusAction {
    Ban,
    Unban,
    Remove,
}

impl ControlStore {
    /// Close access without deleting historical resource, usage or audit ownership.
    pub async fn set_account_status(
        &self,
        actor: &ControlUser,
        user_id: &UserId,
        action: AccountStatusAction,
        status_revision: u64,
        now_ms: u64,
    ) -> Result<AccountRecord, HarnessError> {
        let mut tx = self.database.begin().await?;
        let updated =
            Self::set_account_status_in(&mut tx, actor, user_id, action, status_revision, now_ms)
                .await?;
        tx.commit().await.map_err(database_error)?;
        Ok(updated)
    }

    /// Coordinate access revocation with execution cleanup in the caller's transaction.
    pub async fn set_account_status_in(
        tx: &mut Transaction,
        actor: &ControlUser,
        user_id: &UserId,
        action: AccountStatusAction,
        status_revision: u64,
        now_ms: u64,
    ) -> Result<AccountRecord, HarnessError> {
        user_id.validate()?;
        lock(tx, "ternilo:instance").await?;
        authorize_platform_in(tx, &actor.user_id, PlatformAction::AccountsManage).await?;
        lock(tx, &format!("ternilo:account-role:{user_id}")).await?;
        let account = account_in(tx, user_id).await?;
        if account.platform_role == PlatformRole::Owner || actor.user_id == *user_id {
            return Err(HarnessError::policy(
                "the instance owner and your own account cannot be banned or removed",
            ));
        }
        if account.status_revision != status_revision {
            return Err(HarnessError::conflict(
                "account status changed; reload before saving",
            ));
        }
        let status = match (account.status, action) {
            (AccountStatus::Active, AccountStatusAction::Ban) => AccountStatus::Banned,
            (AccountStatus::Banned, AccountStatusAction::Unban) => AccountStatus::Active,
            (AccountStatus::Removed, _) => {
                return Err(HarnessError::conflict(
                    "removed accounts cannot be restored",
                ));
            }
            (_, AccountStatusAction::Remove) => AccountStatus::Removed,
            _ => {
                return Err(HarnessError::conflict(
                    "this action is not available for the current account status",
                ));
            }
        };
        let revision = status_revision
            .checked_add(1)
            .ok_or_else(|| HarnessError::execution("account status revision overflow"))?;
        sqlx::query(
            "UPDATE control_users SET status = $2, status_revision = $3 WHERE user_id = $1",
        )
        .bind(user_id.as_str())
        .bind(status.as_str())
        .bind(number(revision)?)
        .execute(&mut **tx)
        .await
        .map_err(database_error)?;
        if status != AccountStatus::Active {
            revoke_credentials_in(tx, user_id, now_ms).await?;
        }
        append_platform_audit(
            tx,
            &actor.user_id,
            "account.status",
            user_id.as_str(),
            json!({"previous":account.status,"action":action,"status":status,"revision":revision}),
            now_ms,
        )
        .await?;
        account_in(tx, user_id).await
    }
}

async fn revoke_credentials_in(
    tx: &mut Transaction,
    user_id: &UserId,
    now_ms: u64,
) -> Result<(), HarnessError> {
    let now = number(now_ms)?;
    sqlx::query("DELETE FROM control_oidc_sessions WHERE (issuer, subject) IN (SELECT issuer, subject FROM control_users WHERE user_id = $1)")
        .bind(user_id.as_str()).execute(&mut **tx).await.map_err(database_error)?;
    sqlx::query("UPDATE control_browser_sessions SET revoked_at_ms = COALESCE(revoked_at_ms, $2) WHERE user_id = $1")
        .bind(user_id.as_str()).bind(now).execute(&mut **tx).await.map_err(database_error)?;
    crate::model_store::scope(tx).await?;
    sqlx::query("UPDATE control_model_device_authorizations SET state='denied' WHERE user_id=$1 AND state='approved'")
        .bind(user_id.as_str()).execute(&mut **tx).await.map_err(database_error)?;
    sqlx::query("UPDATE control_model_devices SET revoked_at_ms = COALESCE(revoked_at_ms, $2) WHERE user_id = $1")
        .bind(user_id.as_str()).bind(number(now_ms)?).execute(&mut **tx).await.map_err(database_error)?;
    sqlx::query("UPDATE control_model_keys SET revoked_at_ms = COALESCE(revoked_at_ms, $2) WHERE user_id = $1")
        .bind(user_id.as_str()).bind(now).execute(&mut **tx).await.map_err(database_error)?;
    // Consumed invitations remain as attribution records. Unused grants never revive on unban.
    sqlx::query("UPDATE control_user_invitations SET consumed_at_ms = $2 WHERE created_by = $1 AND consumed_at_ms IS NULL")
        .bind(user_id.as_str()).bind(now).execute(&mut **tx).await.map_err(database_error)?;
    let tenants: Vec<String> = sqlx::query_scalar(match ternilo_storage::backend(tx) {
        Backend::Postgres => "SELECT tenant_id FROM ternilo_account_credential_tenants($1)",
        Backend::Sqlite => "SELECT tenant_id FROM control_executors WHERE owner_user_id = $1 UNION SELECT tenant_id FROM control_executor_enrollments WHERE created_by = $1 ORDER BY tenant_id",
    }).bind(user_id.as_str()).fetch_all(&mut **tx).await.map_err(database_error)?;
    for tenant in tenants {
        set_tenant_scope(tx, &TenantId::new(tenant.clone())).await?;
        sqlx::query("UPDATE control_executor_enrollments SET consumed_at_ms = $3 WHERE tenant_id = $1 AND created_by = $2 AND consumed_at_ms IS NULL")
            .bind(&tenant).bind(user_id.as_str()).bind(now).execute(&mut **tx).await.map_err(database_error)?;
        // Node mutations lock enrollment rows, executor rows, then credential rows.
        sqlx::query("UPDATE control_executors SET state = 'revoked' WHERE tenant_id = $1 AND owner_user_id = $2")
            .bind(&tenant).bind(user_id.as_str()).execute(&mut **tx).await.map_err(database_error)?;
        sqlx::query("UPDATE control_node_credentials SET revoked_at_ms = COALESCE(revoked_at_ms, $3) WHERE tenant_id = $1 AND executor_id IN (SELECT executor_id FROM control_executors WHERE tenant_id = $1 AND owner_user_id = $2)")
            .bind(&tenant).bind(user_id.as_str()).bind(now).execute(&mut **tx).await.map_err(database_error)?;
    }
    Ok(())
}

fn number(value: u64) -> Result<i64, HarnessError> {
    i64::try_from(value).map_err(|_| HarnessError::invalid("account value exceeds database range"))
}

#[cfg(test)]
pub(crate) mod tests;

#[cfg(test)]
pub(crate) mod lock_order_tests;
