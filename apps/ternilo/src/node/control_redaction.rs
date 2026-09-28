use std::collections::BTreeMap;
use ternilo_local::LocalApplication;

use serde_json::Value;
use ternilo_protocol::{HarnessError, SessionEvent, SessionId, SessionSearchHit};
use ternilo_transport::ApplicationOperation;

const LOCAL_WORKSPACE_MARKER: &str = "<local-workspace>";

pub(crate) async fn operation_roots(
    application: &LocalApplication,
    operation: &ApplicationOperation,
) -> Vec<String> {
    let mut roots = match operation {
        ApplicationOperation::Snapshot
        | ApplicationOperation::PendingQuestions { session_id: None }
        | ApplicationOperation::AnswerQuestion { .. } => {
            let snapshot = application.snapshot().await;
            snapshot
                .workspaces
                .into_iter()
                .map(|workspace| workspace.path)
                .chain(
                    snapshot
                        .sessions
                        .into_iter()
                        .map(|session| session.workspace_path),
                )
                .collect::<Vec<_>>()
        }
        // This explicit, ephemeral owner request returns the original path only in its reply.
        ApplicationOperation::WorkspaceLocation { .. } => return Vec::new(),
        ApplicationOperation::WorkspaceRename { workspace_id, .. }
        | ApplicationOperation::WorkspaceUnregister { workspace_id }
        | ApplicationOperation::SessionCreate { workspace_id, .. } => application
            .snapshot()
            .await
            .workspaces
            .into_iter()
            .find(|workspace| workspace.workspace_id == *workspace_id)
            .map(|workspace| workspace.path)
            .into_iter()
            .collect(),
        _ => {
            let Some(session_id) = operation_session(operation) else {
                return Vec::new();
            };
            application
                .snapshot()
                .await
                .sessions
                .into_iter()
                .find(|session| session.identity.session_id == *session_id)
                .map(|session| session.workspace_path)
                .into_iter()
                .collect()
        }
    };
    // Replace nested workspace roots before their parents in a combined snapshot.
    roots.sort_by(|left, right| right.len().cmp(&left.len()).then_with(|| left.cmp(right)));
    roots.dedup();
    roots
}

fn operation_session(operation: &ApplicationOperation) -> Option<&SessionId> {
    match operation {
        ApplicationOperation::SessionUpdate { session_id, .. }
        | ApplicationOperation::SessionFork { session_id, .. }
        | ApplicationOperation::SessionArchive { session_id }
        | ApplicationOperation::SessionRestore { session_id }
        | ApplicationOperation::SessionDelete { session_id }
        | ApplicationOperation::SessionEvents { session_id, .. }
        | ApplicationOperation::SessionPlugins { session_id }
        | ApplicationOperation::SessionCommands { session_id }
        | ApplicationOperation::SessionServices { session_id }
        | ApplicationOperation::SessionServiceStart { session_id, .. }
        | ApplicationOperation::SessionServiceStop { session_id, .. }
        | ApplicationOperation::SessionProjection { session_id }
        | ApplicationOperation::SessionTelemetry { session_id }
        | ApplicationOperation::SessionSkills { session_id }
        | ApplicationOperation::SessionSkillResolve { session_id, .. }
        | ApplicationOperation::SessionStats { session_id }
        | ApplicationOperation::SessionExport { session_id }
        | ApplicationOperation::SessionFeedback { session_id, .. }
        | ApplicationOperation::SessionCommandFeedback { session_id, .. }
        | ApplicationOperation::SessionSubagentFollowup { session_id, .. }
        | ApplicationOperation::SessionSubagentInterrupt { session_id, .. }
        | ApplicationOperation::SessionAgentTeamSnapshot { session_id }
        | ApplicationOperation::SessionAgentTeamTaskCreate { session_id, .. }
        | ApplicationOperation::SessionAgentTeamTaskReplace { session_id, .. }
        | ApplicationOperation::SessionAgentTeamTaskDelete { session_id, .. }
        | ApplicationOperation::SessionAgentTeamMessageSend { session_id, .. }
        | ApplicationOperation::SessionAgentTeamMessageRead { session_id, .. }
        | ApplicationOperation::SessionInbox { session_id }
        | ApplicationOperation::SessionSubmit { session_id, .. }
        | ApplicationOperation::SessionQueueEdit { session_id, .. }
        | ApplicationOperation::SessionQueueRemove { session_id, .. }
        | ApplicationOperation::SessionQueueSteer { session_id, .. }
        | ApplicationOperation::SessionTurn { session_id, .. }
        | ApplicationOperation::SessionSkillTurn { session_id, .. }
        | ApplicationOperation::PendingQuestions {
            session_id: Some(session_id),
        } => Some(session_id),
        // Directory picking needs original paths; search already redacts each hit's own root.
        _ => None,
    }
}

pub(crate) fn application_result(
    mut result: Result<Value, HarnessError>,
    roots: &[String],
) -> Result<Value, HarnessError> {
    for root in roots {
        match &mut result {
            Ok(value) => redact_value(value, root),
            Err(error) => redact_text(&mut error.message, root),
        }
    }
    result
}

pub(crate) fn events(
    events: &mut [SessionEvent],
    workspace_path: &str,
) -> Result<(), HarnessError> {
    if workspace_path.is_empty() {
        return Ok(());
    }
    for event in events {
        let mut value = serde_json::to_value(&*event).map_err(|error| {
            HarnessError::execution(format!("encode Node event for Control redaction: {error}"))
        })?;
        redact_value(&mut value, workspace_path);
        *event = serde_json::from_value(value).map_err(|error| {
            HarnessError::execution(format!("decode redacted Node event: {error}"))
        })?;
    }
    Ok(())
}

pub(crate) async fn search_hits(application: &LocalApplication, hits: &mut [SessionSearchHit]) {
    let paths = application
        .snapshot()
        .await
        .sessions
        .into_iter()
        .map(|session| (session.identity.session_id, session.workspace_path))
        .collect::<BTreeMap<_, _>>();
    redact_search_hits(hits, &paths);
}

fn redact_search_hits(hits: &mut [SessionSearchHit], paths: &BTreeMap<SessionId, String>) {
    for hit in hits {
        if let Some(workspace_path) = paths.get(&hit.session_id) {
            redact_text(&mut hit.excerpt, workspace_path);
        }
    }
}

fn redact_value(value: &mut Value, workspace_path: &str) {
    if workspace_path.is_empty() {
        return;
    }
    match value {
        Value::String(text) => {
            redact_text(text, workspace_path);
        }
        Value::Array(items) => {
            for item in items {
                redact_value(item, workspace_path);
            }
        }
        Value::Object(object) => {
            for item in object.values_mut() {
                redact_value(item, workspace_path);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn redact_text(text: &mut String, workspace_path: &str) {
    if !workspace_path.is_empty() && text.contains(workspace_path) {
        *text = text.replace(workspace_path, LOCAL_WORKSPACE_MARKER);
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use ternilo_protocol::{RunId, SessionEventKind, WorkspaceId};

    use super::*;

    #[test]
    fn recursively_redacts_only_the_exact_workspace_root() {
        let mut value = json!({
            "system_prompt": "Working directory: /home/alice/private/project. Read /home/alice/private/project/src.",
            "nested": ["/home/alice/private/project", "/home/alice/another"],
            "count": 2,
        });
        redact_value(&mut value, "/home/alice/private/project");
        assert_eq!(
            value,
            json!({
                "system_prompt": "Working directory: <local-workspace>. Read <local-workspace>/src.",
                "nested": ["<local-workspace>", "/home/alice/another"],
                "count": 2,
            })
        );
    }

    #[test]
    fn outbound_event_copy_is_redacted_without_touching_the_canonical_event() {
        let workspace = "/home/alice/private/project";
        let canonical = SessionEvent {
            seq: 4,
            occurred_at_ms: 1_234,
            run_id: RunId::new("run-local"),
            kind: SessionEventKind::ModelRequestStarted {
                step: 2,
                system_prompt: format!("Working directory: {workspace}"),
            },
        };
        let mut outbound = vec![canonical.clone()];

        events(&mut outbound, workspace).unwrap();

        let SessionEventKind::ModelRequestStarted {
            system_prompt: local_prompt,
            ..
        } = &canonical.kind
        else {
            panic!("canonical event changed kind")
        };
        assert_eq!(
            local_prompt,
            "Working directory: /home/alice/private/project"
        );
        let SessionEventKind::ModelRequestStarted {
            system_prompt: outbound_prompt,
            ..
        } = &outbound[0].kind
        else {
            panic!("outbound event changed kind")
        };
        assert_eq!(outbound_prompt, "Working directory: <local-workspace>");
    }

    #[test]
    fn search_excerpt_redaction_uses_the_matching_session_workspace_only() {
        let hit = |session: &str, excerpt: &str| SessionSearchHit {
            session_id: SessionId::new(session),
            workspace_id: WorkspaceId::new("opaque-workspace"),
            title: "Search result".to_owned(),
            updated_at_ms: 10,
            event_seq: Some(1),
            occurred_at_ms: Some(10),
            run_id: Some(RunId::new("run")),
            category: None,
            excerpt: excerpt.to_owned(),
        };
        let mut hits = vec![
            hit("session-a", "Read /home/alice/private/project/src/main.rs"),
            hit("session-b", "Read /home/bob/other/README.md"),
        ];
        let paths = BTreeMap::from([
            (
                SessionId::new("session-a"),
                "/home/alice/private/project".to_owned(),
            ),
            (SessionId::new("session-b"), "/home/bob/other".to_owned()),
        ]);

        redact_search_hits(&mut hits, &paths);

        assert_eq!(hits[0].excerpt, "Read <local-workspace>/src/main.rs");
        assert_eq!(hits[1].excerpt, "Read <local-workspace>/README.md");
    }
}
