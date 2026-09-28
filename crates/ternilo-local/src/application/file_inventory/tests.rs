use base64::{Engine as _, engine::general_purpose::STANDARD};
use ternilo_kernel::HostPolicy;
use ternilo_protocol::{
    Attachment, RunId, RunLimits, SessionEvent, SessionEventKind, SessionFileKind, SessionFileQuery,
};

use crate::{LocalApplication, event_store::JsonlEventStore};

async fn open(data: &std::path::Path) -> LocalApplication {
    LocalApplication::open(
        crate::catalog().unwrap(),
        crate::local_profile(),
        HostPolicy::local(RunLimits::default()),
        data.to_owned(),
    )
    .await
    .unwrap()
}

fn event(seq: u64, kind: SessionEventKind) -> SessionEvent {
    SessionEvent {
        seq,
        occurred_at_ms: seq + 1,
        run_id: RunId::new("files-test"),
        kind,
    }
}

async fn retained(
    application: &LocalApplication,
    name: &str,
    media_type: &str,
    bytes: &[u8],
) -> Attachment {
    application
        .attachments
        .save_many(vec![Attachment {
            name: name.to_owned(),
            media_type: media_type.to_owned(),
            content: if media_type.starts_with("text/") {
                String::from_utf8(bytes.to_vec()).unwrap()
            } else {
                format!("data:{media_type};base64,{}", STANDARD.encode(bytes))
            },
        }])
        .await
        .unwrap()
        .remove(0)
}

async fn seed(
    application: &LocalApplication,
    session: &crate::LocalSession,
    events: &[SessionEvent],
) {
    JsonlEventStore::new(
        &application.state.sessions_dir(),
        &session.identity.session_id,
    )
    .seed_events(events)
    .await
    .unwrap();
    application
        .session_archive
        .index()
        .append_many_fail_soft(
            session.identity.session_id.as_str().to_owned(),
            session.workspace_id.as_str().to_owned(),
            events,
        )
        .await;
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "Exercise durable history, index-only pagination, exact bytes, archive, deletion, and restart as one file lifecycle."
)]
async fn inventory_pages_full_history_and_downloads_immutable_bytes_after_restart() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("data");
    let workspace = temp.path().join("workspace");
    tokio::fs::create_dir(&workspace).await.unwrap();
    let application = open(&data).await;
    let workspace = application
        .add_workspace(workspace.to_str().unwrap())
        .await
        .unwrap();
    let first = application
        .create_session(
            workspace.workspace_id.clone(),
            Some("files-a".to_owned()),
            None,
        )
        .await
        .unwrap();
    let second = application
        .create_session(
            workspace.workspace_id.clone(),
            Some("files-b".to_owned()),
            None,
        )
        .await
        .unwrap();
    let literal = b"data:text/plain;base64,VGhpcyBpcyBsaXRlcmFsIHRleHQ=";
    let upload = retained(&application, "literal.txt", "text/plain", literal).await;
    let binary = retained(
        &application,
        "bytes.bin",
        "application/octet-stream",
        &[0, 255, 3, 4],
    )
    .await;
    let version_one = retained(&application, "report.txt", "text/plain", b"first version").await;
    let version_two = retained(&application, "report.txt", "text/plain", b"second version").await;
    let mut events = vec![
        event(
            0,
            SessionEventKind::UserMessage {
                provenance: None,
                content: "uploaded".to_owned(),
                display_content: None,
                source: None,
                references: Vec::new(),
                attachments: vec![upload.clone(), binary],
            },
        ),
        event(
            1,
            SessionEventKind::DeliverableProduced {
                path: "report.txt".to_owned(),
                operation: "write".to_owned(),
                attachment: version_one,
            },
        ),
    ];
    events.extend((2..2_002).map(|seq| {
        event(
            seq,
            SessionEventKind::AssistantMessageDelta {
                step: 1,
                delta: "stream".to_owned(),
            },
        )
    }));
    events.push(event(
        2_002,
        SessionEventKind::DeliverableProduced {
            path: "report.txt".to_owned(),
            operation: "replace".to_owned(),
            attachment: version_two,
        },
    ));
    seed(&application, &first, &events).await;
    seed(
        &application,
        &second,
        &[event(
            0,
            SessionEventKind::UserMessage {
                provenance: None,
                content: "other session".to_owned(),
                display_content: None,
                source: None,
                references: Vec::new(),
                attachments: vec![upload],
            },
        )],
    )
    .await;

    // A normal listing uses the maintained metadata index, not any transcript.
    let history = application.state.sessions_dir();
    let withheld = data.join("temporarily-withheld-history");
    tokio::fs::rename(&history, &withheld).await.unwrap();
    let mut query = SessionFileQuery {
        limit: 1,
        ..SessionFileQuery::default()
    };
    let mut found = Vec::new();
    loop {
        let page = application.files(query.clone()).await.unwrap();
        assert_eq!(page.items.len(), 1);
        found.push((page.items[0].session_id.clone(), page.items[0].id.clone()));
        query.cursor = page.next_cursor;
        if query.cursor.is_none() {
            break;
        }
    }
    tokio::fs::rename(&withheld, &history).await.unwrap();
    assert_eq!(found.len(), 5);
    assert!(found.contains(&(first.identity.session_id.clone(), "upload-0-0".to_owned())));
    assert_eq!(found[0].1, "generated-2002-0");

    let generated = application
        .files(SessionFileQuery {
            kind: Some(SessionFileKind::Generated),
            query: Some("REPORT".to_owned()),
            ..SessionFileQuery::default()
        })
        .await
        .unwrap();
    assert_eq!(generated.items.len(), 2);
    assert!(generated.next_cursor.is_none());
    assert!(
        application
            .files(SessionFileQuery {
                query: Some("%".to_owned()),
                ..SessionFileQuery::default()
            })
            .await
            .unwrap()
            .items
            .is_empty()
    );
    let original = application
        .session_file_content(first.identity.session_id.as_str(), "generated-1-0")
        .await
        .unwrap();
    assert_eq!(
        STANDARD.decode(original.content_base64).unwrap(),
        b"first version"
    );
    let original = application
        .session_file_content(first.identity.session_id.as_str(), "upload-0-0")
        .await
        .unwrap();
    assert_eq!(STANDARD.decode(original.content_base64).unwrap(), literal);
    let original = application
        .session_file_content(first.identity.session_id.as_str(), "upload-0-1")
        .await
        .unwrap();
    assert_eq!(
        STANDARD.decode(original.content_base64).unwrap(),
        [0, 255, 3, 4]
    );
    assert!(
        application
            .session_file_content(second.identity.session_id.as_str(), "generated-1-0")
            .await
            .is_err()
    );
    assert!(
        application
            .session_file_content(first.identity.session_id.as_str(), "../../private")
            .await
            .is_err()
    );

    application
        .archive_session(first.identity.session_id.as_str())
        .await
        .unwrap();
    assert_eq!(
        application
            .files(SessionFileQuery::default())
            .await
            .unwrap()
            .items
            .len(),
        5
    );
    assert!(
        application
            .files(SessionFileQuery {
                session_id: Some(first.identity.session_id.clone()),
                ..SessionFileQuery::default()
            })
            .await
            .unwrap()
            .items
            .iter()
            .all(|item| item.session_archived)
    );
    application
        .delete_session(second.identity.session_id.as_str())
        .await
        .unwrap();
    assert_eq!(
        application
            .files(SessionFileQuery::default())
            .await
            .unwrap()
            .items
            .len(),
        4
    );
    application.shutdown().await.unwrap();
    drop(application);

    let restored = open(&data).await;
    let page = restored
        .files(SessionFileQuery {
            workspace_id: Some(workspace.workspace_id),
            session_id: Some(first.identity.session_id.clone()),
            ..SessionFileQuery::default()
        })
        .await
        .unwrap();
    assert_eq!(page.items.len(), 4);
    let original = restored
        .session_file_content(first.identity.session_id.as_str(), "generated-1-0")
        .await
        .unwrap();
    assert_eq!(
        STANDARD.decode(original.content_base64).unwrap(),
        b"first version"
    );
    restored.shutdown().await.unwrap();
}

#[test]
fn legacy_inline_text_is_not_mistaken_for_an_encoded_url() {
    let text = Attachment {
        name: "literal.txt".to_owned(),
        media_type: "text/plain; charset=utf-8".to_owned(),
        content: "data:text/plain;base64,SGVsbG8=".to_owned(),
    };
    assert_eq!(
        crate::inline_file_attachment_bytes(&text).unwrap(),
        text.content.as_bytes()
    );
    let image = Attachment {
        name: "image.png".to_owned(),
        media_type: "image/png".to_owned(),
        content: "data:image/png;base64,AAECAw==".to_owned(),
    };
    assert_eq!(
        crate::inline_file_attachment_bytes(&image).unwrap(),
        [0, 1, 2, 3]
    );
}

#[tokio::test]
async fn real_write_file_snapshots_preserve_data_url_text_and_previous_versions() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("workspace");
    tokio::fs::create_dir(&workspace).await.unwrap();
    let application = open(&temp.path().join("data")).await;
    let registered = application
        .add_workspace(workspace.to_str().unwrap())
        .await
        .unwrap();
    let session = application
        .create_session(registered.workspace_id, None, None)
        .await
        .unwrap();
    let id = session.identity.session_id.as_str();
    let text = "data:text/plain; charset=utf-8;base64,SGVsbG8=";
    application
        .run_turn(id, None, format!("/write literal.txt {text}"))
        .await
        .unwrap();
    let first = application
        .files(SessionFileQuery::default())
        .await
        .unwrap();
    assert_eq!(first.items.len(), 1);
    let file_id = first.items[0].id.clone();
    application
        .run_turn(id, None, "/write literal.txt changed later".to_owned())
        .await
        .unwrap();
    assert_eq!(
        tokio::fs::read(workspace.join("literal.txt"))
            .await
            .unwrap(),
        b"changed later"
    );
    assert_eq!(
        application
            .files(SessionFileQuery::default())
            .await
            .unwrap()
            .items
            .len(),
        2
    );
    let original = application
        .session_file_content(id, &file_id)
        .await
        .unwrap();
    assert_eq!(
        STANDARD.decode(original.content_base64).unwrap(),
        text.as_bytes()
    );
    application.shutdown().await.unwrap();
}

#[tokio::test]
async fn retained_file_storage_keeps_its_larger_limit_than_user_uploads() {
    let temp = tempfile::tempdir().unwrap();
    let storage = crate::LocalAttachments::open(temp.path()).await.unwrap();
    let content = "x".repeat(8 * 1024 * 1024 + 1);
    let attachment = Attachment {
        name: "large-retained.txt".to_owned(),
        media_type: "text/plain".to_owned(),
        content,
    };
    assert!(storage.save_many(vec![attachment.clone()]).await.is_err());
    let reference = ternilo_kernel::AttachmentResolver::store(&storage, attachment)
        .await
        .unwrap();
    let bytes = storage.reference_bytes(&reference).await.unwrap();
    assert_eq!(bytes.len(), 8 * 1024 * 1024 + 1);
    assert!(bytes.iter().all(|byte| *byte == b'x'));
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "Validate acceptance, cancellation, consumption, stable pagination, fork and restart as one file lifecycle."
)]
async fn queued_uploads_keep_identity_bytes_and_original_time_through_consumption() {
    use std::sync::Arc;
    use ternilo_protocol::{SessionSubmissionRequest, SubmissionContent, SubmissionDelivery};
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("data");
    let workspace = temp.path().join("workspace");
    tokio::fs::create_dir(&workspace).await.unwrap();
    let application = Arc::new(open(&data).await);
    let workspace = application
        .add_workspace(workspace.to_str().unwrap())
        .await
        .unwrap();
    let session = application
        .create_session(workspace.workspace_id, None, None)
        .await
        .unwrap();
    let id = session.identity.session_id.as_str().to_owned();
    let active = {
        let application = Arc::clone(&application);
        let id = id.clone();
        tokio::spawn(async move {
            application
                .run_turn(&id, None, "/ask Hold the queue?".to_owned())
                .await
        })
    };
    let question = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            if let Some(question) = application
                .pending_questions(Some(&id))
                .await
                .into_iter()
                .next()
            {
                break question;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let bytes = "\u{feff}original\r\n原样";
    let attachment = Attachment {
        name: "same.txt".to_owned(),
        media_type: "text/plain".to_owned(),
        content: bytes.to_owned(),
    };
    let request = |attachments| SessionSubmissionRequest {
        delivery: SubmissionDelivery::Queue,
        run_id: None,
        content: SubmissionContent::Prompt {
            input: "/code \"queued with files\"".to_owned(),
        },
        references: Vec::new(),
        attachments,
    };
    let accepted = application
        .submit_session(&id, request(vec![attachment.clone(), attachment.clone()]))
        .await
        .unwrap();
    let cancelled = application
        .submit_session(&id, request(vec![attachment]))
        .await
        .unwrap();
    let query = SessionFileQuery {
        session_id: Some(session.identity.session_id.clone()),
        ..SessionFileQuery::default()
    };
    let before = application.files(query.clone()).await.unwrap();
    assert_eq!(before.items.len(), 3);
    assert!(before.items.iter().all(|file| file.event_seq.is_none()));
    assert_eq!(
        before
            .items
            .iter()
            .map(|file| &file.id)
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        3,
        "identical bytes are distinct accepted occurrences"
    );
    for file in &before.items {
        let content = application
            .session_file_content(&id, &file.id)
            .await
            .unwrap();
        assert_eq!(
            STANDARD.decode(content.content_base64).unwrap(),
            bytes.as_bytes()
        );
    }
    application
        .remove_session_queue_item(&id, cancelled.id.clone())
        .await
        .unwrap();
    assert_eq!(
        application.files(query.clone()).await.unwrap().items.len(),
        3
    );
    let first_page = application
        .files(SessionFileQuery {
            limit: 1,
            ..query.clone()
        })
        .await
        .unwrap();
    let cursor = first_page.next_cursor.clone().unwrap();
    application
        .answer_question(ternilo_protocol::UserAnswer {
            question_id: question.question.id,
            selected: Vec::new(),
            custom: Some("continue".to_owned()),
        })
        .await
        .unwrap();
    active.await.unwrap().unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            if application
                .session_inbox(&id)
                .await
                .unwrap()
                .items
                .is_empty()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let after = application.files(query.clone()).await.unwrap();
    assert_eq!(
        before
            .items
            .iter()
            .map(|file| (&file.id, file.occurred_at_ms))
            .collect::<Vec<_>>(),
        after
            .items
            .iter()
            .map(|file| (&file.id, file.occurred_at_ms))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        after
            .items
            .iter()
            .filter(|file| file.event_seq.is_some())
            .count(),
        2
    );
    assert!(
        after
            .items
            .iter()
            .filter(|file| file.id.starts_with(&format!("submission-{}-", accepted.id)))
            .all(|file| file.occurred_at_ms == accepted.created_at_ms)
    );
    let rest = application
        .files(SessionFileQuery {
            cursor: Some(cursor),
            ..query.clone()
        })
        .await
        .unwrap();
    assert_eq!(rest.items.len(), 2);
    assert!(
        rest.items
            .iter()
            .all(|file| file.id != first_page.items[0].id)
    );
    let child = application.fork_session(&id, None, None).await.unwrap();
    let inherited = application
        .files(SessionFileQuery {
            session_id: Some(child.identity.session_id.clone()),
            ..SessionFileQuery::default()
        })
        .await
        .unwrap();
    assert_eq!(
        inherited.items.len(),
        2,
        "fork excludes the removed, never-consumed upload"
    );
    for file in inherited.items {
        let content = application
            .session_file_content(child.identity.session_id.as_str(), &file.id)
            .await
            .unwrap();
        assert_eq!(
            STANDARD.decode(content.content_base64).unwrap(),
            bytes.as_bytes()
        );
    }
    application.archive_session(&id).await.unwrap();
    assert!(
        application
            .files(query.clone())
            .await
            .unwrap()
            .items
            .iter()
            .all(|file| file.session_archived)
    );
    let stream_id = application.accepted_upload_stream_id().to_owned();
    application.shutdown().await.unwrap();
    drop(application);
    let application = open(&data).await;
    assert_eq!(application.accepted_upload_stream_id(), stream_id);
    assert_eq!(application.files(query).await.unwrap().items.len(), 3);
    let content = application
        .session_file_content(&id, &format!("submission-{}-0", cancelled.id))
        .await
        .unwrap();
    assert_eq!(
        STANDARD.decode(content.content_base64).unwrap(),
        bytes.as_bytes()
    );
    application.shutdown().await.unwrap();
}

#[tokio::test]
async fn deleted_session_ids_cannot_be_recreated_or_forked_after_restart() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("data");
    let workspace = temp.path().join("workspace");
    tokio::fs::create_dir(&workspace).await.unwrap();
    let application = open(&data).await;
    let workspace = application
        .add_workspace(workspace.to_str().unwrap())
        .await
        .unwrap();
    let deleted = "permanent-session-identity";
    application
        .create_session(
            workspace.workspace_id.clone(),
            Some(deleted.to_owned()),
            None,
        )
        .await
        .unwrap();
    application.delete_session(deleted).await.unwrap();
    application.delete_session(deleted).await.unwrap();
    assert!(application.snapshot().await.sessions.is_empty());
    let error = application
        .create_session(
            workspace.workspace_id.clone(),
            Some(deleted.to_owned()),
            None,
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("deleted session IDs"));
    let parent = application
        .create_session(workspace.workspace_id.clone(), None, None)
        .await
        .unwrap();
    application
        .run_turn(
            parent.identity.session_id.as_str(),
            None,
            "/code \"fork source\"".to_owned(),
        )
        .await
        .unwrap();
    let error = application
        .fork_session(
            parent.identity.session_id.as_str(),
            Some(deleted.to_owned()),
            None,
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("deleted session IDs"));
    assert!(application.state.session(deleted).await.is_none());
    application.shutdown().await.unwrap();
    drop(application);
    let application = open(&data).await;
    assert!(
        application
            .create_session(
                workspace.workspace_id.clone(),
                Some(deleted.to_owned()),
                None
            )
            .await
            .is_err()
    );
    assert!(
        application
            .fork_session(
                parent.identity.session_id.as_str(),
                Some(deleted.to_owned()),
                None
            )
            .await
            .is_err()
    );
    assert!(
        application
            .create_session(workspace.workspace_id, None, None)
            .await
            .is_ok()
    );
    application.shutdown().await.unwrap();
}
