use super::session_data::{
    MetadataReadPlan, metadata_read_plan, next_sequence, validate_event_page,
};
use super::subscriptions::{matching_dirty, matching_edge_dirty};
use super::workbench::session_activity;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use ternilo_protocol::{ErrorCode, RunId, SessionEventKind, UserId};

use super::*;

#[test]
fn cursor_contract_uses_last_delivered_and_next_expected_sequences() {
    assert_eq!(next_sequence(None), 0);
    assert_eq!(next_sequence(Some(0)), 1);
    assert_eq!(next_sequence(Some(41)), 42);
    assert!(validate_event_page(None, &[event(0), event(1)]).is_ok());
    assert!(validate_event_page(Some(5), &[event(6), event(7)]).is_ok());
    assert!(validate_event_page(Some(5), &[event(7)]).is_err());
}

#[test]
fn close_messages_end_the_live_receive_loop() {
    assert_eq!(
        classify_incoming(&Message::text("hello")),
        IncomingLiveMessage::Text("hello")
    );
    assert_eq!(
        classify_incoming(&Message::close()),
        IncomingLiveMessage::Close
    );
    assert_eq!(
        classify_incoming(&Message::ping(Vec::new())),
        IncomingLiveMessage::Ignore
    );
}

#[test]
fn subscription_ids_are_monotonic_and_queued_old_generation_frames_do_not_cross() {
    assert!(valid_next_subscription_id(None, 1));
    assert!(valid_next_subscription_id(Some(1), 2));
    assert!(!valid_next_subscription_id(None, 0));
    assert!(!valid_next_subscription_id(Some(2), 2));
    assert!(!valid_next_subscription_id(Some(2), 1));

    let old = LiveServerFrame::EventBatch {
        subscription_id: 1,
        session_id: SessionId::new("old-session"),
        reset: false,
        complete: true,
        events: vec![event(0)],
        next_seq: 1,
    };
    assert!(frame_matches_active_subscription(&old, Some(1)));
    assert!(!frame_matches_active_subscription(&old, Some(2)));
    assert!(!frame_matches_active_subscription(&old, None));
    assert!(frame_matches_active_subscription(
        &error_frame(None, HarnessError::invalid("connection error")),
        Some(2),
    ));
}

#[test]
fn hello_requires_matching_version_token_and_tenant() {
    let valid = serde_json::to_string(&LiveClientFrame::Hello {
        protocol_version: LIVE_PROTOCOL_VERSION,
        bearer_token: Some("signed.token".to_owned()),
        tenant_id: Some(TenantId::new("tenant")),
    })
    .unwrap();
    assert_eq!(parse_hello(&valid).unwrap().1, TenantId::new("tenant"));

    let missing_token = serde_json::to_string(&LiveClientFrame::Hello {
        protocol_version: LIVE_PROTOCOL_VERSION,
        bearer_token: None,
        tenant_id: Some(TenantId::new("tenant")),
    })
    .unwrap();
    assert_eq!(
        parse_hello(&missing_token).unwrap_err().code,
        ErrorCode::PolicyDenied
    );

    let wrong_version = serde_json::to_string(&LiveClientFrame::Hello {
        protocol_version: LIVE_PROTOCOL_VERSION + 1,
        bearer_token: Some("signed.token".to_owned()),
        tenant_id: Some(TenantId::new("tenant")),
    })
    .unwrap();
    assert_eq!(
        parse_hello(&wrong_version).unwrap_err().code,
        ErrorCode::Composition
    );
}

#[tokio::test]
async fn unsubscribe_aborts_the_active_generation() {
    struct DropSignal(Arc<AtomicBool>);
    impl Drop for DropSignal {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    let dropped = Arc::new(AtomicBool::new(false));
    let task_dropped = Arc::clone(&dropped);
    let task = tokio::spawn(async move {
        let _signal = DropSignal(task_dropped);
        std::future::pending::<()>().await;
    });
    tokio::task::yield_now().await;
    let mut active = Some(ActiveSubscription {
        id: 9,
        session_id: SessionId::new("session"),
        task,
    });
    cancel_subscription(&mut active, Some(8));
    assert!(active.is_some());
    cancel_subscription(&mut active, Some(9));
    tokio::task::yield_now().await;
    assert!(active.is_none());
    assert!(dropped.load(Ordering::SeqCst));
}

#[test]
fn dirty_notifications_are_owner_and_session_isolated() {
    let tenant = TenantId::new("tenant-a");
    let user = UserId::new("user-a");
    let session = SessionId::new("session-a");
    let notification = || CloudLiveNotification::Session {
        tenant_id: tenant.clone(),
        user_id: Some(user.clone()),
        session_id: Some(session.clone()),
        dirty: SessionLiveDirty {
            inbox: true,
            ..SessionLiveDirty::default()
        },
        activity: false,
        workbench: false,
    };
    assert!(matching_dirty(notification(), &tenant, &user, &session).is_some());
    assert!(matching_dirty(notification(), &TenantId::new("tenant-b"), &user, &session,).is_none());
    assert!(matching_dirty(notification(), &tenant, &user, &SessionId::new("session-b")).is_none());
    let resources = || CloudLiveNotification::ResourcesChanged {
        tenant_id: tenant.clone(),
    };
    assert!(matching_dirty(resources(), &tenant, &user, &session).is_some());
    assert!(matching_dirty(resources(), &TenantId::new("tenant-b"), &user, &session).is_none());
}

#[test]
fn edge_dirty_notifications_are_tenant_executor_and_node_session_isolated() {
    let session = edge_session();
    let notification = |node_session_id| EdgeLiveNotification::Session {
        tenant_id: session.tenant_id.clone(),
        executor_id: session.executor_id.clone(),
        node_session_id,
        dirty: SessionLiveDirty {
            inbox: true,
            ..SessionLiveDirty::default()
        },
        refresh_events: false,
        workbench: false,
        activity: None,
    };
    assert!(
        matching_edge_dirty(
            notification(Some(session.node_session_id.clone())),
            &session.tenant_id,
            &session,
        )
        .is_some()
    );
    assert!(matching_edge_dirty(notification(None), &session.tenant_id, &session).is_some());
    assert!(
        matching_edge_dirty(
            notification(Some(SessionId::new("another-node-session"))),
            &session.tenant_id,
            &session,
        )
        .is_none()
    );
    assert!(
        matching_edge_dirty(
            EdgeLiveNotification::Rescan {
                tenant_id: session.tenant_id.clone(),
                executor_id: ExecutorId::new("another-executor"),
            },
            &session.tenant_id,
            &session,
        )
        .is_none()
    );
}

#[test]
fn activity_uses_durable_cloud_state() {
    let mut session = cloud_session(CloudSessionState::Running);
    assert!(session_activity(session.clone()).running);
    session.state = CloudSessionState::Succeeded;
    assert!(!session_activity(session).running);
}

#[test]
fn combined_metadata_snapshot_reads_each_shared_source_once() {
    assert_eq!(
        metadata_read_plan(SessionLiveReadMask {
            stats: true,
            projection: true,
            profile: true,
            ..SessionLiveReadMask::default()
        }),
        MetadataReadPlan {
            event_reads: 1,
            profile_reads: 1,
        }
    );
    assert_eq!(
        metadata_read_plan(SessionLiveReadMask {
            inbox: true,
            questions: true,
            agent_team: true,
            ..SessionLiveReadMask::default()
        }),
        MetadataReadPlan {
            event_reads: 0,
            profile_reads: 0,
        }
    );
}

fn event(seq: u64) -> SessionEvent {
    SessionEvent {
        seq,
        occurred_at_ms: seq,
        run_id: RunId::new("run"),
        kind: SessionEventKind::TurnStarted,
    }
}

fn cloud_session(state: CloudSessionState) -> CloudSessionRecord {
    CloudSessionRecord {
        tenant_id: TenantId::new("tenant"),
        session_id: SessionId::new("session"),
        user_id: UserId::new("user"),
        project_id: "project".to_owned(),
        workspace_id: ternilo_protocol::WorkspaceId::new("workspace"),
        parent_session_id: None,
        subagent: None,
        agent_id: ternilo_protocol::AgentId::new("agent"),
        title: "Session".to_owned(),
        archived_at_ms: None,
        state,
        execution: None,
        permissions: ternilo_protocol::PermissionPreset::WorkspaceWrite,
        model: None,
        reserved_model_tokens: 1,
        agent_preset: "standard".to_owned(),
        profile_plugins: Vec::new(),
        mode: ternilo_protocol::SessionMode::Execute,
        last_seq: None,
        created_at_ms: 1,
        updated_at_ms: 2,
    }
}

fn edge_session() -> EdgeSessionRecord {
    EdgeSessionRecord {
        tenant_id: TenantId::new("tenant"),
        session_id: SessionId::new("browser-session"),
        workspace_id: ternilo_protocol::WorkspaceId::new("workspace"),
        executor_id: ExecutorId::new("executor"),
        owner_user_id: UserId::new("user"),
        node_session_id: SessionId::new("node-session"),
        metadata: ternilo_control::EdgeSessionMetadata {
            server_model: None,
            parent_session_id: None,
            subagent: None,
            title: "Edge Session".to_owned(),
            archived_at_ms: None,
            blank: false,
            permissions: ternilo_protocol::PermissionPreset::WorkspaceWrite,
            model: serde_json::json!({ "provider": "profile_default" }),
            agent_preset: "standard".to_owned(),
            preset_plugins: Vec::new(),
            profile_plugins: Vec::new(),
            mode: ternilo_protocol::SessionMode::Execute,
            created_at_ms: 1,
            updated_at_ms: 2,
        },
        last_event_seq: None,
        created_at_ms: 1,
        updated_at_ms: 2,
    }
}
