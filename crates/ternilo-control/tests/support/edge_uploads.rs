use ternilo_control::ControlStore;
use ternilo_protocol::{
    AcceptedUploadChange, AcceptedUploadChangeKind, AcceptedUploadMetadata, RunId, SessionId,
    SubmissionId,
};
use ternilo_transport::{AcceptedUploadBatch, ExecutorScope};

use super::{EdgeFixture, MappingFixture};

#[expect(
    clippy::too_many_lines,
    reason = "Exercise durable upload replication, ordering, canonical mapping and isolation as one lifecycle."
)]
pub(super) async fn contract(store: &ControlStore, fixture: &EdgeFixture) {
    let mapping = MappingFixture {
        browser_session: SessionId::new("upload-browser-session"),
        node_session: SessionId::new("upload-node-session"),
    };
    store
        .create_edge_session_mapping(
            &fixture.alice,
            &fixture.tenant_a,
            &fixture.workspace_a,
            &fixture.executor_a,
            &mapping.node_session,
            Some(&mapping.browser_session),
            super::metadata("Upload lifecycle", None, fixture.now + 600),
            fixture.now + 600,
        )
        .await
        .unwrap();
    let edge = store.edge_store();
    let scope = ExecutorScope {
        tenant_id: fixture.tenant_a.clone(),
        user_id: fixture.alice.user_id.clone(),
    };
    let stream = "1234567890abcdef1234567890abcdef";
    assert_eq!(
        edge.begin_upload_sync(&fixture.executor_a, &scope, stream)
            .await
            .unwrap(),
        None
    );
    let batch = AcceptedUploadBatch {
        scope: scope.clone(),
        stream_id: stream.to_owned(),
        after_seq: None,
        changes: (1..=200)
            .map(|seq| accepted(seq, &mapping.node_session))
            .collect(),
    };
    assert_eq!(
        edge.merge_uploads(&fixture.executor_a, &batch)
            .await
            .unwrap(),
        Some(200)
    );
    assert_eq!(
        edge.merge_uploads(&fixture.executor_a, &batch)
            .await
            .unwrap(),
        Some(200)
    );
    assert_eq!(
        edge.begin_upload_sync(&fixture.executor_a, &scope, stream)
            .await
            .unwrap(),
        Some(200)
    );
    let mut transaction = store
        .database()
        .tenant_transaction(&scope.tenant_id)
        .await
        .unwrap();
    let count = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM control_edge_session_uploads u JOIN control_edge_sessions s
         ON s.tenant_id=u.tenant_id AND s.executor_id=u.executor_id AND s.node_session_id=u.session_id
         WHERE s.tenant_id=$1 AND s.session_id=$2",
    ).bind(scope.tenant_id.as_str()).bind(mapping.browser_session.as_str())
        .fetch_one(&mut *transaction).await.unwrap();
    assert_eq!(count, 200);
    transaction.commit().await.unwrap();

    // A partial failure must roll back both previously inserted rows and cursor.
    let invalid = AcceptedUploadBatch {
        scope: scope.clone(),
        stream_id: stream.to_owned(),
        after_seq: Some(200),
        changes: vec![
            accepted(201, &mapping.node_session),
            AcceptedUploadChange {
                seq: 202,
                ..accepted(1, &mapping.node_session)
            },
        ],
    };
    assert!(
        edge.merge_uploads(&fixture.executor_a, &invalid)
            .await
            .is_err()
    );
    assert_eq!(
        edge.begin_upload_sync(&fixture.executor_a, &scope, stream)
            .await
            .unwrap(),
        Some(200)
    );
    assert_eq!(count_uploads(store, &scope, &fixture.executor_a).await, 200);
    let gap = AcceptedUploadBatch {
        scope: scope.clone(),
        stream_id: stream.to_owned(),
        after_seq: Some(201),
        changes: vec![accepted(202, &mapping.node_session)],
    };
    assert!(edge.merge_uploads(&fixture.executor_a, &gap).await.is_err());
    let mut oversized = batch.clone();
    oversized.changes.push(accepted(201, &mapping.node_session));
    assert!(
        edge.merge_uploads(&fixture.executor_a, &oversized)
            .await
            .is_err()
    );
    assert!(
        edge.begin_upload_sync(
            &fixture.executor_a,
            &scope,
            "abcdef1234567890abcdef1234567890"
        )
        .await
        .is_err()
    );
    let other_user = ExecutorScope {
        user_id: fixture.bob.user_id.clone(),
        ..scope.clone()
    };
    assert!(
        edge.begin_upload_sync(&fixture.executor_a, &other_user, stream)
            .await
            .is_err()
    );
    let other_tenant = ExecutorScope {
        tenant_id: fixture.tenant_b.clone(),
        ..scope.clone()
    };
    assert!(
        edge.begin_upload_sync(&fixture.executor_a, &other_tenant, stream)
            .await
            .is_err()
    );

    // Unmapped metadata can arrive before discovery, but no public session joins it.
    let unmapped = AcceptedUploadBatch {
        scope: scope.clone(),
        stream_id: stream.to_owned(),
        after_seq: Some(200),
        changes: vec![accepted(201, &SessionId::new("private-session"))],
    };
    edge.merge_uploads(&fixture.executor_a, &unmapped)
        .await
        .unwrap();
    let mut transaction = store
        .database()
        .tenant_transaction(&scope.tenant_id)
        .await
        .unwrap();
    let public = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM control_edge_session_uploads u JOIN control_edge_sessions s
         ON s.tenant_id=u.tenant_id AND s.executor_id=u.executor_id AND s.node_session_id=u.session_id
         WHERE u.tenant_id=$1 AND u.submission_id='upload-201'",
    ).bind(scope.tenant_id.as_str()).fetch_one(&mut *transaction).await.unwrap();
    assert_eq!(public, 0);
    transaction.commit().await.unwrap();

    let deletion = AcceptedUploadBatch {
        scope: scope.clone(),
        stream_id: stream.to_owned(),
        after_seq: Some(201),
        changes: vec![AcceptedUploadChange {
            seq: 202,
            session_id: mapping.node_session.clone(),
            kind: AcceptedUploadChangeKind::SessionDeleted,
        }],
    };
    edge.merge_uploads(&fixture.executor_a, &deletion)
        .await
        .unwrap();
    assert!(
        store
            .find_owned_edge_session(&fixture.alice, &scope.tenant_id, &mapping.browser_session)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .create_edge_session_mapping(
                &fixture.alice,
                &scope.tenant_id,
                &fixture.workspace_a,
                &fixture.executor_a,
                &mapping.node_session,
                Some(&mapping.browser_session),
                super::metadata("Stale mapping", None, fixture.now + 650),
                fixture.now + 650,
            )
            .await
            .is_err()
    );
    assert_eq!(count_uploads(store, &scope, &fixture.executor_a).await, 1);
    assert_eq!(
        edge.merge_uploads(&fixture.executor_a, &batch)
            .await
            .unwrap(),
        Some(202)
    );
    assert_eq!(count_uploads(store, &scope, &fixture.executor_a).await, 1);

    // Delayed events cannot reintroduce file metadata after the tombstone.
    edge.merge_events(
        &scope.tenant_id,
        &fixture.executor_a,
        &mapping.node_session,
        &[ternilo_protocol::SessionEvent {
            seq: 0,
            occurred_at_ms: fixture.now,
            run_id: RunId::new("old-run"),
            kind: ternilo_protocol::SessionEventKind::SessionTitleGenerated {
                title: "stale".to_owned(),
            },
        }],
    )
    .await
    .unwrap();
    assert!(
        edge.events(&scope.tenant_id, &fixture.executor_a, &mapping.node_session)
            .await
            .unwrap()
            .is_empty()
    );
    let repeated_identity = AcceptedUploadBatch {
        scope: scope.clone(),
        stream_id: stream.to_owned(),
        after_seq: Some(202),
        changes: vec![accepted(203, &mapping.node_session)],
    };
    assert!(
        edge.merge_uploads(&fixture.executor_a, &repeated_identity)
            .await
            .is_err()
    );
    store
        .delete_edge_session_mapping(
            &fixture.alice,
            &scope.tenant_id,
            &mapping.browser_session,
            &mapping.node_session,
            fixture.now + 650,
        )
        .await
        .unwrap();

    let project = store
        .list_projects(&fixture.alice, &scope.tenant_id)
        .await
        .unwrap()
        .remove(0);
    let revoked = super::enroll(
        store,
        &fixture.alice,
        &scope.tenant_id,
        &project.project_id,
        "upload-revoked",
        fixture.now + 700,
    )
    .await;
    edge.begin_upload_sync(&revoked, &scope, stream)
        .await
        .unwrap();
    store
        .revoke_executor(
            &fixture.alice,
            &scope.tenant_id,
            &revoked,
            fixture.now + 701,
        )
        .await
        .unwrap();
    assert!(edge.merge_uploads(&revoked, &batch).await.is_err());
    assert!(
        edge.begin_upload_sync(&revoked, &scope, stream)
            .await
            .is_err()
    );
}

fn accepted(seq: u64, session_id: &SessionId) -> AcceptedUploadChange {
    AcceptedUploadChange {
        seq,
        session_id: session_id.clone(),
        kind: AcceptedUploadChangeKind::UploadAccepted {
            upload: AcceptedUploadMetadata {
                submission_id: SubmissionId::new(format!("upload-{seq}")),
                attachment_index: 0,
                created_at_ms: 1_900_000_000_000 + seq,
                submitted_run_id: RunId::new(format!("run-{seq}")),
                name: "queued.txt".to_owned(),
                media_type: "text/plain".to_owned(),
            },
        },
    }
}

async fn count_uploads(
    store: &ControlStore,
    scope: &ExecutorScope,
    executor: &ternilo_transport::ExecutorId,
) -> i64 {
    let mut transaction = store
        .database()
        .tenant_transaction(&scope.tenant_id)
        .await
        .unwrap();
    let count = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM control_edge_session_uploads WHERE tenant_id=$1 AND executor_id=$2",
    )
    .bind(scope.tenant_id.as_str())
    .bind(executor.as_str())
    .fetch_one(&mut *transaction)
    .await
    .unwrap();
    transaction.commit().await.unwrap();
    count
}

pub(super) async fn assert_reopened(store: &ControlStore, fixture: &EdgeFixture) {
    let scope = ExecutorScope {
        tenant_id: fixture.tenant_a.clone(),
        user_id: fixture.alice.user_id.clone(),
    };
    assert_eq!(
        store
            .edge_store()
            .begin_upload_sync(
                &fixture.executor_a,
                &scope,
                "1234567890abcdef1234567890abcdef",
            )
            .await
            .unwrap(),
        Some(202)
    );
    assert_eq!(count_uploads(store, &scope, &fixture.executor_a).await, 1);
    let mut transaction = store
        .database()
        .tenant_transaction(&scope.tenant_id)
        .await
        .unwrap();
    let deleted = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM control_edge_deleted_sessions WHERE tenant_id=$1 AND executor_id=$2 AND session_id='upload-node-session'",
    ).bind(scope.tenant_id.as_str()).bind(fixture.executor_a.as_str()).fetch_one(&mut *transaction).await.unwrap();
    assert_eq!(deleted, 1);
    transaction.commit().await.unwrap();
}
