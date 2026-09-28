use std::{collections::BTreeMap, fmt::Write as _, time::Duration};

use sha2::{Digest, Sha256};
use sqlx::{Row, any::AnyRow};
use ternilo_protocol::{
    AgentId, HarnessError, PluginEntry, SessionEvent, SessionEventKind, SessionId, SessionIdentity,
    SessionTelemetryRecord, SessionTelemetrySharingStatus, TenantId, UserId,
};
use ternilo_storage::{Backend, Json, Transaction, backend, for_update};

use crate::{CloudStore, CloudWorkerIdentity, commands, store};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CloudSessionTelemetry {
    pub session_id: SessionId,
    pub sharing: SessionTelemetrySharingStatus,
    pub handoff_seq: Option<u64>,
    pub export_seq: Option<u64>,
    pub last_error: Option<String>,
    pub updated_at_ms: u64,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClaimedCloudTelemetry {
    pub identity: SessionIdentity,
    pub occurrence_id: String,
    pub from_seq: u64,
    pub to_seq: u64,
    pub attempt_count: u32,
    pub events: Vec<SessionEvent>,
}

impl ClaimedCloudTelemetry {
    #[must_use]
    pub fn records(&self) -> Vec<SessionTelemetryRecord> {
        self.events
            .iter()
            .filter_map(|event| ternilo_kernel::session_telemetry_record(&self.identity, event))
            .map(ternilo_builtins::redact_session_telemetry_record)
            .collect()
    }
}

impl CloudStore {
    pub async fn session_telemetry(
        &self,
        tenant_id: &TenantId,
        actor_id: &UserId,
        session_id: &SessionId,
    ) -> Result<CloudSessionTelemetry, HarnessError> {
        let user_id = actor_id;
        tenant_id.validate()?;
        user_id.validate()?;
        session_id.validate()?;
        let mut transaction = self.begin().await?;
        let owner_id = crate::sharing::session_owner_in(
            &mut transaction,
            tenant_id,
            actor_id,
            session_id,
            ternilo_control::ResourceAction::View,
        )
        .await?;
        let user_id = &owner_id;
        commands::set_owner_scope(&mut transaction, tenant_id, user_id).await?;
        let row = sqlx::query(
            "SELECT sharing_status, handoff_seq, export_seq, last_error, updated_at_ms
             FROM cloud_session_telemetry
             WHERE tenant_id = $1 AND user_id = $2 AND session_id = $3",
        )
        .bind(tenant_id.as_str())
        .bind(user_id.as_str())
        .bind(session_id.as_str())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(store::database_error)?
        .ok_or_else(|| HarnessError::invalid("cloud session does not exist"))?;
        transaction.commit().await.map_err(store::database_error)?;
        decode_status(session_id.clone(), &row)
    }

    pub async fn claim_telemetry(
        &self,
        worker: &CloudWorkerIdentity,
        lease: Duration,
        limit: u32,
        now_ms: u64,
    ) -> Result<Vec<ClaimedCloudTelemetry>, HarnessError> {
        if lease.is_zero() || limit == 0 || limit > 32 {
            return Err(HarnessError::invalid(
                "telemetry claim requires a positive lease and a limit from 1 to 32",
            ));
        }
        let lease_ms = u64::try_from(lease.as_millis())
            .map_err(|_| HarnessError::invalid("telemetry lease exceeds u64 milliseconds"))?;
        let until = now_ms
            .checked_add(lease_ms)
            .ok_or_else(|| HarnessError::invalid("telemetry lease deadline exceeds u64"))?;
        let now = store::to_i64(now_ms, "telemetry claim time")?;
        let mut transaction = self.begin().await?;
        let Some(current_worker) =
            commands::worker_in(&mut transaction, worker, now_ms, true).await?
        else {
            return Ok(Vec::new());
        };
        if !current_worker.hello.capabilities.iter().any(|capability| {
            *capability == ternilo_transport::ExecutorCapability::TelemetryDisclosure
        }) {
            return Ok(Vec::new());
        }
        let candidates = telemetry_scopes_in(&mut transaction, "claim", worker, now_ms).await?;
        let mut claims = Vec::new();
        for (tenant, user, session, occurrence) in candidates {
            let Some(telemetry) = telemetry_in(&mut transaction, &tenant, &user, &session).await?
            else {
                continue;
            };
            let Some(item) = outbox_in(&mut transaction, &tenant, &user, &occurrence).await? else {
                continue;
            };
            let state: String = item.try_get("state").map_err(store::database_error)?;
            let expiry: Option<i64> = item
                .try_get("export_lease_until_ms")
                .map_err(store::database_error)?;
            let from: i64 = item.try_get("from_seq").map_err(store::database_error)?;
            let to: i64 = item.try_get("to_seq").map_err(store::database_error)?;
            let exported: i64 = telemetry
                .try_get("export_seq")
                .map_err(store::database_error)?;
            if telemetry
                .try_get::<String, _>("sharing_status")
                .map_err(store::database_error)?
                == "disabled"
                || to <= exported
                || !(state == "pending"
                    || (state == "inflight" && expiry.is_some_and(|expiry| expiry <= now)))
            {
                continue;
            }
            let earlier: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM cloud_telemetry_outbox WHERE tenant_id = $1 AND user_id = $2 AND session_id = $3 AND to_seq < $4 AND to_seq > $5 AND state <> 'exported'")
                .bind(tenant.as_str()).bind(user.as_str()).bind(session.as_str()).bind(from).bind(exported)
                .fetch_one(&mut *transaction).await.map_err(store::database_error)?;
            if earlier != 0 {
                continue;
            }
            let updated = sqlx::query("UPDATE cloud_telemetry_outbox SET state = 'inflight', lease_owner = $3, worker_generation = $4,
                export_lease_until_ms = $5, attempt_count = attempt_count + 1, last_error = NULL,
                updated_at_ms = CASE WHEN updated_at_ms > $6 THEN updated_at_ms ELSE $6 END
                WHERE tenant_id = $1 AND occurrence_id = $2 RETURNING *")
                .bind(tenant.as_str()).bind(&occurrence).bind(worker.worker_id.as_str()).bind(store::to_i64(worker.generation, "worker generation")?)
                .bind(store::to_i64(until, "telemetry expiry")?).bind(now).fetch_one(&mut *transaction).await.map_err(store::database_error)?;
            let agent: String = sqlx::query_scalar("SELECT agent_id FROM cloud_sessions WHERE tenant_id = $1 AND user_id = $2 AND session_id = $3")
                .bind(tenant.as_str()).bind(user.as_str()).bind(session.as_str()).fetch_one(&mut *transaction).await.map_err(store::database_error)?;
            let events = telemetry_events_in(&mut transaction, &tenant, &session, from, to).await?;
            claims.push(ClaimedCloudTelemetry {
                identity: SessionIdentity {
                    tenant_id: tenant,
                    user_id: user,
                    session_id: session,
                    agent_id: AgentId::new(agent),
                },
                occurrence_id: occurrence,
                from_seq: store::from_i64(from, "telemetry start")?,
                to_seq: store::from_i64(to, "telemetry end")?,
                attempt_count: u32::try_from(
                    updated
                        .try_get::<i64, _>("attempt_count")
                        .map_err(store::database_error)?,
                )
                .map_err(|_| HarnessError::execution("telemetry attempt count exceeds u32"))?,
                events,
            });
            if claims.len() == usize::try_from(limit).expect("telemetry claim limit is at most 32")
            {
                break;
            }
        }
        transaction.commit().await.map_err(store::database_error)?;
        Ok(claims)
    }

    pub async fn acknowledge_telemetry(
        &self,
        worker: &CloudWorkerIdentity,
        occurrence: &ClaimedCloudTelemetry,
        now_ms: u64,
    ) -> Result<(), HarnessError> {
        let mut transaction = self.begin().await?;
        commands::require_worker_in(&mut transaction, worker, now_ms).await?;
        let (telemetry, item) = required_occurrence_in(&mut transaction, occurrence).await?;
        if item
            .try_get::<String, _>("state")
            .map_err(store::database_error)?
            == "exported"
        {
            return Ok(());
        }
        if !outbox_owned(&item, worker, Some(now_ms))? {
            return Err(HarnessError::execution(
                "cloud telemetry claim is no longer current",
            ));
        }
        let now = store::to_i64(now_ms, "telemetry acknowledgement time")?;
        let tenant = &occurrence.identity.tenant_id;
        sqlx::query("UPDATE cloud_telemetry_outbox SET state = 'exported', lease_owner = NULL, worker_generation = NULL, export_lease_until_ms = NULL,
            exported_at_ms = CASE WHEN updated_at_ms > $3 THEN updated_at_ms ELSE $3 END,
            updated_at_ms = CASE WHEN updated_at_ms > $3 THEN updated_at_ms ELSE $3 END, last_error = NULL
            WHERE tenant_id = $1 AND occurrence_id = $2")
            .bind(tenant.as_str()).bind(&occurrence.occurrence_id).bind(now).execute(&mut *transaction).await.map_err(store::database_error)?;
        let to: i64 = item.try_get("to_seq").map_err(store::database_error)?;
        let prior: i64 = telemetry
            .try_get("export_seq")
            .map_err(store::database_error)?;
        sqlx::query("UPDATE cloud_session_telemetry SET export_seq = $4, last_error = NULL,
            updated_at_ms = CASE WHEN updated_at_ms > $5 THEN updated_at_ms ELSE $5 END WHERE tenant_id = $1 AND user_id = $2 AND session_id = $3")
            .bind(tenant.as_str()).bind(occurrence.identity.user_id.as_str()).bind(occurrence.identity.session_id.as_str()).bind(prior.max(to)).bind(now)
            .execute(&mut *transaction).await.map_err(store::database_error)?;
        transaction.commit().await.map_err(store::database_error)
    }

    pub async fn fail_telemetry(
        &self,
        worker: &CloudWorkerIdentity,
        occurrence: &ClaimedCloudTelemetry,
        error: &str,
        now_ms: u64,
    ) -> Result<(), HarnessError> {
        let mut transaction = self.begin().await?;
        if commands::worker_in(&mut transaction, worker, now_ms, false)
            .await?
            .is_none()
        {
            return Err(HarnessError::execution(
                "cloud telemetry claim is no longer current",
            ));
        }
        let (telemetry, item) = required_occurrence_in(&mut transaction, occurrence).await?;
        if !outbox_owned(&item, worker, None)? {
            return Err(HarnessError::execution(
                "cloud telemetry claim is no longer current",
            ));
        }
        retry_outbox_in(&mut transaction, &telemetry, &item, Some(error), now_ms).await?;
        transaction.commit().await.map_err(store::database_error)
    }
}

fn decode_status(
    session_id: SessionId,
    row: &sqlx::any::AnyRow,
) -> Result<CloudSessionTelemetry, HarnessError> {
    let sharing = match row
        .try_get::<String, _>("sharing_status")
        .map_err(store::database_error)?
        .as_str()
    {
        "disabled" => SessionTelemetrySharingStatus::Disabled,
        "feedback_only" => SessionTelemetrySharingStatus::FeedbackOnly,
        "full" => SessionTelemetrySharingStatus::Full,
        value => {
            return Err(HarnessError::execution(format!(
                "database contains unknown cloud telemetry status {value:?}",
            )));
        }
    };
    Ok(CloudSessionTelemetry {
        session_id,
        sharing,
        handoff_seq: decode_cursor(
            row.try_get("handoff_seq").map_err(store::database_error)?,
            "telemetry handoff cursor",
        )?,
        export_seq: decode_cursor(
            row.try_get("export_seq").map_err(store::database_error)?,
            "telemetry export cursor",
        )?,
        last_error: row.try_get("last_error").map_err(store::database_error)?,
        updated_at_ms: store::from_i64(
            row.try_get("updated_at_ms")
                .map_err(store::database_error)?,
            "telemetry update timestamp",
        )?,
    })
}

fn decode_cursor(value: i64, label: &str) -> Result<Option<u64>, HarnessError> {
    if value == -1 {
        Ok(None)
    } else {
        store::from_i64(value, label).map(Some)
    }
}

/// Synchronize disclosure after a session is created or its plugin profile changes.
pub(crate) async fn sync_session_telemetry_in(
    transaction: &mut Transaction,
    tenant: &TenantId,
    user: &UserId,
    session: &SessionId,
    now_ms: u64,
) -> Result<(), HarnessError> {
    commands::set_owner_scope(transaction, tenant, user).await?;
    let row = sqlx::query("SELECT profile_plugins, last_seq FROM cloud_sessions WHERE tenant_id = $1 AND user_id = $2 AND session_id = $3")
        .bind(tenant.as_str()).bind(user.as_str()).bind(session.as_str()).fetch_one(&mut **transaction).await.map_err(store::database_error)?;
    let plugins: Json<Vec<PluginEntry>> = row
        .try_get("profile_plugins")
        .map_err(store::database_error)?;
    let next = sharing_status(&plugins.0);
    let last_seq: i64 = row.try_get("last_seq").map_err(store::database_error)?;
    let prior = telemetry_in(transaction, tenant, user, session).await?;
    let now = store::to_i64(now_ms, "telemetry synchronization time")?;
    let Some(prior) = prior else {
        let initial = if next == "disabled" { last_seq } else { -1 };
        sqlx::query("INSERT INTO cloud_session_telemetry (tenant_id, user_id, session_id, sharing_status, handoff_seq, export_seq, updated_at_ms) VALUES ($1, $2, $3, $4, $5, $5, $6)")
            .bind(tenant.as_str()).bind(user.as_str()).bind(session.as_str()).bind(next).bind(initial).bind(now)
            .execute(&mut **transaction).await.map_err(store::database_error)?;
        if next == "full" && last_seq >= 0 {
            enqueue_telemetry_in(transaction, tenant, user, session, 0, last_seq, now_ms).await?;
            sqlx::query("UPDATE cloud_session_telemetry SET handoff_seq = $4 WHERE tenant_id = $1 AND user_id = $2 AND session_id = $3")
                .bind(tenant.as_str()).bind(user.as_str()).bind(session.as_str()).bind(last_seq)
                .execute(&mut **transaction).await.map_err(store::database_error)?;
        }
        return Ok(());
    };
    let previous: String = prior
        .try_get("sharing_status")
        .map_err(store::database_error)?;
    if previous != next {
        let mut handoff: i64 = prior
            .try_get("handoff_seq")
            .map_err(store::database_error)?;
        if next == "full" && last_seq > handoff {
            enqueue_telemetry_in(
                transaction,
                tenant,
                user,
                session,
                handoff + 1,
                last_seq,
                now_ms,
            )
            .await?;
            handoff = last_seq;
        } else if previous == "disabled" || next == "disabled" {
            handoff = last_seq;
        }
        let exported: i64 = prior.try_get("export_seq").map_err(store::database_error)?;
        sqlx::query("UPDATE cloud_session_telemetry SET sharing_status = $4, handoff_seq = $5, export_seq = $6,
            last_error = NULL, updated_at_ms = CASE WHEN updated_at_ms > $7 THEN updated_at_ms ELSE $7 END
            WHERE tenant_id = $1 AND user_id = $2 AND session_id = $3")
            .bind(tenant.as_str()).bind(user.as_str()).bind(session.as_str()).bind(next).bind(handoff)
            .bind(if next == "disabled" { exported.max(last_seq) } else { exported }).bind(now)
            .execute(&mut **transaction).await.map_err(store::database_error)?;
    }
    if next == "disabled" {
        sqlx::query("DELETE FROM cloud_telemetry_outbox WHERE tenant_id = $1 AND user_id = $2 AND session_id = $3 AND state = 'pending'")
            .bind(tenant.as_str()).bind(user.as_str()).bind(session.as_str())
            .execute(&mut **transaction).await.map_err(store::database_error)?;
    }
    Ok(())
}

/// Capture only newly inserted canonical events, in the same transaction as the event.
pub(crate) async fn capture_event_in(
    transaction: &mut Transaction,
    tenant: &TenantId,
    session: &SessionId,
    event: &SessionEvent,
    now_ms: u64,
) -> Result<(), HarnessError> {
    let user: Option<String> = sqlx::query_scalar(
        "SELECT user_id FROM cloud_sessions WHERE tenant_id = $1 AND session_id = $2",
    )
    .bind(tenant.as_str())
    .bind(session.as_str())
    .fetch_optional(&mut **transaction)
    .await
    .map_err(store::database_error)?;
    let Some(user) = user else {
        return Ok(());
    };
    let user = UserId::new(user);
    let Some(telemetry) = telemetry_in(transaction, tenant, &user, session).await? else {
        return Ok(());
    };
    let sharing: String = telemetry
        .try_get("sharing_status")
        .map_err(store::database_error)?;
    let handoff: i64 = telemetry
        .try_get("handoff_seq")
        .map_err(store::database_error)?;
    let seq = store::to_i64(event.seq, "telemetry event sequence")?;
    let release = sharing == "full"
        || (sharing == "feedback_only"
            && matches!(
                event.kind,
                SessionEventKind::FeedbackRecorded { .. }
                    | SessionEventKind::FeedbackSubmitted { .. }
            ));
    if seq <= handoff {
        return Ok(());
    }
    if release {
        enqueue_telemetry_in(
            transaction,
            tenant,
            &user,
            session,
            handoff + 1,
            seq,
            now_ms,
        )
        .await?;
    } else if sharing != "disabled" {
        return Ok(());
    }
    sqlx::query("UPDATE cloud_session_telemetry SET handoff_seq = $4,
        export_seq = CASE WHEN sharing_status = 'disabled' AND export_seq < $4 THEN $4 ELSE export_seq END,
        updated_at_ms = CASE WHEN updated_at_ms > $5 THEN updated_at_ms ELSE $5 END
        WHERE tenant_id = $1 AND user_id = $2 AND session_id = $3")
        .bind(tenant.as_str()).bind(user.as_str()).bind(session.as_str()).bind(seq).bind(store::to_i64(now_ms, "telemetry capture time")?)
        .execute(&mut **transaction).await.map_err(store::database_error)?;
    Ok(())
}

fn sharing_status(plugins: &[PluginEntry]) -> &'static str {
    let mut effective = BTreeMap::<&str, (usize, &PluginEntry)>::new();
    for (position, entry) in plugins.iter().enumerate() {
        effective
            .entry(&entry.id)
            .and_modify(|(_, prior)| *prior = entry)
            .or_insert((position, entry));
    }
    let plugin = effective
        .values()
        .filter(|(_, entry)| entry.enabled && entry.kind == "ternilo.telemetry.otlp")
        .min_by_key(|(position, _)| *position)
        .map(|(_, entry)| *entry);
    match plugin
        .and_then(|entry| entry.config.get("mode"))
        .and_then(serde_json::Value::as_str)
    {
        Some("feedback_only") => "feedback_only",
        Some("full") => "full",
        _ => "disabled",
    }
}

async fn enqueue_telemetry_in(
    transaction: &mut Transaction,
    tenant: &TenantId,
    user: &UserId,
    session: &SessionId,
    from: i64,
    to: i64,
    now_ms: u64,
) -> Result<(), HarnessError> {
    if from > to {
        return Ok(());
    }
    let source = format!(
        "{}\n{}\n{}\n{from}\n{to}",
        tenant.as_str(),
        user.as_str(),
        session.as_str()
    );
    let occurrence =
        Sha256::digest(source.as_bytes())
            .iter()
            .fold(String::from("tel_"), |mut output, byte| {
                write!(&mut output, "{byte:02x}").expect("writing to a String cannot fail");
                output
            });
    sqlx::query("INSERT INTO cloud_telemetry_outbox (tenant_id, user_id, session_id, occurrence_id, from_seq, to_seq, state, created_at_ms, updated_at_ms)
        VALUES ($1, $2, $3, $4, $5, $6, 'pending', $7, $7) ON CONFLICT (tenant_id, user_id, session_id, from_seq, to_seq) DO NOTHING")
        .bind(tenant.as_str()).bind(user.as_str()).bind(session.as_str()).bind(occurrence).bind(from).bind(to)
        .bind(store::to_i64(now_ms, "telemetry enqueue time")?).execute(&mut **transaction).await.map_err(store::database_error)?;
    Ok(())
}

async fn telemetry_in(
    transaction: &mut Transaction,
    tenant: &TenantId,
    user: &UserId,
    session: &SessionId,
) -> Result<Option<AnyRow>, HarnessError> {
    commands::set_owner_scope(transaction, tenant, user).await?;
    let query = for_update(
        transaction,
        "SELECT * FROM cloud_session_telemetry WHERE tenant_id = $1 AND user_id = $2 AND session_id = $3",
        "SELECT * FROM cloud_session_telemetry WHERE tenant_id = $1 AND user_id = $2 AND session_id = $3 FOR UPDATE",
    );
    sqlx::query(query)
        .bind(tenant.as_str())
        .bind(user.as_str())
        .bind(session.as_str())
        .fetch_optional(&mut **transaction)
        .await
        .map_err(store::database_error)
}

async fn outbox_in(
    transaction: &mut Transaction,
    tenant: &TenantId,
    user: &UserId,
    occurrence: &str,
) -> Result<Option<AnyRow>, HarnessError> {
    let query = for_update(
        transaction,
        "SELECT * FROM cloud_telemetry_outbox WHERE tenant_id = $1 AND user_id = $2 AND occurrence_id = $3",
        "SELECT * FROM cloud_telemetry_outbox WHERE tenant_id = $1 AND user_id = $2 AND occurrence_id = $3 FOR UPDATE",
    );
    sqlx::query(query)
        .bind(tenant.as_str())
        .bind(user.as_str())
        .bind(occurrence)
        .fetch_optional(&mut **transaction)
        .await
        .map_err(store::database_error)
}

fn outbox_owned(
    row: &AnyRow,
    worker: &CloudWorkerIdentity,
    now_ms: Option<u64>,
) -> Result<bool, HarnessError> {
    let now = now_ms
        .map(|value| store::to_i64(value, "telemetry ownership time"))
        .transpose()?;
    let expiry: Option<i64> = row
        .try_get("export_lease_until_ms")
        .map_err(store::database_error)?;
    Ok(row
        .try_get::<String, _>("state")
        .map_err(store::database_error)?
        == "inflight"
        && row
            .try_get::<Option<String>, _>("lease_owner")
            .map_err(store::database_error)?
            .as_deref()
            == Some(worker.worker_id.as_str())
        && row
            .try_get::<Option<i64>, _>("worker_generation")
            .map_err(store::database_error)?
            == Some(store::to_i64(worker.generation, "worker generation")?)
        && now.is_none_or(|now| expiry.is_some_and(|expiry| expiry > now)))
}

const TELEMETRY_SCOPES: &str = "SELECT tenant_id, user_id, session_id, occurrence_id FROM cloud_telemetry_outbox
    WHERE ($1 = 'claim' AND (state = 'pending' OR (state = 'inflight' AND export_lease_until_ms <= $4)))
       OR ($1 = 'release' AND state = 'inflight' AND lease_owner = $2 AND worker_generation = $3)
    ORDER BY created_at_ms, occurrence_id";

async fn telemetry_scopes_in(
    transaction: &mut Transaction,
    operation: &str,
    worker: &CloudWorkerIdentity,
    now_ms: u64,
) -> Result<Vec<(TenantId, UserId, SessionId, String)>, HarnessError> {
    let query = if backend(transaction) == Backend::Postgres {
        "SELECT tenant_id, user_id, session_id, occurrence_id FROM ternilo_cloud_telemetry_scopes($1, $2, $3, $4)"
    } else {
        TELEMETRY_SCOPES
    };
    let rows = sqlx::query(query)
        .bind(operation)
        .bind(worker.worker_id.as_str())
        .bind(store::to_i64(worker.generation, "worker generation")?)
        .bind(store::to_i64(now_ms, "telemetry discovery time")?)
        .fetch_all(&mut **transaction)
        .await
        .map_err(store::database_error)?;
    rows.iter()
        .map(|row| {
            Ok((
                TenantId::new(
                    row.try_get::<String, _>("tenant_id")
                        .map_err(store::database_error)?,
                ),
                UserId::new(
                    row.try_get::<String, _>("user_id")
                        .map_err(store::database_error)?,
                ),
                SessionId::new(
                    row.try_get::<String, _>("session_id")
                        .map_err(store::database_error)?,
                ),
                row.try_get("occurrence_id")
                    .map_err(store::database_error)?,
            ))
        })
        .collect()
}

async fn retry_outbox_in(
    transaction: &mut Transaction,
    telemetry: &AnyRow,
    item: &AnyRow,
    error: Option<&str>,
    now_ms: u64,
) -> Result<(), HarnessError> {
    let tenant: String = item.try_get("tenant_id").map_err(store::database_error)?;
    let user: String = item.try_get("user_id").map_err(store::database_error)?;
    let session: String = item.try_get("session_id").map_err(store::database_error)?;
    let occurrence: String = item
        .try_get("occurrence_id")
        .map_err(store::database_error)?;
    let disabled = telemetry
        .try_get::<String, _>("sharing_status")
        .map_err(store::database_error)?
        == "disabled";
    let already_exported = item
        .try_get::<i64, _>("to_seq")
        .map_err(store::database_error)?
        <= telemetry
            .try_get::<i64, _>("export_seq")
            .map_err(store::database_error)?;
    if disabled || already_exported {
        sqlx::query(
            "DELETE FROM cloud_telemetry_outbox WHERE tenant_id = $1 AND occurrence_id = $2",
        )
        .bind(&tenant)
        .bind(occurrence)
        .execute(&mut **transaction)
        .await
        .map_err(store::database_error)?;
        return Ok(());
    }
    let now = store::to_i64(now_ms, "telemetry retry time")?;
    sqlx::query("UPDATE cloud_telemetry_outbox SET state = 'pending', lease_owner = NULL, worker_generation = NULL,
        export_lease_until_ms = NULL, last_error = COALESCE($3, last_error), updated_at_ms = CASE WHEN updated_at_ms > $4 THEN updated_at_ms ELSE $4 END
        WHERE tenant_id = $1 AND occurrence_id = $2")
        .bind(&tenant).bind(occurrence).bind(error).bind(now).execute(&mut **transaction).await.map_err(store::database_error)?;
    if let Some(error) = error {
        sqlx::query("UPDATE cloud_session_telemetry SET last_error = $4, updated_at_ms = CASE WHEN updated_at_ms > $5 THEN updated_at_ms ELSE $5 END WHERE tenant_id = $1 AND user_id = $2 AND session_id = $3")
            .bind(tenant).bind(user).bind(session).bind(error).bind(now).execute(&mut **transaction).await.map_err(store::database_error)?;
    }
    Ok(())
}

pub(crate) async fn release_telemetry_in(
    transaction: &mut Transaction,
    worker: &CloudWorkerIdentity,
    now_ms: u64,
) -> Result<u32, HarnessError> {
    if commands::worker_in(transaction, worker, now_ms, false)
        .await?
        .is_none()
    {
        return Ok(0);
    }
    let candidates = telemetry_scopes_in(transaction, "release", worker, now_ms).await?;
    let mut changed = 0;
    for (tenant, user, session, occurrence) in candidates {
        let Some(telemetry) = telemetry_in(transaction, &tenant, &user, &session).await? else {
            continue;
        };
        let Some(item) = outbox_in(transaction, &tenant, &user, &occurrence).await? else {
            continue;
        };
        if !outbox_owned(&item, worker, None)? {
            continue;
        }
        retry_outbox_in(transaction, &telemetry, &item, None, now_ms).await?;
        changed += 1;
    }
    Ok(changed)
}

async fn required_occurrence_in(
    transaction: &mut Transaction,
    occurrence: &ClaimedCloudTelemetry,
) -> Result<(AnyRow, AnyRow), HarnessError> {
    let identity = &occurrence.identity;
    let telemetry = telemetry_in(
        transaction,
        &identity.tenant_id,
        &identity.user_id,
        &identity.session_id,
    )
    .await?
    .ok_or_else(|| HarnessError::invalid("cloud telemetry session does not exist"))?;
    let item = outbox_in(
        transaction,
        &identity.tenant_id,
        &identity.user_id,
        &occurrence.occurrence_id,
    )
    .await?
    .ok_or_else(|| HarnessError::invalid("cloud telemetry occurrence does not exist"))?;
    if item
        .try_get::<i64, _>("attempt_count")
        .map_err(store::database_error)?
        != i64::from(occurrence.attempt_count)
        || item
            .try_get::<String, _>("session_id")
            .map_err(store::database_error)?
            != identity.session_id.as_str()
        || item
            .try_get::<i64, _>("from_seq")
            .map_err(store::database_error)?
            != store::to_i64(occurrence.from_seq, "telemetry start")?
        || item
            .try_get::<i64, _>("to_seq")
            .map_err(store::database_error)?
            != store::to_i64(occurrence.to_seq, "telemetry end")?
    {
        return Err(HarnessError::policy(
            "telemetry claim does not match its stored owner and range",
        ));
    }
    Ok((telemetry, item))
}

async fn telemetry_events_in(
    transaction: &mut Transaction,
    tenant: &TenantId,
    session: &SessionId,
    from: i64,
    to: i64,
) -> Result<Vec<SessionEvent>, HarnessError> {
    let rows: Vec<Json<SessionEvent>> = sqlx::query_scalar("SELECT event FROM cloud_session_events WHERE tenant_id = $1 AND session_id = $2 AND seq BETWEEN $3 AND $4 ORDER BY seq")
        .bind(tenant.as_str()).bind(session.as_str()).bind(from).bind(to).fetch_all(&mut **transaction).await.map_err(store::database_error)?;
    let mut events = Vec::with_capacity(rows.len());
    for Json(event) in rows {
        if let SessionEventKind::AssistantMessageDelta { step, .. }
        | SessionEventKind::AssistantReasoningDelta { step, .. } = &event.kind
        {
            let query = if backend(transaction) == Backend::Postgres {
                "SELECT MIN(seq) FROM cloud_session_events WHERE tenant_id = $1 AND session_id = $2 AND run_id = $3 AND CAST(event AS jsonb)->>'type' IN ('assistant_message_delta', 'assistant_reasoning_delta') AND CAST(CAST(event AS jsonb)->>'step' AS BIGINT) = $4"
            } else {
                "SELECT MIN(seq) FROM cloud_session_events WHERE tenant_id = $1 AND session_id = $2 AND run_id = $3 AND json_extract(event, '$.type') IN ('assistant_message_delta', 'assistant_reasoning_delta') AND json_extract(event, '$.step') = $4"
            };
            let first: Option<i64> = sqlx::query_scalar(query)
                .bind(tenant.as_str())
                .bind(session.as_str())
                .bind(event.run_id.as_str())
                .bind(i64::from(*step))
                .fetch_one(&mut **transaction)
                .await
                .map_err(store::database_error)?;
            if first != Some(store::to_i64(event.seq, "telemetry delta sequence")?) {
                continue;
            }
        }
        events.push(event);
    }
    Ok(events)
}

#[cfg(test)]
#[path = "telemetry_tests.rs"]
mod tests;
