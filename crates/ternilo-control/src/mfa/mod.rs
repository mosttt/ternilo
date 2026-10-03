use serde::{Deserialize, Serialize};
use sqlx::Row;
use ternilo_protocol::{HarnessError, UserId};
use ternilo_storage::{Backend, Database, Transaction, database_error, lock};
use zeroize::Zeroizing;

use crate::{
    ControlStore, ControlUser, EncryptedSecret, VerifiedNativeCredentials,
    account_store::{append_platform_audit, require_active_account_in},
    crypto::{hex, random_identifier, random_token, token_hash},
    native_recovery::revoke_password_sessions,
};

pub(crate) mod oidc;
pub(crate) mod rotation;
pub use oidc::MfaOidcChallenge;

const SCOPE: &str = "account-mfa-v1";
const ACCESS: &str = "REVOKE ALL ON control_mfa_factors, control_mfa_oidc_challenges, control_oidc_mfa_assurances FROM PUBLIC;
DO $$ BEGIN IF EXISTS(SELECT 1 FROM pg_roles WHERE rolname='ternilo_runtime') THEN
GRANT SELECT, INSERT, UPDATE, DELETE ON control_mfa_factors, control_mfa_oidc_challenges, control_oidc_mfa_assurances TO ternilo_runtime;
END IF; END $$;";

pub(crate) async fn initialize(database: &Database) -> Result<(), HarnessError> {
    database
        .initialize(
            "mfa",
            1,
            match database.backend() {
                Backend::Sqlite => include_str!("sqlite.sql"),
                Backend::Postgres => include_str!("postgres.sql"),
            },
            ACCESS,
        )
        .await
}

#[derive(Serialize)]
pub struct MfaStatus {
    pub enabled: bool,
    pub enabled_at_ms: Option<u64>,
    pub recovery_codes_remaining: usize,
}

#[derive(Serialize)]
pub struct MfaEnrollment {
    pub generation: String,
    pub secret: String,
    pub qr_code: String,
    pub recovery_codes: Vec<String>,
    pub expires_at_ms: u64,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FactorSecret {
    key: Vec<u8>,
    recovery_hashes: Vec<String>,
}
struct Factor {
    generation: String,
    enabled: bool,
    expires_at_ms: i64,
    enabled_at_ms: i64,
    last_step: i64,
    failures: i64,
    blocked_until_ms: i64,
    secret: FactorSecret,
}

impl ControlStore {
    /// Operator-only recovery through the private Server configuration, never a public endpoint.
    pub async fn reset_native_mfa(
        &self,
        username: &str,
        now: u64,
    ) -> Result<ControlUser, HarnessError> {
        let username = crate::identity_store::normalize_username(username)?;
        let mut tx = self.database.begin().await?;
        lock(&mut tx, "ternilo:instance").await?;
        let instance = crate::identity_store::required_instance(&mut tx).await?;
        let row = sqlx::query("SELECT u.user_id,u.username FROM control_users u JOIN control_native_accounts n ON n.user_id=u.user_id WHERE u.username=$1")
            .bind(&username).fetch_optional(&mut *tx).await.map_err(database_error)?.ok_or_else(|| HarnessError::invalid("native account does not exist"))?;
        let user = ControlUser {
            user_id: UserId::new(
                row.try_get::<String, _>("user_id")
                    .map_err(database_error)?,
            ),
            username: row.try_get("username").map_err(database_error)?,
        };
        lock(&mut tx, &format!("ternilo:account-role:{}", user.user_id)).await?;
        let status: String =
            sqlx::query_scalar("SELECT status FROM control_users WHERE user_id=$1")
                .bind(user.user_id.as_str())
                .fetch_one(&mut *tx)
                .await
                .map_err(database_error)?;
        if crate::AccountStatus::parse(&status)? == crate::AccountStatus::Removed {
            return Err(HarnessError::policy("removed accounts cannot be recovered"));
        }
        sqlx::query("DELETE FROM control_mfa_factors WHERE user_id=$1")
            .bind(user.user_id.as_str())
            .execute(&mut *tx)
            .await
            .map_err(database_error)?;
        let (native, oidc) =
            revoke_password_sessions(&mut tx, &user.user_id, timestamp(now)?).await?;
        append_platform_audit(&mut tx, &instance.owner_user_id, "account.mfa.reset", user.user_id.as_str(), serde_json::json!({"source":"operator_cli","native_sessions_revoked":native,"oidc_sessions_revoked":oidc}), now).await?;
        tx.commit().await.map_err(database_error)?;
        Ok(user)
    }

    pub async fn mfa_status(&self, actor: &ControlUser) -> Result<MfaStatus, HarnessError> {
        let mut tx = self.database.begin().await?;
        require_active_account_in(&mut tx, &actor.user_id).await?;
        let factor = load(self, &mut tx, &actor.user_id).await?;
        tx.commit().await.map_err(database_error)?;
        Ok(match factor.filter(|factor| factor.enabled) {
            Some(factor) => MfaStatus {
                enabled: true,
                enabled_at_ms: Some(factor.enabled_at_ms.cast_unsigned()),
                recovery_codes_remaining: factor.secret.recovery_hashes.len(),
            },
            None => MfaStatus {
                enabled: false,
                enabled_at_ms: None,
                recovery_codes_remaining: 0,
            },
        })
    }

    pub async fn begin_mfa_enrollment(
        &self,
        actor: &ControlUser,
        password: &str,
        now: u64,
    ) -> Result<MfaEnrollment, HarnessError> {
        let credentials = self
            .verify_native_credentials(&actor.username, password)
            .await?;
        if credentials.user != *actor {
            return Err(HarnessError::policy(
                "credentials do not belong to this account",
            ));
        }
        let secret = totp_rs::Secret::from(rand::random::<[u8; 20]>());
        let key = secret.as_bytes().to_vec();
        let label = format!("{} ({})", actor.username, actor.user_id);
        let totp = totp_rs::Builder::new()
            .with_secret(secret)
            .with_algorithm(totp_rs::Algorithm::SHA1)
            .with_digits(6)
            .with_skew(1)
            .with_step_duration(30)
            .with_issuer(Some("Ternilo"))
            .with_account_name(label)
            .build()
            .map_err(|_| HarnessError::execution("could not prepare MFA enrollment"))?;
        let generation = random_identifier("ter_mf");
        let recovery_codes: Vec<String> = (0..8).map(|_| random_token("ter_mr")).collect();
        let secret = FactorSecret {
            key,
            recovery_hashes: recovery_codes
                .iter()
                .map(|code| hex(&token_hash(code)))
                .collect(),
        };
        let encrypted = encrypt(self, &actor.user_id, &generation, &secret)?;
        let expires_at_ms = now.saturating_add(10 * 60_000);
        let qr_code = format!(
            "data:image/png;base64,{}",
            totp.to_qr_base64()
                .map_err(|_| HarnessError::execution("could not encode MFA enrollment"))?
        );
        let mut tx = self.database.begin().await?;
        require_credentials(&mut tx, &credentials).await?;
        if load(self, &mut tx, &actor.user_id)
            .await?
            .is_some_and(|factor| factor.enabled)
        {
            return Err(HarnessError::conflict("MFA is already enabled"));
        }
        sqlx::query("INSERT INTO control_mfa_factors(user_id,generation,status,expires_at_ms,enabled_at_ms,last_step,failures,blocked_until_ms,nonce,ciphertext) VALUES($1,$2,'pending',$3,0,-1,0,0,$4,$5) ON CONFLICT(user_id) DO UPDATE SET generation=excluded.generation,status='pending',expires_at_ms=excluded.expires_at_ms,enabled_at_ms=0,last_step=-1,failures=0,blocked_until_ms=0,nonce=excluded.nonce,ciphertext=excluded.ciphertext")
            .bind(actor.user_id.as_str()).bind(&generation).bind(timestamp(expires_at_ms)?).bind(encrypted.nonce.to_vec()).bind(encrypted.ciphertext)
            .execute(&mut *tx).await.map_err(database_error)?;
        tx.commit().await.map_err(database_error)?;
        Ok(MfaEnrollment {
            generation,
            secret: totp.secret().to_base32(),
            qr_code,
            recovery_codes,
            expires_at_ms,
        })
    }

    pub async fn activate_mfa(
        &self,
        actor: &ControlUser,
        password: &str,
        generation: &str,
        code: &str,
        now: u64,
    ) -> Result<(), HarnessError> {
        let credentials = self
            .verify_native_credentials(&actor.username, password)
            .await?;
        if credentials.user != *actor {
            return Err(HarnessError::policy(
                "credentials do not belong to this account",
            ));
        }
        let mut tx = self.database.begin().await?;
        require_credentials(&mut tx, &credentials).await?;
        let mut factor = load(self, &mut tx, &actor.user_id)
            .await?
            .ok_or_else(invalid_code)?;
        if factor.enabled
            || factor.generation != generation
            || factor.expires_at_ms <= timestamp(now)?
        {
            return Err(HarnessError::conflict(
                "MFA enrollment changed or expired; start again",
            ));
        }
        let valid =
            verify_code(self, &mut tx, &actor.user_id, &mut factor, code, now, false).await?;
        if !valid {
            tx.commit().await.map_err(database_error)?;
            return Err(invalid_code());
        }
        sqlx::query("UPDATE control_mfa_factors SET status='enabled',enabled_at_ms=$2,expires_at_ms=0 WHERE user_id=$1")
            .bind(actor.user_id.as_str()).bind(timestamp(now)?).execute(&mut *tx).await.map_err(database_error)?;
        revoke_password_sessions(&mut tx, &actor.user_id, timestamp(now)?).await?;
        append_platform_audit(
            &mut tx,
            &actor.user_id,
            "account.mfa.enable",
            actor.user_id.as_str(),
            serde_json::json!({}),
            now,
        )
        .await?;
        tx.commit().await.map_err(database_error)
    }

    pub async fn disable_mfa(
        &self,
        actor: &ControlUser,
        password: &str,
        code: &str,
        now: u64,
    ) -> Result<(), HarnessError> {
        let credentials = self
            .verify_native_credentials(&actor.username, password)
            .await?;
        if credentials.user != *actor {
            return Err(HarnessError::policy(
                "credentials do not belong to this account",
            ));
        }
        let mut tx = self.database.begin().await?;
        require_credentials(&mut tx, &credentials).await?;
        let mut factor = load(self, &mut tx, &actor.user_id)
            .await?
            .filter(|factor| factor.enabled)
            .ok_or_else(invalid_code)?;
        let valid =
            verify_code(self, &mut tx, &actor.user_id, &mut factor, code, now, true).await?;
        if !valid {
            tx.commit().await.map_err(database_error)?;
            return Err(invalid_code());
        }
        sqlx::query("DELETE FROM control_mfa_factors WHERE user_id=$1")
            .bind(actor.user_id.as_str())
            .execute(&mut *tx)
            .await
            .map_err(database_error)?;
        revoke_password_sessions(&mut tx, &actor.user_id, timestamp(now)?).await?;
        append_platform_audit(
            &mut tx,
            &actor.user_id,
            "account.mfa.disable",
            actor.user_id.as_str(),
            serde_json::json!({}),
            now,
        )
        .await?;
        tx.commit().await.map_err(database_error)
    }

    pub async fn verify_native_mfa(
        &self,
        mut credentials: VerifiedNativeCredentials,
        code: Option<&str>,
        now: u64,
    ) -> Result<VerifiedNativeCredentials, HarnessError> {
        let mut tx = self.database.begin().await?;
        require_credentials(&mut tx, &credentials).await?;
        if let Some(mut factor) = load(self, &mut tx, &credentials.user.user_id)
            .await?
            .filter(|factor| factor.enabled)
        {
            let code = code.ok_or_else(required)?;
            let valid = verify_code(
                self,
                &mut tx,
                &credentials.user.user_id,
                &mut factor,
                code,
                now,
                true,
            )
            .await?;
            if !valid {
                tx.commit().await.map_err(database_error)?;
                return Err(invalid_code());
            }
            credentials.mfa_generation = Some(factor.generation);
        }
        tx.commit().await.map_err(database_error)?;
        Ok(credentials)
    }
}

pub(crate) async fn require_generation(
    tx: &mut Transaction,
    user: &UserId,
    generation: Option<&str>,
) -> Result<(), HarnessError> {
    let enabled: Option<String> = sqlx::query_scalar(
        "SELECT generation FROM control_mfa_factors WHERE user_id=$1 AND status='enabled'",
    )
    .bind(user.as_str())
    .fetch_optional(&mut **tx)
    .await
    .map_err(database_error)?;
    if enabled.is_some() && enabled.as_deref() != generation {
        return Err(required());
    }
    Ok(())
}

async fn require_credentials(
    tx: &mut Transaction,
    credentials: &VerifiedNativeCredentials,
) -> Result<(), HarnessError> {
    lock(
        tx,
        &format!("ternilo:account-role:{}", credentials.user.user_id),
    )
    .await?;
    require_active_account_in(tx, &credentials.user.user_id).await?;
    let valid: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM control_native_accounts WHERE user_id=$1 AND password_hash=$2",
    )
    .bind(credentials.user.user_id.as_str())
    .bind(&credentials.password_hash)
    .fetch_optional(&mut **tx)
    .await
    .map_err(database_error)?;
    if valid.is_none() {
        return Err(HarnessError::policy(
            "password changed during verification; sign in again",
        ));
    }
    Ok(())
}

fn encrypt(
    store: &ControlStore,
    user: &UserId,
    generation: &str,
    secret: &FactorSecret,
) -> Result<EncryptedSecret, HarnessError> {
    let bytes = Zeroizing::new(
        serde_json::to_vec(secret)
            .map_err(|_| HarnessError::execution("encode MFA configuration"))?,
    );
    store
        .cipher
        .encrypt(SCOPE, Some(user.as_str()), generation, 1, &bytes)
}

async fn load(
    store: &ControlStore,
    tx: &mut Transaction,
    user: &UserId,
) -> Result<Option<Factor>, HarnessError> {
    let row = sqlx::query("SELECT generation,status,expires_at_ms,enabled_at_ms,last_step,failures,blocked_until_ms,nonce,ciphertext FROM control_mfa_factors WHERE user_id=$1")
        .bind(user.as_str()).fetch_optional(&mut **tx).await.map_err(database_error)?;
    row.map(|row| {
        let generation: String = row.try_get("generation").map_err(database_error)?;
        let nonce: Vec<u8> = row.try_get("nonce").map_err(database_error)?;
        let encrypted = EncryptedSecret {
            nonce: nonce
                .try_into()
                .map_err(|_| HarnessError::execution("invalid MFA configuration"))?,
            ciphertext: row.try_get("ciphertext").map_err(database_error)?,
        };
        let bytes = store
            .cipher
            .decrypt(SCOPE, Some(user.as_str()), &generation, 1, &encrypted)?;
        Ok(Factor {
            generation,
            enabled: row.try_get::<String, _>("status").map_err(database_error)? == "enabled",
            expires_at_ms: row.try_get("expires_at_ms").map_err(database_error)?,
            enabled_at_ms: row.try_get("enabled_at_ms").map_err(database_error)?,
            last_step: row.try_get("last_step").map_err(database_error)?,
            failures: row.try_get("failures").map_err(database_error)?,
            blocked_until_ms: row.try_get("blocked_until_ms").map_err(database_error)?,
            secret: serde_json::from_slice(&bytes)
                .map_err(|_| HarnessError::execution("invalid MFA configuration"))?,
        })
    })
    .transpose()
}

async fn verify_code(
    store: &ControlStore,
    tx: &mut Transaction,
    user: &UserId,
    factor: &mut Factor,
    code: &str,
    now: u64,
    allow_recovery: bool,
) -> Result<bool, HarnessError> {
    let code = code.trim();
    let digest = hex(&token_hash(code));
    let recovery = allow_recovery
        .then(|| {
            factor
                .secret
                .recovery_hashes
                .iter()
                .position(|stored| *stored == digest)
        })
        .flatten();
    let mut valid = false;
    if let Some(index) = recovery {
        factor.secret.recovery_hashes.remove(index);
        valid = true;
    } else if factor.blocked_until_ms <= timestamp(now)? {
        let totp = totp_rs::Builder::new()
            .with_secret(factor.secret.key.clone())
            .with_algorithm(totp_rs::Algorithm::SHA1)
            .with_digits(6)
            .with_skew(1)
            .with_step_duration(30)
            .build()
            .map_err(|_| HarnessError::execution("invalid MFA configuration"))?;
        if let Some(step) = totp.check(code, now / 1000) {
            let step = i64::try_from(step)
                .map_err(|_| HarnessError::invalid("MFA step exceeds database range"))?;
            if step > factor.last_step {
                factor.last_step = step;
                valid = true;
            }
        }
        if factor.blocked_until_ms > 0 {
            factor.failures = 0;
            factor.blocked_until_ms = 0;
        }
    } else {
        return Ok(false);
    }
    if valid {
        factor.failures = 0;
        factor.blocked_until_ms = 0;
    } else {
        factor.failures += 1;
        if factor.failures >= 5 {
            factor.blocked_until_ms = timestamp(now.saturating_add(5 * 60_000))?;
        }
    }
    let encrypted = encrypt(store, user, &factor.generation, &factor.secret)?;
    sqlx::query("UPDATE control_mfa_factors SET last_step=$2,failures=$3,blocked_until_ms=$4,nonce=$5,ciphertext=$6 WHERE user_id=$1")
        .bind(user.as_str()).bind(factor.last_step).bind(factor.failures).bind(factor.blocked_until_ms).bind(encrypted.nonce.to_vec()).bind(encrypted.ciphertext)
        .execute(&mut **tx).await.map_err(database_error)?;
    Ok(valid)
}
fn required() -> HarnessError {
    HarnessError::policy("multi-factor verification is required")
}
fn invalid_code() -> HarnessError {
    HarnessError::policy("verification code is invalid, already used or temporarily limited")
}
fn timestamp(now: u64) -> Result<i64, HarnessError> {
    i64::try_from(now).map_err(|_| HarnessError::invalid("MFA timestamp exceeds database range"))
}
