use super::*;
use serde_json::json;
use ternilo_protocol::SessionId;
use ternilo_transport::ApplicationOperation;

#[tokio::test]
async fn concurrent_duplicate_commands_share_one_cached_reply() {
    let cache = ReplyCache::open_in_memory().await.unwrap();
    let command = ExecutorCommand {
        input_provenance: Some(ternilo_protocol::InputProvenance {
            run_id: None,
            input_id: ternilo_protocol::SubmissionId::new("input-1"),
            author: ternilo_protocol::InputAuthor::Account {
                user_id: ternilo_protocol::UserId::new("alice-id"),
                username: "alice".to_owned(),
            },
        }),
        command_id: CommandId::new("command-1"),
        scope: ExecutorScope {
            tenant_id: ternilo_protocol::TenantId::new("tenant"),
            user_id: ternilo_protocol::UserId::new("user"),
        },
        issued_at_ms: 1,
        expires_at_ms: u64::MAX,
        body: ExecutorCommandBody::Application {
            request: ApplicationOperation::Snapshot,
        },
    };
    assert!(matches!(
        cache.claim(&command).await.unwrap(),
        ReplyClaim::Execute,
    ));
    let mut forged = command.clone();
    forged.input_provenance.as_mut().unwrap().author = ternilo_protocol::InputAuthor::Account {
        user_id: ternilo_protocol::UserId::new("bob-id"),
        username: "bob".to_owned(),
    };
    let mut changed_body = command.clone();
    changed_body.body = ExecutorCommandBody::Application {
        request: ApplicationOperation::Catalog,
    };
    for different in [&forged, &changed_body] {
        assert!(
            cache.claim(different).await.is_err(),
            "in-flight command content is immutable"
        );
    }
    let waiter = match cache.claim(&command).await.unwrap() {
        ReplyClaim::Wait(waiter) => waiter,
        ReplyClaim::Execute | ReplyClaim::Completed(_) => {
            panic!("an in-flight duplicate must wait")
        }
    };
    let reply = CommandReply::success(command.command_id.clone(), 1, json!({ "ok": true }));
    let reply = cache.complete(&command, reply).await;

    assert_eq!(waiter.await.unwrap(), reply);
    for different in [&forged, &changed_body] {
        assert!(
            cache.claim(different).await.is_err(),
            "cached command content is immutable"
        );
    }
    match cache.claim(&command).await.unwrap() {
        ReplyClaim::Completed(cached) => assert_eq!(cached, reply),
        ReplyClaim::Execute | ReplyClaim::Wait(_) => {
            panic!("a completed duplicate must use the cached reply")
        }
    }
}

#[tokio::test]
async fn workspace_location_reply_is_not_persisted_in_the_node_command_cache() {
    for operation in [
        ApplicationOperation::WorkspaceLocation {
            workspace_id: ternilo_protocol::WorkspaceId::new("workspace"),
        },
        ApplicationOperation::AgentPresetGet {
            preset_id: "standard".to_owned(),
        },
    ] {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("node-transport.sqlite3");
        let command = ExecutorCommand {
            input_provenance: None,
            command_id: CommandId::new("private-location"),
            scope: ExecutorScope {
                tenant_id: ternilo_protocol::TenantId::new("tenant"),
                user_id: ternilo_protocol::UserId::new("owner"),
            },
            issued_at_ms: 1,
            expires_at_ms: u64::MAX,
            body: ExecutorCommandBody::Application { request: operation },
        };
        {
            let cache = ReplyCache::open(path.clone()).await.unwrap();
            assert!(matches!(
                cache.claim(&command).await.unwrap(),
                ReplyClaim::Execute
            ));
            cache
            .complete(
                &command,
                CommandReply::success(
                    command.command_id.clone(),
                    2,
                    json!({
                        "path": "/private/location", "home": "/private", "created_at_ms": 1,
                        "base_profile": {"plugins":[{"id":"private-launch-config","config":{"path":"/private/base"}}]},
                    }),
                ),
            )
            .await;
            assert!(
                matches!(cache.claim(&command).await.unwrap(), ReplyClaim::Execute),
                "repeatable private read replies must not remain in the in-memory cache"
            );
        }
        let reopened = ReplyCache::open(path).await.unwrap();
        assert!(
            matches!(reopened.claim(&command).await.unwrap(), ReplyClaim::Execute),
            "repeatable private read replies must not be restored from disk"
        );
    }
}

#[tokio::test]
async fn completed_command_reply_survives_node_transport_reopen() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("node-transport.sqlite3");
    let command = ExecutorCommand {
        input_provenance: None,
        command_id: CommandId::new("command-restart"),
        scope: ExecutorScope {
            tenant_id: ternilo_protocol::TenantId::new("tenant"),
            user_id: ternilo_protocol::UserId::new("user"),
        },
        issued_at_ms: 1,
        expires_at_ms: u64::MAX,
        body: ExecutorCommandBody::Application {
            request: ApplicationOperation::Snapshot,
        },
    };
    let reply = CommandReply::success(command.command_id.clone(), 2, json!({ "ok": true }));
    {
        let cache = ReplyCache::open(path.clone()).await.unwrap();
        assert!(matches!(
            cache.claim(&command).await.unwrap(),
            ReplyClaim::Execute
        ));
        assert_eq!(cache.complete(&command, reply.clone()).await, reply);
    }

    let reopened = ReplyCache::open(path).await.unwrap();
    match reopened.claim(&command).await.unwrap() {
        ReplyClaim::Completed(cached) => assert_eq!(cached, reply),
        ReplyClaim::Execute | ReplyClaim::Wait(_) => {
            panic!("a completed command must remain deduplicated after restart")
        }
    }
}

#[tokio::test]
async fn file_downloads_share_inflight_work_without_retaining_reply_bodies() {
    let cache = ReplyCache::open_in_memory().await.unwrap();
    let command = ExecutorCommand {
        input_provenance: None,
        command_id: CommandId::new("file-download"),
        scope: ExecutorScope {
            tenant_id: ternilo_protocol::TenantId::new("tenant"),
            user_id: ternilo_protocol::UserId::new("user"),
        },
        issued_at_ms: 1,
        expires_at_ms: u64::MAX,
        body: ExecutorCommandBody::Application {
            request: ApplicationOperation::SessionFileContent {
                session_id: SessionId::new("files"),
                file_id: "upload-1-0".to_owned(),
            },
        },
    };
    assert!(matches!(
        cache.claim(&command).await.unwrap(),
        ReplyClaim::Execute
    ));
    let ReplyClaim::Wait(waiter) = cache.claim(&command).await.unwrap() else {
        panic!("an in-flight file duplicate should share the read");
    };
    let reply = CommandReply::success(
        command.command_id.clone(),
        2,
        json!({ "content_base64": "cHJpdmF0ZQ==" }),
    );
    assert_eq!(cache.complete(&command, reply.clone()).await, reply);
    assert_eq!(waiter.await.unwrap(), reply);
    assert!(matches!(
        cache.claim(&command).await.unwrap(),
        ReplyClaim::Execute
    ));
    assert!(matches!(
        cache.store.claim_node_command(&command, 3).await.unwrap(),
        NodeCommandClaim::Execute
    ));
}
