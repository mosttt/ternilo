use super::*;

#[tokio::test]
async fn default_model_survives_restart_and_only_new_sessions_inherit_changes() {
    let data_dir = test_data_dir();
    let workspace_dir = data_dir.with_extension("workspace");
    tokio::fs::create_dir(&workspace_dir).await.unwrap();
    let selected = ModelSelection::OpenAiCompatible {
        base_url: "https://models.example.test/v1".to_owned(),
        model: "model-a".to_owned(),
        api_key_env: None,
        timeout_ms: 30_000,
        max_attempts: 2,
        retry_base_delay_ms: 100,
    };

    let application = open_test_application(data_dir.clone()).await;
    let workspace = application
        .add_workspace(workspace_dir.to_str().unwrap())
        .await
        .unwrap();
    application
        .set_default_model(selected.clone())
        .await
        .unwrap();
    let first = application
        .create_session(workspace.workspace_id.clone(), None, None)
        .await
        .unwrap();
    assert_eq!(first.model, selected);
    application.shutdown().await.unwrap();
    drop(application);

    let reopened = open_test_application(data_dir.clone()).await;
    assert_eq!(reopened.default_model().await.unwrap(), selected);
    let second = reopened
        .create_session(workspace.workspace_id.clone(), None, None)
        .await
        .unwrap();
    assert_eq!(second.model, selected);
    reopened
        .set_default_model(ModelSelection::ProfileDefault)
        .await
        .unwrap();
    assert_eq!(
        reopened
            .snapshot()
            .await
            .sessions
            .iter()
            .find(|session| session.identity.session_id == first.identity.session_id)
            .unwrap()
            .model,
        selected
    );
    let third = reopened
        .create_session(workspace.workspace_id, None, None)
        .await
        .unwrap();
    assert_eq!(third.model, ModelSelection::ProfileDefault);
    reopened.shutdown().await.unwrap();
    drop(reopened);

    tokio::fs::remove_dir_all(data_dir).await.unwrap();
    tokio::fs::remove_dir_all(workspace_dir).await.unwrap();
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one HTTP fixture verifies provider defaults and explicit model overrides"
)]
async fn local_requests_use_resolved_provider_defaults_and_model_overrides() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let data_dir = test_data_dir();
    let workspace_dir = data_dir.with_extension("workspace");
    tokio::fs::create_dir(&workspace_dir).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let requests = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
    let observed = Arc::clone(&requests);
    let server = tokio::spawn(async move {
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = vec![0_u8; 64 * 1024];
            let read = stream.read(&mut request).await.unwrap();
            let body_start = request[..read]
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .unwrap()
                + 4;
            observed
                .lock()
                .await
                .push(serde_json::from_slice(&request[body_start..read]).unwrap());
            let body = concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\n",
                "data: [DONE]\n\n"
            );
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        }
    });

    let application = open_test_application(data_dir.clone()).await;
    application
        .upsert_provider_profile(ProviderProfile {
            id: "resolution-fixture".to_owned(),
            display_name: "Resolution Fixture".to_owned(),
            base_url: format!("http://{address}/v1"),
            protocol: ProviderProtocol::OpenAiChatCompletions,
            api_key_ref: None,
            defaults: ProviderModelDefaults {
                context_window: 128_000,
                max_output_tokens: 8_192,
                reasoning: Some(ProviderModelReasoning {
                    default_effort: ReasoningEffort::Medium,
                    efforts: BTreeMap::from([(
                        ReasoningEffort::Medium,
                        Some("provider-medium".to_owned()),
                    )]),
                }),
            },
            models: vec![
                ProviderModel {
                    id: "model-a".to_owned(),
                    display_name: None,
                    settings: ProviderModelSettings::Inherit,
                },
                ProviderModel {
                    id: "model-b".to_owned(),
                    display_name: None,
                    settings: ProviderModelSettings::Override {
                        context_window: 1_000_000,
                        max_output_tokens: 64_000,
                        reasoning: Some(ProviderModelReasoning {
                            default_effort: ReasoningEffort::High,
                            efforts: BTreeMap::from([(
                                ReasoningEffort::High,
                                Some("model-high".to_owned()),
                            )]),
                        }),
                    },
                },
            ],
            timeout_ms: 5_000,
            max_attempts: 1,
            retry_base_delay_ms: 10,
        })
        .await
        .unwrap();
    let workspace = application
        .add_workspace(workspace_dir.to_str().unwrap())
        .await
        .unwrap();
    let session = application
        .create_session(workspace.workspace_id, None, None)
        .await
        .unwrap();
    let session_id = session.identity.session_id.as_str();
    application
        .run_turn(session_id, None, "/code \"seed\"".to_owned())
        .await
        .unwrap();
    for model in ["model-a", "model-b"] {
        application
            .update_model(
                session_id,
                ModelSelection::NamedProvider {
                    provider_id: "resolution-fixture".to_owned(),
                    model: model.to_owned(),
                    reasoning_effort: None,
                },
            )
            .await
            .unwrap();
        application
            .run_turn(session_id, None, format!("use {model}"))
            .await
            .unwrap();
    }
    server.await.unwrap();
    let requests = requests.lock().await;
    assert_eq!(requests[0]["model"], "model-a");
    assert_eq!(requests[0]["max_tokens"], 8_192);
    assert_eq!(requests[0]["reasoning_effort"], "provider-medium");
    assert_eq!(requests[1]["model"], "model-b");
    assert_eq!(requests[1]["max_tokens"], 64_000);
    assert_eq!(requests[1]["reasoning_effort"], "model-high");

    application.shutdown().await.unwrap();
    drop(application);
    tokio::fs::remove_dir_all(data_dir).await.unwrap();
    tokio::fs::remove_dir_all(workspace_dir).await.unwrap();
}

#[tokio::test]
async fn provider_discovery_uses_unsaved_draft_endpoint_and_direct_key() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let data_dir = test_data_dir();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = vec![0_u8; 4_096];
        let size = stream.read(&mut request).await.unwrap();
        let request = String::from_utf8_lossy(&request[..size]);
        assert!(request.starts_with("GET /draft/models "));
        assert!(request.contains("authorization: Bearer direct-secret"));
        let body = serde_json::json!({ "data": [
            { "id": "draft-model", "protocol": "openai-responses", "name": "Published model",
                "context_window": 128_000, "max_output_tokens": 4096 },
            { "id": "chat-model", "protocol": "openai-chat-completions" }
        ] })
        .to_string();
        stream
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
    });

    let application = open_test_application(data_dir.clone()).await;
    let discovered = application
        .discover_provider_models(ProviderModelDiscoveryRequest {
            provider_id: None,
            base_url: Some(format!("http://{address}/draft")),
            protocol: Some(ProviderProtocol::OpenAiResponses),
            timeout_ms: Some(5_000),
            api_key: Some("direct-secret".to_owned()),
        })
        .await
        .unwrap();
    assert_eq!(discovered.len(), 1);
    assert_eq!(discovered[0].id, "draft-model");
    assert_eq!(
        discovered[0].display_name.as_deref(),
        Some("Published model")
    );
    assert!(matches!(
        &discovered[0].settings,
        ProviderModelSettings::Automatic { upstream, overrides }
            if upstream.context_window == Some(128_000)
                && upstream.max_output_tokens == Some(4096)
                && overrides == &ternilo_protocol::ProviderModelValues::default()
    ));
    server.await.unwrap();
    application.shutdown().await.unwrap();
    tokio::fs::remove_dir_all(data_dir).await.unwrap();
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "end-to-end provider scenario keeps persistence and discovery assertions together"
)]
async fn named_provider_library_is_persistent_discoverable_and_reference_safe() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let data_dir = test_data_dir();
    let workspace_dir = data_dir.with_extension("workspace");
    tokio::fs::create_dir(&workspace_dir).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = vec![0_u8; 4_096];
        let size = stream.read(&mut request).await.unwrap();
        let request = String::from_utf8_lossy(&request[..size]);
        assert!(request.starts_with("GET /v1/models "));
        assert!(request.contains("authorization: Bearer stored-secret"));
        let body = serde_json::json!({
            "data": [
                { "id": "model-b", "name": "Model B", "context_window": 64000 },
                { "id": "model-a" }
            ]
        })
        .to_string();
        stream
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
    });

    let application = open_test_application(data_dir.clone()).await;
    let provider = ProviderProfile {
        id: "fixture".to_owned(),
        display_name: "Fixture".to_owned(),
        base_url: format!("http://{address}/v1"),
        protocol: ProviderProtocol::OpenAiChatCompletions,
        api_key_ref: Some("FIXTURE_KEY".to_owned()),
        defaults: ProviderModelDefaults {
            context_window: 128_000,
            max_output_tokens: 8_192,
            reasoning: None,
        },
        models: vec![ProviderModel {
            id: "model-a".to_owned(),
            display_name: None,
            settings: ProviderModelSettings::Inherit,
        }],
        timeout_ms: 5_000,
        max_attempts: 2,
        retry_base_delay_ms: 10,
    };
    application
        .upsert_provider_profile(provider.clone())
        .await
        .unwrap();
    application
        .set_credential("FIXTURE_KEY".to_owned(), "stored-secret".to_owned())
        .await
        .unwrap();
    let discovered = application
        .discover_provider_models(ProviderModelDiscoveryRequest {
            provider_id: Some("fixture".to_owned()),
            base_url: None,
            protocol: None,
            timeout_ms: None,
            api_key: Some(String::new()),
        })
        .await
        .unwrap();
    assert_eq!(
        discovered
            .iter()
            .map(|model| model.id.as_str())
            .collect::<Vec<_>>(),
        vec!["model-a", "model-b"]
    );
    server.await.unwrap();
    let workspace = application
        .add_workspace(workspace_dir.to_str().unwrap())
        .await
        .unwrap();
    let session = application
        .create_session(workspace.workspace_id, None, None)
        .await
        .unwrap();
    application
        .update_model(
            session.identity.session_id.as_str(),
            ModelSelection::NamedProvider {
                provider_id: "fixture".to_owned(),
                model: "model-a".to_owned(),
                reasoning_effort: None,
            },
        )
        .await
        .unwrap();
    assert!(
        application
            .delete_provider_profile("fixture")
            .await
            .is_err()
    );
    let mut invalid_update = provider.clone();
    invalid_update.models[0].id = "model-b".to_owned();
    assert!(
        application
            .upsert_provider_profile(invalid_update)
            .await
            .is_err()
    );
    assert_eq!(
        application.provider_profiles().await,
        vec![provider.clone()]
    );
    application.shutdown().await.unwrap();
    drop(application);

    let restored = open_test_application(data_dir.clone()).await;
    assert_eq!(restored.provider_profiles().await, vec![provider]);
    restored.shutdown().await.unwrap();
    tokio::fs::remove_dir_all(data_dir).await.unwrap();
    tokio::fs::remove_dir_all(workspace_dir).await.unwrap();
}
