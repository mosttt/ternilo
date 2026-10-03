use super::{Fixture, ModelTrafficLimits, Source, denied, input, save};
use crate::{PageQuery, ServiceAccountCreate};

#[expect(
    clippy::too_many_lines,
    reason = "Verify the complete attribution and lifecycle contract across authorization and settlement."
)]
pub(super) async fn contract(f: &Fixture, now: u64) {
    let tenant = f
        .store
        .account_provider_space(&f.other.user_id)
        .await
        .unwrap();
    let account = f
        .store
        .create_service_account(
            &f.other,
            &tenant,
            &ServiceAccountCreate {
                name: "Traffic service fixture".into(),
                notes: String::new(),
            },
            now,
        )
        .await
        .unwrap();
    let query = PageQuery {
        query: Some("Traffic service".into()),
        limit: 1,
        ..PageQuery::default()
    };
    assert!(
        f.store
            .model_traffic_targets(&f.owner, &query)
            .await
            .is_err()
    );
    let targets = f
        .store
        .model_traffic_targets(&f.admin, &query)
        .await
        .unwrap();
    assert_eq!(targets.accounts.len(), 1);
    assert_eq!(targets.accounts[0].user_id, account.service_account_id);
    assert_eq!(targets.accounts[0].kind, "service");
    assert!(targets.accounts[0].space_name.is_some());
    let first_page = f
        .store
        .model_traffic_targets(
            &f.admin,
            &PageQuery {
                limit: 1,
                ..PageQuery::default()
            },
        )
        .await
        .unwrap();
    let second_page = f
        .store
        .model_traffic_targets(
            &f.admin,
            &PageQuery {
                limit: 1,
                cursor: first_page.next_cursor,
                ..PageQuery::default()
            },
        )
        .await
        .unwrap();
    assert_ne!(
        first_page.accounts[0].user_id,
        second_page.accounts[0].user_id
    );

    // Seed a canonical accepted request in a different tenant. The aggregate can count it,
    // while an ordinary tenant-scoped read must still be unable to see its contents.
    let mut tx = f
        .store
        .database()
        .tenant_transaction(&tenant)
        .await
        .unwrap();
    sqlx::query("INSERT INTO control_computer_model_requests(tenant_id,request_id,credential_id,session_id,run_id,request_key,payload_hash,execution_executor_id,source_executor_id,actor_user_id,model_owner_user_id,resource_owner_user_id,snapshot_json,max_attempts,state,created_at_ms,updated_at_ms) VALUES($1,'traffic-computer','credential','session','run','key','hash','execution','source',$2,$3,$3,'{}',2,'pending',$4,$4)")
        .bind(tenant.as_str()).bind(f.owner.user_id.as_str()).bind(account.service_account_id.as_str()).bind(i64::try_from(now).unwrap()).execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
    let (device, _) = f
        .connect(&ternilo_protocol::ModelDeviceLimits::default(), now - 6000)
        .await;
    let status = f
        .store
        .account_model_traffic(&f.owner, &f.owner.user_id, now)
        .await
        .unwrap();
    assert_eq!((status.recent_requests, status.active_requests), (1, 1));
    let source = f
        .store
        .account_model_traffic(&f.admin, &account.service_account_id, now)
        .await
        .unwrap();
    assert_eq!(
        (source.recent_requests, source.active_requests),
        (0, 0),
        "charge the actor, not the source model owner"
    );
    save(
        f,
        ModelTrafficLimits::default(),
        ModelTrafficLimits {
            max_concurrent_requests: Some(1),
            ..ModelTrafficLimits::default()
        },
        now,
    )
    .await;
    denied(
        f.reserve(
            &device,
            Source::Account,
            &input("other-tenant-call", 10),
            now,
        )
        .await,
        "account concurrent",
    );
    save(
        f,
        ModelTrafficLimits {
            max_concurrent_requests: Some(1),
            ..ModelTrafficLimits::default()
        },
        ModelTrafficLimits::default(),
        now,
    )
    .await;
    let mut tx = f.store.database().begin().await.unwrap();
    assert!(
        crate::model_traffic::check_admission(&mut tx, &f.other.user_id, now)
            .await
            .is_err()
    );
    tx.rollback().await.unwrap();
    let status = f
        .store
        .account_model_traffic(&f.owner, &f.owner.user_id, now + 60_001)
        .await
        .unwrap();
    assert_eq!((status.recent_requests, status.active_requests), (0, 0));
    let mut tx = f.store.database().begin().await.unwrap();
    assert!(
        f.store
            .renew_computer_model_request_in(&mut tx, &tenant, "traffic-computer", now + 60_001)
            .await
            .is_err(),
        "an expired lease must never resurrect after a replacement is admitted"
    );
    tx.rollback().await.unwrap();
    let admitted = f
        .reserve(
            &device,
            Source::Account,
            &input("after-lease-expiry", 10),
            now + 60_001,
        )
        .await
        .unwrap();
    super::complete(f, &admitted, now + 60_002).await;
    save(
        f,
        ModelTrafficLimits::default(),
        ModelTrafficLimits::default(),
        now + 60_003,
    )
    .await;
}

pub(super) async fn policy_lock(f: &Fixture, now: u64) {
    let mut admitted = f.store.database().begin().await.unwrap();
    crate::model_traffic::check_admission(&mut admitted, &f.owner.user_id, now)
        .await
        .unwrap();
    let edit = save(
        f,
        ModelTrafficLimits {
            max_concurrent_requests: Some(1),
            ..ModelTrafficLimits::default()
        },
        ModelTrafficLimits::default(),
        now,
    );
    tokio::pin!(edit);
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), &mut edit)
            .await
            .is_err(),
        "a policy edit waits until an admission holding the previous policy commits"
    );
    admitted.commit().await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), edit)
        .await
        .unwrap();
}
