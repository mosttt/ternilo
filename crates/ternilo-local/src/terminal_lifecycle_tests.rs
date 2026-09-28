use super::*;
use ternilo_kernel::{HarnessSession, HostEnvironment, HostPolicy, TerminalsClient};
use ternilo_protocol::{
    AgentId, PermissionPreset, PluginEntry, RunLimits, SessionId, SessionIdentity, TenantId,
    UserId, WorkspaceBinding, WorkspaceId,
};

linorun_macros::component_descriptor! {
    static TEST_TERMINALS: () {
        id: "tests/terminal-client@1",
        requires: [Terminals],
        provides: [],
    }
}

struct TerminalClientBridge {
    sender: Arc<std::sync::Mutex<Option<oneshot::Sender<TerminalsClient>>>>,
}

impl HarnessPlugin for TerminalClientBridge {
    fn descriptor(&self) -> &'static ComponentDescriptor {
        &TEST_TERMINALS
    }

    fn activate(&self, context: ComponentContext) -> Activation {
        let client = context.context().service::<Terminals>().unwrap();
        let sender = self.sender.lock().unwrap().take().unwrap();
        Activation::Once(Box::pin(async move {
            let _ = sender.send(client);
            Ok(None)
        }))
    }
}

async fn terminal_service(root: &std::path::Path) -> (HarnessSession, TerminalsClient) {
    let (sender, receiver) = oneshot::channel();
    let sender = Arc::new(std::sync::Mutex::new(Some(sender)));
    let mut catalog = crate::catalog().unwrap();
    catalog
        .register(PluginFactory::new(
            PluginManifest {
                kind: "tests.terminal_client",
                requires: &["ternilo/terminals@1"],
                provides: &[],
            },
            move |_| {
                Ok(Arc::new(TerminalClientBridge {
                    sender: Arc::clone(&sender),
                }))
            },
        ))
        .unwrap();
    let mut profile = crate::local_profile();
    profile.plugins.push(PluginEntry {
        id: "test-terminals".to_owned(),
        kind: "tests.terminal_client".to_owned(),
        enabled: true,
        config: serde_json::json!({}),
    });
    let environment = HostEnvironment::memory(
        SessionIdentity {
            tenant_id: TenantId::new("terminal-tests"),
            user_id: UserId::new("owner"),
            agent_id: AgentId::new("agent"),
            session_id: SessionId::new("terminal-session"),
        },
        Some(WorkspaceBinding {
            workspace_id: WorkspaceId::new("workspace"),
            path: root.to_string_lossy().into_owned(),
        }),
        HostPolicy {
            permissions: PermissionPreset::FullAccess,
            ..HostPolicy::local(RunLimits::default())
        },
    );
    let harness = HarnessSession::boot(&catalog, &profile, environment)
        .await
        .unwrap();
    (harness, receiver.await.unwrap())
}

#[tokio::test]
async fn terminal_close_preserves_shell_state_then_stops_background_descendants() {
    let root = tempfile::tempdir().unwrap();
    let (harness, terminals) = terminal_service(root.path()).await;
    let terminal = terminals.open(Some("persistent".to_owned())).await.unwrap();
    terminals
        .send(
            terminal.terminal_id.clone(),
            "export TERMINAL_STATE=retained".to_owned(),
            1_000,
        )
        .await
        .unwrap();
    let output = terminals
        .send(
            terminal.terminal_id.clone(),
            "printf '%s' \"$TERMINAL_STATE\"".to_owned(),
            1_000,
        )
        .await
        .unwrap();
    assert!(output.output.contains("retained"));
    terminals
        .send(
            terminal.terminal_id.clone(),
            "(sleep 0.8; printf orphan > escaped-write) >/dev/null 2>&1 &".to_owned(),
            1_000,
        )
        .await
        .unwrap();
    let closed = terminals.close(terminal.terminal_id).await.unwrap();
    assert_eq!(closed.status, TerminalStatus::Exited);
    tokio::time::sleep(Duration::from_millis(1_000)).await;
    assert!(!root.path().join("escaped-write").exists());
    harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn terminal_runtime_teardown_stops_commands_after_send_wait_expires() {
    let root = tempfile::tempdir().unwrap();
    let (harness, terminals) = terminal_service(root.path()).await;
    let terminal = terminals.open(None).await.unwrap();
    let output = terminals
        .send(
            terminal.terminal_id,
            "sleep 0.8; printf orphan > escaped-write".to_owned(),
            100,
        )
        .await
        .unwrap();
    assert!(
        output.timed_out,
        "the terminal command remains active after the read wait expires"
    );
    harness.shutdown().await.unwrap();
    tokio::time::sleep(Duration::from_millis(1_000)).await;
    assert!(!root.path().join("escaped-write").exists());
}

#[tokio::test]
async fn terminal_supervisor_stops_descendants_when_the_shell_exits() {
    let root = tempfile::tempdir().unwrap();
    let (harness, terminals) = terminal_service(root.path()).await;
    let terminal = terminals.open(None).await.unwrap();
    let output = terminals
        .send(
            terminal.terminal_id.clone(),
            "(sleep 0.8; printf orphan > escaped-write) >/dev/null 2>&1 & printf final-output; exit 0".to_owned(),
            1_000,
        )
        .await
        .unwrap();
    assert!(output.output.contains("final-output"));
    tokio::time::timeout(Duration::from_secs(2), async {
        while terminals
            .list()
            .await
            .iter()
            .any(|entry| entry.status == TerminalStatus::Running)
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(1_000)).await;
    assert!(!root.path().join("escaped-write").exists());
    assert_eq!(
        terminals.close(terminal.terminal_id).await.unwrap().status,
        TerminalStatus::Exited
    );
    harness.shutdown().await.unwrap();
}
