use super::require_model_lineage;
use crate::{ControlStore, NodePrincipal};
use ternilo_protocol::{
    HarnessError, NodeModelRequest, RunId, ScheduleChange, ScheduleModelOrigin, SessionEvent,
    SessionEventKind, SessionId,
};
use ternilo_storage::{Json, Transaction, database_error};

pub(super) fn validate(body: &NodeModelRequest) -> Result<(), HarnessError> {
    if body.schedule_origins.len() > 32 {
        return Err(HarnessError::policy(
            "scheduled model origin chain is too long",
        ));
    }
    for origin in &body.schedule_origins {
        origin.validate()?;
    }
    Ok(())
}

async fn event(
    tx: &mut Transaction,
    node: &NodePrincipal,
    session: &SessionId,
    seq: u64,
) -> Result<Option<SessionEvent>, HarnessError> {
    let sequence = i64::try_from(seq)
        .map_err(|_| HarnessError::policy("schedule event sequence exceeds storage range"))?;
    let value: Option<Json<SessionEvent>> = sqlx::query_scalar("SELECT event_json FROM control_edge_events WHERE tenant_id=$1 AND executor_id=$2 AND session_id=$3 AND seq=$4")
        .bind(node.scope.tenant_id.as_str()).bind(node.executor_id.as_str()).bind(session.as_str()).bind(sequence)
        .fetch_optional(&mut **tx).await.map_err(database_error)?;
    Ok(value.map(|value| value.0))
}

impl ControlStore {
    pub async fn node_model_request_registered(
        &self,
        node: &NodePrincipal,
        body: &NodeModelRequest,
    ) -> Result<bool, HarnessError> {
        validate(body)?;
        if !self
            .node_model_session_registered(node, &body.session_id)
            .await?
        {
            return Ok(false);
        }
        let mut tx = self
            .database()
            .tenant_transaction(&node.scope.tenant_id)
            .await?;
        for origin in &body.schedule_origins {
            for seq in [origin.created_seq, origin.dispatched_seq] {
                if event(&mut tx, node, &origin.session_id, seq)
                    .await?
                    .is_none()
                {
                    return Ok(false);
                }
            }
        }
        tx.commit().await.map_err(database_error)?;
        Ok(true)
    }
}

async fn creator_run(
    tx: &mut Transaction,
    node: &NodePrincipal,
    origin: &ScheduleModelOrigin,
    current_run: &RunId,
) -> Result<RunId, HarnessError> {
    let created = event(tx, node, &origin.session_id, origin.created_seq)
        .await?
        .ok_or_else(|| HarnessError::policy("schedule creation has not synchronized"))?;
    let dispatched = event(tx, node, &origin.session_id, origin.dispatched_seq)
        .await?
        .ok_or_else(|| HarnessError::policy("schedule dispatch has not synchronized"))?;
    let SessionEventKind::ScheduleChanged {
        change: ScheduleChange::Create { schedule },
    } = created.kind
    else {
        return Err(HarnessError::policy(
            "schedule origin does not reference a creation",
        ));
    };
    if !matches!(dispatched.kind, SessionEventKind::ScheduleChanged { change: ScheduleChange::Dispatch { id, run_id: Some(run_id), .. } } if id == schedule.id && &run_id == current_run)
    {
        return Err(HarnessError::policy(
            "schedule dispatch does not authorize this model run",
        ));
    }
    Ok(created.run_id)
}

pub(super) async fn verify(
    tx: &mut Transaction,
    node: &NodePrincipal,
    body: &NodeModelRequest,
) -> Result<RunId, HarnessError> {
    validate(body)?;
    let mut session = body.session_id.clone();
    let mut run = body.run_id.clone();
    let mut visited = std::collections::BTreeSet::new();
    for origin in &body.schedule_origins {
        if !visited.insert((origin.session_id.clone(), run.clone())) {
            return Err(HarnessError::policy(
                "scheduled model origin contains a cycle",
            ));
        }
        require_model_lineage(tx, node, &session, &origin.session_id).await?;
        run = creator_run(tx, node, origin, &run).await?;
        session = origin.session_id.clone();
        if visited.contains(&(session.clone(), run.clone())) {
            return Err(HarnessError::policy(
                "scheduled model origin contains a cycle",
            ));
        }
    }
    require_model_lineage(tx, node, &session, &body.origin_session_id).await?;
    Ok(run)
}
