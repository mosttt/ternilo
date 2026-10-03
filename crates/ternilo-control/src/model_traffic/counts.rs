use super::{HarnessError, Row, Transaction, UserId, database_error, number, unsigned};
use ternilo_storage::Backend;

#[derive(Default)]
pub(super) struct Counts {
    pub global_requests: u64,
    pub account_requests: u64,
    pub global_active: u64,
    pub account_active: u64,
    pub global_oldest: Option<u64>,
    pub account_oldest: Option<u64>,
}

fn statement(table: &'static str, active: &'static str, global: bool) -> String {
    let actor_filter = if global { "" } else { "AND actor_user_id=$1" };
    format!("SELECT
        COUNT(CASE WHEN created_at_ms >= $2 THEN 1 END) AS global_requests,
        COUNT(CASE WHEN created_at_ms >= $2 AND actor_user_id=$1 THEN 1 END) AS account_requests,
        COUNT(CASE WHEN state='pending' AND {active} THEN 1 END) AS global_active,
        COUNT(CASE WHEN state='pending' AND {active} AND actor_user_id=$1 THEN 1 END) AS account_active,
        MIN(CASE WHEN created_at_ms >= $2 THEN created_at_ms END) AS global_oldest,
        MIN(CASE WHEN created_at_ms >= $2 AND actor_user_id=$1 THEN created_at_ms END) AS account_oldest
        FROM {table} WHERE (created_at_ms >= $2 OR (state='pending' AND {active})) {actor_filter}")
}

pub(super) async fn read(
    tx: &mut Transaction,
    actor: &UserId,
    now: u64,
    global: bool,
) -> Result<Counts, HarnessError> {
    let since = number(now.saturating_sub(59_999))?;
    let query = statement("control_model_requests", "expires_at_ms>$3", global);
    let row = sqlx::query(sqlx::AssertSqlSafe(query))
        .bind(actor.as_str())
        .bind(since)
        .bind(number(now)?)
        .fetch_one(&mut **tx)
        .await
        .map_err(database_error)?;
    let mut result = from_row(&row)?;
    let query = match ternilo_storage::backend(tx) {
        Backend::Postgres => {
            "SELECT * FROM ternilo_computer_traffic_counts($1,$2,$3,$4)".to_owned()
        }
        Backend::Sqlite => statement(
            "control_computer_model_requests",
            "updated_at_ms>=$3",
            global,
        ),
    };
    let mut query = sqlx::query(sqlx::AssertSqlSafe(query))
        .bind(actor.as_str())
        .bind(since)
        .bind(number(now.saturating_sub(60_000))?);
    if ternilo_storage::backend(tx) == Backend::Postgres {
        query = query.bind(global);
    }
    let row = query.fetch_one(&mut **tx).await.map_err(database_error)?;
    let computer = from_row(&row)?;
    for (target, other) in [
        (&mut result.global_requests, computer.global_requests),
        (&mut result.account_requests, computer.account_requests),
        (&mut result.global_active, computer.global_active),
        (&mut result.account_active, computer.account_active),
    ] {
        *target = target
            .checked_add(other)
            .ok_or_else(|| HarnessError::execution("model traffic count overflow"))?;
    }
    result.global_oldest = result
        .global_oldest
        .into_iter()
        .chain(computer.global_oldest)
        .min();
    result.account_oldest = result
        .account_oldest
        .into_iter()
        .chain(computer.account_oldest)
        .min();
    Ok(result)
}
fn from_row(row: &sqlx::any::AnyRow) -> Result<Counts, HarnessError> {
    let count = |name: &str| unsigned(row.try_get(name).map_err(database_error)?);
    let oldest = |name: &str| {
        row.try_get::<Option<i64>, _>(name)
            .map_err(database_error)?
            .map(unsigned)
            .transpose()
    };
    Ok(Counts {
        global_requests: count("global_requests")?,
        account_requests: count("account_requests")?,
        global_active: count("global_active")?,
        account_active: count("account_active")?,
        global_oldest: oldest("global_oldest")?,
        account_oldest: oldest("account_oldest")?,
    })
}
