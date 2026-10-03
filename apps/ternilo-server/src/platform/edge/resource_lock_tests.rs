use super::EdgeGateway;
use std::{task::Poll, time::Instant};
use ternilo_control::{ControlStore, SecretCipher};
use ternilo_protocol::TenantId;
use ternilo_transport::ExecutorId;

#[tokio::test]
async fn concurrent_discovery_shares_success_but_mutations_and_later_reads_refresh() {
    let store = ControlStore::connect("sqlite::memory:", None, SecretCipher::from_key([19; 32]), 1)
        .await
        .unwrap();
    let gateway = EdgeGateway::new(store.edge_store()).await.unwrap();
    let tenant = TenantId::new("tenant");
    let executor = ExecutorId::new("computer");
    let mut first = gateway.lock_discovery(&tenant, &executor).await.unwrap();
    let overlapping = gateway.lock_discovery(&tenant, &executor);
    tokio::pin!(overlapping);
    assert!(matches!(
        futures_util::poll!(&mut overlapping),
        Poll::Pending
    ));
    *first = Some(Instant::now());
    drop(first);
    assert!(overlapping.await.is_none());

    let first = gateway.lock_discovery(&tenant, &executor).await.unwrap();
    let failed = gateway.lock_discovery(&tenant, &executor);
    tokio::pin!(failed);
    assert!(matches!(futures_util::poll!(&mut failed), Poll::Pending));
    drop(first);
    assert!(
        failed.await.is_some(),
        "unsuccessful refreshes must be retried"
    );

    let mut first = gateway.lock_discovery(&tenant, &executor).await.unwrap();
    let mutation = gateway.lock_resources(&tenant, &executor);
    tokio::pin!(mutation);
    assert!(matches!(futures_util::poll!(&mut mutation), Poll::Pending));
    let after_mutation = gateway.lock_discovery(&tenant, &executor);
    tokio::pin!(after_mutation);
    assert!(matches!(
        futures_util::poll!(&mut after_mutation),
        Poll::Pending
    ));
    *first = Some(Instant::now());
    drop(first);
    drop(mutation.await);
    assert!(after_mutation.await.is_some());
}
