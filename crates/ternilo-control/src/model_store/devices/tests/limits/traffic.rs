use super::{Fixture, Source, input, sqlite};
use crate::{
    ControlStore, ModelAccessError, ModelAccessErrorKind, ModelKeyInput, ModelRequestPermit,
    ModelTrafficLimits, ModelTrafficPolicy, SecretCipher,
};
use sqlx::Executor;

mod visibility;

#[tokio::test]
async fn sqlite_layered_traffic_limits_share_accounts_and_preserve_accepted_windows() {
    let (_directory, fixture) = sqlite().await;
    Box::pin(contract(&fixture)).await;
}

#[tokio::test]
#[ignore = "requires TERNILO_TEST_DATABASE_URL pointing to a disposable PostgreSQL database"]
async fn postgres_layered_traffic_limits_use_shared_policy_locks_and_runtime_rls() {
    let url = std::env::var("TERNILO_TEST_DATABASE_URL").unwrap();
    assert!(url.contains("ternilo_control_test"));
    let admin = sqlx::PgPool::connect(&url).await.unwrap();
    admin
        .execute("DROP SCHEMA IF EXISTS public CASCADE")
        .await
        .unwrap();
    admin.execute("CREATE SCHEMA public").await.unwrap();
    crate::postgres_test::prepare_role(&admin, "ternilo_traffic_test", "traffic-test-password")
        .await;
    let mut runtime = url
        .parse::<sqlx::any::AnyConnectOptions>()
        .unwrap()
        .database_url;
    runtime.set_username("ternilo_traffic_test").unwrap();
    runtime.set_password(Some("traffic-test-password")).unwrap();
    let store = ControlStore::connect(
        runtime.as_str(),
        Some(&url),
        SecretCipher::from_key([73; 32]),
        8,
    )
    .await
    .unwrap();
    let fixture = Fixture::new(store).await;
    Box::pin(contract(&fixture)).await;
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM control_model_traffic_policy")
        .fetch_one(fixture.store.database().pool())
        .await
        .unwrap();
    assert_eq!(
        count, 0,
        "model traffic policy must not escape its transaction scope"
    );
    assert!(
        sqlx::query("SELECT * FROM ternilo_computer_traffic_counts('untrusted',0,0,true)")
            .fetch_all(fixture.store.database().pool())
            .await
            .is_err()
    );
    fixture.store.database().close().await;
    admin.close().await;
}

async fn save(
    f: &Fixture,
    platform: ModelTrafficLimits,
    account_default: ModelTrafficLimits,
    now: u64,
) {
    let current = f.store.model_traffic_policy(&f.admin).await.unwrap();
    f.store
        .update_model_traffic_policy(
            &f.admin,
            current.revision,
            &ModelTrafficPolicy {
                platform,
                account_default,
            },
            now,
        )
        .await
        .unwrap();
}
async fn complete(f: &Fixture, permit: &ModelRequestPermit, now: u64) {
    f.store
        .mark_model_request_attempted(&permit.request.request_id, now)
        .await
        .unwrap();
    f.settle(&permit.request.request_id, Some(1), now).await;
}
fn denied(result: Result<ModelRequestPermit, ModelAccessError>, scope: &str) {
    let error = result.err().expect("request should be limited");
    assert!(matches!(
        error.kind,
        ModelAccessErrorKind::RateLimited {
            retry_after_seconds: 1..=60
        }
    ));
    assert!(error.error.message.contains(scope), "{error}");
}

#[expect(
    clippy::too_many_lines,
    reason = "Exercise authorization, per-account inheritance, idempotency and rolling-window transitions together."
)]
async fn contract(f: &Fixture) {
    let now = super::super::NOW + 10_000;
    let (first, _) = f
        .connect(&ternilo_protocol::ModelDeviceLimits::default(), now - 6000)
        .await;
    let (second, _) = f
        .connect(&ternilo_protocol::ModelDeviceLimits::default(), now - 6000)
        .await;
    assert!(f.store.model_traffic_policy(&f.owner).await.is_err());
    assert!(
        f.store
            .account_model_traffic(&f.other, &f.owner.user_id, now)
            .await
            .is_err()
    );
    assert!(
        f.store
            .update_model_traffic_policy(&f.owner, 0, &ModelTrafficPolicy::default(), now)
            .await
            .is_err()
    );
    assert!(
        f.store
            .update_model_traffic_policy(
                &f.admin,
                0,
                &ModelTrafficPolicy {
                    platform: ModelTrafficLimits {
                        requests_per_minute: Some(0),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                now
            )
            .await
            .is_err()
    );
    let defaults = ModelTrafficLimits {
        requests_per_minute: Some(2),
        max_concurrent_requests: Some(1),
    };
    save(f, ModelTrafficLimits::default(), defaults.clone(), now).await;
    assert!(
        f.store
            .update_model_traffic_policy(&f.admin, 0, &ModelTrafficPolicy::default(), now)
            .await
            .is_err()
    );
    let one = f
        .reserve(&first, Source::First, &input("traffic-one", 10), now)
        .await
        .unwrap();
    assert!(
        !f.reserve(&first, Source::First, &input("traffic-one", 10), now + 1)
            .await
            .unwrap()
            .newly_accepted
    );
    denied(
        f.reserve(&second, Source::Account, &input("traffic-two", 10), now + 1)
            .await,
        "account concurrent",
    );
    complete(f, &one, now + 1).await;
    let two = f
        .reserve(&second, Source::Account, &input("traffic-two", 10), now + 2)
        .await
        .unwrap();
    complete(f, &two, now + 3).await;
    denied(
        f.reserve(&first, Source::Second, &input("traffic-three", 10), now + 4)
            .await,
        "account model request rate",
    );
    assert!(
        !f.reserve(&second, Source::Account, &input("traffic-two", 10), now + 4)
            .await
            .unwrap()
            .newly_accepted
    );
    let status = f
        .store
        .account_model_traffic(&f.owner, &f.owner.user_id, now + 4)
        .await
        .unwrap();
    assert_eq!((status.recent_requests, status.active_requests), (2, 0));
    assert_eq!(status.effective, defaults);
    assert!(
        f.store
            .update_account_model_traffic(&f.owner, &f.owner.user_id, 0, None, now + 4)
            .await
            .is_err()
    );
    f.store
        .update_account_model_traffic(
            &f.admin,
            &f.owner.user_id,
            0,
            Some(&ModelTrafficLimits {
                max_concurrent_requests: Some(1),
                ..Default::default()
            }),
            now + 4,
        )
        .await
        .unwrap();
    let three = f
        .reserve(&first, Source::Second, &input("traffic-three", 10), now + 4)
        .await
        .unwrap();
    complete(f, &three, now + 5).await;
    f.store
        .update_account_model_traffic(&f.admin, &f.owner.user_id, 1, None, now + 5)
        .await
        .unwrap();
    assert!(
        f.store
            .update_account_model_traffic(&f.admin, &f.owner.user_id, 1, None, now + 5)
            .await
            .is_err()
    );
    denied(
        f.reserve(
            &first,
            Source::First,
            &input("traffic-window", 10),
            now + 60_000,
        )
        .await,
        "account model request rate",
    );
    let next = f
        .reserve(
            &first,
            Source::First,
            &input("traffic-window", 10),
            now + 60_002,
        )
        .await
        .unwrap();
    complete(f, &next, now + 60_003).await;
    Box::pin(global_competition(f, &first, now + 120_000)).await;
    Box::pin(visibility::contract(f, now + 300_000)).await;
    visibility::policy_lock(f, now + 500_000).await;
}

async fn global_competition(f: &Fixture, device: &str, now: u64) {
    let grant = super::fixture::grant(&f.store, &f.admin, &f.other, "Other traffic account").await;
    let key = f
        .store
        .create_model_key(
            &f.other,
            &ModelKeyInput {
                name: "Traffic key".into(),
                grant_id: grant,
                model_ids: vec!["model".into()],
                monthly_tokens: None,
                max_concurrent_requests: None,
                expires_at_ms: None,
            },
            now,
        )
        .await
        .unwrap();
    save(
        f,
        ModelTrafficLimits {
            max_concurrent_requests: Some(1),
            ..Default::default()
        },
        ModelTrafficLimits::default(),
        now,
    )
    .await;
    let a = input("traffic-compete-a", 10);
    let b = input("traffic-compete-b", 10);
    let (a, b) = tokio::join!(
        f.reserve(device, Source::Account, &a, now),
        f.store.reserve_model_request(&key.token, &b, now)
    );
    let mut accepted = None;
    for result in [a, b] {
        match result {
            Ok(value) => {
                assert!(accepted.replace(value).is_none());
            }
            Err(error) => denied(Err(error), "platform concurrent"),
        }
    }
    let accepted = accepted.expect("one competitor enters");
    complete(f, &accepted, now + 1).await;
    let after = f
        .store
        .reserve_model_request(&key.token, &input("traffic-after-completion", 10), now + 2)
        .await
        .unwrap();
    complete(f, &after, now + 3).await;
    let now = now + 60_000;
    save(
        f,
        ModelTrafficLimits {
            requests_per_minute: Some(1),
            ..Default::default()
        },
        ModelTrafficLimits::default(),
        now,
    )
    .await;
    denied(
        f.reserve(
            device,
            Source::Account,
            &input("traffic-global-window", 10),
            now,
        )
        .await,
        "platform model request rate",
    );
    let next = f
        .reserve(
            device,
            Source::Account,
            &input("traffic-global-window", 10),
            now + 2,
        )
        .await
        .unwrap();
    complete(f, &next, now + 3).await;
    denied(
        f.store
            .reserve_model_request(&key.token, &input("traffic-other-account", 10), now + 3)
            .await,
        "platform model request rate",
    );
    save(
        f,
        ModelTrafficLimits::default(),
        ModelTrafficLimits::default(),
        now + 3,
    )
    .await;
}
