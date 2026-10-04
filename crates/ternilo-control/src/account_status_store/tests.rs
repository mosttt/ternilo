use std::time::Duration;

use serde_json::json;
use ternilo_protocol::{ErrorCode, ProviderProfile, ProviderProtocol};
use ternilo_transport::ExecutorId;

use super::*;
use crate::{
    ControlAction, InstanceMode, ModelGrantInput, ModelGrantSubject, ModelKeyInput,
    ModelProviderInput, ModelPublicationInput, ModelRequestInput, ModelRequestSettlement,
    ModelRequestState, NativeRegistration, OidcPrincipal, PageQuery, RegistrationDecision,
    RegistrationMode, SecretCipher, ServiceModelUsage,
};

fn registration(username: &str) -> NativeRegistration {
    NativeRegistration {
        email: format!("{username}@example.test"),
        username: username.to_owned(),
        password: "account-governance-password".to_owned(),
    }
}

#[tokio::test]
async fn account_lifecycle_revokes_credentials_and_preserves_attribution() {
    let store = ControlStore::connect("sqlite::memory:", None, SecretCipher::from_key([41; 32]), 1)
        .await
        .unwrap();
    lifecycle_contract(&store).await;
}

#[expect(
    clippy::too_many_lines,
    reason = "Exercise one identity through admission, ban, recovery and permanent removal with its real credentials and resources."
)]
pub(crate) async fn lifecycle_contract(store: &ControlStore) {
    let owner = store
        .initialize_owner(&registration("owner"), 1_000)
        .await
        .unwrap();
    let admin = &owner.session.user;
    store
        .set_instance_mode(admin, InstanceMode::MultiUser, 1, 1_001)
        .await
        .unwrap();
    store
        .set_registration_settings(admin, RegistrationMode::Open, true, false, 1, 1_002)
        .await
        .unwrap();
    let mut signup = registration("member");
    signup.email = "  Member@Example.TEST  ".to_owned();
    let pending = store.register_native(&signup, 1_003).await.unwrap();
    let rejected = store
        .review_account_registration(
            admin,
            &pending.user_id,
            RegistrationDecision::Reject,
            1,
            1_004,
        )
        .await
        .unwrap();
    assert_eq!(rejected.status, AccountStatus::Rejected);
    assert_eq!(
        store
            .review_account_registration(
                admin,
                &pending.user_id,
                RegistrationDecision::Approve,
                1,
                1_005
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    let accepted = store
        .review_account_registration(
            admin,
            &pending.user_id,
            RegistrationDecision::Approve,
            rejected.status_revision,
            1_006,
        )
        .await
        .unwrap();
    assert_eq!(accepted.status, AccountStatus::Active);
    assert_eq!(accepted.email.as_deref(), Some("member@example.test"));
    let session = store
        .login_native("member", &signup.password, 1_007)
        .await
        .unwrap();
    let member = &session.session.user;
    assert_eq!(session.session.email, accepted.email);
    assert_eq!(
        serde_json::to_value(member).unwrap(),
        json!({"user_id":pending.user_id,"username":"member"})
    );
    let tenant = &session.session.personal_tenant_id;
    let workspace = store
        .create_cloud_workspace(
            member,
            tenant,
            &session.session.personal_project_id,
            "Preserved project",
            1_008,
        )
        .await
        .unwrap();
    let oidc = OidcPrincipal {
        issuer: "https://accounts.example.test".to_owned(),
        subject: "member-subject".to_owned(),
        email: Some("unverified-other@example.test".to_owned()),
        display_name: None,
    };
    store.link_native_oidc(member, &oidc, 1_009).await.unwrap();
    assert_eq!(
        store.authenticate_oidc_user(&oidc, 1_010).await.unwrap(),
        *member
    );
    assert_eq!(
        store
            .identity_session(member.clone())
            .await
            .unwrap()
            .email
            .as_deref(),
        Some("member@example.test")
    );
    let node = store
        .create_owned_enrollment(
            member,
            tenant,
            None,
            ExecutorId::new("member-node"),
            Duration::from_secs(60),
            1_011,
        )
        .await
        .unwrap();
    let credential = store.consume_enrollment(&node.token, 1_012).await.unwrap();
    let principal = store
        .authenticate_node(&credential.token, 1_013)
        .await
        .unwrap();
    store
        .edge_store()
        .require_node_credential(&principal)
        .await
        .unwrap();
    let unused = store
        .create_owned_enrollment(
            member,
            tenant,
            None,
            ExecutorId::new("unused-node"),
            Duration::from_secs(60),
            1_014,
        )
        .await
        .unwrap();
    let (key, request_id) = model_credentials(store, admin, member, 1_015).await;
    for target in [&admin.user_id, &member.user_id] {
        let actor = if target == &admin.user_id {
            admin
        } else {
            member
        };
        assert_eq!(
            store
                .set_account_status(actor, target, AccountStatusAction::Remove, 1, 1_016)
                .await
                .unwrap_err()
                .code,
            ErrorCode::PolicyDenied
        );
    }
    assert_eq!(
        store
            .set_account_status(member, &admin.user_id, AccountStatusAction::Ban, 1, 1_016)
            .await
            .unwrap_err()
            .code,
        ErrorCode::PolicyDenied
    );
    let banned = store
        .set_account_status(
            admin,
            &member.user_id,
            AccountStatusAction::Ban,
            accepted.status_revision,
            1_017,
        )
        .await
        .unwrap();
    assert_eq!(banned.status, AccountStatus::Banned);
    assert!(
        store
            .login_native("member", &signup.password, 1_018)
            .await
            .is_err()
    );
    assert!(
        store
            .authenticate_native_session(&session.access_token, 1_018)
            .await
            .is_err()
    );
    assert!(store.authenticate_oidc_user(&oidc, 1_018).await.is_err());
    assert!(
        store
            .upsert_user(&oidc, "cannot-revive", 1_018)
            .await
            .is_err()
    );
    assert!(
        store
            .authenticate_node(&credential.token, 1_018)
            .await
            .is_err()
    );
    assert!(
        store
            .edge_store()
            .require_node_credential(&principal)
            .await
            .is_err()
    );
    assert!(
        store
            .consume_enrollment(&unused.token, 1_018)
            .await
            .is_err()
    );
    assert!(store.authenticate_model_key(&key, 1_018).await.is_err());
    assert!(
        store
            .check_model_request_authorized(&request_id, 1_018)
            .await
            .is_err()
    );
    assert!(
        store
            .authorize(member, tenant, ControlAction::TenantRead)
            .await
            .is_err()
    );
    // Cancellation must not erase real upstream usage arriving after access was revoked.
    let settled = store
        .settle_model_request(
            &request_id,
            &ModelRequestSettlement {
                state: ModelRequestState::Completed,
                usage: Some(ServiceModelUsage {
                    input_tokens: Some(12),
                    output_tokens: Some(3),
                    ..ServiceModelUsage::default()
                }),
                upstream_request_id: Some("retained-upstream-request".to_owned()),
                error_code: None,
            },
            1_019,
        )
        .await
        .unwrap();
    assert_eq!(settled.actor_user_id, member.user_id);
    assert_eq!(settled.accounted_tokens, Some(15));
    assert_eq!(
        store
            .set_account_status(
                admin,
                &member.user_id,
                AccountStatusAction::Unban,
                accepted.status_revision,
                1_020
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    let active = store
        .set_account_status(
            admin,
            &member.user_id,
            AccountStatusAction::Unban,
            banned.status_revision,
            1_020,
        )
        .await
        .unwrap();
    assert_eq!(active.status, AccountStatus::Active);
    let fresh = store
        .login_native("member", &signup.password, 1_021)
        .await
        .unwrap();
    assert_eq!(fresh.session.user.user_id, member.user_id);
    assert_eq!(fresh.session.personal_tenant_id, *tenant);
    assert!(
        store
            .authenticate_native_session(&session.access_token, 1_022)
            .await
            .is_err()
    );
    assert!(store.authenticate_model_key(&key, 1_022).await.is_err());
    assert!(
        store
            .authenticate_node(&credential.token, 1_022)
            .await
            .is_err()
    );
    assert!(
        store
            .consume_enrollment(&unused.token, 1_022)
            .await
            .is_err()
    );
    let removed = store
        .set_account_status(
            admin,
            &member.user_id,
            AccountStatusAction::Remove,
            active.status_revision,
            1_023,
        )
        .await
        .unwrap();
    assert_eq!(removed.status, AccountStatus::Removed);
    assert!(
        store
            .authenticate_native_session(&fresh.access_token, 1_024)
            .await
            .is_err()
    );
    assert!(
        store
            .login_native("member", &signup.password, 1_024)
            .await
            .is_err()
    );
    assert!(
        store
            .register_oidc(&oidc, "new-name", "new-contact@example.test", None, 1_024)
            .await
            .is_err()
    );
    assert!(
        store
            .upsert_user(&oidc, "cannot-revive", 1_024)
            .await
            .is_err()
    );
    assert!(
        store
            .set_account_status(
                admin,
                &member.user_id,
                AccountStatusAction::Unban,
                removed.status_revision,
                1_024
            )
            .await
            .is_err()
    );
    assert!(
        store
            .review_account_registration(
                admin,
                &member.user_id,
                RegistrationDecision::Approve,
                removed.status_revision,
                1_024
            )
            .await
            .is_err()
    );
    let mut replacement = registration("replacement");
    replacement.email = "MEMBER@example.test".to_owned();
    let replacement = store.register_native(&replacement, 1_025).await.unwrap();
    assert_ne!(replacement.user_id, member.user_id);
    assert_eq!(
        store
            .register_native(&signup, 1_026)
            .await
            .err()
            .unwrap()
            .message,
        "username is already registered"
    );
    let mut tx = store.database.begin().await.unwrap();
    ternilo_storage::set_tenant_scope(&mut tx, tenant)
        .await
        .unwrap();
    let retained: String = sqlx::query_scalar(
        "SELECT owner_user_id FROM control_workspaces WHERE tenant_id = $1 AND workspace_id = $2",
    )
    .bind(tenant.as_str())
    .bind(workspace.workspace_id.as_str())
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    assert_eq!(retained, member.user_id.as_str());
    tx.commit().await.unwrap();
    let usage = store
        .list_model_service_requests(admin, Some(&member.user_id), &PageQuery::default())
        .await
        .unwrap();
    assert_eq!(usage.requests.len(), 1);
    assert_eq!(usage.requests[0].accounted_tokens, Some(15));
    let events: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM control_platform_audit WHERE resource_id = $1 AND action = 'account.status'")
        .bind(member.user_id.as_str()).fetch_one(store.database.pool()).await.unwrap();
    assert_eq!(events, 3);
}

async fn model_credentials(
    store: &ControlStore,
    owner: &ControlUser,
    user: &ControlUser,
    now: u64,
) -> (String, String) {
    let profile: ProviderProfile = serde_json::from_value(json!({"id":"upstream","display_name":"Upstream","base_url":"https://model.example.test/v1","protocol":"openai-responses","defaults":{"context_window":32768,"max_output_tokens":4096},"models":[{"id":"model","settings":{"mode":"inherit"}}],"timeout_ms":1000,"max_attempts":1,"retry_base_delay_ms":250})).unwrap();
    store
        .save_model_provider(
            owner,
            &ModelProviderInput {
                profile,
                enabled: true,
                api_key: Some("test-key".to_owned()),
                clear_api_key: false,
            },
            now,
        )
        .await
        .unwrap();
    store
        .save_model_publication(
            owner,
            &ModelPublicationInput {
                model_id: "public".to_owned(),
                display_name: "Public model".to_owned(),
                provider_id: "upstream".to_owned(),
                upstream_model: "model".to_owned(),
                enabled: true,
            },
            now,
        )
        .await
        .unwrap();
    let grant = store
        .save_model_grant(
            owner,
            None,
            &ModelGrantInput {
                name: "Account grant".to_owned(),
                subject: ModelGrantSubject::User {
                    id: user.user_id.to_string(),
                },
                model_ids: vec!["public".to_owned()],
                monthly_tokens: 1_000,
                max_concurrent_requests: 2,
                expires_at_ms: None,
                allow_resource_sharing: true,
            },
            now,
        )
        .await
        .unwrap();
    let key = store
        .create_model_key(
            user,
            &ModelKeyInput {
                name: "Existing key".to_owned(),
                grant_id: grant.grant_id,
                model_ids: vec!["public".to_owned()],
                monthly_tokens: None,
                max_concurrent_requests: None,
                expires_at_ms: None,
            },
            now,
        )
        .await
        .unwrap();
    let request = store
        .reserve_model_request(
            &key.token,
            &ModelRequestInput {
                request_key: "existing-request".to_owned(),
                payload_hash: "a".repeat(64),
                model_id: "public".to_owned(),
                protocol: ProviderProtocol::OpenAiResponses,
                reserved_tokens: 100,
            },
            now,
        )
        .await
        .unwrap();
    store
        .mark_model_request_attempted(&request.request.request_id, now)
        .await
        .unwrap();
    (key.token, request.request.request_id)
}

#[tokio::test]
async fn contact_email_is_required_unique_and_never_links_an_identity() {
    let store = ControlStore::connect("sqlite::memory:", None, SecretCipher::from_key([41; 32]), 1)
        .await
        .unwrap();
    email_contract(&store).await;
}

pub(crate) async fn email_contract(store: &ControlStore) {
    let owner = store
        .initialize_owner(&registration("owner"), 1_000)
        .await
        .unwrap();
    let actor = &owner.session.user;
    store
        .set_instance_mode(actor, InstanceMode::MultiUser, 1, 1_001)
        .await
        .unwrap();
    store
        .set_registration_settings(actor, RegistrationMode::Open, false, false, 1, 1_002)
        .await
        .unwrap();
    for email in [
        "",
        "plain",
        "a@",
        "a@@example.test",
        "a b@example.test",
        ".a@example.test",
        "a..b@example.test",
        "a@-example.test",
        "a@example..test",
        "a\n@example.test",
    ] {
        let mut signup = registration("invalid");
        signup.email = email.to_owned();
        assert_eq!(
            store
                .register_native(&signup, 1_003)
                .await
                .err()
                .unwrap()
                .code,
            ErrorCode::InvalidInput,
            "{email:?}"
        );
    }
    let mut duplicate = registration("duplicate");
    duplicate.email = " OWNER@EXAMPLE.TEST ".to_owned();
    assert_eq!(
        store
            .register_native(&duplicate, 1_004)
            .await
            .err()
            .unwrap()
            .message,
        "email is already registered"
    );
    let principal = OidcPrincipal {
        issuer: "https://unverified.example.test".to_owned(),
        subject: "same-contact".to_owned(),
        email: Some("owner@example.test".to_owned()),
        display_name: None,
    };
    assert_eq!(
        store
            .authenticate_oidc_user(&principal, 1_005)
            .await
            .unwrap_err()
            .message,
        "choose a platform username to finish registration"
    );
    let external = store
        .register_oidc(&principal, "external", "external@example.test", None, 1_006)
        .await
        .unwrap();
    assert_ne!(external.user_id, actor.user_id);
    let identity = store
        .authenticate_oidc_user(&principal, 1_007)
        .await
        .unwrap();
    assert_eq!(
        store
            .identity_session(identity)
            .await
            .unwrap()
            .email
            .as_deref(),
        Some("external@example.test")
    );
    assert!(
        store
            .account_login_methods(actor)
            .await
            .unwrap()
            .oidc
            .is_none()
    );
}
