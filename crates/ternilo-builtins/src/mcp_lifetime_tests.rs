use std::{
    path::PathBuf,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

use ternilo_kernel::{
    DeferredToolSource, HarnessSession, HostEnvironment, HostPolicy, RunCancellation,
    ToolRegistration, WorkspaceExecution, WorkspaceExecutionLease,
};
use ternilo_protocol::{PluginEntry, SessionServiceStatus, WorkspaceBinding};
use tokio::sync::Notify;

use super::*;
use crate::stdio_test::{StdioFixture, environment_client, tool_context};

#[derive(Default)]
struct Admission {
    blocked: AtomicBool,
    holders: Arc<AtomicUsize>,
    changed: Notify,
}

struct Permit(Arc<AtomicUsize>);

impl Drop for Permit {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

impl Admission {
    fn grant(&self) -> WorkspaceExecutionLease {
        self.holders.fetch_add(1, Ordering::AcqRel);
        WorkspaceExecutionLease::hold(Permit(Arc::clone(&self.holders)))
    }

    fn unblock(&self) {
        self.blocked.store(false, Ordering::Release);
        self.changed.notify_one();
    }
}

impl WorkspaceExecution for Admission {
    fn try_acquire<'a>(
        &'a self,
    ) -> Pin<
        Box<dyn Future<Output = Result<Option<WorkspaceExecutionLease>, HarnessError>> + Send + 'a>,
    > {
        Box::pin(async move { Ok((!self.blocked.load(Ordering::Acquire)).then(|| self.grant())) })
    }

    fn acquire<'a>(
        &'a self,
        cancellation: RunCancellation,
    ) -> Pin<Box<dyn Future<Output = Result<WorkspaceExecutionLease, HarnessError>> + Send + 'a>>
    {
        Box::pin(async move {
            loop {
                cancellation.check()?;
                let notified = self.changed.notified();
                if !self.blocked.load(Ordering::Acquire) {
                    return Ok(self.grant());
                }
                tokio::select! {
                    () = cancellation.cancelled() => cancellation.check()?,
                    () = notified => {}
                }
            }
        })
    }
}

fn configuration(fixture: &StdioFixture, mode: &str, timeout_ms: u64) -> Value {
    let command = fixture.command("mcp", mode);
    let command = command.as_std();
    let environment = command
        .get_envs()
        .filter_map(|(key, value)| {
            value.map(|value| {
                (
                    key.to_str().unwrap().to_owned(),
                    value.to_str().unwrap().to_owned(),
                )
            })
        })
        .collect::<BTreeMap<_, _>>();
    serde_json::json!({
        "server_name": "fixture", "command": command.get_program().to_str().unwrap(),
        "args": command.get_args().map(|argument| argument.to_str().unwrap()).collect::<Vec<_>>(),
        "cwd": environment["TERNILO_STDIO_DIRECTORY"], "env": environment,
        "startup_timeout_ms": timeout_ms, "tool_call_timeout_ms": timeout_ms,
    })
}

fn environment(directory: &str, admission: Arc<Admission>) -> HostEnvironment {
    HostEnvironment::memory(
        tool_context().identity,
        Some(WorkspaceBinding {
            workspace_id: ternilo_protocol::WorkspaceId::new("stdio-test"),
            path: directory.to_owned(),
        }),
        HostPolicy::local(ternilo_protocol::RunLimits::default()),
    )
    .with_workspace_execution(admission)
}

struct SourceFixture {
    process: StdioFixture,
    source: Arc<source::McpSource>,
    runtime: linorun_core::Runtime,
    admission: Arc<Admission>,
    directory: PathBuf,
}

impl SourceFixture {
    async fn new(mode: &str, timeout_ms: u64) -> Self {
        let process = StdioFixture::new();
        let config: McpConfig =
            serde_json::from_value(configuration(&process, mode, timeout_ms)).unwrap();
        let admission = Arc::new(Admission::default());
        let directory = PathBuf::from(config.cwd.as_ref().unwrap());
        let (runtime, environment) = environment_client(environment(
            directory.to_str().unwrap(),
            Arc::clone(&admission),
        ))
        .await;
        Self {
            process,
            source: Arc::new(source::McpSource::new(config, environment)),
            runtime,
            admission,
            directory,
        }
    }

    async fn tools(&self) -> Vec<ToolRegistration> {
        self.source.prepare(RunCancellation::new()).await.unwrap()
    }

    fn leader(&self) -> String {
        std::fs::read_to_string(self.directory.join("leader")).unwrap()
    }

    async fn finish(&self) {
        self.source.shutdown().await.unwrap();
        self.runtime.shutdown().await;
        assert_eq!(self.admission.holders.load(Ordering::Acquire), 0);
    }
}

async fn wait_for(mut ready: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while !ready() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("MCP source did not reach the expected state");
}

#[tokio::test]
async fn activation_is_inert_and_startup_waits_for_admission() {
    let fixture = SourceFixture::new("success", 2000).await;
    let mut profile = crate::local_profile();
    profile.plugins.push(PluginEntry {
        id: "fixture-mcp".to_owned(),
        kind: KIND.to_owned(),
        enabled: true,
        config: configuration(&fixture.process, "success", 2000),
    });
    let harness = tokio::time::timeout(
        Duration::from_secs(2),
        HarnessSession::boot(
            &crate::catalog().unwrap(),
            &profile,
            environment(
                fixture.directory.to_str().unwrap(),
                Arc::clone(&fixture.admission),
            ),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(!fixture.directory.join("leader").exists());
    harness.shutdown().await.unwrap();
    assert_eq!(fixture.source.snapshot().status, SessionServiceStatus::Idle);
    assert!(fixture.source.initial_tools().is_empty());
    fixture.admission.blocked.store(true, Ordering::Release);
    let source = Arc::clone(&fixture.source);
    let start = tokio::spawn(async move { source.prepare(RunCancellation::new()).await });
    wait_for(|| fixture.source.snapshot().status == SessionServiceStatus::Starting).await;
    assert!(!fixture.directory.join("leader").exists());
    fixture.admission.unblock();
    let tools = start.await.unwrap().unwrap();
    assert_eq!(tools[0].spec.name, "mcp__fixture__echo");
    assert_eq!(
        tools[0].spec.input_schema,
        serde_json::json!({"type":"object"})
    );
    assert_eq!(fixture.admission.holders.load(Ordering::Acquire), 1);
    fixture.finish().await;
    fixture.process.assert_stopped().await;
}

#[tokio::test]
async fn successful_turns_keep_state_and_manual_stop_requires_explicit_restart() {
    let fixture = SourceFixture::new("success", 2000).await;
    let tools = fixture.tools().await;
    let first_pid = fixture.leader();
    for _ in 0..2 {
        let current = fixture.tools().await;
        assert_eq!(
            current[0]
                .handler
                .execute(tool_context(), serde_json::json!({}))
                .await
                .unwrap()
                .content,
            "fixture result"
        );
        assert_eq!(fixture.leader(), first_pid);
    }
    assert_eq!(fixture.process.calls(), 2);
    assert_eq!(
        fixture.source.snapshot().status,
        SessionServiceStatus::Running
    );
    assert_eq!(fixture.admission.holders.load(Ordering::Acquire), 1);
    fixture.source.stop().await.unwrap();
    fixture.process.assert_stopped().await;
    assert_eq!(fixture.admission.holders.load(Ordering::Acquire), 0);
    assert_eq!(
        fixture.source.snapshot().status,
        SessionServiceStatus::Stopped
    );
    assert!(fixture.tools().await.is_empty());
    assert_eq!(fixture.leader(), first_pid);
    let restarted = fixture.source.start(RunCancellation::new()).await.unwrap();
    assert_ne!(fixture.leader(), first_pid);
    assert!(
        tools[0]
            .handler
            .execute(tool_context(), serde_json::json!({}))
            .await
            .is_err()
    );
    restarted[0]
        .handler
        .execute(tool_context(), serde_json::json!({}))
        .await
        .unwrap();
    assert_eq!(fixture.process.calls(), 3);
    fixture.finish().await;
    assert!(fixture.source.start(RunCancellation::new()).await.is_err());
}

#[tokio::test]
async fn startup_timeout_or_failure_stops_descendants_without_automatic_retry() {
    for mode in ["hang_startup", "exit_startup"] {
        let fixture = SourceFixture::new(mode, 250).await;
        assert!(
            fixture
                .source
                .prepare(RunCancellation::new())
                .await
                .is_err()
        );
        fixture.process.assert_stopped().await;
        assert_eq!(
            fixture.source.snapshot().status,
            SessionServiceStatus::Failed
        );
        let first_pid = fixture.leader();
        assert!(
            fixture
                .source
                .prepare(RunCancellation::new())
                .await
                .err()
                .unwrap()
                .message
                .contains("explicitly start")
        );
        assert_eq!(fixture.leader(), first_pid);
        fixture.finish().await;
    }
}

#[tokio::test]
async fn cancelling_or_dropping_startup_stops_descendants() {
    for abort in [false, true] {
        let fixture = SourceFixture::new("hang_startup", 10_000).await;
        let cancellation = RunCancellation::new();
        let requested = cancellation.clone();
        let source = Arc::clone(&fixture.source);
        let start = tokio::spawn(async move { source.prepare(requested).await });
        fixture.process.ready("ready").await;
        if abort {
            start.abort();
            assert!(start.await.err().unwrap().is_cancelled());
        } else {
            cancellation.cancel();
            assert!(start.await.unwrap().err().unwrap().is_cancelled());
        }
        assert_eq!(
            fixture.source.snapshot().status,
            SessionServiceStatus::Failed
        );
        fixture.source.stop().await.unwrap();
        fixture.process.assert_stopped().await;
        fixture.finish().await;
    }
}

#[tokio::test]
async fn manual_stop_cancels_starting_source_without_waiting_for_startup_timeout() {
    let fixture = SourceFixture::new("hang_startup", 10_000).await;
    let source = Arc::clone(&fixture.source);
    let start = tokio::spawn(async move { source.prepare(RunCancellation::new()).await });
    fixture.process.ready("ready").await;
    tokio::time::timeout(Duration::from_secs(1), fixture.source.stop())
        .await
        .unwrap()
        .unwrap();
    assert!(start.await.unwrap().err().unwrap().is_cancelled());
    assert_eq!(
        fixture.source.snapshot().status,
        SessionServiceStatus::Stopped
    );
    assert!(fixture.tools().await.is_empty());
    fixture.process.assert_stopped().await;
    fixture.finish().await;
}

#[tokio::test]
async fn stopped_admission_wait_never_launches_the_server() {
    let fixture = SourceFixture::new("success", 2000).await;
    fixture.admission.blocked.store(true, Ordering::Release);
    let source = Arc::clone(&fixture.source);
    let start = tokio::spawn(async move { source.prepare(RunCancellation::new()).await });
    wait_for(|| fixture.source.snapshot().status == SessionServiceStatus::Starting).await;
    fixture.source.stop().await.unwrap();
    assert!(start.await.unwrap().err().unwrap().is_cancelled());
    fixture.admission.unblock();
    assert!(fixture.tools().await.is_empty());
    assert!(!fixture.directory.join("leader").exists());
    fixture.finish().await;
}

#[tokio::test]
async fn failed_or_timed_out_call_latches_failure_and_stops_writers() {
    for mode in ["hang_call", "fail_call"] {
        let fixture = SourceFixture::new(mode, 300).await;
        let tools = fixture.tools().await;
        assert!(
            tools[0]
                .handler
                .execute(tool_context(), serde_json::json!({}))
                .await
                .is_err()
        );
        assert_eq!(
            fixture.source.snapshot().status,
            SessionServiceStatus::Failed
        );
        assert_eq!(fixture.source.snapshot().active_calls, 0);
        fixture.process.assert_stopped().await;
        let first_pid = fixture.leader();
        let calls = fixture.process.calls();
        let cached = fixture.tools().await;
        assert_eq!(cached[0].spec, tools[0].spec);
        assert_eq!(
            fixture.source.snapshot().status,
            SessionServiceStatus::Failed
        );
        assert!(
            cached[0]
                .handler
                .execute(tool_context(), serde_json::json!({}))
                .await
                .is_err()
        );
        assert_eq!(fixture.leader(), first_pid);
        assert_eq!(
            fixture.process.calls(),
            calls,
            "failed handlers do not issue another RPC"
        );
        fixture.source.start(RunCancellation::new()).await.unwrap();
        assert_ne!(fixture.leader(), first_pid);
        assert_eq!(
            fixture.process.calls(),
            calls,
            "explicit restart never retries the old tool call"
        );
        assert_eq!(
            fixture.source.snapshot().status,
            SessionServiceStatus::Running
        );
        fixture.finish().await;
    }
}

struct MountedSource(Arc<source::McpSource>);

linorun_macros::component_descriptor! {
    static SOURCE_FIXTURE: () {
        id: "ternilo/test-existing-mcp-source@1",
        requires: [Tools],
        provides: [],
    }
}

impl HarnessPlugin for MountedSource {
    fn descriptor(&self) -> &'static ComponentDescriptor {
        &SOURCE_FIXTURE
    }

    fn activate(&self, context: ComponentContext) -> Activation {
        let tools = context.context().service::<Tools>().unwrap();
        let source = Arc::clone(&self.0);
        Activation::once(async move {
            let id = tools
                .register_source(source)
                .await
                .map_err(|error| ActivationFailure::user(error.to_string()))?;
            Ok(Some(effect::inverse(move || async move {
                tools
                    .unregister_source(id)
                    .await
                    .map_err(|error| CleanupError::user(error.to_string()))
            })))
        })
    }
}

#[tokio::test]
async fn failed_mcp_keeps_native_registry_tools_usable_without_reconnecting() {
    let fixture = SourceFixture::new("fail_call", 2000).await;
    let discovered = fixture.tools().await;
    assert!(
        discovered[0]
            .handler
            .execute(tool_context(), serde_json::json!({}))
            .await
            .is_err()
    );
    fixture.process.assert_stopped().await;
    let first_pid = fixture.leader();
    let calls = fixture.process.calls();
    let mut catalog = crate::catalog().unwrap();
    let source = Arc::clone(&fixture.source);
    catalog
        .register(PluginFactory::new(
            PluginManifest {
                kind: "test.existing-mcp",
                requires: &["ternilo/tools@1"],
                provides: &[],
            },
            move |_| Ok(Arc::new(MountedSource(Arc::clone(&source)))),
        ))
        .unwrap();
    let mut profile = crate::local_profile();
    profile.plugins.push(PluginEntry {
        id: "existing-mcp".to_owned(),
        kind: "test.existing-mcp".to_owned(),
        enabled: true,
        config: serde_json::json!({}),
    });
    let harness = HarnessSession::boot(
        &catalog,
        &profile,
        environment(
            fixture.directory.to_str().unwrap(),
            Arc::clone(&fixture.admission),
        ),
    )
    .await
    .unwrap();
    let outcome = harness
        .run(ternilo_protocol::RunId::new("native-fallback"), "/agents")
        .await
        .unwrap();
    assert_eq!(outcome.answer, "[]");
    assert_eq!(
        fixture.source.snapshot().status,
        SessionServiceStatus::Failed
    );
    assert!(
        harness
            .tool_catalog()
            .await
            .unwrap()
            .iter()
            .any(|tool| tool.name == "mcp__fixture__echo")
    );
    assert_eq!(fixture.leader(), first_pid);
    assert_eq!(fixture.process.calls(), calls);
    harness
        .stop_service("mcp:fixture".to_owned())
        .await
        .unwrap();
    assert!(
        !harness
            .tool_catalog()
            .await
            .unwrap()
            .iter()
            .any(|tool| tool.name == "mcp__fixture__echo")
    );
    assert_eq!(
        fixture.source.snapshot().status,
        SessionServiceStatus::Stopped
    );
    harness.shutdown().await.unwrap();
    fixture.finish().await;
}

#[tokio::test]
async fn explicit_or_dropped_call_cancellation_stops_server_but_busy_manual_stop_is_rejected() {
    for abort in [false, true] {
        let fixture = SourceFixture::new("hang_call", 10_000).await;
        let tools = fixture.tools().await;
        let handler = Arc::clone(&tools[0].handler);
        let context = tool_context();
        let cancellation = context.cancellation.clone();
        let call =
            tokio::spawn(async move { handler.execute(context, serde_json::json!({})).await });
        fixture.process.ready("request").await;
        assert_eq!(fixture.source.snapshot().active_calls, 1);
        assert!(fixture.source.stop().await.is_err());
        assert_eq!(
            fixture.source.snapshot().status,
            SessionServiceStatus::Running
        );
        if abort {
            call.abort();
            assert!(call.await.err().unwrap().is_cancelled());
        } else {
            cancellation.cancel();
            assert!(call.await.unwrap().err().unwrap().is_cancelled());
        }
        assert_eq!(fixture.source.snapshot().active_calls, 0);
        assert_eq!(
            fixture.source.snapshot().status,
            SessionServiceStatus::Failed
        );
        fixture.process.assert_stopped().await;
        fixture.finish().await;
    }
}

#[tokio::test]
async fn concurrent_calls_are_counted_and_one_failed_call_ends_the_shared_server() {
    let fixture = SourceFixture::new("hang_call", 10_000).await;
    let tools = fixture.tools().await;
    let mut calls = Vec::new();
    for _ in 0..2 {
        let handler = Arc::clone(&tools[0].handler);
        calls.push(tokio::spawn(async move {
            handler.execute(tool_context(), serde_json::json!({})).await
        }));
    }
    wait_for(|| fixture.source.snapshot().active_calls == 2).await;
    assert!(fixture.source.stop().await.is_err());
    let first = calls.remove(0);
    first.abort();
    assert!(first.await.err().unwrap().is_cancelled());
    assert!(
        tokio::time::timeout(Duration::from_secs(2), calls.remove(0))
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    assert_eq!(fixture.source.snapshot().active_calls, 0);
    assert_eq!(
        fixture.source.snapshot().status,
        SessionServiceStatus::Failed
    );
    fixture.finish().await;
    fixture.process.assert_stopped().await;
}

#[tokio::test]
async fn unmount_stops_active_calls_and_invalidates_existing_handlers() {
    let fixture = SourceFixture::new("hang_call", 10_000).await;
    let tools = fixture.tools().await;
    let handler = Arc::clone(&tools[0].handler);
    let call =
        tokio::spawn(async move { handler.execute(tool_context(), serde_json::json!({})).await });
    fixture.process.ready("request").await;
    fixture.source.shutdown().await.unwrap();
    assert!(call.await.unwrap().is_err());
    assert!(
        tools[0]
            .handler
            .execute(tool_context(), serde_json::json!({}))
            .await
            .is_err()
    );
    assert!(fixture.source.start(RunCancellation::new()).await.is_err());
    fixture.process.assert_stopped().await;
    fixture.finish().await;
}

#[tokio::test]
async fn exited_server_cannot_leave_idle_descendant_writers() {
    let fixture = SourceFixture::new("exit_after_reply", 2000).await;
    let tools = fixture.tools().await;
    let _ = tools[0]
        .handler
        .execute(tool_context(), serde_json::json!({}))
        .await;
    fixture.process.assert_stopped().await;
    wait_for(|| fixture.source.snapshot().status == SessionServiceStatus::Failed).await;
    fixture.finish().await;
}
