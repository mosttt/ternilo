use super::*;
use crate::stdio_test::StdioFixture;

fn config(timeout_ms: u64) -> LspConfig {
    serde_json::from_value(json!({
        "server_name": "fixture", "command": "/bin/sh", "timeout_ms": timeout_ms,
    }))
    .unwrap()
}

async fn start_fixture(
    mut command: Command,
    config: LspConfig,
) -> Result<LspProcess, HarnessError> {
    let mut process = LspProcess::spawn(&mut command, config.max_message_bytes, None)?;
    process
        .initialize(&config, "file:///workspace/".to_owned(), "fixture")
        .await?;
    Ok(process)
}

#[tokio::test]
async fn successful_calls_reuse_server_and_shutdown_cleans_descendants_and_stderr() {
    let fixture = StdioFixture::new();
    let mut process = start_fixture(fixture.command("lsp", "success"), config(2000))
        .await
        .unwrap();
    let stderr = process.stderr.abort_handle();
    for _ in 0..2 {
        let (next, response) = process
            .call("workspace/symbol", json!({}), 2000)
            .await
            .unwrap();
        assert_eq!(response["value"], "fixture result");
        process = next;
    }
    assert_eq!(fixture.calls(), 2);
    assert!(process.child.status().is_none());
    process.shutdown().await.unwrap();
    fixture.assert_stopped().await;
    assert!(stderr.is_finished());
}

#[tokio::test]
async fn startup_timeout_or_failure_stops_descendants() {
    for mode in ["hang_startup", "exit_startup"] {
        let fixture = StdioFixture::new();
        assert!(
            start_fixture(fixture.command("lsp", mode), config(250))
                .await
                .is_err()
        );
        fixture.assert_stopped().await;
    }
}

#[tokio::test]
async fn cancelled_startup_stops_descendants() {
    let fixture = StdioFixture::new();
    let task = tokio::spawn(start_fixture(
        fixture.command("lsp", "hang_startup"),
        config(10_000),
    ));
    fixture.ready("ready").await;
    task.abort();
    assert!(matches!(task.await, Err(error) if error.is_cancelled()));
    fixture.assert_stopped().await;
}

#[tokio::test]
async fn request_timeout_or_failure_stops_server_and_stderr_task() {
    for mode in ["hang_call", "fail_call"] {
        let fixture = StdioFixture::new();
        let process = start_fixture(fixture.command("lsp", mode), config(2000))
            .await
            .unwrap();
        let stderr = process.stderr.abort_handle();
        assert!(
            process
                .call("workspace/symbol", json!({}), 200)
                .await
                .is_err()
        );
        fixture.assert_stopped().await;
        assert!(stderr.is_finished());
    }
}

#[tokio::test]
async fn cancelled_request_drops_owned_process_and_stderr_task() {
    let fixture = StdioFixture::new();
    let process = start_fixture(fixture.command("lsp", "hang_call"), config(2000))
        .await
        .unwrap();
    let stderr = process.stderr.abort_handle();
    let task = tokio::spawn(process.call("workspace/symbol", json!({}), 10_000));
    fixture.ready("request").await;
    task.abort();
    assert!(matches!(task.await, Err(error) if error.is_cancelled()));
    fixture.assert_stopped().await;
    assert!(stderr.is_finished());
}

#[tokio::test]
async fn idle_leader_exit_does_not_leave_descendant_writers() {
    let fixture = StdioFixture::new();
    let process = start_fixture(fixture.command("lsp", "exit_after_reply"), config(2000))
        .await
        .unwrap();
    let stderr = process.stderr.abort_handle();
    let result = process.call("workspace/symbol", json!({}), 2000).await;
    fixture.assert_stopped().await;
    assert!(stderr.is_finished());
    drop(result);
}

#[tokio::test]
async fn dropping_idle_process_cleans_descendants_and_stderr_task() {
    let fixture = StdioFixture::new();
    let process = start_fixture(fixture.command("lsp", "success"), config(2000))
        .await
        .unwrap();
    let stderr = process.stderr.abort_handle();
    drop(process);
    fixture.assert_stopped().await;
    assert!(stderr.is_finished());
}

struct Admission {
    available: Arc<tokio::sync::Semaphore>,
}

impl ternilo_kernel::WorkspaceExecution for Admission {
    fn try_acquire<'a>(
        &'a self,
    ) -> Pin<
        Box<dyn Future<Output = Result<Option<WorkspaceExecutionLease>, HarnessError>> + Send + 'a>,
    > {
        Box::pin(async move {
            match Arc::clone(&self.available).try_acquire_owned() {
                Ok(permit) => Ok(Some(WorkspaceExecutionLease::hold(permit))),
                Err(tokio::sync::TryAcquireError::NoPermits) => Ok(None),
                Err(error) => Err(HarnessError::execution(error.to_string())),
            }
        })
    }

    fn acquire<'a>(
        &'a self,
        cancellation: RunCancellation,
    ) -> Pin<Box<dyn Future<Output = Result<WorkspaceExecutionLease, HarnessError>> + Send + 'a>>
    {
        Box::pin(async move {
            tokio::select! {
                biased;
                () = cancellation.cancelled() => Err(HarnessError::cancelled("fixture admission was cancelled")),
                permit = Arc::clone(&self.available).acquire_owned() => {
                    permit.map(WorkspaceExecutionLease::hold).map_err(|error| HarnessError::execution(error.to_string()))
                }
            }
        })
    }
}

async fn source_fixture(
    fixture: &StdioFixture,
    mode: &str,
) -> (
    linorun_core::Runtime,
    Arc<LspSource>,
    Arc<tokio::sync::Semaphore>,
    ternilo_protocol::WorkspaceBinding,
) {
    let command = fixture.command("lsp", mode);
    let command = command.as_std();
    let env = command
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
    let workspace = ternilo_protocol::WorkspaceBinding {
        workspace_id: ternilo_protocol::WorkspaceId::new("lsp-fixture"),
        path: env["TERNILO_STDIO_DIRECTORY"].clone(),
    };
    let available = Arc::new(tokio::sync::Semaphore::new(1));
    let environment = ternilo_kernel::HostEnvironment::memory(
        crate::stdio_test::tool_context().identity,
        Some(workspace.clone()),
        ternilo_kernel::HostPolicy::local(ternilo_protocol::RunLimits::default()),
    )
    .with_workspace_execution(Arc::new(Admission {
        available: Arc::clone(&available),
    }));
    let (runtime, environment) = crate::stdio_test::environment_client(environment).await;
    let mut config = config(10_000);
    config.args = command
        .get_args()
        .map(|arg| arg.to_str().unwrap().to_owned())
        .collect();
    config.env = env;
    let source = Arc::new(LspSource {
        handler: Arc::new(LspTool {
            config,
            environment,
            process: Mutex::new(None),
            state: StdMutex::new(LspState::default()),
        }),
    });
    (runtime, source, available, workspace)
}

async fn request(
    source: &LspSource,
    workspace: &ternilo_protocol::WorkspaceBinding,
) -> Result<ToolOutput, HarnessError> {
    let mut context = crate::stdio_test::tool_context();
    context.workspace = Some(workspace.clone());
    source
        .handler
        .execute(context, json!({ "method": "workspace/symbol" }))
        .await
}

async fn wait_status(source: &LspSource, status: SessionServiceStatus) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while source.snapshot().status != status {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("LSP did not reach the expected service status");
}

#[tokio::test]
async fn source_is_lazy_reuses_process_and_requires_explicit_restart_after_stop() {
    let fixture = StdioFixture::new();
    let (runtime, source, available, workspace) = source_fixture(&fixture, "success").await;
    assert_eq!(source.initial_tools().len(), 1);
    assert_eq!(
        source.prepare(RunCancellation::new()).await.unwrap().len(),
        1
    );
    assert_eq!(source.snapshot().status, SessionServiceStatus::Idle);
    assert_eq!(available.available_permits(), 1);
    let leader_file = Path::new(&workspace.path).join("leader");
    assert!(!leader_file.exists());

    assert!(
        request(&source, &workspace)
            .await
            .unwrap()
            .content
            .contains("fixture result")
    );
    let leader = std::fs::read_to_string(&leader_file).unwrap();
    let (first, second) = tokio::join!(request(&source, &workspace), request(&source, &workspace));
    assert!(first.is_ok() && second.is_ok());
    assert_eq!(fixture.calls(), 3);
    assert_eq!(std::fs::read_to_string(&leader_file).unwrap(), leader);
    assert_eq!(source.snapshot().status, SessionServiceStatus::Running);
    assert_eq!(source.snapshot().active_calls, 0);
    assert_eq!(available.available_permits(), 0);

    source.stop().await.unwrap();
    fixture.assert_stopped().await;
    assert_eq!(available.available_permits(), 1);
    assert_eq!(source.snapshot().status, SessionServiceStatus::Stopped);
    source.prepare(RunCancellation::new()).await.unwrap();
    assert!(request(&source, &workspace).await.is_err());
    assert_eq!(std::fs::read_to_string(&leader_file).unwrap(), leader);
    source.start(RunCancellation::new()).await.unwrap();
    assert_ne!(std::fs::read_to_string(&leader_file).unwrap(), leader);
    assert_eq!(available.available_permits(), 0);
    assert!(request(&source, &workspace).await.is_ok());
    source.shutdown().await.unwrap();
    fixture.assert_stopped().await;
    assert_eq!(available.available_permits(), 1);
    assert!(source.start(RunCancellation::new()).await.is_err());
    runtime.shutdown().await;
}

#[tokio::test]
async fn stopping_initialization_cancels_startup_and_releases_lifetime_admission() {
    let fixture = StdioFixture::new();
    let (runtime, source, available, workspace) = source_fixture(&fixture, "hang_startup").await;
    let starting = Arc::clone(&source);
    let task = tokio::spawn(async move { starting.start(RunCancellation::new()).await });
    fixture.ready("ready").await;
    assert_eq!(source.snapshot().status, SessionServiceStatus::Starting);
    assert_eq!(available.available_permits(), 0);
    tokio::time::timeout(Duration::from_secs(3), source.stop())
        .await
        .unwrap()
        .unwrap();
    assert!(task.await.unwrap().is_err());
    fixture.assert_stopped().await;
    assert_eq!(source.snapshot().status, SessionServiceStatus::Stopped);
    assert_eq!(available.available_permits(), 1);
    assert!(request(&source, &workspace).await.is_err());
    runtime.shutdown().await;
}

#[tokio::test]
async fn stopping_waiting_startup_does_not_need_the_occupied_directory() {
    let fixture = StdioFixture::new();
    let (runtime, source, available, workspace) = source_fixture(&fixture, "success").await;
    let occupied = Arc::clone(&available).acquire_owned().await.unwrap();
    let starting = Arc::clone(&source);
    let task = tokio::spawn(async move { starting.start(RunCancellation::new()).await });
    wait_status(&source, SessionServiceStatus::Starting).await;
    tokio::time::timeout(Duration::from_secs(1), source.stop())
        .await
        .unwrap()
        .unwrap();
    assert!(task.await.unwrap().is_err());
    assert_eq!(source.snapshot().status, SessionServiceStatus::Stopped);
    assert!(!Path::new(&workspace.path).join("leader").exists());
    drop(occupied);
    source.shutdown().await.unwrap();
    runtime.shutdown().await;
}

#[tokio::test]
async fn active_rpc_rejects_manual_stop_but_unmount_cancels_it_and_joins_cleanup() {
    let fixture = StdioFixture::new();
    let (runtime, source, available, workspace) = source_fixture(&fixture, "hang_call").await;
    source.start(RunCancellation::new()).await.unwrap();
    let running = Arc::clone(&source);
    let task = tokio::spawn(async move { request(&running, &workspace).await });
    fixture.ready("request").await;
    assert_eq!(source.snapshot().active_calls, 1);
    assert_eq!(
        source.stop().await.unwrap_err().code,
        ternilo_protocol::ErrorCode::Conflict
    );
    assert_eq!(source.snapshot().status, SessionServiceStatus::Running);
    assert_eq!(available.available_permits(), 0);
    source.shutdown().await.unwrap();
    assert!(task.await.unwrap().is_err());
    fixture.assert_stopped().await;
    assert_eq!(source.snapshot().status, SessionServiceStatus::Stopped);
    assert_eq!(source.snapshot().active_calls, 0);
    assert_eq!(available.available_permits(), 1);
    runtime.shutdown().await;
}

#[tokio::test]
async fn dropped_source_initialization_becomes_failed_without_releasing_a_live_process() {
    let fixture = StdioFixture::new();
    let (runtime, source, available, _) = source_fixture(&fixture, "hang_startup").await;
    let starting = Arc::clone(&source);
    let task = tokio::spawn(async move { starting.start(RunCancellation::new()).await });
    fixture.ready("ready").await;
    task.abort();
    assert!(matches!(task.await, Err(error) if error.is_cancelled()));
    wait_status(&source, SessionServiceStatus::Failed).await;
    source.shutdown().await.unwrap();
    fixture.assert_stopped().await;
    assert_eq!(available.available_permits(), 1);
    runtime.shutdown().await;
}

#[tokio::test]
async fn source_reports_idle_server_exit_and_does_not_automatically_relaunch() {
    let fixture = StdioFixture::new();
    let (runtime, source, available, workspace) =
        source_fixture(&fixture, "exit_after_reply").await;
    let _ = request(&source, &workspace).await;
    fixture.assert_stopped().await;
    wait_status(&source, SessionServiceStatus::Failed).await;
    assert!(source.snapshot().error.is_some());
    assert_eq!(available.available_permits(), 1);
    source.prepare(RunCancellation::new()).await.unwrap();
    assert!(request(&source, &workspace).await.is_err());
    assert_eq!(fixture.calls(), 1);
    source.shutdown().await.unwrap();
    runtime.shutdown().await;
}

#[tokio::test]
async fn dropping_managed_owner_retains_lease_until_joinable_supervisor_cleanup() {
    let fixture = StdioFixture::new();
    let available = Arc::new(tokio::sync::Semaphore::new(1));
    let lease =
        WorkspaceExecutionLease::hold(Arc::clone(&available).acquire_owned().await.unwrap());
    let process = LspProcess::spawn(
        &mut fixture.command("lsp", "hang_startup"),
        8192,
        Some(lease),
    )
    .unwrap();
    let control = process.child.control();
    fixture.ready("ready").await;
    assert_eq!(available.available_permits(), 0);
    drop(process);
    // This current-thread task has not yielded to the detached supervisor yet.
    assert_eq!(available.available_permits(), 0);
    control.stop().await.unwrap();
    assert!(control.is_finished());
    assert!(control.status().is_some());
    assert_eq!(available.available_permits(), 1);
    fixture.assert_stopped().await;
}

#[tokio::test]
async fn dropping_source_stops_process_even_when_an_old_tool_handle_survives() {
    let fixture = StdioFixture::new();
    let (runtime, source, available, workspace) = source_fixture(&fixture, "success").await;
    let tool = source.initial_tools().pop().unwrap().handler;
    source.start(RunCancellation::new()).await.unwrap();
    let control = source
        .handler
        .state
        .lock()
        .unwrap()
        .control
        .clone()
        .unwrap();
    drop(source);
    control.stop().await.unwrap();
    fixture.assert_stopped().await;
    assert_eq!(available.available_permits(), 1);
    let mut context = crate::stdio_test::tool_context();
    context.workspace = Some(workspace);
    assert!(
        tool.execute(context, json!({"method": "workspace/symbol"}))
            .await
            .is_err()
    );
    drop(tool);
    runtime.shutdown().await;
}
