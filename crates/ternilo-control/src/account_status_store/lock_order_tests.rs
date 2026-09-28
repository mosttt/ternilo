use std::time::Duration;

use ternilo_protocol::{ErrorCode, TenantId};
use ternilo_storage::{Transaction, lock, set_tenant_scope};
use ternilo_transport::ExecutorId;
use tokio::task::JoinHandle;

use crate::{
    AccountStatus, AccountStatusAction, ControlStore, InstanceMode, NativeRegistration,
    OidcPrincipal, TenantQuota, TenantRole, UserInvitationRequest,
};

async fn instance_owner(store: &ControlStore) -> crate::ControlUser {
    let owner = store
        .initialize_owner(
            &NativeRegistration {
                email: "lock-owner@example.test".to_owned(),
                username: "lock-owner".to_owned(),
                password: "lock-order-test-password".to_owned(),
            },
            1_000,
        )
        .await
        .unwrap()
        .session
        .user;
    store
        .set_instance_mode(&owner, InstanceMode::MultiUser, 1, 1_001)
        .await
        .unwrap();
    owner
}

async fn user(store: &ControlStore, name: &str) -> crate::ControlUser {
    store
        .upsert_user(
            &OidcPrincipal {
                issuer: "https://lock-order.example.test".to_owned(),
                subject: name.to_owned(),
                email: None,
                display_name: None,
            },
            name,
            1_002,
        )
        .await
        .unwrap()
}

async fn blocker(store: &ControlStore, tenant: Option<&TenantId>) -> (Transaction, i64) {
    let mut tx = store.database().begin().await.unwrap();
    if let Some(tenant) = tenant {
        set_tenant_scope(&mut tx, tenant).await.unwrap();
    }
    sqlx::query("SET LOCAL lock_timeout = '2s'")
        .execute(&mut *tx)
        .await
        .unwrap();
    let pid = sqlx::query_scalar("SELECT CAST(pg_backend_pid() AS BIGINT)")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    (tx, pid)
}

/// Observe a real dependency on our transaction, rather than inferring execution from elapsed time.
async fn wait_until_blocked<T>(admin: &sqlx::PgPool, blocker_pid: i64, task: &JoinHandle<T>) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            assert!(!task.is_finished(), "the operation must wait at the declared lock boundary");
            let waiting: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM pg_stat_activity WHERE datname = current_database() AND CAST($1 AS INTEGER) = ANY(pg_blocking_pids(pid))")
                .bind(blocker_pid).fetch_one(admin).await.unwrap();
            if waiting != 0 { break; }
            tokio::task::yield_now().await;
        }
    }).await.expect("the real operation must enter an observable PostgreSQL lock wait");
}

pub(crate) async fn invitation_lock_order_contract(store: &ControlStore, admin: &sqlx::PgPool) {
    let _owner = instance_owner(store).await;
    let issuer = user(store, "team-issuer").await;
    let recipient = user(store, "team-recipient").await;
    let team = store
        .create_tenant(
            &issuer,
            "invitation-lock-team",
            "Invitation lock team",
            TenantQuota::default(),
            1_003,
        )
        .await
        .unwrap();
    let invitation = store
        .create_user_invitation(
            &issuer,
            &UserInvitationRequest {
                tenant_id: Some(team.tenant_id.clone()),
                role: TenantRole::Member,
                expires_in_seconds: 300,
            },
            1_004,
        )
        .await
        .unwrap();
    let (mut tx, pid) = blocker(store, None).await;
    lock(&mut tx, "ternilo:instance").await.unwrap();
    lock(&mut tx, &format!("ternilo:account-role:{}", issuer.user_id))
        .await
        .unwrap();
    let pending = {
        let store = store.clone();
        let recipient = recipient.clone();
        let token = invitation.token;
        tokio::spawn(async move { store.join_invitation(&recipient, &token, 1_006).await })
    };
    wait_until_blocked(admin, pid, &pending).await;
    // Mirror the relevant ban transaction locks; this isolates ordering, not the complete ban API.
    sqlx::query("UPDATE control_users SET status = 'banned', status_revision = status_revision + 1 WHERE user_id = $1")
        .bind(issuer.user_id.as_str()).execute(&mut *tx).await.unwrap();
    sqlx::query(
        "UPDATE control_user_invitations SET consumed_at_ms = 1005 WHERE invitation_id = $1",
    )
    .bind(&invitation.invitation_id)
    .execute(&mut *tx)
    .await
    .expect("join must not hold the invitation row while waiting for the issuer's account lock");
    tx.commit().await.unwrap();
    let result = tokio::time::timeout(Duration::from_secs(10), pending)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.unwrap_err().code, ErrorCode::PolicyDenied);
    assert!(
        !store
            .list_tenants(&recipient)
            .await
            .unwrap()
            .iter()
            .any(|tenant| tenant.tenant_id == team.tenant_id)
    );
}

#[derive(Clone, Copy)]
enum Operation {
    Authenticate,
    Ban,
    Revoke,
}

#[expect(
    clippy::too_many_lines,
    reason = "Three deterministic lock interleavings validate the same enrollment/executor/credential ordering."
)]
pub(crate) async fn node_lock_order_contract(store: &ControlStore, admin: &sqlx::PgPool) {
    let owner = instance_owner(store).await;
    for (index, operation) in [Operation::Authenticate, Operation::Ban, Operation::Revoke]
        .into_iter()
        .enumerate()
    {
        let member = user(store, &format!("node-owner-{index}")).await;
        let identity = store.identity_session(member.clone()).await.unwrap();
        let tenant = identity.personal_tenant_id;
        let executor = ExecutorId::new(format!("node-{index}"));
        let enrollment = store
            .create_owned_enrollment(
                &member,
                &tenant,
                None,
                executor.clone(),
                Duration::from_secs(60),
                1_003,
            )
            .await
            .unwrap();
        let unused = store
            .create_owned_enrollment(
                &member,
                &tenant,
                None,
                executor.clone(),
                Duration::from_secs(60),
                1_004,
            )
            .await
            .unwrap();
        let credential = store
            .consume_enrollment(&enrollment.token, 1_005)
            .await
            .unwrap();
        store
            .authenticate_node(&credential.token, 1_006)
            .await
            .unwrap();
        let (mut tx, pid) = blocker(store, Some(&tenant)).await;
        if matches!(operation, Operation::Revoke) {
            sqlx::query("SELECT enrollment_id FROM control_executor_enrollments WHERE enrollment_id = $1 FOR UPDATE")
                .bind(&unused.enrollment_id).fetch_one(&mut *tx).await.unwrap();
        } else {
            sqlx::query("SELECT executor_id FROM control_executors WHERE tenant_id = $1 AND executor_id = $2 FOR UPDATE")
                .bind(tenant.as_str()).bind(executor.as_str()).fetch_one(&mut *tx).await.unwrap();
        }
        let pending = {
            let store = store.clone();
            let member = member.clone();
            let owner = owner.clone();
            let tenant = tenant.clone();
            let executor = executor.clone();
            let token = credential.token.clone();
            tokio::spawn(async move {
                match operation {
                    Operation::Authenticate => {
                        store.authenticate_node(&token, 1_008).await.map(|_| ())
                    }
                    Operation::Ban => store
                        .set_account_status(
                            &owner,
                            &member.user_id,
                            AccountStatusAction::Ban,
                            1,
                            1_008,
                        )
                        .await
                        .map(|_| ()),
                    Operation::Revoke => {
                        store
                            .revoke_owned_executor(&member, &tenant, &executor, 1_008)
                            .await
                    }
                }
            })
        };
        wait_until_blocked(admin, pid, &pending).await;
        if matches!(operation, Operation::Revoke) {
            sqlx::query("UPDATE control_executors SET last_seen_at_ms = 1007 WHERE tenant_id = $1 AND executor_id = $2")
                .bind(tenant.as_str()).bind(executor.as_str()).execute(&mut *tx).await
                .expect("executor revocation must wait on enrollment before locking the executor");
        }
        sqlx::query(
            "UPDATE control_node_credentials SET revoked_at_ms = 1007 WHERE credential_id = $1",
        )
        .bind(&credential.credential_id)
        .execute(&mut *tx)
        .await
        .expect("a pending executor lock must not retain a credential row lock");
        tx.commit().await.unwrap();
        let result = tokio::time::timeout(Duration::from_secs(10), pending)
            .await
            .unwrap()
            .unwrap();
        if matches!(operation, Operation::Authenticate) {
            assert_eq!(
                result.unwrap_err().code,
                ErrorCode::PolicyDenied,
                "a credential revoked while authentication waited cannot be authenticated from the earlier snapshot"
            );
        } else {
            result.unwrap();
        }
        assert!(
            store
                .authenticate_node(&credential.token, 1_009)
                .await
                .is_err()
        );
        if matches!(operation, Operation::Ban) {
            assert_eq!(
                store
                    .get_account(&owner, &member.user_id)
                    .await
                    .unwrap()
                    .status,
                AccountStatus::Banned
            );
        }
    }
}
