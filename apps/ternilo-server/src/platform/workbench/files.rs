use std::collections::{BTreeMap, BTreeSet};

use salvo_core::prelude::{Depot, Json, Request, handler};
use sqlx::Row;
use ternilo_control::{
    ControlAction, ControlUser, ResourceAction, ResourceKind, resource_access_in,
};
use ternilo_protocol::{
    FileSourceStatus, HarnessError, OfflineFileSource, RunId, SessionFileContent,
    SessionFileCursor, SessionFileItem, SessionFileKind, SessionFileLocator, SessionFilePage,
    SessionFileQuery, SessionId, TenantId, WorkspaceId,
};
use ternilo_storage::{Backend, database_error};
use ternilo_transport::{ApplicationOperation, ExecutorId};

use super::sessions::scope;

#[cfg(test)]
#[path = "files_tests.rs"]
mod tests;
use crate::platform::{
    http::{ApiError, invalid_request, path_parameter, tenant_parameter},
    state::{AppState, actor, app_state},
};

#[handler]
pub(crate) async fn list_files(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<SessionFilePage>, ApiError> {
    let tenant = tenant_parameter(request)?;
    let query = request
        .parse_queries::<SessionFileQuery>()
        .map_err(invalid_request)?;
    Ok(Json(
        file_page(app_state(depot), actor(depot), &tenant, &query).await?,
    ))
}

#[expect(
    clippy::too_many_lines,
    reason = "Read a bounded metadata page and authorize its resources in the same transaction."
)]
pub(super) async fn file_page(
    state: &AppState,
    user: &ControlUser,
    tenant: &TenantId,
    query: &SessionFileQuery,
) -> Result<SessionFilePage, HarnessError> {
    query.validate()?;
    state
        .store
        .authorize(user, tenant, ControlAction::TenantRead)
        .await?;
    let cursor = query.parsed_cursor()?;
    let batch_limit = usize::from(query.limit) * 4;
    let sql = match state.cloud.database().backend() {
        Backend::Sqlite => include_str!("files_sqlite.sql"),
        Backend::Postgres => include_str!("files_postgres.sql"),
    };
    let mut tx = state.cloud.database().tenant_transaction(tenant).await?;
    let rows = sqlx::query(sql)
        .bind(tenant.as_str())
        .bind(user.user_id.as_str())
        .bind(query.session_id.as_ref().map(SessionId::as_str))
        .bind(query.workspace_id.as_ref().map(WorkspaceId::as_str))
        .bind(query.kind.map(SessionFileKind::as_str))
        .bind(query.query.as_deref().unwrap_or("").trim())
        .bind(
            cursor
                .as_ref()
                .map(|value| signed(value.occurred_at_ms))
                .transpose()?,
        )
        .bind(cursor.as_ref().map(|value| value.session_id.as_str()))
        .bind(cursor.as_ref().map(|value| value.file_id.as_str()))
        .bind(i64::try_from(batch_limit).expect("bounded file page"))
        .fetch_all(&mut *tx)
        .await
        .map_err(database_error)?;
    let mut more = rows.len() == batch_limit;
    let mut last_cursor: Option<SessionFileCursor> = None;
    let mut items = Vec::new();
    let mut permissions = BTreeMap::new();
    let mut connections = BTreeMap::new();
    let mut offline = BTreeSet::new();
    for row in rows {
        if items.len() == usize::from(query.limit) {
            more = true;
            break;
        }
        let session = SessionId::new(
            row.try_get::<String, _>("session_id")
                .map_err(database_error)?,
        );
        let position = SessionFileCursor {
            occurred_at_ms: unsigned(row.try_get("occurred_at_ms").map_err(database_error)?)?,
            session_id: session.clone(),
            file_id: row.try_get("file_id").map_err(database_error)?,
        };
        last_cursor = Some(position.clone());
        let visible = if let Some(visible) = permissions.get(&session) {
            *visible
        } else {
            let visible = resource_access_in(
                &mut tx,
                &user.user_id,
                tenant,
                ResourceKind::Session,
                session.as_str(),
            )
            .await?
            .permissions
            .view;
            permissions.insert(session.clone(), visible);
            visible
        };
        if !visible {
            continue;
        }
        let workspace = WorkspaceId::new(
            row.try_get::<String, _>("workspace_id")
                .map_err(database_error)?,
        );
        let executor = row
            .try_get::<Option<String>, _>("executor_id")
            .map_err(database_error)?;
        let status = match executor {
            Some(executor) => {
                let connected = connected(state, tenant, &executor, &mut connections).await;
                if !connected {
                    offline.insert((workspace.clone(), executor));
                }
                if connected {
                    FileSourceStatus::Online
                } else {
                    FileSourceStatus::Offline
                }
            }
            None => FileSourceStatus::Online,
        };
        let kind = match row
            .try_get::<String, _>("kind")
            .map_err(database_error)?
            .as_str()
        {
            "upload" => SessionFileKind::Upload,
            "generated" => SessionFileKind::Generated,
            _ => return Err(HarnessError::execution("invalid file kind")),
        };
        items.push(SessionFileItem {
            id: position.file_id.clone(),
            session_id: session,
            session_title: row.try_get("session_title").map_err(database_error)?,
            session_archived: row
                .try_get::<i64, _>("session_archived")
                .map_err(database_error)?
                != 0,
            workspace_id: workspace,
            workspace_name: row.try_get("workspace_name").map_err(database_error)?,
            kind,
            name: row.try_get("name").map_err(database_error)?,
            media_type: row.try_get("media_type").map_err(database_error)?,
            path: row.try_get("path").map_err(database_error)?,
            occurred_at_ms: position.occurred_at_ms,
            event_seq: row
                .try_get::<Option<i64>, _>("event_seq")
                .map_err(database_error)?
                .map(unsigned)
                .transpose()?,
            run_id: RunId::new(row.try_get::<String, _>("run_id").map_err(database_error)?),
            source_status: status,
            attachment_index: u32::try_from(
                row.try_get::<i64, _>("attachment_index")
                    .map_err(database_error)?,
            )
            .map_err(|_| HarnessError::execution("invalid file attachment index"))?,
        });
    }
    tx.commit().await.map_err(database_error)?;
    offline.extend(offline_sources(state, user, tenant, query, &mut connections).await?);
    Ok(SessionFilePage {
        items,
        next_cursor: if more {
            last_cursor.map(|value| value.encode()).transpose()?
        } else {
            None
        },
        offline_sources: offline
            .into_iter()
            .map(|(workspace_id, executor_id)| OfflineFileSource {
                workspace_id,
                executor_id,
            })
            .collect(),
    })
}

async fn offline_sources(
    state: &AppState,
    user: &ControlUser,
    tenant: &TenantId,
    query: &SessionFileQuery,
    connections: &mut BTreeMap<String, bool>,
) -> Result<BTreeSet<(WorkspaceId, String)>, HarnessError> {
    // Source availability is independent of filename matches or cached file events.
    let mut tx = state.cloud.database().tenant_transaction(tenant).await?;
    let rows = sqlx::query(include_str!("files_sources.sql"))
        .bind(tenant.as_str())
        .bind(user.user_id.as_str())
        .bind(query.session_id.as_ref().map(SessionId::as_str))
        .bind(query.workspace_id.as_ref().map(WorkspaceId::as_str))
        .fetch_all(&mut *tx)
        .await
        .map_err(database_error)?;
    let mut sources = BTreeSet::new();
    for row in rows {
        let resource = row
            .try_get::<String, _>("resource_id")
            .map_err(database_error)?;
        let kind = match row
            .try_get::<String, _>("resource_kind")
            .map_err(database_error)?
            .as_str()
        {
            "workspace" => ResourceKind::Workspace,
            _ => ResourceKind::Session,
        };
        if !resource_access_in(&mut tx, &user.user_id, tenant, kind, &resource)
            .await?
            .permissions
            .view
        {
            continue;
        }
        let executor = row
            .try_get::<String, _>("executor_id")
            .map_err(database_error)?;
        if !connected(state, tenant, &executor, connections).await {
            sources.insert((
                WorkspaceId::new(
                    row.try_get::<String, _>("workspace_id")
                        .map_err(database_error)?,
                ),
                executor,
            ));
        }
    }
    tx.commit().await.map_err(database_error)?;
    Ok(sources)
}

async fn connected(
    state: &AppState,
    tenant: &TenantId,
    executor: &str,
    cache: &mut BTreeMap<String, bool>,
) -> bool {
    if let Some(value) = cache.get(executor) {
        return *value;
    }
    let value = state
        .edge
        .is_connected(tenant, &ExecutorId::new(executor))
        .await;
    cache.insert(executor.to_owned(), value);
    value
}

fn signed(value: u64) -> Result<i64, HarnessError> {
    i64::try_from(value)
        .map_err(|_| HarnessError::invalid("file cursor exceeds the supported range"))
}

fn unsigned(value: i64) -> Result<u64, HarnessError> {
    u64::try_from(value).map_err(|_| HarnessError::execution("negative file position"))
}

#[handler]
pub(crate) async fn file_content(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<SessionFileContent>, ApiError> {
    let (tenant, session) = scope(request)?;
    let file_id = path_parameter(request, "file_id")?;
    SessionFileLocator::parse(&file_id)?;
    Ok(Json(
        read_file_content(app_state(depot), actor(depot), &tenant, &session, &file_id).await?,
    ))
}

pub(super) async fn read_file_content(
    state: &AppState,
    user: &ControlUser,
    tenant: &TenantId,
    session: &SessionId,
    file_id: &str,
) -> Result<SessionFileContent, HarnessError> {
    let mut tx = state.cloud.database().tenant_transaction(tenant).await?;
    resource_access_in(
        &mut tx,
        &user.user_id,
        tenant,
        ResourceKind::Session,
        session.as_str(),
    )
    .await?
    .require(ResourceAction::View)?;
    let node = sqlx::query("SELECT executor_id, node_session_id FROM control_edge_sessions WHERE tenant_id=$1 AND session_id=$2")
        .bind(tenant.as_str()).bind(session.as_str()).fetch_optional(&mut *tx).await.map_err(database_error)?;
    tx.commit().await.map_err(database_error)?;
    if let Some(node) = node {
        let executor = ExecutorId::new(
            node.try_get::<String, _>("executor_id")
                .map_err(database_error)?,
        );
        let node_session = SessionId::new(
            node.try_get::<String, _>("node_session_id")
                .map_err(database_error)?,
        );
        let value = state
            .edge
            .call(
                tenant,
                &executor,
                ApplicationOperation::SessionFileContent {
                    session_id: node_session,
                    file_id: file_id.to_owned(),
                },
            )
            .await?;
        state
            .store
            .resource_access(user, tenant, ResourceKind::Session, session.as_str())
            .await?
            .require(ResourceAction::View)?;
        serde_json::from_value(value)
            .map_err(|error| HarnessError::execution(format!("invalid Node file content: {error}")))
    } else {
        state
            .cloud
            .session_file_content_as(tenant, &user.user_id, session, file_id)
            .await
    }
}
