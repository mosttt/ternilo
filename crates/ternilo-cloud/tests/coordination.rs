use ternilo_cloud::{BROKERED_MODEL_KIND, CloudStore};
use ternilo_control::{ControlStore, SecretCipher};
use ternilo_protocol::{
    AgentRole, CoordinationConfig, CoordinationMode, PermissionPreset, PluginEntry, Profile,
    ProviderModelDefaults, ProviderProtocol, RunId, RunModelBinding, RunModelSnapshot, SessionId,
    SubagentId, SubagentSessionMetadata, SubagentTranscriptKind,
};

#[allow(dead_code)]
#[path = "support/execution_family_fixture.rs"]
mod fixture;
#[path = "support/server_runtime.rs"]
mod server_runtime;
mod support;

#[tokio::test]
async fn sqlite_coordination_preserves_accepted_role_model_and_read_only_policy() {
    let directory = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}",
        directory.path().join("coordination.sqlite3").display()
    );
    let control = ControlStore::connect(&url, None, SecretCipher::from_key([53; 32]), 4)
        .await
        .unwrap();
    let cloud = CloudStore::connect(&url, None, 4).await.unwrap();
    Box::pin(contract(control, cloud)).await;
}

#[tokio::test]
#[ignore = "requires TERNILO_CLOUD_TEST_DATABASE_URL pointing to a disposable PostgreSQL database"]
async fn postgres_coordination_preserves_accepted_role_model_with_restricted_grants() {
    let admin = std::env::var("TERNILO_CLOUD_TEST_DATABASE_URL").unwrap();
    assert!(admin.contains("ternilo_cloud_test"));
    let url = server_runtime::initialize(&admin, "ternilo_coordination_runtime", [53; 32]).await;
    let control = ControlStore::connect(&url, Some(&admin), SecretCipher::from_key([53; 32]), 4)
        .await
        .unwrap();
    let cloud = CloudStore::connect(&url, Some(&admin), 4).await.unwrap();
    Box::pin(contract(control, cloud)).await;
    server_runtime::assert_scoped_without_schema_access(&url).await;
}

#[expect(
    clippy::too_many_lines,
    reason = "Verify accepted role bindings, denied overrides and durable follow-up across parent runs together."
)]
async fn contract(control: ControlStore, cloud: CloudStore) {
    let mut fixture = fixture::Fixture::open(control, cloud).await;
    let session = fixture.session("coordination-parent").await;
    let snapshot = |model: &str| RunModelSnapshot {
        binding: RunModelBinding::UserProvider {
            tenant_id: fixture.tenant.clone(),
            owner_user_id: fixture.owner.user_id.clone(),
            provider_id: "role-fixture".to_owned(),
            model: model.to_owned(),
        },
        protocol: ProviderProtocol::OpenAiChatCompletions,
        defaults: ProviderModelDefaults {
            context_window: 128_000,
            max_output_tokens: 4096,
            reasoning: None,
        },
        reasoning_effort: None,
        display_name: model.to_owned(),
        source_name: "Role fixture".to_owned(),
    };
    let lead = snapshot("lead-model");
    let role_model = snapshot("role-model");
    let mut config = CoordinationConfig::default();
    config.investigator.model = Some(role_model.selection());
    config
        .resolved_models
        .insert(AgentRole::Investigator, role_model.clone());
    let mut compiled = fixture.compiled(&session, "coordination-run", &fixture.owner.user_id);
    compiled.spec.profile = Profile {
        plugins: vec![
            PluginEntry {
                id: "model".to_owned(),
                kind: BROKERED_MODEL_KIND.to_owned(),
                enabled: true,
                config: serde_json::json!({"snapshot":lead}),
            },
            PluginEntry {
                id: "subagents".to_owned(),
                kind: ternilo_protocol::SUBAGENTS_PLUGIN_KIND.to_owned(),
                enabled: true,
                config: serde_json::json!({"coordination":config}),
            },
        ],
    };
    let now = fixture.tick();
    fixture
        .cloud
        .enqueue_session_submission_as(
            &fixture.owner.user_id,
            &compiled,
            &fixture::request(&compiled),
            now,
        )
        .await
        .unwrap();
    let parent = fixture.start(&compiled.spec.metadata.run_id).await;
    let child_id = SessionId::new("role-child");
    let metadata = SubagentSessionMetadata {
        role: Some(AgentRole::Investigator),
        subagent_id: SubagentId::new("investigator"),
        provider: "in-process".to_owned(),
        transcript_kind: SubagentTranscriptKind::Conversation,
    };
    let now = fixture.tick();
    fixture
        .cloud
        .create_subagent_for_worker(
            "family-worker",
            &parent,
            &child_id,
            &metadata,
            "Investigator",
            now,
        )
        .await
        .unwrap();
    let child = fixture
        .cloud
        .get_session(&fixture.tenant, &child_id)
        .await
        .unwrap();
    assert_eq!(child.model.as_ref(), Some(&role_model));
    assert_eq!(child.permissions, PermissionPreset::ReadOnly);
    assert_eq!(
        CoordinationConfig::from_profile(&Profile {
            plugins: child.profile_plugins.clone()
        })
        .unwrap()
        .mode,
        CoordinationMode::Independent
    );
    let mut spec = parent.claim.spec.clone();
    spec.metadata.session_id = child_id.clone();
    spec.metadata.run_id = RunId::new("role-run");
    "investigate".clone_into(&mut spec.input);
    let now = fixture.tick();
    assert!(
        fixture
            .cloud
            .enqueue_subagent_for_worker(
                "family-worker",
                &parent,
                &child_id,
                &spec,
                "investigate",
                1,
                now
            )
            .await
            .is_err()
    );
    spec.profile.plugins = child.profile_plugins.clone();
    spec.profile
        .plugins
        .iter_mut()
        .find(|entry| entry.id == "model")
        .unwrap()
        .config = serde_json::json!({"snapshot":role_model});
    let now = fixture.tick();
    assert!(
        fixture
            .cloud
            .enqueue_subagent_for_worker(
                "family-worker",
                &parent,
                &child_id,
                &spec,
                "investigate",
                1,
                now
            )
            .await
            .is_err()
    );
    spec.permissions = PermissionPreset::ReadOnly;
    let now = fixture.tick();
    fixture
        .cloud
        .enqueue_subagent_for_worker(
            "family-worker",
            &parent,
            &child_id,
            &spec,
            "investigate",
            1,
            now,
        )
        .await
        .unwrap();
    let saved = fixture
        .cloud
        .get_session(&fixture.tenant, &child_id)
        .await
        .unwrap();
    assert_eq!(saved.model, child.model);
    assert_eq!(saved.permissions, PermissionPreset::ReadOnly);
    let running_child = fixture.start(&spec.metadata.run_id).await;
    fixture.finish(&running_child).await;
    fixture.finish(&parent).await;
    compiled.spec.metadata.run_id = RunId::new("changed-parent");
    config.investigator.model = Some(lead.selection());
    config.resolved_models.insert(AgentRole::Investigator, lead);
    compiled.spec.profile.plugins[1].config = serde_json::json!({"coordination": config});
    let now = fixture.tick();
    fixture
        .cloud
        .enqueue_session_submission_as(
            &fixture.owner.user_id,
            &compiled,
            &fixture::request(&compiled),
            now,
        )
        .await
        .unwrap();
    let next_parent = fixture.start(&compiled.spec.metadata.run_id).await;
    spec.metadata.run_id = RunId::new("role-followup");
    "followup".clone_into(&mut spec.input);
    let mut forged = spec.clone();
    forged
        .profile
        .plugins
        .iter_mut()
        .find(|entry| entry.id == "subagents")
        .unwrap()
        .config = serde_json::json!({"coordination":{"mode":"parallel"}});
    let now = fixture.tick();
    assert!(
        fixture
            .cloud
            .enqueue_subagent_for_worker(
                "family-worker",
                &next_parent,
                &child_id,
                &forged,
                "followup",
                1,
                now
            )
            .await
            .is_err()
    );
    let now = fixture.tick();
    fixture
        .cloud
        .enqueue_subagent_for_worker(
            "family-worker",
            &next_parent,
            &child_id,
            &spec,
            "followup",
            1,
            now,
        )
        .await
        .unwrap();
    let resumed = fixture.start(&spec.metadata.run_id).await;
    assert_eq!(
        resumed
            .claim
            .spec
            .profile
            .plugins
            .iter()
            .find(|entry| entry.id == "model")
            .unwrap()
            .config["snapshot"],
        serde_json::to_value(&child.model).unwrap()
    );
    fixture.finish(&resumed).await;
    fixture.finish(&next_parent).await;
    fixture.cloud.database().close().await;
}

#[tokio::test]
async fn sqlite_existing_sessions_survive_installing_role_storage() {
    let directory = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}",
        directory.path().join("existing.sqlite3").display()
    );
    existing_session_contract(&url).await;
}

async fn existing_session_contract(url: &str) {
    let control = ControlStore::connect(url, None, SecretCipher::from_key([53; 32]), 4)
        .await
        .unwrap();
    let cloud = CloudStore::connect(url, None, 4).await.unwrap();
    let mut fixture = fixture::Fixture::open(control, cloud).await;
    let parent = fixture.session("existing-parent").await;
    let run_id = fixture
        .submit(&parent, "existing-run", &fixture.owner.user_id.clone())
        .await;
    let run = fixture.start(&run_id).await;
    let (child, _) = fixture.child(&run, "existing-child").await;
    // Reproduce the pre-role component layout with real parent and child records.
    let mut transaction = fixture.cloud.database().begin().await.unwrap();
    sqlx::raw_sql("ALTER TABLE cloud_sessions DROP COLUMN subagent_role; DELETE FROM ternilo_schema WHERE component='cloud_agent_roles';")
        .execute(&mut *transaction).await.unwrap();
    transaction.commit().await.unwrap();
    fixture.cloud.database().close().await;
    fixture.control.database().close().await;
    let reopened = CloudStore::connect(url, None, 4).await.unwrap();
    let saved_parent = reopened
        .get_session(&fixture.tenant, &parent.session_id)
        .await
        .unwrap();
    let saved_child = reopened
        .get_session(&fixture.tenant, &child.session_id)
        .await
        .unwrap();
    assert_eq!(saved_parent.session_id, parent.session_id);
    assert_eq!(saved_child.parent_session_id, Some(parent.session_id));
    assert_eq!(saved_child.subagent, child.subagent);
    assert_eq!(saved_child.profile_plugins, child.profile_plugins);
    let version: i64 =
        sqlx::query_scalar("SELECT version FROM ternilo_schema WHERE component='cloud'")
            .fetch_one(reopened.database().pool())
            .await
            .unwrap();
    assert_eq!(version, 15);
    reopened.database().close().await;
}
