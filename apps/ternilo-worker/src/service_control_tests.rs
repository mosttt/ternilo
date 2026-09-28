use std::{sync::Arc, time::Duration};

use ternilo_kernel::{HarnessSession, HostEnvironment, HostPolicy, RunCancellation};
use ternilo_protocol::{
    AgentId, ErrorCode, PluginEntry, RunLimits, SessionId, SessionIdentity, SessionServiceStatus,
    TenantId, UserId, WorkspaceBinding, WorkspaceId,
};

use super::start_child_service;

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "Verify the real startup timeout, process-tree cleanup, source state, and physical lease release together."
)]
async fn startup_control_timeout_stops_a_blocked_real_service_and_releases_its_directory() {
    let directory = tempfile::tempdir().unwrap();
    let workspace = directory.path().join("workspace");
    tokio::fs::create_dir(&workspace).await.unwrap();
    let coordinator = ternilo_local::DirectoryCoordinator::new(directory.path().join("locks"));
    let mut profile = ternilo_local::local_profile();
    profile.plugins.push(PluginEntry {
        id: "blocked-service".to_owned(),
        kind: ternilo_builtins::LSP_STDIO_KIND.to_owned(),
        enabled: true,
        config: serde_json::json!({
            "server_name": "blocked-service",
            "command": "/bin/sh",
            "args": ["-c", "printf '%s' \"$$\" > service.pid; (while :; do printf x >> heartbeat; sleep 0.03; done) & wait"],
            "timeout_ms": 60_000,
        }),
    });
    let harness = Arc::new(
        HarnessSession::boot(
            &ternilo_local::catalog().unwrap(),
            &profile,
            HostEnvironment::memory(
                SessionIdentity {
                    tenant_id: TenantId::new("service-test"),
                    user_id: UserId::new("owner"),
                    agent_id: AgentId::new("agent"),
                    session_id: SessionId::new("session"),
                },
                Some(WorkspaceBinding {
                    workspace_id: WorkspaceId::new("workspace"),
                    path: workspace.to_str().unwrap().to_owned(),
                }),
                HostPolicy::local(RunLimits::default()),
            )
            .with_workspace_execution(coordinator.bind("family".to_owned(), workspace.clone())),
        )
        .await
        .unwrap(),
    );
    let began = tokio::time::Instant::now();
    let mut starting = {
        let harness = Arc::clone(&harness);
        tokio::spawn(async move {
            start_child_service(
                &harness,
                "lsp:blocked-service".to_owned(),
                RunCancellation::new(),
            )
            .await
        })
    };
    tokio::time::timeout(Duration::from_secs(5), async {
        while tokio::fs::metadata(workspace.join("heartbeat"))
            .await
            .map_or(true, |metadata| metadata.len() == 0)
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the real service process must begin initialization");
    let pid = tokio::fs::read_to_string(workspace.join("service.pid"))
        .await
        .unwrap()
        .parse::<i32>()
        .unwrap();
    let catalog = harness.service_catalog().await;
    assert_eq!(
        catalog
            .iter()
            .find(|service| service.id == "lsp:blocked-service")
            .unwrap()
            .status,
        SessionServiceStatus::Starting
    );
    let independent = coordinator.bind("another-family".to_owned(), workspace.clone());
    assert!(independent.try_acquire().await.unwrap().is_none());

    let completed = tokio::time::timeout(Duration::from_secs(25), &mut starting).await;
    if completed.is_err() {
        starting.abort();
        let _ = starting.await;
    }
    let error = completed
        .expect("control timeout must advance startup cancellation and stop cleanup together")
        .unwrap()
        .unwrap_err();
    assert!(began.elapsed() >= Duration::from_secs(20));
    assert_eq!(error.code, ErrorCode::Unavailable);
    assert!(error.message.contains("20-second cloud control limit"));
    let catalog = harness.service_catalog().await;
    let service = catalog
        .iter()
        .find(|service| service.id == "lsp:blocked-service")
        .unwrap();
    assert_eq!(service.status, SessionServiceStatus::Stopped);
    assert_eq!(service.active_calls, 0);
    assert_eq!(
        nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None),
        Err(nix::errno::Errno::ESRCH)
    );
    let heartbeat = tokio::fs::read(workspace.join("heartbeat")).await.unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(
        tokio::fs::read(workspace.join("heartbeat")).await.unwrap(),
        heartbeat,
        "service descendants must stop writing after timeout cleanup"
    );
    assert!(
        independent.try_acquire().await.unwrap().is_some(),
        "timed-out initialization must release its physical directory lease"
    );
    harness.shutdown().await.unwrap();
}
