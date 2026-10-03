use super::*;
use crate::{
    AccountStatusAction, InstanceMode, NativeRegistration, RegistrationMode, SecretCipher,
};
use std::{collections::BTreeSet, time::Duration};
use ternilo_protocol::{RunId, SessionId, SubmissionId};
use ternilo_transport::NodeCleanupReceipt;

fn registration(name: &str) -> NativeRegistration {
    NativeRegistration {
        username: name.to_owned(),
        email: format!("{name}@example.test"),
        password: "cleanup-test-password".to_owned(),
    }
}

#[tokio::test]
async fn revoked_node_can_only_read_its_durable_cleanup_and_unban_does_not_erase_it() {
    let store = ControlStore::connect("sqlite::memory:", None, SecretCipher::from_key([67; 32]), 1)
        .await
        .unwrap();
    contract(&store).await;
}

#[expect(
    clippy::too_many_lines,
    reason = "run the same account, credential and receipt isolation scenario on both database backends"
)]
pub(crate) async fn contract(store: &ControlStore) {
    let owner = store
        .initialize_owner(&registration("owner"), 1000)
        .await
        .unwrap();
    let admin = &owner.session.user;
    store
        .set_instance_mode(admin, InstanceMode::MultiUser, 1, 1001)
        .await
        .unwrap();
    store
        .set_registration_settings(admin, RegistrationMode::Open, false, 1, 1002)
        .await
        .unwrap();
    let alice = store
        .register_native(&registration("alice"), 1003)
        .await
        .unwrap();
    let bob = store
        .register_native(&registration("bob"), 1004)
        .await
        .unwrap();
    let alice_identity = store
        .login_native("alice", "cleanup-test-password", 1005)
        .await
        .unwrap();
    let bob_identity = store
        .login_native("bob", "cleanup-test-password", 1005)
        .await
        .unwrap();
    let mut credentials = Vec::new();
    for (identity, id) in [
        (&alice_identity, "alice-node"),
        (&bob_identity, "shared-node"),
    ] {
        let invite = store
            .create_owned_enrollment(
                &identity.session.user,
                &identity.session.personal_tenant_id,
                None,
                ExecutorId::new(id),
                Duration::from_secs(60),
                1006,
            )
            .await
            .unwrap();
        credentials.push(store.consume_enrollment(&invite.token, 1007).await.unwrap());
    }
    let shared = &credentials[1];
    for credential in &credentials {
        store
            .synchronize_node_cleanup(
                &credential.token,
                &format!("storage-{}", credential.executor_id),
                1008,
            )
            .await
            .unwrap();
    }
    let node = store.authenticate_node(&shared.token, 1008).await.unwrap();
    store
        .edge_store()
        .register_executor(
            &shared.scope.tenant_id,
            &ternilo_transport::ExecutorHello {
                protocol_version: ternilo_transport::EXECUTOR_PROTOCOL_VERSION,
                executor_id: shared.executor_id.clone(),
                executor_kind: ternilo_transport::ExecutorKind::EdgeNode,
                instance_nonce: "cleanup-test".to_owned(),
                catalog_revision: "cleanup-test".to_owned(),
                capabilities: BTreeSet::new(),
            },
            1008,
        )
        .await
        .unwrap();
    let provenance = InputProvenance {
        input_id: SubmissionId::new("shared-alice-input"),
        run_id: Some(RunId::new("shared-alice-run")),
        author: InputAuthor::Account {
            user_id: alice.user_id.clone(),
            username: "alice".to_owned(),
        },
    };
    let authorization = store
        .edge_store()
        .node_input_authorization(&node, &alice.user_id)
        .await
        .unwrap();
    let mut tx = store
        .database
        .tenant_transaction(&shared.scope.tenant_id)
        .await
        .unwrap();
    EdgeStore::record_input_provenance_in_transaction(
        &mut tx,
        &shared.scope.tenant_id,
        &shared.executor_id,
        &SessionId::new("shared-session"),
        None,
        &provenance,
        1009,
    )
    .await
    .unwrap();
    EdgeStore::record_node_input_authorization_in(
        &mut tx,
        &shared.scope.tenant_id,
        &shared.executor_id,
        &provenance,
        &authorization,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let before = store.node_cleanup_snapshot(&shared.token).await.unwrap();
    assert!(before.requests.is_empty());
    assert!(
        before
            .authorizations
            .iter()
            .any(|account| account.user_id == alice.user_id && account.active)
    );
    let revision = store
        .get_account(admin, &alice.user_id)
        .await
        .unwrap()
        .status_revision;
    let banned = store
        .set_account_status(
            admin,
            &alice.user_id,
            AccountStatusAction::Ban,
            revision,
            1010,
        )
        .await
        .unwrap();
    assert!(
        store
            .authenticate_node(&credentials[0].token, 1011)
            .await
            .is_err()
    );
    let own = store
        .node_cleanup_snapshot(&credentials[0].token)
        .await
        .unwrap();
    let received = store.node_cleanup_snapshot(&shared.token).await.unwrap();
    assert_eq!(own.requests.len(), 1);
    assert_eq!(received.requests.len(), 1);
    assert_eq!(received.requests[0].user_id, alice.user_id);
    assert_eq!(received.requests[0].status_revision, banned.status_revision);
    assert_ne!(own.credential_id, received.credential_id);
    assert_ne!(own.requests[0].request_id, received.requests[0].request_id);
    assert!(
        received
            .authorizations
            .iter()
            .any(|account| account.user_id == bob.user_id && account.active)
    );
    assert!(
        !received
            .authorizations
            .iter()
            .find(|account| account.user_id == alice.user_id)
            .unwrap()
            .active
    );
    let unbanned = store
        .set_account_status(
            admin,
            &alice.user_id,
            AccountStatusAction::Unban,
            banned.status_revision,
            1012,
        )
        .await
        .unwrap();
    let repeated = store.node_cleanup_snapshot(&shared.token).await.unwrap();
    assert_eq!(repeated.requests, received.requests);
    assert_eq!(
        repeated
            .authorizations
            .iter()
            .find(|account| account.user_id == alice.user_id)
            .unwrap()
            .status_revision,
        unbanned.status_revision
    );
    assert!(
        store
            .authenticate_node(&credentials[0].token, 1013)
            .await
            .is_err()
    );
    // A new registration cannot use an earlier credential instance's cleanup receipt.
    let invite = store
        .create_owned_enrollment(
            &alice_identity.session.user,
            &alice_identity.session.personal_tenant_id,
            None,
            ExecutorId::new("alice-node"),
            Duration::from_secs(60),
            1014,
        )
        .await
        .unwrap();
    let replacement = store.consume_enrollment(&invite.token, 1015).await.unwrap();
    assert!(
        store
            .node_cleanup_snapshot(&replacement.token)
            .await
            .unwrap()
            .requests
            .is_empty()
    );
    assert_eq!(
        store
            .node_cleanup_snapshot(&credentials[0].token)
            .await
            .unwrap()
            .requests,
        own.requests
    );
    assert!(store.node_cleanup_snapshot("ter_n_invalid").await.is_err());
    // Revocation keeps a purpose-limited channel for an outstanding request.
    store
        .revoke_owned_executor(
            &bob_identity.session.user,
            &shared.scope.tenant_id,
            &shared.executor_id,
            1016,
        )
        .await
        .unwrap();
    assert!(store.authenticate_node(&shared.token, 1017).await.is_err());
    assert_eq!(
        store
            .node_cleanup_snapshot(&shared.token)
            .await
            .unwrap()
            .requests,
        received.requests
    );
    let invite = store
        .create_owned_enrollment(
            &bob_identity.session.user,
            &shared.scope.tenant_id,
            None,
            ExecutorId::new("no-cleanup-node"),
            Duration::from_secs(60),
            1018,
        )
        .await
        .unwrap();
    let no_cleanup = store.consume_enrollment(&invite.token, 1019).await.unwrap();
    store
        .revoke_owned_executor(
            &bob_identity.session.user,
            &shared.scope.tenant_id,
            &no_cleanup.executor_id,
            1020,
        )
        .await
        .unwrap();
    assert!(
        store
            .node_cleanup_snapshot(&no_cleanup.token)
            .await
            .is_err()
    );
    let receipt = NodeCleanupReceipt {
        storage_instance_id: format!("storage-{}", shared.executor_id),
        request_id: received.requests[0].request_id.clone(),
        status_revision: received.requests[0].status_revision,
        state: NodeCleanupState::Confirmed,
        detail: None,
    };
    for bad in [
        NodeCleanupReceipt {
            detail: Some("/private/node/state.json: access denied".into()),
            ..receipt.clone()
        },
        NodeCleanupReceipt {
            storage_instance_id: "another-data-directory".into(),
            ..receipt.clone()
        },
        NodeCleanupReceipt {
            status_revision: receipt.status_revision + 1,
            ..receipt.clone()
        },
        NodeCleanupReceipt {
            request_id: own.requests[0].request_id.clone(),
            ..receipt.clone()
        },
    ] {
        assert!(
            store
                .record_node_cleanup_receipt(&shared.token, &bad, 1021)
                .await
                .is_err()
        );
    }
    assert!(
        store
            .synchronize_node_cleanup(&replacement.token, "replacement-storage", 1021)
            .await
            .is_err()
    );
    let replacement_storage = format!("storage-{}", replacement.executor_id);
    store
        .synchronize_node_cleanup(&replacement.token, &replacement_storage, 1021)
        .await
        .unwrap();
    assert!(
        store
            .record_node_cleanup_receipt(
                &replacement.token,
                &NodeCleanupReceipt {
                    storage_instance_id: replacement_storage,
                    request_id: own.requests[0].request_id.clone(),
                    ..receipt.clone()
                },
                1021
            )
            .await
            .is_err()
    );
    assert!(
        store
            .synchronize_node_cleanup(&shared.token, "another-data-directory", 1021)
            .await
            .is_err()
    );
    assert!(
        store
            .account_node_cleanup(&bob_identity.session.user, &alice.user_id)
            .await
            .is_err()
    );
    let pending = NodeCleanupReceipt {
        state: NodeCleanupState::Pending,
        detail: Some("process_exit_pending".into()),
        ..receipt.clone()
    };
    store
        .record_node_cleanup_receipt(&shared.token, &pending, 1022)
        .await
        .unwrap();
    let records = store
        .account_node_cleanup(admin, &alice.user_id)
        .await
        .unwrap();
    assert_eq!(records.len(), 2);
    let shared_record = records
        .iter()
        .find(|record| record.executor_id == shared.executor_id)
        .unwrap();
    assert_eq!(shared_record.request.state, NodeCleanupState::Pending);
    assert_eq!(shared_record.request.detail, pending.detail);
    assert_eq!(shared_record.request.confirmed_at_ms, None);
    store
        .record_node_cleanup_receipt(&shared.token, &receipt, 1023)
        .await
        .unwrap();
    store
        .record_node_cleanup_receipt(&shared.token, &receipt, 1024)
        .await
        .unwrap();
    store
        .record_node_cleanup_receipt(&shared.token, &pending, 1025)
        .await
        .unwrap();
    let confirmed = store
        .synchronize_node_cleanup(&shared.token, &receipt.storage_instance_id, 1026)
        .await
        .unwrap();
    assert!(!confirmed.connection_allowed);
    assert_eq!(confirmed.requests[0].state, NodeCleanupState::Confirmed);
    assert_eq!(confirmed.requests[0].confirmed_at_ms, Some(1023));
    assert_eq!(confirmed.requests[0].detail, None);
    assert_eq!(
        store
            .node_cleanup_snapshot(&credentials[0].token)
            .await
            .unwrap()
            .requests[0]
            .state,
        NodeCleanupState::Pending
    );
}

#[tokio::test]
#[ignore = "requires TERNILO_TEST_DATABASE_URL pointing to a disposable PostgreSQL database"]
async fn cleanup_uses_only_production_postgres_runtime_grants() {
    use sqlx::Executor as _;
    let url = std::env::var("TERNILO_TEST_DATABASE_URL").unwrap();
    assert!(url.contains("ternilo_control_test"));
    let admin = sqlx::PgPool::connect(&url).await.unwrap();
    admin
        .execute("DROP SCHEMA IF EXISTS public CASCADE")
        .await
        .unwrap();
    admin.execute("CREATE SCHEMA public").await.unwrap();
    crate::postgres_test::prepare_role(&admin, "ternilo_cleanup_test", "cleanup-password").await;
    let mut runtime = url
        .parse::<sqlx::any::AnyConnectOptions>()
        .unwrap()
        .database_url;
    runtime.set_username("ternilo_cleanup_test").unwrap();
    runtime.set_password(Some("cleanup-password")).unwrap();
    let store = ControlStore::connect(
        runtime.as_str(),
        Some(&url),
        SecretCipher::from_key([67; 32]),
        2,
    )
    .await
    .unwrap();
    contract(&store).await;
    store.database.close().await;
    admin.close().await;
}
