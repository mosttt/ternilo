use super::{
    AccountModelTraffic, ControlStore, ControlUser, HarnessError, ModelTrafficLimits,
    ModelTrafficPolicy, ModelTrafficPolicyRecord, PlatformAction, UserId, account_override,
    append_platform_audit, authorize_platform_in, counts, current, database_error, json, lock,
    number,
};

impl ControlStore {
    pub async fn model_traffic_policy(
        &self,
        actor: &ControlUser,
    ) -> Result<ModelTrafficPolicyRecord, HarnessError> {
        let mut tx = self.database.begin_read().await?;
        authorize_platform_in(&mut tx, &actor.user_id, PlatformAction::ModelsRead).await?;
        crate::model_store::scope(&mut tx).await?;
        let value = current(&mut tx).await?;
        tx.commit().await.map_err(database_error)?;
        Ok(value)
    }

    pub async fn update_model_traffic_policy(
        &self,
        actor: &ControlUser,
        expected_revision: u64,
        policy: &ModelTrafficPolicy,
        now: u64,
    ) -> Result<ModelTrafficPolicyRecord, HarnessError> {
        policy.platform.validate()?;
        policy.account_default.validate()?;
        let mut tx = self.database.begin().await?;
        authorize_platform_in(&mut tx, &actor.user_id, PlatformAction::ModelsManage).await?;
        crate::model_store::scope(&mut tx).await?;
        lock(&mut tx, "ternilo:model-traffic").await?;
        let previous = current(&mut tx).await?;
        if previous.revision != expected_revision {
            return Err(HarnessError::conflict(
                "model traffic policy changed; reload before saving",
            ));
        }
        let revision = previous
            .revision
            .checked_add(1)
            .ok_or_else(|| HarnessError::execution("model traffic policy revision overflow"))?;
        sqlx::query(
            "UPDATE control_model_traffic_policy SET revision=$1,policy_json=$2 WHERE singleton=1",
        )
        .bind(number(revision)?)
        .bind(json(policy)?)
        .execute(&mut *tx)
        .await
        .map_err(database_error)?;
        append_platform_audit(
            &mut tx,
            &actor.user_id,
            "model.traffic.policy.update",
            "instance",
            serde_json::json!({"previous":previous.policy,"policy":policy,"revision":revision}),
            now,
        )
        .await?;
        tx.commit().await.map_err(database_error)?;
        Ok(ModelTrafficPolicyRecord {
            revision,
            policy: policy.clone(),
        })
    }

    pub async fn account_model_traffic(
        &self,
        actor: &ControlUser,
        target: &UserId,
        now: u64,
    ) -> Result<AccountModelTraffic, HarnessError> {
        target.validate()?;
        let mut tx = self.database.begin_read().await?;
        if target == &actor.user_id {
            crate::account_store::require_active_account_in(&mut tx, &actor.user_id).await?;
        } else {
            authorize_platform_in(&mut tx, &actor.user_id, PlatformAction::AccountsRead).await?;
        }
        crate::model_store::scope(&mut tx).await?;
        let username: String = sqlx::query_scalar(
            "SELECT username FROM control_users WHERE user_id=$1 AND status<>'removed'",
        )
        .bind(target.as_str())
        .fetch_optional(&mut *tx)
        .await
        .map_err(database_error)?
        .ok_or_else(|| HarnessError::invalid("account does not exist"))?;
        let (revision, limits) = account_override(&mut tx, target).await?;
        let policy = current(&mut tx).await?.policy;
        let counts = counts::read(&mut tx, target, now, false).await?;
        let effective = limits.as_ref().unwrap_or(&policy.account_default).clone();
        tx.commit().await.map_err(database_error)?;
        Ok(AccountModelTraffic {
            user_id: target.clone(),
            username,
            revision,
            limits,
            effective,
            platform: policy.platform,
            recent_requests: counts.account_requests,
            active_requests: counts.account_active,
        })
    }

    pub async fn update_account_model_traffic(
        &self,
        actor: &ControlUser,
        target: &UserId,
        expected_revision: u64,
        limits: Option<&ModelTrafficLimits>,
        now: u64,
    ) -> Result<(), HarnessError> {
        target.validate()?;
        if let Some(limits) = limits {
            limits.validate()?;
        }
        let mut tx = self.database.begin().await?;
        authorize_platform_in(&mut tx, &actor.user_id, PlatformAction::AccountsManage).await?;
        crate::model_store::scope(&mut tx).await?;
        lock(&mut tx, "ternilo:model-traffic").await?;
        let exists: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM control_users WHERE user_id=$1 AND status<>'removed'",
        )
        .bind(target.as_str())
        .fetch_one(&mut *tx)
        .await
        .map_err(database_error)?;
        if exists != 1 {
            return Err(HarnessError::invalid("account does not exist"));
        }
        let (previous_revision, previous) = account_override(&mut tx, target).await?;
        if previous_revision != expected_revision {
            return Err(HarnessError::conflict(
                "account model limits changed; reload before saving",
            ));
        }
        let revision = previous_revision
            .checked_add(1)
            .ok_or_else(|| HarnessError::execution("account model limits revision overflow"))?;
        sqlx::query("INSERT INTO control_model_traffic_accounts(user_id,revision,limits_json) VALUES($1,$2,$3) ON CONFLICT(user_id) DO UPDATE SET revision=excluded.revision,limits_json=excluded.limits_json").bind(target.as_str()).bind(number(revision)?).bind(limits.map(json).transpose()?).execute(&mut *tx).await.map_err(database_error)?;
        append_platform_audit(
            &mut tx,
            &actor.user_id,
            "model.traffic.account.update",
            target.as_str(),
            serde_json::json!({"previous":previous,"limits":limits,"revision":revision}),
            now,
        )
        .await?;
        tx.commit().await.map_err(database_error)
    }
}
