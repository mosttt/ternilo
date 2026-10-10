use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use ternilo_kernel::SessionProjectionUnit;
use ternilo_protocol::{
    HarnessError, RunId, SessionEvent, SessionId, SessionStats, TenantId, UserId,
};
use ternilo_storage::{Json, Transaction, database_error};

use crate::{CloudStore, store::lock_session_in};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StatsCheckpoint {
    version: u32,
    through_seq: i64,
    state: Value,
    discarded_runs: BTreeSet<RunId>,
}

impl CloudStore {
    pub async fn session_stats_as(
        &self,
        tenant: &TenantId,
        actor: &UserId,
        session: &SessionId,
    ) -> Result<SessionStats, HarnessError> {
        tenant.validate()?;
        actor.validate()?;
        session.validate()?;
        let mut tx = self.tenant_transaction(tenant).await?;
        crate::sharing::session_owner_in(
            &mut tx,
            tenant,
            actor,
            session,
            ternilo_control::ResourceAction::View,
        )
        .await?;
        let last = lock_session_in(&mut tx, tenant, session).await?;
        let unit = ternilo_builtins::stats_unit();
        let cached = checkpoint_in(&mut tx, tenant, session, unit.as_ref(), last).await?;
        if let Some(cached) = &cached
            && cached.through_seq == last
        {
            let result = decode_stats(unit.view(&cached.state)?)?;
            tx.commit().await.map_err(database_error)?;
            return Ok(result);
        }
        let mut events = events_after(
            &mut tx,
            tenant,
            session,
            cached.as_ref().map_or(-1, |c| c.through_seq),
        )
        .await?;
        let replaced = events
            .iter()
            .any(|event| event.regeneration_target().is_some());
        if replaced && cached.is_some() {
            events = events_after(&mut tx, tenant, session, -1).await?;
        }
        let mut checkpoint = cached
            .filter(|_| !replaced)
            .unwrap_or_else(|| StatsCheckpoint {
                version: unit.version(),
                through_seq: -1,
                state: unit.initial(),
                discarded_runs: BTreeSet::new(),
            });
        let active = ternilo_protocol::conversation_events(&events);
        if replaced {
            let retained: BTreeSet<_> = active.iter().map(|event| event.seq).collect();
            checkpoint.discarded_runs.extend(
                events
                    .iter()
                    .filter(|event| !retained.contains(&event.seq))
                    .map(|event| event.run_id.clone()),
            );
        }
        for event in active.iter() {
            if replaced || !checkpoint.discarded_runs.contains(&event.run_id) {
                unit.apply(&mut checkpoint.state, event)?;
            }
        }
        checkpoint.through_seq = last;
        let result = decode_stats(unit.view(&checkpoint.state)?)?;
        save_checkpoint_in(&mut tx, tenant, session, &checkpoint).await?;
        tx.commit().await.map_err(database_error)?;
        Ok(result)
    }
}

async fn checkpoint_in(
    tx: &mut Transaction,
    tenant: &TenantId,
    session: &SessionId,
    unit: &dyn SessionProjectionUnit,
    last: i64,
) -> Result<Option<StatsCheckpoint>, HarnessError> {
    Ok(sqlx::query_scalar::<_, String>(
        "SELECT checkpoint_json FROM cloud_session_stats WHERE tenant_id=$1 AND session_id=$2",
    )
    .bind(tenant.as_str())
    .bind(session.as_str())
    .fetch_optional(&mut **tx)
    .await
    .map_err(database_error)?
    .and_then(|json| serde_json::from_str::<StatsCheckpoint>(&json).ok())
    .filter(|cached| {
        cached.version == unit.version()
            && (-1..=last).contains(&cached.through_seq)
            && unit.valid_state(&cached.state)
    }))
}

async fn save_checkpoint_in(
    tx: &mut Transaction,
    tenant: &TenantId,
    session: &SessionId,
    checkpoint: &StatsCheckpoint,
) -> Result<(), HarnessError> {
    let json = serde_json::to_string(checkpoint).map_err(|error| {
        HarnessError::execution(format!("encode cloud stats checkpoint: {error}"))
    })?;
    sqlx::query(
        "INSERT INTO cloud_session_stats(tenant_id,session_id,checkpoint_json) VALUES($1,$2,$3)
         ON CONFLICT(tenant_id,session_id) DO UPDATE SET checkpoint_json=excluded.checkpoint_json",
    )
    .bind(tenant.as_str())
    .bind(session.as_str())
    .bind(json)
    .execute(&mut **tx)
    .await
    .map_err(database_error)?;
    Ok(())
}

fn decode_stats(value: Value) -> Result<SessionStats, HarnessError> {
    serde_json::from_value(value)
        .map_err(|error| HarnessError::execution(format!("decode cloud session stats: {error}")))
}

async fn events_after(
    tx: &mut Transaction,
    tenant: &TenantId,
    session: &SessionId,
    after: i64,
) -> Result<Vec<SessionEvent>, HarnessError> {
    Ok(sqlx::query_scalar::<_, Json<SessionEvent>>(
        "SELECT event FROM cloud_session_events WHERE tenant_id=$1 AND session_id=$2 AND seq>$3 ORDER BY seq",
    )
    .bind(tenant.as_str())
    .bind(session.as_str())
    .bind(after)
    .fetch_all(&mut **tx)
    .await
    .map_err(database_error)?
    .into_iter()
    .map(|event| event.0)
    .collect())
}
