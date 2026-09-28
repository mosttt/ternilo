use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier, password_hash::SaltString};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{Row, any::AnyRow};
use ternilo_protocol::{HarnessError, TenantId, UserId};
use ternilo_storage::{Transaction, database_error, lock, set_tenant_scope};
use zeroize::Zeroizing;

use crate::{
    ControlStore, ControlUser, OidcPrincipal, PlatformAction, PlatformRole, SpaceKind, TenantRole,
    TenantSummary,
    account_store::{
        append_platform_audit, authorize_platform_in, personal_space_in, platform_role_in,
        require_team_in,
    },
    crypto::{hex, random_identifier, random_token, token_hash},
    registration_store::require_account_invitations_in,
    store::append_audit,
};

const SESSION_TTL_MS: u64 = 7 * 24 * 60 * 60 * 1_000;
const MAX_INVITATION_TTL_SECONDS: u64 = 7 * 24 * 60 * 60;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstanceMode {
    #[default]
    SingleUser,
    MultiUser,
}

impl InstanceMode {
    const fn as_str(self) -> &'static str {
        match self {
            Self::SingleUser => "single_user",
            Self::MultiUser => "multi_user",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct InstanceSettings {
    pub mode: InstanceMode,
    pub owner_user_id: UserId,
    pub revision: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeRegistration {
    pub email: String,
    pub username: String,
    pub password: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct IdentitySession {
    pub email: Option<String>,
    pub user: ControlUser,
    pub instance: InstanceSettings,
    pub is_instance_owner: bool,
    pub platform_role: PlatformRole,
    pub personal_tenant_id: TenantId,
    pub personal_project_id: String,
    pub expires_at_ms: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct OidcAccountLink {
    pub issuer: String,
    pub subject: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AccountLoginMethods {
    pub native: bool,
    pub oidc: Option<OidcAccountLink>,
}

#[derive(Serialize)]
pub struct NativeSessionGrant {
    pub access_token: String,
    #[serde(flatten)]
    pub session: IdentitySession,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserInvitationRequest {
    #[serde(default)]
    pub tenant_id: Option<TenantId>,
    pub role: TenantRole,
    #[serde(default = "default_invitation_ttl")]
    pub expires_in_seconds: u64,
}

#[derive(Serialize)]
pub struct UserInvitationGrant {
    pub invitation_id: String,
    pub token: String,
    pub expires_at_ms: u64,
    pub tenant_id: Option<TenantId>,
    pub role: TenantRole,
}

const fn default_invitation_ttl() -> u64 {
    24 * 60 * 60
}

impl ControlStore {
    pub async fn instance_settings(&self) -> Result<Option<InstanceSettings>, HarnessError> {
        sqlx::query("SELECT mode, owner_user_id, revision FROM control_instance_settings WHERE singleton = 1")
            .fetch_optional(self.database.pool()).await.map_err(database_error)?
            .as_ref().map(instance_from_row).transpose()
    }

    /// Bootstrap the owner and default resources atomically, exactly once.
    pub async fn initialize_owner(
        &self,
        registration: &NativeRegistration,
        now_ms: u64,
    ) -> Result<NativeSessionGrant, HarnessError> {
        let username = normalize_username(&registration.username)?;
        validate_registration(registration)?;
        let password_hash = hash_password(&registration.password).await?;
        let mut transaction = self.database.begin().await?;
        lock(&mut transaction, "ternilo:instance").await?;
        if read_instance(&mut transaction).await?.is_some() {
            return Err(HarnessError::conflict("server is already initialized"));
        }
        let user = create_native_user(
            &mut transaction,
            &username,
            &registration.email,
            &password_hash,
            now_ms,
        )
        .await?;
        let instance = InstanceSettings {
            mode: InstanceMode::SingleUser,
            owner_user_id: user.user_id.clone(),
            revision: 1,
        };
        sqlx::query("INSERT INTO control_instance_settings (singleton, mode, owner_user_id, revision, created_at_ms, updated_at_ms) VALUES (1, 'single_user', $1, 1, $2, $2)")
            .bind(user.user_id.as_str()).bind(timestamp(now_ms)?)
            .execute(&mut *transaction).await.map_err(database_error)?;
        let grant = issue_session(&mut transaction, user, instance, now_ms).await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(grant)
    }

    pub async fn login_native(
        &self,
        username: &str,
        password: &str,
        now_ms: u64,
    ) -> Result<NativeSessionGrant, HarnessError> {
        let user = self
            .authenticate_native_credentials(username, password)
            .await?;
        self.create_browser_session(user, now_ms).await
    }

    /// Verify credentials separately from the instance access policy.
    pub async fn authenticate_native_credentials(
        &self,
        username: &str,
        password: &str,
    ) -> Result<ControlUser, HarnessError> {
        let username = normalize_username(username).map_err(|_| invalid_login())?;
        if password.len() > 1_024 {
            return Err(invalid_login());
        }
        let row = sqlx::query("SELECT account.password_hash, user_row.user_id, user_row.username FROM control_native_accounts AS account JOIN control_users AS user_row ON user_row.user_id = account.user_id WHERE user_row.username = $1")
            .bind(&username).fetch_optional(self.database.pool()).await.map_err(database_error)?
            .ok_or_else(invalid_login)?;
        let encoded: String = row.try_get("password_hash").map_err(database_error)?;
        verify_password(password, encoded).await?;
        user_from_row(&row)
    }

    /// Issue a session only after the caller has verified the identity.
    pub async fn create_browser_session(
        &self,
        user: ControlUser,
        now_ms: u64,
    ) -> Result<NativeSessionGrant, HarnessError> {
        let mut transaction = self.database.begin().await?;
        let instance = required_instance(&mut transaction).await?;
        require_remote_access(&instance, &user)?;
        let grant = issue_session(&mut transaction, user, instance, now_ms).await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(grant)
    }

    pub async fn authenticate_native_session(
        &self,
        token: &str,
        now_ms: u64,
    ) -> Result<IdentitySession, HarnessError> {
        let (user, expires_at_ms) = self.authenticate_native_token(token, now_ms).await?;
        let mut session = self.identity_session(user).await?;
        session.expires_at_ms = Some(expires_at_ms);
        Ok(session)
    }

    /// Authenticate first so HTTP can distinguish invalid credentials from mode denial.
    pub async fn authenticate_native_token(
        &self,
        token: &str,
        now_ms: u64,
    ) -> Result<(ControlUser, u64), HarnessError> {
        let row = sqlx::query("SELECT session.expires_at_ms, user_row.user_id, user_row.username, user_row.status FROM control_browser_sessions AS session JOIN control_users AS user_row ON user_row.user_id = session.user_id WHERE session.token_hash = $1 AND session.revoked_at_ms IS NULL AND session.expires_at_ms > $2")
            .bind(hex(&token_hash(token))).bind(timestamp(now_ms)?)
            .fetch_optional(self.database.pool()).await.map_err(database_error)?
            .ok_or_else(|| HarnessError::policy("browser session is invalid or expired"))?;
        crate::AccountStatus::parse(&row.try_get::<String, _>("status").map_err(database_error)?)?
            .require_active()?;
        Ok((
            user_from_row(&row)?,
            unsigned(row.try_get("expires_at_ms").map_err(database_error)?)?,
        ))
    }

    /// Apply the same instance policy to native sessions and verified OIDC users.
    pub async fn identity_session(
        &self,
        user: ControlUser,
    ) -> Result<IdentitySession, HarnessError> {
        let mut transaction = self.database.begin().await?;
        let instance = required_instance(&mut transaction).await?;
        let session = identity_session_in(&mut transaction, user, instance, None).await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(session)
    }

    pub async fn account_login_methods(
        &self,
        actor: &ControlUser,
    ) -> Result<AccountLoginMethods, HarnessError> {
        let mut transaction = self.database.begin().await?;
        let methods = login_methods_in(&mut transaction, &actor.user_id).await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(methods)
    }

    /// The caller must authenticate the current native session and verify the
    /// configured OIDC token without creating or merging an OIDC user first.
    pub async fn link_native_oidc(
        &self,
        actor: &ControlUser,
        principal: &OidcPrincipal,
        now_ms: u64,
    ) -> Result<AccountLoginMethods, HarnessError> {
        principal.validate()?;
        if principal.issuer == "ternilo:native" {
            return Err(HarnessError::invalid(
                "a native identity is not an OIDC account",
            ));
        }
        let mut transaction = self.database.begin().await?;
        lock(
            &mut transaction,
            &format!("ternilo:oidc:{}:{}", principal.issuer, principal.subject),
        )
        .await?;
        lock(
            &mut transaction,
            &format!("ternilo:identity:{}", actor.user_id),
        )
        .await?;
        let instance = required_instance(&mut transaction).await?;
        require_remote_access(&instance, actor)?;
        platform_role_in(&mut transaction, &actor.user_id).await?;
        let methods = login_methods_in(&mut transaction, &actor.user_id).await?;
        if !methods.native {
            return Err(HarnessError::policy(
                "OIDC binding requires an existing native account",
            ));
        }
        let linked = OidcAccountLink {
            issuer: principal.issuer.clone(),
            subject: principal.subject.clone(),
        };
        if let Some(existing) = &methods.oidc {
            if existing != &linked {
                return Err(HarnessError::conflict(
                    "this account already has a different OIDC identity",
                ));
            }
            transaction.commit().await.map_err(database_error)?;
            return Ok(methods);
        }
        let existing: Option<String> =
            sqlx::query_scalar("SELECT user_id FROM control_users WHERE issuer=$1 AND subject=$2")
                .bind(&principal.issuer)
                .bind(&principal.subject)
                .fetch_optional(&mut *transaction)
                .await
                .map_err(database_error)?;
        if existing.is_some_and(|user_id| user_id != actor.user_id.as_str()) {
            return Err(oidc_link_conflict());
        }
        sqlx::query(
            "UPDATE control_users SET issuer=$2, subject=$3, last_seen_at_ms=$4 WHERE user_id=$1",
        )
        .bind(actor.user_id.as_str())
        .bind(&principal.issuer)
        .bind(&principal.subject)
        .bind(timestamp(now_ms)?)
        .execute(&mut *transaction)
        .await
        .map_err(|error| {
            if error
                .as_database_error()
                .is_some_and(sqlx::error::DatabaseError::is_unique_violation)
            {
                oidc_link_conflict()
            } else {
                database_error(error)
            }
        })?;
        append_platform_audit(
            &mut transaction,
            &actor.user_id,
            "identity.oidc_link",
            actor.user_id.as_str(),
            json!({ "issuer": principal.issuer, "subject": principal.subject }),
            now_ms,
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(AccountLoginMethods {
            native: true,
            oidc: Some(linked),
        })
    }

    pub async fn logout_native_session(
        &self,
        token: &str,
        now_ms: u64,
    ) -> Result<(), HarnessError> {
        sqlx::query("UPDATE control_browser_sessions SET revoked_at_ms = $2 WHERE token_hash = $1 AND revoked_at_ms IS NULL")
            .bind(hex(&token_hash(token))).bind(timestamp(now_ms)?)
            .execute(self.database.pool()).await.map_err(database_error)?;
        Ok(())
    }

    pub async fn set_instance_mode(
        &self,
        actor: &ControlUser,
        mode: InstanceMode,
        revision: u64,
        now_ms: u64,
    ) -> Result<InstanceSettings, HarnessError> {
        let mut transaction = self.database.begin().await?;
        lock(&mut transaction, "ternilo:instance").await?;
        let mut instance = required_instance(&mut transaction).await?;
        require_owner(&instance, actor)?;
        if instance.revision != revision {
            return Err(HarnessError::conflict(
                "instance settings changed; reload before saving",
            ));
        }
        if instance.mode == mode {
            return Ok(instance);
        }
        let next_revision = revision
            .checked_add(1)
            .ok_or_else(|| HarnessError::execution("instance revision overflow"))?;
        sqlx::query("UPDATE control_instance_settings SET mode = $1, revision = $2, updated_at_ms = $3 WHERE singleton = 1")
            .bind(mode.as_str()).bind(timestamp(next_revision)?).bind(timestamp(now_ms)?)
            .execute(&mut *transaction).await.map_err(database_error)?;
        append_platform_audit(
            &mut transaction,
            &actor.user_id,
            "instance.mode",
            "default",
            json!({"previous": instance.mode, "mode": mode, "revision": next_revision}),
            now_ms,
        )
        .await?;
        instance.mode = mode;
        instance.revision = next_revision;
        transaction.commit().await.map_err(database_error)?;
        Ok(instance)
    }

    pub async fn create_user_invitation(
        &self,
        actor: &ControlUser,
        request: &UserInvitationRequest,
        now_ms: u64,
    ) -> Result<UserInvitationGrant, HarnessError> {
        if request.role == TenantRole::Owner
            || request.expires_in_seconds == 0
            || request.expires_in_seconds > MAX_INVITATION_TTL_SECONDS
        {
            return Err(HarnessError::invalid(
                "invitations require a non-owner role and a lifetime of 1 to 604800 seconds",
            ));
        }
        let mut transaction = self.database.begin().await?;
        lock(&mut transaction, "ternilo:instance").await?;
        let instance = required_instance(&mut transaction).await?;
        require_multi_user(&instance)?;
        platform_role_in(&mut transaction, &actor.user_id).await?;
        if let Some(tenant_id) = &request.tenant_id {
            tenant_id.validate()?;
            require_team_in(&mut transaction, tenant_id).await?;
            crate::store::require_action(
                &mut transaction,
                tenant_id,
                &actor.user_id,
                crate::ControlAction::MembershipManage,
            )
            .await?;
        } else {
            require_account_invitations_in(&mut transaction).await?;
            authorize_platform_in(
                &mut transaction,
                &actor.user_id,
                PlatformAction::AccountsInvite,
            )
            .await?;
        }
        let expires_at_ms = now_ms
            .checked_add(request.expires_in_seconds * 1_000)
            .ok_or_else(|| HarnessError::invalid("invitation expiry overflow"))?;
        let grant = UserInvitationGrant {
            invitation_id: random_identifier("inv"),
            token: random_token("kni"),
            expires_at_ms,
            tenant_id: request.tenant_id.clone(),
            role: request.role,
        };
        sqlx::query("INSERT INTO control_user_invitations (invitation_id, token_hash, tenant_id, role, created_by, created_at_ms, expires_at_ms) VALUES ($1, $2, $3, $4, $5, $6, $7)")
            .bind(&grant.invitation_id).bind(hex(&token_hash(&grant.token))).bind(grant.tenant_id.as_ref().map(TenantId::as_str))
            .bind(grant.role.as_str()).bind(actor.user_id.as_str()).bind(timestamp(now_ms)?).bind(timestamp(grant.expires_at_ms)?)
            .execute(&mut *transaction).await.map_err(database_error)?;
        invitation_audit(
            &mut transaction,
            &actor.user_id,
            &grant.invitation_id,
            grant.tenant_id.as_ref(),
            "user.invite",
            json!({"role": grant.role, "expires_at_ms": expires_at_ms}),
            now_ms,
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(grant)
    }

    pub async fn accept_user_invitation(
        &self,
        token: &str,
        registration: &NativeRegistration,
        now_ms: u64,
    ) -> Result<NativeSessionGrant, HarnessError> {
        let username = normalize_username(&registration.username)?;
        validate_registration(registration)?;
        let password_hash = hash_password(&registration.password).await?;
        let mut transaction = self.database.begin().await?;
        lock(&mut transaction, "ternilo:instance").await?;
        let instance = required_instance(&mut transaction).await?;
        require_multi_user(&instance)?;
        require_account_invitations_in(&mut transaction).await?;
        let invitation = consume_invitation(&mut transaction, token, now_ms).await?;
        if invitation.tenant_id.is_some() {
            return Err(HarnessError::policy(
                "team invitations require an existing active account; register or sign in first",
            ));
        }
        let user = create_native_user(
            &mut transaction,
            &username,
            &registration.email,
            &password_hash,
            now_ms,
        )
        .await?;
        finish_invitation(&mut transaction, &user, &invitation, now_ms).await?;
        let grant = issue_session(&mut transaction, user, instance, now_ms).await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(grant)
    }

    pub async fn join_invitation(
        &self,
        actor: &ControlUser,
        token: &str,
        now_ms: u64,
    ) -> Result<TenantSummary, HarnessError> {
        let mut transaction = self.database.begin().await?;
        lock(&mut transaction, "ternilo:instance").await?;
        let instance = required_instance(&mut transaction).await?;
        require_multi_user(&instance)?;
        platform_role_in(&mut transaction, &actor.user_id).await?;
        let invitation = consume_invitation(&mut transaction, token, now_ms).await?;
        let tenant_id = invitation.tenant_id.as_ref().ok_or_else(|| {
            HarnessError::invalid("this invitation creates an account; it does not join a team")
        })?;
        finish_invitation(&mut transaction, actor, &invitation, now_ms).await?;
        let row = sqlx::query(
            "SELECT tenant.slug, tenant.display_name, membership.role FROM control_tenants tenant
            JOIN control_memberships membership ON membership.tenant_id = tenant.tenant_id
            WHERE tenant.tenant_id = $1 AND membership.user_id = $2",
        )
        .bind(tenant_id.as_str())
        .bind(actor.user_id.as_str())
        .fetch_one(&mut *transaction)
        .await
        .map_err(database_error)?;
        let tenant = TenantSummary {
            tenant_id: tenant_id.clone(),
            kind: SpaceKind::Team,
            slug: row.try_get("slug").map_err(database_error)?,
            display_name: row.try_get("display_name").map_err(database_error)?,
            role: TenantRole::parse(&row.try_get::<String, _>("role").map_err(database_error)?)?,
        };
        transaction.commit().await.map_err(database_error)?;
        Ok(tenant)
    }
}

async fn identity_session_in(
    transaction: &mut Transaction,
    user: ControlUser,
    instance: InstanceSettings,
    expires_at_ms: Option<u64>,
) -> Result<IdentitySession, HarnessError> {
    require_remote_access(&instance, &user)?;
    let platform_role = platform_role_in(transaction, &user.user_id).await?;
    let (personal_tenant_id, personal_project_id) =
        personal_space_in(transaction, &user.user_id).await?;
    let email = sqlx::query_scalar("SELECT email FROM control_users WHERE user_id = $1")
        .bind(user.user_id.as_str())
        .fetch_one(&mut **transaction)
        .await
        .map_err(database_error)?;
    Ok(IdentitySession {
        email,
        is_instance_owner: platform_role == PlatformRole::Owner,
        platform_role,
        personal_tenant_id,
        personal_project_id,
        user,
        instance,
        expires_at_ms,
    })
}

struct ConsumedInvitation {
    invitation_id: String,
    tenant_id: Option<TenantId>,
    role: TenantRole,
}

async fn consume_invitation(
    transaction: &mut Transaction,
    token: &str,
    now_ms: u64,
) -> Result<ConsumedInvitation, HarnessError> {
    let row = sqlx::query(
        "UPDATE control_user_invitations SET consumed_at_ms = $2 WHERE token_hash = $1
        AND consumed_at_ms IS NULL AND expires_at_ms > $2 RETURNING invitation_id, tenant_id, role, created_by",
    )
    .bind(hex(&token_hash(token)))
    .bind(timestamp(now_ms)?)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(database_error)?
    .ok_or_else(|| HarnessError::policy("invitation is invalid, expired, or already used"))?;
    let invitation = ConsumedInvitation {
        invitation_id: row.try_get("invitation_id").map_err(database_error)?,
        tenant_id: row
            .try_get::<Option<String>, _>("tenant_id")
            .map_err(database_error)?
            .map(TenantId::new),
        role: TenantRole::parse(&row.try_get::<String, _>("role").map_err(database_error)?)?,
    };
    let creator = UserId::new(
        row.try_get::<String, _>("created_by")
            .map_err(database_error)?,
    );
    lock(transaction, &format!("ternilo:account-role:{creator}")).await?;
    platform_role_in(transaction, &creator).await?;
    // An unused invitation cannot outlive its creator's authority to grant access.
    if let Some(tenant_id) = &invitation.tenant_id {
        require_team_in(transaction, tenant_id).await?;
        crate::store::require_action(
            transaction,
            tenant_id,
            &creator,
            crate::ControlAction::MembershipManage,
        )
        .await?;
    } else {
        authorize_platform_in(transaction, &creator, PlatformAction::AccountsInvite).await?;
    }
    Ok(invitation)
}

async fn finish_invitation(
    transaction: &mut Transaction,
    user: &ControlUser,
    invitation: &ConsumedInvitation,
    now_ms: u64,
) -> Result<(), HarnessError> {
    if let Some(tenant_id) = &invitation.tenant_id {
        require_team_in(transaction, tenant_id).await?;
        sqlx::query("INSERT INTO control_memberships (tenant_id, user_id, role, created_at_ms) VALUES ($1, $2, $3, $4)
            ON CONFLICT (tenant_id, user_id) DO NOTHING")
            .bind(tenant_id.as_str()).bind(user.user_id.as_str()).bind(invitation.role.as_str()).bind(timestamp(now_ms)?)
            .execute(&mut **transaction).await.map_err(database_error)?;
    }
    sqlx::query("UPDATE control_user_invitations SET consumed_by = $2 WHERE invitation_id = $1")
        .bind(&invitation.invitation_id)
        .bind(user.user_id.as_str())
        .execute(&mut **transaction)
        .await
        .map_err(database_error)?;
    invitation_audit(
        transaction,
        &user.user_id,
        &invitation.invitation_id,
        invitation.tenant_id.as_ref(),
        "user.invitation.accept",
        json!({"role": invitation.role}),
        now_ms,
    )
    .await
}

async fn invitation_audit(
    transaction: &mut Transaction,
    actor: &UserId,
    invitation_id: &str,
    tenant_id: Option<&TenantId>,
    action: &str,
    metadata: serde_json::Value,
    now_ms: u64,
) -> Result<(), HarnessError> {
    if let Some(tenant_id) = tenant_id {
        set_tenant_scope(transaction, tenant_id).await?;
        append_audit(
            transaction,
            tenant_id,
            Some(actor),
            "user",
            action,
            "invitation",
            invitation_id,
            "success",
            metadata,
            now_ms,
        )
        .await?;
    } else {
        append_platform_audit(transaction, actor, action, invitation_id, metadata, now_ms).await?;
    }
    Ok(())
}

pub(crate) fn normalize_username(value: &str) -> Result<String, HarnessError> {
    let username = value.trim().to_ascii_lowercase();
    if !(3..=64).contains(&username.len())
        || !username
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(HarnessError::invalid(
            "username must contain 3 to 64 ASCII letters, digits, dots, hyphens, or underscores",
        ));
    }
    Ok(username)
}

/// Contact addresses are not proof of mailbox ownership or an identity-linking credential.
pub(crate) fn normalize_email(value: &str) -> Result<String, HarnessError> {
    let email = value.trim().to_ascii_lowercase();
    let invalid =
        || HarnessError::invalid("email must be a valid address such as name@example.com");
    if email.len() > 254 || !email.is_ascii() {
        return Err(invalid());
    }
    let (local, domain) = email.split_once('@').ok_or_else(invalid)?;
    if local.is_empty()
        || local.len() > 64
        || local.starts_with('.')
        || local.ends_with('.')
        || local.contains("..")
        || !local
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b".!#$%&'*+-/=?^_`{|}~".contains(&byte))
        || !domain.contains('.')
        || domain.split('.').any(|label| {
            label.is_empty()
                || label.len() > 63
                || label.starts_with('-')
                || label.ends_with('-')
                || !label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
    {
        return Err(invalid());
    }
    Ok(email)
}

pub(crate) async fn require_available_username_in(
    transaction: &mut Transaction,
    username: &str,
) -> Result<(), HarnessError> {
    let exists: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM control_users WHERE username = $1")
        .bind(username)
        .fetch_one(&mut **transaction)
        .await
        .map_err(database_error)?;
    if exists != 0 {
        return Err(HarnessError::conflict("username is already registered"));
    }
    Ok(())
}

pub(crate) async fn reserve_email_in(
    transaction: &mut Transaction,
    email: &str,
) -> Result<(), HarnessError> {
    lock(transaction, &format!("ternilo:contact-email:{email}")).await?;
    let exists: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM control_users WHERE LOWER(email) = $1 AND status <> 'removed'",
    )
    .bind(email)
    .fetch_one(&mut **transaction)
    .await
    .map_err(database_error)?;
    if exists != 0 {
        return Err(HarnessError::conflict("email is already registered"));
    }
    Ok(())
}

pub(crate) fn validate_registration(registration: &NativeRegistration) -> Result<(), HarnessError> {
    normalize_email(&registration.email)?;
    if !(8..=1_024).contains(&registration.password.len()) {
        return Err(HarnessError::invalid(
            "password must contain 8 to 1024 bytes",
        ));
    }
    Ok(())
}

pub(crate) async fn hash_password(password: &str) -> Result<String, HarnessError> {
    let password = Zeroizing::new(password.to_owned());
    tokio::task::spawn_blocking(move || {
        let salt = SaltString::encode_b64(&rand::random::<[u8; 16]>())
            .map_err(|_| HarnessError::execution("encode password salt"))?;
        Argon2::default()
            .hash_password(password.as_bytes(), &salt)
            .map(|hash| hash.to_string())
            .map_err(|_| HarnessError::execution("hash native account password"))
    })
    .await
    .map_err(|error| HarnessError::execution(format!("password hashing task: {error}")))?
}

async fn verify_password(password: &str, encoded: String) -> Result<(), HarnessError> {
    let password = Zeroizing::new(password.to_owned());
    tokio::task::spawn_blocking(move || {
        let hash = PasswordHash::new(&encoded)
            .map_err(|_| HarnessError::execution("stored password hash is invalid"))?;
        Argon2::default()
            .verify_password(password.as_bytes(), &hash)
            .map_err(|_| invalid_login())
    })
    .await
    .map_err(|error| HarnessError::execution(format!("password verification task: {error}")))?
}

pub(crate) async fn create_native_user(
    transaction: &mut Transaction,
    username: &str,
    email: &str,
    password_hash: &str,
    now_ms: u64,
) -> Result<ControlUser, HarnessError> {
    let user = ControlUser {
        user_id: UserId::new(random_identifier("usr")),
        username: username.to_owned(),
    };
    require_available_username_in(transaction, username).await?;
    let email = normalize_email(email)?;
    reserve_email_in(transaction, &email).await?;
    let created = sqlx::query("INSERT INTO control_users (user_id, issuer, subject, username, created_at_ms, last_seen_at_ms, email) VALUES ($1, 'ternilo:native', $2, $3, $4, $4, $5) ON CONFLICT (username) DO NOTHING")
        .bind(user.user_id.as_str()).bind(random_identifier("account")).bind(username).bind(timestamp(now_ms)?).bind(email)
        .execute(&mut **transaction).await.map_err(database_error)?;
    if created.rows_affected() != 1 {
        return Err(HarnessError::conflict("username is already registered"));
    }
    sqlx::query("INSERT INTO control_native_accounts (user_id, password_hash, created_at_ms) VALUES ($1, $2, $3)")
        .bind(user.user_id.as_str()).bind(password_hash).bind(timestamp(now_ms)?)
        .execute(&mut **transaction).await.map_err(database_error)?;
    ControlStore::create_personal_space_in(transaction, &user, now_ms).await?;
    Ok(user)
}

pub(crate) async fn issue_session(
    transaction: &mut Transaction,
    user: ControlUser,
    instance: InstanceSettings,
    now_ms: u64,
) -> Result<NativeSessionGrant, HarnessError> {
    lock(
        transaction,
        &format!("ternilo:account-role:{}", user.user_id),
    )
    .await?;
    crate::account_store::require_active_account_in(transaction, &user.user_id).await?;
    let access_token = random_token("kns");
    let expires_at_ms = now_ms
        .checked_add(SESSION_TTL_MS)
        .ok_or_else(|| HarnessError::invalid("session expiry overflow"))?;
    sqlx::query("INSERT INTO control_browser_sessions (token_hash, user_id, created_at_ms, expires_at_ms) VALUES ($1, $2, $3, $4)")
        .bind(hex(&token_hash(&access_token))).bind(user.user_id.as_str()).bind(timestamp(now_ms)?).bind(timestamp(expires_at_ms)?)
        .execute(&mut **transaction).await.map_err(database_error)?;
    Ok(NativeSessionGrant {
        access_token,
        session: identity_session_in(transaction, user, instance, Some(expires_at_ms)).await?,
    })
}

async fn read_instance(
    transaction: &mut Transaction,
) -> Result<Option<InstanceSettings>, HarnessError> {
    sqlx::query(
        "SELECT mode, owner_user_id, revision FROM control_instance_settings WHERE singleton = 1",
    )
    .fetch_optional(&mut **transaction)
    .await
    .map_err(database_error)?
    .as_ref()
    .map(instance_from_row)
    .transpose()
}

pub(crate) async fn required_instance(
    transaction: &mut Transaction,
) -> Result<InstanceSettings, HarnessError> {
    read_instance(transaction)
        .await?
        .ok_or_else(|| HarnessError::policy("server initialization is required"))
}

fn instance_from_row(row: &AnyRow) -> Result<InstanceSettings, HarnessError> {
    let mode: String = row.try_get("mode").map_err(database_error)?;
    Ok(InstanceSettings {
        mode: match mode.as_str() {
            "single_user" => InstanceMode::SingleUser,
            "multi_user" => InstanceMode::MultiUser,
            _ => return Err(HarnessError::execution("stored instance mode is invalid")),
        },
        owner_user_id: UserId::new(
            row.try_get::<String, _>("owner_user_id")
                .map_err(database_error)?,
        ),
        revision: unsigned(row.try_get("revision").map_err(database_error)?)?,
    })
}

fn user_from_row(row: &AnyRow) -> Result<ControlUser, HarnessError> {
    Ok(ControlUser {
        user_id: UserId::new(
            row.try_get::<String, _>("user_id")
                .map_err(database_error)?,
        ),
        username: row.try_get("username").map_err(database_error)?,
    })
}

fn require_owner(instance: &InstanceSettings, actor: &ControlUser) -> Result<(), HarnessError> {
    if instance.owner_user_id == actor.user_id {
        Ok(())
    } else {
        Err(HarnessError::policy(
            "only the instance owner can change server administration",
        ))
    }
}

pub(crate) fn require_multi_user(instance: &InstanceSettings) -> Result<(), HarnessError> {
    if instance.mode == InstanceMode::MultiUser {
        Ok(())
    } else {
        Err(HarnessError::policy(
            "multi-user mode is required for registration and invitations",
        ))
    }
}

pub(crate) fn require_remote_access(
    instance: &InstanceSettings,
    user: &ControlUser,
) -> Result<(), HarnessError> {
    if instance.mode == InstanceMode::MultiUser || instance.owner_user_id == user.user_id {
        Ok(())
    } else {
        Err(HarnessError::policy(
            "this account is paused while the server is in single-user mode",
        ))
    }
}

async fn login_methods_in(
    transaction: &mut Transaction,
    user_id: &UserId,
) -> Result<AccountLoginMethods, HarnessError> {
    user_id.validate()?;
    let row = sqlx::query(
        "SELECT user_row.issuer, user_row.subject,
                CAST(EXISTS(SELECT 1 FROM control_native_accounts account WHERE account.user_id=user_row.user_id) AS INTEGER) AS native
         FROM control_users user_row WHERE user_row.user_id=$1",
    ).bind(user_id.as_str()).fetch_optional(&mut **transaction).await.map_err(database_error)?
        .ok_or_else(|| HarnessError::policy("account does not exist"))?;
    let issuer: String = row.try_get("issuer").map_err(database_error)?;
    Ok(AccountLoginMethods {
        native: row.try_get::<i64, _>("native").map_err(database_error)? != 0,
        oidc: if issuer == "ternilo:native" {
            None
        } else {
            Some(OidcAccountLink {
                issuer,
                subject: row.try_get("subject").map_err(database_error)?,
            })
        },
    })
}

fn oidc_link_conflict() -> HarnessError {
    HarnessError::conflict("this OIDC identity already belongs to another account")
}

fn invalid_login() -> HarnessError {
    HarnessError::policy("invalid username or password")
}

fn timestamp(value: u64) -> Result<i64, HarnessError> {
    i64::try_from(value).map_err(|_| HarnessError::invalid("timestamp exceeds database range"))
}

fn unsigned(value: i64) -> Result<u64, HarnessError> {
    u64::try_from(value).map_err(|_| HarnessError::execution("stored identity value is negative"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SecretCipher;

    fn registration(username: &str) -> NativeRegistration {
        NativeRegistration {
            email: format!("{}@example.test", username.trim().to_ascii_lowercase()),
            username: username.to_owned(),
            password: "test-password-123".to_owned(),
        }
    }

    async fn store() -> ControlStore {
        ControlStore::connect("sqlite::memory:", None, SecretCipher::from_key([11; 32]), 1)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn native_owner_invitation_and_mode_roundtrip_preserve_identity() {
        native_owner_invitation_and_mode_roundtrip_preserve_identity_contract(&store().await).await;
    }

    #[expect(
        clippy::too_many_lines,
        reason = "Keep the same complete identity and mode roundtrip scenario on both database backends."
    )]
    async fn native_owner_invitation_and_mode_roundtrip_preserve_identity_contract(
        store: &ControlStore,
    ) {
        assert!(store.instance_settings().await.unwrap().is_none());
        let owner = store
            .initialize_owner(&registration("Owner"), 1_000)
            .await
            .unwrap();
        let initial = owner.session.instance.clone();
        assert!(owner.session.is_instance_owner);
        let login = store
            .login_native("OWNER", "test-password-123", 2_000)
            .await
            .unwrap();
        assert_eq!(login.session.user, owner.session.user);
        assert!(
            store
                .login_native("owner", "incorrect", 2_000)
                .await
                .is_err()
        );
        assert!(
            store
                .initialize_owner(&registration("second-owner"), 2_000)
                .await
                .is_err()
        );
        let multiple = store
            .set_instance_mode(&owner.session.user, InstanceMode::MultiUser, 1, 3_000)
            .await
            .unwrap();
        let invite = store
            .create_user_invitation(
                &owner.session.user,
                &UserInvitationRequest {
                    tenant_id: None,
                    role: TenantRole::Member,
                    expires_in_seconds: 300,
                },
                4_000,
            )
            .await
            .unwrap();
        let member = store
            .accept_user_invitation(&invite.token, &registration("member"), 5_000)
            .await
            .unwrap();
        assert!(!member.session.is_instance_owner);
        assert!(
            store
                .accept_user_invitation(&invite.token, &registration("other"), 5_001)
                .await
                .is_err()
        );
        assert_eq!(
            store.list_tenants(&member.session.user).await.unwrap()[0].tenant_id,
            member.session.personal_tenant_id
        );
        assert!(
            store
                .set_instance_mode(
                    &member.session.user,
                    InstanceMode::SingleUser,
                    multiple.revision,
                    6_000
                )
                .await
                .is_err()
        );
        let single = store
            .set_instance_mode(
                &owner.session.user,
                InstanceMode::SingleUser,
                multiple.revision,
                6_000,
            )
            .await
            .unwrap();
        assert!(
            store
                .authenticate_native_session(&member.access_token, 7_000)
                .await
                .is_err()
        );
        assert!(
            store
                .authenticate_native_token(&member.access_token, 7_000)
                .await
                .is_ok()
        );
        assert!(
            store
                .authenticate_native_session(&owner.access_token, 7_000)
                .await
                .is_ok()
        );
        let restored = store
            .set_instance_mode(
                &owner.session.user,
                InstanceMode::MultiUser,
                single.revision,
                8_000,
            )
            .await
            .unwrap();
        assert_eq!(restored.owner_user_id, initial.owner_user_id);
        let restored_session = store
            .identity_session(owner.session.user.clone())
            .await
            .unwrap();
        assert_eq!(
            restored_session.personal_tenant_id,
            owner.session.personal_tenant_id
        );
        assert_eq!(
            restored_session.personal_project_id,
            owner.session.personal_project_id
        );
        assert_eq!(
            store
                .authenticate_native_session(&member.access_token, 9_000)
                .await
                .unwrap()
                .user,
            member.session.user
        );
        store
            .logout_native_session(&member.access_token, 10_000)
            .await
            .unwrap();
        assert!(
            store
                .authenticate_native_session(&member.access_token, 10_001)
                .await
                .is_err()
        );
        assert!(
            store
                .authenticate_native_session(&owner.access_token, 1_000 + SESSION_TTL_MS)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn conflicting_account_creation_does_not_consume_invitation() {
        conflicting_account_creation_does_not_consume_invitation_contract(&store().await).await;
    }

    async fn conflicting_account_creation_does_not_consume_invitation_contract(
        store: &ControlStore,
    ) {
        let owner = store
            .initialize_owner(&registration("owner"), 1_000)
            .await
            .unwrap();
        store
            .set_instance_mode(&owner.session.user, InstanceMode::MultiUser, 1, 2_000)
            .await
            .unwrap();
        let invite = store
            .create_user_invitation(
                &owner.session.user,
                &UserInvitationRequest {
                    tenant_id: None,
                    role: TenantRole::Viewer,
                    expires_in_seconds: 1,
                },
                3_000,
            )
            .await
            .unwrap();
        assert!(
            store
                .accept_user_invitation(&invite.token, &registration("owner"), 3_001)
                .await
                .is_err()
        );
        let accepted = store
            .accept_user_invitation(&invite.token, &registration("new-user"), 3_002)
            .await
            .unwrap();
        assert!(!accepted.session.is_instance_owner);
        let users: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM control_users")
            .fetch_one(store.database.pool())
            .await
            .unwrap();
        assert_eq!(users, 2);
        let expired = store
            .create_user_invitation(
                &owner.session.user,
                &UserInvitationRequest {
                    tenant_id: None,
                    role: TenantRole::Viewer,
                    expires_in_seconds: 1,
                },
                4_000,
            )
            .await
            .unwrap();
        assert!(
            store
                .accept_user_invitation(&expired.token, &registration("expired"), 5_000)
                .await
                .is_err()
        );
        assert!(
            store
                .set_instance_mode(&owner.session.user, InstanceMode::SingleUser, 1, 6_000)
                .await
                .is_err()
        );
        assert_eq!(
            store.instance_settings().await.unwrap().unwrap().mode,
            InstanceMode::MultiUser
        );
    }

    #[tokio::test]
    async fn database_contains_hashes_and_no_plaintext_authentication_secrets() {
        database_contains_hashes_and_no_plaintext_authentication_secrets_contract(&store().await)
            .await;
    }

    async fn database_contains_hashes_and_no_plaintext_authentication_secrets_contract(
        store: &ControlStore,
    ) {
        let registration = registration("owner");
        let owner = store.initialize_owner(&registration, 1_000).await.unwrap();
        let hash: String = sqlx::query_scalar("SELECT password_hash FROM control_native_accounts")
            .fetch_one(store.database.pool())
            .await
            .unwrap();
        assert!(hash.starts_with("$argon2id$"));
        assert!(!hash.contains(&registration.password));
        let digest: String = sqlx::query_scalar("SELECT token_hash FROM control_browser_sessions")
            .fetch_one(store.database.pool())
            .await
            .unwrap();
        assert_eq!(digest, hex(&token_hash(&owner.access_token)));
        assert!(!digest.contains(&owner.access_token));
        store
            .set_instance_mode(&owner.session.user, InstanceMode::MultiUser, 1, 2_000)
            .await
            .unwrap();
        let invite = store
            .create_user_invitation(
                &owner.session.user,
                &UserInvitationRequest {
                    tenant_id: None,
                    role: TenantRole::Admin,
                    expires_in_seconds: 300,
                },
                3_000,
            )
            .await
            .unwrap();
        let digest: String = sqlx::query_scalar("SELECT token_hash FROM control_user_invitations")
            .fetch_one(store.database.pool())
            .await
            .unwrap();
        assert_eq!(digest, hex(&token_hash(&invite.token)));
        sqlx::query("UPDATE control_users SET email = 'owner@example.test' WHERE user_id = $1")
            .bind(owner.session.user.user_id.as_str())
            .execute(store.database.pool())
            .await
            .unwrap();
        let oidc = store
            .upsert_user(
                &crate::OidcPrincipal {
                    issuer: "https://identity.example".to_owned(),
                    subject: "external-owner".to_owned(),
                    email: Some("owner@example.test".to_owned()),
                    display_name: Some("owner".to_owned()),
                },
                "test-external-owner",
                4_000,
            )
            .await
            .unwrap_err();
        assert_eq!(oidc.message, "email is already registered");
        assert!(
            store
                .account_login_methods(&owner.session.user)
                .await
                .unwrap()
                .oidc
                .is_none()
        );
    }

    #[tokio::test]
    async fn explicit_oidc_link_preserves_native_owner_and_rejects_account_merging() {
        explicit_oidc_link_contract(&store().await).await;
    }

    #[expect(
        clippy::too_many_lines,
        reason = "Verify explicit binding, conflict rollback and both login methods against the same persistent owner resources."
    )]
    async fn explicit_oidc_link_contract(store: &ControlStore) {
        let owner = store
            .initialize_owner(&registration("owner"), 1_000)
            .await
            .unwrap();
        let user = &owner.session.user;
        let instance = &owner.session.instance;
        let workspace = store
            .create_cloud_workspace(
                user,
                &owner.session.personal_tenant_id,
                &owner.session.personal_project_id,
                "Owner resources",
                1_001,
            )
            .await
            .unwrap();
        store
            .put_user_credential(
                user,
                &owner.session.personal_tenant_id,
                "EXISTING_KEY",
                "retained-value",
                1_002,
            )
            .await
            .unwrap();
        let inventory = store
            .user_credential_inventory(user, &owner.session.personal_tenant_id)
            .await
            .unwrap();
        sqlx::query("UPDATE control_users SET email='shared@example.test' WHERE user_id=$1")
            .bind(user.user_id.as_str())
            .execute(store.database.pool())
            .await
            .unwrap();
        let other_principal = OidcPrincipal {
            issuer: "https://identity.example".to_owned(),
            subject: "existing-other-account".to_owned(),
            email: Some("external-contact@example.test".to_owned()),
            display_name: Some("External account".to_owned()),
        };
        let other = store
            .upsert_user(
                &other_principal,
                &format!("test-{}", other_principal.subject),
                1_003,
            )
            .await
            .unwrap();
        assert_ne!(other.user_id, user.user_id);
        assert!(!store.account_login_methods(&other).await.unwrap().native);
        assert_eq!(
            store.account_login_methods(user).await.unwrap(),
            AccountLoginMethods {
                native: true,
                oidc: None
            }
        );
        assert_eq!(
            store
                .link_native_oidc(user, &other_principal, 1_004)
                .await
                .unwrap_err()
                .code,
            ternilo_protocol::ErrorCode::Conflict
        );
        assert!(
            store
                .account_login_methods(user)
                .await
                .unwrap()
                .oidc
                .is_none()
        );
        let principal = OidcPrincipal {
            email: Some("shared@example.test".to_owned()),
            subject: "explicit-owner-account".to_owned(),
            ..other_principal.clone()
        };
        let methods = store
            .link_native_oidc(user, &principal, 1_005)
            .await
            .unwrap();
        assert_eq!(
            methods,
            AccountLoginMethods {
                native: true,
                oidc: Some(OidcAccountLink {
                    issuer: principal.issuer.clone(),
                    subject: principal.subject.clone()
                }),
            }
        );
        assert_eq!(
            store
                .link_native_oidc(user, &principal, 1_006)
                .await
                .unwrap(),
            methods
        );
        assert_eq!(
            store
                .link_native_oidc(user, &other_principal, 1_007)
                .await
                .unwrap_err()
                .code,
            ternilo_protocol::ErrorCode::Conflict
        );
        let oidc_user = store
            .upsert_user(&principal, &format!("test-{}", principal.subject), 1_008)
            .await
            .unwrap();
        assert_eq!(oidc_user.user_id, user.user_id);
        let oidc_session = store.identity_session(oidc_user).await.unwrap();
        assert!(oidc_session.is_instance_owner);
        assert_eq!(oidc_session.instance, *instance);
        let native = store
            .login_native("owner", "test-password-123", 1_009)
            .await
            .unwrap();
        assert_eq!(native.session.user.user_id, user.user_id);
        assert!(native.session.is_instance_owner);
        assert_eq!(
            store
                .authenticate_native_session(&owner.access_token, 1_010)
                .await
                .unwrap()
                .user
                .user_id,
            user.user_id
        );
        assert_eq!(
            store
                .resolve_owned_workspace(
                    user,
                    &owner.session.personal_tenant_id,
                    &workspace.workspace_id
                )
                .await
                .unwrap(),
            workspace
        );
        assert_eq!(
            store
                .user_credential_inventory(user, &owner.session.personal_tenant_id)
                .await
                .unwrap(),
            inventory
        );
        assert_eq!(
            store
                .upsert_user(
                    &other_principal,
                    &format!("test-{}", other_principal.subject),
                    1_011
                )
                .await
                .unwrap()
                .user_id,
            other.user_id
        );
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM control_users")
            .fetch_one(store.database.pool())
            .await
            .unwrap();
        assert_eq!(count, 2, "binding must not create or merge accounts");
        let links = sqlx::query("SELECT actor_user_id, metadata FROM control_platform_audit WHERE action = 'identity.oidc_link'")
            .fetch_all(store.database.pool()).await.unwrap();
        assert_eq!(links.len(), 1);
        assert_eq!(
            links[0].try_get::<String, _>("actor_user_id").unwrap(),
            user.user_id.as_str()
        );
        let metadata: serde_json::Value =
            serde_json::from_str(&links[0].try_get::<String, _>("metadata").unwrap()).unwrap();
        assert_eq!(metadata["subject"], principal.subject);
    }

    #[tokio::test]
    #[ignore = "requires TERNILO_IDENTITY_TEST_DATABASE_URL for a disposable PostgreSQL database"]
    async fn postgres_native_identity_enforces_the_same_contract_with_runtime_role() {
        use sqlx::Executor as _;
        let admin_url = std::env::var("TERNILO_IDENTITY_TEST_DATABASE_URL").unwrap();
        assert!(
            admin_url.contains("ternilo_identity_test"),
            "identity tests require a disposable ternilo_identity_test database"
        );
        let admin = sqlx::PgPool::connect(&admin_url).await.unwrap();
        admin
            .execute("DROP SCHEMA IF EXISTS public CASCADE")
            .await
            .unwrap();
        admin.execute("CREATE SCHEMA public").await.unwrap();
        crate::postgres_test::prepare_role(
            &admin,
            "ternilo_identity_runtime_test",
            "identity-test-password",
        )
        .await;
        let mut runtime_url = admin_url
            .parse::<sqlx::any::AnyConnectOptions>()
            .unwrap()
            .database_url;
        runtime_url
            .set_username("ternilo_identity_runtime_test")
            .unwrap();
        runtime_url
            .set_password(Some("identity-test-password"))
            .unwrap();
        for scenario in 0..17 {
            if scenario != 0 {
                admin.execute("DROP SCHEMA public CASCADE").await.unwrap();
                admin.execute("CREATE SCHEMA public").await.unwrap();
            }
            let bootstrap =
                ControlStore::connect(&admin_url, None, SecretCipher::from_key([11; 32]), 1)
                    .await
                    .unwrap();
            bootstrap.database().close().await;
            crate::postgres_test::grant_schema_usage(&admin, "ternilo_identity_runtime_test").await;
            let store = ControlStore::connect(
                runtime_url.as_str(),
                Some(&admin_url),
                SecretCipher::from_key([11; 32]),
                2,
            )
            .await
            .unwrap();
            match scenario {
                0 => {
                    native_owner_invitation_and_mode_roundtrip_preserve_identity_contract(&store)
                        .await;
                }
                1 => {
                    conflicting_account_creation_does_not_consume_invitation_contract(&store).await;
                }
                2 => {
                    database_contains_hashes_and_no_plaintext_authentication_secrets_contract(
                        &store,
                    )
                    .await;
                }
                3 => explicit_oidc_link_contract(&store).await,
                4 => {
                    crate::account_store::tests::personal_spaces_and_invitations_contract(&store)
                        .await;
                }
                5 => crate::account_store::tests::directory_and_roles_contract(&store).await,
                6 => crate::account_store::tests::invitations_require_current_grant_authority_contract(&store).await,
                7 => crate::registration_store::tests::native_review_contract(&store).await,
                8 => crate::registration_store::tests::invitation_gate_contract(&store).await,
                9 => crate::registration_store::tests::oidc_gate_contract(&store).await,
                10 => crate::registration_store::tests::status_pagination_contract(&store).await,
                11 => crate::registration_store::tests::canonical_username_contract(&store).await,
                12 => crate::registration_store::tests::username_race_contract(&store).await,
                13 => crate::account_status_store::tests::lifecycle_contract(&store).await,
                14 => crate::account_status_store::tests::email_contract(&store).await,
                15 => crate::account_status_store::lock_order_tests::invitation_lock_order_contract(&store, &admin).await,
                16 => crate::account_status_store::lock_order_tests::node_lock_order_contract(&store, &admin).await,
                _ => unreachable!(),
            }
            store.database().close().await;
        }
        admin.execute("DROP SCHEMA public CASCADE").await.unwrap();
        admin.execute("CREATE SCHEMA public").await.unwrap();
        admin
            .execute("DROP ROLE ternilo_identity_runtime_test")
            .await
            .unwrap();
        admin.close().await;
    }
}
