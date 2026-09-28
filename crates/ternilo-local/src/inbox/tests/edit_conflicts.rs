use super::*;
use ternilo_protocol::ErrorCode;

fn edit(input: &str, expected_updated_at_ms: u64) -> QueueEditRequest {
    QueueEditRequest {
        input: input.to_owned(),
        expected_updated_at_ms,
    }
}

#[tokio::test]
async fn independent_connections_compete_on_one_revision_without_overwriting_the_winner() {
    let directory = tempfile::tempdir().unwrap();
    let first = test_store(directory.path().to_owned()).await;
    let second = test_store(directory.path().to_owned()).await;
    let submission = item("competing");
    first.enqueue("session", submission.clone()).await.unwrap();
    let (first_result, second_result) = tokio::join!(
        first.edit("session", &submission.id, edit("first", 10), 10),
        second.edit("session", &submission.id, edit("second", 10), 10),
    );
    let (winner, loser) = match (first_result, second_result) {
        (Ok(winner), Err(loser)) | (Err(loser), Ok(winner)) => (winner, loser),
        outcomes => panic!("exactly one edit must win: {outcomes:?}"),
    };
    assert_eq!(loser.code, ErrorCode::Conflict);
    assert_eq!(winner.updated_at_ms, 11);
    assert_eq!(winner.created_at_ms, submission.created_at_ms);
    assert_eq!(
        first.snapshot("session").await.unwrap().items.as_slice(),
        std::slice::from_ref(&winner)
    );
    let next = second
        .edit("session", &submission.id, edit("clock moved back", 11), 1)
        .await
        .unwrap();
    assert_eq!(next.updated_at_ms, 12);
    drop(first);
    drop(second);
    let reopened = test_store(directory.path().to_owned()).await;
    assert_eq!(
        reopened.snapshot("session").await.unwrap().items.as_slice(),
        std::slice::from_ref(&next)
    );
    let stale = reopened
        .edit(
            "session",
            &submission.id,
            edit("stale winner", winner.updated_at_ms),
            20,
        )
        .await
        .unwrap_err();
    assert_eq!(stale.code, ErrorCode::Conflict);
    assert_eq!(reopened.snapshot("session").await.unwrap().items, [next]);
}

#[tokio::test]
async fn placement_recovery_never_reuses_an_edit_revision_and_preserves_rejections() {
    let directory = tempfile::tempdir().unwrap();
    let store = test_store(directory.path().to_owned()).await;
    let submission = item("recovering");
    store.enqueue("session", submission.clone()).await.unwrap();
    let edited = store
        .edit("session", &submission.id, edit("edited", 10), 10)
        .await
        .unwrap();
    let batch = store.claim_batch("session", 1).await.unwrap();
    assert!(batch[0].updated_at_ms > edited.updated_at_ms);
    let running = store
        .edit("session", &submission.id, edit("running", 10), 1)
        .await
        .unwrap_err();
    assert_eq!(running.code, ErrorCode::InvalidInput);
    assert!(running.message.contains("only queued"));
    store.requeue("session", &submission.id, 1).await.unwrap();
    let requeued = store.snapshot("session").await.unwrap().items.remove(0);
    assert!(requeued.updated_at_ms > batch[0].updated_at_ms);
    store.claim_batch("session", 1).await.unwrap();
    store.settle_run("session", &[], false, 1).await.unwrap();
    let settled = store.snapshot("session").await.unwrap().items.remove(0);
    assert!(settled.updated_at_ms > requeued.updated_at_ms);
    store.claim_batch("session", 1).await.unwrap();
    store.recover("session", &[]).await.unwrap();
    let recovered = store.snapshot("session").await.unwrap().items.remove(0);
    assert!(recovered.updated_at_ms > settled.updated_at_ms);
    assert_eq!(
        store
            .edit(
                "session",
                &submission.id,
                edit("stale", edited.updated_at_ms),
                1
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    assert_eq!(recovered.content, edited.content);
    store
        .remove_queued("session", &submission.id)
        .await
        .unwrap();
    let removed = store
        .edit(
            "session",
            &submission.id,
            edit("deleted", recovered.updated_at_ms),
            1,
        )
        .await
        .unwrap_err();
    assert_eq!(removed.code, ErrorCode::InvalidInput);
    assert!(removed.message.contains("unknown submission"));
}
