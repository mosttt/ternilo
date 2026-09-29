use std::{net::SocketAddr, path::Path, sync::Arc, time::Duration};

use ternilo_kernel::HostPolicy;
use ternilo_protocol::{
    RunId, RunLimits, SessionEventKind, SessionSubmissionRequest, SubmissionContent,
    SubmissionDelivery, SubmissionPlacement, UserMessageSource,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::mpsc,
    task::JoinHandle,
};

use super::{LocalApplication, ModelSelection};

struct SlowModel {
    address: SocketAddr,
    requests: mpsc::Receiver<()>,
    task: JoinHandle<()>,
}

impl SlowModel {
    async fn start(completed_requests: usize) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (requests, received) = mpsc::channel(4);
        let task = tokio::spawn(async move {
            for index in 0..=completed_requests {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = vec![0; 64 * 1024];
                assert!(stream.read(&mut request).await.unwrap() > 0);
                requests.send(()).await.unwrap();
                if index == completed_requests {
                    // Keep the socket open without sending a response. Shutdown
                    // must cancel this request instead of waiting for its timeout.
                    std::future::pending::<()>().await;
                }
                let body = concat!(
                    "data: {\"choices\":[{\"delta\":{\"content\":\"Completed answer\"}}]}\n\n",
                    "data: [DONE]\n\n"
                );
                stream
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                            body.len()
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
            }
        });
        Self {
            address,
            requests: received,
            task,
        }
    }

    async fn wait_for_request(&mut self) {
        tokio::time::timeout(Duration::from_secs(5), self.requests.recv())
            .await
            .expect("model request started")
            .expect("model server remains alive");
    }
}

impl Drop for SlowModel {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn open_application(data_dir: &Path) -> Arc<LocalApplication> {
    Arc::new(
        LocalApplication::open(
            crate::catalog().unwrap(),
            crate::local_profile(),
            HostPolicy::local(RunLimits::default()),
            data_dir.to_owned(),
        )
        .await
        .unwrap(),
    )
}

async fn setup(root: &Path, model: &SlowModel) -> (Arc<LocalApplication>, String) {
    let workspace = root.join("workspace");
    tokio::fs::create_dir(&workspace).await.unwrap();
    let application = open_application(&root.join("data")).await;
    let workspace = application
        .add_workspace(workspace.to_str().unwrap())
        .await
        .unwrap();
    let session = application
        .create_session(workspace.workspace_id, None, None)
        .await
        .unwrap();
    let session_id = session.identity.session_id.as_str().to_owned();
    application
        .update_model(
            &session_id,
            ModelSelection::OpenAiCompatible {
                base_url: format!("http://{}/v1", model.address),
                model: "slow-model".to_owned(),
                api_key_env: None,
                timeout_ms: 60_000,
                max_attempts: 1,
                retry_base_delay_ms: 10,
            },
        )
        .await
        .unwrap();
    (application, session_id)
}

fn submission(input: &str) -> SessionSubmissionRequest {
    SessionSubmissionRequest {
        delivery: SubmissionDelivery::Queue,
        run_id: None,
        content: SubmissionContent::Prompt {
            input: input.to_owned(),
        },
        references: Vec::new(),
        attachments: Vec::new(),
    }
}

#[tokio::test]
async fn shutdown_cancels_slow_run_joins_fifo_drivers_and_preserves_pending_occurrences() {
    let root = tempfile::tempdir().unwrap();
    let mut model = SlowModel::start(0).await;
    let (application, session_id) = setup(root.path(), &model).await;
    let active = application
        .submit_session(&session_id, submission("slow active request"))
        .await
        .unwrap();
    model.wait_for_request().await;
    let queued = application
        .submit_session(&session_id, submission("queued request"))
        .await
        .unwrap();

    tokio::time::timeout(Duration::from_secs(5), application.shutdown())
        .await
        .expect("shutdown cancels the provider before waiting for the turn gate")
        .unwrap();
    assert!(application.live.read().await.is_empty());
    assert_eq!(
        Arc::strong_count(&application),
        1,
        "shutdown joins every FIFO driver that owns the application data lock"
    );
    assert!(
        application
            .submit_session(&session_id, submission("too late"))
            .await
            .unwrap_err()
            .is_cancelled()
    );
    drop(application);

    let restored = open_application(&root.path().join("data")).await;
    let inbox = restored.session_inbox(&session_id).await.unwrap();
    assert!(inbox.paused, "cancellation pause survives restart");
    assert_eq!(inbox.items.len(), 1);
    assert_eq!(inbox.items[0].placement, SubmissionPlacement::Queued);
    assert_eq!(inbox.items[0].id, queued.id);
    let resumed_model = super::test_model::TestModel::start().await;
    resumed_model.install(&restored, &session_id).await.unwrap();
    let wake = restored
        .submit_session(&session_id, submission("resume the queue"))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !restored
            .session_inbox(&session_id)
            .await
            .unwrap()
            .items
            .is_empty()
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("a new submission resumes the preserved FIFO");
    // Join the last post-turn bookkeeping before checking the durable log.
    restored.shutdown().await.unwrap();
    let events = super::JsonlEventStore::new(
        &root.path().join("data/sessions"),
        &ternilo_protocol::SessionId::new(&session_id),
    )
    .load_events()
    .await
    .unwrap();
    assert!(events.iter().any(|event| {
        event.run_id == active.run_id && matches!(event.kind, SessionEventKind::TurnCancelled)
    }));
    let consumed = events
        .iter()
        .filter_map(|event| match &event.kind {
            SessionEventKind::UserMessage {
                source: Some(UserMessageSource::Submission { submission_id, .. }),
                ..
            } => Some(submission_id.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(consumed, vec![active.id, queued.id, wake.id]);
}

#[tokio::test]
async fn shutdown_interrupts_title_generation_after_the_answer_is_durable() {
    let root = tempfile::tempdir().unwrap();
    let mut model = SlowModel::start(1).await;
    let (application, session_id) = setup(root.path(), &model).await;
    let running = {
        let application = Arc::clone(&application);
        let session_id = session_id.clone();
        tokio::spawn(async move {
            application
                .run_turn(
                    &session_id,
                    Some("title-run".to_owned()),
                    "request".to_owned(),
                )
                .await
        })
    };
    model.wait_for_request().await;
    model.wait_for_request().await;
    assert!(
        application
            .session_inbox(&session_id)
            .await
            .unwrap()
            .active_run_id
            .is_none(),
        "the model answer finished before the slow title request"
    );
    tokio::time::timeout(Duration::from_secs(5), application.shutdown())
        .await
        .expect("title generation cannot hold shutdown until the model timeout")
        .unwrap();
    let outcome = running.await.unwrap().unwrap();
    assert_eq!(outcome.answer, "Completed answer");
    assert_eq!(outcome.generated_title, None);
    assert!(outcome.events.iter().any(|event| {
        matches!(
            event.kind,
            SessionEventKind::SessionTitleGenerationFinished { generated: false }
        )
    }));
    assert!(
        application
            .run_turn(&session_id, None, "too late".to_owned())
            .await
            .unwrap_err()
            .is_cancelled()
    );
    assert!(outcome.events.iter().any(|event| {
        event.run_id == RunId::new("title-run")
            && matches!(event.kind, SessionEventKind::TurnFinished { .. })
    }));
}
