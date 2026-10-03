#[path = "reconciliation_tests.rs"]
mod reconciliation_tests;

use sqlx::Executor;
use ternilo_protocol::{
    ProviderModel, ProviderModelDefaults, ProviderModelSettings, ProviderProfile, ProviderProtocol,
};

use super::*;
use crate::{GroupInput, InstanceMode, NativeRegistration, OidcPrincipal, PageQuery, SecretCipher};

const NOW: u64 = 1_800_000_000_000;

fn provider() -> ModelProviderInput {
    ModelProviderInput {
        profile: ProviderProfile {
            hosted_tools: None,
            id: "upstream".to_owned(),
            display_name: "Internal upstream".to_owned(),
            base_url: "https://model.example/v1".to_owned(),
            protocol: ProviderProtocol::OpenAiResponses,
            api_key_ref: None,
            defaults: ProviderModelDefaults {
                context_window: 32_768,
                max_output_tokens: 4096,
                reasoning: None,
            },
            models: vec![ProviderModel {
                id: "private-model".to_owned(),
                display_name: None,
                settings: ProviderModelSettings::Inherit,
            }],
            timeout_ms: 1000,
            max_attempts: 1,
            retry_base_delay_ms: 250,
        },
        enabled: true,
        api_key: Some("upstream-private-key".to_owned()),
        clear_api_key: false,
    }
}

fn request(id: &str, tokens: u64) -> ModelRequestInput {
    ModelRequestInput {
        request_key: id.to_owned(),
        payload_hash: "a".repeat(64),
        model_id: "public-model".to_owned(),
        protocol: ProviderProtocol::OpenAiResponses,
        reserved_tokens: tokens,
    }
}

fn known(input: u64, output: u64) -> ModelRequestSettlement {
    ModelRequestSettlement {
        state: ModelRequestState::Completed,
        usage: Some(ServiceModelUsage {
            input_tokens: Some(input),
            output_tokens: Some(output),
            cached_input_tokens: Some(4),
            reasoning_tokens: Some(2),
            ..ServiceModelUsage::default()
        }),
        upstream_request_id: Some("upstream-request".to_owned()),
        error_code: None,
    }
}

async fn user(store: &ControlStore, name: &str) -> crate::ControlUser {
    store
        .upsert_user(
            &OidcPrincipal {
                issuer: "https://model-id.example".to_owned(),
                subject: name.to_owned(),
                email: Some(format!("{name}@example.test")),
                display_name: Some(name.to_owned()),
            },
            &format!("test-{name}"),
            NOW,
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn sqlite_model_service_keeps_authorization_budget_and_usage_independent_of_workers() {
    let directory = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}",
        directory.path().join("models.sqlite3").display()
    );
    let store = ControlStore::connect(&url, None, SecretCipher::from_key([19; 32]), 8)
        .await
        .unwrap();
    Box::pin(model_contract(&store, &url)).await;
}

#[tokio::test]
#[ignore = "requires TERNILO_TEST_DATABASE_URL pointing to a disposable PostgreSQL database"]
async fn postgres_model_service_enforces_the_same_contract_with_runtime_rls() {
    let url = std::env::var("TERNILO_TEST_DATABASE_URL").unwrap();
    assert!(url.contains("ternilo_control_test"));
    let admin = sqlx::PgPool::connect(&url).await.unwrap();
    admin
        .execute("DROP SCHEMA IF EXISTS public CASCADE")
        .await
        .unwrap();
    admin.execute("CREATE SCHEMA public").await.unwrap();
    crate::postgres_test::prepare_role(&admin, "ternilo_models_test", "models-test-password").await;
    let owner = ControlStore::connect(&url, None, SecretCipher::from_key([19; 32]), 1)
        .await
        .unwrap();
    owner.database().close().await;
    let mut runtime = url
        .parse::<sqlx::any::AnyConnectOptions>()
        .unwrap()
        .database_url;
    runtime.set_username("ternilo_models_test").unwrap();
    runtime.set_password(Some("models-test-password")).unwrap();
    let store = ControlStore::connect(
        runtime.as_str(),
        Some(&url),
        SecretCipher::from_key([19; 32]),
        8,
    )
    .await
    .unwrap();
    Box::pin(model_contract(&store, &url)).await;
    let visible: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM control_model_providers")
        .fetch_one(store.database.pool())
        .await
        .unwrap();
    assert_eq!(
        visible, 0,
        "unscoped runtime queries must not expose platform upstreams"
    );
    let requests: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM control_model_requests")
        .fetch_one(store.database.pool())
        .await
        .unwrap();
    assert_eq!(
        requests, 0,
        "unscoped runtime queries must not expose account model usage"
    );
    admin.close().await;
}

#[expect(
    clippy::too_many_lines,
    reason = "Exercise the same end-to-end independent model service contract against SQLite and restricted PostgreSQL."
)]
async fn model_contract(store: &ControlStore, owner_url: &str) {
    let owner = store
        .initialize_owner(
            &NativeRegistration {
                email: "model-owner@example.test".to_owned(),
                username: "model-owner".to_owned(),
                password: "model-owner-test-password".to_owned(),
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
    let alice = user(store, "alice").await;
    let bob = user(store, "bob").await;
    let mut invalid_retries = provider();
    invalid_retries.profile.max_attempts = 9;
    assert!(
        store
            .save_model_provider(&owner, &invalid_retries, NOW)
            .await
            .is_err()
    );
    let record = store
        .save_model_provider(&owner, &provider(), NOW)
        .await
        .unwrap();
    assert!(record.has_api_key);
    let serialized = serde_json::to_string(&record).unwrap();
    assert!(!serialized.contains("upstream-private-key"));
    assert!(store.get_model_provider(&alice, "upstream").await.is_err());
    store
        .save_model_publication(
            &owner,
            &ModelPublicationInput {
                model_id: "public-model".to_owned(),
                display_name: "Public model".to_owned(),
                provider_id: "upstream".to_owned(),
                upstream_model: "private-model".to_owned(),
                enabled: true,
            },
            NOW,
        )
        .await
        .unwrap();
    let group = store
        .save_model_group(
            &owner,
            None,
            &GroupInput {
                name: "Model readers".to_owned(),
                description: None,
            },
            NOW,
        )
        .await
        .unwrap();
    store
        .set_model_group_member(&owner, &group.group_id, &alice.user_id, NOW)
        .await
        .unwrap();
    assert_eq!(
        store.list_tenants(&alice).await.unwrap().len(),
        1,
        "model groups do not manufacture team memberships"
    );
    assert_eq!(
        store
            .list_model_group_members(&owner, &group.group_id, &PageQuery::default())
            .await
            .unwrap()
            .users
            .len(),
        1
    );
    let mut grant_input = ModelGrantInput {
        allow_resource_sharing: true,
        name: "Shared model budget".to_owned(),
        subject: ModelGrantSubject::Group {
            id: group.group_id.clone(),
        },
        model_ids: vec!["public-model".to_owned()],
        monthly_tokens: 1000,
        max_concurrent_requests: 1,
        expires_at_ms: None,
    };
    let grant = store
        .save_model_grant(&owner, None, &grant_input, NOW)
        .await
        .unwrap();
    let key_input = ModelKeyInput {
        name: "My laptop".to_owned(),
        grant_id: grant.grant_id.clone(),
        model_ids: vec!["public-model".to_owned()],
        monthly_tokens: Some(800),
        max_concurrent_requests: Some(1),
        expires_at_ms: None,
    };
    let key = store
        .create_model_key(&alice, &key_input, NOW)
        .await
        .unwrap();
    assert_eq!(key.key.grant_name, grant.name);
    assert!(store.create_model_key(&bob, &key_input, NOW).await.is_err());
    assert_eq!(
        store
            .list_model_keys(&bob, &PageQuery::default())
            .await
            .unwrap()
            .keys
            .len(),
        0
    );
    assert!(
        store
            .revoke_model_key(&bob, &key.key.key_id, NOW)
            .await
            .is_err()
    );
    assert_eq!(
        store
            .authenticate_model_key("browser-or-node-token", NOW)
            .await
            .unwrap_err()
            .kind,
        ModelAccessErrorKind::Unauthorized
    );
    let catalog = store.list_key_models(&key.token, NOW).await.unwrap();
    assert_eq!(catalog.len(), 1);
    assert_eq!(catalog[0].model_id, "public-model");
    assert!(
        !serde_json::to_string(&catalog)
            .unwrap()
            .contains("private-model")
    );
    let mut wrong_protocol = request("wrong-protocol", 50);
    wrong_protocol.protocol = ProviderProtocol::OpenAiChatCompletions;
    assert!(
        store
            .reserve_model_request(&key.token, &wrong_protocol, NOW)
            .await
            .is_err()
    );
    assert_eq!(
        store
            .model_service_usage(&alice, Some(&alice.user_id), NOW)
            .await
            .unwrap()
            .request_count,
        0
    );
    let one = request("first", 200);
    let two = request("racing", 200);
    let (first, second) = tokio::join!(
        store.reserve_model_request(&key.token, &one, NOW),
        store.reserve_model_request(&key.token, &two, NOW)
    );
    let ((Ok(accepted), Err(rejected)) | (Err(rejected), Ok(accepted))) = (first, second) else {
        panic!("concurrency must atomically accept exactly one request");
    };
    assert_eq!(rejected.kind, ModelAccessErrorKind::QuotaExceeded);
    store
        .set_model_group_member(&owner, &group.group_id, &bob.user_id, NOW)
        .await
        .unwrap();
    let bob_key = store.create_model_key(&bob, &key_input, NOW).await.unwrap();
    assert_eq!(
        store
            .reserve_model_request(&bob_key.token, &request("group-concurrency", 1), NOW)
            .await
            .err()
            .unwrap()
            .kind,
        ModelAccessErrorKind::QuotaExceeded,
        "different group members share the grant concurrency budget"
    );
    assert_eq!(
        accepted.route.api_key.as_deref().map(String::as_str),
        Some("upstream-private-key")
    );
    let duplicate_input = if accepted.request.request_id.is_empty() {
        unreachable!()
    } else {
        let mut tx = store.model_transaction().await.unwrap();
        let id: String = sqlx::query_scalar(
            "SELECT request_key FROM control_model_requests WHERE request_id=$1",
        )
        .bind(&accepted.request.request_id)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        tx.commit().await.unwrap();
        request(&id, 200)
    };
    let duplicate = store
        .reserve_model_request(&key.token, &duplicate_input, NOW)
        .await
        .unwrap();
    assert!(!duplicate.newly_accepted);
    assert_eq!(duplicate.request.request_id, accepted.request.request_id);
    let mut changed = duplicate_input.clone();
    changed.payload_hash = "b".repeat(64);
    assert_eq!(
        store
            .reserve_model_request(&key.token, &changed, NOW)
            .await
            .err()
            .unwrap()
            .kind,
        ModelAccessErrorKind::Conflict
    );
    store
        .mark_model_request_attempted(&accepted.request.request_id, NOW)
        .await
        .unwrap();
    assert!(
        store
            .mark_model_request_attempted(&accepted.request.request_id, NOW)
            .await
            .is_err()
    );
    reconciliation_tests::reject_pending(store, &owner, &accepted.request.request_id).await;
    let settled = store
        .settle_model_request(&accepted.request.request_id, &known(20, 10), NOW + 1)
        .await
        .unwrap();
    assert_eq!(settled.accounted_tokens, Some(30));
    store
        .settle_model_request(&accepted.request.request_id, &known(20, 10), NOW + 2)
        .await
        .unwrap();
    assert!(
        store
            .settle_model_request(&accepted.request.request_id, &known(21, 10), NOW + 2)
            .await
            .is_err()
    );
    let unknown = store
        .reserve_model_request(&key.token, &request("unknown", 200), NOW + 3)
        .await
        .unwrap();
    store
        .mark_model_request_attempted(&unknown.request.request_id, NOW + 3)
        .await
        .unwrap();
    store
        .remove_model_group_member(&owner, &group.group_id, &alice.user_id, NOW + 4)
        .await
        .unwrap();
    assert!(
        store
            .check_model_request_authorized(&unknown.request.request_id, NOW + 4)
            .await
            .is_err()
    );
    assert!(
        store
            .reserve_model_request(&key.token, &request("revoked-group", 10), NOW + 4)
            .await
            .is_err()
    );
    let unknown_settlement = ModelRequestSettlement {
        state: ModelRequestState::Cancelled,
        usage: None,
        upstream_request_id: None,
        error_code: Some("access_revoked".to_owned()),
    };
    store
        .settle_model_request(&unknown.request.request_id, &unknown_settlement, NOW + 5)
        .await
        .unwrap();
    let quota = store
        .get_model_grant(&owner, &grant.grant_id, NOW + 5)
        .await
        .unwrap()
        .quota;
    assert_eq!(
        (
            quota.used_tokens,
            quota.reserved_tokens,
            quota.active_requests
        ),
        (30, 200, 0)
    );
    store
        .set_model_group_member(&owner, &group.group_id, &alice.user_id, NOW + 6)
        .await
        .unwrap();
    assert_eq!(
        store
            .reserve_model_request(&key.token, &request("key-budget", 600), NOW + 6)
            .await
            .err()
            .unwrap()
            .kind,
        ModelAccessErrorKind::QuotaExceeded
    );
    let preflight = store
        .reserve_model_request(&key.token, &request("preflight", 200), NOW + 7)
        .await
        .unwrap();
    store
        .settle_model_request(
            &preflight.request.request_id,
            &ModelRequestSettlement {
                state: ModelRequestState::Failed,
                usage: None,
                upstream_request_id: None,
                error_code: Some("validation".to_owned()),
            },
            NOW + 8,
        )
        .await
        .unwrap();
    assert_eq!(
        store
            .get_model_grant(&owner, &grant.grant_id, NOW + 8)
            .await
            .unwrap()
            .quota
            .reserved_tokens,
        200
    );
    let expired = store
        .reserve_model_request(&key.token, &request("expired", 100), NOW + 9)
        .await
        .unwrap();
    store
        .mark_model_request_attempted(&expired.request.request_id, NOW + 9)
        .await
        .unwrap();
    assert_eq!(
        store
            .expire_model_requests(expired.request.expires_at_ms, 100)
            .await
            .unwrap(),
        1
    );
    let quota = store
        .get_model_grant(&owner, &grant.grant_id, expired.request.expires_at_ms)
        .await
        .unwrap()
        .quota;
    assert_eq!((quota.reserved_tokens, quota.active_requests), (300, 0));
    store
        .settle_model_request(
            &expired.request.request_id,
            &known(10, 5),
            expired.request.expires_at_ms + 1,
        )
        .await
        .unwrap();
    assert_eq!(
        store
            .get_model_grant(&owner, &grant.grant_id, NOW + 10)
            .await
            .unwrap()
            .quota
            .used_tokens,
        45
    );
    grant_input.monthly_tokens = 244;
    store
        .save_model_grant(&owner, Some(&grant.grant_id), &grant_input, NOW + 11)
        .await
        .unwrap();
    assert_eq!(
        store
            .reserve_model_request(&key.token, &request("grant-budget", 1), NOW + 11)
            .await
            .err()
            .unwrap()
            .kind,
        ModelAccessErrorKind::QuotaExceeded
    );
    grant_input.monthly_tokens = 1000;
    store
        .save_model_grant(&owner, Some(&grant.grant_id), &grant_input, NOW + 12)
        .await
        .unwrap();
    store
        .set_instance_mode(&owner, InstanceMode::SingleUser, 2, NOW + 13)
        .await
        .unwrap();
    assert!(store.list_key_models(&key.token, NOW + 13).await.is_err());
    store
        .set_instance_mode(&owner, InstanceMode::MultiUser, 3, NOW + 14)
        .await
        .unwrap();
    let revoked = store
        .reserve_model_request(&key.token, &request("revoked-key", 50), NOW + 15)
        .await
        .unwrap();
    store
        .mark_model_request_attempted(&revoked.request.request_id, NOW + 15)
        .await
        .unwrap();
    store
        .revoke_model_key(&alice, &key.key.key_id, NOW + 16)
        .await
        .unwrap();
    assert_eq!(
        store
            .authenticate_model_key(&key.token, NOW + 16)
            .await
            .unwrap_err()
            .kind,
        ModelAccessErrorKind::Unauthorized
    );
    store
        .settle_model_request(&revoked.request.request_id, &known(5, 5), NOW + 17)
        .await
        .unwrap();
    let next = store
        .create_model_key(&alice, &key_input, NOW + 18)
        .await
        .unwrap();
    let active = store
        .reserve_model_request(&next.token, &request("grant-revoked", 50), NOW + 18)
        .await
        .unwrap();
    store
        .mark_model_request_attempted(&active.request.request_id, NOW + 18)
        .await
        .unwrap();
    store
        .revoke_model_grant(&owner, &grant.grant_id, NOW + 19)
        .await
        .unwrap();
    assert!(store.list_key_models(&next.token, NOW + 19).await.is_err());
    store
        .settle_model_request(&active.request.request_id, &known(6, 4), NOW + 20)
        .await
        .unwrap();
    store
        .delete_model_group(&owner, &group.group_id, NOW + 21)
        .await
        .unwrap();
    assert!(
        store
            .get_model_group(&owner, &group.group_id)
            .await
            .is_err()
    );
    let report = store
        .model_service_usage(&alice, Some(&alice.user_id), NOW + 22)
        .await
        .unwrap();
    assert_eq!(
        (
            report.used_tokens,
            report.reserved_tokens,
            report.unknown_requests
        ),
        (65, 200, 1)
    );
    assert_eq!(report.input_tokens, 41);
    assert!(
        store
            .list_model_service_requests(&bob, Some(&alice.user_id), &PageQuery::default())
            .await
            .is_err()
    );
    assert_eq!(
        store
            .list_model_service_requests(&bob, Some(&bob.user_id), &PageQuery::default())
            .await
            .unwrap()
            .requests
            .len(),
        0
    );
    assert_eq!(
        store
            .list_model_service_requests(&owner, None, &PageQuery::default())
            .await
            .unwrap()
            .requests
            .len(),
        6
    );
    let page = store
        .list_model_service_requests(
            &alice,
            Some(&alice.user_id),
            &PageQuery {
                limit: 2,
                ..PageQuery::default()
            },
        )
        .await
        .unwrap();
    assert!(page.requests[0].created_at_ms >= page.requests[1].created_at_ms);
    let next_page = store
        .list_model_service_requests(
            &alice,
            Some(&alice.user_id),
            &PageQuery {
                limit: 2,
                cursor: page.next_cursor,
                ..PageQuery::default()
            },
        )
        .await
        .unwrap();
    assert!(page.requests[1].created_at_ms >= next_page.requests[0].created_at_ms);
    assert!(!page.requests.iter().any(|request| {
        next_page
            .requests
            .iter()
            .any(|next| next.request_id == request.request_id)
    }));
    reconciliation_tests::verify(
        store,
        &owner,
        &alice,
        &bob,
        &unknown.request.request_id,
        &accepted.request.request_id,
        &grant.grant_id,
    )
    .await;
    assert!(
        store
            .begin_model_provider_key_rotation(&alice, "upstream", "not-authorized", NOW + 100)
            .await
            .is_err()
    );
    let rotation = store
        .begin_model_provider_key_rotation(&owner, "upstream", "canary-next-key", NOW + 101)
        .await
        .unwrap();
    let status = store
        .model_provider_key_rotation(&owner, "upstream")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status.rotation_id, rotation.rotation_id);
    let exposed = serde_json::to_string(&status).unwrap();
    assert!(!exposed.contains("canary-next-key") && !exposed.contains("upstream-private-key"));
    assert!(
        store
            .begin_model_provider_key_rotation(&owner, "upstream", "overlapping-key", NOW + 102)
            .await
            .is_err()
    );
    assert!(
        store
            .save_model_provider(&owner, &provider(), NOW + 103)
            .await
            .is_err()
    );
    assert!(
        store
            .finish_model_provider_key_rotation(
                &owner,
                "upstream",
                "stale-rotation",
                true,
                NOW + 104
            )
            .await
            .is_err()
    );
    store
        .finish_model_provider_key_rotation(
            &owner,
            "upstream",
            &rotation.rotation_id,
            true,
            NOW + 105,
        )
        .await
        .unwrap();
    assert_eq!(
        store
            .resolve_model_provider_secret(&owner, "upstream")
            .await
            .unwrap()
            .as_deref()
            .map(String::as_str),
        Some("upstream-private-key")
    );
    let pending = store
        .begin_model_provider_key_rotation(&owner, "upstream", "canary-next-key", NOW + 106)
        .await
        .unwrap();
    assert_eq!(
        ControlStore::rotate_secret_master_key(
            owner_url,
            &SecretCipher::from_key([19; 32]),
            &SecretCipher::from_key([23; 32])
        )
        .await
        .unwrap(),
        2
    );
    assert!(
        store
            .resolve_model_provider_secret(&owner, "upstream")
            .await
            .is_err()
    );
    let rotated = ControlStore::connect(owner_url, None, SecretCipher::from_key([23; 32]), 1)
        .await
        .unwrap();
    assert_eq!(
        rotated
            .resolve_model_provider_secret(&owner, "upstream")
            .await
            .unwrap()
            .as_deref()
            .map(String::as_str),
        Some("canary-next-key")
    );
    rotated
        .finish_model_provider_key_rotation(
            &owner,
            "upstream",
            &pending.rotation_id,
            true,
            NOW + 107,
        )
        .await
        .unwrap();
    assert_eq!(
        rotated
            .resolve_model_provider_secret(&owner, "upstream")
            .await
            .unwrap()
            .as_deref()
            .map(String::as_str),
        Some("upstream-private-key")
    );
    let committed = rotated
        .begin_model_provider_key_rotation(&owner, "upstream", "committed-key", NOW + 108)
        .await
        .unwrap();
    rotated
        .finish_model_provider_key_rotation(
            &owner,
            "upstream",
            &committed.rotation_id,
            false,
            NOW + 109,
        )
        .await
        .unwrap();
    assert!(
        rotated
            .model_provider_key_rotation(&owner, "upstream")
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        rotated
            .resolve_model_provider_secret(&owner, "upstream")
            .await
            .unwrap()
            .as_deref()
            .map(String::as_str),
        Some("committed-key")
    );
    assert!(
        rotated
            .finish_model_provider_key_rotation(
                &owner,
                "upstream",
                &committed.rotation_id,
                true,
                NOW + 110
            )
            .await
            .is_err()
    );
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "Check the directory, multiple budget sources, and immutable upstream identity through real store operations."
)]
async fn model_catalog_pages_and_keys_keep_separate_budget_sources() {
    let store = ControlStore::connect("sqlite::memory:", None, SecretCipher::from_key([19; 32]), 1)
        .await
        .unwrap();
    let owner = store
        .initialize_owner(
            &NativeRegistration {
                email: "page-owner@example.test".to_owned(),
                username: "page-owner".to_owned(),
                password: "page-owner-test-password".to_owned(),
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
    let alice = user(&store, "paged-alice").await;
    let mut groups = Vec::new();
    for index in 0..27 {
        groups.push(
            store
                .save_model_group(
                    &owner,
                    None,
                    &GroupInput {
                        name: format!("Group {index:02}"),
                        description: None,
                    },
                    NOW,
                )
                .await
                .unwrap(),
        );
    }
    let first = store
        .list_model_groups(&owner, &PageQuery::default())
        .await
        .unwrap();
    assert_eq!(first.groups.len(), 25);
    let second = store
        .list_model_groups(
            &owner,
            &PageQuery {
                cursor: first.next_cursor.clone(),
                ..PageQuery::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(second.groups.len(), 2);
    assert!(second.next_cursor.is_none());
    let unique = first
        .groups
        .iter()
        .chain(&second.groups)
        .map(|group| &group.group_id)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(unique.len(), 27);
    assert_eq!(
        store
            .list_model_groups(
                &owner,
                &PageQuery {
                    query: Some("Group 26".to_owned()),
                    ..PageQuery::default()
                }
            )
            .await
            .unwrap()
            .groups
            .len(),
        1
    );
    assert!(
        store
            .list_model_groups(&alice, &PageQuery::default())
            .await
            .is_err()
    );
    let group = &groups[0];
    let operator = user(&store, "model-operator").await;
    store
        .set_account_role(
            &owner,
            &operator.user_id,
            crate::PlatformRole::Operator,
            1,
            NOW,
        )
        .await
        .unwrap();
    assert!(
        store
            .list_model_group_candidates(&operator, &group.group_id, &PageQuery::default())
            .await
            .is_err(),
        "model operations must not expose the platform account directory through group candidates"
    );
    assert!(
        store
            .list_model_group_members(&operator, &group.group_id, &PageQuery::default())
            .await
            .is_ok()
    );
    for index in 0..26 {
        let member = user(&store, &format!("candidate-{index:02}")).await;
        store
            .set_model_group_member(&owner, &group.group_id, &member.user_id, NOW)
            .await
            .unwrap();
    }
    let first = store
        .list_model_group_members(&owner, &group.group_id, &PageQuery::default())
        .await
        .unwrap();
    let second = store
        .list_model_group_members(
            &owner,
            &group.group_id,
            &PageQuery {
                cursor: first.next_cursor,
                ..PageQuery::default()
            },
        )
        .await
        .unwrap();
    assert_eq!((first.users.len(), second.users.len()), (25, 1));
    let candidate = store
        .list_model_group_candidates(
            &owner,
            &group.group_id,
            &PageQuery {
                query: Some("paged-alice".to_owned()),
                ..PageQuery::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(candidate.users[0].user_id, alice.user_id);
    store
        .save_model_provider(&owner, &provider(), NOW)
        .await
        .unwrap();
    let mut publication = ModelPublicationInput {
        model_id: "public-model".to_owned(),
        display_name: "Public model".to_owned(),
        provider_id: "upstream".to_owned(),
        upstream_model: "private-model".to_owned(),
        enabled: true,
    };
    store
        .save_model_publication(&owner, &publication, NOW)
        .await
        .unwrap();
    let mut input = ModelGrantInput {
        allow_resource_sharing: true,
        name: "Small budget".to_owned(),
        subject: ModelGrantSubject::User {
            id: alice.user_id.to_string(),
        },
        model_ids: vec!["public-model".to_owned()],
        monthly_tokens: 50,
        max_concurrent_requests: 1,
        expires_at_ms: None,
    };
    let small = store
        .save_model_grant(&owner, None, &input, NOW)
        .await
        .unwrap();
    input.name = "Separate large budget".to_owned();
    input.monthly_tokens = 1000;
    let large = store
        .save_model_grant(&owner, None, &input, NOW)
        .await
        .unwrap();
    let mut key_input = ModelKeyInput {
        name: "Small source".to_owned(),
        grant_id: small.grant_id.clone(),
        model_ids: vec!["public-model".to_owned()],
        monthly_tokens: None,
        max_concurrent_requests: None,
        expires_at_ms: None,
    };
    let small_key = store
        .create_model_key(&alice, &key_input, NOW)
        .await
        .unwrap();
    key_input.name = "Large source".to_owned();
    key_input.grant_id = large.grant_id.clone();
    let large_key = store
        .create_model_key(&alice, &key_input, NOW)
        .await
        .unwrap();
    assert_eq!(
        store
            .reserve_model_request(&small_key.token, &request("no-fallback", 51), NOW)
            .await
            .err()
            .unwrap()
            .kind,
        ModelAccessErrorKind::QuotaExceeded
    );
    let accepted = store
        .reserve_model_request(&large_key.token, &request("large-source", 51), NOW)
        .await
        .unwrap();
    assert_eq!(
        accepted.request.grant_id.as_deref(),
        Some(large.grant_id.as_str())
    );
    let active = store
        .model_service_usage(&alice, Some(&alice.user_id), NOW)
        .await
        .unwrap();
    assert_eq!((active.active_requests, active.unknown_requests), (1, 0));
    store
        .mark_model_request_attempted(&accepted.request.request_id, NOW)
        .await
        .unwrap();
    let mut replacement = provider();
    replacement.profile.id = "replacement".to_owned();
    replacement.api_key = Some("replacement-private-key".to_owned());
    store
        .save_model_provider(&owner, &replacement, NOW + 1)
        .await
        .unwrap();
    publication.provider_id = "replacement".to_owned();
    store
        .save_model_publication(&owner, &publication, NOW + 2)
        .await
        .unwrap();
    store
        .disable_model_provider(&owner, "upstream", NOW + 3)
        .await
        .unwrap();
    assert!(
        store
            .check_model_request_authorized(&accepted.request.request_id, NOW + 3)
            .await
            .is_err(),
        "publication changes must not hide disablement of an accepted request's original upstream"
    );
    store
        .settle_model_request(&accepted.request.request_id, &known(10, 5), NOW + 4)
        .await
        .unwrap();
    let requests = store
        .list_model_service_requests(&alice, Some(&alice.user_id), &PageQuery::default())
        .await
        .unwrap();
    assert_eq!(requests.requests[0].provider_id, "upstream");
    assert_eq!(
        store
            .list_model_entitlements(&alice, &PageQuery::default(), NOW + 4)
            .await
            .unwrap()
            .entitlements
            .len(),
        2
    );
    let next = store
        .reserve_model_request(&large_key.token, &request("new-source", 51), NOW + 5)
        .await
        .unwrap();
    assert_eq!(next.route.provider.id, "replacement");
    assert_eq!(
        next.route.api_key.as_deref().map(String::as_str),
        Some("replacement-private-key")
    );
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "Verify one accepted request across the month boundary with real authorization, limits, and settlement."
)]
async fn model_concurrency_crosses_months_and_usage_stays_in_the_acceptance_month() {
    use chrono::TimeZone as _;
    let before = u64::try_from(
        Utc.with_ymd_and_hms(2027, 1, 31, 23, 59, 59)
            .unwrap()
            .timestamp_millis(),
    )
    .unwrap();
    let after = before + 2000;
    let store = ControlStore::connect("sqlite::memory:", None, SecretCipher::from_key([19; 32]), 1)
        .await
        .unwrap();
    let owner = store
        .initialize_owner(
            &NativeRegistration {
                email: "month-owner@example.test".to_owned(),
                username: "month-owner".to_owned(),
                password: "month-owner-test-password".to_owned(),
            },
            before,
        )
        .await
        .unwrap()
        .session
        .user;
    store
        .save_model_provider(&owner, &provider(), before)
        .await
        .unwrap();
    store
        .save_model_publication(
            &owner,
            &ModelPublicationInput {
                model_id: "public-model".to_owned(),
                display_name: "Public".to_owned(),
                provider_id: "upstream".to_owned(),
                upstream_model: "private-model".to_owned(),
                enabled: true,
            },
            before,
        )
        .await
        .unwrap();
    let grant = store
        .save_model_grant(
            &owner,
            None,
            &ModelGrantInput {
                allow_resource_sharing: true,
                name: "Month budget".to_owned(),
                subject: ModelGrantSubject::User {
                    id: owner.user_id.to_string(),
                },
                model_ids: vec!["public-model".to_owned()],
                monthly_tokens: 200,
                max_concurrent_requests: 1,
                expires_at_ms: None,
            },
            before,
        )
        .await
        .unwrap();
    let key = store
        .create_model_key(
            &owner,
            &ModelKeyInput {
                name: "Month key".to_owned(),
                grant_id: grant.grant_id.clone(),
                model_ids: vec!["public-model".to_owned()],
                monthly_tokens: None,
                max_concurrent_requests: None,
                expires_at_ms: None,
            },
            before,
        )
        .await
        .unwrap();
    let old = store
        .reserve_model_request(&key.token, &request("old-month", 200), before)
        .await
        .unwrap();
    store
        .mark_model_request_attempted(&old.request.request_id, before)
        .await
        .unwrap();
    assert_eq!(
        store
            .reserve_model_request(&key.token, &request("next-month", 200), after)
            .await
            .err()
            .unwrap()
            .kind,
        ModelAccessErrorKind::QuotaExceeded
    );
    store
        .settle_model_request(&old.request.request_id, &known(100, 50), after)
        .await
        .unwrap();
    assert_eq!(
        store
            .model_service_usage(&owner, Some(&owner.user_id), before)
            .await
            .unwrap()
            .used_tokens,
        150
    );
    assert_eq!(
        store
            .model_service_usage(&owner, Some(&owner.user_id), after)
            .await
            .unwrap()
            .used_tokens,
        0
    );
    store
        .reserve_model_request(&key.token, &request("next-month", 200), after)
        .await
        .unwrap();
    let quota = store
        .get_model_grant(&owner, &grant.grant_id, after)
        .await
        .unwrap()
        .quota;
    assert_eq!((quota.used_tokens, quota.reserved_tokens), (0, 200));
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "Exercise dynamic model intersections, separate key/grant expiration, and partially known usage."
)]
async fn model_keys_intersect_current_grants_and_preserve_partial_usage() {
    let store = ControlStore::connect("sqlite::memory:", None, SecretCipher::from_key([19; 32]), 1)
        .await
        .unwrap();
    let owner = store
        .initialize_owner(
            &NativeRegistration {
                email: "scope-owner@example.test".to_owned(),
                username: "scope-owner".to_owned(),
                password: "scope-owner-test-password".to_owned(),
            },
            NOW,
        )
        .await
        .unwrap()
        .session
        .user;
    store
        .save_model_provider(&owner, &provider(), NOW)
        .await
        .unwrap();
    for model_id in ["public-model", "second-model"] {
        store
            .save_model_publication(
                &owner,
                &ModelPublicationInput {
                    model_id: model_id.to_owned(),
                    display_name: model_id.to_owned(),
                    provider_id: "upstream".to_owned(),
                    upstream_model: "private-model".to_owned(),
                    enabled: true,
                },
                NOW,
            )
            .await
            .unwrap();
    }
    let mut input = ModelGrantInput {
        allow_resource_sharing: true,
        name: "Scope".to_owned(),
        subject: ModelGrantSubject::User {
            id: owner.user_id.to_string(),
        },
        model_ids: vec!["public-model".to_owned(), "second-model".to_owned()],
        monthly_tokens: 1000,
        max_concurrent_requests: 2,
        expires_at_ms: None,
    };
    let grant = store
        .save_model_grant(&owner, None, &input, NOW)
        .await
        .unwrap();
    let key_input = ModelKeyInput {
        name: "Scoped".to_owned(),
        grant_id: grant.grant_id.clone(),
        model_ids: vec!["public-model".to_owned()],
        monthly_tokens: None,
        max_concurrent_requests: None,
        expires_at_ms: None,
    };
    let key = store
        .create_model_key(&owner, &key_input, NOW)
        .await
        .unwrap();
    let mut denied = request("outside-key", 10);
    denied.model_id = "second-model".to_owned();
    assert_eq!(
        store
            .reserve_model_request(&key.token, &denied, NOW)
            .await
            .err()
            .unwrap()
            .kind,
        ModelAccessErrorKind::Forbidden
    );
    input.model_ids = vec!["second-model".to_owned()];
    store
        .save_model_grant(&owner, Some(&grant.grant_id), &input, NOW + 1)
        .await
        .unwrap();
    assert!(
        store
            .list_key_models(&key.token, NOW + 1)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        store
            .reserve_model_request(&key.token, &request("removed-model", 10), NOW + 1)
            .await
            .err()
            .unwrap()
            .kind,
        ModelAccessErrorKind::Forbidden
    );
    input.model_ids.push("public-model".to_owned());
    store
        .save_model_grant(&owner, Some(&grant.grant_id), &input, NOW + 2)
        .await
        .unwrap();
    let accepted = store
        .reserve_model_request(&key.token, &request("partial", 50), NOW + 2)
        .await
        .unwrap();
    store
        .mark_model_request_attempted(&accepted.request.request_id, NOW + 2)
        .await
        .unwrap();
    let partial = ServiceModelUsage {
        input_tokens: Some(12),
        cached_input_tokens: Some(3),
        raw_usage: Some(
            serde_json::json!({"input_tokens":12,"input_tokens_details":{"cached_tokens":3}}),
        ),
        ..ServiceModelUsage::default()
    };
    let settled = store
        .settle_model_request(
            &accepted.request.request_id,
            &ModelRequestSettlement {
                state: ModelRequestState::Failed,
                usage: Some(partial.clone()),
                upstream_request_id: None,
                error_code: Some("incomplete_stream".to_owned()),
            },
            NOW + 3,
        )
        .await
        .unwrap();
    assert_eq!(settled.accounted_tokens, None);
    assert_eq!(settled.usage, Some(partial));
    let report = store
        .model_service_usage(&owner, Some(&owner.user_id), NOW + 3)
        .await
        .unwrap();
    assert_eq!(
        (
            report.unknown_requests,
            report.reserved_tokens,
            report.input_tokens,
            report.cached_input_tokens
        ),
        (1, 50, 12, 3)
    );
    let expiring = store
        .create_model_key(
            &owner,
            &ModelKeyInput {
                expires_at_ms: Some(NOW + 4),
                ..key_input
            },
            NOW + 3,
        )
        .await
        .unwrap();
    assert_eq!(
        store
            .authenticate_model_key(&expiring.token, NOW + 4)
            .await
            .unwrap_err()
            .kind,
        ModelAccessErrorKind::Unauthorized
    );
    input.expires_at_ms = Some(NOW + 5);
    store
        .save_model_grant(&owner, Some(&grant.grant_id), &input, NOW + 4)
        .await
        .unwrap();
    assert_eq!(
        store
            .authenticate_model_key(&key.token, NOW + 5)
            .await
            .unwrap_err()
            .kind,
        ModelAccessErrorKind::Forbidden
    );
    let mut tx = store.model_transaction().await.unwrap();
    let stored: String =
        sqlx::query_scalar("SELECT token_hash FROM control_model_keys WHERE key_id=$1")
            .bind(&key.key.key_id)
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    assert_eq!(stored.len(), 64);
    assert_ne!(stored, key.token);
    tx.commit().await.unwrap();
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "Exercise admission, actual settlement and disabling hosted tools in one budget lifecycle."
)]
async fn hosted_tools_reserve_provider_work_before_admission_and_settle_actual_usage() {
    let store = ControlStore::connect("sqlite::memory:", None, SecretCipher::from_key([82; 32]), 1)
        .await
        .unwrap();
    let owner = store
        .initialize_owner(
            &NativeRegistration {
                username: "hosted-owner".to_owned(),
                email: "hosted@example.test".to_owned(),
                password: "hosted-fixture-password".to_owned(),
            },
            NOW,
        )
        .await
        .unwrap()
        .session
        .user;
    let mut upstream = provider();
    upstream.profile.protocol = ProviderProtocol::AnthropicMessages;
    upstream.profile.hosted_tools = Some(ternilo_protocol::HostedWebTools {
        web_search: true,
        web_fetch: false,
        max_uses: 1,
        max_content_tokens: 1000,
        allowed_domains: Vec::new(),
        blocked_domains: Vec::new(),
    });
    store
        .save_model_provider(&owner, &upstream, NOW)
        .await
        .unwrap();
    store
        .save_model_publication(
            &owner,
            &ModelPublicationInput {
                model_id: "public-model".to_owned(),
                display_name: "Hosted model".to_owned(),
                provider_id: "upstream".to_owned(),
                upstream_model: "private-model".to_owned(),
                enabled: true,
            },
            NOW,
        )
        .await
        .unwrap();
    let mut grant = ModelGrantInput {
        allow_resource_sharing: false,
        name: "Hosted budget".to_owned(),
        subject: ModelGrantSubject::User {
            id: owner.user_id.to_string(),
        },
        model_ids: vec!["public-model".to_owned()],
        monthly_tokens: 1000,
        max_concurrent_requests: 1,
        expires_at_ms: None,
    };
    let granted = store
        .save_model_grant(&owner, None, &grant, NOW)
        .await
        .unwrap();
    let key = store
        .create_model_key(
            &owner,
            &ModelKeyInput {
                name: "Hosted client".to_owned(),
                grant_id: granted.grant_id.clone(),
                model_ids: vec!["public-model".to_owned()],
                monthly_tokens: None,
                max_concurrent_requests: None,
                expires_at_ms: None,
            },
            NOW,
        )
        .await
        .unwrap();
    let mut input = request("hosted-low", 50);
    input.protocol = ProviderProtocol::AnthropicMessages;
    let rejected = store
        .reserve_model_request(&key.token, &input, NOW)
        .await
        .err()
        .expect("hosted work must not fit a tiny client reservation");
    assert_eq!(rejected.kind, ModelAccessErrorKind::QuotaExceeded);
    grant.monthly_tokens = 1_000_000;
    store
        .save_model_grant(&owner, Some(&granted.grant_id), &grant, NOW + 1)
        .await
        .unwrap();
    let permit = store
        .reserve_model_request(&key.token, &input, NOW + 2)
        .await
        .unwrap();
    assert_eq!(permit.request.reserved_tokens, 2 * (32768 + 4096));
    store
        .mark_model_request_attempted(&permit.request.request_id, NOW + 3)
        .await
        .unwrap();
    let settled = store
        .settle_model_request(&permit.request.request_id, &known(100, 20), NOW + 4)
        .await
        .unwrap();
    assert_eq!(settled.accounted_tokens, Some(120));
    upstream.profile.hosted_tools = None;
    store
        .save_model_provider(&owner, &upstream, NOW + 5)
        .await
        .unwrap();
    input.request_key = "ordinary-after-hosted".to_owned();
    let ordinary = store
        .reserve_model_request(&key.token, &input, NOW + 6)
        .await
        .unwrap();
    assert_eq!(ordinary.request.reserved_tokens, 50);
}
