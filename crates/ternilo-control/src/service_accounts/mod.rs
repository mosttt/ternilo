use serde::{Deserialize, Serialize};
use sqlx::{Row, any::AnyRow};
use ternilo_protocol::{HarnessError, TenantId, UserId};
use ternilo_storage::{Database, Transaction, database_error, lock};

use crate::{
    ControlAction, ControlStore, ControlUser,
    store::{append_audit, from_i64, require_action, to_i64},
};

mod credentials;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub enum ServiceScope {
    #[serde(rename = "resource.read")]
    ResourceRead,
    #[serde(rename = "run.execute")]
    RunExecute,
}

#[derive(Debug, Serialize)]
pub struct ServiceAccount {
    pub service_account_id: UserId,
    pub tenant_id: TenantId,
    pub name: String,
    pub notes: String,
    pub enabled: bool,
    pub revision: u64,
    pub created_by: UserId,
    pub created_at_ms: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceAccountCreate {
    pub name: String,
    #[serde(default)]
    pub notes: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceAccountUpdate {
    pub name: String,
    pub notes: String,
    pub enabled: bool,
    pub expected_revision: u64,
}

#[derive(Debug, Serialize)]
pub struct ServiceCredential {
    pub credential_id: String,
    pub name: String,
    pub scopes: Vec<ServiceScope>,
    pub issued_at_ms: u64,
    pub expires_at_ms: u64,
    pub last_used_at_ms: Option<u64>,
    pub revoked_at_ms: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceCredentialCreate {
    pub name: String,
    pub scopes: Vec<ServiceScope>,
    pub expires_at_ms: u64,
}

#[derive(Serialize)]
pub struct ServiceCredentialGrant {
    pub credential: ServiceCredential,
    pub access_token: String,
}

#[derive(Debug)]
pub struct ServicePrincipal {
    pub user: ControlUser,
    pub tenant_id: TenantId,
    pub credential_id: String,
    pub scopes: Vec<ServiceScope>,
    pub expires_at_ms: u64,
}

impl ServicePrincipal {
    pub fn require(&self, scope: ServiceScope) -> Result<(), HarnessError> {
        if self.scopes.contains(&scope) {
            Ok(())
        } else {
            Err(HarnessError::policy(
                "service credential does not allow this operation",
            ))
        }
    }
}

pub(crate) async fn initialize(database: &Database) -> Result<(), HarnessError> {
    database
        .initialize(
            "service_accounts",
            1,
            include_str!("schema.sql"),
            include_str!("postgres.sql"),
        )
        .await
}

impl ControlStore {
    pub async fn list_service_accounts(
        &self,
        actor: &ControlUser,
        tenant: &TenantId,
    ) -> Result<Vec<ServiceAccount>, HarnessError> {
        let mut tx = self.service_management(actor, tenant).await?;
        let rows = sqlx::query("SELECT * FROM control_service_accounts WHERE tenant_id=$1 ORDER BY created_at_ms,service_account_id")
            .bind(tenant.as_str()).fetch_all(&mut *tx).await.map_err(database_error)?;
        let accounts = rows
            .iter()
            .map(account_record)
            .collect::<Result<Vec<_>, _>>()?;
        tx.commit().await.map_err(database_error)?;
        Ok(accounts)
    }

    pub async fn create_service_account(
        &self,
        actor: &ControlUser,
        tenant: &TenantId,
        draft: &ServiceAccountCreate,
        now_ms: u64,
    ) -> Result<ServiceAccount, HarnessError> {
        let name = validate_name(&draft.name)?;
        validate_notes(&draft.notes)?;
        let mut tx = self.service_management(actor, tenant).await?;
        lock(&mut tx, &format!("ternilo:service-accounts:{tenant}")).await?;
        require_available_name(&mut tx, tenant, name, "").await?;
        let id = UserId::new(crate::crypto::random_identifier("ter_sa"));
        let username = format!("svc-{}", crate::crypto::hex(&rand::random::<[u8; 16]>()));
        let now = to_i64(now_ms, "service account creation")?;
        sqlx::query("INSERT INTO control_users(user_id,issuer,subject,username,created_at_ms,last_seen_at_ms) VALUES($1,'urn:ternilo:service',$1,$2,$3,$3)")
            .bind(id.as_str()).bind(&username).bind(now).execute(&mut *tx).await.map_err(database_error)?;
        sqlx::query("INSERT INTO control_memberships(tenant_id,user_id,role,created_at_ms) VALUES($1,$2,'member',$3)")
            .bind(tenant.as_str()).bind(id.as_str()).bind(now).execute(&mut *tx).await.map_err(database_error)?;
        sqlx::query("INSERT INTO control_service_accounts(tenant_id,service_account_id,name,notes,created_by,created_at_ms) VALUES($1,$2,$3,$4,$5,$6)")
            .bind(tenant.as_str()).bind(id.as_str()).bind(name).bind(&draft.notes).bind(actor.user_id.as_str()).bind(now)
            .execute(&mut *tx).await.map_err(database_error)?;
        append_audit(
            &mut tx,
            tenant,
            Some(&actor.user_id),
            "user",
            "service_account.create",
            "service_account",
            id.as_str(),
            "success",
            serde_json::json!({"name":name}),
            now_ms,
        )
        .await?;
        let account = account_in(&mut tx, tenant, &id).await?;
        tx.commit().await.map_err(database_error)?;
        Ok(account)
    }

    pub async fn update_service_account(
        &self,
        actor: &ControlUser,
        tenant: &TenantId,
        id: &UserId,
        draft: &ServiceAccountUpdate,
        now_ms: u64,
    ) -> Result<ServiceAccount, HarnessError> {
        let name = validate_name(&draft.name)?;
        validate_notes(&draft.notes)?;
        let mut tx = self.service_management(actor, tenant).await?;
        lock(&mut tx, &format!("ternilo:service-accounts:{tenant}")).await?;
        let current = account_in(&mut tx, tenant, id).await?;
        if current.revision != draft.expected_revision {
            return Err(HarnessError::conflict(
                "service account changed; reload before saving",
            ));
        }
        require_available_name(&mut tx, tenant, name, id.as_str()).await?;
        sqlx::query("UPDATE control_service_accounts SET name=$3,notes=$4,enabled=$5,revision=revision+1 WHERE tenant_id=$1 AND service_account_id=$2")
            .bind(tenant.as_str()).bind(id.as_str()).bind(name).bind(&draft.notes).bind(i32::from(draft.enabled))
            .execute(&mut *tx).await.map_err(database_error)?;
        append_audit(
            &mut tx,
            tenant,
            Some(&actor.user_id),
            "user",
            "service_account.update",
            "service_account",
            id.as_str(),
            "success",
            serde_json::json!({"name":name,"enabled":draft.enabled}),
            now_ms,
        )
        .await?;
        let account = account_in(&mut tx, tenant, id).await?;
        tx.commit().await.map_err(database_error)?;
        Ok(account)
    }

    async fn service_management(
        &self,
        actor: &ControlUser,
        tenant: &TenantId,
    ) -> Result<Transaction, HarnessError> {
        tenant.validate()?;
        let mut tx = self.database.tenant_transaction(tenant).await?;
        require_action(
            &mut tx,
            tenant,
            &actor.user_id,
            ControlAction::MembershipManage,
        )
        .await?;
        let instance = crate::identity_store::required_instance(&mut tx).await?;
        crate::identity_store::require_remote_access(&instance, actor)?;
        Ok(tx)
    }
}

async fn account_in(
    tx: &mut Transaction,
    tenant: &TenantId,
    id: &UserId,
) -> Result<ServiceAccount, HarnessError> {
    id.validate()?;
    let row = sqlx::query(
        "SELECT * FROM control_service_accounts WHERE tenant_id=$1 AND service_account_id=$2",
    )
    .bind(tenant.as_str())
    .bind(id.as_str())
    .fetch_optional(&mut **tx)
    .await
    .map_err(database_error)?
    .ok_or_else(|| HarnessError::policy("service account is not available in this space"))?;
    account_record(&row)
}

fn account_record(row: &AnyRow) -> Result<ServiceAccount, HarnessError> {
    Ok(ServiceAccount {
        tenant_id: TenantId::new(
            row.try_get::<String, _>("tenant_id")
                .map_err(database_error)?,
        ),
        service_account_id: UserId::new(
            row.try_get::<String, _>("service_account_id")
                .map_err(database_error)?,
        ),
        name: row.try_get("name").map_err(database_error)?,
        notes: row.try_get("notes").map_err(database_error)?,
        enabled: row.try_get::<i32, _>("enabled").map_err(database_error)? == 1,
        revision: from_i64(
            row.try_get("revision").map_err(database_error)?,
            "service account revision",
        )?,
        created_by: UserId::new(
            row.try_get::<String, _>("created_by")
                .map_err(database_error)?,
        ),
        created_at_ms: from_i64(
            row.try_get("created_at_ms").map_err(database_error)?,
            "service account creation",
        )?,
    })
}

fn validate_name(name: &str) -> Result<&str, HarnessError> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 128 || name.chars().any(char::is_control) {
        return Err(HarnessError::invalid(
            "service account name must contain 1 to 128 characters without controls",
        ));
    }
    Ok(name)
}

async fn require_available_name(
    tx: &mut Transaction,
    tenant: &TenantId,
    name: &str,
    id: &str,
) -> Result<(), HarnessError> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM control_service_accounts WHERE tenant_id=$1 AND name=$2 AND service_account_id<>$3")
        .bind(tenant.as_str()).bind(name).bind(id).fetch_one(&mut **tx).await.map_err(database_error)?;
    if count != 0 {
        return Err(HarnessError::conflict(
            "service account name is already used in this space",
        ));
    }
    Ok(())
}

fn validate_notes(notes: &str) -> Result<(), HarnessError> {
    if notes.len() > 4096 || notes.contains('\0') {
        return Err(HarnessError::invalid(
            "service account notes exceed 4096 bytes or contain a null character",
        ));
    }
    Ok(())
}
