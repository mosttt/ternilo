use serde::Serialize;
use sqlx::Row;
use ternilo_protocol::{HarnessError, UserId};
use ternilo_storage::{database_error, lock};

use crate::{
    AccountStatus, ControlStore, ControlUser,
    account_store::append_platform_audit,
    identity_store::{hash_password, normalize_username, required_instance, validate_password},
};

#[derive(Debug, Serialize)]
pub struct NativePasswordReset {
    pub user: ControlUser,
    pub native_sessions_revoked: u64,
    pub oidc_sessions_revoked: u64,
}

impl ControlStore {
    /// Change only the authenticated user's password and invalidate existing logins.
    pub async fn change_native_password(
        &self,
        actor: &ControlUser,
        current_password: &str,
        new_password: &str,
        now_ms: u64,
    ) -> Result<NativePasswordReset, HarnessError> {
        validate_password(new_password)?;
        let credentials = self
            .verify_native_credentials(&actor.username, current_password)
            .await?;
        if credentials.user != *actor {
            return Err(HarnessError::policy(
                "credentials do not belong to this account",
            ));
        }
        let password_hash = hash_password(new_password).await?;
        let now = i64::try_from(now_ms).map_err(|_| {
            HarnessError::invalid("password change timestamp exceeds database range")
        })?;
        let mut transaction = self.database.begin().await?;
        lock(
            &mut transaction,
            &format!("ternilo:account-role:{}", actor.user_id),
        )
        .await?;
        crate::account_store::require_active_account_in(&mut transaction, &actor.user_id).await?;
        let changed = sqlx::query("UPDATE control_native_accounts SET password_hash=$2 WHERE user_id=$1 AND password_hash=$3")
            .bind(actor.user_id.as_str()).bind(password_hash).bind(&credentials.password_hash)
            .execute(&mut *transaction).await.map_err(database_error)?.rows_affected();
        if changed != 1 {
            return Err(HarnessError::policy(
                "password changed during verification; sign in again",
            ));
        }
        let (native_sessions_revoked, oidc_sessions_revoked) =
            revoke_password_sessions(&mut transaction, &actor.user_id, now).await?;
        append_platform_audit(&mut transaction, &actor.user_id, "account.password.change", actor.user_id.as_str(),
            serde_json::json!({"source":"self_service","native_sessions_revoked":native_sessions_revoked,"oidc_sessions_revoked":oidc_sessions_revoked}), now_ms).await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(NativePasswordReset {
            user: actor.clone(),
            native_sessions_revoked,
            oidc_sessions_revoked,
        })
    }

    /// Operator recovery using the existing private Server configuration, never a public route.
    pub async fn reset_native_password(
        &self,
        username: &str,
        password: &str,
        now_ms: u64,
    ) -> Result<NativePasswordReset, HarnessError> {
        let username = normalize_username(username)?;
        validate_password(password)?;
        let now = i64::try_from(now_ms).map_err(|_| {
            HarnessError::invalid("password reset timestamp exceeds database range")
        })?;
        let password_hash = hash_password(password).await?;
        let mut transaction = self.database.begin().await?;
        lock(&mut transaction, "ternilo:instance").await?;
        let instance = required_instance(&mut transaction).await?;
        let row = sqlx::query(
            "SELECT u.user_id, u.username FROM control_users u
             JOIN control_native_accounts n ON n.user_id=u.user_id WHERE u.username=$1",
        )
        .bind(&username)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?
        .ok_or_else(|| HarnessError::invalid("native account does not exist"))?;
        let user = ControlUser {
            user_id: UserId::new(
                row.try_get::<String, _>("user_id")
                    .map_err(database_error)?,
            ),
            username: row.try_get("username").map_err(database_error)?,
        };
        lock(
            &mut transaction,
            &format!("ternilo:account-role:{}", user.user_id),
        )
        .await?;
        let status: String =
            sqlx::query_scalar("SELECT status FROM control_users WHERE user_id=$1")
                .bind(user.user_id.as_str())
                .fetch_one(&mut *transaction)
                .await
                .map_err(database_error)?;
        if AccountStatus::parse(&status)? == AccountStatus::Removed {
            return Err(HarnessError::policy("removed accounts cannot be recovered"));
        }
        sqlx::query("UPDATE control_native_accounts SET password_hash=$2 WHERE user_id=$1")
            .bind(user.user_id.as_str())
            .bind(password_hash)
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
        let (native_sessions_revoked, oidc_sessions_revoked) =
            revoke_password_sessions(&mut transaction, &user.user_id, now).await?;
        append_platform_audit(
            &mut transaction,
            &instance.owner_user_id,
            "account.password.reset",
            user.user_id.as_str(),
            serde_json::json!({
                "source": "operator_cli",
                "native_sessions_revoked": native_sessions_revoked,
                "oidc_sessions_revoked": oidc_sessions_revoked,
            }),
            now_ms,
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(NativePasswordReset {
            user,
            native_sessions_revoked,
            oidc_sessions_revoked,
        })
    }
}

pub(crate) async fn revoke_password_sessions(
    transaction: &mut ternilo_storage::Transaction,
    user_id: &UserId,
    now: i64,
) -> Result<(u64, u64), HarnessError> {
    sqlx::query("DELETE FROM control_email_challenges WHERE user_id=$1")
        .bind(user_id.as_str())
        .execute(&mut **transaction)
        .await
        .map_err(database_error)?;
    let native_sessions_revoked = sqlx::query(
            "UPDATE control_browser_sessions SET revoked_at_ms=$2 WHERE user_id=$1 AND revoked_at_ms IS NULL",
        )
        .bind(user_id.as_str())
        .bind(now)
        .execute(&mut **transaction)
        .await
        .map_err(database_error)?
        .rows_affected();
    let oidc_sessions_revoked = sqlx::query(
        "DELETE FROM control_oidc_sessions WHERE (issuer, subject) IN
             (SELECT issuer, subject FROM control_users WHERE user_id=$1)",
    )
    .bind(user_id.as_str())
    .execute(&mut **transaction)
    .await
    .map_err(database_error)?
    .rows_affected();
    Ok((native_sessions_revoked, oidc_sessions_revoked))
}
