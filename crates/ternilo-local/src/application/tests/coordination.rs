use super::*;
use ternilo_protocol::{AgentRole, CoordinationMode};

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "Exercise role model requests, permission enforcement, child delegation and persistence together."
)]
async fn coordination_role_uses_its_model_without_widening_access_and_survives_restart() {
    let root = tempfile::tempdir().unwrap();
    let workspace_path = root.path().join("workspace");
    tokio::fs::create_dir(&workspace_path).await.unwrap();
    let model = super::super::test_model::TestModel::start().await;
    let data = root.path().join("data");
    let app = open_test_application(data.clone()).await;
    model
        .install_named(&app, "coordination-fixture", &["lead-model", "role-model"])
        .await;
    let workspace = app
        .add_workspace(workspace_path.to_str().unwrap())
        .await
        .unwrap();
    let parent = app
        .create_session(workspace.workspace_id, None, None)
        .await
        .unwrap();
    let selected = |model: &str| ModelSelection::NamedProvider {
        provider_id: "coordination-fixture".to_owned(),
        model: model.to_owned(),
        reasoning_effort: None,
    };
    app.update_model(parent.identity.session_id.as_str(), selected("lead-model"))
        .await
        .unwrap();
    let settings = PluginEntry {
        id: "subagents".to_owned(),
        kind: ternilo_protocol::SUBAGENTS_PLUGIN_KIND.to_owned(),
        enabled: true,
        config: serde_json::json!({"coordination": {
            "mode":"assisted", "investigator": {
                "model":selected("role-model"), "read_only":true,
                "instructions":"Return evidence for the coordination fixture."
            }
        }}),
    };
    app.update_profile_plugins(parent.identity.session_id.as_str(), vec![settings])
        .await
        .unwrap();
    let request = SubagentSessionRequest {
        role: Some(AgentRole::Investigator),
        subagent_id: SubagentId::new("investigate"),
        provider: "in-process".to_owned(),
        label: "Investigator".to_owned(),
        task: "collect evidence".to_owned(),
        transcript_kind: ternilo_protocol::SubagentTranscriptKind::Conversation,
    };
    let host = app.subagent_session_host();
    let child = host
        .create(parent.identity.clone(), request.clone())
        .await
        .unwrap()
        .unwrap();
    let child_session = app.state.session(child.session_id.as_str()).await.unwrap();
    assert_eq!(child_session.model, selected("role-model"));
    assert_eq!(child_session.permissions, PermissionPreset::ReadOnly);
    assert_eq!(
        child_session.subagent.as_ref().unwrap().role,
        Some(AgentRole::Investigator)
    );
    let outcome = app
        .run_turn(
            child.session_id.as_str(),
            None,
            "collect evidence".to_owned(),
        )
        .await
        .unwrap();
    assert_eq!(outcome.answer, "collect evidence");
    let requests = model.requests();
    assert!(!requests.is_empty());
    assert!(
        requests
            .iter()
            .all(|request| request["model"] == "role-model")
    );
    assert!(requests.iter().any(|request| {
        request["messages"][0]["content"]
            .as_str()
            .is_some_and(|prompt| prompt.contains("Return evidence for the coordination fixture."))
    }));
    let denied = app
        .run_turn(
            child.session_id.as_str(),
            None,
            "/write unexpected.txt mutation".to_owned(),
        )
        .await
        .unwrap();
    assert!(denied.events.iter().any(|event| matches!(&event.kind,
        SessionEventKind::ToolCallFinished { output, .. } if output.is_error)));
    assert!(!workspace_path.join("unexpected.txt").exists());
    assert!(
        host.create(child_session.identity.clone(), request.clone())
            .await
            .unwrap_err()
            .message
            .contains("Independent coordination")
    );
    let profile = app
        .effective_session_profile(child.session_id.as_str())
        .await
        .unwrap();
    assert_eq!(
        ternilo_protocol::CoordinationConfig::from_profile(&profile)
            .unwrap()
            .mode,
        CoordinationMode::Independent
    );

    app.update_profile_plugins(
        parent.identity.session_id.as_str(),
        vec![PluginEntry {
            id: "subagents".to_owned(),
            kind: ternilo_protocol::SUBAGENTS_PLUGIN_KIND.to_owned(),
            enabled: true,
            config: serde_json::json!({"coordination":{"mode":"independent"}}),
        }],
    )
    .await
    .unwrap();
    assert!(
        host.create(parent.identity, request)
            .await
            .unwrap_err()
            .message
            .contains("Independent coordination")
    );
    app.shutdown().await.unwrap();
    drop(host);
    drop(app);
    let reopened = open_test_application(data).await;
    let saved = reopened
        .state
        .session(child.session_id.as_str())
        .await
        .unwrap();
    assert_eq!(saved.model, selected("role-model"));
    assert_eq!(saved.permissions, PermissionPreset::ReadOnly);
    reopened
        .run_turn(
            child.session_id.as_str(),
            None,
            "continue investigation".to_owned(),
        )
        .await
        .unwrap();
    assert!(
        model
            .requests()
            .iter()
            .all(|request| request["model"] == "role-model")
    );
    reopened.shutdown().await.unwrap();
}
