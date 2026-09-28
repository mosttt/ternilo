use sqlx::Row;
use ternilo_protocol::{
    HarnessError, PermissionPreset, PluginEntry, SessionId, SessionMode, SubagentSessionMetadata,
    TenantId, UserId, WorkspaceId,
};

use crate::store::{database_error, from_i64, optional_u64};
use crate::{CloudSessionRecord, CloudSessionState};

pub(super) async fn select_session(
    transaction: &mut ternilo_storage::Transaction,
    tenant_id: &TenantId,
    session_id: &SessionId,
) -> Result<sqlx::any::AnyRow, HarnessError> {
    sqlx::query(
        "SELECT tenant_id, session_id, user_id, project_id, workspace_id, agent_id,
                parent_session_id, subagent_metadata, title, archived_at_ms, state, permissions,
                model_snapshot,
                reserved_model_tokens,
                agent_preset, profile_plugins, mode, execution, last_seq, created_at_ms, updated_at_ms
         FROM cloud_sessions WHERE tenant_id = $1 AND session_id = $2",
    )
    .bind(tenant_id.as_str())
    .bind(session_id.as_str())
    .fetch_optional(&mut **transaction)
    .await
    .map_err(database_error)?
    .ok_or_else(|| HarnessError::invalid("cloud session does not exist"))
}

pub(crate) fn decode_session(row: &sqlx::any::AnyRow) -> Result<CloudSessionRecord, HarnessError> {
    let last_seq: i64 = row.try_get("last_seq").map_err(database_error)?;
    Ok(CloudSessionRecord {
        execution: row
            .try_get::<Option<ternilo_storage::Json<ternilo_protocol::SessionExecutionActivity>>, _>("execution")
            .map_err(database_error)?
            .map(|value| value.0),
        tenant_id: TenantId::new(
            row.try_get::<String, _>("tenant_id")
                .map_err(database_error)?,
        ),
        session_id: SessionId::new(
            row.try_get::<String, _>("session_id")
                .map_err(database_error)?,
        ),
        user_id: UserId::new(
            row.try_get::<String, _>("user_id")
                .map_err(database_error)?,
        ),
        project_id: row.try_get("project_id").map_err(database_error)?,
        workspace_id: WorkspaceId::new(
            row.try_get::<String, _>("workspace_id")
                .map_err(database_error)?,
        ),
        parent_session_id: row
            .try_get::<Option<String>, _>("parent_session_id")
            .map_err(database_error)?
            .map(SessionId::new),
        subagent: row
            .try_get::<Option<ternilo_storage::Json<SubagentSessionMetadata>>, _>(
                "subagent_metadata",
            )
            .map_err(database_error)?
            .map(|value| value.0),
        agent_id: ternilo_protocol::AgentId::new(
            row.try_get::<String, _>("agent_id")
                .map_err(database_error)?,
        ),
        title: row.try_get("title").map_err(database_error)?,
        archived_at_ms: optional_u64(row, "archived_at_ms", "cloud session archive timestamp")?,
        state: CloudSessionState::parse(
            &row.try_get::<String, _>("state").map_err(database_error)?,
        )?,
        permissions: permission_parse(
            &row.try_get::<String, _>("permissions")
                .map_err(database_error)?,
        )?,
        model: row
            .try_get::<Option<ternilo_storage::Json<ternilo_protocol::RunModelSnapshot>>, _>(
                "model_snapshot",
            )
            .map_err(database_error)?
            .map(|value| value.0),
        reserved_model_tokens: from_i64(
            row.try_get("reserved_model_tokens")
                .map_err(database_error)?,
            "cloud session model token budget",
        )?,
        agent_preset: row.try_get("agent_preset").map_err(database_error)?,
        profile_plugins: row
            .try_get::<ternilo_storage::Json<Vec<PluginEntry>>, _>("profile_plugins")
            .map_err(database_error)?
            .0,
        mode: mode_parse(&row.try_get::<String, _>("mode").map_err(database_error)?)?,
        last_seq: if last_seq < 0 {
            None
        } else {
            Some(from_i64(last_seq, "cloud session sequence")?)
        },
        created_at_ms: from_i64(
            row.try_get("created_at_ms").map_err(database_error)?,
            "cloud session creation timestamp",
        )?,
        updated_at_ms: from_i64(
            row.try_get("updated_at_ms").map_err(database_error)?,
            "cloud session update timestamp",
        )?,
    })
}

fn permission_parse(value: &str) -> Result<PermissionPreset, HarnessError> {
    match value {
        "read_only" => Ok(PermissionPreset::ReadOnly),
        "workspace_write" => Ok(PermissionPreset::WorkspaceWrite),
        _ => Err(HarnessError::execution(format!(
            "database contains unknown cloud session permission {value:?}"
        ))),
    }
}

fn mode_parse(value: &str) -> Result<SessionMode, HarnessError> {
    match value {
        "execute" => Ok(SessionMode::Execute),
        "plan" => Ok(SessionMode::Plan),
        _ => Err(HarnessError::execution(format!(
            "database contains unknown cloud session mode {value:?}"
        ))),
    }
}
