use super::*;
use ternilo_kernel::SessionEventStore as _;
use ternilo_protocol::{RunId, SessionEventKind};

fn event(seq: u64) -> SessionEvent {
    SessionEvent {
        seq,
        occurred_at_ms: 100 + seq,
        run_id: RunId::new("repair-run"),
        kind: SessionEventKind::AssistantMessageDelta {
            step: 1,
            delta: "preserved history 中文".to_owned(),
        },
    }
}

fn fixture(bytes: &[u8]) -> (tempfile::TempDir, SessionId, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let session = SessionId::new("session-to-repair");
    let directory = root.path().join("sessions");
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join(format!(
        "{}.jsonl",
        crate::event_store::encode_id(session.as_str())
    ));
    std::fs::write(&path, bytes).unwrap();
    (root, session, path)
}

#[tokio::test]
async fn repair_preserves_exact_backup_and_sequence_before_new_appends() {
    let first = serde_json::to_string(&event(0)).unwrap() + "\n";
    let damaged = format!("{first}{{\"seq\":1,\"occurred_at_ms\":");
    let (root, session, path) = fixture(damaged.as_bytes());
    let proposal = repair_session_log(root.path(), &session, false)
        .await
        .unwrap();
    assert_eq!(
        proposal.action,
        SessionLogRepairAction::DiscardIncompleteTail
    );
    assert_eq!(proposal.valid_records, 1);
    assert_eq!(proposal.bytes_after, first.len() as u64);
    assert!(!proposal.applied);
    assert!(proposal.backup_path.is_none());
    assert_eq!(std::fs::read(&path).unwrap(), damaged.as_bytes());
    let report = repair_session_log(root.path(), &session, true)
        .await
        .unwrap();
    assert!(report.applied);
    let backup = report.backup_path.unwrap();
    assert_eq!(std::fs::read(&backup).unwrap(), damaged.as_bytes());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            std::fs::metadata(&backup).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    let store = crate::event_store::JsonlEventStore::new(&root.path().join("sessions"), &session);
    assert_eq!(store.load().await.unwrap(), vec![event(0)]);
    store.append(event(1)).await.unwrap();
    assert_eq!(store.load().await.unwrap(), vec![event(0), event(1)]);
    assert_eq!(
        repair_session_log(root.path(), &session, true)
            .await
            .unwrap()
            .action,
        SessionLogRepairAction::None
    );
    assert_eq!(
        std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
        2
    );
}

#[tokio::test]
async fn valid_unterminated_record_is_preserved_and_separated_before_append() {
    let bytes = serde_json::to_vec(&event(0)).unwrap();
    let (root, session, path) = fixture(&bytes);
    let report = repair_session_log(root.path(), &session, true)
        .await
        .unwrap();
    assert_eq!(report.action, SessionLogRepairAction::AppendNewline);
    assert_eq!(report.valid_records, 1);
    assert_eq!(std::fs::read(report.backup_path.unwrap()).unwrap(), bytes);
    assert_eq!(std::fs::read(path).unwrap().last(), Some(&b'\n'));
    let store = crate::event_store::JsonlEventStore::new(&root.path().join("sessions"), &session);
    store.append(event(1)).await.unwrap();
    assert_eq!(store.load().await.unwrap(), vec![event(0), event(1)]);
}

#[tokio::test]
async fn invalid_complete_records_and_sequence_gaps_never_change_the_original() {
    let good = serde_json::to_string(&event(0)).unwrap() + "\n";
    for bytes in [
        format!("{good}{{\"seq\":\n"),
        format!("{good}not-json"),
        format!("{good}{{}}"),
        format!("{good}{}", serde_json::to_string(&event(2)).unwrap()),
        format!("{good}\n{{\"seq\":"),
        format!("bad record\n{good}{{\"seq\":"),
    ] {
        let (root, session, path) = fixture(bytes.as_bytes());
        assert!(
            repair_session_log(root.path(), &session, true)
                .await
                .is_err()
        );
        assert_eq!(std::fs::read(&path).unwrap(), bytes.as_bytes());
        assert_eq!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1
        );
    }
}

#[tokio::test]
async fn active_directory_owner_prevents_maintenance_and_releases_after_inspection() {
    let (root, session, path) = fixture(b"{\"seq\":");
    let lock = crate::application::acquire_data_lock(root.path())
        .await
        .unwrap();
    for apply in [false, true] {
        let error = repair_session_log(root.path(), &session, apply)
            .await
            .unwrap_err();
        assert_eq!(error.code, ternilo_protocol::ErrorCode::PolicyDenied);
        assert_eq!(std::fs::read(&path).unwrap(), b"{\"seq\":");
    }
    drop(lock);
    repair_session_log(root.path(), &session, false)
        .await
        .unwrap();
    let _lock = crate::application::acquire_data_lock(root.path())
        .await
        .unwrap();
}

#[tokio::test]
async fn truncated_utf8_tail_is_repaired_without_rewriting_prior_events() {
    let first = serde_json::to_string(&event(0)).unwrap() + "\n";
    let second = serde_json::to_vec(&event(1)).unwrap();
    let cut = second.iter().position(|byte| *byte == 0xe4).unwrap() + 1;
    let mut bytes = first.as_bytes().to_vec();
    bytes.extend_from_slice(&second[..cut]);
    let (root, session, path) = fixture(&bytes);
    let report = repair_session_log(root.path(), &session, true)
        .await
        .unwrap();
    assert_eq!(report.action, SessionLogRepairAction::DiscardIncompleteTail);
    assert_eq!(std::fs::read(path).unwrap(), first.as_bytes());
    assert_eq!(std::fs::read(report.backup_path.unwrap()).unwrap(), bytes);
}
