use std::collections::BTreeSet;

use sqlx::Row;
use ternilo_control::{ResourceKind, resource_access_in};
use ternilo_local::{searchable_event_text, session_event_category};
use ternilo_protocol::{
    HarnessError, SessionEvent, SessionId, SessionSearchFilters, SessionSearchHit,
    SessionSearchRequest, TenantId, UserId, WorkspaceId,
};

use crate::{
    CloudStore,
    store::{database_error, from_i64, set_tenant},
};

const SEARCH_SESSIONS_SQL: &str = "SELECT session.session_id, session.workspace_id, session.title, session.updated_at_ms
             FROM cloud_sessions AS session
             WHERE session.tenant_id = $1 AND session.archived_at_ms IS NULL
               AND (session.user_id = $2 OR EXISTS (
                 SELECT 1 FROM control_project_workspace_access p WHERE p.tenant_id=session.tenant_id
                 AND p.workspace_id=session.workspace_id AND p.user_id=$2) OR EXISTS (
                   SELECT 1 FROM control_resource_shares AS grant_record
                   WHERE grant_record.tenant_id = session.tenant_id AND grant_record.grantee_user_id = $2
                   AND ((grant_record.resource_kind = 'session' AND grant_record.resource_id = session.session_id)
                   OR (grant_record.resource_kind = 'workspace' AND grant_record.resource_id = session.workspace_id)))
               OR EXISTS (
                   SELECT 1 FROM control_resource_group_shares AS group_grant
                   JOIN control_permission_group_members AS group_member
                     ON group_member.tenant_id = group_grant.tenant_id AND group_member.group_id = group_grant.group_id
                   WHERE group_grant.tenant_id = session.tenant_id AND group_member.user_id = $2
                     AND ((group_grant.resource_kind = 'session' AND group_grant.resource_id = session.session_id)
                     OR (group_grant.resource_kind = 'workspace' AND group_grant.resource_id = session.workspace_id)))
               OR EXISTS (
                   SELECT 1 FROM control_resource_fork_group_sources AS source
                   JOIN control_resource_group_shares AS group_grant
                     ON group_grant.tenant_id = source.tenant_id AND group_grant.resource_kind = source.source_resource_kind
                     AND group_grant.resource_id = source.source_resource_id AND group_grant.group_id = source.group_id
                   JOIN control_permission_group_members AS group_member
                     ON group_member.tenant_id = source.tenant_id AND group_member.group_id = source.group_id
                     AND group_member.user_id = source.user_id
                   WHERE source.tenant_id = session.tenant_id AND source.session_id = session.session_id
                     AND source.user_id = $2))
               AND (CAST($3 AS TEXT) IS NULL OR session.session_id = $3)
               AND (CAST($4 AS TEXT) IS NULL OR session.workspace_id = $4)
             ORDER BY session.updated_at_ms DESC, session.session_id";

const SEARCH_EVENTS_SQL: &str = "SELECT session.session_id, session.workspace_id, session.title,
                    session.updated_at_ms, event.event
             FROM cloud_sessions AS session
             JOIN cloud_session_events AS event
               ON event.tenant_id = session.tenant_id
              AND event.session_id = session.session_id
             WHERE session.tenant_id = $1 AND session.archived_at_ms IS NULL
               AND (session.user_id = $2 OR EXISTS (
                 SELECT 1 FROM control_project_workspace_access p WHERE p.tenant_id=session.tenant_id
                 AND p.workspace_id=session.workspace_id AND p.user_id=$2) OR EXISTS (
                   SELECT 1 FROM control_resource_shares AS grant_record
                   WHERE grant_record.tenant_id = session.tenant_id AND grant_record.grantee_user_id = $2
                   AND ((grant_record.resource_kind = 'session' AND grant_record.resource_id = session.session_id)
                   OR (grant_record.resource_kind = 'workspace' AND grant_record.resource_id = session.workspace_id)))
               OR EXISTS (
                   SELECT 1 FROM control_resource_group_shares AS group_grant
                   JOIN control_permission_group_members AS group_member
                     ON group_member.tenant_id = group_grant.tenant_id AND group_member.group_id = group_grant.group_id
                   WHERE group_grant.tenant_id = session.tenant_id AND group_member.user_id = $2
                     AND ((group_grant.resource_kind = 'session' AND group_grant.resource_id = session.session_id)
                     OR (group_grant.resource_kind = 'workspace' AND group_grant.resource_id = session.workspace_id)))
               OR EXISTS (
                   SELECT 1 FROM control_resource_fork_group_sources AS source
                   JOIN control_resource_group_shares AS group_grant
                     ON group_grant.tenant_id = source.tenant_id AND group_grant.resource_kind = source.source_resource_kind
                     AND group_grant.resource_id = source.source_resource_id AND group_grant.group_id = source.group_id
                   JOIN control_permission_group_members AS group_member
                     ON group_member.tenant_id = source.tenant_id AND group_member.group_id = source.group_id
                     AND group_member.user_id = source.user_id
                   WHERE source.tenant_id = session.tenant_id AND source.session_id = session.session_id
                     AND source.user_id = $2))
               AND (CAST($3 AS TEXT) IS NULL OR session.session_id = $3)
               AND (CAST($4 AS TEXT) IS NULL OR session.workspace_id = $4)";

#[derive(Clone)]
struct SearchSession {
    session_id: SessionId,
    workspace_id: WorkspaceId,
    title: String,
    updated_at_ms: u64,
}

#[derive(Clone)]
struct SearchEvent {
    session: SearchSession,
    event: SessionEvent,
}

impl CloudStore {
    pub async fn search_sessions(
        &self,
        tenant_id: &TenantId,
        actor_id: &UserId,
        request: SessionSearchRequest,
    ) -> Result<Vec<SessionSearchHit>, HarnessError> {
        tenant_id.validate()?;
        actor_id.validate()?;
        request.validate()?;
        let session_filter = request.session_id.as_ref().map(SessionId::as_str);
        let workspace_filter = request.workspace_id.as_ref().map(WorkspaceId::as_str);
        let mut transaction = self.database.begin().await?;
        set_tenant(&mut transaction, tenant_id).await?;
        let session_rows = sqlx::query(SEARCH_SESSIONS_SQL)
            .bind(tenant_id.as_str())
            .bind(actor_id.as_str())
            .bind(session_filter)
            .bind(workspace_filter)
            .fetch_all(&mut *transaction)
            .await
            .map_err(database_error)?;
        let mut sessions = session_rows
            .iter()
            .map(decode_search_session)
            .collect::<Result<Vec<_>, HarnessError>>()?;
        let mut visible_sessions = BTreeSet::new();
        for session in &sessions {
            let access = resource_access_in(
                &mut transaction,
                actor_id,
                tenant_id,
                ResourceKind::Session,
                session.session_id.as_str(),
            )
            .await?;
            if access.permissions.view {
                visible_sessions.insert(session.session_id.clone());
            }
        }
        sessions.retain(|session| visible_sessions.contains(&session.session_id));
        let event_rows = sqlx::query(SEARCH_EVENTS_SQL)
            .bind(tenant_id.as_str())
            .bind(actor_id.as_str())
            .bind(session_filter)
            .bind(workspace_filter)
            .fetch_all(&mut *transaction)
            .await
            .map_err(database_error)?;
        let mut events = event_rows
            .into_iter()
            .map(|row| {
                Ok(SearchEvent {
                    session: decode_search_session(&row)?,
                    event: row
                        .try_get::<ternilo_storage::Json<SessionEvent>, _>("event")
                        .map_err(database_error)?
                        .0,
                })
            })
            .collect::<Result<Vec<_>, HarnessError>>()?;
        events.retain(|event| visible_sessions.contains(&event.session.session_id));
        transaction.commit().await.map_err(database_error)?;
        Ok(collect_search_hits(&sessions, &events, &request))
    }
}

fn decode_search_session(row: &sqlx::any::AnyRow) -> Result<SearchSession, HarnessError> {
    Ok(SearchSession {
        session_id: SessionId::new(
            row.try_get::<String, _>("session_id")
                .map_err(database_error)?,
        ),
        workspace_id: WorkspaceId::new(
            row.try_get::<String, _>("workspace_id")
                .map_err(database_error)?,
        ),
        title: row.try_get("title").map_err(database_error)?,
        updated_at_ms: from_i64(
            row.try_get("updated_at_ms").map_err(database_error)?,
            "cloud session search timestamp",
        )?,
    })
}

fn collect_search_hits(
    sessions: &[SearchSession],
    events: &[SearchEvent],
    request: &SessionSearchRequest,
) -> Vec<SessionSearchHit> {
    let needle = request.query.trim().to_lowercase();
    let limit = request.limit as usize;
    let mut hits = Vec::new();
    if request.filters == SessionSearchFilters::default() {
        hits.extend(
            sessions
                .iter()
                .filter(|session| session.title.to_lowercase().contains(&needle))
                .map(|session| SessionSearchHit {
                    session_id: session.session_id.clone(),
                    workspace_id: session.workspace_id.clone(),
                    title: session.title.clone(),
                    updated_at_ms: session.updated_at_ms,
                    event_seq: None,
                    occurred_at_ms: None,
                    run_id: None,
                    category: None,
                    excerpt: session.title.clone(),
                })
                .take(limit),
        );
    }
    if hits.len() == limit {
        return hits;
    }
    let mut event_hits = events
        .iter()
        .filter_map(|candidate| {
            let event = &candidate.event;
            let category = session_event_category(&event.kind);
            if request
                .filters
                .run_id
                .as_ref()
                .is_some_and(|run_id| run_id != &event.run_id)
                || request
                    .filters
                    .category
                    .is_some_and(|requested| requested != category)
                || request
                    .filters
                    .occurred_after_ms
                    .is_some_and(|after| event.occurred_at_ms < after)
                || request
                    .filters
                    .occurred_before_ms
                    .is_some_and(|before| event.occurred_at_ms > before)
            {
                return None;
            }
            let searchable = searchable_event_text(event);
            if !searchable.to_lowercase().contains(&needle) {
                return None;
            }
            Some(SessionSearchHit {
                session_id: candidate.session.session_id.clone(),
                workspace_id: candidate.session.workspace_id.clone(),
                title: candidate.session.title.clone(),
                updated_at_ms: candidate.session.updated_at_ms,
                event_seq: Some(event.seq),
                occurred_at_ms: Some(event.occurred_at_ms),
                run_id: Some(event.run_id.clone()),
                category: Some(category),
                excerpt: search_excerpt(&searchable, &needle),
            })
        })
        .collect::<Vec<_>>();
    event_hits.sort_by(|left, right| {
        right
            .occurred_at_ms
            .cmp(&left.occurred_at_ms)
            .then_with(|| left.session_id.as_str().cmp(right.session_id.as_str()))
            .then_with(|| right.event_seq.cmp(&left.event_seq))
    });
    hits.extend(event_hits.into_iter().take(limit - hits.len()));
    hits
}

fn search_excerpt(value: &str, needle: &str) -> String {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let lowered = normalized.to_lowercase();
    let match_character = lowered
        .find(needle)
        .map_or(0, |offset| lowered[..offset].chars().count());
    let characters = normalized.chars().collect::<Vec<_>>();
    let start = match_character.saturating_sub(80);
    let end = (start + 240).min(characters.len());
    let body = characters[start..end].iter().collect::<String>();
    format!(
        "{}{}{}",
        if start > 0 { "…" } else { "" },
        body,
        if end < characters.len() { "…" } else { "" },
    )
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use ternilo_protocol::{RunId, SessionEventCategory, SessionEventKind, ToolCall};

    use super::*;

    fn request(query: &str) -> SessionSearchRequest {
        SessionSearchRequest {
            query: query.to_owned(),
            session_id: None,
            workspace_id: None,
            filters: SessionSearchFilters::default(),
            limit: 100,
        }
    }

    fn session() -> SearchSession {
        SearchSession {
            session_id: SessionId::new("cloud-session"),
            workspace_id: WorkspaceId::new("cloud-workspace"),
            title: "Cobalt release".to_owned(),
            updated_at_ms: 500,
        }
    }

    fn event(seq: u64, occurred_at_ms: u64, kind: SessionEventKind) -> SearchEvent {
        SearchEvent {
            session: session(),
            event: SessionEvent {
                seq,
                occurred_at_ms,
                run_id: RunId::new("cloud-run"),
                kind,
            },
        }
    }

    #[test]
    fn cloud_search_projects_title_reasoning_and_tool_events() {
        let sessions = vec![session()];
        let events = vec![
            event(
                1,
                100,
                SessionEventKind::AssistantReasoningDelta {
                    step: 1,
                    delta: "reasoning-cobalt private analysis".to_owned(),
                },
            ),
            event(
                2,
                200,
                SessionEventKind::ToolCallStarted {
                    call: ToolCall {
                        id: "call-1".to_owned(),
                        name: "workspace_search".to_owned(),
                        arguments: json!({ "query": "tool-cobalt" }),
                        presentation: None,
                    },
                },
            ),
        ];

        let title = collect_search_hits(&sessions, &events, &request("cobalt release"));
        assert_eq!(title[0].event_seq, None);
        let reasoning = collect_search_hits(&sessions, &events, &request("reasoning-cobalt"));
        assert_eq!(reasoning[0].event_seq, Some(1));
        assert_eq!(reasoning[0].category, Some(SessionEventCategory::Assistant));
        let tool = collect_search_hits(&sessions, &events, &request("tool-cobalt"));
        assert_eq!(tool[0].event_seq, Some(2));
        assert_eq!(tool[0].category, Some(SessionEventCategory::Tool));
        assert!(tool[0].excerpt.contains("tool-cobalt"));
    }
}
