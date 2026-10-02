use super::*;
#[path = "computer_models.rs"]
mod computers;
#[path = "edge_model_schedules.rs"]
mod schedules;
use ternilo_control::{
    EdgeStore, ModelRequestInput, ModelRequestSettlement, ModelRequestState, NodePrincipal,
    ServiceModelUsage,
};
use ternilo_protocol::{
    DefaultModelSelection, InputAuthor, InputProvenance, ModelRequest, NodeModelRequest,
    ProviderModel, ProviderModelDefaults, ProviderModelSettings, ProviderProfile, ProviderProtocol,
    RunModelBinding, SubmissionId,
};

pub(super) async fn contract(
    store: &ControlStore,
    fixture: &EdgeFixture,
    mapping: &MappingFixture,
) {
    let now = fixture.now + 500_000;
    let tenant = &fixture.tenant_a;
    let session = &mapping.browser_session;
    let snapshot = prepare_source(store, fixture, mapping).await;
    let node = node_principal(store, fixture).await;
    let body = accepted_input(store, fixture, mapping, &snapshot, &node).await;
    let mut tx = store.database().begin().await.unwrap();
    let principal = store
        .authorize_node_model_in(&mut tx, &node, &body)
        .await
        .unwrap();
    assert_eq!(principal.actor_user_id, fixture.alice.user_id);
    assert_eq!(principal.resource_owner_user_id, fixture.alice.user_id);
    assert_eq!(
        principal.snapshot.binding.beneficiary_user_id(),
        &fixture.bob.user_id
    );
    tx.commit().await.unwrap();
    lineage_contract(store, fixture, mapping, &node, &body).await;
    schedules::contract(store, fixture, mapping, &node, &body).await;
    ledger_contract(store, &principal, now).await;
    Box::pin(computers::contract(
        store, fixture, mapping, &node, &body, &snapshot,
    ))
    .await;
    store
        .set_resource_share(
            &fixture.alice,
            tenant,
            ResourceKind::Session,
            session.as_str(),
            &fixture.bob.user_id,
            None,
            now + 113_000,
        )
        .await
        .unwrap();
    assert_denied(store, &node, &body).await;
    store
        .set_edge_session_model_snapshot(&fixture.alice, tenant, session, None)
        .await
        .unwrap();
}

async fn lineage_contract(
    store: &ControlStore,
    fixture: &EdgeFixture,
    mapping: &MappingFixture,
    node: &NodePrincipal,
    body: &NodeModelRequest,
) {
    let child =
        edge_provenance::subagent_mapping(store, fixture, "model-child", &mapping.browser_session)
            .await;
    let nested =
        edge_provenance::subagent_mapping(store, fixture, "model-nested", &child.browser_session)
            .await;
    let mut snapshot = store
        .find_owned_edge_session(&fixture.alice, &fixture.tenant_a, &nested.browser_session)
        .await
        .unwrap()
        .unwrap()
        .metadata
        .server_model
        .unwrap();
    snapshot.defaults.context_window = 2048;
    store
        .set_edge_session_model_snapshot(
            &fixture.alice,
            &fixture.tenant_a,
            &nested.browser_session,
            Some(snapshot.clone()),
        )
        .await
        .unwrap();
    let mut request = body.clone();
    request.session_id = nested.node_session.clone();
    let mut transaction = store.database().begin().await.unwrap();
    let principal = store
        .authorize_node_model_in(&mut transaction, node, &request)
        .await
        .unwrap();
    assert_eq!(principal.session_id, nested.browser_session);
    assert_eq!(
        principal.snapshot, snapshot,
        "a child uses its own Server snapshot, not the root's latest model"
    );
    transaction.commit().await.unwrap();
    let fork = store
        .create_edge_session_mapping(
            &fixture.alice,
            &fixture.tenant_a,
            &fixture.workspace_a,
            &fixture.executor_a,
            &SessionId::new("model-fork-node"),
            Some(&SessionId::new("model-fork-browser")),
            super::metadata(
                "ordinary fork",
                Some(mapping.browser_session.clone()),
                fixture.now + 1_000,
            ),
            fixture.now + 1_000,
        )
        .await
        .unwrap();
    store
        .set_edge_session_model_snapshot(
            &fixture.alice,
            &fixture.tenant_a,
            &fork.session_id,
            Some(snapshot),
        )
        .await
        .unwrap();
    request.session_id = fork.node_session_id;
    assert_denied(store, node, &request).await;
    request.origin_session_id = request.session_id.clone();
    let mut tx = store.database().begin().await.unwrap();
    let error = store
        .authorize_node_model_in(&mut tx, node, &request)
        .await
        .unwrap_err();
    assert!(error.message.contains("subagent ancestor"), "{error}");
}

async fn prepare_source(
    store: &ControlStore,
    fixture: &EdgeFixture,
    mapping: &MappingFixture,
) -> ternilo_protocol::RunModelSnapshot {
    let now = fixture.now + 500_000;
    let tenant = &fixture.tenant_a;
    let session = &mapping.browser_session;
    let permissions = ResourcePermissions {
        view: true,
        submit: true,
        configure: true,
        ..Default::default()
    };
    store
        .set_resource_share(
            &fixture.alice,
            tenant,
            ResourceKind::Session,
            session.as_str(),
            &fixture.bob.user_id,
            Some(permissions),
            now,
        )
        .await
        .unwrap();
    let profile = ProviderProfile {
        id: "account-provider".to_owned(),
        display_name: "Bob's Provider".to_owned(),
        base_url: "https://example.test/v1".to_owned(),
        protocol: ProviderProtocol::OpenAiChatCompletions,
        api_key_ref: Some("ACCOUNT_MODEL_KEY".to_owned()),
        defaults: ProviderModelDefaults {
            context_window: 4096,
            max_output_tokens: 100,
            reasoning: None,
        },
        models: vec![ProviderModel {
            id: "private-model".to_owned(),
            display_name: None,
            settings: ProviderModelSettings::Inherit,
        }],
        timeout_ms: 0,
        max_attempts: 2,
        retry_base_delay_ms: 1,
    };
    store
        .put_user_credential(
            &fixture.bob,
            tenant,
            "ACCOUNT_MODEL_KEY",
            "bob-private-secret",
            now,
        )
        .await
        .unwrap();
    store
        .upsert_user_provider_profile(&fixture.bob, tenant, profile, now)
        .await
        .unwrap();
    let binding = RunModelBinding::UserProvider {
        tenant_id: tenant.clone(),
        owner_user_id: fixture.bob.user_id.clone(),
        provider_id: "account-provider".to_owned(),
        model: "private-model".to_owned(),
    };
    let snapshot = store
        .resolve_workload_model_snapshot(
            &fixture.alice.user_id,
            &fixture.bob.user_id,
            tenant,
            &binding,
            None,
            now,
        )
        .await
        .unwrap();
    store
        .set_edge_session_model_snapshot(&fixture.bob, tenant, session, Some(snapshot.clone()))
        .await
        .unwrap();
    snapshot
}

async fn accepted_input(
    store: &ControlStore,
    fixture: &EdgeFixture,
    mapping: &MappingFixture,
    snapshot: &ternilo_protocol::RunModelSnapshot,
    node: &NodePrincipal,
) -> NodeModelRequest {
    let now = fixture.now + 500_000;
    let tenant = &fixture.tenant_a;
    let session = &mapping.browser_session;
    let mut refreshed = store
        .find_owned_edge_session(&fixture.alice, tenant, session)
        .await
        .unwrap()
        .unwrap()
        .metadata;
    refreshed.server_model = None;
    refreshed.model = serde_json::to_value(DefaultModelSelection::AccountProvider {
        owner_user_id: fixture.bob.user_id.clone(),
        provider_id: "account-provider".to_owned(),
        model: "private-model".to_owned(),
        reasoning_effort: None,
    })
    .unwrap();
    let updated = store
        .update_edge_session_metadata(
            &fixture.alice,
            tenant,
            session,
            &mapping.node_session,
            refreshed,
            now,
        )
        .await
        .unwrap();
    assert_eq!(
        updated.metadata.server_model,
        Some(snapshot.clone()),
        "Node refresh cannot erase the Server authorization"
    );
    let provenance = InputProvenance {
        run_id: Some(RunId::new("account-model-run")),
        input_id: SubmissionId::new("account-model-input"),
        author: InputAuthor::Account {
            user_id: fixture.alice.user_id.clone(),
            username: fixture.alice.username.clone(),
        },
    };
    let body = NodeModelRequest {
        schedule_origins: Vec::new(),
        session_id: mapping.node_session.clone(),
        origin_session_id: mapping.node_session.clone(),
        run_id: RunId::new("account-model-run"),
        provenance: Some(provenance.clone()),
        request_id: "request-1".to_owned(),
        binding: snapshot.binding.clone(),
        request: ModelRequest {
            run_id: RunId::new("account-model-run"),
            system_prompt: String::new(),
            messages: Vec::new(),
            tools: Vec::new(),
            step: 1,
        },
    };
    assert_denied(store, node, &body).await;
    let mut tx = store.database().tenant_transaction(tenant).await.unwrap();
    EdgeStore::record_input_provenance_in_transaction(
        &mut tx,
        tenant,
        &fixture.executor_a,
        &mapping.node_session,
        None,
        &provenance,
        now,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let mut forged = body.clone();
    forged.request.run_id = RunId::new("unrelated-run");
    assert_denied(store, node, &forged).await;
    forged.run_id = forged.request.run_id.clone();
    assert_denied(store, node, &forged).await;
    forged.provenance.as_mut().unwrap().run_id = Some(forged.run_id.clone());
    assert_denied(store, node, &forged).await;
    forged = body.clone();
    forged.origin_session_id = SessionId::new("unrelated-session");
    assert_denied(store, node, &forged).await;
    forged = body.clone();
    forged.provenance.as_mut().unwrap().author = InputAuthor::Account {
        user_id: fixture.bob.user_id.clone(),
        username: fixture.bob.username.clone(),
    };
    assert_denied(store, node, &forged).await;
    forged.provenance = None;
    assert_denied(store, node, &forged).await;
    forged = body.clone();
    forged.binding = RunModelBinding::UserProvider {
        tenant_id: tenant.clone(),
        owner_user_id: fixture.bob.user_id.clone(),
        provider_id: "another-provider".to_owned(),
        model: "private-model".to_owned(),
    };
    assert_denied(store, node, &forged).await;
    body
}

async fn ledger_contract(
    store: &ControlStore,
    principal: &ternilo_control::NodeModelPrincipal,
    now: u64,
) {
    let mut tx = store.database().begin().await.unwrap();
    let input = ModelRequestInput {
        request_key: "node-test-request".to_owned(),
        payload_hash: "a".repeat(64),
        model_id: "private-model".to_owned(),
        protocol: principal.snapshot.protocol,
        reserved_tokens: 100,
    };
    let permit = store
        .reserve_node_model_request_in(&mut tx, principal, &input, now)
        .await
        .unwrap();
    assert_eq!(
        permit.route.api_key.as_ref().map(|key| key.as_str()),
        Some("bob-private-secret")
    );
    let id = permit.request.request_id.clone();
    let duplicate = store
        .reserve_node_model_request_in(&mut tx, principal, &input, now + 1)
        .await
        .unwrap();
    assert!(!duplicate.newly_accepted);
    assert_eq!(duplicate.request.request_id, id);
    store
        .begin_node_model_attempt_in(&mut tx, principal, &id, 1, now + 2)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    store
        .settle_model_attempt(&id, 1, &settled(ModelRequestState::Failed, 12), now + 3)
        .await
        .unwrap();
    let mut tx = store.database().begin().await.unwrap();
    store
        .begin_node_model_attempt_in(&mut tx, principal, &id, 2, now + 4)
        .await
        .unwrap();
    store
        .check_node_model_request_in(&mut tx, principal, &id, now + 50_000)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        store
            .expire_model_requests(now + 65_000, 100)
            .await
            .unwrap(),
        0,
        "timeout zero survives the original recovery deadline while live"
    );
    assert_eq!(
        store
            .expire_model_requests(now + 111_000, 100)
            .await
            .unwrap(),
        1,
        "an abandoned unlimited request is recovered"
    );
    store
        .settle_model_attempt(
            &id,
            2,
            &settled(ModelRequestState::Cancelled, 20),
            now + 112_000,
        )
        .await
        .unwrap();
}

fn settled(state: ModelRequestState, count: u64) -> ModelRequestSettlement {
    ModelRequestSettlement {
        state,
        usage: Some(ServiceModelUsage {
            input_tokens: Some(count),
            output_tokens: Some(0),
            ..Default::default()
        }),
        upstream_request_id: None,
        error_code: None,
    }
}

async fn assert_denied(store: &ControlStore, node: &NodePrincipal, body: &NodeModelRequest) {
    let mut tx = store.database().begin().await.unwrap();
    assert!(
        store
            .authorize_node_model_in(&mut tx, node, body)
            .await
            .is_err()
    );
    tx.rollback().await.unwrap();
}

async fn node_principal(store: &ControlStore, fixture: &EdgeFixture) -> NodePrincipal {
    let mut tx = store
        .database()
        .tenant_transaction(&fixture.tenant_a)
        .await
        .unwrap();
    let credential_id: String = sqlx::query_scalar("SELECT credential_id FROM control_node_credentials WHERE tenant_id=$1 AND executor_id=$2 AND revoked_at_ms IS NULL")
        .bind(fixture.tenant_a.as_str()).bind(fixture.executor_a.as_str()).fetch_one(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
    NodePrincipal {
        credential_id,
        scope: ternilo_transport::ExecutorScope {
            tenant_id: fixture.tenant_a.clone(),
            user_id: fixture.alice.user_id.clone(),
        },
        executor_id: fixture.executor_a.clone(),
        project_id: None,
    }
}
