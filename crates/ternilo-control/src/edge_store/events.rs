use super::{
    EdgeSessionMetadata, EdgeStore, ExecutorId, HarnessError, Json, SessionEvent, SessionEventKind,
    SessionId, TenantId, Value, database_error, from_i64, from_json, to_i64, to_json, uploads,
    validate_route,
};
use sqlx::Row;
use std::collections::BTreeMap;
use ternilo_storage::Transaction;

impl EdgeStore {
    pub async fn merge_events(
        &self,
        tenant_id: &TenantId,
        executor_id: &ExecutorId,
        session_id: &SessionId,
        events: &[SessionEvent],
    ) -> Result<(), HarnessError> {
        let mut transaction = self.transaction(tenant_id).await?;
        self.merge_events_in_transaction(
            &mut transaction,
            tenant_id,
            executor_id,
            session_id,
            events,
        )
        .await?;
        transaction.commit().await.map_err(database_error)
    }

    pub async fn merge_events_in_transaction(
        &self,
        transaction: &mut ternilo_storage::Transaction,
        tenant_id: &TenantId,
        executor_id: &ExecutorId,
        session_id: &SessionId,
        events: &[SessionEvent],
    ) -> Result<(), HarnessError> {
        validate_route(tenant_id, executor_id)?;
        session_id.validate()?;
        if uploads::session_deleted(transaction, tenant_id, executor_id, session_id).await? {
            return Ok(());
        }
        let metadata = sqlx::query_scalar::<_, Json<EdgeSessionMetadata>>(ternilo_storage::for_update(
            transaction,
            "SELECT metadata_json FROM control_edge_sessions WHERE tenant_id = $1 AND executor_id = $2 AND node_session_id = $3",
            "SELECT metadata_json FROM control_edge_sessions WHERE tenant_id = $1 AND executor_id = $2 AND node_session_id = $3 FOR UPDATE",
        ))
        .bind(tenant_id.as_str()).bind(executor_id.as_str()).bind(session_id.as_str())
        .fetch_optional(&mut **transaction).await.map_err(database_error)?;
        // Unmapped sessions belong to the Node's private local workbench.
        let Some(mut metadata) = metadata else {
            return Ok(());
        };
        let last = sqlx::query_scalar::<_, Option<i64>>(
            "SELECT MAX(seq) FROM control_edge_events
             WHERE tenant_id = $1 AND executor_id = $2 AND session_id = $3",
        )
        .bind(tenant_id.as_str())
        .bind(executor_id.as_str())
        .bind(session_id.as_str())
        .fetch_one(&mut **transaction)
        .await
        .map_err(database_error)?;
        append_events(
            transaction,
            tenant_id,
            executor_id,
            session_id,
            last,
            events,
        )
        .await?;
        if let Some((title, occurred_at_ms)) = events.iter().rev().find_map(|event| {
            if let SessionEventKind::SessionTitleGenerated { title } = &event.kind {
                Some((title.as_str(), event.occurred_at_ms))
            } else {
                None
            }
        }) && metadata.0.title == "New session"
        {
            metadata.0.title = title.to_owned();
            metadata.0.updated_at_ms = metadata.0.updated_at_ms.max(occurred_at_ms);
            sqlx::query("UPDATE control_edge_sessions SET metadata_json = $4 WHERE tenant_id = $1 AND executor_id = $2 AND node_session_id = $3")
                    .bind(tenant_id.as_str()).bind(executor_id.as_str()).bind(session_id.as_str())
                    .bind(metadata).execute(&mut **transaction).await.map_err(database_error)?;
        }

        if let Some(last) = events.last() {
            let changed = sqlx::query(
                "UPDATE control_edge_sessions
                 SET last_event_seq = CASE WHEN COALESCE(last_event_seq, 0) > $4 THEN COALESCE(last_event_seq, 0) ELSE $4 END,
                     updated_at_ms = CASE WHEN updated_at_ms > $5 THEN updated_at_ms ELSE $5 END
                 WHERE tenant_id = $1 AND executor_id = $2 AND node_session_id = $3",
            )
            .bind(tenant_id.as_str())
            .bind(executor_id.as_str())
            .bind(session_id.as_str())
            .bind(to_i64(last.seq, "session event sequence")?)
            .bind(to_i64(last.occurred_at_ms, "session event timestamp")?)
            .execute(&mut **transaction)
            .await
            .map_err(database_error)?
            .rows_affected();
            if changed != 1 {
                return Err(HarnessError::conflict(
                    "edge session mapping changed during event replication",
                ));
            }
        }
        Ok(())
    }

    pub async fn events(
        &self,
        tenant_id: &TenantId,
        executor_id: &ExecutorId,
        session_id: &SessionId,
    ) -> Result<Vec<SessionEvent>, HarnessError> {
        self.events_after(tenant_id, executor_id, session_id, None)
            .await
    }

    pub async fn history(
        &self,
        tenant_id: &TenantId,
        executor_id: &ExecutorId,
        session_id: &SessionId,
        query: ternilo_protocol::SessionHistoryQuery,
    ) -> Result<ternilo_protocol::SessionEventPage, HarnessError> {
        query.validate()?;
        validate_route(tenant_id, executor_id)?;
        session_id.validate()?;
        let mut transaction = self.transaction(tenant_id).await?;
        let rows = sqlx::query(
            "SELECT event_json FROM control_edge_events
             WHERE tenant_id = $1 AND executor_id = $2 AND session_id = $3
             AND (CAST($4 AS BIGINT) IS NULL OR seq < $4) ORDER BY seq DESC LIMIT $5",
        )
        .bind(tenant_id.as_str())
        .bind(executor_id.as_str())
        .bind(session_id.as_str())
        .bind(
            query
                .before_seq
                .map(|seq| to_i64(seq, "event cursor"))
                .transpose()?,
        )
        .bind(i64::from(query.limit))
        .fetch_all(&mut *transaction)
        .await
        .map_err(database_error)?;
        let mut events = rows
            .into_iter()
            .map(|row| {
                let value: Json<Value> = row.try_get("event_json").map_err(database_error)?;
                from_json(value, "session event")
            })
            .collect::<Result<Vec<SessionEvent>, HarnessError>>()?;
        Self::project_event_provenance_in_transaction(
            &mut transaction,
            tenant_id,
            executor_id,
            session_id,
            &mut events,
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;
        events.reverse();
        Ok(ternilo_protocol::SessionEventPage::new(events))
    }

    pub async fn events_after(
        &self,
        tenant_id: &TenantId,
        executor_id: &ExecutorId,
        session_id: &SessionId,
        after_seq: Option<u64>,
    ) -> Result<Vec<SessionEvent>, HarnessError> {
        validate_route(tenant_id, executor_id)?;
        session_id.validate()?;
        let mut transaction = self.transaction(tenant_id).await?;
        let rows = sqlx::query(
            "SELECT event_json FROM control_edge_events
             WHERE tenant_id = $1 AND executor_id = $2 AND session_id = $3
             AND (CAST($4 AS BIGINT) IS NULL OR seq > $4) ORDER BY seq",
        )
        .bind(tenant_id.as_str())
        .bind(executor_id.as_str())
        .bind(session_id.as_str())
        .bind(
            after_seq
                .map(|seq| to_i64(seq, "event cursor"))
                .transpose()?,
        )
        .fetch_all(&mut *transaction)
        .await
        .map_err(database_error)?;
        let mut events = rows
            .into_iter()
            .map(|row| {
                let value: Json<Value> = row.try_get("event_json").map_err(database_error)?;
                from_json(value, "session event")
            })
            .collect::<Result<Vec<SessionEvent>, HarnessError>>()?;
        Self::project_event_provenance_in_transaction(
            &mut transaction,
            tenant_id,
            executor_id,
            session_id,
            &mut events,
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(events)
    }

    pub async fn last_event_seq(
        &self,
        tenant_id: &TenantId,
        executor_id: &ExecutorId,
        session_id: &SessionId,
    ) -> Result<Option<u64>, HarnessError> {
        validate_route(tenant_id, executor_id)?;
        session_id.validate()?;
        let mut transaction = self.transaction(tenant_id).await?;
        let seq: Option<i64> = sqlx::query_scalar("SELECT MAX(seq) FROM control_edge_events WHERE tenant_id=$1 AND executor_id=$2 AND session_id=$3")
            .bind(tenant_id.as_str()).bind(executor_id.as_str()).bind(session_id.as_str())
            .fetch_one(&mut *transaction).await.map_err(database_error)?;
        transaction.commit().await.map_err(database_error)?;
        seq.map(|seq| from_i64(seq, "event cursor")).transpose()
    }
}

async fn append_events(
    transaction: &mut Transaction,
    tenant_id: &TenantId,
    executor_id: &ExecutorId,
    session_id: &SessionId,
    last: Option<i64>,
    events: &[SessionEvent],
) -> Result<(), HarnessError> {
    let mut existing = BTreeMap::new();
    if let (Some(last), Some(first)) = (last, events.iter().map(|event| event.seq).min()) {
        let first = to_i64(first, "session event sequence")?;
        if first <= last {
            let end = events
                .iter()
                .map(|event| event.seq)
                .max()
                .unwrap_or_default();
            let end = to_i64(end, "session event sequence")?.min(last);
            let rows = sqlx::query("SELECT seq,event_json FROM control_edge_events WHERE tenant_id=$1 AND executor_id=$2 AND session_id=$3 AND seq >= $4 AND seq <= $5")
                .bind(tenant_id.as_str()).bind(executor_id.as_str()).bind(session_id.as_str()).bind(first).bind(end)
                .fetch_all(&mut **transaction).await.map_err(database_error)?;
            for row in rows {
                existing.insert(
                    row.try_get::<i64, _>("seq").map_err(database_error)?,
                    row.try_get::<Json<Value>, _>("event_json")
                        .map_err(database_error)?,
                );
            }
        }
    }
    let mut expected = last.map_or(0, |seq| seq.saturating_add(1));
    let mut appended: Vec<(i64, Json<Value>)> = Vec::new();
    for event in events {
        if let SessionEventKind::UserMessage {
            provenance, source, ..
        } = &event.kind
        {
            EdgeStore::classify_message_provenance_in_transaction(
                transaction,
                tenant_id,
                executor_id,
                session_id,
                provenance.as_ref(),
                source.as_ref(),
            )
            .await?;
        }
        let seq = to_i64(event.seq, "session event sequence")?;
        let document = to_json(event, "session event")?;
        let previous = existing.get(&seq).or_else(|| {
            appended
                .binary_search_by_key(&seq, |(seq, _)| *seq)
                .ok()
                .map(|index| &appended[index].1)
        });
        if let Some(previous) = previous {
            if *previous != document {
                return Err(HarnessError::policy(format!(
                    "executor changed existing session event {}",
                    event.seq
                )));
            }
            continue;
        }
        if seq != expected {
            return Err(HarnessError::invalid(format!(
                "executor event stream has a gap: expected {expected}, found {seq}"
            )));
        }
        appended.push((seq, document));
        expected = expected.saturating_add(1);
    }
    for chunk in appended.chunks(100) {
        let values = (0..chunk.len())
            .map(|index| format!("($1,$2,$3,${},${})", 4 + index * 2, 5 + index * 2))
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "INSERT INTO control_edge_events (tenant_id,executor_id,session_id,seq,event_json) VALUES {values}"
        );
        // Only numbered placeholders are generated; all event data stays bound.
        let mut query = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(tenant_id.as_str())
            .bind(executor_id.as_str())
            .bind(session_id.as_str());
        for (seq, value) in chunk {
            query = query.bind(*seq).bind(value.clone());
        }
        query
            .execute(&mut **transaction)
            .await
            .map_err(database_error)?;
    }
    Ok(())
}
