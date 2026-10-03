use serde::{Deserialize, Serialize};
use sqlx::{Row, any::AnyRow};
use ternilo_protocol::{HarnessError, UserId};
use ternilo_storage::{Backend, Database, Transaction, database_error, lock};
use zeroize::Zeroizing;

use crate::{
    ControlStore, EncryptedSecret, OidcPrincipal, SecretCipher,
    crypto::{hex, random_identifier, random_token, token_hash},
};

mod details;

const SCOPE: &str = "server-oidc-session-v1";
const REFRESH_TTL_MS: u64 = 7 * 24 * 60 * 60 * 1000;

#[derive(Clone, Serialize, Deserialize)]
pub struct OidcSessionIdentity {
    pub principal: OidcPrincipal,
    pub nonce: String,
    pub upstream_refresh_token: Option<String>,
}

pub struct OidcRefreshSession {
    pub identity: OidcSessionIdentity,
    pub expires_at_ms: u64,
}

pub struct OidcSessionGrant {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at_ms: u64,
}

const SQLITE_SCHEMA: &str = "CREATE TABLE control_oidc_sessions (
    session_id TEXT PRIMARY KEY, token_hash TEXT NOT NULL UNIQUE, refresh_hash TEXT UNIQUE,
    binding TEXT NOT NULL, issuer TEXT NOT NULL, subject TEXT NOT NULL,
    access_expires_at_ms BIGINT NOT NULL, expires_at_ms BIGINT NOT NULL,
    nonce BLOB NOT NULL, ciphertext BLOB NOT NULL
);
CREATE INDEX control_oidc_sessions_expiry ON control_oidc_sessions(expires_at_ms);
CREATE INDEX control_oidc_sessions_subject ON control_oidc_sessions(issuer, subject);";
const POSTGRES_SCHEMA: &str = "CREATE TABLE control_oidc_sessions (
    session_id TEXT PRIMARY KEY, token_hash TEXT NOT NULL UNIQUE, refresh_hash TEXT UNIQUE,
    binding TEXT NOT NULL, issuer TEXT NOT NULL, subject TEXT NOT NULL,
    access_expires_at_ms BIGINT NOT NULL, expires_at_ms BIGINT NOT NULL,
    nonce BYTEA NOT NULL, ciphertext BYTEA NOT NULL
);
CREATE INDEX control_oidc_sessions_expiry ON control_oidc_sessions(expires_at_ms);
CREATE INDEX control_oidc_sessions_subject ON control_oidc_sessions(issuer, subject);";
const POSTGRES_ACCESS: &str = "REVOKE ALL ON control_oidc_sessions FROM PUBLIC;
DO $$ BEGIN
IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'ternilo_runtime') THEN
    GRANT SELECT, INSERT, UPDATE, DELETE ON control_oidc_sessions TO ternilo_runtime;
END IF;
END $$;";

pub(crate) async fn initialize(database: &Database) -> Result<(), HarnessError> {
    database
        .initialize(
            "oidc_sessions",
            1,
            match database.backend() {
                Backend::Sqlite => SQLITE_SCHEMA,
                Backend::Postgres => POSTGRES_SCHEMA,
            },
            POSTGRES_ACCESS,
        )
        .await?;
    details::initialize(database).await
}

enum SessionAuthentication<'a> {
    FirstFactor,
    Refresh(&'a str),
    Mfa(&'a str),
}

impl SessionAuthentication<'_> {
    async fn authorize(
        &self,
        transaction: &mut Transaction,
        principal: &OidcPrincipal,
        session_id: &str,
    ) -> Result<Option<UserId>, HarnessError> {
        match self {
            Self::Refresh(_) => {
                crate::mfa::oidc::authorize_oidc_session(transaction, principal, session_id)
                    .await?;
            }
            Self::FirstFactor => {
                crate::mfa::oidc::authorize_new_oidc(transaction, principal, None).await?;
            }
            Self::Mfa(generation) => {
                return crate::mfa::oidc::authorize_new_oidc(
                    transaction,
                    principal,
                    Some(generation),
                )
                .await;
            }
        }
        Ok(None)
    }
}

impl ControlStore {
    pub async fn create_oidc_session(
        &self,
        identity: &OidcSessionIdentity,
        binding: &str,
        expires_at_ms: u64,
        now_ms: u64,
    ) -> Result<OidcSessionGrant, HarnessError> {
        self.save_oidc_session(
            identity,
            binding,
            expires_at_ms,
            now_ms.saturating_add(REFRESH_TTL_MS),
            SessionAuthentication::FirstFactor,
            now_ms,
        )
        .await
    }

    pub async fn replace_oidc_session(
        &self,
        identity: &OidcSessionIdentity,
        binding: &str,
        expires_at_ms: u64,
        refresh: (&str, u64),
        now_ms: u64,
    ) -> Result<OidcSessionGrant, HarnessError> {
        self.save_oidc_session(
            identity,
            binding,
            expires_at_ms,
            refresh.1,
            SessionAuthentication::Refresh(refresh.0),
            now_ms,
        )
        .await
    }

    pub(crate) async fn create_mfa_oidc_session(
        &self,
        identity: &OidcSessionIdentity,
        binding: &str,
        expires_at_ms: u64,
        generation: &str,
        now_ms: u64,
    ) -> Result<OidcSessionGrant, HarnessError> {
        self.save_oidc_session(
            identity,
            binding,
            expires_at_ms,
            now_ms.saturating_add(REFRESH_TTL_MS),
            SessionAuthentication::Mfa(generation),
            now_ms,
        )
        .await
    }

    async fn save_oidc_session(
        &self,
        identity: &OidcSessionIdentity,
        binding: &str,
        access_expires_at_ms: u64,
        refresh_expires_at_ms: u64,
        authentication: SessionAuthentication<'_>,
        now_ms: u64,
    ) -> Result<OidcSessionGrant, HarnessError> {
        identity.principal.validate()?;
        if access_expires_at_ms <= now_ms || access_expires_at_ms > now_ms.saturating_add(3_600_000)
        {
            return Err(HarnessError::invalid("OIDC session expiry is invalid"));
        }
        let mut transaction = self.database.begin().await?;
        lock(&mut transaction, "ternilo:instance").await?;
        require_active_identity_in(&mut transaction, &identity.principal).await?;
        let previous = if let SessionAuthentication::Refresh(refresh) = authentication {
            Some(sqlx::query(ternilo_storage::for_update(&transaction,
                "SELECT session_id,expires_at_ms FROM control_oidc_sessions WHERE refresh_hash=$1 AND binding=$2 AND issuer=$3 AND subject=$4 AND expires_at_ms>$5",
                "SELECT session_id,expires_at_ms FROM control_oidc_sessions WHERE refresh_hash=$1 AND binding=$2 AND issuer=$3 AND subject=$4 AND expires_at_ms>$5 FOR UPDATE"))
                .bind(hex(&token_hash(refresh))).bind(binding).bind(&identity.principal.issuer)
                .bind(&identity.principal.subject).bind(timestamp(now_ms)?)
                .fetch_optional(&mut *transaction).await.map_err(database_error)?.ok_or_else(expired)?)
        } else {
            None
        };
        let session_id = match &previous {
            Some(row) => row.try_get("session_id").map_err(database_error)?,
            None => random_identifier("ter_s"),
        };
        let mfa_user = authentication
            .authorize(&mut transaction, &identity.principal, &session_id)
            .await?;
        let access_token = random_token("ter_o");
        let refresh_token = identity
            .upstream_refresh_token
            .as_ref()
            .map(|_| random_token("ter_r"));
        let expires_at_ms = if refresh_token.is_some() {
            refresh_expires_at_ms
        } else {
            access_expires_at_ms
        };
        let expires_at_ms = match &previous {
            Some(row) => expires_at_ms.min(
                row.try_get::<i64, _>("expires_at_ms")
                    .map_err(database_error)?
                    .cast_unsigned(),
            ),
            None => expires_at_ms,
        };
        if expires_at_ms <= now_ms {
            return Err(expired());
        }
        let access_expires_at_ms = access_expires_at_ms.min(expires_at_ms);
        let bytes = Zeroizing::new(
            serde_json::to_vec(identity)
                .map_err(|_| HarnessError::execution("encode OIDC session"))?,
        );
        let encrypted = self
            .cipher
            .encrypt(SCOPE, Some(&session_id), "identity", 1, &bytes)?;
        sqlx::query("DELETE FROM control_oidc_sessions WHERE expires_at_ms <= $1")
            .bind(timestamp(now_ms)?)
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
        if previous.is_some() {
            sqlx::query("UPDATE control_oidc_sessions SET token_hash=$2,refresh_hash=$3,access_expires_at_ms=$4,expires_at_ms=$5,nonce=$6,ciphertext=$7 WHERE session_id=$1")
                .bind(&session_id).bind(hex(&token_hash(&access_token)))
                .bind(refresh_token.as_ref().map(|token| hex(&token_hash(token))))
                .bind(timestamp(access_expires_at_ms)?).bind(timestamp(expires_at_ms)?)
                .bind(encrypted.nonce.to_vec()).bind(encrypted.ciphertext)
                .execute(&mut *transaction).await.map_err(database_error)?;
        } else {
            sqlx::query("INSERT INTO control_oidc_sessions (session_id,token_hash,refresh_hash,binding,issuer,subject,access_expires_at_ms,expires_at_ms,nonce,ciphertext) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)")
                .bind(&session_id).bind(hex(&token_hash(&access_token)))
                .bind(refresh_token.as_ref().map(|token| hex(&token_hash(token))))
                .bind(binding).bind(&identity.principal.issuer).bind(&identity.principal.subject)
                .bind(timestamp(access_expires_at_ms)?).bind(timestamp(expires_at_ms)?)
                .bind(encrypted.nonce.to_vec()).bind(encrypted.ciphertext)
                .execute(&mut *transaction).await.map_err(database_error)?;
            sqlx::query(
                "INSERT INTO control_oidc_session_details(session_id,created_at_ms) VALUES($1,$2)",
            )
            .bind(&session_id)
            .bind(timestamp(now_ms)?)
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
        }
        if let (SessionAuthentication::Mfa(generation), Some(user)) = (authentication, mfa_user) {
            sqlx::query("INSERT INTO control_oidc_mfa_assurances(session_id,user_id,generation) VALUES($1,$2,$3)")
                .bind(&session_id).bind(user.as_str()).bind(generation).execute(&mut *transaction).await.map_err(database_error)?;
        }
        transaction.commit().await.map_err(database_error)?;
        Ok(OidcSessionGrant {
            access_token,
            refresh_token,
            expires_at_ms: access_expires_at_ms.min(expires_at_ms),
        })
    }

    pub async fn authenticate_oidc_session(
        &self,
        token: &str,
        binding: &str,
        now_ms: u64,
    ) -> Result<(OidcPrincipal, u64), HarnessError> {
        if !token.starts_with("ter_o_") || token.len() > 128 {
            return Err(expired());
        }
        let row = sqlx::query("SELECT session_id, nonce, ciphertext, access_expires_at_ms FROM control_oidc_sessions WHERE token_hash = $1 AND binding = $2 AND access_expires_at_ms > $3 AND expires_at_ms > $3")
            .bind(hex(&token_hash(token))).bind(binding).bind(timestamp(now_ms)?)
            .fetch_optional(&self.pool).await.map_err(database_error)?.ok_or_else(expired)?;
        let identity = self.oidc_identity_from_row(&row)?;
        let mut tx = self.database.begin().await?;
        crate::mfa::oidc::authorize_oidc_session(
            &mut tx,
            &identity.principal,
            &row.try_get::<String, _>("session_id")
                .map_err(database_error)?,
        )
        .await?;
        tx.commit().await.map_err(database_error)?;
        let expiry = row
            .try_get::<i64, _>("access_expires_at_ms")
            .map_err(database_error)?
            .cast_unsigned();
        Ok((identity.principal, expiry))
    }

    pub async fn oidc_refresh_session(
        &self,
        token: &str,
        binding: &str,
        now_ms: u64,
    ) -> Result<OidcRefreshSession, HarnessError> {
        if !token.starts_with("ter_r_") || token.len() > 128 {
            return Err(expired());
        }
        let row = sqlx::query("SELECT session_id, nonce, ciphertext, expires_at_ms FROM control_oidc_sessions WHERE refresh_hash = $1 AND binding = $2 AND expires_at_ms > $3")
            .bind(hex(&token_hash(token))).bind(binding).bind(timestamp(now_ms)?)
            .fetch_optional(&self.pool).await.map_err(database_error)?.ok_or_else(expired)?;
        let identity = self.oidc_identity_from_row(&row)?;
        let mut tx = self.database.begin().await?;
        crate::mfa::oidc::authorize_oidc_session(
            &mut tx,
            &identity.principal,
            &row.try_get::<String, _>("session_id")
                .map_err(database_error)?,
        )
        .await?;
        tx.commit().await.map_err(database_error)?;
        Ok(OidcRefreshSession {
            identity,
            expires_at_ms: row
                .try_get::<i64, _>("expires_at_ms")
                .map_err(database_error)?
                .cast_unsigned(),
        })
    }

    pub async fn revoke_oidc_session(&self, token: &str) -> Result<(), HarnessError> {
        sqlx::query("DELETE FROM control_oidc_sessions WHERE token_hash = $1")
            .bind(hex(&token_hash(token)))
            .execute(&self.pool)
            .await
            .map_err(database_error)?;
        Ok(())
    }

    fn oidc_identity_from_row(&self, row: &AnyRow) -> Result<OidcSessionIdentity, HarnessError> {
        let session_id: String = row.try_get("session_id").map_err(database_error)?;
        let bytes = self.cipher.decrypt(
            SCOPE,
            Some(&session_id),
            "identity",
            1,
            &encrypted_from_row(row)?,
        )?;
        serde_json::from_slice(&bytes)
            .map_err(|_| HarnessError::execution("stored OIDC session is invalid"))
    }
}

fn encrypted_from_row(row: &AnyRow) -> Result<EncryptedSecret, HarnessError> {
    Ok(EncryptedSecret {
        nonce: row
            .try_get::<Vec<u8>, _>("nonce")
            .map_err(database_error)?
            .try_into()
            .map_err(|_| HarnessError::execution("stored OIDC nonce is invalid"))?,
        ciphertext: row.try_get("ciphertext").map_err(database_error)?,
    })
}

fn timestamp(value: u64) -> Result<i64, HarnessError> {
    i64::try_from(value).map_err(|_| HarnessError::invalid("OIDC timestamp exceeds database range"))
}

async fn require_active_identity_in(
    transaction: &mut Transaction,
    principal: &OidcPrincipal,
) -> Result<(), HarnessError> {
    if let Some(user) = crate::mfa::oidc::principal_user(transaction, principal).await? {
        lock(transaction, &format!("ternilo:account-role:{user}")).await?;
        crate::account_store::require_active_account_in(transaction, &user).await?;
    }
    Ok(())
}

fn expired() -> HarnessError {
    HarnessError::policy("OIDC session is invalid or expired")
}

pub(crate) async fn rotate(
    transaction: &mut Transaction,
    current: &SecretCipher,
    next: &SecretCipher,
) -> Result<u64, HarnessError> {
    let mut cursor = String::new();
    let mut count = 0;
    loop {
        let rows = sqlx::query("SELECT session_id, nonce, ciphertext FROM control_oidc_sessions WHERE session_id > $1 ORDER BY session_id LIMIT 128")
            .bind(&cursor).fetch_all(&mut **transaction).await.map_err(database_error)?;
        if rows.is_empty() {
            return Ok(count);
        }
        for row in rows {
            cursor = row.try_get("session_id").map_err(database_error)?;
            let bytes = current.decrypt(
                SCOPE,
                Some(&cursor),
                "identity",
                1,
                &encrypted_from_row(&row)?,
            )?;
            let encrypted = next.encrypt(SCOPE, Some(&cursor), "identity", 1, &bytes)?;
            sqlx::query("UPDATE control_oidc_sessions SET nonce = $1, ciphertext = $2 WHERE session_id = $3")
                .bind(encrypted.nonce.to_vec()).bind(encrypted.ciphertext).bind(&cursor)
                .execute(&mut **transaction).await.map_err(database_error)?;
            count += 1;
        }
    }
}
