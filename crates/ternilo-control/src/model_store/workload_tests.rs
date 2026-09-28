use std::time::Duration;

use ternilo_protocol::{
    ProviderModel, ProviderModelDefaults, ProviderModelSettings, ProviderProfile, ProviderProtocol,
    RunId, RunModelBinding, SessionId, WorkspaceId,
};

use super::*;
use crate::{
    ControlUser, InstanceMode, NativeRegistration, OidcPrincipal, SecretCipher, TenantQuota,
    TenantRole,
};

const NOW: u64 = 1_800_000_000_000;

mod nodes;

#[expect(
    clippy::too_many_lines,
    reason = "Set up independent resource, execution, platform and BYOK owners for the backend contract."
)]
async fn fixture(
    store: &ControlStore,
) -> (
    ControlUser,
    ControlUser,
    WorkloadModelPrincipal,
    ModelGrantInput,
) {
    let owner = store
        .initialize_owner(
            &NativeRegistration {
                email: "workload-owner@example.test".to_owned(),
                username: "workload-owner".to_owned(),
                password: "workload-owner-password".to_owned(),
            },
            NOW,
        )
        .await
        .unwrap()
        .session
        .user;
    store
        .set_instance_mode(&owner, InstanceMode::MultiUser, 1, NOW)
        .await
        .unwrap();
    let actor = store
        .upsert_user(
            &OidcPrincipal {
                issuer: "https://identity.example".to_owned(),
                subject: "collaborator".to_owned(),
                email: None,
                display_name: Some("Collaborator".to_owned()),
            },
            "test-collaborator",
            NOW,
        )
        .await
        .unwrap();
    let tenant = store
        .create_tenant(
            &owner,
            "workload-team",
            "Workload team",
            TenantQuota {
                max_nodes: 2,
                max_concurrent_runs: 8,
                monthly_model_tokens: 1_000,
                max_secrets: 8,
            },
            NOW,
        )
        .await
        .unwrap();
    store
        .set_membership(
            &owner,
            &tenant.tenant_id,
            &actor.user_id,
            TenantRole::Member,
            NOW,
        )
        .await
        .unwrap();
    let project = store
        .list_projects(&owner, &tenant.tenant_id)
        .await
        .unwrap()
        .remove(0);
    let profile = ProviderProfile {
        id: "upstream".to_owned(),
        display_name: "Upstream".to_owned(),
        base_url: "https://models.example/v1".to_owned(),
        protocol: ProviderProtocol::OpenAiResponses,
        api_key_ref: None,
        defaults: ProviderModelDefaults {
            context_window: 1000,
            max_output_tokens: 100,
            reasoning: None,
        },
        models: vec![ProviderModel {
            id: "upstream-model".to_owned(),
            display_name: None,
            settings: ProviderModelSettings::Inherit,
        }],
        timeout_ms: 1000,
        max_attempts: 3,
        retry_base_delay_ms: 1,
    };
    store
        .save_model_provider(
            &owner,
            &ModelProviderInput {
                profile: profile.clone(),
                enabled: true,
                api_key: Some("platform-secret".to_owned()),
                clear_api_key: false,
            },
            NOW,
        )
        .await
        .unwrap();
    store
        .save_model_publication(
            &owner,
            &ModelPublicationInput {
                model_id: "public".to_owned(),
                display_name: "Public".to_owned(),
                provider_id: "upstream".to_owned(),
                upstream_model: "upstream-model".to_owned(),
                enabled: true,
            },
            NOW,
        )
        .await
        .unwrap();
    let grant_input = ModelGrantInput {
        name: "Owner grant".to_owned(),
        subject: ModelGrantSubject::User {
            id: owner.user_id.to_string(),
        },
        model_ids: vec!["public".to_owned()],
        monthly_tokens: 2000,
        max_concurrent_requests: 1,
        expires_at_ms: None,
        allow_resource_sharing: true,
    };
    let grant = store
        .save_model_grant(&owner, None, &grant_input, NOW)
        .await
        .unwrap();
    let reservation = store
        .reserve_quota(
            &owner,
            &tenant.tenant_id,
            Some("workload-run"),
            800,
            Duration::from_hours(1),
            NOW,
        )
        .await
        .unwrap();
    let principal = WorkloadModelPrincipal {
        tenant_id: tenant.tenant_id,
        project_id: project.project_id,
        workspace_id: WorkspaceId::new("workspace"),
        session_id: SessionId::new("session"),
        authorization_session_id: SessionId::new("authority-session"),
        run_id: RunId::new("workload-run"),
        actor_user_id: actor.user_id.clone(),
        resource_owner_user_id: owner.user_id.clone(),
        execution_owner_user_id: owner.user_id.clone(),
        execution_reservation_id: reservation.reservation_id,
        worker_id: "worker".to_owned(),
        worker_generation: 1,
        lease_token: 1,
        writer_fencing_token: 1,
        model: RunModelBinding::Platform {
            grant_id: grant.grant_id,
            model_id: "public".to_owned(),
            beneficiary_user_id: owner.user_id.clone(),
        },
        run_token_limit: 800,
    };
    let byok = ProviderProfile {
        id: "byok".to_owned(),
        api_key_ref: Some("BYOK_KEY".to_owned()),
        ..profile
    };
    store
        .upsert_user_provider_profile(&owner, &principal.tenant_id, byok, NOW)
        .await
        .unwrap();
    store
        .put_user_credential(&owner, &principal.tenant_id, "BYOK_KEY", "byok-secret", NOW)
        .await
        .unwrap();
    (owner, actor, principal, grant_input)
}

fn request(id: &str, model: &str, tokens: u64) -> ModelRequestInput {
    ModelRequestInput {
        request_key: id.to_owned(),
        payload_hash: "a".repeat(64),
        model_id: model.to_owned(),
        protocol: ProviderProtocol::OpenAiResponses,
        reserved_tokens: tokens,
    }
}

fn settlement(state: ModelRequestState, tokens: Option<u64>) -> ModelRequestSettlement {
    ModelRequestSettlement {
        state,
        usage: tokens.map(|value| ServiceModelUsage {
            input_tokens: Some(value),
            output_tokens: Some(0),
            ..ServiceModelUsage::default()
        }),
        upstream_request_id: None,
        error_code: (state == ModelRequestState::Failed).then(|| "upstream_failure".to_owned()),
    }
}

async fn reserve(
    store: &ControlStore,
    principal: &WorkloadModelPrincipal,
    id: &str,
    tokens: u64,
    now: u64,
) -> Result<ModelRequestPermit, ModelAccessError> {
    let mut tx = store
        .database()
        .tenant_transaction(&principal.tenant_id)
        .await?;
    let result = store
        .reserve_workload_model_request_in(
            &mut tx,
            principal,
            &request(id, principal.model.model_id(), tokens),
            now,
        )
        .await?;
    tx.commit().await.map_err(ternilo_storage::database_error)?;
    Ok(result)
}

async fn begin(
    store: &ControlStore,
    principal: &WorkloadModelPrincipal,
    id: &str,
    attempt: u32,
    now: u64,
) -> Result<ModelServiceAttempt, ModelAccessError> {
    let mut tx = store
        .database()
        .tenant_transaction(&principal.tenant_id)
        .await?;
    let result = store
        .begin_workload_model_attempt_in(&mut tx, principal, id, attempt, now)
        .await?;
    tx.commit().await.map_err(ternilo_storage::database_error)?;
    Ok(result)
}

#[tokio::test]
async fn sqlite_workload_attempts_share_public_grants_and_keep_usage_after_termination() {
    let directory = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}",
        directory.path().join("workloads.sqlite3").display()
    );
    let store = ControlStore::connect(&url, None, SecretCipher::from_key([23; 32]), 1)
        .await
        .unwrap();
    Box::pin(workload_contract(&store)).await;
}

#[tokio::test]
#[ignore = "requires TERNILO_TEST_DATABASE_URL pointing to a disposable PostgreSQL database"]
async fn postgres_workload_attempts_share_public_grants_with_runtime_rls() {
    use sqlx::Executor as _;
    let url = std::env::var("TERNILO_TEST_DATABASE_URL").unwrap();
    assert!(url.contains("ternilo_control_test"));
    let admin = sqlx::PgPool::connect(&url).await.unwrap();
    admin
        .execute("DROP SCHEMA IF EXISTS public CASCADE")
        .await
        .unwrap();
    admin.execute("CREATE SCHEMA public").await.unwrap();
    crate::postgres_test::prepare_role(&admin, "ternilo_workload_test", "workload-test-password")
        .await;
    let owner = ControlStore::connect(&url, None, SecretCipher::from_key([23; 32]), 1)
        .await
        .unwrap();
    owner.database().close().await;
    let mut runtime = url
        .parse::<sqlx::any::AnyConnectOptions>()
        .unwrap()
        .database_url;
    runtime.set_username("ternilo_workload_test").unwrap();
    runtime
        .set_password(Some("workload-test-password"))
        .unwrap();
    let store = ControlStore::connect(
        runtime.as_str(),
        Some(&url),
        SecretCipher::from_key([23; 32]),
        8,
    )
    .await
    .unwrap();
    Box::pin(workload_contract(&store)).await;
    let visible: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM control_model_attempts")
        .fetch_one(store.database().pool())
        .await
        .unwrap();
    assert_eq!(visible, 0);
}

#[expect(
    clippy::too_many_lines,
    reason = "Verify shared request admission, independent attempts, retained budgets and post-termination settlement as one backend contract."
)]
async fn workload_contract(store: &ControlStore) {
    let (owner, actor, principal, mut grant_input) = fixture(store).await;
    let RunModelBinding::Platform { grant_id, .. } = &principal.model else {
        unreachable!()
    };
    let key = store
        .create_model_key(
            &owner,
            &ModelKeyInput {
                name: "Public client".to_owned(),
                grant_id: grant_id.clone(),
                model_ids: vec!["public".to_owned()],
                monthly_tokens: None,
                max_concurrent_requests: None,
                expires_at_ms: None,
            },
            NOW,
        )
        .await
        .unwrap();
    let accepted = reserve(store, &principal, "logical-1", 250, NOW)
        .await
        .unwrap();
    assert_eq!(accepted.request.actor_user_id, actor.user_id);
    assert_eq!(
        accepted.request.resource_owner_user_id,
        Some(owner.user_id.clone())
    );
    assert_eq!(accepted.request.model_beneficiary_user_id, owner.user_id);
    assert!(accepted.request.key_id.is_none());
    assert_eq!(
        accepted.route.api_key.as_deref().map(String::as_str),
        Some("platform-secret")
    );
    assert_eq!(
        store
            .reserve_model_request(&key.token, &request("public-competing", "public", 1), NOW)
            .await
            .err()
            .unwrap()
            .kind,
        ModelAccessErrorKind::QuotaExceeded
    );
    assert!(
        !reserve(store, &principal, "logical-1", 250, NOW)
            .await
            .unwrap()
            .newly_accepted
    );
    let mut wrong = principal.clone();
    wrong.actor_user_id = owner.user_id.clone();
    assert!(
        begin(store, &wrong, &accepted.request.request_id, 1, NOW)
            .await
            .is_err()
    );
    begin(store, &principal, &accepted.request.request_id, 1, NOW)
        .await
        .unwrap();
    store
        .settle_model_attempt(
            &accepted.request.request_id,
            1,
            &settlement(ModelRequestState::Failed, Some(50)),
            NOW + 1,
        )
        .await
        .unwrap();
    begin(store, &principal, &accepted.request.request_id, 2, NOW + 2)
        .await
        .unwrap();
    store
        .settle_model_attempt(
            &accepted.request.request_id,
            2,
            &settlement(ModelRequestState::Failed, None),
            NOW + 3,
        )
        .await
        .unwrap();
    begin(store, &principal, &accepted.request.request_id, 3, NOW + 4)
        .await
        .unwrap();
    assert!(
        begin(store, &principal, &accepted.request.request_id, 4, NOW + 4)
            .await
            .is_err()
    );
    store
        .settle_model_attempt(
            &accepted.request.request_id,
            3,
            &settlement(ModelRequestState::Completed, Some(70)),
            NOW + 5,
        )
        .await
        .unwrap();
    let finished = store
        .finish_workload_model_request(
            &accepted.request.request_id,
            ModelRequestState::Completed,
            None,
            NOW + 6,
        )
        .await
        .unwrap();
    assert_eq!(finished.attempts.len(), 3);
    assert_eq!(finished.accounted_tokens, None);
    let grant = store
        .get_model_grant(&owner, grant_id, NOW + 6)
        .await
        .unwrap();
    assert_eq!(
        (
            grant.quota.used_tokens,
            grant.quota.reserved_tokens,
            grant.quota.active_requests
        ),
        (120, 250, 0)
    );
    assert!(
        store
            .reserve_quota(
                &owner,
                &principal.tenant_id,
                None,
                201,
                Duration::from_hours(1),
                NOW + 7
            )
            .await
            .is_err(),
        "active Run keeps its full 800-token admission reservation without adding attempt suballocations"
    );
    let mut tx = store
        .database()
        .tenant_transaction(&principal.tenant_id)
        .await
        .unwrap();
    let summary = ControlStore::finalize_workload_reservation_in(
        &mut tx,
        &principal.tenant_id,
        &principal.execution_reservation_id,
        NOW + 8,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        (summary.used_model_tokens, summary.unknown_model_tokens),
        (120, 250)
    );
    let second = store
        .reserve_quota(
            &owner,
            &principal.tenant_id,
            None,
            630,
            Duration::from_hours(1),
            NOW + 9,
        )
        .await
        .unwrap();
    assert!(
        store
            .reserve_quota(
                &owner,
                &principal.tenant_id,
                None,
                1,
                Duration::from_hours(1),
                NOW + 9
            )
            .await
            .is_err()
    );
    assert!(
        begin(store, &principal, &accepted.request.request_id, 3, NOW + 9)
            .await
            .is_err()
    );
    store
        .revoke_model_grant(&owner, grant_id, NOW + 10)
        .await
        .unwrap();
    store
        .reconcile_model_usage(
            &owner,
            &accepted.request.request_id,
            2,
            &super::ModelUsageReconciliationInput {
                expected_settled_at_ms: NOW + 3,
                usage: settlement(ModelRequestState::Failed, Some(20))
                    .usage
                    .unwrap(),
                reference: "upstream/retried-attempt-2".to_owned(),
                note: "Verified after workload termination and grant revocation.".to_owned(),
            },
            NOW + 11,
        )
        .await
        .unwrap();
    assert_eq!(
        store
            .model_usage_reconciliations(&owner, &accepted.request.request_id)
            .await
            .unwrap()
            .len(),
        1
    );
    store
        .settle_model_attempt(
            &accepted.request.request_id,
            2,
            &settlement(ModelRequestState::Failed, Some(20)),
            NOW + 12,
        )
        .await
        .unwrap();
    assert!(
        store
            .settle_model_attempt(
                &accepted.request.request_id,
                2,
                &settlement(ModelRequestState::Failed, Some(21)),
                NOW + 12
            )
            .await
            .is_err()
    );
    let summary = store
        .model_service_usage(&owner, None, NOW + 12)
        .await
        .unwrap();
    assert_eq!((summary.used_tokens, summary.reserved_tokens), (140, 0));
    let more = store
        .reserve_quota(
            &owner,
            &principal.tenant_id,
            None,
            230,
            Duration::from_hours(1),
            NOW + 13,
        )
        .await
        .unwrap();
    store
        .release_quota_reservation(
            &owner,
            &principal.tenant_id,
            &second.reservation_id,
            NOW + 14,
        )
        .await
        .unwrap();
    store
        .release_quota_reservation(&owner, &principal.tenant_id, &more.reservation_id, NOW + 14)
        .await
        .unwrap();
    let reservation = store
        .reserve_quota(
            &owner,
            &principal.tenant_id,
            Some("byok-run"),
            800,
            Duration::from_hours(1),
            NOW + 15,
        )
        .await
        .unwrap();
    let mut byok = principal.clone();
    byok.run_id = RunId::new("byok-run");
    byok.execution_reservation_id = reservation.reservation_id;
    byok.model = RunModelBinding::UserProvider {
        tenant_id: principal.tenant_id.clone(),
        owner_user_id: owner.user_id.clone(),
        provider_id: "byok".to_owned(),
        model: "upstream-model".to_owned(),
    };
    let accepted = reserve(store, &byok, "byok-logical", 100, NOW + 15)
        .await
        .unwrap();
    assert!(accepted.request.grant_id.is_none());
    assert_eq!(accepted.request.source, ModelRequestSource::UserProvider);
    assert_eq!(
        accepted.route.api_key.as_deref().map(String::as_str),
        Some("byok-secret")
    );
    store
        .set_instance_mode(&owner, InstanceMode::SingleUser, 2, NOW + 16)
        .await
        .unwrap();
    begin(store, &byok, &accepted.request.request_id, 1, NOW + 17)
        .await
        .unwrap();
    store
        .settle_model_attempt(
            &accepted.request.request_id,
            1,
            &settlement(ModelRequestState::Completed, Some(15)),
            NOW + 18,
        )
        .await
        .unwrap();
    store
        .finish_workload_model_request(
            &accepted.request.request_id,
            ModelRequestState::Completed,
            None,
            NOW + 18,
        )
        .await
        .unwrap();
    store
        .set_instance_mode(&owner, InstanceMode::MultiUser, 3, NOW + 19)
        .await
        .unwrap();
    grant_input.allow_resource_sharing = false;
    let no_share = store
        .save_model_grant(&owner, None, &grant_input, NOW + 20)
        .await
        .unwrap();
    let mut blocked = byok.clone();
    blocked.model = RunModelBinding::Platform {
        grant_id: no_share.grant_id,
        model_id: "public".to_owned(),
        beneficiary_user_id: owner.user_id.clone(),
    };
    assert!(
        reserve(store, &blocked, "sharing-denied", 1, NOW + 20)
            .await
            .is_err()
    );
    blocked.actor_user_id = owner.user_id.clone();
    let owner_request = reserve(store, &blocked, "owner-still-allowed", 1, NOW + 21)
        .await
        .unwrap();
    store
        .finish_workload_model_request(
            &owner_request.request.request_id,
            ModelRequestState::Cancelled,
            None,
            NOW + 22,
        )
        .await
        .unwrap();
    usage_source_contract(store, &owner).await;
    account_provider_contract(store, &owner, &actor, &principal).await;
    workload_ceiling_contract(store, &owner, &principal).await;
    workload_calendar_contract(store, &owner, &principal).await;
    workload_concurrency_contract(store, &owner, &principal).await;
    workload_group_revocation_contract(store, &owner, &principal).await;
    workload_visibility_contract(store, &owner, &actor, &principal).await;
    workload_byok_readiness_contract(store, &owner, &actor, &principal).await;
    nodes::contract(store, &owner, &actor, &principal).await;
}

#[expect(
    clippy::too_many_lines,
    reason = "Verify account source, execution budget and revocation across two independent owners in one transaction contract."
)]
async fn account_provider_contract(
    store: &ControlStore,
    owner: &ControlUser,
    actor: &ControlUser,
    base: &WorkloadModelPrincipal,
) {
    let now = NOW + 30;
    let mut principal = isolated_principal(store, owner, base, "account-source", now).await;
    principal.actor_user_id = actor.user_id.clone();
    store
        .set_membership(
            owner,
            &principal.tenant_id,
            &actor.user_id,
            TenantRole::Member,
            now,
        )
        .await
        .unwrap();
    let personal = store.account_provider_space(&owner.user_id).await.unwrap();
    let other_personal = store.account_provider_space(&actor.user_id).await.unwrap();
    let profile = ProviderProfile {
        id: "private-account".to_owned(),
        display_name: "Private account model".to_owned(),
        base_url: "https://account-model.example/v1".to_owned(),
        protocol: ProviderProtocol::OpenAiResponses,
        api_key_ref: Some("ACCOUNT_MODEL_KEY".to_owned()),
        defaults: ProviderModelDefaults {
            context_window: 4096,
            max_output_tokens: 1024,
            reasoning: None,
        },
        models: vec![ProviderModel {
            id: "account-model".to_owned(),
            display_name: None,
            settings: ProviderModelSettings::Inherit,
        }],
        timeout_ms: 1000,
        max_attempts: 1,
        retry_base_delay_ms: 1,
    };
    store
        .upsert_user_provider_profile(owner, &personal, profile.clone(), now)
        .await
        .unwrap();
    store
        .put_user_credential(
            owner,
            &personal,
            "ACCOUNT_MODEL_KEY",
            "owner-private-secret",
            now,
        )
        .await
        .unwrap();
    store
        .upsert_user_provider_profile(actor, &other_personal, profile, now)
        .await
        .unwrap();
    store
        .put_user_credential(
            actor,
            &other_personal,
            "ACCOUNT_MODEL_KEY",
            "other-private-secret",
            now,
        )
        .await
        .unwrap();
    principal.model = RunModelBinding::UserProvider {
        tenant_id: personal.clone(),
        owner_user_id: owner.user_id.clone(),
        provider_id: "private-account".to_owned(),
        model: "account-model".to_owned(),
    };
    let snapshot = store
        .resolve_workload_model_snapshot(
            &actor.user_id,
            &owner.user_id,
            &principal.tenant_id,
            &principal.model,
            None,
            now,
        )
        .await
        .unwrap();
    assert_eq!(snapshot.binding, principal.model);
    let mut forged = principal.clone();
    if let RunModelBinding::UserProvider { tenant_id, .. } = &mut forged.model {
        *tenant_id = other_personal;
    }
    assert!(
        reserve(store, &forged, "wrong-account-namespace", 100, now)
            .await
            .is_err()
    );
    let mut wrong_owner = principal.clone();
    if let RunModelBinding::UserProvider { owner_user_id, .. } = &mut wrong_owner.model {
        *owner_user_id = actor.user_id.clone();
    }
    assert!(
        reserve(store, &wrong_owner, "wrong-account-owner", 100, now)
            .await
            .is_err()
    );
    let accepted = reserve(store, &principal, "account-call", 100, now)
        .await
        .unwrap();
    assert_eq!(
        accepted.route.api_key.as_deref().map(String::as_str),
        Some("owner-private-secret")
    );
    assert_eq!(accepted.request.actor_user_id, actor.user_id);
    assert_eq!(accepted.request.model_beneficiary_user_id, owner.user_id);
    assert_eq!(
        accepted.request.workload.as_ref().unwrap().tenant_id,
        principal.tenant_id
    );
    begin(store, &principal, &accepted.request.request_id, 1, now + 1)
        .await
        .unwrap();
    store
        .delete_user_credential(owner, &personal, "ACCOUNT_MODEL_KEY")
        .await
        .unwrap();
    let mut transaction = store
        .database()
        .tenant_transaction(&principal.tenant_id)
        .await
        .unwrap();
    assert!(
        store
            .check_workload_model_request_in(
                &mut transaction,
                &principal,
                &accepted.request.request_id,
                now + 2
            )
            .await
            .is_err()
    );
    transaction.rollback().await.unwrap();
    assert!(
        store
            .resolve_workload_model_snapshot(
                &actor.user_id,
                &owner.user_id,
                &principal.tenant_id,
                &principal.model,
                None,
                now + 2
            )
            .await
            .is_err()
    );
}

async fn usage_source_contract(store: &ControlStore, owner: &ControlUser) {
    let mut total_requests = 0;
    let mut total_tokens = 0;
    for source in [
        ModelRequestSource::UserProvider,
        ModelRequestSource::PlatformGrant,
    ] {
        let mut query = crate::PageQuery {
            limit: 1,
            ..crate::PageQuery::default()
        };
        let mut requests = Vec::new();
        loop {
            let page = store
                .list_model_service_requests_by_source(
                    owner,
                    Some(&owner.user_id),
                    &query,
                    Some(source),
                )
                .await
                .unwrap();
            assert!(page.requests.iter().all(|request| request.source == source));
            requests.extend(page.requests);
            query.cursor = page.next_cursor;
            if query.cursor.is_none() {
                break;
            }
        }
        assert!(!requests.is_empty());
        let usage = store
            .model_service_usage_by_source(owner, Some(&owner.user_id), NOW + 22, Some(source))
            .await
            .unwrap();
        assert_eq!(usage.request_count, requests.len() as u64);
        let tokens: u64 = requests
            .iter()
            .flat_map(|request| &request.attempts)
            .filter_map(|attempt| attempt.accounted_tokens)
            .sum();
        assert_eq!(usage.used_tokens, tokens);
        total_requests += usage.request_count;
        total_tokens += usage.used_tokens;
    }
    let all = store
        .model_service_usage(owner, Some(&owner.user_id), NOW + 22)
        .await
        .unwrap();
    assert_eq!(
        (all.request_count, all.used_tokens),
        (total_requests, total_tokens)
    );
}

fn at(value: &str) -> u64 {
    chrono::DateTime::parse_from_rfc3339(value)
        .unwrap()
        .timestamp_millis()
        .try_into()
        .unwrap()
}

async fn isolated_principal(
    store: &ControlStore,
    owner: &ControlUser,
    base: &WorkloadModelPrincipal,
    name: &str,
    now: u64,
) -> WorkloadModelPrincipal {
    let tenant = store
        .create_tenant(
            owner,
            name,
            name,
            TenantQuota {
                max_nodes: 1,
                max_concurrent_runs: 8,
                monthly_model_tokens: 1000,
                max_secrets: 1,
            },
            now,
        )
        .await
        .unwrap();
    let project = store
        .list_projects(owner, &tenant.tenant_id)
        .await
        .unwrap()
        .remove(0);
    let reservation = store
        .reserve_quota(
            owner,
            &tenant.tenant_id,
            Some(name),
            800,
            Duration::from_hours(1),
            now,
        )
        .await
        .unwrap();
    let grant = store
        .save_model_grant(
            owner,
            None,
            &ModelGrantInput {
                name: name.to_owned(),
                subject: ModelGrantSubject::User {
                    id: owner.user_id.to_string(),
                },
                model_ids: vec!["public".to_owned()],
                monthly_tokens: 2000,
                max_concurrent_requests: 1,
                expires_at_ms: None,
                allow_resource_sharing: true,
            },
            now,
        )
        .await
        .unwrap();
    WorkloadModelPrincipal {
        tenant_id: tenant.tenant_id,
        project_id: project.project_id,
        run_id: RunId::new(name),
        actor_user_id: owner.user_id.clone(),
        execution_reservation_id: reservation.reservation_id,
        model: RunModelBinding::Platform {
            grant_id: grant.grant_id,
            model_id: "public".to_owned(),
            beneficiary_user_id: owner.user_id.clone(),
        },
        ..base.clone()
    }
}

async fn check(
    store: &ControlStore,
    principal: &WorkloadModelPrincipal,
    id: &str,
    now: u64,
) -> Result<(), ModelAccessError> {
    let mut tx = store
        .database()
        .tenant_transaction(&principal.tenant_id)
        .await?;
    store
        .check_workload_model_request_in(&mut tx, principal, id, now)
        .await?;
    tx.commit().await.map_err(ternilo_storage::database_error)?;
    Ok(())
}

#[expect(
    clippy::too_many_lines,
    reason = "Keep acceptance month, execution month, retry and late settlement assertions together."
)]
async fn workload_calendar_contract(
    store: &ControlStore,
    owner: &ControlUser,
    base: &WorkloadModelPrincipal,
) {
    let january = at("2030-01-31T23:59:59Z");
    let february = january + 1000;
    for (name, accepted_at, expected_grant_month) in [
        ("retry-cross-month", january, "2030-01"),
        ("run-cross-month", february, "2030-02"),
    ] {
        let principal = isolated_principal(store, owner, base, name, january - 1000).await;
        let RunModelBinding::Platform { grant_id, .. } = &principal.model else {
            unreachable!()
        };
        let request = reserve(store, &principal, name, 250, accepted_at)
            .await
            .unwrap();
        begin(
            store,
            &principal,
            &request.request.request_id,
            1,
            accepted_at,
        )
        .await
        .unwrap();
        store
            .settle_model_attempt(
                &request.request.request_id,
                1,
                &settlement(ModelRequestState::Failed, Some(50)),
                accepted_at + 1,
            )
            .await
            .unwrap();
        begin(
            store,
            &principal,
            &request.request.request_id,
            2,
            february + 10,
        )
        .await
        .unwrap();
        store
            .settle_model_attempt(
                &request.request.request_id,
                2,
                &settlement(ModelRequestState::Completed, Some(20)),
                february + 20,
            )
            .await
            .unwrap();
        let completed = store
            .finish_workload_model_request(
                &request.request.request_id,
                ModelRequestState::Completed,
                None,
                february + 30,
            )
            .await
            .unwrap();
        assert_eq!(completed.month, expected_grant_month);
        assert_eq!(completed.accounted_tokens, Some(70));
        let mut tx = store
            .database()
            .tenant_transaction(&principal.tenant_id)
            .await
            .unwrap();
        let summary = ControlStore::finalize_workload_reservation_in(
            &mut tx,
            &principal.tenant_id,
            &principal.execution_reservation_id,
            at("2030-03-01T00:00:00Z"),
        )
        .await
        .unwrap();
        assert_eq!(summary.period_start, "2030-01-01");
        assert_eq!(summary.used_model_tokens, 70);
        tx.commit().await.unwrap();
        let january_quota = store
            .get_model_grant(owner, grant_id, january)
            .await
            .unwrap()
            .quota;
        let february_quota = store
            .get_model_grant(owner, grant_id, february)
            .await
            .unwrap()
            .quota;
        assert_eq!(
            (january_quota.used_tokens, february_quota.used_tokens),
            if expected_grant_month == "2030-01" {
                (70, 0)
            } else {
                (0, 70)
            }
        );
        let mut tx = store
            .database()
            .tenant_transaction(&principal.tenant_id)
            .await
            .unwrap();
        let january_usage: i64 = sqlx::query_scalar("SELECT used_model_tokens FROM control_quota_usage WHERE tenant_id=$1 AND period_start='2030-01-01'").bind(principal.tenant_id.as_str()).fetch_one(&mut *tx).await.unwrap();
        let february_rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM control_quota_usage WHERE tenant_id=$1 AND period_start='2030-02-01'").bind(principal.tenant_id.as_str()).fetch_one(&mut *tx).await.unwrap();
        assert_eq!(january_usage, 70);
        assert_eq!(february_rows, 0);
        tx.commit().await.unwrap();
    }
}

async fn workload_concurrency_contract(
    store: &ControlStore,
    owner: &ControlUser,
    base: &WorkloadModelPrincipal,
) {
    let principal = isolated_principal(store, owner, base, "concurrent-admission", NOW).await;
    let RunModelBinding::Platform { grant_id, .. } = &principal.model else {
        unreachable!()
    };
    let key = store
        .create_model_key(
            owner,
            &ModelKeyInput {
                name: "Concurrent public client".to_owned(),
                grant_id: grant_id.clone(),
                model_ids: vec!["public".to_owned()],
                monthly_tokens: None,
                max_concurrent_requests: None,
                expires_at_ms: None,
            },
            NOW,
        )
        .await
        .unwrap();
    let public_input = request("concurrent-public", "public", 250);
    let (workload, public) = tokio::join!(
        reserve(store, &principal, "concurrent-workload", 250, NOW),
        store.reserve_model_request(&key.token, &public_input, NOW)
    );
    assert_eq!(
        usize::from(workload.is_ok()) + usize::from(public.is_ok()),
        1,
        "public and Worker admissions share one atomic grant concurrency slot"
    );
    match (workload, public) {
        (Ok(accepted), Err(error)) => {
            assert_eq!(error.kind, ModelAccessErrorKind::QuotaExceeded);
            store
                .finish_workload_model_request(
                    &accepted.request.request_id,
                    ModelRequestState::Cancelled,
                    None,
                    NOW + 1,
                )
                .await
                .unwrap();
        }
        (Err(error), Ok(accepted)) => {
            assert_eq!(error.kind, ModelAccessErrorKind::QuotaExceeded);
            store
                .settle_model_request(
                    &accepted.request.request_id,
                    &settlement(ModelRequestState::Cancelled, None),
                    NOW + 1,
                )
                .await
                .unwrap();
        }
        _ => unreachable!(),
    }
    let grant = store
        .get_model_grant(owner, grant_id, NOW + 1)
        .await
        .unwrap();
    assert_eq!(
        (
            grant.quota.used_tokens,
            grant.quota.reserved_tokens,
            grant.quota.active_requests
        ),
        (0, 0, 0),
        "cancelled attempts that never reached upstream consume no budget"
    );
    assert!(
        store
            .release_quota_reservation(
                owner,
                &principal.tenant_id,
                &principal.execution_reservation_id,
                NOW + 2
            )
            .await
            .is_err(),
        "public release cannot bypass attached execution authority"
    );
}

async fn workload_group_revocation_contract(
    store: &ControlStore,
    owner: &ControlUser,
    base: &WorkloadModelPrincipal,
) {
    let mut principal = isolated_principal(store, owner, base, "group-revocation", NOW).await;
    let group = store
        .save_model_group(
            owner,
            None,
            &crate::GroupInput {
                name: "Model group".to_owned(),
                description: None,
            },
            NOW,
        )
        .await
        .unwrap();
    store
        .set_model_group_member(owner, &group.group_id, &owner.user_id, NOW)
        .await
        .unwrap();
    let grant = store
        .save_model_grant(
            owner,
            None,
            &ModelGrantInput {
                name: "Group grant".to_owned(),
                subject: ModelGrantSubject::Group {
                    id: group.group_id.clone(),
                },
                model_ids: vec!["public".to_owned()],
                monthly_tokens: 2000,
                max_concurrent_requests: 1,
                expires_at_ms: None,
                allow_resource_sharing: true,
            },
            NOW,
        )
        .await
        .unwrap();
    principal.model = RunModelBinding::Platform {
        grant_id: grant.grant_id.clone(),
        model_id: "public".to_owned(),
        beneficiary_user_id: owner.user_id.clone(),
    };
    let accepted = reserve(store, &principal, "group-call", 250, NOW)
        .await
        .unwrap();
    begin(store, &principal, &accepted.request.request_id, 1, NOW)
        .await
        .unwrap();
    store
        .remove_model_group_member(owner, &group.group_id, &owner.user_id, NOW + 1)
        .await
        .unwrap();
    assert!(
        check(store, &principal, &accepted.request.request_id, NOW + 2)
            .await
            .is_err()
    );
    store
        .settle_model_attempt(
            &accepted.request.request_id,
            1,
            &settlement(ModelRequestState::Failed, Some(60)),
            NOW + 3,
        )
        .await
        .unwrap();
    assert!(
        begin(store, &principal, &accepted.request.request_id, 2, NOW + 4)
            .await
            .is_err()
    );
    store
        .finish_workload_model_request(
            &accepted.request.request_id,
            ModelRequestState::Failed,
            Some("access_revoked"),
            NOW + 5,
        )
        .await
        .unwrap();
    let grant = store
        .get_model_grant(owner, &grant.grant_id, NOW + 6)
        .await
        .unwrap();
    assert_eq!(
        (
            grant.quota.used_tokens,
            grant.quota.reserved_tokens,
            grant.quota.active_requests
        ),
        (60, 0, 0)
    );
}

async fn workload_ceiling_contract(
    store: &ControlStore,
    owner: &ControlUser,
    base: &WorkloadModelPrincipal,
) {
    let mut principal = isolated_principal(store, owner, base, "growing-run-budget", NOW).await;
    let accepted = reserve(store, &principal, "growing-budget-call", 400, NOW)
        .await
        .unwrap();
    begin(store, &principal, &accepted.request.request_id, 1, NOW)
        .await
        .unwrap();
    store
        .settle_model_attempt(
            &accepted.request.request_id,
            1,
            &settlement(ModelRequestState::Failed, Some(500)),
            NOW + 1,
        )
        .await
        .unwrap();
    assert!(
        begin(store, &principal, &accepted.request.request_id, 2, NOW + 2)
            .await
            .is_err(),
        "retry must account for already observed usage, not the original estimate"
    );
    let mut tx = store
        .database()
        .tenant_transaction(&principal.tenant_id)
        .await
        .unwrap();
    sqlx::query("UPDATE control_quota_reservations SET reserved_model_tokens=900 WHERE tenant_id=$1 AND reservation_id=$2").bind(principal.tenant_id.as_str()).bind(&principal.execution_reservation_id).execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
    assert!(
        check(store, &principal, &accepted.request.request_id, NOW + 3)
            .await
            .is_err(),
        "stale canonical ceilings cannot authorize new work"
    );
    principal.run_token_limit = 900;
    check(store, &principal, &accepted.request.request_id, NOW + 3)
        .await
        .unwrap();
    begin(store, &principal, &accepted.request.request_id, 2, NOW + 4)
        .await
        .unwrap();
    store
        .settle_model_attempt(
            &accepted.request.request_id,
            2,
            &settlement(ModelRequestState::Completed, Some(550)),
            NOW + 5,
        )
        .await
        .unwrap();
    assert!(
        store
            .reserve_quota(
                owner,
                &principal.tenant_id,
                None,
                1,
                Duration::from_hours(1),
                NOW + 6
            )
            .await
            .is_err(),
        "known upstream overage cannot hide behind the smaller active Run ceiling"
    );
    let completed = store
        .finish_workload_model_request(
            &accepted.request.request_id,
            ModelRequestState::Completed,
            None,
            NOW + 7,
        )
        .await
        .unwrap();
    assert_eq!(completed.accounted_tokens, Some(1050));
    let mut tx = store
        .database()
        .tenant_transaction(&principal.tenant_id)
        .await
        .unwrap();
    let summary = ControlStore::finalize_workload_reservation_in(
        &mut tx,
        &principal.tenant_id,
        &principal.execution_reservation_id,
        NOW + 8,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        (summary.used_model_tokens, summary.unknown_model_tokens),
        (1050, 0)
    );
}

#[expect(
    clippy::too_many_lines,
    reason = "Trace one shared call across the actor, model beneficiary and unrelated group member."
)]
async fn workload_visibility_contract(
    store: &ControlStore,
    owner: &ControlUser,
    actor: &ControlUser,
    base: &WorkloadModelPrincipal,
) {
    let now = at("2031-01-01T00:00:00Z");
    let mut principal =
        isolated_principal(store, owner, base, "shared-model-visibility", now).await;
    principal.actor_user_id = actor.user_id.clone();
    store
        .set_membership(
            owner,
            &principal.tenant_id,
            &actor.user_id,
            TenantRole::Member,
            now,
        )
        .await
        .unwrap();
    let unrelated = store
        .upsert_user(
            &OidcPrincipal {
                issuer: "https://identity.example".to_owned(),
                subject: "unrelated-model-group-member".to_owned(),
                email: None,
                display_name: None,
            },
            "test-unrelated-model-group-member",
            now,
        )
        .await
        .unwrap();
    let group = store
        .save_model_group(
            owner,
            None,
            &crate::GroupInput {
                name: "Shared model budget".to_owned(),
                description: None,
            },
            now,
        )
        .await
        .unwrap();
    for user in [&owner.user_id, &unrelated.user_id] {
        store
            .set_model_group_member(owner, &group.group_id, user, now)
            .await
            .unwrap();
    }
    let grant = store
        .save_model_grant(
            owner,
            None,
            &ModelGrantInput {
                name: "Visibility grant".to_owned(),
                subject: ModelGrantSubject::Group { id: group.group_id },
                model_ids: vec!["public".to_owned()],
                monthly_tokens: 2000,
                max_concurrent_requests: 1,
                expires_at_ms: None,
                allow_resource_sharing: true,
            },
            now,
        )
        .await
        .unwrap();
    principal.model = RunModelBinding::Platform {
        grant_id: grant.grant_id,
        model_id: "public".to_owned(),
        beneficiary_user_id: owner.user_id.clone(),
    };
    let accepted = reserve(store, &principal, "shared-visibility-request", 200, now)
        .await
        .unwrap();
    begin(store, &principal, &accepted.request.request_id, 1, now)
        .await
        .unwrap();
    store
        .settle_model_attempt(
            &accepted.request.request_id,
            1,
            &settlement(ModelRequestState::Completed, Some(35)),
            now + 1,
        )
        .await
        .unwrap();
    store
        .finish_workload_model_request(
            &accepted.request.request_id,
            ModelRequestState::Completed,
            None,
            now + 2,
        )
        .await
        .unwrap();
    for viewer in [owner, actor, &unrelated] {
        let requests = store
            .list_model_service_requests(
                viewer,
                Some(&viewer.user_id),
                &crate::PageQuery {
                    query: Some(accepted.request.request_id.clone()),
                    ..crate::PageQuery::default()
                },
            )
            .await
            .unwrap();
        let summary = store
            .model_service_usage(viewer, Some(&viewer.user_id), now + 3)
            .await
            .unwrap();
        if viewer.user_id == unrelated.user_id {
            assert!(
                requests.requests.is_empty(),
                "sharing a model budget does not expose other members' private requests"
            );
            assert_eq!((summary.request_count, summary.used_tokens), (0, 0));
        } else {
            assert_eq!(
                requests.requests.len(),
                1,
                "both the collaborator and model beneficiary can trace this request"
            );
            assert_eq!((summary.request_count, summary.used_tokens), (1, 35));
        }
    }
}

async fn workload_byok_readiness_contract(
    store: &ControlStore,
    owner: &ControlUser,
    actor: &ControlUser,
    principal: &WorkloadModelPrincipal,
) {
    let tenant = &principal.tenant_id;
    let binding = RunModelBinding::UserProvider {
        tenant_id: tenant.clone(),
        owner_user_id: owner.user_id.clone(),
        provider_id: "byok".to_owned(),
        model: "upstream-model".to_owned(),
    };
    let saved = store
        .resolve_workload_model_snapshot(
            &actor.user_id,
            &owner.user_id,
            tenant,
            &binding,
            None,
            NOW + 30,
        )
        .await
        .unwrap();
    store
        .delete_user_credential(owner, tenant, "BYOK_KEY")
        .await
        .unwrap();
    store
        .put_user_credential(
            actor,
            tenant,
            "BYOK_KEY",
            "collaborator-owned-key",
            NOW + 32,
        )
        .await
        .unwrap();
    assert!(
        store
            .resolve_workload_model_snapshot(
                &actor.user_id,
                &owner.user_id,
                tenant,
                &binding,
                None,
                NOW + 32
            )
            .await
            .is_err(),
        "a collaborator's credential with the same name cannot make the owner's missing model credential available"
    );
    assert!(
        store
            .put_user_credential(owner, tenant, "BYOK_KEY", "", NOW + 33)
            .await
            .is_err()
    );
    saved.validate().unwrap();
    assert_eq!(
        saved.binding, binding,
        "inspection keeps its immutable saved capability snapshot when live model access disappears"
    );
    let no_key = ProviderProfile {
        id: "byok".to_owned(),
        display_name: "No-key local model".to_owned(),
        base_url: "http://127.0.0.1:9999/v1".to_owned(),
        protocol: ProviderProtocol::OpenAiResponses,
        api_key_ref: None,
        defaults: saved.defaults.clone(),
        models: vec![ProviderModel {
            id: "upstream-model".to_owned(),
            display_name: None,
            settings: ProviderModelSettings::Inherit,
        }],
        timeout_ms: 1000,
        max_attempts: 3,
        retry_base_delay_ms: 1,
    };
    store
        .upsert_user_provider_profile(owner, tenant, no_key, NOW + 34)
        .await
        .unwrap();
    let available = store
        .resolve_workload_model_snapshot(
            &actor.user_id,
            &owner.user_id,
            tenant,
            &binding,
            None,
            NOW + 35,
        )
        .await
        .unwrap();
    assert_eq!(
        available.binding, binding,
        "a Provider that declares no API-key requirement remains valid"
    );
    let snapshot = serde_json::to_string(&available).unwrap();
    assert!(!snapshot.contains("collaborator-owned-key"));
}
