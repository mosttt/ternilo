use super::*;
use crate::{LocalApplication, ModelSelection};
use ternilo_kernel::HostPolicy;
use ternilo_protocol::{
    InputAuthor, InputProvenance, ModelDeviceGrant, ModelDeviceIdentity, ModelDeviceScope,
    ModelDeviceSession, ProviderModelDefaults, ProviderProtocol, PublishedModel, RunLimits,
    SessionSubmissionRequest, SubmissionContent, SubmissionDelivery, SubmissionId, UserId,
};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

#[tokio::test]
async fn failed_model_server_refresh_preserves_connection_and_classifies_expired_access() {
    for (status, code) in [
        (401, ternilo_protocol::ErrorCode::PolicyDenied),
        (403, ternilo_protocol::ErrorCode::PolicyDenied),
        (500, ternilo_protocol::ErrorCode::Execution),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let reply = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buffer = [0_u8; 4096];
            assert!(stream.read(&mut buffer).await.unwrap() > 0);
            stream.write_all(format!("HTTP/1.1 {status} Rejected\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
        });
        let data = directory.path().to_path_buf();
        let connections = LocalModelConnections::open(data.clone()).await.unwrap();
        let credentials = LocalCredentials::open(data).await.unwrap();
        let identity = "0123456789abcdef0123456789abcdef";
        let mut connection = fixture_connection(identity);
        connection.server_url = origin;
        connection.remember_providers();
        connections.save(connection.clone()).await.unwrap();
        credentials
            .set(
                credential_reference(identity),
                "device-fixture-secret".to_owned(),
            )
            .await
            .unwrap();
        let error = connections
            .refresh(identity, &credentials)
            .await
            .unwrap_err();
        assert_eq!(error.code, code);
        assert!(!error.message.contains("device-fixture-secret"));
        if code == ternilo_protocol::ErrorCode::PolicyDenied {
            assert!(error.message.contains("expired or revoked"));
        }
        assert_eq!(
            serde_json::to_value(connections.get(identity).await.unwrap()).unwrap(),
            serde_json::to_value(connection).unwrap(),
        );
        assert_eq!(
            credentials
                .resolve_value(&credential_reference(identity))
                .await
                .unwrap()
                .as_deref(),
            Some("device-fixture-secret")
        );
        reply.await.unwrap();
    }
}

#[tokio::test]
async fn disconnected_model_sources_remain_readable_without_fallback_or_remote_actor_impersonation()
{
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("data");
    let workspace = directory.path().join("workspace");
    tokio::fs::create_dir_all(&data).await.unwrap();
    tokio::fs::create_dir_all(&workspace).await.unwrap();
    let connections = LocalModelConnections::open(data.clone()).await.unwrap();
    let id = "0123456789abcdef0123456789abcdef";
    let mut connection = fixture_connection(id);
    connection.remember_providers();
    let profile = connection.providers().remove(0);
    connections.save(connection).await.unwrap();
    let credentials = LocalCredentials::open(data.clone()).await.unwrap();
    credentials
        .set(credential_reference(id), "device-secret".to_owned())
        .await
        .unwrap();
    drop(connections);
    drop(credentials);
    let open = || {
        LocalApplication::open(
            crate::catalog().unwrap(),
            crate::local_profile(),
            HostPolicy::local(RunLimits::default()),
            data.clone(),
        )
    };
    let app = Arc::new(open().await.unwrap());
    assert_eq!(app.provider_profiles().await, vec![profile.clone()]);
    assert!(app.upsert_provider_profile(profile.clone()).await.is_err());
    let workspace = app
        .add_workspace(workspace.to_str().unwrap())
        .await
        .unwrap();
    let session = app
        .create_session(workspace.workspace_id, None, None)
        .await
        .unwrap();
    let session_id = session.identity.session_id.as_str();
    let selection = ModelSelection::NamedProvider {
        provider_id: profile.id,
        model: "model".to_owned(),
        reasoning_effort: None,
    };
    app.update_model(session_id, selection).await.unwrap();
    let error = app
        .submit_session_with_provenance(
            session_id,
            SessionSubmissionRequest {
                run_id: None,
                content: SubmissionContent::Prompt {
                    input: "Use the model".to_owned(),
                },
                references: vec![],
                attachments: vec![],
                delivery: SubmissionDelivery::Queue,
            },
            InputProvenance {
                run_id: None,
                input_id: SubmissionId::new("remote-input"),
                author: InputAuthor::Account {
                    user_id: UserId::new("other"),
                    username: "other".to_owned(),
                },
            },
        )
        .await
        .unwrap_err();
    assert!(error.message.contains("local inputs only"));
    app.remove_model_connection(id, false).await.unwrap();
    assert!(app.model_connections().await.is_empty());
    assert!(app.provider_profiles().await.is_empty());
    app.shutdown().await.unwrap();
    drop(app);
    let app = open().await.unwrap();
    assert!(app.model_connections().await.is_empty());
    let error = app
        .run_turn(
            session_id,
            Some("disconnected".to_owned()),
            "Use the model".to_owned(),
        )
        .await
        .unwrap_err();
    assert!(error.message.contains("credential"));
    app.shutdown().await.unwrap();
}

fn fixture_connection(id: &str) -> ModelConnection {
    ModelConnection {
        connection_id: id.to_owned(),
        name: "My server".to_owned(),
        server_url: "https://models.example".to_owned(),
        disconnected: false,
        known_providers: Vec::new(),
        session: ModelDeviceSession {
            providers: Vec::new(),
            identity: ModelDeviceIdentity {
                device_id: "mdv-test".to_owned(),
                device_name: "Laptop".to_owned(),
                user_id: UserId::new("owner"),
                username: "owner".to_owned(),
                scope: ModelDeviceScope::Account {
                    include_account_providers: false,
                },
                limits: ternilo_protocol::ModelDeviceLimits::default(),
                revoked_at_ms: None,
                created_at_ms: 0,
                last_used_at_ms: None,
            },
            next_cursor: None,
            grants: vec![ModelDeviceGrant {
                grant_id: "grant-1".to_owned(),
                grant_name: "Personal".to_owned(),
                models: vec![PublishedModel {
                    model_id: "model".to_owned(),
                    display_name: "Model".to_owned(),
                    protocol: ProviderProtocol::DeepSeekResponses,
                    defaults: ProviderModelDefaults {
                        context_window: 4096,
                        max_output_tokens: 1024,
                        reasoning: None,
                    },
                }],
            }],
        },
    }
}

#[test]
fn account_and_platform_sources_keep_distinct_provider_routes_and_ids() {
    let mut connection = fixture_connection("source_identity");
    let grant = &connection.session.grants[0];
    connection
        .session
        .providers
        .push(ternilo_protocol::ModelDeviceProvider {
            provider_id: grant.grant_id.clone(),
            provider_name: grant.grant_name.clone(),
            models: grant.models.clone(),
        });
    let profiles = connection.providers();
    assert_eq!(profiles.len(), 2);
    assert_ne!(profiles[0].id, profiles[1].id);
    assert!(profiles[1].base_url.ends_with("/v1/device-account/grant-1"));
    assert!(profiles[0].base_url.ends_with("/v1/device/grant-1"));
    for profile in &profiles {
        assert!(is_connection_provider(&profile.id));
        profile.validate().unwrap();
    }
    assert_eq!(profiles[0].api_key_ref, profiles[1].api_key_ref);
}
