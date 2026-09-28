use serde_json::json;
use sqlx::{Executor, Row};
use ternilo_control::{
    ControlStore, ControlUser, OidcPrincipal, SecretCipher, TenantQuota, TenantRole, TenantSummary,
};
use ternilo_protocol::{
    AgentPresetCopyRequest, AgentPresetTrust, AgentPresetUpdateRequest, DefaultModelSelection,
    Profile, ProviderModel, ProviderModelDefaults, ProviderModelSettings, ProviderProfile,
    ProviderProtocol, SidebarOrdering, system_agent_preset,
};

const NOW: u64 = 1_800_000_000_000;
const CURRENT_KEY: [u8; 32] = [31; 32];
const NEXT_KEY: [u8; 32] = [47; 32];

struct Fixture {
    admin_url: String,
    store: ControlStore,
    alice: ControlUser,
    bob: ControlUser,
    tenant_a: TenantSummary,
    tenant_b: TenantSummary,
}

#[tokio::test]
#[ignore = "requires TERNILO_TEST_DATABASE_URL pointing to a disposable PostgreSQL database"]
async fn postgres_user_settings_are_tenant_owner_scoped_encrypted_and_rotatable() {
    let fixture = setup().await;
    settings_contract(fixture).await;
}

#[tokio::test]
async fn sqlite_user_settings_follow_the_same_contract() {
    let directory = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}",
        directory.path().join("settings.sqlite3").display()
    );
    let fixture = setup_with_url(url).await;
    settings_contract(fixture).await;
}

async fn settings_contract(fixture: Fixture) {
    write_and_assert_credentials(&fixture).await;
    write_and_assert_providers(&fixture).await;
    write_and_assert_default_models(&fixture).await;
    write_and_assert_presets(&fixture).await;
    write_and_assert_sidebar_ordering(&fixture).await;
    assert_ciphertext_and_rotation(fixture).await;
}

async fn write_and_assert_sidebar_ordering(fixture: &Fixture) {
    let alice_a = SidebarOrdering {
        workspace_order: vec!["workspace-b".to_owned(), "workspace-a".to_owned()],
        session_order_by_account: std::collections::BTreeMap::from([(
            "workspace-a".to_owned(),
            vec!["session-2".to_owned(), "session-1".to_owned()],
        )]),
    };
    let bob_a = SidebarOrdering {
        workspace_order: vec!["workspace-c".to_owned()],
        session_order_by_account: std::collections::BTreeMap::new(),
    };
    fixture
        .store
        .set_user_sidebar_ordering(
            &fixture.alice,
            &fixture.tenant_a.tenant_id,
            alice_a.clone(),
            NOW + 40,
        )
        .await
        .unwrap();
    fixture
        .store
        .set_user_sidebar_ordering(
            &fixture.bob,
            &fixture.tenant_a.tenant_id,
            bob_a.clone(),
            NOW + 41,
        )
        .await
        .unwrap();
    assert_eq!(
        fixture
            .store
            .user_sidebar_ordering(&fixture.alice, &fixture.tenant_a.tenant_id)
            .await
            .unwrap(),
        alice_a
    );
    assert_eq!(
        fixture
            .store
            .user_sidebar_ordering(&fixture.bob, &fixture.tenant_a.tenant_id)
            .await
            .unwrap(),
        bob_a
    );
    assert_eq!(
        fixture
            .store
            .user_sidebar_ordering(&fixture.alice, &fixture.tenant_b.tenant_id)
            .await
            .unwrap(),
        SidebarOrdering::default()
    );
}

async fn write_and_assert_default_models(fixture: &Fixture) {
    let selection = DefaultModelSelection::NamedProvider {
        provider_id: "shared-provider".to_owned(),
        model: "model-a".to_owned(),
        reasoning_effort: None,
    };
    assert_eq!(
        fixture
            .store
            .set_user_default_model(
                &fixture.alice,
                &fixture.tenant_a.tenant_id,
                selection.clone(),
                NOW + 21,
            )
            .await
            .unwrap(),
        selection
    );
    assert_eq!(
        fixture
            .store
            .user_default_model(&fixture.alice, &fixture.tenant_a.tenant_id)
            .await
            .unwrap(),
        selection
    );
    assert_eq!(
        fixture
            .store
            .user_default_model(&fixture.bob, &fixture.tenant_a.tenant_id)
            .await
            .unwrap(),
        DefaultModelSelection::ProfileDefault
    );
    assert_eq!(
        fixture
            .store
            .user_default_model(&fixture.alice, &fixture.tenant_b.tenant_id)
            .await
            .unwrap(),
        DefaultModelSelection::ProfileDefault
    );
}

async fn write_and_assert_providers(fixture: &Fixture) {
    for (actor, tenant, display_name) in [
        (&fixture.alice, &fixture.tenant_a, "Alice A"),
        (&fixture.bob, &fixture.tenant_a, "Bob A"),
        (&fixture.alice, &fixture.tenant_b, "Alice B"),
    ] {
        fixture
            .store
            .upsert_user_provider_profile(
                actor,
                &tenant.tenant_id,
                provider(display_name),
                NOW + 20,
            )
            .await
            .unwrap();
    }
    for (actor, tenant, expected) in [
        (&fixture.alice, &fixture.tenant_a, "Alice A"),
        (&fixture.bob, &fixture.tenant_a, "Bob A"),
        (&fixture.alice, &fixture.tenant_b, "Alice B"),
    ] {
        let providers = fixture
            .store
            .user_provider_profiles(actor, &tenant.tenant_id)
            .await
            .unwrap();
        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].display_name, expected);
    }
    let mut materialized = provider("Materialized");
    "materialized-provider".clone_into(&mut materialized.id);
    fixture
        .store
        .create_user_provider_profile(
            &fixture.alice,
            &fixture.tenant_a.tenant_id,
            materialized.clone(),
            NOW + 22,
        )
        .await
        .unwrap();
    let mut overwrite = materialized.clone();
    "Must Not Replace".clone_into(&mut overwrite.display_name);
    let conflict = fixture
        .store
        .create_user_provider_profile(
            &fixture.alice,
            &fixture.tenant_a.tenant_id,
            overwrite,
            NOW + 23,
        )
        .await
        .unwrap_err();
    assert_eq!(conflict.code, ternilo_protocol::ErrorCode::Conflict);
    assert_eq!(
        fixture
            .store
            .user_provider_profile(
                &fixture.alice,
                &fixture.tenant_a.tenant_id,
                "materialized-provider"
            )
            .await
            .unwrap(),
        Some(materialized)
    );
    assert_eq!(
        fixture
            .store
            .user_provider_profile(
                &fixture.bob,
                &fixture.tenant_a.tenant_id,
                "materialized-provider"
            )
            .await
            .unwrap(),
        None
    );
}

fn provider(display_name: &str) -> ProviderProfile {
    ProviderProfile {
        id: "shared-provider".to_owned(),
        display_name: display_name.to_owned(),
        base_url: "https://models.example.test/v1".to_owned(),
        protocol: ProviderProtocol::OpenAiResponses,
        api_key_ref: Some("SHARED_NAME".to_owned()),
        defaults: ProviderModelDefaults {
            context_window: 128_000,
            max_output_tokens: 4_096,
            reasoning: None,
        },
        models: vec![ProviderModel {
            id: "model-a".to_owned(),
            display_name: Some("Model A".to_owned()),
            settings: ProviderModelSettings::Inherit,
        }],
        timeout_ms: 30_000,
        max_attempts: 2,
        retry_base_delay_ms: 100,
    }
}

async fn setup() -> Fixture {
    let admin_url = std::env::var("TERNILO_TEST_DATABASE_URL")
        .expect("TERNILO_TEST_DATABASE_URL must be set for the ignored PostgreSQL test");
    assert!(
        admin_url.contains("ternilo_control_test"),
        "integration test refuses a database URL without ternilo_control_test"
    );
    let admin = sqlx::PgPool::connect(&admin_url).await.unwrap();
    admin
        .execute("DROP SCHEMA IF EXISTS public CASCADE")
        .await
        .unwrap();
    admin.execute("CREATE SCHEMA public").await.unwrap();
    admin.close().await;

    setup_with_url(admin_url).await
}

async fn setup_with_url(admin_url: String) -> Fixture {
    let store = ControlStore::connect(&admin_url, None, SecretCipher::from_key(CURRENT_KEY), 4)
        .await
        .unwrap();
    let alice = user(&store, "settings-alice", "Alice").await;
    let bob = user(&store, "settings-bob", "Bob").await;
    let tenant_a = store
        .create_tenant(
            &alice,
            "settings-a",
            "Settings A",
            TenantQuota::default(),
            NOW + 1,
        )
        .await
        .unwrap();
    let tenant_b = store
        .create_tenant(
            &bob,
            "settings-b",
            "Settings B",
            TenantQuota::default(),
            NOW + 2,
        )
        .await
        .unwrap();
    store
        .set_membership(
            &alice,
            &tenant_a.tenant_id,
            &bob.user_id,
            TenantRole::Member,
            NOW + 3,
        )
        .await
        .unwrap();
    store
        .set_membership(
            &bob,
            &tenant_b.tenant_id,
            &alice.user_id,
            TenantRole::Member,
            NOW + 4,
        )
        .await
        .unwrap();
    Fixture {
        admin_url,
        store,
        alice,
        bob,
        tenant_a,
        tenant_b,
    }
}

async fn user(store: &ControlStore, subject: &str, display_name: &str) -> ControlUser {
    store
        .upsert_user(
            &OidcPrincipal {
                issuer: "https://id.example.test".to_owned(),
                subject: subject.to_owned(),
                email: None,
                display_name: Some(display_name.to_owned()),
            },
            &format!("test-{subject}"),
            NOW,
        )
        .await
        .unwrap()
}

async fn write_and_assert_credentials(fixture: &Fixture) {
    put_credential(
        fixture,
        &fixture.alice,
        &fixture.tenant_a,
        "alice-tenant-a-secret",
        NOW + 5,
    )
    .await;
    put_credential(
        fixture,
        &fixture.bob,
        &fixture.tenant_a,
        "bob-tenant-a-secret",
        NOW + 6,
    )
    .await;
    put_credential(
        fixture,
        &fixture.alice,
        &fixture.tenant_b,
        "alice-tenant-b-secret",
        NOW + 7,
    )
    .await;
    let record = fixture
        .store
        .put_user_credential_record(
            &fixture.alice,
            &fixture.tenant_a.tenant_id,
            "fixture/account",
            "oauth-token",
            &json!({ "access_token": "record-secret", "expires_at": 123 }),
            NOW + 8,
        )
        .await
        .unwrap();
    let inventory = fixture
        .store
        .user_credential_inventory(&fixture.alice, &fixture.tenant_a.tenant_id)
        .await
        .unwrap();
    assert_eq!(inventory.references.len(), 1);
    assert_eq!(inventory.records, vec![record]);
    let serialized = serde_json::to_string(&inventory).unwrap();
    assert!(!serialized.contains("alice-tenant-a-secret"));
    assert!(!serialized.contains("record-secret"));
    assert_credential(
        fixture,
        &fixture.alice,
        &fixture.tenant_a,
        "alice-tenant-a-secret",
    )
    .await;
    assert_credential(
        fixture,
        &fixture.bob,
        &fixture.tenant_a,
        "bob-tenant-a-secret",
    )
    .await;
    assert_credential(
        fixture,
        &fixture.alice,
        &fixture.tenant_b,
        "alice-tenant-b-secret",
    )
    .await;
}

async fn put_credential(
    fixture: &Fixture,
    actor: &ControlUser,
    tenant: &TenantSummary,
    value: &str,
    now_ms: u64,
) {
    fixture
        .store
        .put_user_credential(actor, &tenant.tenant_id, "SHARED_NAME", value, now_ms)
        .await
        .unwrap();
}

async fn assert_credential(
    fixture: &Fixture,
    actor: &ControlUser,
    tenant: &TenantSummary,
    expected: &str,
) {
    assert_eq!(
        fixture
            .store
            .resolve_user_credential(actor, &tenant.tenant_id, "SHARED_NAME")
            .await
            .unwrap()
            .unwrap()
            .as_str(),
        expected
    );
}

async fn write_and_assert_presets(fixture: &Fixture) {
    let initial = fixture
        .store
        .user_agent_preset_roster(&fixture.alice, &fixture.tenant_a.tenant_id)
        .await
        .unwrap();
    assert_eq!(
        initial
            .presets
            .iter()
            .map(|preset| preset.id.as_str())
            .collect::<Vec<_>>(),
        ["standard", "ptc", "minimal", "creative"],
    );
    for id in ["standard", "ptc", "minimal", "creative"] {
        assert_eq!(
            fixture
                .store
                .user_agent_preset(&fixture.alice, &fixture.tenant_a.tenant_id, id)
                .await
                .unwrap(),
            system_agent_preset(id).unwrap(),
        );
    }
    let copied = fixture
        .store
        .copy_user_agent_preset(
            &fixture.alice,
            &fixture.tenant_a.tenant_id,
            AgentPresetCopyRequest {
                from: "ptc".to_owned(),
                id: "alice-ptc".to_owned(),
                display_name: Some("Alice PTC".to_owned()),
            },
            NOW + 9,
        )
        .await
        .unwrap();
    assert_eq!(copied.summary.trust, AgentPresetTrust::User);
    assert_eq!(copied.profile, system_agent_preset("ptc").unwrap().profile);
    assert!(
        fixture
            .store
            .copy_user_agent_preset(
                &fixture.alice,
                &fixture.tenant_a.tenant_id,
                AgentPresetCopyRequest {
                    from: "standard".to_owned(),
                    id: "creative".to_owned(),
                    display_name: None,
                },
                NOW + 9,
            )
            .await
            .is_err()
    );
    let updated = fixture
        .store
        .update_user_agent_preset(
            &fixture.alice,
            &fixture.tenant_a.tenant_id,
            "alice-ptc",
            AgentPresetUpdateRequest {
                display_name: "Alice Coding".to_owned(),
                description: "Personal preset".to_owned(),
                profile: Profile::default(),
            },
            NOW + 10,
        )
        .await
        .unwrap();
    assert_eq!(updated.summary.display_name, "Alice Coding");
    let roster = fixture
        .store
        .set_default_user_agent_preset(
            &fixture.alice,
            &fixture.tenant_a.tenant_id,
            "alice-ptc",
            NOW + 11,
        )
        .await
        .unwrap();
    assert_eq!(roster.default_id, "alice-ptc");
    assert_eq!(roster.presets.len(), 5);
    let bob_roster = fixture
        .store
        .user_agent_preset_roster(&fixture.bob, &fixture.tenant_a.tenant_id)
        .await
        .unwrap();
    assert_eq!(bob_roster.default_id, "standard");
    assert_eq!(bob_roster.presets.len(), 4);
}

async fn assert_ciphertext_and_rotation(fixture: Fixture) {
    let raw = ternilo_storage::Database::connect(&fixture.admin_url, 2)
        .await
        .unwrap();
    let rows = sqlx::query("SELECT nonce, ciphertext FROM control_user_credentials")
        .fetch_all(raw.pool())
        .await
        .unwrap();
    assert_eq!(rows.len(), 3);
    for row in rows {
        assert_eq!(row.try_get::<Vec<u8>, _>("nonce").unwrap().len(), 24);
        let ciphertext = row.try_get::<Vec<u8>, _>("ciphertext").unwrap();
        assert!(!String::from_utf8_lossy(&ciphertext).contains("secret"));
    }
    let record_ciphertext =
        sqlx::query_scalar::<_, Vec<u8>>("SELECT ciphertext FROM control_user_credential_records")
            .fetch_one(raw.pool())
            .await
            .unwrap();
    assert!(!String::from_utf8_lossy(&record_ciphertext).contains("record-secret"));
    raw.close().await;
    drop(fixture.store);

    assert_eq!(
        ControlStore::rotate_secret_master_key(
            &fixture.admin_url,
            &SecretCipher::from_key(CURRENT_KEY),
            &SecretCipher::from_key(NEXT_KEY),
        )
        .await
        .unwrap(),
        4
    );
    let reopened = ControlStore::connect(
        &fixture.admin_url,
        None,
        SecretCipher::from_key(NEXT_KEY),
        2,
    )
    .await
    .unwrap();
    assert_eq!(
        reopened
            .resolve_user_credential(&fixture.alice, &fixture.tenant_a.tenant_id, "SHARED_NAME",)
            .await
            .unwrap()
            .unwrap()
            .as_str(),
        "alice-tenant-a-secret"
    );
}
