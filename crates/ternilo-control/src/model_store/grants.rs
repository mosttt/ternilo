use serde_json::json;
use sqlx::{Row, any::AnyRow};
use ternilo_protocol::HarnessError;
use ternilo_storage::{Transaction, database_error, lock};

use super::{
    ControlStore, ModelEntitlement, ModelEntitlementPage, ModelGrantInput, ModelGrantPage,
    ModelGrantRecord, ModelGrantSubject, ModelQuotaSnapshot, PlatformAction, PublicModel, UserId,
    config, groups, month_at, number, read_number, read_optional_number, require_account,
    subject_from_row, subject_parts, unsigned, validate_ids, validate_text,
};
use crate::{
    ControlUser, PageQuery, account_store::append_platform_audit, crypto::random_identifier,
};

impl ControlStore {
    pub async fn list_model_grants(
        &self,
        actor: &ControlUser,
        query: &PageQuery,
        now_ms: u64,
    ) -> Result<ModelGrantPage, HarnessError> {
        let (pattern, cursor) = query.parameters()?;
        let mut tx = self
            .model_admin_transaction(actor, PlatformAction::ModelsRead)
            .await?;
        let rows=sqlx::query("SELECT g.*,CASE WHEN g.subject_kind='group' THEN (SELECT name FROM control_model_groups WHERE group_id=g.subject_id) ELSE (SELECT username FROM control_users WHERE user_id=g.subject_id) END AS subject_name FROM control_model_grants g WHERE (CAST($1 AS TEXT) IS NULL OR LOWER(g.name) LIKE $1 ESCAPE '!' OR LOWER(g.grant_id) LIKE $1 ESCAPE '!') AND (CAST($2 AS TEXT) IS NULL OR g.grant_id>$2) ORDER BY g.grant_id LIMIT $3")
            .bind(pattern).bind(cursor).bind(i64::from(query.limit)+1).fetch_all(&mut *tx).await.map_err(database_error)?;
        let mut grants = Vec::with_capacity(rows.len());
        for row in rows {
            grants.push(grant_from_row(&mut tx, &row, now_ms).await?);
        }
        let next_cursor = query.finish(&mut grants, |grant| grant.grant_id.clone());
        tx.commit().await.map_err(database_error)?;
        Ok(ModelGrantPage {
            grants,
            next_cursor,
        })
    }

    pub async fn get_model_grant(
        &self,
        actor: &ControlUser,
        id: &str,
        now_ms: u64,
    ) -> Result<ModelGrantRecord, HarnessError> {
        let mut tx = self
            .model_admin_transaction(actor, PlatformAction::ModelsRead)
            .await?;
        let grant = grant_in(&mut tx, id, now_ms).await?;
        tx.commit().await.map_err(database_error)?;
        Ok(grant)
    }

    pub async fn save_model_grant(
        &self,
        actor: &ControlUser,
        id: Option<&str>,
        input: &ModelGrantInput,
        now_ms: u64,
    ) -> Result<ModelGrantRecord, HarnessError> {
        validate_text(&input.name, "model grant name", 120)?;
        validate_ids(&input.model_ids)?;
        if input.monthly_tokens == 0
            || input.max_concurrent_requests == 0
            || input.max_concurrent_requests > 10_000
        {
            return Err(HarnessError::invalid(
                "model grant requires a positive token limit and 1 to 10000 concurrent requests",
            ));
        }
        if input.expires_at_ms.is_some_and(|expires| expires <= now_ms) {
            return Err(HarnessError::invalid(
                "model grant expiration must be in the future",
            ));
        }
        let generated = random_identifier("mgr");
        let grant_id = id.unwrap_or(&generated);
        let (kind, subject) = subject_parts(&input.subject);
        let mut tx = self
            .model_admin_transaction(actor, PlatformAction::ModelGrantsManage)
            .await?;
        if kind == "group" {
            lock(&mut tx, &format!("ternilo:model-group:{subject}")).await?;
        }
        lock(&mut tx, &format!("ternilo:model-grant:{grant_id}")).await?;
        if id.is_some() {
            let old = grant_in(&mut tx, grant_id, now_ms).await?;
            if old.subject != input.subject {
                return Err(HarnessError::invalid(
                    "a model grant cannot change its budget recipient; create another grant",
                ));
            }
            if old.revoked_at_ms.is_some() {
                return Err(HarnessError::policy(
                    "a revoked model grant cannot be restored",
                ));
            }
        }
        match &input.subject {
            ModelGrantSubject::Group { id } => {
                groups::group_in(&mut tx, id).await?;
            }
            ModelGrantSubject::User { id } => {
                UserId::new(id).validate()?;
                let exists: i64 = sqlx::query_scalar(
                    "SELECT CAST(EXISTS(SELECT 1 FROM control_users WHERE user_id=$1) AS INTEGER)",
                )
                .bind(id)
                .fetch_one(&mut *tx)
                .await
                .map_err(database_error)?;
                if exists == 0 {
                    return Err(HarnessError::invalid("model grant account does not exist"));
                }
            }
        }
        for model in &input.model_ids {
            config::publication_in(&mut tx, model).await?;
        }
        sqlx::query("INSERT INTO control_model_grants(grant_id,name,subject_kind,subject_id,monthly_tokens,max_concurrent_requests,expires_at_ms,allow_resource_sharing,created_at_ms,updated_at_ms) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$9) ON CONFLICT(grant_id) DO UPDATE SET name=EXCLUDED.name,monthly_tokens=EXCLUDED.monthly_tokens,max_concurrent_requests=EXCLUDED.max_concurrent_requests,expires_at_ms=EXCLUDED.expires_at_ms,allow_resource_sharing=EXCLUDED.allow_resource_sharing,updated_at_ms=EXCLUDED.updated_at_ms")
            .bind(grant_id).bind(input.name.trim()).bind(kind).bind(subject).bind(number(input.monthly_tokens)?).bind(i64::from(input.max_concurrent_requests)).bind(input.expires_at_ms.map(number).transpose()?).bind(i64::from(input.allow_resource_sharing)).bind(number(now_ms)?).execute(&mut *tx).await.map_err(database_error)?;
        sqlx::query("DELETE FROM control_model_grant_models WHERE grant_id=$1")
            .bind(grant_id)
            .execute(&mut *tx)
            .await
            .map_err(database_error)?;
        for model in &input.model_ids {
            sqlx::query("INSERT INTO control_model_grant_models(grant_id,model_id) VALUES($1,$2)")
                .bind(grant_id)
                .bind(model)
                .execute(&mut *tx)
                .await
                .map_err(database_error)?;
        }
        append_platform_audit(&mut tx,&actor.user_id,"model.grant.save",grant_id,json!({"subject":input.subject,"models":input.model_ids,"monthly_tokens":input.monthly_tokens,"max_concurrent_requests":input.max_concurrent_requests,"allow_resource_sharing":input.allow_resource_sharing}),now_ms).await?;
        let value = grant_in(&mut tx, grant_id, now_ms).await?;
        tx.commit().await.map_err(database_error)?;
        Ok(value)
    }

    pub async fn revoke_model_grant(
        &self,
        actor: &ControlUser,
        id: &str,
        now_ms: u64,
    ) -> Result<(), HarnessError> {
        let mut tx = self
            .model_admin_transaction(actor, PlatformAction::ModelGrantsManage)
            .await?;
        lock(&mut tx, &format!("ternilo:model-grant:{id}")).await?;
        grant_in(&mut tx, id, now_ms).await?;
        sqlx::query("UPDATE control_model_grants SET revoked_at_ms=COALESCE(revoked_at_ms,$2),updated_at_ms=$2 WHERE grant_id=$1").bind(id).bind(number(now_ms)?).execute(&mut *tx).await.map_err(database_error)?;
        append_platform_audit(
            &mut tx,
            &actor.user_id,
            "model.grant.revoke",
            id,
            json!({}),
            now_ms,
        )
        .await?;
        tx.commit().await.map_err(database_error)
    }

    pub async fn list_model_entitlements(
        &self,
        actor: &ControlUser,
        query: &PageQuery,
        now_ms: u64,
    ) -> Result<ModelEntitlementPage, HarnessError> {
        let mut tx = self.model_transaction().await?;
        require_account(&mut tx, &actor.user_id).await?;
        let page = entitlements_for_user_in(&mut tx, &actor.user_id, None, query, now_ms).await?;
        tx.commit().await.map_err(database_error)?;
        Ok(page)
    }
}

pub(super) async fn grant_in(
    tx: &mut Transaction,
    id: &str,
    now_ms: u64,
) -> Result<ModelGrantRecord, HarnessError> {
    let row=sqlx::query("SELECT g.*,CASE WHEN g.subject_kind='group' THEN (SELECT name FROM control_model_groups WHERE group_id=g.subject_id) ELSE (SELECT username FROM control_users WHERE user_id=g.subject_id) END AS subject_name FROM control_model_grants g WHERE grant_id=$1")
        .bind(id).fetch_optional(&mut **tx).await.map_err(database_error)?.ok_or_else(||HarnessError::policy("model grant does not exist"))?;
    grant_from_row(tx, &row, now_ms).await
}

async fn grant_from_row(
    tx: &mut Transaction,
    row: &AnyRow,
    now_ms: u64,
) -> Result<ModelGrantRecord, HarnessError> {
    let grant_id: String = row.try_get("grant_id").map_err(database_error)?;
    let model_ids = sqlx::query_scalar(
        "SELECT model_id FROM control_model_grant_models WHERE grant_id=$1 ORDER BY model_id",
    )
    .bind(&grant_id)
    .fetch_all(&mut **tx)
    .await
    .map_err(database_error)?;
    let limit = read_number(row, "monthly_tokens")?;
    let max = u32::try_from(read_number(row, "max_concurrent_requests")?)
        .map_err(|_| HarnessError::execution("stored model concurrency exceeds u32"))?;
    let quota = quota_in(tx, &grant_id, None, limit, max, now_ms).await?;
    Ok(ModelGrantRecord {
        grant_id,
        name: row.try_get("name").map_err(database_error)?,
        subject: subject_from_row(row)?,
        subject_name: row.try_get("subject_name").map_err(database_error)?,
        model_ids,
        quota,
        expires_at_ms: read_optional_number(row, "expires_at_ms")?,
        allow_resource_sharing: row
            .try_get::<i64, _>("allow_resource_sharing")
            .map_err(database_error)?
            != 0,
        revoked_at_ms: read_optional_number(row, "revoked_at_ms")?,
        created_at_ms: read_number(row, "created_at_ms")?,
        updated_at_ms: read_number(row, "updated_at_ms")?,
    })
}

pub(super) async fn quota_in(
    tx: &mut Transaction,
    grant_id: &str,
    key_id: Option<&str>,
    limit: u64,
    max: u32,
    now_ms: u64,
) -> Result<ModelQuotaSnapshot, HarnessError> {
    quota_for_month_in(tx, grant_id, key_id, limit, max, &month_at(now_ms)?, now_ms).await
}

pub(super) async fn quota_for_month_in(
    tx: &mut Transaction,
    grant_id: &str,
    key_id: Option<&str>,
    limit: u64,
    max: u32,
    month: &str,
    now_ms: u64,
) -> Result<ModelQuotaSnapshot, HarnessError> {
    let row=sqlx::query("SELECT CAST(COALESCE(SUM(a.accounted_tokens),0) AS BIGINT) AS used_tokens,CAST(COALESCE(SUM(CASE WHEN a.accounted_tokens IS NULL THEN a.reserved_tokens ELSE 0 END),0) AS BIGINT) AS reserved_tokens FROM control_model_attempts a JOIN control_model_requests r ON r.request_id=a.request_id WHERE r.grant_id=$1 AND (CAST($2 AS TEXT) IS NULL OR r.key_id=$2) AND r.month=$3")
        .bind(grant_id).bind(key_id).bind(month).fetch_one(&mut **tx).await.map_err(database_error)?;
    let active:i64=sqlx::query_scalar("SELECT COUNT(*) FROM control_model_requests WHERE grant_id=$1 AND (CAST($2 AS TEXT) IS NULL OR key_id=$2) AND state='pending' AND expires_at_ms>$3")
        .bind(grant_id).bind(key_id).bind(number(now_ms)?).fetch_one(&mut **tx).await.map_err(database_error)?;
    Ok(ModelQuotaSnapshot {
        month: month.to_owned(),
        limit_tokens: limit,
        used_tokens: read_number(&row, "used_tokens")?,
        reserved_tokens: read_number(&row, "reserved_tokens")?,
        active_requests: unsigned(active)?,
        max_concurrent_requests: max,
    })
}

pub(super) async fn grant_models_in(
    tx: &mut Transaction,
    id: &str,
) -> Result<Vec<PublicModel>, HarnessError> {
    let rows=sqlx::query("SELECT p.*,r.profile_json,r.enabled AS provider_enabled FROM control_model_grant_models g JOIN control_model_publications p ON p.model_id=g.model_id JOIN control_model_providers r ON r.provider_id=p.provider_id WHERE g.grant_id=$1 AND p.enabled=1 AND r.enabled=1 ORDER BY p.model_id")
        .bind(id).fetch_all(&mut **tx).await.map_err(database_error)?;
    rows.iter()
        .map(|row| config::publication_from_row(row).map(|value| value.model))
        .collect()
}

pub(super) async fn require_grant(
    tx: &mut Transaction,
    user_id: &UserId,
    grant: &ModelGrantRecord,
    now_ms: u64,
) -> Result<(), HarnessError> {
    require_account(tx, user_id).await?;
    require_grant_for_workload(tx, user_id, grant, now_ms).await
}

pub(super) async fn require_grant_for_workload(
    tx: &mut Transaction,
    user_id: &UserId,
    grant: &ModelGrantRecord,
    now_ms: u64,
) -> Result<(), HarnessError> {
    super::workloads::require_existing_account(tx, user_id).await?;
    if grant.revoked_at_ms.is_some() || grant.expires_at_ms.is_some_and(|expires| expires <= now_ms)
    {
        return Err(HarnessError::policy("model grant was revoked or expired"));
    }
    match &grant.subject {
        ModelGrantSubject::User { id } if id == user_id.as_str() => Ok(()),
        ModelGrantSubject::Group { id } => {
            let member:i64=sqlx::query_scalar("SELECT CAST(EXISTS(SELECT 1 FROM control_model_group_members m JOIN control_model_groups g ON g.group_id=m.group_id WHERE m.group_id=$1 AND m.user_id=$2 AND g.deleted_at_ms IS NULL) AS INTEGER)")
                .bind(id).bind(user_id.as_str()).fetch_one(&mut **tx).await.map_err(database_error)?;
            if member != 0 {
                Ok(())
            } else {
                Err(HarnessError::policy(
                    "account is no longer a member of this model group",
                ))
            }
        }
        ModelGrantSubject::User { .. } => Err(HarnessError::policy(
            "model grant does not belong to this account",
        )),
    }
}

pub(super) async fn entitlements_for_user_in(
    tx: &mut Transaction,
    user: &UserId,
    actor: Option<&UserId>,
    query: &PageQuery,
    now: u64,
) -> Result<ModelEntitlementPage, HarnessError> {
    let (pattern, cursor) = query.parameters()?;
    super::workloads::require_existing_account(tx, user).await?;
    let sharing = actor.is_some_and(|actor| actor != user);
    let rows=sqlx::query("SELECT g.*,CASE WHEN g.subject_kind='group' THEN (SELECT name FROM control_model_groups WHERE group_id=g.subject_id) ELSE (SELECT username FROM control_users WHERE user_id=g.subject_id) END AS subject_name FROM control_model_grants g WHERE revoked_at_ms IS NULL AND (expires_at_ms IS NULL OR expires_at_ms>$2) AND ((subject_kind='user' AND subject_id=$1) OR (subject_kind='group' AND EXISTS(SELECT 1 FROM control_model_group_members m JOIN control_model_groups mg ON mg.group_id=m.group_id WHERE m.group_id=g.subject_id AND m.user_id=$1 AND mg.deleted_at_ms IS NULL))) AND ($3=0 OR allow_resource_sharing=1) AND (CAST($4 AS TEXT) IS NULL OR LOWER(g.name) LIKE $4 ESCAPE '!' OR EXISTS(SELECT 1 FROM control_model_grant_models AS assigned JOIN control_model_publications AS model ON model.model_id=assigned.model_id WHERE assigned.grant_id=g.grant_id AND (LOWER(model.model_id) LIKE $4 ESCAPE '!' OR LOWER(model.display_name) LIKE $4 ESCAPE '!'))) AND (CAST($5 AS TEXT) IS NULL OR g.grant_id>$5) ORDER BY g.grant_id LIMIT $6")
        .bind(user.as_str()).bind(number(now)?).bind(i64::from(sharing)).bind(pattern).bind(cursor).bind(i64::from(query.limit)+1).fetch_all(&mut **tx).await.map_err(database_error)?;
    let mut entitlements = Vec::with_capacity(rows.len());
    for row in rows {
        let grant = grant_from_row(tx, &row, now).await?;
        let mut models = grant_models_in(tx, &grant.grant_id).await?;
        if let Some(search) = query
            .query
            .as_ref()
            .map(|value| value.trim().to_lowercase())
            && !grant.name.to_lowercase().contains(&search)
        {
            models.retain(|model| {
                model.model_id.to_lowercase().contains(&search)
                    || model.display_name.to_lowercase().contains(&search)
            });
        }
        entitlements.push(ModelEntitlement { grant, models });
    }
    let next_cursor = query.finish(&mut entitlements, |value| value.grant.grant_id.clone());
    Ok(ModelEntitlementPage {
        entitlements,
        next_cursor,
    })
}
