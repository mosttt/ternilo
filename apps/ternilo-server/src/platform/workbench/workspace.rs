use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};

use futures_util::future::join_all;
use salvo_core::{
    http::{StatusCode, header},
    prelude::{Depot, Json, Request, Response, handler},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use ternilo_control::{ControlUser, EdgeSessionRecord, ResourceAction, ResourceKind};
use ternilo_protocol::{
    HarnessError, SessionId, SessionSearchFilters, SessionSearchHit, SessionSearchRequest,
    TenantId, WorkspaceId,
};
use ternilo_transport::{ApplicationOperation, ExecutorId};

use crate::platform::{
    http::{ApiError, invalid_request, now_ms, path_parameter, tenant_parameter},
    state::{AppState, actor, app_state},
};

use super::{
    edge_adapter::authorize_edge_mutation,
    types::{
        CreateDirectoryRequest, DirectoryQuery, ExecutionTarget, RenameWorkspaceRequest,
        SearchQuery, WorkbenchSession, WorkbenchState, WorkbenchWorkspace,
    },
};

const EDGE_SEARCH_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WorkbenchQuery {
    #[serde(default)]
    pub(crate) online_computers_only: bool,
}

#[handler]
pub(crate) async fn execution_targets(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Value>, ApiError> {
    let tenant_id = tenant_parameter(request)?;
    let state = app_state(depot);
    let records = state
        .store
        .list_owned_executors(actor(depot), &tenant_id)
        .await?;
    let mut targets = Vec::with_capacity(records.len());
    for record in records {
        let connected = state
            .edge
            .is_connected(&tenant_id, &record.executor_id)
            .await;
        targets.push(ExecutionTarget::from_record(record, connected));
    }
    Ok(Json(json!({ "executors": targets })))
}

#[handler]
pub(crate) async fn node_directory_listing(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Value>, ApiError> {
    let tenant_id = tenant_parameter(request)?;
    let executor_id = ExecutorId::new(path_parameter(request, "executor_id")?);
    let query = request
        .parse_queries::<DirectoryQuery>()
        .map_err(invalid_request)?;
    let state = app_state(depot);
    state
        .store
        .owned_executor(actor(depot), &tenant_id, &executor_id)
        .await?;
    let value = state
        .edge
        .call(
            &tenant_id,
            &executor_id,
            ApplicationOperation::DirectoryList { path: query.path },
        )
        .await?;
    Ok(Json(value))
}

#[derive(Deserialize)]
struct NodeWorkspaceLocation {
    path: String,
    home: Option<String>,
    created_at_ms: u64,
}

#[derive(Serialize)]
pub(crate) struct WorkspaceLocation {
    status: &'static str,
    path: Option<String>,
    home: Option<String>,
    created_at_ms: Option<u64>,
}

impl WorkspaceLocation {
    fn absent(status: &'static str) -> Self {
        Self {
            status,
            path: None,
            home: None,
            created_at_ms: None,
        }
    }
}

#[handler]
pub(crate) async fn workspace_location(
    request: &mut Request,
    depot: &mut Depot,
    response: &mut Response,
) -> Result<Json<WorkspaceLocation>, ApiError> {
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        "no-store".parse().expect("static header value"),
    );
    let tenant_id = tenant_parameter(request)?;
    let workspace_id = WorkspaceId::new(path_parameter(request, "workspace_id")?);
    let state = app_state(depot);
    let user = actor(depot);
    state
        .store
        .resource_access(
            user,
            &tenant_id,
            ResourceKind::Workspace,
            workspace_id.as_str(),
        )
        .await?
        .require(ResourceAction::View)?;
    let target = super::placement::PlacementResolver::new(state, user, &tenant_id)
        .workspace(&workspace_id)
        .await?;
    let super::placement::WorkspaceTarget::Edge {
        executor_id,
        node_workspace_id,
        ..
    } = target
    else {
        return Ok(Json(WorkspaceLocation::absent("unavailable")));
    };
    state
        .store
        .owned_executor(user, &tenant_id, &executor_id)
        .await
        .map_err(|error| {
            if error.code == ternilo_protocol::ErrorCode::InvalidInput {
                HarnessError::policy("workspace locations are private to the machine owner")
            } else {
                error
            }
        })?;
    if !state.edge.is_connected(&tenant_id, &executor_id).await {
        return Ok(Json(WorkspaceLocation::absent("offline")));
    }
    let value = match state
        .edge
        .call_with_timeout(
            &tenant_id,
            &executor_id,
            ApplicationOperation::WorkspaceLocation {
                workspace_id: node_workspace_id,
            },
            Duration::from_secs(5),
        )
        .await
    {
        Ok(value) => value,
        Err(_) if !state.edge.is_connected(&tenant_id, &executor_id).await => {
            return Ok(Json(WorkspaceLocation::absent("offline")));
        }
        Err(error) => return Err(error.into()),
    };
    let location: NodeWorkspaceLocation = serde_json::from_value(value)
        .map_err(|_| HarnessError::execution("invalid Node workspace location response"))?;
    Ok(Json(WorkspaceLocation {
        status: "available",
        path: Some(location.path),
        home: location.home,
        created_at_ms: Some(location.created_at_ms),
    }))
}

#[handler]
pub(crate) async fn make_node_directory(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let tenant_id = tenant_parameter(request)?;
    let executor_id = ExecutorId::new(path_parameter(request, "executor_id")?);
    let body = request
        .parse_json::<CreateDirectoryRequest>()
        .await
        .map_err(invalid_request)?;
    let state = app_state(depot);
    authorize_edge_mutation(state, actor(depot), &tenant_id).await?;
    state
        .store
        .owned_executor(actor(depot), &tenant_id, &executor_id)
        .await?;
    let value = state
        .edge
        .call(
            &tenant_id,
            &executor_id,
            ApplicationOperation::DirectoryCreate {
                parent: body.parent,
                name: body.name,
            },
        )
        .await?;
    Ok((StatusCode::CREATED, Json(value)))
}

pub(crate) async fn load_state(
    state: &AppState,
    user: &ControlUser,
    tenant_id: &TenantId,
) -> Result<WorkbenchState, HarnessError> {
    load_state_by_archive(state, user, tenant_id, false, false).await
}

pub(crate) async fn load_state_filtered(
    state: &AppState,
    user: &ControlUser,
    tenant_id: &TenantId,
    online_only: bool,
) -> Result<WorkbenchState, HarnessError> {
    load_state_by_archive(state, user, tenant_id, false, online_only).await
}

pub(super) async fn load_archived_sessions(
    state: &AppState,
    user: &ControlUser,
    tenant_id: &TenantId,
    online_only: bool,
) -> Result<Vec<WorkbenchSession>, HarnessError> {
    Ok(
        load_state_by_archive(state, user, tenant_id, true, online_only)
            .await?
            .sessions,
    )
}

#[expect(
    clippy::too_many_lines,
    reason = "Keep filtered discovery, current access and both session projections in one authorization flow."
)]
async fn load_state_by_archive(
    state: &AppState,
    user: &ControlUser,
    tenant_id: &TenantId,
    archived: bool,
    online_only: bool,
) -> Result<WorkbenchState, HarnessError> {
    let online = if online_only {
        Some(state.edge.online_executor_ids(tenant_id).await?)
    } else {
        None
    };
    super::discovery::discover(state, user, tenant_id, online.as_deref()).await?;
    let records = if let Some(ids) = &online {
        state
            .store
            .list_accessible_workspaces_on_executors(user, tenant_id, ids)
            .await?
    } else {
        state
            .store
            .list_accessible_workspaces(user, tenant_id)
            .await?
    };
    let mut workspaces = Vec::with_capacity(records.len());
    let mut paths = BTreeMap::new();
    for record in records {
        let connected = if let Some(executor_id) = &record.executor_id {
            state.edge.is_connected(tenant_id, executor_id).await
        } else {
            false
        };
        let access = state
            .store
            .resource_access(
                user,
                tenant_id,
                ResourceKind::Workspace,
                record.workspace_id.as_str(),
            )
            .await?;
        let workspace = WorkbenchWorkspace::from_record(record, connected).with_access(access);
        paths.insert(workspace.workspace_id.clone(), workspace.path.clone());
        workspaces.push(workspace);
    }
    let cloud = if archived {
        state
            .cloud
            .list_accessible_archived_sessions(tenant_id, &user.user_id, 500)
            .await?
    } else {
        state
            .cloud
            .list_accessible_sessions(tenant_id, &user.user_id, 500)
            .await?
    }
    .into_iter()
    .map(|session| {
        let path = paths
            .get(&session.workspace_id)
            .cloned()
            .unwrap_or_else(|| "云端 / 未分组".to_owned());
        WorkbenchSession::cloud(session, path)
    });
    let edge = if let Some(ids) = &online {
        state
            .store
            .list_accessible_edge_sessions_on_executors(user, tenant_id, ids)
            .await?
    } else {
        state
            .store
            .list_accessible_edge_sessions(user, tenant_id)
            .await?
    }
    .into_iter()
    .filter(|session| session.metadata.archived_at_ms.is_some() == archived)
    .map(|session| {
        let path = paths
            .get(&session.workspace_id)
            .cloned()
            .unwrap_or_else(|| "此电脑 / 未分组".to_owned());
        WorkbenchSession::edge(session, path)
    });
    let mut sessions = cloud.chain(edge).collect::<Vec<_>>();
    for session in &mut sessions {
        session.access = Some(
            state
                .store
                .resource_access(
                    user,
                    tenant_id,
                    ResourceKind::Session,
                    session.identity.session_id.as_str(),
                )
                .await?,
        );
    }
    let mut session_ids = BTreeSet::new();
    if sessions
        .iter()
        .any(|session| !session_ids.insert(session.identity.session_id.clone()))
    {
        return Err(HarnessError::execution(
            "session identifier has conflicting cloud and local-node placements",
        ));
    }
    sessions.sort_by(|left, right| {
        right.updated_at_ms.cmp(&left.updated_at_ms).then_with(|| {
            left.identity
                .session_id
                .as_str()
                .cmp(right.identity.session_id.as_str())
        })
    });
    Ok(WorkbenchState {
        workspaces,
        sessions,
    })
}

#[handler]
pub(crate) async fn workbench_state(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<WorkbenchState>, ApiError> {
    let tenant_id = tenant_parameter(request)?;
    let query = request
        .parse_queries::<WorkbenchQuery>()
        .map_err(invalid_request)?;
    Ok(Json(
        load_state_filtered(
            app_state(depot),
            actor(depot),
            &tenant_id,
            query.online_computers_only,
        )
        .await?,
    ))
}

#[handler]
pub(crate) async fn rename_workspace(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<WorkbenchWorkspace>, ApiError> {
    let tenant_id = tenant_parameter(request)?;
    let workspace_id = WorkspaceId::new(path_parameter(request, "workspace_id")?);
    let body = request
        .parse_json::<RenameWorkspaceRequest>()
        .await
        .map_err(invalid_request)?;
    let state = app_state(depot);
    let record = state
        .store
        .rename_owned_workspace(
            actor(depot),
            &tenant_id,
            &workspace_id,
            &body.title,
            now_ms()?,
        )
        .await?;
    let connected = if let Some(executor_id) = &record.executor_id {
        state.edge.is_connected(&tenant_id, executor_id).await
    } else {
        false
    };
    let access = state
        .store
        .resource_access(
            actor(depot),
            &tenant_id,
            ResourceKind::Workspace,
            record.workspace_id.as_str(),
        )
        .await?;
    let workspace = WorkbenchWorkspace::from_record(record, connected).with_access(access);
    Ok(Json(workspace))
}

#[handler]
pub(crate) async fn unregister_workspace(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<StatusCode, ApiError> {
    let tenant_id = tenant_parameter(request)?;
    let workspace_id = WorkspaceId::new(path_parameter(request, "workspace_id")?);
    app_state(depot)
        .store
        .unregister_owned_workspace(actor(depot), &tenant_id, &workspace_id, now_ms()?)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[handler]
pub(crate) async fn search_sessions(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Vec<SessionSearchHit>>, ApiError> {
    let tenant_id = tenant_parameter(request)?;
    let query = request
        .parse_queries::<SearchQuery>()
        .map_err(invalid_request)?;
    let Some((needle, limit)) = normalize_search_query(&query)? else {
        return Ok(Json(Vec::new()));
    };
    let state = app_state(depot);
    let user = actor(depot);
    let sessions = load_state_filtered(state, user, &tenant_id, query.online_computers_only)
        .await?
        .sessions
        .into_iter()
        .filter(|session| session.archived_at_ms.is_none() && !session.blank)
        .collect::<Vec<_>>();
    let search_request = SessionSearchRequest {
        query: needle.clone(),
        session_id: None,
        workspace_id: None,
        filters: SessionSearchFilters::default(),
        limit: u32::try_from(limit).expect("normalized search limit is at most 100"),
    };
    let title_hits = title_search_hits(sessions.clone(), &needle, sessions.len());
    let cloud_hits = state
        .cloud
        .search_sessions(&tenant_id, &user.user_id, search_request.clone())
        .await?;
    let edge_hits = edge_search_hits(state, user, &tenant_id, &sessions, &search_request).await?;
    Ok(Json(merge_search_hits(
        [title_hits, cloud_hits, edge_hits],
        limit,
    )))
}

fn normalize_search_query(query: &SearchQuery) -> Result<Option<(String, usize)>, HarnessError> {
    let needle = query.query.trim().to_lowercase();
    if needle.is_empty() {
        return Ok(None);
    }
    if needle.encode_utf16().count() > 500 {
        return Err(HarnessError::invalid(
            "session search query may not exceed 500 UTF-16 code units",
        ));
    }
    let limit = query.limit.unwrap_or(100).clamp(1, 100) as usize;
    Ok(Some((needle, limit)))
}

fn title_search_hits(
    sessions: Vec<WorkbenchSession>,
    needle: &str,
    limit: usize,
) -> Vec<SessionSearchHit> {
    sessions
        .into_iter()
        .filter(|session| session.title.to_lowercase().contains(needle))
        .take(limit)
        .map(|session| SessionSearchHit {
            session_id: session.identity.session_id,
            workspace_id: session.workspace_id,
            title: session.title.clone(),
            updated_at_ms: session.updated_at_ms,
            event_seq: None,
            occurred_at_ms: None,
            run_id: None,
            category: None,
            excerpt: session.title,
        })
        .collect()
}

async fn edge_search_hits(
    state: &AppState,
    user: &ControlUser,
    tenant_id: &TenantId,
    visible_sessions: &[WorkbenchSession],
    request: &SessionSearchRequest,
) -> Result<Vec<SessionSearchHit>, HarnessError> {
    let visible = visible_sessions
        .iter()
        .cloned()
        .map(|session| (session.identity.session_id.clone(), session))
        .collect::<BTreeMap<_, _>>();
    let mut groups = BTreeMap::<String, (ExecutorId, Vec<EdgeSessionRecord>)>::new();
    for mapping in state
        .store
        .list_accessible_edge_sessions(user, tenant_id)
        .await?
    {
        if !visible.contains_key(&mapping.session_id) {
            continue;
        }
        groups
            .entry(mapping.executor_id.as_str().to_owned())
            .or_insert_with(|| (mapping.executor_id.clone(), Vec::new()))
            .1
            .push(mapping);
    }
    let connected = join_all(
        groups
            .into_values()
            .map(|(executor_id, mappings)| async move {
                state
                    .edge
                    .is_connected(tenant_id, &executor_id)
                    .await
                    .then_some((executor_id, mappings))
            }),
    )
    .await;
    let batches = join_all(
        connected
            .into_iter()
            .flatten()
            .map(|(executor_id, mappings)| {
                let request = request.clone();
                async move {
                    let value = state
                        .edge
                        .call_with_timeout(
                            tenant_id,
                            &executor_id,
                            ApplicationOperation::SessionSearch {
                                query: request.query,
                                session_id: None,
                                workspace_id: None,
                                filters: request.filters,
                                limit: request.limit,
                            },
                            EDGE_SEARCH_TIMEOUT,
                        )
                        .await?;
                    let hits = serde_json::from_value::<Vec<SessionSearchHit>>(value).map_err(
                        |error| {
                            HarnessError::execution(format!(
                                "decode Node Session search response: {error}"
                            ))
                        },
                    )?;
                    Ok::<_, HarnessError>((mappings, hits))
                }
            }),
    )
    .await;
    Ok(batches
        .into_iter()
        .filter_map(Result::ok)
        .flat_map(|(mappings, hits)| translate_edge_hits(&mappings, &visible, hits))
        .collect())
}

fn translate_edge_hits(
    mappings: &[EdgeSessionRecord],
    visible: &BTreeMap<SessionId, WorkbenchSession>,
    hits: Vec<SessionSearchHit>,
) -> Vec<SessionSearchHit> {
    let by_node = mappings
        .iter()
        .map(|mapping| (mapping.node_session_id.clone(), mapping))
        .collect::<BTreeMap<_, _>>();
    hits.into_iter()
        .filter_map(|hit| {
            let mapping = by_node.get(&hit.session_id)?;
            let session = visible.get(&mapping.session_id)?;
            Some(SessionSearchHit {
                session_id: mapping.session_id.clone(),
                workspace_id: mapping.workspace_id.clone(),
                title: session.title.clone(),
                updated_at_ms: session.updated_at_ms,
                event_seq: hit.event_seq,
                occurred_at_ms: hit.occurred_at_ms,
                run_id: hit.run_id,
                category: hit.category,
                excerpt: hit.excerpt,
            })
        })
        .collect()
}

fn merge_search_hits(
    sources: impl IntoIterator<Item = Vec<SessionSearchHit>>,
    limit: usize,
) -> Vec<SessionSearchHit> {
    let mut hits = sources.into_iter().flatten().collect::<Vec<_>>();
    hits.sort_by(|left, right| {
        left.event_seq
            .is_some()
            .cmp(&right.event_seq.is_some())
            .then_with(|| {
                right
                    .occurred_at_ms
                    .unwrap_or(right.updated_at_ms)
                    .cmp(&left.occurred_at_ms.unwrap_or(left.updated_at_ms))
            })
            .then_with(|| right.updated_at_ms.cmp(&left.updated_at_ms))
            .then_with(|| left.session_id.as_str().cmp(right.session_id.as_str()))
            .then_with(|| left.event_seq.cmp(&right.event_seq))
    });
    let mut seen = BTreeSet::new();
    hits.retain(|hit| seen.insert((hit.session_id.clone(), hit.event_seq)));
    hits.truncate(limit);
    hits
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use ternilo_control::{EdgeSessionMetadata, EdgeSessionRecord, WorkspacePlacement};
    use ternilo_protocol::{
        AgentId, PermissionPreset, RunId, SessionEventCategory, SessionId, SessionIdentity,
        SessionMode, SessionSearchHit, TenantId, UserId, WorkspaceId,
    };
    use ternilo_transport::ExecutorId;

    use super::{
        SearchQuery, WorkbenchSession, merge_search_hits, normalize_search_query,
        title_search_hits, translate_edge_hits,
    };

    fn session(title: &str) -> WorkbenchSession {
        WorkbenchSession {
            access: None,
            identity: SessionIdentity {
                tenant_id: TenantId::new("tenant-a"),
                user_id: UserId::new("user-a"),
                agent_id: AgentId::new("agent-a"),
                session_id: SessionId::new("session-a"),
            },
            workspace_id: WorkspaceId::new("workspace-a"),
            placement: ternilo_control::WorkspacePlacement::Cloud,
            workspace_path: "cloud / workspace-a".to_owned(),
            parent_session_id: None,
            subagent: None,
            title: title.to_owned(),
            archived_at_ms: None,
            blank: false,
            permissions: PermissionPreset::WorkspaceWrite,
            model: json!({}),
            agent_preset: "default".to_owned(),
            preset_plugins: Vec::new(),
            profile_plugins: Vec::new(),
            mode: SessionMode::Execute,
            model_token_limit: Some(100),
            created_at_ms: 10,
            updated_at_ms: 20,
        }
    }

    #[test]
    fn search_handler_payload_uses_canonical_session_search_shape() {
        let payload = serde_json::to_value(title_search_hits(
            vec![session("Cobalt needle")],
            "cobalt",
            100,
        ))
        .unwrap();
        assert_eq!(
            payload,
            json!([{
                "session_id": "session-a",
                "workspace_id": "workspace-a",
                "title": "Cobalt needle",
                "updated_at_ms": 20,
                "event_seq": null,
                "occurred_at_ms": null,
                "run_id": null,
                "category": null,
                "excerpt": "Cobalt needle"
            }])
        );
    }

    #[test]
    fn utf16_search_limit_matches_browser_query_semantics() {
        let (_, limit) = normalize_search_query(&SearchQuery {
            query: "🦀".repeat(250),
            limit: Some(500),
            online_computers_only: false,
        })
        .unwrap()
        .unwrap();
        assert_eq!(limit, 100);
        assert!(
            normalize_search_query(&SearchQuery {
                query: "🦀".repeat(251),
                limit: None,
                online_computers_only: false,
            })
            .is_err()
        );
    }

    #[test]
    fn merged_search_keeps_titles_then_recent_events_and_deduplicates_session_events() {
        let title = SessionSearchHit {
            session_id: SessionId::new("session-a"),
            workspace_id: WorkspaceId::new("workspace-a"),
            title: "Cobalt title".to_owned(),
            updated_at_ms: 20,
            event_seq: None,
            occurred_at_ms: None,
            run_id: None,
            category: None,
            excerpt: "Cobalt title".to_owned(),
        };
        let event = |session_id: &str, seq: u64, occurred_at_ms: u64| SessionSearchHit {
            session_id: SessionId::new(session_id),
            workspace_id: WorkspaceId::new("workspace-a"),
            title: session_id.to_owned(),
            updated_at_ms: occurred_at_ms,
            event_seq: Some(seq),
            occurred_at_ms: Some(occurred_at_ms),
            run_id: Some(RunId::new("run-a")),
            category: Some(SessionEventCategory::Assistant),
            excerpt: "canonical content".to_owned(),
        };
        let merged = merge_search_hits(
            [
                vec![title.clone()],
                vec![title, event("session-a", 4, 40)],
                vec![event("session-b", 8, 80), event("session-b", 8, 80)],
            ],
            10,
        );
        assert_eq!(
            merged
                .iter()
                .map(|hit| (hit.session_id.as_str(), hit.event_seq))
                .collect::<Vec<_>>(),
            vec![
                ("session-a", None),
                ("session-b", Some(8)),
                ("session-a", Some(4))
            ]
        );
    }

    #[test]
    fn edge_search_translation_only_exposes_control_visible_mappings() {
        let mut visible_session = session("Control title");
        visible_session.placement = WorkspacePlacement::LocalNode;
        let visible = [(
            visible_session.identity.session_id.clone(),
            visible_session.clone(),
        )]
        .into_iter()
        .collect();
        let mapping = EdgeSessionRecord {
            tenant_id: TenantId::new("tenant-a"),
            session_id: visible_session.identity.session_id.clone(),
            workspace_id: visible_session.workspace_id.clone(),
            executor_id: ExecutorId::new("node-a"),
            owner_user_id: UserId::new("user-a"),
            node_session_id: SessionId::new("node-session-a"),
            metadata: EdgeSessionMetadata {
                server_model: None,
                parent_session_id: None,
                subagent: None,
                title: "Control title".to_owned(),
                archived_at_ms: None,
                blank: false,
                permissions: PermissionPreset::WorkspaceWrite,
                model: json!({}),
                agent_preset: "standard".to_owned(),
                preset_plugins: Vec::new(),
                profile_plugins: Vec::new(),
                mode: SessionMode::Execute,
                created_at_ms: 10,
                updated_at_ms: 20,
            },
            last_event_seq: Some(3),
            created_at_ms: 10,
            updated_at_ms: 20,
        };
        let node_hit = |session_id: &str| SessionSearchHit {
            session_id: SessionId::new(session_id),
            workspace_id: WorkspaceId::new("node-workspace"),
            title: "Node title".to_owned(),
            updated_at_ms: 999,
            event_seq: Some(3),
            occurred_at_ms: Some(19),
            run_id: Some(RunId::new("node-run")),
            category: Some(SessionEventCategory::Tool),
            excerpt: "tool result".to_owned(),
        };
        let translated = translate_edge_hits(
            &[mapping],
            &visible,
            vec![node_hit("node-session-a"), node_hit("foreign-session")],
        );
        assert_eq!(translated.len(), 1);
        assert_eq!(translated[0].session_id.as_str(), "session-a");
        assert_eq!(translated[0].workspace_id.as_str(), "workspace-a");
        assert_eq!(translated[0].title, "Control title");
        assert_eq!(translated[0].updated_at_ms, 20);
        assert_eq!(translated[0].excerpt, "tool result");
    }
}
