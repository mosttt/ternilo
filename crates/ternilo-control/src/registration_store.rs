use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{Row, any::AnyRow};
use ternilo_protocol::{HarnessError, UserId};
use ternilo_storage::{Database, Transaction, database_error, lock};

use crate::{
    AccountRecord, ControlStore, ControlUser, NativeRegistration, NativeSessionGrant,
    OidcPrincipal, PlatformAction,
    account_store::{account_in, append_platform_audit, authorize_platform_in},
    identity_store::{
        consume_invitation, create_native_user, finish_invitation, hash_password, issue_session,
        normalize_username, require_multi_user, required_instance, validate_registration,
    },
};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RegistrationMode {
    Open,
    #[default]
    Invite,
}

impl RegistrationMode {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Invite => "invite",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RegistrationSettings {
    pub mode: RegistrationMode,
    pub require_approval: bool,
    pub oidc_only: bool,
    pub revision: u64,
}

impl Default for RegistrationSettings {
    fn default() -> Self {
        Self {
            mode: RegistrationMode::Invite,
            require_approval: false,
            oidc_only: false,
            revision: 1,
        }
    }
}

pub(crate) async fn initialize(database: &Database) -> Result<(), HarnessError> {
    database
        .initialize(
            "registration_policy",
            1,
            "CREATE TABLE control_registration_policy (
            singleton BIGINT PRIMARY KEY CHECK (singleton = 1),
            oidc_only BIGINT NOT NULL CHECK (oidc_only IN (0, 1))
        );",
            "REVOKE ALL ON control_registration_policy FROM PUBLIC;
         DO $$ BEGIN IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'ternilo_runtime') THEN
         GRANT SELECT, INSERT, UPDATE, DELETE ON control_registration_policy TO ternilo_runtime;
         END IF; END $$;",
        )
        .await
}

pub(crate) fn require_password_registration(
    settings: &RegistrationSettings,
) -> Result<(), HarnessError> {
    if settings.oidc_only {
        return Err(HarnessError::policy(
            "new accounts must register using OAuth2/OIDC",
        ));
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountStatus {
    Active,
    Pending,
    Rejected,
    Banned,
    Removed,
}

impl AccountStatus {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Pending => "pending",
            Self::Rejected => "rejected",
            Self::Banned => "banned",
            Self::Removed => "removed",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self, HarnessError> {
        match value {
            "active" => Ok(Self::Active),
            "pending" => Ok(Self::Pending),
            "rejected" => Ok(Self::Rejected),
            "banned" => Ok(Self::Banned),
            "removed" => Ok(Self::Removed),
            _ => Err(HarnessError::execution("stored account status is invalid")),
        }
    }

    pub(crate) fn require_active(self) -> Result<(), HarnessError> {
        match self {
            Self::Active => Ok(()),
            Self::Pending => Err(HarnessError::policy(
                "account registration is pending approval",
            )),
            Self::Rejected => Err(HarnessError::policy("account registration was rejected")),
            Self::Banned => Err(HarnessError::policy("account is banned")),
            Self::Removed => Err(HarnessError::policy("account was removed")),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RegistrationDecision {
    Approve,
    Reject,
}

#[derive(Serialize)]
pub struct NativeRegistrationOutcome {
    pub status: AccountStatus,
    pub user_id: UserId,
    pub session: Option<NativeSessionGrant>,
}

#[derive(Debug, Serialize)]
pub struct OidcRegistrationOutcome {
    pub status: AccountStatus,
    pub user_id: UserId,
}

impl ControlStore {
    /// Public bootstrap metadata contains no account information or credentials.
    pub async fn registration_settings(&self) -> Result<RegistrationSettings, HarnessError> {
        sqlx::query("SELECT registration_mode, registration_require_approval, registration_revision, COALESCE((SELECT oidc_only FROM control_registration_policy WHERE singleton = 1), 0) AS oidc_only FROM control_instance_settings WHERE singleton = 1")
            .fetch_optional(self.database.pool()).await.map_err(database_error)?.as_ref().map(settings_from_row).transpose().map(Option::unwrap_or_default)
    }

    pub async fn set_registration_settings(
        &self,
        actor: &ControlUser,
        mode: RegistrationMode,
        require_approval: bool,
        oidc_only: bool,
        revision: u64,
        now_ms: u64,
    ) -> Result<RegistrationSettings, HarnessError> {
        if mode == RegistrationMode::Invite && require_approval {
            return Err(HarnessError::invalid(
                "administrator invitations do not require registration approval",
            ));
        }
        let mut transaction = self.database.begin().await?;
        lock(&mut transaction, "ternilo:instance").await?;
        authorize_platform_in(
            &mut transaction,
            &actor.user_id,
            PlatformAction::RegistrationManage,
        )
        .await?;
        let previous = registration_settings_in(&mut transaction).await?;
        if previous.revision != revision {
            return Err(HarnessError::conflict(
                "registration settings changed; reload before saving",
            ));
        }
        if previous.mode == mode
            && previous.require_approval == require_approval
            && previous.oidc_only == oidc_only
        {
            transaction.commit().await.map_err(database_error)?;
            return Ok(previous);
        }
        let revision = next_revision(revision)?;
        sqlx::query("UPDATE control_instance_settings SET registration_mode = $1, registration_require_approval = $2, registration_revision = $3, updated_at_ms = $4 WHERE singleton = 1")
            .bind(mode.as_str()).bind(i64::from(require_approval)).bind(timestamp(revision)?).bind(timestamp(now_ms)?)
            .execute(&mut *transaction).await.map_err(database_error)?;
        sqlx::query("INSERT INTO control_registration_policy (singleton, oidc_only) VALUES (1, $1) ON CONFLICT (singleton) DO UPDATE SET oidc_only = EXCLUDED.oidc_only")
            .bind(i64::from(oidc_only)).execute(&mut *transaction).await.map_err(database_error)?;
        let settings = RegistrationSettings {
            mode,
            require_approval,
            oidc_only,
            revision,
        };
        append_platform_audit(
            &mut transaction,
            &actor.user_id,
            "registration.settings",
            "default",
            json!({"previous": previous, "settings": settings}),
            now_ms,
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(settings)
    }

    pub async fn register_native(
        &self,
        registration: &NativeRegistration,
        now_ms: u64,
    ) -> Result<NativeRegistrationOutcome, HarnessError> {
        let username = normalize_username(&registration.username)?;
        validate_registration(registration)?;
        let instance = self
            .instance_settings()
            .await?
            .ok_or_else(|| HarnessError::policy("server initialization is required"))?;
        require_multi_user(&instance)?;
        let settings = self.registration_settings().await?;
        require_password_registration(&settings)?;
        public_registration_status(&settings)?;
        let password_hash = hash_password(&registration.password).await?;
        let mut transaction = self.database.begin().await?;
        lock(&mut transaction, "ternilo:instance").await?;
        let instance = required_instance(&mut transaction).await?;
        require_multi_user(&instance)?;
        require_password_registration(&registration_settings_in(&mut transaction).await?)?;
        let status = public_registration_status_in(&mut transaction).await?;
        let user = create_native_user(
            &mut transaction,
            &username,
            &registration.email,
            &password_hash,
            now_ms,
        )
        .await?;
        sqlx::query("UPDATE control_users SET status = $2 WHERE user_id = $1")
            .bind(user.user_id.as_str())
            .bind(status.as_str())
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
        append_registration_audit(&mut transaction, &user.user_id, status, "native", now_ms)
            .await?;
        let user_id = user.user_id.clone();
        let session = if status == AccountStatus::Active {
            Some(issue_session(&mut transaction, user, instance, None, now_ms).await?)
        } else {
            None
        };
        transaction.commit().await.map_err(database_error)?;
        Ok(NativeRegistrationOutcome {
            status,
            user_id,
            session,
        })
    }

    /// Existing external identities retain the platform username chosen at registration.
    pub async fn authenticate_oidc_user(
        &self,
        principal: &OidcPrincipal,
        now_ms: u64,
    ) -> Result<ControlUser, HarnessError> {
        validate_oidc_principal(principal)?;
        let mut transaction = self.database.begin().await?;
        if let Some(user) = Self::refresh_oidc_user_in(&mut transaction, principal, now_ms).await? {
            transaction.commit().await.map_err(database_error)?;
            return Ok(user);
        }
        require_multi_user(&required_instance(&mut transaction).await?)?;
        Err(HarnessError::policy(
            "choose a platform username to finish registration",
        ))
    }

    /// Admit a verified external identity using an explicitly chosen platform username.
    /// This endpoint never creates a native credential or browser session token.
    pub async fn register_oidc(
        &self,
        principal: &OidcPrincipal,
        username: &str,
        email: &str,
        invitation_token: Option<&str>,
        now_ms: u64,
    ) -> Result<OidcRegistrationOutcome, HarnessError> {
        validate_oidc_principal(principal)?;
        let username = normalize_username(username)?;
        let email = crate::identity_store::normalize_email(email)?;
        let mut transaction = self.database.begin().await?;
        lock(&mut transaction, "ternilo:instance").await?;
        lock(
            &mut transaction,
            &format!("ternilo:oidc:{}:{}", principal.issuer, principal.subject),
        )
        .await?;
        let instance = required_instance(&mut transaction).await?;
        if let Some(user) = Self::refresh_oidc_user_in(&mut transaction, principal, now_ms).await? {
            crate::identity_store::require_remote_access(&instance, &user)?;
            let status: String =
                sqlx::query_scalar("SELECT status FROM control_users WHERE user_id = $1")
                    .bind(user.user_id.as_str())
                    .fetch_one(&mut *transaction)
                    .await
                    .map_err(database_error)?;
            let status = AccountStatus::parse(&status)?;
            if matches!(
                status,
                AccountStatus::Rejected | AccountStatus::Banned | AccountStatus::Removed
            ) {
                status.require_active()?;
            }
            transaction.commit().await.map_err(database_error)?;
            return Ok(OidcRegistrationOutcome {
                status,
                user_id: user.user_id,
            });
        }
        require_multi_user(&instance)?;
        let settings = registration_settings_in(&mut transaction).await?;
        let invitation = if settings.mode == RegistrationMode::Invite {
            let invitation = consume_invitation(
                &mut transaction,
                invitation_token.ok_or_else(|| {
                    HarnessError::policy("registration requires an administrator invitation")
                })?,
                now_ms,
            )
            .await?;
            if invitation.tenant_id.is_some() {
                return Err(HarnessError::policy(
                    "team invitations require an existing active account; register or sign in first",
                ));
            }
            Some(invitation)
        } else {
            None
        };
        let status = if invitation.is_some() {
            AccountStatus::Active
        } else {
            public_registration_status(&settings)?
        };
        let mut principal = principal.clone();
        principal.email = Some(email);
        let user =
            Self::upsert_user_in(&mut transaction, &principal, &username, status, now_ms).await?;
        if let Some(invitation) = &invitation {
            finish_invitation(&mut transaction, &user, invitation, now_ms).await?;
        }
        append_registration_audit(&mut transaction, &user.user_id, status, "oidc", now_ms).await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(OidcRegistrationOutcome {
            status,
            user_id: user.user_id,
        })
    }

    pub async fn review_account_registration(
        &self,
        actor: &ControlUser,
        user_id: &UserId,
        decision: RegistrationDecision,
        status_revision: u64,
        now_ms: u64,
    ) -> Result<AccountRecord, HarnessError> {
        user_id.validate()?;
        let mut transaction = self.database.begin().await?;
        lock(&mut transaction, "ternilo:instance").await?;
        authorize_platform_in(
            &mut transaction,
            &actor.user_id,
            PlatformAction::AccountsReview,
        )
        .await?;
        lock(&mut transaction, &format!("ternilo:account-role:{user_id}")).await?;
        let account = account_in(&mut transaction, user_id).await?;
        if account.status_revision != status_revision {
            return Err(HarnessError::conflict(
                "account registration changed; reload before reviewing",
            ));
        }
        if account.status != AccountStatus::Pending
            && !(account.status == AccountStatus::Rejected
                && decision == RegistrationDecision::Approve)
        {
            return Err(HarnessError::conflict(
                "only pending registrations can be rejected; pending or rejected registrations can be approved",
            ));
        }
        let status = match decision {
            RegistrationDecision::Approve => AccountStatus::Active,
            RegistrationDecision::Reject => AccountStatus::Rejected,
        };
        let revision = next_revision(status_revision)?;
        sqlx::query(
            "UPDATE control_users SET status = $2, status_revision = $3 WHERE user_id = $1",
        )
        .bind(user_id.as_str())
        .bind(status.as_str())
        .bind(timestamp(revision)?)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        append_platform_audit(&mut transaction, &actor.user_id, "account.registration.review", user_id.as_str(), json!({"previous": account.status, "decision": decision, "status": status, "revision": revision}), now_ms).await?;
        let account = account_in(&mut transaction, user_id).await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(account)
    }
}

pub(crate) async fn registration_settings_in(
    transaction: &mut Transaction,
) -> Result<RegistrationSettings, HarnessError> {
    sqlx::query("SELECT registration_mode, registration_require_approval, registration_revision, COALESCE((SELECT oidc_only FROM control_registration_policy WHERE singleton = 1), 0) AS oidc_only FROM control_instance_settings WHERE singleton = 1")
        .fetch_optional(&mut **transaction).await.map_err(database_error)?.as_ref().map(settings_from_row).transpose().map(Option::unwrap_or_default)
}

pub(crate) async fn require_account_invitations_in(
    transaction: &mut Transaction,
) -> Result<(), HarnessError> {
    if registration_settings_in(transaction).await?.mode == RegistrationMode::Invite {
        Ok(())
    } else {
        Err(HarnessError::policy(
            "account invitations are disabled while open registration is enabled",
        ))
    }
}

async fn public_registration_status_in(
    transaction: &mut Transaction,
) -> Result<AccountStatus, HarnessError> {
    public_registration_status(&registration_settings_in(transaction).await?)
}

fn public_registration_status(
    settings: &RegistrationSettings,
) -> Result<AccountStatus, HarnessError> {
    if settings.mode != RegistrationMode::Open {
        return Err(HarnessError::policy(
            "registration requires an administrator invitation",
        ));
    }
    Ok(if settings.require_approval {
        AccountStatus::Pending
    } else {
        AccountStatus::Active
    })
}

fn validate_oidc_principal(principal: &OidcPrincipal) -> Result<(), HarnessError> {
    principal.validate()?;
    if principal.issuer == "ternilo:native" {
        return Err(HarnessError::invalid(
            "a native identity is not an OIDC account",
        ));
    }
    Ok(())
}

async fn append_registration_audit(
    transaction: &mut Transaction,
    user_id: &UserId,
    status: AccountStatus,
    method: &str,
    now_ms: u64,
) -> Result<(), HarnessError> {
    append_platform_audit(
        transaction,
        user_id,
        "account.registration",
        user_id.as_str(),
        json!({"status": status, "method": method}),
        now_ms,
    )
    .await
}

fn settings_from_row(row: &AnyRow) -> Result<RegistrationSettings, HarnessError> {
    Ok(RegistrationSettings {
        mode: match row
            .try_get::<String, _>("registration_mode")
            .map_err(database_error)?
            .as_str()
        {
            "open" => RegistrationMode::Open,
            "invite" => RegistrationMode::Invite,
            _ => {
                return Err(HarnessError::execution(
                    "stored registration mode is invalid",
                ));
            }
        },
        require_approval: row
            .try_get::<i64, _>("registration_require_approval")
            .map_err(database_error)?
            != 0,
        oidc_only: row.try_get::<i64, _>("oidc_only").map_err(database_error)? != 0,
        revision: u64::try_from(
            row.try_get::<i64, _>("registration_revision")
                .map_err(database_error)?,
        )
        .map_err(|_| HarnessError::execution("stored registration revision is negative"))?,
    })
}

fn next_revision(value: u64) -> Result<u64, HarnessError> {
    value
        .checked_add(1)
        .ok_or_else(|| HarnessError::execution("registration revision overflow"))
}

fn timestamp(value: u64) -> Result<i64, HarnessError> {
    i64::try_from(value)
        .map_err(|_| HarnessError::invalid("registration value exceeds database range"))
}

#[cfg(test)]
pub(crate) mod tests;
