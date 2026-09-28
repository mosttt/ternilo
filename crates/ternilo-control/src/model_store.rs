use chrono::{DateTime, Utc};
use serde::{Serialize, de::DeserializeOwned};
use sqlx::{Row, any::AnyRow};
use ternilo_protocol::{HarnessError, UserId};
use ternilo_storage::{Backend, Transaction, database_error};

use crate::{ControlStore, PlatformAction, authorize_platform_in};

mod budgets;
mod config;
mod provider_rotation;
pub use provider_rotation::ModelProviderKeyRotation;
mod devices;
mod grants;
mod groups;
mod keys;
mod ledger;
mod nodes;
mod reconciliation;
mod requests;
pub use reconciliation::{
    ModelUsageReconciliation, ModelUsageReconciliationInput, ModelUsageReconciliationResult,
};
mod types;
mod workloads;
pub use nodes::NodeModelPrincipal;

pub(crate) use budgets::{lock_budget, refresh_period_in};
pub(crate) use config::rotate_model_credentials;
pub use types::*;

pub(crate) async fn scope(tx: &mut Transaction) -> Result<(), HarnessError> {
    if ternilo_storage::backend(tx) == Backend::Postgres {
        sqlx::query("SELECT set_config('ternilo.model_service', 'on', true)")
            .execute(&mut **tx)
            .await
            .map_err(database_error)?;
    }
    Ok(())
}

impl ControlStore {
    async fn model_transaction(&self) -> Result<Transaction, HarnessError> {
        let mut tx = self.database.begin().await?;
        scope(&mut tx).await?;
        Ok(tx)
    }

    async fn model_admin_transaction(
        &self,
        actor: &crate::ControlUser,
        action: PlatformAction,
    ) -> Result<Transaction, HarnessError> {
        let mut tx = self.model_transaction().await?;
        authorize_platform_in(&mut tx, &actor.user_id, action).await?;
        Ok(tx)
    }
}

fn number(value: u64) -> Result<i64, HarnessError> {
    i64::try_from(value).map_err(|_| HarnessError::invalid("model value exceeds database range"))
}

fn unsigned(value: i64) -> Result<u64, HarnessError> {
    u64::try_from(value).map_err(|_| HarnessError::execution("stored model value is negative"))
}

fn read_number(row: &AnyRow, name: &str) -> Result<u64, HarnessError> {
    unsigned(row.try_get(name).map_err(database_error)?)
}

fn read_optional_number(row: &AnyRow, name: &str) -> Result<Option<u64>, HarnessError> {
    row.try_get::<Option<i64>, _>(name)
        .map_err(database_error)?
        .map(unsigned)
        .transpose()
}

fn json_text(value: &impl Serialize) -> Result<String, HarnessError> {
    serde_json::to_string(value).map_err(|error| HarnessError::execution(error.to_string()))
}

fn from_json<T: DeserializeOwned>(text: &str) -> Result<T, HarnessError> {
    serde_json::from_str(text)
        .map_err(|error| HarnessError::execution(format!("stored model JSON is invalid: {error}")))
}

fn month_at(now_ms: u64) -> Result<String, HarnessError> {
    DateTime::<Utc>::from_timestamp_millis(number(now_ms)?)
        .map(|time| time.format("%Y-%m").to_string())
        .ok_or_else(|| HarnessError::invalid("model timestamp is out of range"))
}

fn validate_text(value: &str, name: &str, max: usize) -> Result<(), HarnessError> {
    if value.trim().is_empty() || value.chars().count() > max || value.chars().any(char::is_control)
    {
        return Err(HarnessError::invalid(format!(
            "{name} must contain 1 to {max} characters without control characters"
        )));
    }
    Ok(())
}

fn validate_ids(ids: &[String]) -> Result<(), HarnessError> {
    if ids.is_empty() || ids.len() > 100 {
        return Err(HarnessError::invalid(
            "model scope must contain 1 to 100 models",
        ));
    }
    let mut seen = std::collections::BTreeSet::new();
    for id in ids {
        validate_text(id, "model ID", 128)?;
        if !seen.insert(id) {
            return Err(HarnessError::invalid("model scope contains duplicate IDs"));
        }
    }
    Ok(())
}

fn subject_parts(subject: &ModelGrantSubject) -> (&'static str, &str) {
    match subject {
        ModelGrantSubject::User { id } => ("user", id),
        ModelGrantSubject::Group { id } => ("group", id),
    }
}

fn subject_from_row(row: &AnyRow) -> Result<ModelGrantSubject, HarnessError> {
    let id = row.try_get("subject_id").map_err(database_error)?;
    match row
        .try_get::<String, _>("subject_kind")
        .map_err(database_error)?
        .as_str()
    {
        "user" => Ok(ModelGrantSubject::User { id }),
        "group" => Ok(ModelGrantSubject::Group { id }),
        _ => Err(HarnessError::execution(
            "stored model grant subject is invalid",
        )),
    }
}

async fn require_account(tx: &mut Transaction, user: &UserId) -> Result<(), HarnessError> {
    crate::account_store::platform_role_in(tx, user)
        .await
        .map(|_| ())
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod workload_tests;
