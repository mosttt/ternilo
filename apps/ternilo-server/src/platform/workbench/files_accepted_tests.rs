use std::fmt::Write as _;

use salvo_core::http::StatusCode;
use sha2::{Digest as _, Sha256};
use ternilo_cloud::{CloudRunDraft, CloudSessionRecord, CloudSubmissionReceipt, CompiledRun};
use ternilo_protocol::{
    Attachment, Profile, QueueEditRequest, RunId, SessionEventKind, SessionFileQuery,
    SessionSubmissionRequest, SubmissionContent, SubmissionDelivery, SubmissionPlacement,
    UserMessageSource,
};

use super::Fixture;

impl Fixture {
    async fn assert_stale_queue_http(
        &self,
        session: &CloudSessionRecord,
        original: &ternilo_protocol::SessionSubmission,
        current: &ternilo_protocol::SessionSubmission,
    ) {
        use salvo_core::test::ResponseExt as _;
        for (body, expected_status) in [
            (
                serde_json::json!({ "input": "stale HTTP edit", "expected_updated_at_ms": original.updated_at_ms }),
                StatusCode::CONFLICT,
            ),
            (
                serde_json::json!({ "input": "unconditional HTTP edit" }),
                StatusCode::BAD_REQUEST,
            ),
        ] {
            let mut response = salvo_core::test::TestClient::patch(format!(
                "http://server.test/api/v1/sessions/{}/queue/{}",
                session.session_id, original.id
            ))
            .add_header(
                "Authorization",
                format!("Bearer {}", self.owner.access_token),
                true,
            )
            .add_header("x-ternilo-tenant", self.tenant.as_str(), true)
            .json(&body)
            .send(&self.service)
            .await;
            let status = response.status_code;
            let body = response.take_json::<serde_json::Value>().await.unwrap();
            assert_eq!(status, Some(expected_status), "{body}");
            let persisted = self
                .state
                .cloud
                .strict_steering_candidate(
                    &self.tenant,
                    &self.owner.session.user.user_id,
                    &session.session_id,
                    &original.id,
                )
                .await
                .unwrap();
            assert_eq!(persisted, *current);
        }
    }

    async fn delete_http(
        &self,
        session_id: &ternilo_protocol::SessionId,
        account: &super::NativeSessionGrant,
    ) -> StatusCode {
        salvo_core::test::TestClient::delete(format!(
            "http://server.test/api/v1/sessions/{session_id}"
        ))
        .add_header(
            "Authorization",
            format!("Bearer {}", account.access_token),
            true,
        )
        .add_header("x-ternilo-tenant", self.tenant.as_str(), true)
        .send(&self.service)
        .await
        .status_code
        .expect("HTTP response has a status")
    }

    fn compile_uploads(
        &self,
        session: &CloudSessionRecord,
        run: &str,
        attachments: Vec<Attachment>,
    ) -> CompiledRun {
        self.state
            .worker_policy
            .compile_run(
                CloudRunDraft {
                    project_id: session.project_id.clone(),
                    workspace_id: session.workspace_id.clone(),
                    agent_id: session.agent_id.clone(),
                    session_id: session.session_id.clone(),
                    run_id: Some(RunId::new(run)),
                    limits: self.state.worker_policy.maximum_limits,
                    permissions: session.permissions,
                    mode: session.mode,
                    profile: Profile::default(),
                    input: "Inspect accepted uploads".to_owned(),
                    references: vec![],
                    reference_contexts: vec![],
                    attachments,
                    reserved_model_tokens: 100,
                },
                self.tenant.clone(),
                self.owner.session.user.user_id.clone(),
                self.owner.session.user.user_id.clone(),
                &self.state.catalog,
            )
            .unwrap()
    }

    async fn retained_upload(&self, session: &CloudSessionRecord, name: &str) -> Attachment {
        let mut upload = attachment(name);
        let digest = Sha256::digest(upload.content.as_bytes()).iter().fold(
            String::with_capacity(64),
            |mut digest, byte| {
                write!(&mut digest, "{byte:02x}").expect("writing to a String cannot fail");
                digest
            },
        );
        let mut tx = self
            .state
            .cloud
            .database()
            .owner_transaction(&self.tenant, &self.owner.session.user.user_id)
            .await
            .unwrap();
        sqlx::query("INSERT INTO cloud_attachment_objects (tenant_id,workspace_id,digest,content,created_at_ms)
            VALUES ($1,$2,$3,$4,$5)")
            .bind(self.tenant.as_str()).bind(session.workspace_id.as_str()).bind(&digest)
            .bind(upload.content.as_bytes()).bind(i64::try_from(self.now).unwrap())
            .execute(&mut *tx).await.unwrap();
        tx.commit().await.unwrap();
        upload.content = format!("ternilo-attachment://sha256/{digest}");
        upload
    }

    async fn accept_uploads(&self, compiled: &CompiledRun) -> CloudSubmissionReceipt {
        let request = SessionSubmissionRequest {
            delivery: SubmissionDelivery::Queue,
            run_id: Some(compiled.spec.metadata.run_id.clone()),
            content: SubmissionContent::Prompt {
                input: compiled.spec.input.clone(),
            },
            references: vec![],
            attachments: compiled.spec.attachments.clone(),
        };
        self.state
            .cloud
            .enqueue_session_submission_as(
                &self.owner.session.user.user_id,
                compiled,
                &request,
                self.now,
            )
            .await
            .unwrap()
    }
}

fn attachment(name: &str) -> Attachment {
    Attachment {
        name: name.to_owned(),
        media_type: "text/plain".to_owned(),
        content: format!("\u{feff}Accepted {name}\r\n"),
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "Exercise acceptance, queue edits, consumption and stable pagination in one lifecycle."
)]
pub(super) async fn accepted_upload_contract(fixture: &Fixture) {
    let session = fixture.session("accepted-files").await;
    let owner = &fixture.owner.session.user.user_id;
    let compiled = fixture.compile_uploads(
        &session,
        "accepted-primary",
        vec![
            fixture.retained_upload(&session, "first.txt").await,
            attachment("second.txt"),
        ],
    );
    let primary = fixture.accept_uploads(&compiled).await;
    assert_eq!(primary.submission.placement, SubmissionPlacement::Running);
    let pending = fixture.compile_uploads(
        &session,
        "accepted-pending",
        vec![attachment("pending.txt")],
    );
    let queued = fixture.accept_uploads(&pending).await;
    assert_eq!(queued.submission.placement, SubmissionPlacement::Queued);
    let query = SessionFileQuery {
        session_id: Some(session.session_id.clone()),
        limit: 1,
        ..SessionFileQuery::default()
    };
    let first = super::super::file_page(
        &fixture.state,
        &fixture.owner.session.user,
        &fixture.tenant,
        &query,
    )
    .await
    .unwrap();
    assert_eq!(first.items.len(), 1);
    assert!(first.items[0].event_seq.is_none());
    assert_eq!(first.items[0].occurred_at_ms, fixture.now);
    let mut visited = vec![first.items[0].id.clone()];
    let cursor = first.next_cursor.clone();

    // A consumed upload keeps the accepted identity and time even when another run consumes it.
    fixture
        .event(
            &session,
            20,
            SessionEventKind::UserMessage {
                provenance: primary.submission.provenance.clone(),
                content: "Inspect accepted uploads".to_owned(),
                display_content: None,
                source: Some(UserMessageSource::Submission {
                    regenerate_from: None,
                    submission_id: primary.submission.id.clone(),
                    created_at_ms: primary.submission.created_at_ms,
                    delivery: SubmissionDelivery::Steer,
                    skill_name: None,
                }),
                references: vec![],
                attachments: primary.submission.attachments.clone(),
            },
        )
        .await;
    let mut query = SessionFileQuery { cursor, ..query };
    while query.cursor.is_some() {
        let page = super::super::file_page(
            &fixture.state,
            &fixture.owner.session.user,
            &fixture.tenant,
            &query,
        )
        .await
        .unwrap();
        visited.extend(page.items.into_iter().map(|item| item.id));
        query.cursor = page.next_cursor;
    }
    assert_eq!(visited.len(), 3);
    assert_eq!(
        visited
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        3
    );
    let (all, raw) = fixture
        .page(
            &format!("/files?session_id={}", session.session_id),
            &fixture.owner,
        )
        .await;
    assert_eq!(all.items.len(), 3);
    assert!(!raw.to_string().contains("Accepted first.txt"));
    assert!(!raw.to_string().contains("ternilo-attachment://"));
    assert!(
        !raw.to_string()
            .contains(compiled.spec.attachments[0].reference_digest().unwrap())
    );
    for item in &all.items {
        assert_eq!(item.occurred_at_ms, fixture.now);
        let bytes = fixture
            .content(&session.session_id, &item.id, &fixture.owner)
            .await;
        assert_eq!(bytes, attachment(&item.name).content.as_bytes());
        if item.name != "pending.txt" {
            assert_eq!(item.event_seq, Some(20));
            assert_eq!(item.run_id, primary.submission.run_id);
        }
    }

    let mut changed = pending.clone();
    changed.spec.attachments = vec![attachment("replacement.txt")];
    let edit = fixture
        .state
        .cloud
        .edit_queued_session_submission(
            &fixture.tenant,
            owner,
            &session.session_id,
            &queued.submission.id,
            QueueEditRequest {
                input: "Change text only".to_owned(),
                expected_updated_at_ms: queued.submission.updated_at_ms,
            },
            &changed,
            fixture.now + 30,
        )
        .await
        .unwrap_err();
    assert!(edit.message.contains("preserve accepted attachments"));
    let mut text_edit = pending.clone();
    text_edit.spec.input = "Change text only".to_owned();
    let edited = fixture
        .state
        .cloud
        .edit_queued_session_submission(
            &fixture.tenant,
            owner,
            &session.session_id,
            &queued.submission.id,
            QueueEditRequest {
                input: text_edit.spec.input.clone(),
                expected_updated_at_ms: queued.submission.updated_at_ms,
            },
            &text_edit,
            fixture.now + 30,
        )
        .await
        .unwrap();
    assert_eq!(edited.id, queued.submission.id);
    assert_eq!(edited.attachments, queued.submission.attachments);
    assert_eq!(edited.created_at_ms, queued.submission.created_at_ms);
    fixture
        .assert_stale_queue_http(&session, &queued.submission, &edited)
        .await;
    fixture
        .state
        .cloud
        .remove_queued_session_submission(
            &fixture.tenant,
            owner,
            &session.session_id,
            &queued.submission.id,
            fixture.now + 31,
        )
        .await
        .unwrap();
    let (after_removal, _) = fixture
        .page(
            &format!("/files?session_id={}", session.session_id),
            &fixture.owner,
        )
        .await;
    assert_eq!(after_removal.items.len(), 3);

    fixture
        .event(&session, 21, SessionEventKind::TurnCancelled)
        .await;
    let child = fixture
        .state
        .cloud
        .fork_session(
            &fixture.tenant,
            owner,
            &session.session_id,
            None,
            fixture.now + 32,
        )
        .await
        .unwrap();
    let (child_files, _) = fixture
        .page(
            &format!("/files?session_id={}", child.session_id),
            &fixture.owner,
        )
        .await;
    assert_eq!(
        child_files.items.len(),
        2,
        "pending parent uploads must not enter the fork"
    );
    for item in child_files.items {
        assert_eq!(item.run_id, primary.submission.run_id);
        assert_eq!(
            fixture
                .content(&child.session_id, &item.id, &fixture.owner)
                .await,
            attachment(&item.name).content.as_bytes()
        );
    }
    assert_eq!(
        fixture.delete_http(&child.session_id, &fixture.owner).await,
        StatusCode::NO_CONTENT
    );
    let (deleted, _) = fixture
        .page(
            &format!("/files?session_id={}", child.session_id),
            &fixture.owner,
        )
        .await;
    assert!(deleted.items.is_empty());
    accepted_archive_access_contract(fixture, &session).await;
}

#[expect(
    clippy::too_many_lines,
    reason = "Verify queued-file visibility through sharing, archival, and revocation as one lifecycle."
)]
async fn accepted_archive_access_contract(fixture: &Fixture, session: &CloudSessionRecord) {
    let owner = &fixture.owner.session.user;
    let collaborator = fixture.collaborator().await;
    fixture
        .state
        .store
        .set_resource_share(
            owner,
            &fixture.tenant,
            super::ResourceKind::Session,
            session.session_id.as_str(),
            &collaborator.session.user.user_id,
            Some(super::ResourcePermissions {
                view: true,
                ..super::ResourcePermissions::default()
            }),
            fixture.now + 33,
        )
        .await
        .unwrap();
    let (files, _) = fixture
        .page(
            &format!("/files?session_id={}", session.session_id),
            &collaborator,
        )
        .await;
    assert_eq!(files.items.len(), 3);
    fixture
        .state
        .cloud
        .pause_session_inbox(
            &fixture.tenant,
            &owner.user_id,
            &session.session_id,
            None,
            fixture.now + 34,
        )
        .await
        .unwrap();
    for run in fixture
        .state
        .cloud
        .active_session_runs(&fixture.tenant, &owner.user_id, &session.session_id)
        .await
        .unwrap()
    {
        fixture
            .state
            .cloud
            .cancel_run_as(
                &fixture.tenant,
                &owner.user_id,
                &session.session_id,
                &run,
                fixture.now + 35,
            )
            .await
            .unwrap();
    }
    fixture
        .state
        .cloud
        .archive_session(
            &fixture.tenant,
            &owner.user_id,
            &session.session_id,
            fixture.now + 36,
        )
        .await
        .unwrap();
    let (archived, _) = fixture
        .page(
            &format!("/files?session_id={}", session.session_id),
            &collaborator,
        )
        .await;
    assert_eq!(archived.items.len(), 3);
    for item in &archived.items {
        assert!(item.session_archived);
        assert_eq!(
            fixture
                .content(&session.session_id, &item.id, &collaborator)
                .await,
            attachment(&item.name).content.as_bytes()
        );
    }
    assert_eq!(
        fixture
            .delete_http(&session.session_id, &collaborator)
            .await,
        StatusCode::FORBIDDEN,
        "viewing shared archived files does not grant deletion",
    );
    assert_eq!(
        fixture
            .get(
                &format!("/sessions/{}/events", session.session_id),
                &fixture.owner,
                &fixture.tenant
            )
            .await
            .status_code,
        Some(StatusCode::BAD_REQUEST),
        "ordinary session resolution still excludes archived Cloud sessions",
    );
    fixture
        .state
        .store
        .set_resource_share(
            owner,
            &fixture.tenant,
            super::ResourceKind::Session,
            session.session_id.as_str(),
            &collaborator.session.user.user_id,
            None,
            fixture.now + 37,
        )
        .await
        .unwrap();
    let (revoked, _) = fixture
        .page(
            &format!("/files?session_id={}", session.session_id),
            &collaborator,
        )
        .await;
    assert!(revoked.items.is_empty());
    let path = format!(
        "/sessions/{}/files/{}/content",
        session.session_id, archived.items[0].id
    );
    assert_eq!(
        fixture
            .get(&path, &collaborator, &fixture.tenant)
            .await
            .status_code,
        Some(StatusCode::FORBIDDEN)
    );
    assert_eq!(
        fixture
            .delete_http(&session.session_id, &fixture.owner)
            .await,
        StatusCode::NO_CONTENT
    );
    let (deleted, _) = fixture
        .page(
            &format!("/files?session_id={}", session.session_id),
            &fixture.owner,
        )
        .await;
    assert!(
        deleted.items.is_empty(),
        "HTTP deletion removes accepted uploads and historical file metadata"
    );
}

#[tokio::test]
async fn sqlite_accepted_uploads_survive_queue_lifecycle_and_fork_only_consumed_files() {
    accepted_upload_contract(&Fixture::new("sqlite::memory:", None).await).await;
}
