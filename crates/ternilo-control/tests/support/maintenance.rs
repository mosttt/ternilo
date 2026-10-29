use super::*;
use ternilo_protocol::{ClientUpdateInfo, ClientUpdatePhase, ClientUpdateProgress};
use ternilo_transport::{
    ClientUpdateCommand, MAINTENANCE_PROTOCOL_VERSION, MaintenanceHello, MaintenanceOperation,
    MaintenanceReply,
};

#[expect(
    clippy::too_many_lines,
    reason = "Sequential lifecycle contract retains one enrollment and update operation"
)]
pub(super) async fn contract(store: &ControlStore, fixture: &EdgeFixture, node: &NodePrincipal) {
    let tenant = &fixture.tenant_a;
    let executor = &node.executor_id;
    let now = fixture.now + 500_000;
    let mut hello = MaintenanceHello {
        protocol_version: MAINTENANCE_PROTOCOL_VERSION,
        executor_id: executor.clone(),
        instance_nonce: "maintenance-first".into(),
        execution_protocol_version: u32::MAX,
        info: ClientUpdateInfo {
            protocol_version: 1,
            version: "0.2.29".into(),
            target: Some("x86_64-unknown-linux-gnu".into()),
            enabled: true,
            data_epoch: 1,
            restart_method: Some("supervisor".into()),
            progress: None,
        },
    };
    store.maintenance_hello(node, &hello, now).await.unwrap();
    let state = store
        .maintenance_state(&fixture.alice, tenant, executor)
        .await
        .unwrap()
        .unwrap();
    assert!(state.online(now));
    assert_eq!(state.hello.execution_protocol_version, u32::MAX);
    assert!(
        store
            .maintenance_state(&fixture.bob, tenant, executor)
            .await
            .is_err()
    );
    let id = store
        .enqueue_maintenance_command(
            &fixture.alice,
            tenant,
            executor,
            MaintenanceOperation::Status,
            now,
        )
        .await
        .unwrap();
    let command = store
        .maintenance_poll(node, &hello.instance_nonce, &hello.info, None, now + 1)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(command.id, id);
    assert!(matches!(command.command, ClientUpdateCommand::Status));
    let reply = MaintenanceReply {
        id: id.clone(),
        info: Some(hello.info.clone()),
        error: None,
    };
    assert!(
        store
            .maintenance_poll(
                node,
                &hello.instance_nonce,
                &hello.info,
                Some(&reply),
                now + 2
            )
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        store
            .take_maintenance_reply(tenant, executor, &id)
            .await
            .unwrap()
            .unwrap()
            .info
            .unwrap()
            .version,
        "0.2.29"
    );
    // A retry after acknowledgement was consumed must not recreate the command.
    assert!(
        store
            .maintenance_poll(
                node,
                &hello.instance_nonce,
                &hello.info,
                Some(&reply),
                now + 3
            )
            .await
            .unwrap()
            .is_none()
    );
    // A live lease never overrides suspension, and polling cannot acknowledge commands while suspended.
    let revision = store
        .computer_details(&fixture.alice, tenant, executor, true)
        .await
        .unwrap()
        .management
        .revision;
    let suspended = store
        .set_computer_suspended(
            &fixture.alice,
            tenant,
            executor,
            true,
            true,
            revision,
            now + 3,
        )
        .await
        .unwrap();
    assert!(
        store
            .enqueue_maintenance_command(
                &fixture.alice,
                tenant,
                executor,
                MaintenanceOperation::Status,
                now + 3
            )
            .await
            .is_err()
    );
    assert!(
        store
            .maintenance_poll(node, &hello.instance_nonce, &hello.info, None, now + 3)
            .await
            .is_err()
    );
    store
        .set_computer_suspended(
            &fixture.alice,
            tenant,
            executor,
            true,
            false,
            suspended.revision,
            now + 3,
        )
        .await
        .unwrap();
    let cancel_id = store
        .enqueue_maintenance_command(
            &fixture.alice,
            tenant,
            executor,
            MaintenanceOperation::Cancel {
                id: "cancel-fixture".into(),
            },
            now + 3,
        )
        .await
        .unwrap();
    let cancel = store
        .maintenance_poll(node, &hello.instance_nonce, &hello.info, None, now + 3)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(cancel.command, ClientUpdateCommand::Cancel { id } if id == "cancel-fixture"));
    let mut invalid_reply = MaintenanceReply {
        id: cancel_id.clone(),
        info: Some(hello.info.clone()),
        error: None,
    };
    invalid_reply.info.as_mut().unwrap().version = "9.9.9".into();
    assert!(
        store
            .maintenance_poll(
                node,
                &hello.instance_nonce,
                &hello.info,
                Some(&invalid_reply),
                now + 3
            )
            .await
            .is_err()
    );
    invalid_reply.info = None;
    invalid_reply.error = Some("no update is active".into());
    store
        .maintenance_poll(
            node,
            &hello.instance_nonce,
            &hello.info,
            Some(&invalid_reply),
            now + 3,
        )
        .await
        .unwrap();
    assert_eq!(
        store
            .take_maintenance_reply(tenant, executor, &cancel_id)
            .await
            .unwrap()
            .unwrap()
            .error
            .as_deref(),
        Some("no update is active")
    );
    let job = ternilo_control::ClientUpdateJob {
        progress: ClientUpdateProgress {
            id: "maintenance-operation".into(),
            version: "0.2.30".into(),
            phase: ClientUpdatePhase::Preparing,
            revision: 0,
            error: None,
        },
        target: "x86_64-unknown-linux-gnu".into(),
        manifest: "signed-manifest-fixture".into(),
        signature: "fixture-signature".into(),
        created_at_ms: now,
        expires_at_ms: now + 60_000,
    };
    store
        .create_client_update_job(
            &fixture.alice,
            tenant,
            executor,
            &job,
            "initial-download-grant",
        )
        .await
        .unwrap();
    store
        .enqueue_maintenance_command(
            &fixture.alice,
            tenant,
            executor,
            MaintenanceOperation::Start {
                id: job.progress.id.clone(),
            },
            now + 4,
        )
        .await
        .unwrap();
    let command = store
        .maintenance_poll(node, &hello.instance_nonce, &hello.info, None, now + 5)
        .await
        .unwrap()
        .unwrap();
    let ClientUpdateCommand::Start { request } = &command.command else {
        panic!("expected update")
    };
    assert_eq!(request.manifest, job.manifest);
    store
        .client_update_download(
            tenant,
            executor,
            &job.progress.id,
            &request.download_token,
            now + 6,
        )
        .await
        .unwrap();
    let mut tx = store.database().tenant_transaction(tenant).await.unwrap();
    let queued: Vec<String> = sqlx::query_scalar("SELECT operation_json FROM control_maintenance_commands WHERE tenant_id=$1 AND executor_id=$2").bind(tenant.as_str()).bind(executor.as_str()).fetch_all(&mut *tx).await.unwrap();
    assert!(!queued.join("").contains(&request.download_token));
    tx.commit().await.unwrap();
    // A new instance keeps maintenance available, but cannot be overwritten by the old instance.
    let previous = hello.instance_nonce.clone();
    hello.instance_nonce = "maintenance-second".into();
    hello.info.version = "0.2.30".into();
    hello.info.progress = Some(ClientUpdateProgress {
        phase: ClientUpdatePhase::Completed,
        revision: 3,
        ..job.progress.clone()
    });
    store
        .maintenance_hello(node, &hello, now + 7)
        .await
        .unwrap();
    assert!(
        store
            .maintenance_poll(node, &previous, &hello.info, None, now + 8)
            .await
            .is_err()
    );
    assert_eq!(
        store
            .client_update_job(&fixture.alice, tenant, executor)
            .await
            .unwrap()
            .unwrap()
            .progress
            .phase,
        ClientUpdatePhase::Completed
    );
    // An already completed operation cannot block status reporting or mint another grant.
    assert!(
        store
            .maintenance_poll(node, &hello.instance_nonce, &hello.info, None, now + 9)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .take_maintenance_reply(tenant, executor, &command.id)
            .await
            .unwrap()
            .unwrap()
            .error
            .is_some()
    );
    assert!(
        store
            .enqueue_maintenance_command(
                &fixture.alice,
                tenant,
                executor,
                MaintenanceOperation::Status,
                now + 60_000
            )
            .await
            .is_err()
    );
    let mut revoked = node.clone();
    revoked.credential_id = "revoked-fixture".into();
    assert!(
        store
            .maintenance_poll(&revoked, &hello.instance_nonce, &hello.info, None, now + 10)
            .await
            .is_err()
    );
}
