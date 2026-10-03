use super::*;

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "the materialization scenario proves signed projection, persistence, and model execution together"
)]
async fn extension_provider_materializes_create_only_and_survives_uninstall_for_real_requests() {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use ed25519_dalek::SigningKey;
    use std::collections::BTreeSet;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let data_dir = test_data_dir();
    let workspace_dir = data_dir.with_extension("workspace");
    tokio::fs::create_dir(&workspace_dir).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let requests = Arc::new(Mutex::new(Vec::<String>::new()));
    let observed = Arc::clone(&requests);
    let server = tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let mut request = vec![0_u8; 64 * 1024];
            let read = stream.read(&mut request).await.unwrap();
            observed
                .lock()
                .await
                .push(String::from_utf8_lossy(&request[..read]).into_owned());
            let body = concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"materialized provider reached\"}}]}\n\n",
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
    let signing_key = SigningKey::from_bytes(&[63; 32]);
    let source = "https://plugins.ternilo.dev/provider-fixture";
    application
        .trust_extension_publisher(ternilo_extension::PublisherTrust {
            key_id: "provider-fixture-key".to_owned(),
            public_key_base64: STANDARD.encode(signing_key.verifying_key().to_bytes()),
            allowed_sources: [source.to_owned()].into_iter().collect(),
        })
        .unwrap();
    let manifest = ternilo_extension::ExtensionManifest {
        schema_version: ternilo_extension::EXTENSION_PACKAGE_SCHEMA_VERSION,
        package_id: "dev.ternilo.provider-fixture".to_owned(),
        version: "1.0.0".to_owned(),
        description: Some("Provider materialization fixture".to_owned()),
        source: source.to_owned(),
        publisher_key_id: "provider-fixture-key".to_owned(),
        payload_sha256: "0".repeat(64),
        runtime: ternilo_extension::ExtensionRuntime::Rhai {
            limits: ternilo_extension::RhaiExecutionLimits::default(),
        },
        config_schema: serde_json::json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        }),
        contributions: ternilo_extension::ExtensionContributions {
            tools: Vec::new(),
            prompt_sections: Vec::new(),
            skills: Vec::new(),
            hooks: Vec::new(),
            commands: Vec::new(),
            providers: vec![ternilo_extension::ExtensionProviderContribution {
                id: "fixture".to_owned(),
                display_name: "Signed Provider Fixture".to_owned(),
                base_url: format!("http://{address}/v1"),
                protocol: ProviderProtocol::OpenAiChatCompletions,
                defaults: ProviderModelDefaults {
                    context_window: 128_000,
                    max_output_tokens: 8_192,
                    reasoning: None,
                },
                models: vec![ProviderModel {
                    id: "fixture-model".to_owned(),
                    display_name: Some("Fixture Model".to_owned()),
                    settings: ProviderModelSettings::Inherit,
                }],
                timeout_ms: 5_000,
                max_attempts: 1,
                retry_base_delay_ms: 10,
                credential: ternilo_extension::ExtensionProviderCredential {
                    required: true,
                    suggested_ref: Some("SUGGESTED_ONLY".to_owned()),
                },
            }],
        },
        requested_capabilities: BTreeSet::new(),
    };
    application
        .install_extension(ternilo_extension::ExtensionInstallRequest {
            bundle: ternilo_extension::sign_bundle(
                manifest,
                ternilo_extension::ExtensionPayload::Utf8(
                    "fn unused(context, input, settings) { #{} }".to_owned(),
                ),
                &signing_key,
            )
            .unwrap(),
            granted_capabilities: BTreeSet::new(),
        })
        .unwrap();
    let materialize = ExtensionProviderMaterializeRequest {
        package_id: "dev.ternilo.provider-fixture".to_owned(),
        version: "1.0.0".to_owned(),
        template: "fixture".to_owned(),
        provider_id: "materialized-fixture".to_owned(),
        api_key_ref: Some("USER_SELECTED_KEY".to_owned()),
    };
    let provider = application
        .materialize_extension_provider(materialize.clone())
        .await
        .unwrap();
    assert_eq!(provider.display_name, "Signed Provider Fixture");
    assert_eq!(provider.api_key_ref.as_deref(), Some("USER_SELECTED_KEY"));
    let duplicate = application
        .materialize_extension_provider(materialize)
        .await
        .unwrap_err();
    assert_eq!(duplicate.code, ternilo_protocol::ErrorCode::Conflict);
    application
        .set_credential("USER_SELECTED_KEY".to_owned(), "stored-secret".to_owned())
        .await
        .unwrap();
    application
        .uninstall_extension("dev.ternilo.provider-fixture", "1.0.0")
        .await
        .unwrap();
    application.shutdown().await.unwrap();
    drop(application);

    let restored = open_test_application(data_dir.clone()).await;
    assert_eq!(restored.provider_profiles().await, vec![provider]);
    let workspace = restored
        .add_workspace(workspace_dir.to_str().unwrap())
        .await
        .unwrap();
    let session = restored
        .create_session(workspace.workspace_id, None, None)
        .await
        .unwrap();
    restored
        .update_model(
            session.identity.session_id.as_str(),
            ModelSelection::NamedProvider {
                provider_id: "materialized-fixture".to_owned(),
                model: "fixture-model".to_owned(),
                reasoning_effort: None,
            },
        )
        .await
        .unwrap();
    let outcome = restored
        .run_turn(
            session.identity.session_id.as_str(),
            Some("materialized-provider-run".to_owned()),
            "hello".to_owned(),
        )
        .await
        .unwrap();
    assert_eq!(outcome.answer, "materialized provider reached");
    let requests = requests.lock().await;
    assert!(requests.iter().any(|request| {
        request.starts_with("POST /v1/chat/completions ")
            && request.contains("authorization: Bearer stored-secret")
    }));
    drop(requests);

    restored.shutdown().await.unwrap();
    drop(restored);
    server.abort();
    tokio::fs::remove_dir_all(data_dir).await.unwrap();
    tokio::fs::remove_dir_all(workspace_dir).await.unwrap();
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "authorization end-to-end scenario verifies one complete surface-scoped flow"
)]
async fn plugin_authorization_is_surface_scoped_single_flight_and_provider_keys_stay_direct() {
    let data_dir = test_data_dir();
    let application = open_test_application(data_dir.clone()).await;
    application
        .upsert_provider_profile(ProviderProfile {
            hosted_tools: None,
            id: "authorization-fixture".to_owned(),
            display_name: "Authorization Fixture".to_owned(),
            base_url: "https://provider.invalid/v1".to_owned(),
            protocol: ProviderProtocol::OpenAiChatCompletions,
            api_key_ref: Some("AUTHORIZATION_FIXTURE_KEY".to_owned()),
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
            max_attempts: 1,
            retry_base_delay_ms: 10,
        })
        .await
        .unwrap();
    assert!(
        application
            .authorization_snapshot("surface-one")
            .await
            .unwrap()
            .entries
            .is_empty(),
        "a Provider API key belongs to the Models page, not plugin sign-in"
    );
    application
        .authorizations
        .register_fixture_flow()
        .await
        .unwrap();
    let key = ternilo_protocol::AuthorizationCredentialKey {
        space: ternilo_protocol::AuthorizationCredentialSpace::Record,
        key: "dev.ternilo.authorization-fixture/device-code".to_owned(),
    };
    let snapshot = application
        .authorization_snapshot("surface-one")
        .await
        .unwrap();
    assert_eq!(snapshot.entries.len(), 1);
    assert!(!snapshot.entries[0].configured);
    let attempt = application
        .begin_authorization(ternilo_protocol::AuthorizationBeginRequest {
            key: key.clone(),
            method: Some("device-code".to_owned()),
            surface_id: "surface-one".to_owned(),
        })
        .await
        .unwrap();
    assert!(
        application
            .begin_authorization(ternilo_protocol::AuthorizationBeginRequest {
                key: key.clone(),
                method: None,
                surface_id: "surface-two".to_owned(),
            })
            .await
            .is_err()
    );
    let prompt = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let snapshot = application
                .authorization_snapshot("surface-one")
                .await
                .unwrap();
            if let Some(prompt) = snapshot.prompts.into_iter().next() {
                break prompt;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(
        application
            .authorization_snapshot("surface-two")
            .await
            .unwrap()
            .prompts
            .is_empty()
    );
    application
        .answer_authorization_prompt(ternilo_protocol::AuthorizationPromptAnswer {
            prompt_id: prompt.id,
            surface_id: "surface-one".to_owned(),
            value: "complete".to_owned(),
        })
        .unwrap();
    let settled = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let snapshot = application
                .authorization_snapshot("surface-one")
                .await
                .unwrap();
            if snapshot.attempts.iter().any(|candidate| {
                candidate.attempt_id == attempt.attempt_id
                    && candidate.status == ternilo_protocol::AuthorizationAttemptStatus::Authorized
            }) {
                break snapshot;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(settled.entries[0].configured);
    assert_eq!(
        application
            .credentials
            .record("dev.ternilo.authorization-fixture/device-code")
            .await
            .unwrap()
            .unwrap(),
        (
            "grant".to_owned(),
            serde_json::json!({
                "provider": "fixture-device-code",
                "account": "fixture-user",
                "authorized": true,
            })
        )
    );

    application.shutdown().await.unwrap();
    tokio::fs::remove_dir_all(data_dir).await.unwrap();
}

#[tokio::test]
async fn runtime_extension_inspection_uses_the_live_local_catalog() {
    let data_dir = test_data_dir();
    let workspace_dir = data_dir.with_extension("workspace");
    tokio::fs::create_dir(&workspace_dir).await.unwrap();
    let application = open_test_application(data_dir.clone()).await;
    let workspace = application
        .add_workspace(workspace_dir.to_str().unwrap())
        .await
        .unwrap();
    let session = application
        .create_session(workspace.workspace_id, None, None)
        .await
        .unwrap();
    application
        .run_turn(
            session.identity.session_id.as_str(),
            None,
            "/extensions".to_owned(),
        )
        .await
        .unwrap();
    let (preview, retained) = application
        .events(session.identity.session_id.as_str())
        .await
        .unwrap()
        .into_iter()
        .find_map(|event| match event.kind {
            SessionEventKind::ToolCallFinished {
                name,
                output,
                retained_output,
                ..
            } if name == "extension_inspect" => Some((output.content, retained_output)),
            _ => None,
        })
        .expect("extension inspection emitted a tool result");
    let report = if let Some(retained) = retained {
        application
            .resolve_attachment(retained)
            .await
            .unwrap()
            .content
    } else {
        preview
    };
    assert!(report.contains(crate::LOCAL_CATALOG_REVISION));
    assert!(report.contains("ternilo.code_runtime.rhai"));
    assert!(report.contains("extension_set_enabled"));
    assert!(report.contains("\"extensions\": []"));

    application.shutdown().await.unwrap();
    tokio::fs::remove_dir_all(data_dir).await.unwrap();
    tokio::fs::remove_dir_all(workspace_dir).await.unwrap();
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one uninstall scenario verifies inherited mounts and user preset cleanup"
)]
async fn extension_lifecycle_cleanup_shadows_inherited_rows_and_cleans_user_presets() {
    let data_dir = test_data_dir();
    let workspace_dir = data_dir.with_extension("workspace");
    tokio::fs::create_dir(&workspace_dir).await.unwrap();
    let application = open_test_application(data_dir.clone()).await;
    let mount = PluginEntry {
        id: "extension:dev.lifecycle@1.0.0".to_owned(),
        kind: ternilo_extension::EXTENSION_PACKAGE_KIND.to_owned(),
        enabled: true,
        config: serde_json::json!({
            "package_id": "dev.lifecycle",
            "version": "1.0.0",
            "settings": {"mode": "strict"}
        }),
    };
    application
        .copy_agent_preset(AgentPresetCopyRequest {
            from: "standard".to_owned(),
            id: "extension-lifecycle".to_owned(),
            display_name: Some("Extension lifecycle".to_owned()),
        })
        .await
        .unwrap();
    application
        .presets
        .update(
            "extension-lifecycle",
            AgentPresetUpdateRequest {
                display_name: "Extension lifecycle".to_owned(),
                description: String::new(),
                profile: Profile {
                    plugins: vec![mount.clone()],
                },
            },
        )
        .await
        .unwrap();
    let workspace = application
        .add_workspace(workspace_dir.to_str().unwrap())
        .await
        .unwrap();
    let session = application
        .create_session_with_preset(
            workspace.workspace_id,
            None,
            None,
            Some("extension-lifecycle".to_owned()),
        )
        .await
        .unwrap();
    let mut stored = application
        .state
        .session(session.identity.session_id.as_str())
        .await
        .unwrap();
    stored.profile_plugins.push(mount);
    application
        .state
        .replace_session(session.identity.session_id.as_str(), stored)
        .await
        .unwrap();

    application
        .remove_extension_mounts(&[("dev.lifecycle".to_owned(), "1.0.0".to_owned())])
        .await
        .unwrap();

    let cleaned = application
        .state
        .session(session.identity.session_id.as_str())
        .await
        .unwrap();
    assert!(
        cleaned
            .preset_plugins
            .iter()
            .all(|entry| entry.kind != ternilo_extension::EXTENSION_PACKAGE_KIND)
    );
    let shadow = cleaned
        .profile_plugins
        .iter()
        .find(|entry| entry.id == "extension:dev.lifecycle@1.0.0")
        .expect("preset mount receives a durable session shadow");
    assert!(!shadow.enabled);
    assert!(
        application
            .agent_preset("extension-lifecycle")
            .await
            .unwrap()
            .profile
            .plugins
            .iter()
            .all(|entry| entry.kind != ternilo_extension::EXTENSION_PACKAGE_KIND)
    );

    application.shutdown().await.unwrap();
    drop(application);
    let reopened = open_test_application(data_dir.clone()).await;
    let reopened_session = reopened
        .state
        .session(session.identity.session_id.as_str())
        .await
        .unwrap();
    let shadow = reopened_session
        .profile_plugins
        .iter()
        .find(|entry| entry.id == "extension:dev.lifecycle@1.0.0")
        .expect("disabled session shadow survives restart");
    assert!(!shadow.enabled);
    reopened.shutdown().await.unwrap();
    tokio::fs::remove_dir_all(data_dir).await.unwrap();
    tokio::fs::remove_dir_all(workspace_dir).await.unwrap();
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "extension restart scenario keeps approval, profile, and next-turn assertions together"
)]
async fn approved_extension_unmount_updates_profile_and_restarts_next_turn() {
    let data_dir = test_data_dir();
    let workspace_dir = data_dir.with_extension("workspace");
    tokio::fs::create_dir(&workspace_dir).await.unwrap();
    let application = Arc::new(open_test_application(data_dir.clone()).await);
    let workspace = application
        .add_workspace(workspace_dir.to_str().unwrap())
        .await
        .unwrap();
    let session = application
        .create_session(workspace.workspace_id, None, None)
        .await
        .unwrap();
    let session_id = session.identity.session_id.as_str().to_owned();
    application
        .run_turn(&session_id, None, "/code \"warm-runtime\"".to_owned())
        .await
        .unwrap();

    let mut stored = application.state.session(&session_id).await.unwrap();
    stored.preset_plugins.push(PluginEntry {
        id: "preset-custom-extension-row".to_owned(),
        kind: ternilo_extension::EXTENSION_PACKAGE_KIND.to_owned(),
        enabled: true,
        config: serde_json::json!({
            "package_id": "dev.stale",
            "version": "1.0.0",
            "settings": {"scope": "session"}
        }),
    });
    application
        .state
        .replace_session(&session_id, stored)
        .await
        .unwrap();

    let running = {
        let application = Arc::clone(&application);
        let session_id = session_id.clone();
        tokio::spawn(async move {
            application
                .run_turn(
                    &session_id,
                    Some("extension-unmount".to_owned()),
                    "/extension-unmount dev.stale 1.0.0".to_owned(),
                )
                .await
        })
    };
    let question = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if let Some(question) = application
                .pending_questions(Some(&session_id))
                .await
                .into_iter()
                .next()
            {
                break question;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    application
        .answer_question(ternilo_protocol::UserAnswer {
            question_id: question.question.id,
            selected: vec!["Allow once".to_owned()],
            custom: None,
        })
        .await
        .unwrap();
    let outcome = running.await.unwrap().unwrap();
    assert!(outcome.answer.contains("\"mounted\": false"));
    let stored = application.state.session(&session_id).await.unwrap();
    let shadow = stored
        .profile_plugins
        .iter()
        .find(|entry| entry.id == "preset-custom-extension-row")
        .expect("preset extension mount receives a session shadow");
    assert!(!shadow.enabled);
    assert_eq!(shadow.kind, ternilo_extension::EXTENSION_PACKAGE_KIND);
    assert_eq!(shadow.config["package_id"], "dev.stale");
    assert_eq!(shadow.config["version"], "1.0.0");
    assert!(
        application
            .events(&session_id)
            .await
            .unwrap()
            .iter()
            .any(|event| matches!(
                event.kind,
                SessionEventKind::RuntimeExtensionChanged {
                    action: ternilo_protocol::RuntimeExtensionAction::Unmounted,
                    ..
                }
            ))
    );
    let next = application
        .run_turn(&session_id, None, "/code \"runtime restarted\"".to_owned())
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&next.answer).unwrap()["result"],
        "runtime restarted"
    );

    application.shutdown().await.unwrap();
    drop(application);
    tokio::fs::remove_dir_all(data_dir).await.unwrap();
    tokio::fs::remove_dir_all(workspace_dir).await.unwrap();
}
