use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{Row, any::AnyRow};
use ternilo_protocol::{HarnessError, TenantId, UserId};
use ternilo_storage::{Transaction, database_error, lock, set_tenant_scope};

use crate::{
    AccountStatus, ControlStore, ControlUser, SpaceKind, TenantQuota,
    crypto::{chained_hash, hex, random_identifier},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlatformRole {
    Owner,
    Admin,
    Operator,
    Auditor,
    User,
}

impl PlatformRole {
    pub(crate) fn parse(value: &str) -> Result<Self, HarnessError> {
        match value {
            "owner" => Ok(Self::Owner),
            "admin" => Ok(Self::Admin),
            "operator" => Ok(Self::Operator),
            "auditor" => Ok(Self::Auditor),
            "user" => Ok(Self::User),
            _ => Err(HarnessError::execution("stored platform role is invalid")),
        }
    }

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Admin => "admin",
            Self::Operator => "operator",
            Self::Auditor => "auditor",
            Self::User => "user",
        }
    }

    #[must_use]
    pub const fn allows(self, action: PlatformAction) -> bool {
        match action {
            PlatformAction::AccountsRead => {
                matches!(self, Self::Owner | Self::Admin | Self::Auditor)
            }
            PlatformAction::AccountsInvite
            | PlatformAction::RegistrationManage
            | PlatformAction::AccountsManage
            | PlatformAction::AccountsReview
            | PlatformAction::ModelGrantsManage => {
                matches!(self, Self::Owner | Self::Admin)
            }
            PlatformAction::WorkersRead | PlatformAction::ModelsRead => !matches!(self, Self::User),
            PlatformAction::WorkersManage | PlatformAction::ModelsManage => {
                matches!(self, Self::Owner | Self::Admin | Self::Operator)
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlatformAction {
    AccountsRead,
    AccountsInvite,
    RegistrationManage,
    AccountsReview,
    AccountsManage,
    WorkersRead,
    WorkersManage,
    ModelsRead,
    ModelsManage,
    ModelGrantsManage,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountListQuery {
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default)]
    pub role: Option<PlatformRole>,
    #[serde(default)]
    pub status: Option<AccountStatus>,
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default = "default_page_limit")]
    pub limit: u32,
}

impl Default for AccountListQuery {
    fn default() -> Self {
        Self {
            query: None,
            role: None,
            status: None,
            cursor: None,
            limit: default_page_limit(),
        }
    }
}

const fn default_page_limit() -> u32 {
    25
}

#[derive(Clone, Debug, Serialize)]
pub struct AccountRecord {
    pub user_id: UserId,
    pub username: String,
    pub email: Option<String>,
    pub platform_role: PlatformRole,
    pub role_revision: u64,
    pub status: AccountStatus,
    pub status_revision: u64,
    pub created_at_ms: u64,
    pub personal_tenant_id: TenantId,
}

#[derive(Clone, Debug, Serialize)]
pub struct AccountPage {
    pub accounts: Vec<AccountRecord>,
    pub next_cursor: Option<String>,
}

const ACCOUNT_COLUMNS: &str = "user_row.user_id, user_row.username, user_row.email,
    CASE WHEN user_row.user_id = instance.owner_user_id THEN 'owner' ELSE user_row.platform_role END AS effective_role,
    user_row.role_revision, user_row.status, user_row.status_revision, user_row.created_at_ms, home.personal_tenant_id";
const ACCOUNT_JOINS: &str = "FROM control_users user_row
    JOIN control_account_spaces home ON home.user_id = user_row.user_id
    JOIN control_instance_settings instance ON instance.singleton = 1";

impl ControlStore {
    pub(crate) async fn create_personal_space_in(
        transaction: &mut Transaction,
        user: &ControlUser,
        now_ms: u64,
    ) -> Result<(), HarnessError> {
        let slug = format!("personal-{}", hex(&rand::random::<[u8; 16]>()));
        let tenant = Self::create_tenant_in(
            transaction,
            user,
            &slug,
            "Personal",
            TenantQuota::default(),
            SpaceKind::Personal,
            now_ms,
        )
        .await?;
        sqlx::query(
            "INSERT INTO control_account_spaces (user_id, personal_tenant_id, default_project_id)
            SELECT $1, tenant_id, project_id FROM control_projects WHERE tenant_id = $2",
        )
        .bind(user.user_id.as_str())
        .bind(tenant.tenant_id.as_str())
        .execute(&mut **transaction)
        .await
        .map_err(database_error)?;
        Ok(())
    }

    pub async fn require_platform_action(
        &self,
        actor: &ControlUser,
        action: PlatformAction,
    ) -> Result<PlatformRole, HarnessError> {
        let mut transaction = self.database.begin().await?;
        let role = platform_role_in(&mut transaction, &actor.user_id).await?;
        require_role_action(role, action)?;
        transaction.commit().await.map_err(database_error)?;
        Ok(role)
    }

    pub async fn list_accounts(
        &self,
        actor: &ControlUser,
        query: &AccountListQuery,
    ) -> Result<AccountPage, HarnessError> {
        if !(1..=100).contains(&query.limit) {
            return Err(HarnessError::invalid(
                "account page limit must be between 1 and 100",
            ));
        }
        let search = query
            .query
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        if search.is_some_and(|value| value.len() > 256 || value.chars().any(char::is_control)) {
            return Err(HarnessError::invalid(
                "account search must contain at most 256 bytes without control characters",
            ));
        }
        let pattern = search.map(|value| {
            format!(
                "%{}%",
                value
                    .to_lowercase()
                    .replace('!', "!!")
                    .replace('%', "!%")
                    .replace('_', "!_")
            )
        });
        let cursor = query.cursor.as_deref().map(decode_cursor).transpose()?;
        let mut transaction = self.database.begin().await?;
        authorize_platform_in(
            &mut transaction,
            &actor.user_id,
            PlatformAction::AccountsRead,
        )
        .await?;
        let sql = format!("SELECT {ACCOUNT_COLUMNS} {ACCOUNT_JOINS}
            WHERE (CAST($1 AS TEXT) IS NULL OR LOWER(user_row.username) LIKE $1 ESCAPE '!'
                OR LOWER(user_row.user_id) LIKE $1 ESCAPE '!'
                OR LOWER(user_row.email) LIKE $1 ESCAPE '!')
              AND (CAST($2 AS TEXT) IS NULL OR (CASE WHEN user_row.user_id = instance.owner_user_id THEN 'owner' ELSE user_row.platform_role END) = $2)
              AND (CAST($3 AS BIGINT) IS NULL OR user_row.created_at_ms < $3
                OR (user_row.created_at_ms = $3 AND user_row.user_id < $4))
              AND (CAST($5 AS TEXT) IS NULL OR user_row.status = $5)
            ORDER BY user_row.created_at_ms DESC, user_row.user_id DESC LIMIT $6");
        // SQL fragments above are constants; every request value is bound below.
        let rows = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(pattern)
            .bind(query.role.map(PlatformRole::as_str))
            .bind(cursor.as_ref().map(|value| value.0))
            .bind(cursor.as_ref().map(|value| value.1.as_str()))
            .bind(query.status.map(AccountStatus::as_str))
            .bind(i64::from(query.limit) + 1)
            .fetch_all(&mut *transaction)
            .await
            .map_err(database_error)?;
        transaction.commit().await.map_err(database_error)?;
        let mut accounts = rows
            .iter()
            .map(account_from_row)
            .collect::<Result<Vec<_>, _>>()?;
        let has_more = accounts.len() > query.limit as usize;
        accounts.truncate(query.limit as usize);
        let next_cursor = if has_more {
            accounts.last().map(encode_cursor).transpose()?
        } else {
            None
        };
        Ok(AccountPage {
            accounts,
            next_cursor,
        })
    }

    pub async fn get_account(
        &self,
        actor: &ControlUser,
        user_id: &UserId,
    ) -> Result<AccountRecord, HarnessError> {
        let mut transaction = self.database.begin().await?;
        authorize_platform_in(
            &mut transaction,
            &actor.user_id,
            PlatformAction::AccountsRead,
        )
        .await?;
        let account = account_in(&mut transaction, user_id).await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(account)
    }

    pub async fn set_account_role(
        &self,
        actor: &ControlUser,
        user_id: &UserId,
        role: PlatformRole,
        role_revision: u64,
        now_ms: u64,
    ) -> Result<AccountRecord, HarnessError> {
        if role == PlatformRole::Owner {
            return Err(HarnessError::invalid(
                "the platform owner cannot be assigned through account roles",
            ));
        }
        user_id.validate()?;
        let mut transaction = self.database.begin().await?;
        lock(&mut transaction, "ternilo:instance").await?;
        if platform_role_in(&mut transaction, &actor.user_id).await? != PlatformRole::Owner {
            return Err(HarnessError::policy(
                "only the platform owner can assign account roles",
            ));
        }
        lock(&mut transaction, &format!("ternilo:account-role:{user_id}")).await?;
        let account = account_in(&mut transaction, user_id).await?;
        account.status.require_active()?;
        if account.platform_role == PlatformRole::Owner {
            return Err(HarnessError::policy(
                "the platform owner role cannot be changed",
            ));
        }
        if account.role_revision != role_revision {
            return Err(HarnessError::conflict(
                "account role changed; reload before saving",
            ));
        }
        if account.platform_role == role {
            transaction.commit().await.map_err(database_error)?;
            return Ok(account);
        }
        let revision = role_revision
            .checked_add(1)
            .ok_or_else(|| HarnessError::execution("account role revision overflow"))?;
        sqlx::query(
            "UPDATE control_users SET platform_role = $2, role_revision = $3 WHERE user_id = $1",
        )
        .bind(user_id.as_str())
        .bind(role.as_str())
        .bind(timestamp(revision)?)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        append_platform_audit(
            &mut transaction,
            &actor.user_id,
            "account.role",
            user_id.as_str(),
            json!({"previous": account.platform_role, "role": role, "revision": revision}),
            now_ms,
        )
        .await?;
        let updated = account_in(&mut transaction, user_id).await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(updated)
    }
}

pub async fn authorize_platform_in(
    transaction: &mut Transaction,
    user_id: &UserId,
    action: PlatformAction,
) -> Result<(), HarnessError> {
    require_role_action(platform_role_in(transaction, user_id).await?, action)
}

pub(crate) async fn platform_role_in(
    transaction: &mut Transaction,
    user_id: &UserId,
) -> Result<PlatformRole, HarnessError> {
    user_id.validate()?;
    let row = sqlx::query("SELECT user_row.platform_role, user_row.status, instance.owner_user_id, instance.mode
        FROM control_users user_row JOIN control_instance_settings instance ON instance.singleton = 1
        WHERE user_row.user_id = $1")
        .bind(user_id.as_str()).fetch_optional(&mut **transaction).await.map_err(database_error)?
        .ok_or_else(|| HarnessError::policy("account or server initialization does not exist"))?;
    AccountStatus::parse(&row.try_get::<String, _>("status").map_err(database_error)?)?
        .require_active()?;
    let owner: String = row.try_get("owner_user_id").map_err(database_error)?;
    if owner == user_id.as_str() {
        return Ok(PlatformRole::Owner);
    }
    if row.try_get::<String, _>("mode").map_err(database_error)? == "single_user" {
        return Err(HarnessError::policy(
            "this account is paused while the server is in single-user mode",
        ));
    }
    PlatformRole::parse(
        &row.try_get::<String, _>("platform_role")
            .map_err(database_error)?,
    )
}

fn require_role_action(role: PlatformRole, action: PlatformAction) -> Result<(), HarnessError> {
    if role.allows(action) {
        Ok(())
    } else {
        Err(HarnessError::policy(
            "platform role does not allow this action",
        ))
    }
}

pub(crate) async fn personal_space_in(
    transaction: &mut Transaction,
    user_id: &UserId,
) -> Result<(TenantId, String), HarnessError> {
    let row = sqlx::query("SELECT personal_tenant_id, default_project_id FROM control_account_spaces WHERE user_id = $1")
        .bind(user_id.as_str()).fetch_one(&mut **transaction).await.map_err(database_error)?;
    Ok((
        TenantId::new(
            row.try_get::<String, _>("personal_tenant_id")
                .map_err(database_error)?,
        ),
        row.try_get("default_project_id").map_err(database_error)?,
    ))
}

pub(crate) async fn require_team_in(
    transaction: &mut Transaction,
    tenant_id: &TenantId,
) -> Result<(), HarnessError> {
    set_tenant_scope(transaction, tenant_id).await?;
    let kind: Option<String> =
        sqlx::query_scalar("SELECT kind FROM control_tenants WHERE tenant_id = $1")
            .bind(tenant_id.as_str())
            .fetch_optional(&mut **transaction)
            .await
            .map_err(database_error)?;
    match kind.as_deref() {
        Some("team") => Ok(()),
        Some("personal") => Err(HarnessError::policy(
            "personal space membership cannot be changed; use a team space for members",
        )),
        _ => Err(HarnessError::invalid("space does not exist")),
    }
}

pub(crate) async fn append_platform_audit(
    transaction: &mut Transaction,
    actor: &UserId,
    action: &str,
    resource_id: &str,
    metadata: Value,
    now_ms: u64,
) -> Result<(), HarnessError> {
    lock(transaction, "ternilo:platform-audit").await?;
    let previous = sqlx::query(
        "SELECT sequence, entry_hash FROM control_platform_audit ORDER BY sequence DESC LIMIT 1",
    )
    .fetch_optional(&mut **transaction)
    .await
    .map_err(database_error)?;
    let (sequence, previous_hash) = if let Some(row) = previous {
        (
            row.try_get::<i64, _>("sequence")
                .map_err(database_error)?
                .checked_add(1)
                .ok_or_else(|| HarnessError::execution("platform audit sequence overflow"))?,
            Some(
                row.try_get::<String, _>("entry_hash")
                    .map_err(database_error)?,
            ),
        )
    } else {
        (1, None)
    };
    let audit_id = random_identifier("aud");
    let metadata_text = serde_json::to_string(&metadata)
        .map_err(|error| HarnessError::execution(error.to_string()))?;
    let payload = serde_json::to_vec(&json!({"sequence": sequence, "audit_id": audit_id, "actor_user_id": actor,
        "action": action, "resource_id": resource_id, "metadata": metadata, "occurred_at_ms": now_ms}))
        .map_err(|error| HarnessError::execution(error.to_string()))?;
    let entry_hash = hex(&chained_hash(
        previous_hash.as_deref().map(str::as_bytes),
        &payload,
    ));
    sqlx::query("INSERT INTO control_platform_audit (sequence, audit_id, actor_user_id, action, resource_id, metadata, occurred_at_ms, previous_hash, entry_hash)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)")
        .bind(sequence).bind(audit_id).bind(actor.as_str()).bind(action).bind(resource_id).bind(metadata_text)
        .bind(timestamp(now_ms)?).bind(previous_hash).bind(entry_hash)
        .execute(&mut **transaction).await.map_err(database_error)?;
    Ok(())
}

pub(crate) async fn account_in(
    transaction: &mut Transaction,
    user_id: &UserId,
) -> Result<AccountRecord, HarnessError> {
    user_id.validate()?;
    // Both interpolated fragments are constants, independent of account input.
    let row = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {ACCOUNT_COLUMNS} {ACCOUNT_JOINS} WHERE user_row.user_id = $1"
    )))
    .bind(user_id.as_str())
    .fetch_optional(&mut **transaction)
    .await
    .map_err(database_error)?
    .ok_or_else(|| HarnessError::invalid("account does not exist"))?;
    account_from_row(&row)
}

fn account_from_row(row: &AnyRow) -> Result<AccountRecord, HarnessError> {
    Ok(AccountRecord {
        user_id: UserId::new(
            row.try_get::<String, _>("user_id")
                .map_err(database_error)?,
        ),
        username: row.try_get("username").map_err(database_error)?,
        email: row.try_get("email").map_err(database_error)?,
        platform_role: PlatformRole::parse(
            &row.try_get::<String, _>("effective_role")
                .map_err(database_error)?,
        )?,
        role_revision: unsigned(row.try_get("role_revision").map_err(database_error)?)?,
        status: AccountStatus::parse(&row.try_get::<String, _>("status").map_err(database_error)?)?,
        status_revision: unsigned(row.try_get("status_revision").map_err(database_error)?)?,
        created_at_ms: unsigned(row.try_get("created_at_ms").map_err(database_error)?)?,
        personal_tenant_id: TenantId::new(
            row.try_get::<String, _>("personal_tenant_id")
                .map_err(database_error)?,
        ),
    })
}

fn encode_cursor(account: &AccountRecord) -> Result<String, HarnessError> {
    serde_json::to_vec(&(account.created_at_ms, account.user_id.as_str()))
        .map(|value| URL_SAFE_NO_PAD.encode(value))
        .map_err(|error| HarnessError::execution(error.to_string()))
}

fn decode_cursor(value: &str) -> Result<(i64, String), HarnessError> {
    let invalid = || HarnessError::invalid("account cursor is invalid");
    if value.len() > 512 {
        return Err(invalid());
    }
    let bytes = URL_SAFE_NO_PAD.decode(value).map_err(|_| invalid())?;
    let (created, user_id): (u64, String) =
        serde_json::from_slice(&bytes).map_err(|_| invalid())?;
    UserId::new(user_id.clone())
        .validate()
        .map_err(|_| invalid())?;
    Ok((timestamp(created)?, user_id))
}

fn timestamp(value: u64) -> Result<i64, HarnessError> {
    i64::try_from(value).map_err(|_| HarnessError::invalid("account value exceeds database range"))
}

fn unsigned(value: i64) -> Result<u64, HarnessError> {
    u64::try_from(value).map_err(|_| HarnessError::execution("stored account value is negative"))
}

#[cfg(test)]
pub(crate) mod tests;

pub(crate) async fn require_active_account_in(
    transaction: &mut Transaction,
    user_id: &UserId,
) -> Result<(), HarnessError> {
    let status: String = sqlx::query_scalar("SELECT status FROM control_users WHERE user_id = $1")
        .bind(user_id.as_str())
        .fetch_optional(&mut **transaction)
        .await
        .map_err(database_error)?
        .ok_or_else(|| HarnessError::policy("account does not exist"))?;
    AccountStatus::parse(&status)?.require_active()
}
