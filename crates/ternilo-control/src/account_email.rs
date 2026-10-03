use serde::Serialize;
use sqlx::Row;
use ternilo_protocol::{HarnessError, UserId};
use ternilo_storage::{Database, Transaction, database_error, lock};

use crate::{
    ControlStore, ControlUser, NativePasswordReset,
    account_store::{append_platform_audit, require_active_account_in},
    crypto::{hex, random_token, token_hash},
    identity_store::{hash_password, normalize_email, validate_password},
    native_recovery::revoke_password_sessions,
};

const SCHEMA: &str = "
CREATE TABLE control_verified_emails (
    user_id TEXT PRIMARY KEY REFERENCES control_users(user_id) ON DELETE CASCADE,
    email TEXT NOT NULL,
    verified_at_ms BIGINT NOT NULL
);
CREATE TABLE control_email_challenges (
    user_id TEXT NOT NULL REFERENCES control_users(user_id) ON DELETE CASCADE,
    purpose TEXT NOT NULL CHECK (purpose IN ('verify', 'reset')),
    email TEXT NOT NULL,
    token_hash TEXT NOT NULL UNIQUE,
    created_at_ms BIGINT NOT NULL,
    expires_at_ms BIGINT NOT NULL,
    PRIMARY KEY (user_id, purpose)
);
CREATE TABLE control_email_request_limits (bucket TEXT PRIMARY KEY, started_at_ms BIGINT NOT NULL, count BIGINT NOT NULL);
CREATE INDEX control_email_request_limits_expiry ON control_email_request_limits(started_at_ms);";
const ACCESS: &str = "REVOKE ALL ON control_verified_emails, control_email_challenges, control_email_request_limits FROM PUBLIC;
DO $$ BEGIN IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname='ternilo_runtime') THEN
GRANT SELECT, INSERT, UPDATE, DELETE ON control_verified_emails, control_email_challenges, control_email_request_limits TO ternilo_runtime;
END IF; END $$;";

pub(crate) async fn initialize(database: &Database) -> Result<(), HarnessError> {
    database
        .initialize("account-email", 1, SCHEMA, ACCESS)
        .await
}

#[derive(Serialize)]
pub struct AccountEmailStatus {
    pub email: Option<String>,
    pub verified_at_ms: Option<u64>,
}

/// Private delivery material. Never serialize it into an HTTP response or logs.
pub struct AccountEmailDelivery {
    pub email: String,
    pub token: String,
}

impl ControlStore {
    /// Bound public email delivery requests across all Server instances.
    pub async fn admit_email_delivery(&self, client: &str, now: u64) -> Result<bool, HarnessError> {
        let mut tx = self.database.begin().await?;
        lock(&mut tx, "ternilo:email-delivery-limits").await?;
        let cutoff = now
            .checked_sub(60_000)
            .map(timestamp)
            .transpose()?
            .unwrap_or(-1);
        sqlx::query("DELETE FROM control_email_request_limits WHERE started_at_ms<=$1")
            .bind(cutoff)
            .execute(&mut *tx)
            .await
            .map_err(database_error)?;
        for (bucket, limit) in [
            (format!("ip:{}", hex(&token_hash(client))), 5_i64),
            ("global".to_owned(), 100),
        ] {
            let updated = sqlx::query("INSERT INTO control_email_request_limits (bucket,started_at_ms,count) VALUES ($1,$2,1) ON CONFLICT(bucket) DO UPDATE SET count=control_email_request_limits.count+1 WHERE control_email_request_limits.count<$3")
                .bind(bucket).bind(timestamp(now)?).bind(limit).execute(&mut *tx).await.map_err(database_error)?.rows_affected();
            if updated == 0 {
                return Ok(false);
            }
        }
        tx.commit().await.map_err(database_error)?;
        Ok(true)
    }
    pub async fn account_email_status(
        &self,
        actor: &ControlUser,
    ) -> Result<AccountEmailStatus, HarnessError> {
        let row = sqlx::query("SELECT u.email, v.verified_at_ms FROM control_users u LEFT JOIN control_verified_emails v ON v.user_id=u.user_id AND v.email=LOWER(u.email) WHERE u.user_id=$1")
            .bind(actor.user_id.as_str()).fetch_one(self.database.pool()).await.map_err(database_error)?;
        Ok(AccountEmailStatus {
            email: row.try_get("email").map_err(database_error)?,
            verified_at_ms: row
                .try_get::<Option<i64>, _>("verified_at_ms")
                .map_err(database_error)?
                .map(i64::cast_unsigned),
        })
    }

    pub async fn request_email_verification(
        &self,
        actor: &ControlUser,
        now: u64,
    ) -> Result<Option<AccountEmailDelivery>, HarnessError> {
        let mut tx = self.database.begin().await?;
        lock(&mut tx, &format!("ternilo:account-role:{}", actor.user_id)).await?;
        require_active_account_in(&mut tx, &actor.user_id).await?;
        let email: Option<String> =
            sqlx::query_scalar("SELECT email FROM control_users WHERE user_id=$1")
                .bind(actor.user_id.as_str())
                .fetch_one(&mut *tx)
                .await
                .map_err(database_error)?;
        let email = normalize_email(
            &email.ok_or_else(|| HarnessError::invalid("account has no email address"))?,
        )?;
        let verified: Option<i64> = sqlx::query_scalar(
            "SELECT verified_at_ms FROM control_verified_emails WHERE user_id=$1 AND email=$2",
        )
        .bind(actor.user_id.as_str())
        .bind(&email)
        .fetch_optional(&mut *tx)
        .await
        .map_err(database_error)?;
        let delivery = if verified.is_some() {
            None
        } else {
            issue(&mut tx, &actor.user_id, &email, "verify", now).await?
        };
        tx.commit().await.map_err(database_error)?;
        Ok(delivery)
    }

    pub async fn verify_account_email(
        &self,
        actor: &ControlUser,
        token: &str,
        now: u64,
    ) -> Result<(), HarnessError> {
        let mut tx = self.database.begin().await?;
        lock(&mut tx, &format!("ternilo:account-role:{}", actor.user_id)).await?;
        require_active_account_in(&mut tx, &actor.user_id).await?;
        let email = consume(&mut tx, &actor.user_id, token, "verify", now).await?;
        sqlx::query("INSERT INTO control_verified_emails (user_id,email,verified_at_ms) VALUES ($1,$2,$3) ON CONFLICT(user_id) DO UPDATE SET email=excluded.email, verified_at_ms=excluded.verified_at_ms")
            .bind(actor.user_id.as_str()).bind(email).bind(timestamp(now)?).execute(&mut *tx).await.map_err(database_error)?;
        append_platform_audit(
            &mut tx,
            &actor.user_id,
            "account.email.verify",
            actor.user_id.as_str(),
            serde_json::json!({}),
            now,
        )
        .await?;
        tx.commit().await.map_err(database_error)
    }

    /// An absent, inactive, OIDC-only or unverified account yields the same public outcome.
    pub async fn request_password_recovery(
        &self,
        email: &str,
        now: u64,
    ) -> Result<Option<AccountEmailDelivery>, HarnessError> {
        let Ok(email) = normalize_email(email) else {
            return Ok(None);
        };
        let user: Option<String> = sqlx::query_scalar("SELECT u.user_id FROM control_users u JOIN control_native_accounts n ON n.user_id=u.user_id JOIN control_verified_emails v ON v.user_id=u.user_id AND v.email=LOWER(u.email) WHERE v.email=$1 AND u.status='active'")
            .bind(&email).fetch_optional(self.database.pool()).await.map_err(database_error)?;
        let Some(user) = user else { return Ok(None) };
        let user = UserId::new(user);
        let mut tx = self.database.begin().await?;
        lock(&mut tx, &format!("ternilo:account-role:{user}")).await?;
        let active: Option<i64> = sqlx::query_scalar("SELECT 1 FROM control_users u JOIN control_verified_emails v ON v.user_id=u.user_id AND v.email=LOWER(u.email) WHERE u.user_id=$1 AND v.email=$2 AND u.status='active'")
            .bind(user.as_str()).bind(&email).fetch_optional(&mut *tx).await.map_err(database_error)?;
        let delivery = if active.is_some() {
            issue(&mut tx, &user, &email, "reset", now).await?
        } else {
            None
        };
        tx.commit().await.map_err(database_error)?;
        Ok(delivery)
    }

    pub async fn recover_native_password(
        &self,
        token: &str,
        password: &str,
        now: u64,
    ) -> Result<NativePasswordReset, HarnessError> {
        validate_password(password)?;
        validate_token(token, "reset")?;
        let user: Option<String> = sqlx::query_scalar("SELECT user_id FROM control_email_challenges WHERE purpose='reset' AND token_hash=$1 AND expires_at_ms>$2")
            .bind(hex(&token_hash(token))).bind(timestamp(now)?).fetch_optional(self.database.pool()).await.map_err(database_error)?;
        let user = UserId::new(user.ok_or_else(invalid_challenge)?);
        let password_hash = hash_password(password).await?;
        let mut tx = self.database.begin().await?;
        lock(&mut tx, &format!("ternilo:account-role:{user}")).await?;
        require_active_account_in(&mut tx, &user).await?;
        let email = consume(&mut tx, &user, token, "reset", now).await?;
        let verified: Option<i64> = sqlx::query_scalar(
            "SELECT 1 FROM control_verified_emails WHERE user_id=$1 AND email=$2",
        )
        .bind(user.as_str())
        .bind(email)
        .fetch_optional(&mut *tx)
        .await
        .map_err(database_error)?;
        if verified.is_none() {
            return Err(invalid_challenge());
        }
        let updated =
            sqlx::query("UPDATE control_native_accounts SET password_hash=$2 WHERE user_id=$1")
                .bind(user.as_str())
                .bind(password_hash)
                .execute(&mut *tx)
                .await
                .map_err(database_error)?
                .rows_affected();
        if updated != 1 {
            return Err(invalid_challenge());
        }
        let username: String =
            sqlx::query_scalar("SELECT username FROM control_users WHERE user_id=$1")
                .bind(user.as_str())
                .fetch_one(&mut *tx)
                .await
                .map_err(database_error)?;
        let (native_sessions_revoked, oidc_sessions_revoked) =
            revoke_password_sessions(&mut tx, &user, timestamp(now)?).await?;
        append_platform_audit(&mut tx, &user, "account.password.recover", user.as_str(), serde_json::json!({"source":"verified_email","native_sessions_revoked":native_sessions_revoked,"oidc_sessions_revoked":oidc_sessions_revoked}), now).await?;
        tx.commit().await.map_err(database_error)?;
        Ok(NativePasswordReset {
            user: ControlUser {
                user_id: user,
                username,
            },
            native_sessions_revoked,
            oidc_sessions_revoked,
        })
    }
}

async fn issue(
    tx: &mut Transaction,
    user: &UserId,
    email: &str,
    purpose: &str,
    now: u64,
) -> Result<Option<AccountEmailDelivery>, HarnessError> {
    let token = random_token(if purpose == "verify" {
        "ter_ev"
    } else {
        "ter_pr"
    });
    let result = sqlx::query("INSERT INTO control_email_challenges (user_id,purpose,email,token_hash,created_at_ms,expires_at_ms) VALUES ($1,$2,$3,$4,$5,$6) ON CONFLICT(user_id,purpose) DO UPDATE SET email=excluded.email, token_hash=excluded.token_hash, created_at_ms=excluded.created_at_ms, expires_at_ms=excluded.expires_at_ms WHERE control_email_challenges.created_at_ms<=$7")
        .bind(user.as_str()).bind(purpose).bind(email).bind(hex(&token_hash(&token))).bind(timestamp(now)?).bind(timestamp(now.saturating_add(15 * 60_000))?).bind(now.checked_sub(60_000).map(timestamp).transpose()?.unwrap_or(-1))
        .execute(&mut **tx).await.map_err(database_error)?.rows_affected();
    Ok((result == 1).then(|| AccountEmailDelivery {
        email: email.to_owned(),
        token,
    }))
}

async fn consume(
    tx: &mut Transaction,
    user: &UserId,
    token: &str,
    purpose: &str,
    now: u64,
) -> Result<String, HarnessError> {
    validate_token(token, purpose)?;
    let email: Option<String> = sqlx::query_scalar("DELETE FROM control_email_challenges WHERE user_id=$1 AND purpose=$2 AND token_hash=$3 AND expires_at_ms>$4 AND email=(SELECT LOWER(email) FROM control_users WHERE user_id=$1) RETURNING email")
        .bind(user.as_str()).bind(purpose).bind(hex(&token_hash(token))).bind(timestamp(now)?).fetch_optional(&mut **tx).await.map_err(database_error)?;
    email.ok_or_else(invalid_challenge)
}

fn validate_token(token: &str, purpose: &str) -> Result<(), HarnessError> {
    let prefix = if purpose == "verify" {
        "ter_ev_"
    } else {
        "ter_pr_"
    };
    if !token.starts_with(prefix) || token.len() != prefix.len() + 43 {
        return Err(invalid_challenge());
    }
    Ok(())
}
fn invalid_challenge() -> HarnessError {
    HarnessError::policy("email link is invalid, expired or already used")
}
fn timestamp(now: u64) -> Result<i64, HarnessError> {
    i64::try_from(now).map_err(|_| HarnessError::invalid("email timestamp exceeds database range"))
}
