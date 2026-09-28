use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::{Row, any::AnyRow};
use ternilo_protocol::HarnessError;
use ternilo_storage::{Transaction, database_error, lock};

use crate::{
    ControlStore, ControlUser, OidcPrincipal,
    account_store::require_active_account_in,
    crypto::{hex, token_hash},
    identity_store::{require_remote_access, required_instance},
};

pub enum BrowserSessionAuthentication {
    NativeToken(String),
    VerifiedOidc(OidcPrincipal),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserLoginKind {
    Native,
    Oidc,
}

#[derive(Debug, Serialize)]
pub struct NativeBrowserSession {
    pub session_id: String,
    pub created_at_ms: u64,
    pub expires_at_ms: u64,
    pub is_current: bool,
}

#[derive(Debug, Serialize)]
pub struct BrowserSessions {
    pub current_login: BrowserLoginKind,
    pub sessions: Vec<NativeBrowserSession>,
}

#[derive(Debug, Serialize)]
pub struct BrowserSessionRevocation {
    pub revoked_count: u64,
    pub current_revoked: bool,
}

impl ControlStore {
    pub async fn list_browser_sessions(
        &self,
        actor: &ControlUser,
        authentication: &BrowserSessionAuthentication,
        now_ms: u64,
    ) -> Result<BrowserSessions, HarnessError> {
        let (mut transaction, current_hash) = self
            .browser_session_management(actor, authentication, now_ms)
            .await?;
        let rows = active_sessions_in(&mut transaction, actor, now_ms).await?;
        let mut sessions = rows
            .iter()
            .map(|row| {
                let stored_hash: String = row.try_get("token_hash").map_err(database_error)?;
                Ok(NativeBrowserSession {
                    session_id: public_session_id(actor, &stored_hash),
                    created_at_ms: unsigned(row.try_get("created_at_ms").map_err(database_error)?)?,
                    expires_at_ms: unsigned(row.try_get("expires_at_ms").map_err(database_error)?)?,
                    is_current: current_hash.as_ref() == Some(&stored_hash),
                })
            })
            .collect::<Result<Vec<_>, HarnessError>>()?;
        sessions.sort_by_key(|session| !session.is_current);
        transaction.commit().await.map_err(database_error)?;
        Ok(BrowserSessions {
            current_login: if current_hash.is_some() {
                BrowserLoginKind::Native
            } else {
                BrowserLoginKind::Oidc
            },
            sessions,
        })
    }

    pub async fn revoke_browser_session(
        &self,
        actor: &ControlUser,
        authentication: &BrowserSessionAuthentication,
        session_id: &str,
        now_ms: u64,
    ) -> Result<BrowserSessionRevocation, HarnessError> {
        let (mut transaction, current_hash) = self
            .browser_session_management(actor, authentication, now_ms)
            .await?;
        let rows = active_sessions_in(&mut transaction, actor, now_ms).await?;
        let mut target_hash = None;
        for row in rows {
            let stored_hash: String = row.try_get("token_hash").map_err(database_error)?;
            if public_session_id(actor, &stored_hash) == session_id {
                target_hash = Some(stored_hash);
                break;
            }
        }
        let target_hash = target_hash
            .ok_or_else(|| HarnessError::conflict("browser session is no longer available"))?;
        let result = sqlx::query("UPDATE control_browser_sessions SET revoked_at_ms = $3 WHERE user_id = $1 AND token_hash = $2 AND revoked_at_ms IS NULL AND expires_at_ms > $3")
            .bind(actor.user_id.as_str()).bind(&target_hash).bind(timestamp(now_ms)?)
            .execute(&mut *transaction).await.map_err(database_error)?;
        transaction.commit().await.map_err(database_error)?;
        Ok(BrowserSessionRevocation {
            revoked_count: result.rows_affected(),
            current_revoked: current_hash.as_ref() == Some(&target_hash),
        })
    }

    pub async fn revoke_other_browser_sessions(
        &self,
        actor: &ControlUser,
        authentication: &BrowserSessionAuthentication,
        now_ms: u64,
    ) -> Result<BrowserSessionRevocation, HarnessError> {
        let (mut transaction, current_hash) = self
            .browser_session_management(actor, authentication, now_ms)
            .await?;
        let result = sqlx::query("UPDATE control_browser_sessions SET revoked_at_ms = $3 WHERE user_id = $1 AND token_hash <> $2 AND revoked_at_ms IS NULL AND expires_at_ms > $3")
            .bind(actor.user_id.as_str()).bind(current_hash.as_deref().unwrap_or(""))
            .bind(timestamp(now_ms)?).execute(&mut *transaction).await.map_err(database_error)?;
        transaction.commit().await.map_err(database_error)?;
        Ok(BrowserSessionRevocation {
            revoked_count: result.rows_affected(),
            current_revoked: false,
        })
    }

    async fn browser_session_management(
        &self,
        actor: &ControlUser,
        authentication: &BrowserSessionAuthentication,
        now_ms: u64,
    ) -> Result<(Transaction, Option<String>), HarnessError> {
        let mut transaction = self.database.begin().await?;
        lock(
            &mut transaction,
            &format!("ternilo:account-role:{}", actor.user_id),
        )
        .await?;
        require_active_account_in(&mut transaction, &actor.user_id).await?;
        require_remote_access(&required_instance(&mut transaction).await?, actor)?;
        let current_hash = match authentication {
            BrowserSessionAuthentication::NativeToken(token) => {
                let stored_hash = hex(&token_hash(token));
                let exists: Option<String> = sqlx::query_scalar("SELECT token_hash FROM control_browser_sessions WHERE user_id = $1 AND token_hash = $2 AND revoked_at_ms IS NULL AND expires_at_ms > $3")
                    .bind(actor.user_id.as_str()).bind(&stored_hash).bind(timestamp(now_ms)?)
                    .fetch_optional(&mut *transaction).await.map_err(database_error)?;
                if !token.starts_with("kns_") || exists.is_none() {
                    return Err(HarnessError::policy(
                        "browser session is invalid or expired",
                    ));
                }
                Some(stored_hash)
            }
            BrowserSessionAuthentication::VerifiedOidc(principal) => {
                principal.validate()?;
                let user_id: Option<String> = sqlx::query_scalar("SELECT user_id FROM control_users WHERE user_id = $1 AND issuer = $2 AND subject = $3")
                    .bind(actor.user_id.as_str()).bind(&principal.issuer).bind(&principal.subject)
                    .fetch_optional(&mut *transaction).await.map_err(database_error)?;
                if principal.issuer == "ternilo:native" || user_id.is_none() {
                    return Err(HarnessError::policy(
                        "OIDC identity does not match the current account",
                    ));
                }
                None
            }
        };
        Ok((transaction, current_hash))
    }
}

async fn active_sessions_in(
    transaction: &mut Transaction,
    actor: &ControlUser,
    now_ms: u64,
) -> Result<Vec<AnyRow>, HarnessError> {
    sqlx::query("SELECT token_hash, created_at_ms, expires_at_ms FROM control_browser_sessions WHERE user_id = $1 AND revoked_at_ms IS NULL AND expires_at_ms > $2 ORDER BY created_at_ms DESC, token_hash")
        .bind(actor.user_id.as_str()).bind(timestamp(now_ms)?)
        .fetch_all(&mut **transaction).await.map_err(database_error)
}

fn public_session_id(actor: &ControlUser, stored_hash: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ternilo-browser-session-id-v1\0");
    digest.update(actor.user_id.as_str().as_bytes());
    digest.update(b"\0");
    digest.update(stored_hash.as_bytes());
    format!("knbs_{}", hex(&digest.finalize()))
}

fn timestamp(value: u64) -> Result<i64, HarnessError> {
    i64::try_from(value).map_err(|_| HarnessError::invalid("timestamp exceeds i64"))
}

fn unsigned(value: i64) -> Result<u64, HarnessError> {
    u64::try_from(value).map_err(|_| HarnessError::execution("stored timestamp is negative"))
}
