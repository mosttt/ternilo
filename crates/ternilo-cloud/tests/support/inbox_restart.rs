use super::*;

#[tokio::test]
async fn sqlite_stop_and_send_is_durable_and_waits_for_worker_completion() {
    restart_contract(false).await;
}

#[tokio::test]
async fn sqlite_later_stop_clears_the_pending_queue_restart() {
    restart_contract(true).await;
}

async fn restart_contract(later_stop: bool) {
    let directory = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}",
        directory.path().join("restart.sqlite").display()
    );
    let control = ControlStore::connect(&url, None, SecretCipher::from_key([7; 32]), 4)
        .await
        .unwrap();
    restart_queue_contract(control, later_stop).await;
}

#[tokio::test]
#[ignore = "requires TERNILO_CLOUD_TEST_DATABASE_URL pointing to a disposable PostgreSQL database"]
async fn postgres_stop_and_send_preserves_authorization_and_waits_for_cleanup() {
    let admin = std::env::var("TERNILO_CLOUD_TEST_DATABASE_URL").unwrap();
    assert!(admin.contains("ternilo_cloud_test"));
    reset_database(&admin).await;
    let runtime = support::database_url_for_role(
        &admin,
        "ternilo_inbox_runtime_test",
        "inbox-runtime-password",
    );
    let control = ControlStore::connect(&runtime, Some(&admin), SecretCipher::from_key([7; 32]), 4)
        .await
        .unwrap();
    restart_queue_contract(control, false).await;
}

#[expect(
    clippy::too_many_lines,
    reason = "Verify acceptance, reopen, new input and worker completion in one queue lifecycle."
)]
async fn restart_queue_contract(control: ControlStore, later_stop: bool) {
    let cloud = CloudStore::from_database(control.database().clone())
        .await
        .unwrap();
    worker_storage::bind_workers(&cloud, &["worker-restart"], "restart-storage").await;
    let now = 2_000_000_000_000;
    let owner = user(&control, "restart-owner", "Restart owner", now).await;
    let tenant = control
        .create_tenant(
            &owner,
            "restart",
            "Restart",
            TenantQuota {
                max_nodes: 2,
                max_concurrent_runs: 8,
                monthly_model_tokens: 100_000,
                max_secrets: 4,
            },
            now + 1,
        )
        .await
        .unwrap()
        .tenant_id;
    let project = control
        .create_project(&owner, &tenant, "Restart", now + 2)
        .await
        .unwrap();
    let workspace = control
        .create_cloud_workspace(&owner, &tenant, &project.project_id, "Restart", now + 3)
        .await
        .unwrap();
    let session = SessionId::new("restart-session");
    let catalog = Catalog::new("inbox-test-catalog");
    let policy = policy();
    let mut receipts = Vec::new();
    let mut started = None;
    for (offset, name) in ["head", "tail", "new-input"].iter().enumerate() {
        let time = now + 10 + offset as u64 * 10;
        let run = format!("restart-{name}");
        let compiled = compile(
            &policy,
            &catalog,
            &tenant,
            &owner.user_id,
            &project.project_id,
            &workspace.workspace_id,
            &session,
            &run,
            name,
        );
        let reservation = reserve(&control, &owner, &tenant, &run, time).await;
        receipts.push(
            cloud
                .enqueue_session_submission(
                    &compiled,
                    &reservation,
                    &request(&run, name, SubmissionDelivery::Queue),
                    time + 1,
                )
                .await
                .unwrap(),
        );
        if offset == 0 {
            let claim = cloud
                .claim_run("worker-restart", Duration::from_secs(30), time + 2)
                .await
                .unwrap()
                .unwrap();
            started = cloud
                .start_run(claim, "worker-restart", Duration::from_secs(30), time + 3)
                .await
                .unwrap();
        } else if offset == 1 {
            let accepted = tokio::time::timeout(
                Duration::from_millis(500),
                cloud.restart_session_inbox(
                    &tenant,
                    &owner.user_id,
                    &session,
                    &receipts[1].submission.id,
                    time + 2,
                ),
            )
            .await
            .unwrap()
            .unwrap();
            assert_eq!(accepted.id, receipts[1].submission.id);
            assert_eq!(
                cloud
                    .get_run(&tenant, &receipts[0].submission.run_id)
                    .await
                    .unwrap()
                    .state,
                CloudRunState::CancelRequested
            );
            cloud
                .restart_session_inbox(
                    &tenant,
                    &owner.user_id,
                    &session,
                    &receipts[1].submission.id,
                    time + 3,
                )
                .await
                .unwrap();
        }
    }
    let reopened = CloudStore::from_database(control.database().clone())
        .await
        .unwrap();
    let pending = reopened
        .session_inbox(&tenant, &owner.user_id, &session)
        .await
        .unwrap();
    assert!(
        pending.paused,
        "a new input must not undo a pending stop-and-send"
    );
    assert_eq!(
        pending.active_run_id,
        Some(receipts[0].submission.run_id.clone())
    );
    assert!(
        pending
            .items
            .iter()
            .skip(1)
            .all(|item| item.placement == SubmissionPlacement::Queued)
    );
    if later_stop {
        reopened
            .pause_session_inbox(&tenant, &owner.user_id, &session, None, now + 35)
            .await
            .unwrap();
    }
    let started = started.unwrap();
    reopened
        .finish_run(
            &started,
            "worker-restart",
            TerminalState::Cancelled,
            None,
            None,
            now + 36,
        )
        .await
        .unwrap();
    let final_inbox = reopened
        .session_inbox(&tenant, &owner.user_id, &session)
        .await
        .unwrap();
    assert_eq!(final_inbox.paused, later_stop);
    assert_eq!(final_inbox.items.len(), 2);
    assert_eq!(
        final_inbox.items[0].provenance,
        receipts[1].submission.provenance
    );
    assert_eq!(
        final_inbox.items[1].provenance,
        receipts[2].submission.provenance
    );
    if later_stop {
        assert_eq!(final_inbox.active_run_id, None);
        assert!(
            final_inbox
                .items
                .iter()
                .all(|item| item.placement == SubmissionPlacement::Queued)
        );
    } else {
        assert_eq!(
            final_inbox.active_run_id,
            Some(receipts[1].submission.run_id.clone())
        );
        assert!(
            final_inbox
                .items
                .iter()
                .all(|item| item.placement == SubmissionPlacement::Running)
        );
    }
    assert!(
        reopened
            .claim_run("worker-restart", Duration::from_secs(30), now + 37)
            .await
            .unwrap()
            .is_none(),
        "old residency must still block execution until cleanup is confirmed"
    );
}
