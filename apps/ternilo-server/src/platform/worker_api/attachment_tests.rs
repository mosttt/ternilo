use std::fmt::Write as _;

use futures_util::TryStreamExt as _;
use sha2::{Digest as _, Sha256};

use super::*;

#[expect(
    clippy::too_many_lines,
    reason = "Exercise upload, canonical download and rejection paths against one leased run."
)]
pub(super) async fn verify_upload(
    service: &Service,
    bearer: &str,
    identity: &ternilo_cloud::CloudWorkerIdentity,
    cloud: &CloudStore,
    run: &ternilo_cloud::StartedRun,
) {
    let bytes = vec![128_u8; 512 * 1024];
    let attachment = reference(&bytes);
    let metadata = ternilo_cloud::WorkerAttachmentUpload {
        identity: identity.clone(),
        run: run.into(),
        attachment: attachment.clone(),
    };
    let encoded = |value: &ternilo_cloud::WorkerAttachmentUpload| {
        STANDARD.encode(serde_json::to_vec(value).unwrap())
    };
    let endpoint = "http://server.test/internal/worker/v1/attachment";
    let mut response = TestClient::post(endpoint)
        .add_header("Authorization", bearer, true)
        .add_header("x-ternilo-attachment", encoded(&metadata), true)
        .body(bytes.clone())
        .send(service)
        .await;
    assert_eq!(response.status_code, Some(StatusCode::OK));
    assert!(matches!(
        response.take_json::<WorkerReply>().await.unwrap(),
        WorkerReply::Unit
    ));
    let event = ternilo_protocol::SessionEvent {
        seq: run.prior_events.last().map_or(0, |event| event.seq + 1),
        occurred_at_ms: now_ms().unwrap(),
        run_id: run.claim.run_id.clone(),
        kind: ternilo_protocol::SessionEventKind::DeliverableProduced {
            path: "generated.bin".into(),
            operation: "write".into(),
            attachment,
        },
    };
    cloud
        .append_event(run, identity.worker_id.as_str(), &event, now_ms().unwrap())
        .await
        .unwrap();
    let id = format!("generated-{}-0", event.seq);
    let first = cloud
        .session_file_chunk_as(
            &run.claim.tenant_id,
            &run.claim.actor_user_id,
            &run.claim.session_id,
            &id,
            0,
        )
        .await
        .unwrap();
    let chunks =
        ternilo_local::workspace_download_stream(first, |_, _| async { panic!("one chunk file") })
            .try_collect::<Vec<_>>()
            .await
            .unwrap();
    assert_eq!(chunks.concat(), bytes);
    assert!(
        cloud
            .session_file_chunk_as(
                &run.claim.tenant_id,
                &ternilo_protocol::UserId::new("unrelated"),
                &run.claim.session_id,
                &id,
                0
            )
            .await
            .is_err()
    );

    let mut forged = metadata.clone();
    forged.run.writer_fencing_token += 1;
    assert_eq!(
        TestClient::post(endpoint)
            .add_header("Authorization", bearer, true)
            .add_header("x-ternilo-attachment", encoded(&forged), true)
            .body(bytes.clone())
            .send(service)
            .await
            .status_code,
        Some(StatusCode::FORBIDDEN)
    );
    assert_eq!(
        TestClient::post(endpoint)
            .add_header("Authorization", bearer, true)
            .add_header("x-ternilo-attachment", encoded(&metadata), true)
            .body(vec![0; bytes.len()])
            .send(service)
            .await
            .status_code,
        Some(StatusCode::BAD_REQUEST)
    );
    let oversized = vec![0_u8; 1024 * 1024 + 1];
    let metadata = ternilo_cloud::WorkerAttachmentUpload {
        attachment: reference(&oversized),
        ..metadata
    };
    assert_eq!(
        TestClient::post(endpoint)
            .add_header("Authorization", bearer, true)
            .add_header("x-ternilo-attachment", encoded(&metadata), true)
            .body(oversized)
            .send(service)
            .await
            .status_code,
        Some(StatusCode::BAD_REQUEST)
    );
}

fn reference(bytes: &[u8]) -> ternilo_protocol::Attachment {
    let digest =
        Sha256::digest(bytes)
            .iter()
            .fold(String::with_capacity(64), |mut digest, byte| {
                write!(digest, "{byte:02x}").unwrap();
                digest
            });
    ternilo_protocol::Attachment {
        name: "generated.bin".into(),
        media_type: "application/octet-stream".into(),
        content: format!("ternilo-attachment://sha256/{digest}"),
    }
}
