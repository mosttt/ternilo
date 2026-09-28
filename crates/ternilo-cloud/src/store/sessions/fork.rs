use sqlx::Row;
use ternilo_protocol::{HarnessError, SessionEvent, SessionEventKind, SessionId, TenantId, UserId};

use super::{
    CloudStore, database_error, decode_session, mode_str, permission_str, select_session,
    set_tenant, to_i64,
};
use crate::CloudSessionRecord;

impl CloudStore {
    pub async fn fork_session(
        &self,
        tenant_id: &TenantId,
        actor_id: &UserId,
        parent_session_id: &SessionId,
        at_seq: Option<u64>,
        now_ms: u64,
    ) -> Result<CloudSessionRecord, HarnessError> {
        let user_id = actor_id;
        tenant_id.validate()?;
        user_id.validate()?;
        parent_session_id.validate()?;
        let now = to_i64(now_ms, "cloud session fork timestamp")?;
        let mut transaction = self.begin().await?;
        let owner_id = crate::sharing::session_owner_in(
            &mut transaction,
            tenant_id,
            actor_id,
            parent_session_id,
            ternilo_control::ResourceAction::Submit,
        )
        .await?;
        let user_id = &owner_id;
        set_tenant(&mut transaction, tenant_id).await?;
        let (parent, events) = load_fork_source_in(
            &mut transaction,
            tenant_id,
            user_id,
            parent_session_id,
            at_seq,
        )
        .await?;
        let child_id = crate::random_session_id();
        let terminal = events.last().expect("fork source ends at a completed turn");
        insert_fork_in(
            &mut transaction,
            tenant_id,
            &child_id,
            &parent,
            terminal,
            now,
        )
        .await?;
        crate::execution_families::ensure_root_in(
            &mut transaction,
            tenant_id,
            &child_id,
            user_id,
            &parent.workspace_id,
            now_ms,
        )
        .await?;
        ternilo_storage::set_user_scope(&mut transaction, user_id).await?;
        crate::telemetry::sync_session_telemetry_in(
            &mut transaction,
            tenant_id,
            user_id,
            &child_id,
            now_ms,
        )
        .await?;
        copy_fork_history_in(
            &mut transaction,
            tenant_id,
            parent_session_id,
            &child_id,
            &events,
            now_ms,
        )
        .await?;
        ternilo_control::ControlStore::inherit_fork_access_in(
            &mut transaction,
            actor_id,
            tenant_id,
            parent_session_id,
            &child_id,
            now_ms,
        )
        .await?;
        let child = select_session(&mut transaction, tenant_id, &child_id).await?;
        crate::sharing::audit_session_in(
            &mut transaction,
            tenant_id,
            actor_id,
            parent_session_id,
            user_id,
            ternilo_control::ResourceAction::Submit,
            now_ms,
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;
        decode_session(&child)
    }
}

async fn load_fork_source_in(
    transaction: &mut ternilo_storage::Transaction,
    tenant_id: &TenantId,
    user_id: &UserId,
    parent_session_id: &SessionId,
    at_seq: Option<u64>,
) -> Result<(CloudSessionRecord, Vec<SessionEvent>), HarnessError> {
    let parent_row = sqlx::query(ternilo_storage::for_update(
        transaction,
        "SELECT tenant_id, session_id, user_id, project_id, workspace_id, agent_id,
                parent_session_id, subagent_metadata, title, archived_at_ms, state, permissions,
                model_snapshot, reserved_model_tokens,
                agent_preset, profile_plugins, mode, execution, last_seq, created_at_ms, updated_at_ms
         FROM cloud_sessions
         WHERE tenant_id = $1 AND session_id = $2 AND user_id = $3
           AND archived_at_ms IS NULL",
        "SELECT tenant_id, session_id, user_id, project_id, workspace_id, agent_id,
                parent_session_id, subagent_metadata, title, archived_at_ms, state, permissions,
                model_snapshot, reserved_model_tokens,
                agent_preset, profile_plugins, mode, execution, last_seq, created_at_ms, updated_at_ms
         FROM cloud_sessions
         WHERE tenant_id = $1 AND session_id = $2 AND user_id = $3
           AND archived_at_ms IS NULL
         FOR UPDATE",
    ))
    .bind(tenant_id.as_str())
    .bind(parent_session_id.as_str())
    .bind(user_id.as_str())
    .fetch_optional(&mut **transaction)
    .await
    .map_err(database_error)?
    .ok_or_else(|| HarnessError::invalid("cloud session does not exist"))?;
    let parent = decode_session(&parent_row)?;
    let event_rows = sqlx::query(
        "SELECT event FROM cloud_session_events
         WHERE tenant_id = $1 AND session_id = $2
         ORDER BY seq",
    )
    .bind(tenant_id.as_str())
    .bind(parent_session_id.as_str())
    .fetch_all(&mut **transaction)
    .await
    .map_err(database_error)?;
    let mut events = event_rows
        .into_iter()
        .map(|row| {
            row.try_get::<ternilo_storage::Json<SessionEvent>, _>("event")
                .map(|event| event.0)
                .map_err(database_error)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let boundary = match at_seq {
        Some(sequence) => events
            .iter()
            .position(|event| event.seq >= sequence && terminal_turn(&event.kind)),
        None => events.iter().rposition(|event| terminal_turn(&event.kind)),
    }
    .ok_or_else(|| match at_seq {
        Some(sequence) => HarnessError::invalid(format!(
            "cloud session has not completed the turn containing event {sequence}"
        )),
        None => HarnessError::invalid("cloud session has no completed turn to fork"),
    })?;
    events.truncate(boundary + 1);
    Ok((parent, events))
}

async fn insert_fork_in(
    transaction: &mut ternilo_storage::Transaction,
    tenant_id: &TenantId,
    child_id: &SessionId,
    parent: &CloudSessionRecord,
    terminal: &SessionEvent,
    now: i64,
) -> Result<(), HarnessError> {
    let title = fork_title(&parent.title);
    sqlx::query(
        "INSERT INTO cloud_sessions
            (tenant_id, session_id, user_id, project_id, workspace_id, parent_session_id,
             agent_id, title, permissions, model_snapshot,
             reserved_model_tokens, agent_preset, profile_plugins, mode, state, last_seq,
             created_at_ms, updated_at_ms)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12,
                 $13, $14, $15, $16, $17, $17)",
    )
    .bind(tenant_id.as_str())
    .bind(child_id.as_str())
    .bind(parent.user_id.as_str())
    .bind(&parent.project_id)
    .bind(parent.workspace_id.as_str())
    .bind(parent.session_id.as_str())
    .bind(parent.agent_id.as_str())
    .bind(title)
    .bind(permission_str(parent.permissions))
    .bind(parent.model.as_ref().map(ternilo_storage::Json))
    .bind(to_i64(
        parent.reserved_model_tokens,
        "cloud session model token budget",
    )?)
    .bind(&parent.agent_preset)
    .bind(ternilo_storage::Json(&parent.profile_plugins))
    .bind(mode_str(parent.mode))
    .bind(terminal_session_state(&terminal.kind))
    .bind(to_i64(terminal.seq, "cloud fork event sequence")?)
    .bind(now)
    .execute(&mut **transaction)
    .await
    .map_err(database_error)?;
    Ok(())
}

async fn copy_fork_history_in(
    transaction: &mut ternilo_storage::Transaction,
    tenant_id: &TenantId,
    parent_session_id: &SessionId,
    child_id: &SessionId,
    events: &[SessionEvent],
    now_ms: u64,
) -> Result<(), HarnessError> {
    let terminal = events.last().expect("fork source ends at a completed turn");
    sqlx::query(
        "INSERT INTO cloud_session_events
            (tenant_id, session_id, seq, run_id, event, writer_fencing_token, created_at_ms)
         SELECT tenant_id, $3, seq, run_id, event, writer_fencing_token, created_at_ms
         FROM cloud_session_events
         WHERE tenant_id = $1 AND session_id = $2 AND seq <= $4
         ORDER BY seq",
    )
    .bind(tenant_id.as_str())
    .bind(parent_session_id.as_str())
    .bind(child_id.as_str())
    .bind(to_i64(terminal.seq, "cloud fork event sequence")?)
    .execute(&mut **transaction)
    .await
    .map_err(database_error)?;
    for event in events {
        if let ternilo_protocol::SessionEventKind::UserMessage {
            source: Some(ternilo_protocol::UserMessageSource::Submission { submission_id, .. }),
            attachments,
            ..
        } = &event.kind
        {
            sqlx::query("INSERT INTO cloud_session_uploads (tenant_id,session_id,submission_id,attachment_index,created_at_ms,submitted_run_id,attachment)
                SELECT tenant_id,$3,submission_id,attachment_index,created_at_ms,submitted_run_id,attachment
                FROM cloud_session_uploads WHERE tenant_id=$1 AND session_id=$2 AND submission_id=$4 AND attachment_index<$5
                ON CONFLICT (tenant_id,session_id,submission_id,attachment_index) DO NOTHING")
                .bind(tenant_id.as_str()).bind(parent_session_id.as_str()).bind(child_id.as_str())
                .bind(submission_id.as_str()).bind(i64::try_from(attachments.len()).expect("bounded attachment count"))
                .execute(&mut **transaction).await.map_err(database_error)?;
        }
        crate::telemetry::capture_event_in(transaction, tenant_id, child_id, event, now_ms).await?;
    }
    Ok(())
}

fn terminal_turn(kind: &SessionEventKind) -> bool {
    matches!(
        kind,
        SessionEventKind::TurnFinished { .. }
            | SessionEventKind::TurnFailed { .. }
            | SessionEventKind::TurnCancelled
    )
}

fn terminal_session_state(kind: &SessionEventKind) -> &'static str {
    match kind {
        SessionEventKind::TurnFinished { .. } => "succeeded",
        SessionEventKind::TurnFailed { .. } => "failed",
        SessionEventKind::TurnCancelled => "cancelled",
        _ => unreachable!("fork boundary is always a terminal turn"),
    }
}

fn fork_title(title: &str) -> String {
    let suffix = title
        .strip_suffix(')')
        .and_then(|body| body.rsplit_once(" ("))
        .and_then(|(prefix, digits)| {
            digits
                .parse::<u64>()
                .ok()
                .map(|value| (prefix, format!(" ({})", value.saturating_add(1))))
        })
        .unwrap_or((title, " (1)".to_owned()));
    let keep = 256_usize.saturating_sub(suffix.1.chars().count());
    format!(
        "{}{}",
        suffix.0.chars().take(keep).collect::<String>(),
        suffix.1
    )
}
