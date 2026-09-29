use std::{sync::Arc, time::Duration};

use ternilo_local::{LocalApplication, LocalInvalidationCategory};
use ternilo_protocol::HarnessError;
use ternilo_transport::{AcceptedUploadBatch, ExecutorFrame, ExecutorScope};
use tokio::sync::{broadcast, mpsc};

type Acknowledgement = (String, Option<u64>);

pub(super) async fn pump(
    application: Arc<LocalApplication>,
    scope: ExecutorScope,
    outgoing: mpsc::Sender<ExecutorFrame>,
    mut acknowledgements: mpsc::Receiver<Acknowledgement>,
) {
    if let Err(error) = synchronize(application, scope, outgoing, &mut acknowledgements).await {
        eprintln!("synchronize accepted uploads: {error}");
    }
}

async fn synchronize(
    application: Arc<LocalApplication>,
    scope: ExecutorScope,
    outgoing: mpsc::Sender<ExecutorFrame>,
    acknowledgements: &mut mpsc::Receiver<Acknowledgement>,
) -> Result<(), HarnessError> {
    // Subscribe before reading the ledger so an acceptance between catch-up and
    // waiting cannot be missed. Lag recovery reads the same durable sequence.
    let mut notifications = application.subscribe_invalidations();
    let stream_id = application.accepted_upload_stream_id().to_owned();
    let mut cursor = acknowledge(acknowledgements, &stream_id).await?;
    loop {
        let changes = application.accepted_upload_changes(cursor, 200).await?;
        if let Some(last) = changes.last().map(|change| change.seq) {
            let batch = AcceptedUploadBatch {
                scope: scope.clone(),
                stream_id: stream_id.clone(),
                after_seq: cursor,
                changes,
            };
            batch.validate()?;
            outgoing
                .send(ExecutorFrame::AcceptedUploads { batch })
                .await
                .map_err(|_| HarnessError::execution("gateway upload channel closed"))?;
            let acknowledged = acknowledge(acknowledgements, &stream_id).await?;
            if acknowledged != Some(last) {
                return Err(HarnessError::invalid(
                    "gateway acknowledged an unexpected upload sequence",
                ));
            }
            cursor = acknowledged;
            continue;
        }
        loop {
            match notifications.recv().await {
                Ok(notification)
                    if matches!(
                        notification.category,
                        LocalInvalidationCategory::Inbox | LocalInvalidationCategory::Workbench
                    ) =>
                {
                    break;
                }
                Err(broadcast::error::RecvError::Lagged(_)) => break,
                Err(broadcast::error::RecvError::Closed) => return Ok(()),
                Ok(_) => {}
            }
        }
    }
}

async fn acknowledge(
    acknowledgements: &mut mpsc::Receiver<Acknowledgement>,
    expected_stream: &str,
) -> Result<Option<u64>, HarnessError> {
    let (stream_id, sequence) =
        tokio::time::timeout(Duration::from_secs(30), acknowledgements.recv())
            .await
            .map_err(|_| HarnessError::execution("gateway did not acknowledge the upload delta"))?
            .ok_or_else(|| {
                HarnessError::execution("gateway upload acknowledgement channel closed")
            })?;
    if stream_id != expected_stream {
        return Err(HarnessError::invalid(
            "gateway acknowledged a different upload data directory",
        ));
    }
    Ok(sequence)
}

#[cfg(test)]
mod tests {
    use ternilo_kernel::HostPolicy;
    use ternilo_protocol::{
        Attachment, RunLimits, SessionSubmissionRequest, SubmissionContent, SubmissionDelivery,
        TenantId, UserId,
    };

    use super::*;

    #[tokio::test]
    #[expect(
        clippy::too_many_lines,
        reason = "Exercise actual local acceptance, backpressure, lost acknowledgement, reconnect and deletion in one transport lifecycle."
    )]
    async fn upload_sync_is_bounded_resumes_committed_metadata_and_keeps_contents_local() {
        let data = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let application = Arc::new(
            LocalApplication::open(
                ternilo_local::catalog().unwrap(),
                ternilo_local::local_profile(),
                HostPolicy::local(RunLimits::default()),
                data.path().to_path_buf(),
            )
            .await
            .unwrap(),
        );
        let workspace = application
            .add_workspace(workspace.path().to_str().unwrap())
            .await
            .unwrap();
        let session = application
            .create_session(workspace.workspace_id, None, None)
            .await
            .unwrap();
        for _ in 0..21 {
            application
                .submit_session(
                    session.identity.session_id.as_str(),
                    SessionSubmissionRequest {
                        delivery: SubmissionDelivery::Queue,
                        run_id: None,
                        content: SubmissionContent::Prompt {
                            input: "/code \"accepted\"".to_owned(),
                        },
                        references: vec![],
                        attachments: (0..10)
                            .map(|index| Attachment {
                                name: format!("document-{index}.txt"),
                                media_type: "text/plain".to_owned(),
                                content: "private file body stays on the computer".to_owned(),
                            })
                            .collect(),
                    },
                )
                .await
                .unwrap();
        }
        application.close().await.unwrap();
        drop(application);
        let application = Arc::new(
            LocalApplication::open(
                ternilo_local::catalog().unwrap(),
                ternilo_local::local_profile(),
                HostPolicy::local(RunLimits::default()),
                data.path().to_path_buf(),
            )
            .await
            .unwrap(),
        );
        let scope = ExecutorScope {
            tenant_id: TenantId::new("tenant"),
            user_id: UserId::new("owner"),
        };
        let stream_id = application.accepted_upload_stream_id().to_owned();
        let (outgoing, mut frames) = mpsc::channel(4);
        let (acks, receiver) = mpsc::channel(2);
        let first = tokio::spawn(pump(
            Arc::clone(&application),
            scope.clone(),
            outgoing,
            receiver,
        ));
        acks.send((stream_id.clone(), None)).await.unwrap();
        let first_frame = frame(&mut frames).await;
        let wire = serde_json::to_string(&first_frame).unwrap();
        assert!(!wire.contains("private file body"));
        assert!(!wire.contains("attachment://"));
        assert!(!wire.contains("sha256"));
        assert!(!wire.contains("\"content\""));
        let ExecutorFrame::AcceptedUploads { batch } = first_frame else {
            panic!("expected upload batch")
        };
        assert_eq!(batch.changes.len(), 200);
        assert_eq!(batch.after_seq, None);
        assert_eq!(batch.changes.last().unwrap().seq, 200);
        // The next page cannot be sent until the server commits this page.
        assert!(
            tokio::time::timeout(Duration::from_millis(50), frames.recv())
                .await
                .is_err()
        );
        first.abort();
        let _ = first.await;
        drop(acks);

        let (outgoing, mut frames) = mpsc::channel(4);
        let (acks, receiver) = mpsc::channel(2);
        let resumed = tokio::spawn(pump(Arc::clone(&application), scope, outgoing, receiver));
        // Server persisted the previous batch even though its ack was lost.
        acks.send((stream_id.clone(), Some(200))).await.unwrap();
        let ExecutorFrame::AcceptedUploads { batch } = frame(&mut frames).await else {
            panic!("expected resumed batch")
        };
        assert_eq!(batch.after_seq, Some(200));
        assert_eq!(batch.changes.len(), 10);
        acks.send((stream_id.clone(), Some(210))).await.unwrap();
        application
            .delete_session(session.identity.session_id.as_str())
            .await
            .unwrap();
        let ExecutorFrame::AcceptedUploads { batch } = frame(&mut frames).await else {
            panic!("expected deletion")
        };
        assert_eq!(batch.after_seq, Some(210));
        assert_eq!(batch.changes.len(), 1);
        assert!(matches!(
            batch.changes[0].kind,
            ternilo_protocol::AcceptedUploadChangeKind::SessionDeleted
        ));
        acks.send((stream_id.clone(), Some(211))).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(50), frames.recv())
                .await
                .is_err()
        );
        resumed.abort();
        let _ = resumed.await;
        application.close().await.unwrap();
        drop(application);
        let reopened = LocalApplication::open(
            ternilo_local::catalog().unwrap(),
            ternilo_local::local_profile(),
            HostPolicy::local(RunLimits::default()),
            data.path().to_path_buf(),
        )
        .await
        .unwrap();
        assert_eq!(reopened.accepted_upload_stream_id(), stream_id);
        assert!(
            reopened
                .accepted_upload_changes(Some(211), 200)
                .await
                .unwrap()
                .is_empty()
        );
        reopened.close().await.unwrap();
    }

    async fn frame(receiver: &mut mpsc::Receiver<ExecutorFrame>) -> ExecutorFrame {
        tokio::time::timeout(Duration::from_secs(5), receiver.recv())
            .await
            .unwrap()
            .unwrap()
    }
}
