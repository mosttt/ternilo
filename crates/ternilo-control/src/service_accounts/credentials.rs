use serde_json::json;
use sqlx::{Row, any::AnyRow};
use ternilo_protocol::{HarnessError, TenantId, UserId};
use ternilo_storage::{database_error, lock};

use super::{
    ServiceCredential, ServiceCredentialCreate, ServiceCredentialGrant, ServicePrincipal,
    account_in, validate_name,
};
use crate::{
    AccountStatus, ControlAction, ControlStore, ControlUser, InstanceMode, TenantRole,
    crypto::{hex, random_identifier, random_token, token_hash},
    store::{append_audit, from_i64, require_action, to_i64},
};

impl ControlStore {
    pub async fn list_service_credentials(
        &self,
        actor: &ControlUser,
        tenant: &TenantId,
        id: &UserId,
    ) -> Result<Vec<ServiceCredential>, HarnessError> {
        let mut tx = self.service_management(actor, tenant).await?;
        account_in(&mut tx, tenant, id).await?;
        let rows = sqlx::query("SELECT credential_id,name,scopes,issued_at_ms,expires_at_ms,last_used_at_ms,revoked_at_ms FROM control_service_credentials WHERE tenant_id=$1 AND service_account_id=$2 ORDER BY issued_at_ms DESC,credential_id")
            .bind(tenant.as_str()).bind(id.as_str()).fetch_all(&mut *tx).await.map_err(database_error)?;
        let credentials = rows
            .iter()
            .map(credential_record)
            .collect::<Result<Vec<_>, _>>()?;
        tx.commit().await.map_err(database_error)?;
        Ok(credentials)
    }

    pub async fn create_service_credential(
        &self,
        actor: &ControlUser,
        tenant: &TenantId,
        id: &UserId,
        draft: &ServiceCredentialCreate,
        now_ms: u64,
    ) -> Result<ServiceCredentialGrant, HarnessError> {
        let name = validate_name(&draft.name)?;
        let mut scopes = draft.scopes.clone();
        scopes.sort_unstable();
        scopes.dedup();
        if scopes.is_empty()
            || draft.expires_at_ms <= now_ms
            || draft.expires_at_ms > now_ms.saturating_add(365 * 24 * 60 * 60 * 1000)
        {
            return Err(HarnessError::invalid(
                "service credential requires scopes and an expiry within one year",
            ));
        }
        let mut tx = self.service_management(actor, tenant).await?;
        lock(&mut tx, &format!("ternilo:service-accounts:{tenant}")).await?;
        if !account_in(&mut tx, tenant, id).await?.enabled {
            return Err(HarnessError::conflict(
                "enable the service account before issuing credentials",
            ));
        }
        let credential_id = random_identifier("ter_sk");
        let access_token = random_token("ter_t");
        let encoded_scopes = serde_json::to_string(&scopes)
            .map_err(|_| HarnessError::execution("encode service credential scopes"))?;
        sqlx::query("INSERT INTO control_service_credentials(tenant_id,credential_id,service_account_id,name,token_hash,scopes,issued_at_ms,expires_at_ms) VALUES($1,$2,$3,$4,$5,$6,$7,$8)")
            .bind(tenant.as_str()).bind(&credential_id).bind(id.as_str()).bind(name).bind(hex(&token_hash(&access_token)))
            .bind(encoded_scopes).bind(to_i64(now_ms,"service credential issue")?).bind(to_i64(draft.expires_at_ms,"service credential expiry")?)
            .execute(&mut *tx).await.map_err(database_error)?;
        append_audit(&mut tx, tenant, Some(&actor.user_id), "user", "service_credential.create", "service_account", id.as_str(), "success", json!({"credential_id":credential_id,"scopes":scopes,"expires_at_ms":draft.expires_at_ms}), now_ms).await?;
        tx.commit().await.map_err(database_error)?;
        Ok(ServiceCredentialGrant {
            credential: ServiceCredential {
                credential_id,
                name: name.into(),
                scopes,
                issued_at_ms: now_ms,
                expires_at_ms: draft.expires_at_ms,
                last_used_at_ms: None,
                revoked_at_ms: None,
            },
            access_token,
        })
    }

    pub async fn revoke_service_credential(
        &self,
        actor: &ControlUser,
        tenant: &TenantId,
        id: &UserId,
        credential: &str,
        now_ms: u64,
    ) -> Result<(), HarnessError> {
        let mut tx = self.service_management(actor, tenant).await?;
        lock(&mut tx, &format!("ternilo:service-accounts:{tenant}")).await?;
        account_in(&mut tx, tenant, id).await?;
        let changed = sqlx::query("UPDATE control_service_credentials SET revoked_at_ms=$4 WHERE tenant_id=$1 AND service_account_id=$2 AND credential_id=$3 AND revoked_at_ms IS NULL")
            .bind(tenant.as_str()).bind(id.as_str()).bind(credential).bind(to_i64(now_ms,"service credential revocation")?)
            .execute(&mut *tx).await.map_err(database_error)?;
        if changed.rows_affected() != 1 {
            return Err(HarnessError::conflict(
                "service credential is no longer available",
            ));
        }
        append_audit(
            &mut tx,
            tenant,
            Some(&actor.user_id),
            "user",
            "service_credential.revoke",
            "service_account",
            id.as_str(),
            "success",
            json!({"credential_id":credential}),
            now_ms,
        )
        .await?;
        tx.commit().await.map_err(database_error)
    }

    pub async fn authenticate_service_credential(
        &self,
        token: &str,
        tenant: &TenantId,
        now_ms: u64,
    ) -> Result<ServicePrincipal, HarnessError> {
        if !token.starts_with("ter_t_") || token.len() > 128 {
            return Err(invalid_credential());
        }
        tenant.validate()?;
        let mut tx = self.database.tenant_transaction(tenant).await?;
        let row = sqlx::query("SELECT a.service_account_id,u.username,u.status,c.credential_id,c.scopes,c.expires_at_ms FROM control_service_credentials c JOIN control_service_accounts a ON a.tenant_id=c.tenant_id AND a.service_account_id=c.service_account_id JOIN control_users u ON u.user_id=a.service_account_id WHERE c.tenant_id=$1 AND c.token_hash=$2 AND a.enabled=1 AND c.revoked_at_ms IS NULL AND c.expires_at_ms>$3")
            .bind(tenant.as_str()).bind(hex(&token_hash(token))).bind(to_i64(now_ms,"service authentication")?)
            .fetch_optional(&mut *tx).await.map_err(database_error)?.ok_or_else(invalid_credential)?;
        AccountStatus::parse(&row.try_get::<String, _>("status").map_err(database_error)?)?
            .require_active()?;
        let user = ControlUser {
            user_id: UserId::new(
                row.try_get::<String, _>("service_account_id")
                    .map_err(database_error)?,
            ),
            username: row.try_get("username").map_err(database_error)?,
        };
        let role =
            require_action(&mut tx, tenant, &user.user_id, ControlAction::TenantRead).await?;
        let instance = crate::identity_store::required_instance(&mut tx).await?;
        if instance.mode == InstanceMode::SingleUser {
            let role = require_action(
                &mut tx,
                tenant,
                &instance.owner_user_id,
                ControlAction::TenantRead,
            )
            .await?;
            if role != TenantRole::Owner {
                return Err(invalid_credential());
            }
        }
        let mut scopes: Vec<super::ServiceScope> =
            serde_json::from_str(&row.try_get::<String, _>("scopes").map_err(database_error)?)
                .map_err(|_| {
                    HarnessError::execution("stored service credential scopes are invalid")
                })?;
        if !role.allows(ControlAction::RunReserve) {
            scopes.retain(|scope| *scope != super::ServiceScope::RunExecute);
        }
        let credential_id: String = row.try_get("credential_id").map_err(database_error)?;
        let principal = ServicePrincipal {
            user,
            tenant_id: tenant.clone(),
            credential_id: credential_id.clone(),
            scopes,
            expires_at_ms: from_i64(
                row.try_get("expires_at_ms").map_err(database_error)?,
                "service credential expiry",
            )?,
        };
        sqlx::query("UPDATE control_service_credentials SET last_used_at_ms=$3 WHERE tenant_id=$1 AND credential_id=$2 AND (last_used_at_ms IS NULL OR last_used_at_ms<=$4)")
            .bind(tenant.as_str()).bind(credential_id).bind(to_i64(now_ms,"service activity")?).bind(to_i64(now_ms.saturating_sub(60_000),"service activity threshold")?)
            .execute(&mut *tx).await.map_err(database_error)?;
        tx.commit().await.map_err(database_error)?;
        Ok(principal)
    }
}

fn credential_record(row: &AnyRow) -> Result<ServiceCredential, HarnessError> {
    let time = |name| {
        row.try_get::<Option<i64>, _>(name)
            .map_err(database_error)?
            .map(|value| from_i64(value, "service credential timestamp"))
            .transpose()
    };
    Ok(ServiceCredential {
        credential_id: row.try_get("credential_id").map_err(database_error)?,
        name: row.try_get("name").map_err(database_error)?,
        scopes: serde_json::from_str(&row.try_get::<String, _>("scopes").map_err(database_error)?)
            .map_err(|_| HarnessError::execution("stored service credential scopes are invalid"))?,
        issued_at_ms: from_i64(
            row.try_get("issued_at_ms").map_err(database_error)?,
            "service credential issue",
        )?,
        expires_at_ms: from_i64(
            row.try_get("expires_at_ms").map_err(database_error)?,
            "service credential expiry",
        )?,
        last_used_at_ms: time("last_used_at_ms")?,
        revoked_at_ms: time("revoked_at_ms")?,
    })
}

fn invalid_credential() -> HarnessError {
    HarnessError::policy("service credential is invalid, expired or disabled")
}
