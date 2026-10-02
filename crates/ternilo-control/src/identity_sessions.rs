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
    OidcToken { token: String, binding: String },
    VerifiedOidc(OidcPrincipal),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserLoginKind {
    Native,
    Oidc,
}

#[derive(Debug, Serialize)]
pub struct BrowserSession {
    pub session_id: String,
    pub login_kind: BrowserLoginKind,
    pub issuer: Option<String>,
    pub created_at_ms: Option<u64>,
    pub expires_at_ms: u64,
    pub access_expires_at_ms: u64,
    pub is_current: bool,
    pub user_agent: Option<String>,
    pub first_ip: Option<String>,
    pub last_ip: Option<String>,
    pub last_active_at_ms: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct BrowserSessions {
    pub current_login: BrowserLoginKind,
    pub current_session_managed: bool,
    pub sessions: Vec<BrowserSession>,
}

#[derive(Debug, Serialize)]
pub struct BrowserSessionRevocation {
    pub revoked_count: u64,
    pub current_revoked: bool,
}

struct CurrentSession {
    kind: BrowserLoginKind,
    key: Option<String>,
}

impl ControlStore {
    pub async fn list_browser_sessions(
        &self,
        actor: &ControlUser,
        authentication: &BrowserSessionAuthentication,
        now_ms: u64,
    ) -> Result<BrowserSessions, HarnessError> {
        let (mut transaction, current) = self
            .browser_session_management(actor, authentication, now_ms)
            .await?;
        let rows = active_sessions_in(&mut transaction, actor, now_ms).await?;
        let mut sessions = rows
            .iter()
            .map(|row| {
                let key: String = row.try_get("session_key").map_err(database_error)?;
                let kind = login_kind(row)?;
                Ok(BrowserSession {
                    session_id: public_session_id(actor, &key),
                    login_kind: kind,
                    issuer: row.try_get("issuer").map_err(database_error)?,
                    created_at_ms: optional_time(row, "created_at_ms")?,
                    expires_at_ms: unsigned(row.try_get("expires_at_ms").map_err(database_error)?)?,
                    access_expires_at_ms: unsigned(
                        row.try_get("access_expires_at_ms")
                            .map_err(database_error)?,
                    )?,
                    is_current: current.kind == kind && current.key.as_ref() == Some(&key),
                    user_agent: row.try_get("user_agent").map_err(database_error)?,
                    first_ip: row.try_get("first_ip").map_err(database_error)?,
                    last_ip: row.try_get("last_ip").map_err(database_error)?,
                    last_active_at_ms: optional_time(row, "last_active_at_ms")?,
                })
            })
            .collect::<Result<Vec<_>, HarnessError>>()?;
        sessions.sort_by_key(|session| {
            (
                !session.is_current,
                std::cmp::Reverse(session.created_at_ms),
            )
        });
        transaction.commit().await.map_err(database_error)?;
        Ok(BrowserSessions {
            current_login: current.kind,
            current_session_managed: current.key.is_some(),
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
        let (mut transaction, current) = self
            .browser_session_management(actor, authentication, now_ms)
            .await?;
        let rows = active_sessions_in(&mut transaction, actor, now_ms).await?;
        let mut target = None;
        for row in rows {
            let key: String = row.try_get("session_key").map_err(database_error)?;
            if public_session_id(actor, &key) == session_id {
                target = Some((login_kind(&row)?, key));
                break;
            }
        }
        let (kind, key) = target
            .ok_or_else(|| HarnessError::conflict("browser session is no longer available"))?;
        let result = match kind {
            BrowserLoginKind::Native => sqlx::query("UPDATE control_browser_sessions SET revoked_at_ms=$3 WHERE user_id=$1 AND token_hash=$2 AND revoked_at_ms IS NULL AND expires_at_ms>$3")
                .bind(actor.user_id.as_str()).bind(&key).bind(timestamp(now_ms)?)
                .execute(&mut *transaction).await.map_err(database_error)?,
            BrowserLoginKind::Oidc => sqlx::query("DELETE FROM control_oidc_sessions WHERE session_id=$2 AND (issuer,subject) IN (SELECT issuer,subject FROM control_users WHERE user_id=$1)")
                .bind(actor.user_id.as_str()).bind(&key).execute(&mut *transaction).await.map_err(database_error)?,
        };
        transaction.commit().await.map_err(database_error)?;
        Ok(BrowserSessionRevocation {
            revoked_count: result.rows_affected(),
            current_revoked: current.kind == kind && current.key.as_ref() == Some(&key),
        })
    }

    pub async fn revoke_other_browser_sessions(
        &self,
        actor: &ControlUser,
        authentication: &BrowserSessionAuthentication,
        now_ms: u64,
    ) -> Result<BrowserSessionRevocation, HarnessError> {
        let (mut transaction, current) = self
            .browser_session_management(actor, authentication, now_ms)
            .await?;
        let native = sqlx::query("UPDATE control_browser_sessions SET revoked_at_ms=$3 WHERE user_id=$1 AND token_hash<>$2 AND revoked_at_ms IS NULL AND expires_at_ms>$3")
            .bind(actor.user_id.as_str()).bind(current.key.as_deref().unwrap_or(""))
            .bind(timestamp(now_ms)?).execute(&mut *transaction).await.map_err(database_error)?;
        let oidc = sqlx::query("DELETE FROM control_oidc_sessions WHERE (issuer,subject) IN (SELECT issuer,subject FROM control_users WHERE user_id=$1) AND session_id<>$2 AND expires_at_ms>$3")
            .bind(actor.user_id.as_str()).bind(current.key.as_deref().unwrap_or(""))
            .bind(timestamp(now_ms)?).execute(&mut *transaction).await.map_err(database_error)?;
        transaction.commit().await.map_err(database_error)?;
        Ok(BrowserSessionRevocation {
            revoked_count: native.rows_affected() + oidc.rows_affected(),
            current_revoked: false,
        })
    }

    async fn browser_session_management(
        &self,
        actor: &ControlUser,
        authentication: &BrowserSessionAuthentication,
        now_ms: u64,
    ) -> Result<(Transaction, CurrentSession), HarnessError> {
        let mut transaction = self.database.begin().await?;
        lock(
            &mut transaction,
            &format!("ternilo:account-role:{}", actor.user_id),
        )
        .await?;
        require_active_account_in(&mut transaction, &actor.user_id).await?;
        require_remote_access(&required_instance(&mut transaction).await?, actor)?;
        let current = match authentication {
            BrowserSessionAuthentication::NativeToken(token) => {
                let key = hex(&token_hash(token));
                let exists: Option<String> = sqlx::query_scalar("SELECT token_hash FROM control_browser_sessions WHERE user_id=$1 AND token_hash=$2 AND revoked_at_ms IS NULL AND expires_at_ms>$3")
                    .bind(actor.user_id.as_str()).bind(&key).bind(timestamp(now_ms)?)
                    .fetch_optional(&mut *transaction).await.map_err(database_error)?;
                if !token.starts_with("ter_a_") || exists.is_none() {
                    return Err(HarnessError::policy(
                        "browser session is invalid or expired",
                    ));
                }
                CurrentSession {
                    kind: BrowserLoginKind::Native,
                    key: Some(key),
                }
            }
            BrowserSessionAuthentication::OidcToken { token, binding } => {
                let key: Option<String> = sqlx::query_scalar("SELECT s.session_id FROM control_oidc_sessions s JOIN control_users u ON u.issuer=s.issuer AND u.subject=s.subject WHERE u.user_id=$1 AND s.token_hash=$2 AND s.binding=$3 AND s.access_expires_at_ms>$4 AND s.expires_at_ms>$4")
                    .bind(actor.user_id.as_str()).bind(hex(&token_hash(token))).bind(binding).bind(timestamp(now_ms)?)
                    .fetch_optional(&mut *transaction).await.map_err(database_error)?;
                if !token.starts_with("ter_o_") || key.is_none() {
                    return Err(HarnessError::policy("OIDC session is invalid or expired"));
                }
                CurrentSession {
                    kind: BrowserLoginKind::Oidc,
                    key,
                }
            }
            BrowserSessionAuthentication::VerifiedOidc(principal) => {
                principal.validate()?;
                let user_id: Option<String> = sqlx::query_scalar("SELECT user_id FROM control_users WHERE user_id=$1 AND issuer=$2 AND subject=$3")
                    .bind(actor.user_id.as_str()).bind(&principal.issuer).bind(&principal.subject)
                    .fetch_optional(&mut *transaction).await.map_err(database_error)?;
                if principal.issuer == "ternilo:native" || user_id.is_none() {
                    return Err(HarnessError::policy(
                        "OIDC identity does not match the current account",
                    ));
                }
                CurrentSession {
                    kind: BrowserLoginKind::Oidc,
                    key: None,
                }
            }
        };
        Ok((transaction, current))
    }
}

async fn active_sessions_in(
    transaction: &mut Transaction,
    actor: &ControlUser,
    now_ms: u64,
) -> Result<Vec<AnyRow>, HarnessError> {
    sqlx::query(r"
        SELECT s.token_hash AS session_key, 'native' AS login_kind, NULL AS issuer,
            s.created_at_ms,s.expires_at_ms,s.expires_at_ms AS access_expires_at_ms,
            d.user_agent,d.first_ip,d.last_ip,d.last_active_at_ms
        FROM control_browser_sessions s LEFT JOIN control_browser_session_details d ON d.token_hash=s.token_hash
        WHERE s.user_id=$1 AND s.revoked_at_ms IS NULL AND s.expires_at_ms>$2
        UNION ALL
        SELECT s.session_id AS session_key, 'oidc' AS login_kind, s.issuer,
            d.created_at_ms,s.expires_at_ms,s.access_expires_at_ms,
            d.user_agent,d.first_ip,d.last_ip,d.last_active_at_ms
        FROM control_oidc_sessions s
        JOIN control_users u ON u.issuer=s.issuer AND u.subject=s.subject
        LEFT JOIN control_oidc_session_details d ON d.session_id=s.session_id
        WHERE u.user_id=$1 AND s.expires_at_ms>$2
    ")
        .bind(actor.user_id.as_str()).bind(timestamp(now_ms)?)
        .fetch_all(&mut **transaction).await.map_err(database_error)
}

fn login_kind(row: &AnyRow) -> Result<BrowserLoginKind, HarnessError> {
    Ok(
        if row
            .try_get::<String, _>("login_kind")
            .map_err(database_error)?
            == "native"
        {
            BrowserLoginKind::Native
        } else {
            BrowserLoginKind::Oidc
        },
    )
}

fn optional_time(row: &AnyRow, field: &str) -> Result<Option<u64>, HarnessError> {
    row.try_get::<Option<i64>, _>(field)
        .map_err(database_error)?
        .map(unsigned)
        .transpose()
}

fn public_session_id(actor: &ControlUser, key: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ternilo-browser-session-id-v1\0");
    digest.update(actor.user_id.as_str().as_bytes());
    digest.update(b"\0");
    digest.update(key.as_bytes());
    format!("ter_s_{}", hex(&digest.finalize()))
}

fn timestamp(value: u64) -> Result<i64, HarnessError> {
    i64::try_from(value).map_err(|_| HarnessError::invalid("timestamp exceeds i64"))
}

fn unsigned(value: i64) -> Result<u64, HarnessError> {
    u64::try_from(value).map_err(|_| HarnessError::execution("stored timestamp is negative"))
}
