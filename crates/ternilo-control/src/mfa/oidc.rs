use super::{SCOPE, invalid_code, load, require_generation, required, timestamp, verify_code};
use crate::{
    ControlStore, ControlUser, EncryptedSecret,
    account_store::require_active_account_in,
    crypto::{hex, random_token, token_hash},
};
use crate::{OidcPrincipal, OidcSessionGrant, OidcSessionIdentity};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use ternilo_protocol::{HarnessError, UserId};
use ternilo_storage::{Transaction, database_error, lock};
use zeroize::Zeroizing;

#[derive(Serialize)]
pub struct MfaOidcChallenge {
    pub mfa_challenge: String,
    pub expires_at_ms: u64,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingIdentity {
    identity: OidcSessionIdentity,
    binding: String,
    access_expires_at_ms: u64,
}

impl ControlStore {
    pub async fn begin_oidc_mfa(
        &self,
        identity: &OidcSessionIdentity,
        binding: &str,
        access_expires_at_ms: u64,
        now: u64,
    ) -> Result<Option<MfaOidcChallenge>, HarnessError> {
        let mut tx = self.database.begin().await?;
        let Some(user) = principal_user(&mut tx, &identity.principal).await? else {
            return Ok(None);
        };
        lock(&mut tx, &format!("ternilo:account-role:{user}")).await?;
        require_active_account_in(&mut tx, &user).await?;
        let generation: Option<String> = sqlx::query_scalar(
            "SELECT generation FROM control_mfa_factors WHERE user_id=$1 AND status='enabled'",
        )
        .bind(user.as_str())
        .fetch_optional(&mut *tx)
        .await
        .map_err(database_error)?;
        let Some(generation) = generation else {
            return Ok(None);
        };
        let challenge = random_token("ter_mc");
        let digest = hex(&token_hash(&challenge));
        let expires_at_ms = access_expires_at_ms.min(now.saturating_add(5 * 60_000));
        if expires_at_ms <= now {
            return Err(expired());
        }
        let bytes = Zeroizing::new(
            serde_json::to_vec(&PendingIdentity {
                identity: identity.clone(),
                binding: binding.to_owned(),
                access_expires_at_ms,
            })
            .map_err(|_| HarnessError::execution("encode pending OIDC authentication"))?,
        );
        let secret = self
            .cipher
            .encrypt(SCOPE, Some(user.as_str()), &digest, 1, &bytes)?;
        sqlx::query("DELETE FROM control_mfa_oidc_challenges WHERE expires_at_ms<=$1")
            .bind(timestamp(now)?)
            .execute(&mut *tx)
            .await
            .map_err(database_error)?;
        // Bound unfinished sign-ins while permitting separate browser/device attempts.
        sqlx::query("DELETE FROM control_mfa_oidc_challenges WHERE user_id=$1 AND token_hash NOT IN (SELECT token_hash FROM control_mfa_oidc_challenges WHERE user_id=$1 ORDER BY expires_at_ms DESC,token_hash DESC LIMIT 7)")
            .bind(user.as_str()).execute(&mut *tx).await.map_err(database_error)?;
        sqlx::query("INSERT INTO control_mfa_oidc_challenges(token_hash,user_id,generation,expires_at_ms,nonce,ciphertext) VALUES($1,$2,$3,$4,$5,$6)")
            .bind(&digest).bind(user.as_str()).bind(generation).bind(timestamp(expires_at_ms)?).bind(secret.nonce.to_vec()).bind(secret.ciphertext)
            .execute(&mut *tx).await.map_err(database_error)?;
        tx.commit().await.map_err(database_error)?;
        Ok(Some(MfaOidcChallenge {
            mfa_challenge: challenge,
            expires_at_ms,
        }))
    }

    pub async fn complete_oidc_mfa(
        &self,
        challenge: &str,
        code: &str,
        binding: &str,
        now: u64,
    ) -> Result<OidcSessionGrant, HarnessError> {
        if !challenge.starts_with("ter_mc_") || challenge.len() != 50 {
            return Err(expired());
        }
        let digest = hex(&token_hash(challenge));
        let user: Option<String> = sqlx::query_scalar("SELECT user_id FROM control_mfa_oidc_challenges WHERE token_hash=$1 AND expires_at_ms>$2")
            .bind(&digest).bind(timestamp(now)?).fetch_optional(self.database.pool()).await.map_err(database_error)?;
        let user = UserId::new(user.ok_or_else(expired)?);
        let mut tx = self.database.begin().await?;
        lock(&mut tx, &format!("ternilo:account-role:{user}")).await?;
        require_active_account_in(&mut tx, &user).await?;
        let row = sqlx::query("SELECT generation,nonce,ciphertext FROM control_mfa_oidc_challenges WHERE token_hash=$1 AND user_id=$2 AND expires_at_ms>$3")
            .bind(&digest).bind(user.as_str()).bind(timestamp(now)?).fetch_optional(&mut *tx).await.map_err(database_error)?.ok_or_else(expired)?;
        let generation: String = row.try_get("generation").map_err(database_error)?;
        let nonce: Vec<u8> = row.try_get("nonce").map_err(database_error)?;
        let encrypted = EncryptedSecret {
            nonce: nonce.try_into().map_err(|_| expired())?,
            ciphertext: row.try_get("ciphertext").map_err(database_error)?,
        };
        let bytes = self
            .cipher
            .decrypt(SCOPE, Some(user.as_str()), &digest, 1, &encrypted)?;
        let pending: PendingIdentity = serde_json::from_slice(&bytes).map_err(|_| expired())?;
        if pending.binding != binding
            || principal_user(&mut tx, &pending.identity.principal)
                .await?
                .as_ref()
                != Some(&user)
        {
            return Err(expired());
        }
        let mut factor = load(self, &mut tx, &user)
            .await?
            .filter(|factor| factor.enabled && factor.generation == generation)
            .ok_or_else(expired)?;
        let valid = verify_code(self, &mut tx, &user, &mut factor, code, now, true).await?;
        if !valid {
            tx.commit().await.map_err(database_error)?;
            return Err(invalid_code());
        }
        sqlx::query("DELETE FROM control_mfa_oidc_challenges WHERE token_hash=$1")
            .bind(&digest)
            .execute(&mut *tx)
            .await
            .map_err(database_error)?;
        tx.commit().await.map_err(database_error)?;
        self.create_mfa_oidc_session(
            &pending.identity,
            binding,
            pending.access_expires_at_ms,
            &generation,
            now,
        )
        .await
    }

    pub async fn require_external_oidc_allowed(
        &self,
        user: &ControlUser,
    ) -> Result<(), HarnessError> {
        let mut tx = self.database.begin().await?;
        require_generation(&mut tx, &user.user_id, None).await?;
        tx.commit().await.map_err(database_error)
    }
}

pub(crate) async fn principal_user(
    tx: &mut Transaction,
    principal: &OidcPrincipal,
) -> Result<Option<UserId>, HarnessError> {
    let user: Option<String> =
        sqlx::query_scalar("SELECT user_id FROM control_users WHERE issuer=$1 AND subject=$2")
            .bind(&principal.issuer)
            .bind(&principal.subject)
            .fetch_optional(&mut **tx)
            .await
            .map_err(database_error)?;
    Ok(user.map(UserId::new))
}

pub(crate) async fn authorize_new_oidc(
    tx: &mut Transaction,
    principal: &OidcPrincipal,
    generation: Option<&str>,
) -> Result<Option<UserId>, HarnessError> {
    let user = principal_user(tx, principal).await?;
    if let Some(user) = &user {
        let current: Option<String> = sqlx::query_scalar(
            "SELECT generation FROM control_mfa_factors WHERE user_id=$1 AND status='enabled'",
        )
        .bind(user.as_str())
        .fetch_optional(&mut **tx)
        .await
        .map_err(database_error)?;
        if current.as_deref() != generation {
            return Err(required());
        }
    } else if generation.is_some() {
        return Err(expired());
    }
    Ok(user)
}

pub(crate) async fn authorize_oidc_session(
    tx: &mut Transaction,
    principal: &OidcPrincipal,
    session: &str,
) -> Result<(), HarnessError> {
    let denied: Option<i64> = sqlx::query_scalar("SELECT 1 FROM control_mfa_factors f JOIN control_users u ON u.user_id=f.user_id WHERE u.issuer=$1 AND u.subject=$2 AND f.status='enabled' AND NOT EXISTS (SELECT 1 FROM control_oidc_mfa_assurances a WHERE a.session_id=$3 AND a.user_id=f.user_id AND a.generation=f.generation)")
        .bind(&principal.issuer).bind(&principal.subject).bind(session).fetch_optional(&mut **tx).await.map_err(database_error)?;
    if denied.is_some() {
        return Err(required());
    }
    Ok(())
}
fn expired() -> HarnessError {
    HarnessError::policy("multi-factor sign-in expired or changed; sign in again")
}
