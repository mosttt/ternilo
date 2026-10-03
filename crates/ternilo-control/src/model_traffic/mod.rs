use crate::{
    ControlStore, ControlUser, PlatformAction, account_store::append_platform_audit,
    authorize_platform_in,
};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use ternilo_protocol::{HarnessError, UserId};
use ternilo_storage::{Database, Transaction, database_error, lock};

mod counts;
mod directory;
pub use directory::{ModelTrafficTarget, ModelTrafficTargetPage};
mod policy;

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ModelTrafficLimits {
    pub requests_per_minute: Option<u32>,
    pub max_concurrent_requests: Option<u32>,
}
impl ModelTrafficLimits {
    fn validate(&self) -> Result<(), HarnessError> {
        if self
            .requests_per_minute
            .is_some_and(|n| !(1..=1_000_000).contains(&n))
            || self
                .max_concurrent_requests
                .is_some_and(|n| !(1..=10_000).contains(&n))
        {
            return Err(HarnessError::invalid(
                "traffic limits require 1..1000000 requests per minute and 1..10000 concurrent requests, or null",
            ));
        }
        Ok(())
    }
    fn enabled(&self) -> bool {
        self.requests_per_minute.is_some() || self.max_concurrent_requests.is_some()
    }
}
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ModelTrafficPolicy {
    pub platform: ModelTrafficLimits,
    pub account_default: ModelTrafficLimits,
}
#[derive(Clone, Debug, Serialize)]
pub struct ModelTrafficPolicyRecord {
    pub revision: u64,
    pub policy: ModelTrafficPolicy,
}
#[derive(Clone, Debug, Serialize)]
pub struct AccountModelTraffic {
    pub user_id: UserId,
    pub username: String,
    pub revision: u64,
    pub limits: Option<ModelTrafficLimits>,
    pub effective: ModelTrafficLimits,
    pub platform: ModelTrafficLimits,
    pub recent_requests: u64,
    pub active_requests: u64,
}

pub(crate) async fn initialize(database: &Database) -> Result<(), HarnessError> {
    database
        .initialize(
            "model_traffic",
            1,
            include_str!("schema.sql"),
            include_str!("postgres.sql"),
        )
        .await
}

fn number(value: u64) -> Result<i64, HarnessError> {
    i64::try_from(value)
        .map_err(|_| HarnessError::invalid("traffic timestamp or revision is out of range"))
}
fn unsigned(value: i64) -> Result<u64, HarnessError> {
    u64::try_from(value).map_err(|_| HarnessError::execution("invalid model traffic value"))
}
fn json<T: Serialize>(value: &T) -> Result<String, HarnessError> {
    serde_json::to_string(value).map_err(|_| HarnessError::invalid("invalid model traffic policy"))
}

async fn current(tx: &mut Transaction) -> Result<ModelTrafficPolicyRecord, HarnessError> {
    let row = sqlx::query(
        "SELECT revision,policy_json FROM control_model_traffic_policy WHERE singleton=1",
    )
    .fetch_one(&mut **tx)
    .await
    .map_err(database_error)?;
    Ok(ModelTrafficPolicyRecord {
        revision: unsigned(row.try_get("revision").map_err(database_error)?)?,
        policy: serde_json::from_str(
            &row.try_get::<String, _>("policy_json")
                .map_err(database_error)?,
        )
        .map_err(|_| HarnessError::execution("invalid stored model traffic policy"))?,
    })
}
async fn account_override(
    tx: &mut Transaction,
    user: &UserId,
) -> Result<(u64, Option<ModelTrafficLimits>), HarnessError> {
    let row = sqlx::query(
        "SELECT revision,limits_json FROM control_model_traffic_accounts WHERE user_id=$1",
    )
    .bind(user.as_str())
    .fetch_optional(&mut **tx)
    .await
    .map_err(database_error)?;
    let Some(row) = row else {
        return Ok((0, None));
    };
    Ok((
        unsigned(row.try_get("revision").map_err(database_error)?)?,
        row.try_get::<Option<String>, _>("limits_json")
            .map_err(database_error)?
            .map(|value| {
                serde_json::from_str(&value)
                    .map_err(|_| HarnessError::execution("invalid stored account model limits"))
            })
            .transpose()?,
    ))
}

pub(crate) async fn check_admission(
    tx: &mut Transaction,
    actor: &UserId,
    now: u64,
) -> Result<(), crate::ModelAccessError> {
    crate::model_store::scope(tx).await?;
    // Policy edits wait for admitted transactions; unrelated accounts can enter concurrently.
    // SQLite's write transaction already provides this serialization.
    if ternilo_storage::backend(tx) == ternilo_storage::Backend::Postgres {
        sqlx::query(
            "SELECT pg_advisory_xact_lock_shared(hashtextextended('ternilo:model-traffic',0))",
        )
        .execute(&mut **tx)
        .await
        .map_err(database_error)?;
    }
    let policy = current(tx).await?.policy;
    let (_, account) = account_override(tx, actor).await?;
    let account = account.as_ref().unwrap_or(&policy.account_default);
    if !policy.platform.enabled() && !account.enabled() {
        return Ok(());
    }
    if policy.platform.enabled() {
        lock(tx, "ternilo:model-traffic-platform").await?;
    }
    if account.enabled() {
        lock(tx, &format!("ternilo:model-traffic-account:{actor}")).await?;
    }
    let usage = counts::read(tx, actor, now, policy.platform.enabled()).await?;
    check(
        &policy.platform,
        usage.global_requests,
        usage.global_active,
        usage.global_oldest,
        now,
        "platform",
    )?;
    check(
        account,
        usage.account_requests,
        usage.account_active,
        usage.account_oldest,
        now,
        "account",
    )
}

fn check(
    limits: &ModelTrafficLimits,
    recent: u64,
    active: u64,
    oldest: Option<u64>,
    now: u64,
    scope: &str,
) -> Result<(), crate::ModelAccessError> {
    let limited = |message: String, retry_after_seconds| crate::ModelAccessError {
        kind: crate::ModelAccessErrorKind::RateLimited {
            retry_after_seconds,
        },
        error: HarnessError::policy(message),
    };
    if limits
        .requests_per_minute
        .is_some_and(|maximum| recent >= u64::from(maximum))
    {
        let retry = oldest
            .unwrap_or(now)
            .saturating_add(60_000)
            .saturating_sub(now)
            .div_ceil(1000)
            .clamp(1, 60);
        return Err(limited(
            format!("{scope} model request rate limit reached; retry after {retry} seconds"),
            retry,
        ));
    }
    if limits
        .max_concurrent_requests
        .is_some_and(|maximum| active >= u64::from(maximum))
    {
        return Err(limited(
            format!(
                "{scope} concurrent model request limit reached; wait for an active request to finish"
            ),
            1,
        ));
    }
    Ok(())
}
