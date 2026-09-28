use ternilo_protocol::{HarnessError, Profile};

use super::{LocalApplication, TurnInput};

const MODEL_SETUP_REQUIRED: &str = "No model is configured for this session. Configure a Provider and select a model in Settings > Models.";

impl LocalApplication {
    pub(super) async fn validate_turn_model(
        &self,
        session_id: &str,
        input: &TurnInput,
    ) -> Result<(), HarnessError> {
        let profile = self.effective_session_profile(session_id).await?;
        let model_error = self.profile_model_error(&profile).await?;
        let Some(error) = model_error else {
            return Ok(());
        };

        // Registered direct commands execute tools without calling a model.
        // Resolve against this session so disabled tools and unknown slash
        // commands cannot silently fall back to the offline rule model.
        if let TurnInput::Prompt(input) = input
            && input.trim_start().starts_with('/')
        {
            let lifecycle = self.session_lifecycle(session_id).await;
            let _lifecycle = lifecycle.lock().await;
            let managed = self.ensure_session_locked(session_id).await?;
            if managed
                .harness
                .resolve_command(input.clone())
                .await
                .is_some()
            {
                return Ok(());
            }
        }
        Err(error)
    }

    async fn profile_model_error(
        &self,
        profile: &Profile,
    ) -> Result<Option<HarnessError>, HarnessError> {
        for entry in profile.plugins.iter().filter(|entry| entry.enabled) {
            if !self
                .catalog
                .factory(&entry.kind)?
                .manifest
                .provides
                .contains(&"ternilo/models@3")
            {
                continue;
            }
            if entry.kind == "ternilo.model.rule" {
                return Ok(Some(HarnessError::invalid(MODEL_SETUP_REQUIRED)));
            }
            if entry.kind == "ternilo.model.openai_compatible"
                && let Some(reference) = entry
                    .config
                    .get("api_key_env")
                    .and_then(|value| value.as_str())
                && !self.credentials.describe(reference).await?.configured
            {
                return Ok(Some(HarnessError::invalid(format!(
                    "Model credential {reference:?} is not configured. Configure the Provider credential in Settings > Models."
                ))));
            }
            return Ok(None);
        }
        Ok(Some(HarnessError::invalid(MODEL_SETUP_REQUIRED)))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;
    use ternilo_kernel::HostPolicy;
    use ternilo_protocol::{
        ErrorCode, RunLimits, SessionSubmissionRequest, SubmissionContent, SubmissionDelivery,
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;
    use crate::{LocalSessionUpdate, ModelSelection};

    async fn setup(root: &std::path::Path, profile: Profile) -> (Arc<LocalApplication>, String) {
        let workspace = root.join("workspace");
        tokio::fs::create_dir(&workspace).await.unwrap();
        let app = Arc::new(
            LocalApplication::open(
                crate::catalog().unwrap(),
                profile,
                HostPolicy::local(RunLimits::default()),
                root.join("data"),
            )
            .await
            .unwrap(),
        );
        let workspace = app
            .add_workspace(workspace.to_str().unwrap())
            .await
            .unwrap();
        let session = app
            .create_session(workspace.workspace_id, None, None)
            .await
            .unwrap();
        (app, session.identity.session_id.as_str().to_owned())
    }

    #[tokio::test]
    async fn unconfigured_chat_is_rejected_before_queuing_or_recording_a_turn() {
        let root = tempfile::tempdir().unwrap();
        let (app, session) = setup(root.path(), crate::local_profile()).await;
        for input in ["Hello", "/unknown-command hello"] {
            let error = app
                .run_turn(&session, None, input.to_owned())
                .await
                .unwrap_err();
            assert_eq!(error.code, ErrorCode::InvalidInput);
            assert!(error.message.contains("Configure a Provider"));
            let error = app
                .submit_session(
                    &session,
                    SessionSubmissionRequest {
                        delivery: SubmissionDelivery::Queue,
                        run_id: None,
                        content: SubmissionContent::Prompt {
                            input: input.to_owned(),
                        },
                        references: vec![],
                        attachments: vec![],
                    },
                )
                .await
                .unwrap_err();
            assert!(error.message.contains("Configure a Provider"));
        }
        let error = app
            .run_skill_turn(
                &session,
                None,
                "example".to_owned(),
                "Hello".to_owned(),
                vec![],
            )
            .await
            .unwrap_err();
        assert!(error.message.contains("Configure a Provider"));
        assert!(app.events(&session).await.unwrap().is_empty());
        assert!(app.session_inbox(&session).await.unwrap().items.is_empty());
        app.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn direct_tools_remain_available_but_disabled_code_does_not_fake_chat() {
        let root = tempfile::tempdir().unwrap();
        let (app, session) = setup(root.path(), crate::local_profile()).await;
        let answer = app
            .run_turn(&session, None, r#"/code "diagnostic""#.to_owned())
            .await
            .unwrap();
        assert!(answer.answer.contains("diagnostic"));
        app.run_turn(&session, None, "/write note.txt durable".to_owned())
            .await
            .unwrap();
        assert_eq!(
            tokio::fs::read_to_string(root.path().join("workspace/note.txt"))
                .await
                .unwrap(),
            "durable"
        );
        let mut code_mode = crate::local_profile()
            .plugins
            .into_iter()
            .find(|entry| entry.id == "code-mode")
            .unwrap();
        code_mode.enabled = false;
        app.update_session(
            &session,
            LocalSessionUpdate {
                profile_plugins: Some(vec![code_mode]),
                ..LocalSessionUpdate::default()
            },
        )
        .await
        .unwrap();
        let before = app.events(&session).await.unwrap().len();
        let error = app
            .run_turn(&session, None, "Hello".to_owned())
            .await
            .unwrap_err();
        assert!(error.message.contains("Configure a Provider"));
        assert_eq!(app.events(&session).await.unwrap().len(), before);
        let disabled = app
            .run_turn(&session, None, r#"/code "diagnostic""#.to_owned())
            .await
            .unwrap();
        assert!(disabled.answer.contains("not enabled"));
        assert!(disabled.events.iter().any(|event| matches!(&event.kind,
            ternilo_protocol::SessionEventKind::CommandFinished { outcome, .. }
                if outcome.kind == ternilo_protocol::SessionCommandOutcomeKind::Error
        )));
        assert!(
            app.run_turn(&session, None, "/read note.txt".to_owned())
                .await
                .unwrap()
                .answer
                .contains("durable")
        );
        app.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn startup_model_checks_its_credential_and_named_provider_still_streams() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = vec![0; 32 * 1024];
                let read = stream.read(&mut request).await.unwrap();
                let request = String::from_utf8_lossy(&request[..read]).to_ascii_lowercase();
                assert!(request.contains("authorization: bearer fixture-key"));
                let body = "data: {\"choices\":[{\"delta\":{\"content\":\"Real provider answer\"}}]}\n\ndata: [DONE]\n\n";
                stream.write_all(format!("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
            }
        });
        let root = tempfile::tempdir().unwrap();
        let mut profile = crate::local_profile();
        let model = profile
            .plugins
            .iter_mut()
            .find(|entry| entry.id == "model")
            .unwrap();
        model.kind = "ternilo.model.openai_compatible".to_owned();
        model.config = json!({"provider": "fixture", "base_url": format!("http://{address}/v1"), "model": "fixture", "api_key_env": "TERNILO_MODEL_AVAILABILITY_FIXTURE_KEY"});
        let (app, session) = setup(root.path(), profile).await;
        let before = app.credential_inventory().await;
        assert!(before.references.iter().any(|reference| reference.reference
            == "TERNILO_MODEL_AVAILABILITY_FIXTURE_KEY"
            && !reference.configured));
        let error = app
            .run_turn(&session, None, "Hello".to_owned())
            .await
            .unwrap_err();
        assert!(error.message.contains("Configure the Provider credential"));
        assert!(app.events(&session).await.unwrap().is_empty());
        app.set_credential(
            "TERNILO_MODEL_AVAILABILITY_FIXTURE_KEY".to_owned(),
            "fixture-key".to_owned(),
        )
        .await
        .unwrap();
        assert_eq!(
            app.run_turn(&session, None, "Hello".to_owned())
                .await
                .unwrap()
                .answer,
            "Real provider answer"
        );
        let provider: ternilo_protocol::ProviderProfile = serde_json::from_value(json!({
            "id": "named", "display_name": "Fixture", "base_url": format!("http://{address}/v1"), "protocol": "openai-chat-completions", "api_key_ref": "TERNILO_MODEL_AVAILABILITY_FIXTURE_KEY",
            "defaults": {"context_window": 128_000, "max_output_tokens": 8192}, "models": [{"id": "fixture", "settings": {"mode": "inherit"}}],
            "timeout_ms": 5000, "max_attempts": 1, "retry_base_delay_ms": 10
        })).unwrap();
        app.upsert_provider_profile(provider).await.unwrap();
        app.update_model(
            &session,
            ModelSelection::NamedProvider {
                provider_id: "named".to_owned(),
                model: "fixture".to_owned(),
                reasoning_effort: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(
            app.run_turn(&session, None, "Hello again".to_owned())
                .await
                .unwrap()
                .answer,
            "Real provider answer"
        );
        app.shutdown().await.unwrap();
        server.abort();
    }

    #[tokio::test]
    async fn environment_references_are_described_without_exposing_their_values() {
        let root = tempfile::tempdir().unwrap();
        let mut profile = crate::local_profile();
        let model = profile
            .plugins
            .iter_mut()
            .find(|entry| entry.id == "model")
            .unwrap();
        model.kind = "ternilo.model.openai_compatible".to_owned();
        model.config = json!({"provider": "fixture", "base_url": "http://127.0.0.1:1/v1", "model": "fixture", "api_key_env": "PATH"});
        let (app, _) = setup(root.path(), profile).await;
        let inventory = app.credential_inventory().await;
        let reference = inventory
            .references
            .iter()
            .find(|reference| reference.reference == "PATH")
            .unwrap();
        assert!(reference.configured);
        assert!(!reference.writable);
        assert_eq!(
            reference.source,
            Some(ternilo_protocol::CredentialSource::Environment)
        );
        assert!(
            !serde_json::to_value(inventory)
                .unwrap()
                .to_string()
                .contains(&std::env::var("PATH").unwrap())
        );
        app.shutdown().await.unwrap();
    }
}
