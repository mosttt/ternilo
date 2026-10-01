use super::application::{handle_application, handle_application_inner, require_input_provenance};
use super::*;
use ternilo_kernel::HostPolicy;
use ternilo_protocol::{RunLimits, SessionEventKind};

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "Exercise real Node replies and verify the canonical local data in one scenario."
)]
async fn application_replies_redact_workspace_roots_without_changing_local_data() {
    let data = tempfile::tempdir().unwrap();
    let directory = tempfile::tempdir().unwrap();
    let application = Arc::new(
        LocalApplication::open(
            ternilo_local::catalog().unwrap(),
            ternilo_local::local_profile(),
            HostPolicy::local(RunLimits::default()),
            data.path().to_path_buf(),
        )
        .await
        .unwrap(),
    );
    let path = std::fs::canonicalize(directory.path())
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    let workspace: ternilo_local::Workspace = serde_json::from_value(
        handle_application(
            Arc::clone(&application),
            ApplicationOperation::WorkspaceCreate { path: path.clone() },
        )
        .await
        .unwrap(),
    )
    .unwrap();
    // Directory picking must still return a usable path to the remote browser.
    assert_eq!(workspace.path, path);
    let location = handle_application(
        Arc::clone(&application),
        ApplicationOperation::WorkspaceLocation {
            workspace_id: workspace.workspace_id.clone(),
        },
    )
    .await
    .unwrap();
    assert_eq!(location["path"], path);
    assert_eq!(location["created_at_ms"], workspace.created_at_ms);
    assert_eq!(
        location["home"],
        json!(
            ternilo_local::home_directory()
                .ok()
                .map(|home| home.to_string_lossy().into_owned())
        )
    );
    let session: ternilo_local::LocalSession = serde_json::from_value(
        handle_application(
            Arc::clone(&application),
            ApplicationOperation::SessionCreate {
                workspace_id: workspace.workspace_id.clone(),
                session_id: None,
                agent_id: None,
                agent_preset: None,
                permissions: None,
            },
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(session.workspace_path, "<local-workspace>");
    let id = session.identity.session_id;
    let (provider_handle, provider_task) =
        redaction_provider(application.as_ref(), id.as_str()).await;
    let outcome: ternilo_protocol::RunOutcome = serde_json::from_value(
        handle_application(
            Arc::clone(&application),
            ApplicationOperation::SessionTurn {
                session_id: id.clone(),
                run_id: Some("redacted-reply".to_owned()),
                input: "reply ready".to_owned(),
                attachments: Vec::new(),
            },
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(outcome.answer, "provider reply ready");
    assert!(!serde_json::to_string(&outcome).unwrap().contains(&path));
    assert!(outcome.events.iter().any(|event| matches!(
        &event.kind, SessionEventKind::ModelRequestStarted { system_prompt, .. }
            if system_prompt.contains("<local-workspace>")
    )));
    let canonical_events = application.events(id.as_str()).await.unwrap();
    assert!(canonical_events.iter().any(|event| matches!(
        &event.kind, SessionEventKind::ModelRequestStarted { system_prompt, .. }
            if system_prompt.contains(&path)
    )));
    let invalid_model = || ApplicationOperation::SessionUpdate {
        server_model: None,
        session_id: id.clone(),
        title: None,
        permissions: None,
        model: Some(json!({ "provider": path })),
        agent_preset: None,
        profile_plugins: None,
        mode: None,
    };
    let local_error = handle_application_inner(Arc::clone(&application), invalid_model(), None)
        .await
        .unwrap_err();
    assert!(local_error.message.contains(&path));
    let remote_error = handle_application(Arc::clone(&application), invalid_model())
        .await
        .unwrap_err();
    assert_eq!(remote_error.code, local_error.code);
    assert!(!remote_error.message.contains(&path));
    assert!(remote_error.message.contains("<local-workspace>"));
    let renamed = handle_application(
        Arc::clone(&application),
        ApplicationOperation::WorkspaceRename {
            workspace_id: workspace.workspace_id,
            title: "Renamed".to_owned(),
        },
    )
    .await
    .unwrap();
    assert_eq!(renamed["path"], "<local-workspace>");
    let forked = handle_application(
        Arc::clone(&application),
        ApplicationOperation::SessionFork {
            session_id: id.clone(),
            at_seq: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(forked["workspace_path"], "<local-workspace>");
    let archived = handle_application(
        Arc::clone(&application),
        ApplicationOperation::SessionArchive {
            session_id: id.clone(),
        },
    )
    .await
    .unwrap();
    assert_eq!(archived["workspace_path"], "<local-workspace>");
    let restored = handle_application(
        Arc::clone(&application),
        ApplicationOperation::SessionRestore { session_id: id },
    )
    .await
    .unwrap();
    let mut expected = archived;
    expected["archived_at_ms"] = Value::Null;
    assert_eq!(restored, expected);
    let remote_snapshot =
        handle_application(Arc::clone(&application), ApplicationOperation::Snapshot)
            .await
            .unwrap();
    assert!(!remote_snapshot.to_string().contains(&path));
    let canonical = application.snapshot().await;
    assert_eq!(canonical.workspaces[0].path, path);
    assert!(
        canonical
            .sessions
            .iter()
            .all(|session| session.workspace_path == path)
    );
    application.shutdown().await.unwrap();
    provider_handle.stop_graceful(Some(Duration::from_secs(1)));
    provider_task.await.unwrap();
}

#[salvo_core::handler]
async fn redaction_model(request: &mut salvo_core::Request, response: &mut salvo_core::Response) {
    let body: Value = request.parse_json().await.unwrap();
    assert_eq!(body["model"], "redaction-model");
    response
        .headers_mut()
        .insert("content-type", "text/event-stream".parse().unwrap());
    response.write_body(concat!(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"provider reply ready\"},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n",
    )).unwrap();
}

async fn redaction_provider(
    application: &LocalApplication,
    session_id: &str,
) -> (
    salvo_core::server::ServerHandle,
    tokio::task::JoinHandle<()>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server =
        salvo_core::Server::new(salvo_core::conn::tcp::TcpAcceptor::try_from(listener).unwrap());
    let handle = server.handle();
    let router = salvo_core::Router::with_path("v1/chat/completions").post(redaction_model);
    let task = tokio::spawn(async move { server.try_serve(router).await.unwrap() });
    application.upsert_provider_profile(serde_json::from_value(json!({
        "id":"redaction-provider", "display_name":"Redaction Provider",
        "base_url":format!("http://{address}/v1"), "protocol":"openai-chat-completions",
        "defaults":{"context_window":128_000,"max_output_tokens":8_192,"reasoning":null},
        "models":[{"id":"redaction-model","display_name":null,"settings":{"mode":"inherit"}}],
        "timeout_ms":5_000,"max_attempts":1,"retry_base_delay_ms":10,
    })).unwrap()).await.unwrap();
    application
        .update_model(
            session_id,
            ModelSelection::NamedProvider {
                provider_id: "redaction-provider".to_owned(),
                model: "redaction-model".to_owned(),
                reasoning_effort: None,
            },
        )
        .await
        .unwrap();
    (handle, task)
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "Keep setup, protocol actions and assertions together for this integration scenario."
)]
async fn system_agent_presets_cross_the_node_application_boundary_unchanged() {
    let data_dir = tempfile::tempdir().unwrap();
    let workspace_dir = tempfile::tempdir().unwrap();
    let mut base_profile = ternilo_local::local_profile();
    base_profile
        .plugins
        .iter_mut()
        .find(|plugin| plugin.id == "agent-loop")
        .unwrap()
        .config["max_tool_calls"] = json!(73);
    let application = Arc::new(
        LocalApplication::open(
            ternilo_local::catalog().unwrap(),
            base_profile.clone(),
            HostPolicy::local(RunLimits::default()),
            data_dir.path().to_path_buf(),
        )
        .await
        .unwrap(),
    );
    let roster: ternilo_protocol::AgentPresetRoster = serde_json::from_value(
        handle_application(
            Arc::clone(&application),
            ApplicationOperation::AgentPresetList,
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        roster
            .presets
            .iter()
            .map(|preset| preset.id.as_str())
            .collect::<Vec<_>>(),
        ternilo_protocol::SYSTEM_AGENT_PRESET_IDS,
    );

    let source_id = ternilo_protocol::SYSTEM_AGENT_PRESET_IDS[0];
    let source: ternilo_protocol::AgentPresetDocument = serde_json::from_value(
        handle_application(
            Arc::clone(&application),
            ApplicationOperation::AgentPresetGet {
                preset_id: source_id.to_owned(),
            },
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(source.summary.id, source_id);
    assert_eq!(source.base_profile.as_ref(), Some(&base_profile));
    assert_eq!(
        source.profile,
        ternilo_protocol::system_agent_preset(source_id)
            .unwrap()
            .profile
    );
    assert!(
        application
            .agent_preset(source_id)
            .await
            .unwrap()
            .base_profile
            .is_none()
    );

    let copied: ternilo_protocol::AgentPresetDocument = serde_json::from_value(
        handle_application(
            Arc::clone(&application),
            ApplicationOperation::AgentPresetCopy {
                request: ternilo_protocol::AgentPresetCopyRequest {
                    from: source_id.to_owned(),
                    id: "node-custom".to_owned(),
                    display_name: Some("Node Custom".to_owned()),
                },
            },
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(copied.summary.id, "node-custom");
    assert!(copied.base_profile.is_none());

    let updated: ternilo_protocol::AgentPresetDocument = serde_json::from_value(
        handle_application(
            Arc::clone(&application),
            ApplicationOperation::AgentPresetUpdate {
                preset_id: "node-custom".to_owned(),
                request: ternilo_protocol::AgentPresetUpdateRequest {
                    display_name: "Updated Node Custom".to_owned(),
                    description: "Node lifecycle dispatch".to_owned(),
                    profile: copied.profile,
                },
            },
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(updated.summary.display_name, "Updated Node Custom");
    assert!(updated.base_profile.is_none());
    let stored = tokio::fs::read_to_string(data_dir.path().join("config/agent-presets.json"))
        .await
        .unwrap();
    assert!(!stored.contains("base_profile"));
    assert!(
        !stored.contains("max_tool_calls"),
        "copying a preset must not snapshot inherited host configuration"
    );

    let default_roster: ternilo_protocol::AgentPresetRoster = serde_json::from_value(
        handle_application(
            Arc::clone(&application),
            ApplicationOperation::AgentPresetSetDefault {
                preset_id: "node-custom".to_owned(),
            },
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(default_roster.default_id, "node-custom");

    handle_application(
        Arc::clone(&application),
        ApplicationOperation::AgentPresetSetDefault {
            preset_id: source_id.to_owned(),
        },
    )
    .await
    .unwrap();
    handle_application(
        Arc::clone(&application),
        ApplicationOperation::AgentPresetDelete {
            preset_id: "node-custom".to_owned(),
        },
    )
    .await
    .unwrap();
    assert!(
        handle_application(
            Arc::clone(&application),
            ApplicationOperation::AgentPresetGet {
                preset_id: "node-custom".to_owned(),
            },
        )
        .await
        .is_err()
    );

    let workspace = application
        .add_workspace(workspace_dir.path().to_str().unwrap())
        .await
        .unwrap();
    for id in ternilo_protocol::SYSTEM_AGENT_PRESET_IDS {
        let session: ternilo_local::LocalSession = serde_json::from_value(
            handle_application(
                Arc::clone(&application),
                ApplicationOperation::SessionCreate {
                    workspace_id: workspace.workspace_id.clone(),
                    session_id: None,
                    agent_id: None,
                    agent_preset: Some(id.to_owned()),
                    permissions: None,
                },
            )
            .await
            .unwrap(),
        )
        .unwrap();
        assert_eq!(session.agent_preset, id);
        assert_eq!(
            session.preset_plugins,
            ternilo_protocol::system_agent_preset(id)
                .unwrap()
                .profile
                .plugins,
        );
    }
    application.shutdown().await.unwrap();
}

#[tokio::test]
async fn agent_team_operations_dispatch_to_the_local_application() {
    let data_dir = tempfile::tempdir().unwrap();
    let workspace_dir = tempfile::tempdir().unwrap();
    let application = Arc::new(
        LocalApplication::open(
            ternilo_local::catalog().unwrap(),
            ternilo_local::local_profile(),
            HostPolicy::local(RunLimits::default()),
            data_dir.path().to_path_buf(),
        )
        .await
        .unwrap(),
    );
    let workspace = application
        .add_workspace(workspace_dir.path().to_str().unwrap())
        .await
        .unwrap();
    let session = application
        .create_session(workspace.workspace_id, None, None)
        .await
        .unwrap();
    let session_id = session.identity.session_id;

    let snapshot: ternilo_protocol::AgentTeamSnapshot = serde_json::from_value(
        handle_application(
            Arc::clone(&application),
            ApplicationOperation::SessionAgentTeamSnapshot {
                session_id: session_id.clone(),
            },
        )
        .await
        .unwrap(),
    )
    .unwrap();
    let task: ternilo_protocol::AgentTeamTask = serde_json::from_value(
        handle_application(
            Arc::clone(&application),
            ApplicationOperation::SessionAgentTeamTaskCreate {
                session_id: session_id.clone(),
                request: ternilo_protocol::AgentTeamTaskCreate {
                    subject: "Node dispatch".to_owned(),
                    description: String::new(),
                    status: ternilo_protocol::AgentTeamTaskStatus::InProgress,
                    dependencies: Vec::new(),
                    owner: Some(snapshot.current_member_id.clone()),
                },
            },
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(task.revision, 1);

    let message: ternilo_protocol::AgentTeamMessage = serde_json::from_value(
        handle_application(
            Arc::clone(&application),
            ApplicationOperation::SessionAgentTeamMessageSend {
                session_id,
                request: ternilo_protocol::AgentTeamMessageSend {
                    to: snapshot.current_member_id,
                    content: "Transport ready".to_owned(),
                },
            },
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(message.content, "Transport ready");
    application.shutdown().await.unwrap();
}

#[test]
fn gateway_human_input_requires_an_account_author() {
    assert!(require_input_provenance(None).is_err());
    let input = ternilo_protocol::InputProvenance {
        run_id: None,
        input_id: ternilo_protocol::SubmissionId::new("input"),
        author: ternilo_protocol::InputAuthor::Local,
    };
    assert!(require_input_provenance(Some(input)).is_err());
}

#[tokio::test]
async fn node_catalog_reports_its_actual_host_ceiling() {
    for max_tool_calls in [0, 37] {
        let data = tempfile::tempdir().unwrap();
        let application = Arc::new(
            LocalApplication::open(
                ternilo_local::catalog().unwrap(),
                ternilo_local::local_profile(),
                HostPolicy::local(RunLimits {
                    max_steps: 0,
                    max_tool_calls,
                }),
                data.path().to_path_buf(),
            )
            .await
            .unwrap(),
        );
        let response = handle_application(Arc::clone(&application), ApplicationOperation::Catalog)
            .await
            .unwrap();
        let catalog: ternilo_protocol::ApplicationCatalog =
            serde_json::from_value(response).unwrap();
        assert_eq!(
            catalog.host_limits,
            Some(RunLimits {
                max_steps: 0,
                max_tool_calls
            })
        );
        application.shutdown().await.unwrap();
    }
}
