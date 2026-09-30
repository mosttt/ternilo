use super::*;
use ternilo_protocol::{AutomatedInputSource, UserId};

fn account(user_id: &str, username: &str) -> InputAuthor {
    InputAuthor::Account {
        user_id: UserId::new(user_id),
        username: username.to_owned(),
    }
}

#[tokio::test]
async fn account_batches_preserve_alice_bob_alice_fifo_after_reopening() {
    let directory = tempfile::tempdir().unwrap();
    let store = test_store(directory.path().to_owned()).await;
    for (id, author) in [
        ("alice-one", account("alice", "alice")),
        ("bob", account("bob", "bob")),
        ("alice-two", account("alice", "alice")),
    ] {
        store
            .enqueue("session", authored_item(id, author))
            .await
            .unwrap();
    }
    let batch = store.claim_batch("session", 11).await.unwrap();
    assert_eq!(batch.len(), 1);
    assert_eq!(batch[0].id, SubmissionId::new("alice-one"));
    let snapshot = store.snapshot("session").await.unwrap();
    assert_eq!(snapshot.items[0].placement, SubmissionPlacement::Running);
    for item in &snapshot.items[1..] {
        assert_eq!(item.placement, SubmissionPlacement::Queued);
        assert_eq!(item.updated_at_ms, 10);
    }
    store.close().await.unwrap();
    drop(store);
    let store = test_store(directory.path().to_owned()).await;
    assert_eq!(store.snapshot("session").await.unwrap(), snapshot);
    store
        .settle_run("session", &[SubmissionId::new("alice-one")], false, 12)
        .await
        .unwrap();
    for id in ["bob", "alice-two"] {
        let batch = store.claim_batch("session", 13).await.unwrap();
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].id, SubmissionId::new(id));
        store.finish("session", &batch[0].id).await.unwrap();
    }
    assert!(store.claim_batch("session", 14).await.unwrap().is_empty());
}

#[tokio::test]
async fn one_account_merges_consecutive_messages_despite_username_changes() {
    let directory = tempfile::tempdir().unwrap();
    let store = test_store(directory.path().to_owned()).await;
    for (id, author) in [
        ("alice-one", account("alice", "before-rename")),
        ("alice-two", account("alice", "after-rename")),
        ("another-account", account("bob", "after-rename")),
    ] {
        store
            .enqueue("session", authored_item(id, author))
            .await
            .unwrap();
    }
    let batch = store.claim_batch("session", 11).await.unwrap();
    assert_eq!(
        batch
            .iter()
            .map(|item| item.id.as_str())
            .collect::<Vec<_>>(),
        ["alice-one", "alice-two"]
    );
    assert_eq!(
        batch[0].provenance.as_ref().unwrap().author,
        account("alice", "before-rename")
    );
    assert_eq!(
        batch[1].provenance.as_ref().unwrap().author,
        account("alice", "after-rename")
    );
    let snapshot = store.snapshot("session").await.unwrap();
    assert_eq!(snapshot.items[2].placement, SubmissionPlacement::Queued);
    assert_eq!(snapshot.items[2].updated_at_ms, 10);
}

#[tokio::test]
async fn local_and_account_inputs_keep_separate_consecutive_batches() {
    let directory = tempfile::tempdir().unwrap();
    let store = test_store(directory.path().to_owned()).await;
    for (id, author) in [
        ("local-one", InputAuthor::Local),
        ("local-two", InputAuthor::Local),
        ("account-one", account("alice", "alice")),
        ("account-two", account("alice", "alice")),
        ("local-three", InputAuthor::Local),
    ] {
        store
            .enqueue("session", authored_item(id, author))
            .await
            .unwrap();
    }
    for expected in [
        vec!["local-one", "local-two"],
        vec!["account-one", "account-two"],
        vec!["local-three"],
    ] {
        let batch = store.claim_batch("session", 11).await.unwrap();
        assert_eq!(
            batch
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            expected
        );
        for item in batch {
            store.finish("session", &item.id).await.unwrap();
        }
    }
    assert!(store.snapshot("session").await.unwrap().items.is_empty());
}

#[tokio::test]
async fn missing_authors_and_automation_categories_do_not_establish_shared_authorship() {
    let directory = tempfile::tempdir().unwrap();
    let store = test_store(directory.path().to_owned()).await;
    let schedule = InputAuthor::Automation {
        source: AutomatedInputSource::Schedule,
    };
    let subagent = InputAuthor::Automation {
        source: AutomatedInputSource::Subagent,
    };
    let inputs = [
        authored_item("local", InputAuthor::Local),
        item("unknown-one"),
        item("unknown-two"),
        authored_item("account", account("alice", "alice")),
        authored_item("schedule-one", schedule.clone()),
        authored_item("schedule-two", schedule),
        authored_item("subagent-one", subagent.clone()),
        authored_item("subagent-two", subagent),
        authored_item("last-local", InputAuthor::Local),
    ];
    for input in &inputs {
        store.enqueue("session", input.clone()).await.unwrap();
    }
    for expected in inputs {
        let batch = store.claim_batch("session", 11).await.unwrap();
        assert_eq!(batch.len(), 1, "separate authorship for {}", expected.id);
        assert_eq!(batch[0].id, expected.id);
        assert_eq!(batch[0].provenance, expected.provenance);
        store.finish("session", &expected.id).await.unwrap();
    }
    assert!(store.snapshot("session").await.unwrap().items.is_empty());
}
