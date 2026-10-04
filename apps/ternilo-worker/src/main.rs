#![forbid(unsafe_code)]

use std::{
    collections::{BTreeMap, BTreeSet},
    net::SocketAddr,
    path::{Path, PathBuf},
    pin::Pin,
    process::ExitCode,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use clap::{Parser, Subcommand};
use linorun_core::CallContext;
use sha2::{Digest, Sha256};
use ternilo_cloud::{
    ExecutionAttachmentObject, ExecutionEnvelope, StartedRun, TerminalState, WorkerPolicy,
    WorkerTeamRequest,
};
use ternilo_kernel::{
    AgentTeamProvider, HarnessSession, HostEnvironment, ModelGatewayProvider, ModelOutput,
    RunCancellation, SessionEventStore, SubagentAdmission, SubagentRunStart,
    SubagentSessionBinding, SubagentSessionHost, SubagentSessionRequest, UserInteraction,
};
use ternilo_local::{
    LocalAttachmentReader, LocalAttachments, fit_reference_contexts, resolve_file_references,
};
use ternilo_protocol::{
    AcceptedSubagentRun, AgentTeamMessage, AgentTeamMessageId, AgentTeamMessageSend,
    AgentTeamSnapshot, AgentTeamTask, AgentTeamTaskCreate, AgentTeamTaskId, AgentTeamTaskReplace,
    Attachment, HarnessError, ModelRequest, ModelResponse, ModelRetryFailure, ReferenceContext,
    RunOutcome, SessionEvent, SessionEventKind, SubmissionDelivery, TenantId, UserAnswer,
    UserMessageSource, UserQuestion, WorkspaceBinding, WorkspaceId,
};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    sync::{Mutex, mpsc, oneshot},
};

mod child_protocol;
mod client;
mod config;
mod execution_activity;
mod isolation;
mod namespace_recovery;
mod process_lifetime;
mod sandbox_lifetime;
mod storage_root;
mod subagent_cleanup;
mod workspace_occupancy;
mod workspace_recovery;

#[cfg(all(test, unix))]
mod service_control_tests;

#[cfg(target_os = "linux")]
type WorkspaceOccupancyGuard = workspace_occupancy::OccupancyGuard;
#[cfg(not(target_os = "linux"))]
type WorkspaceOccupancyGuard = ();

use client::WorkerClient;
use config::{InitOptions, LoadedWorkerConfig, SandboxMode, ServeOptions};
#[cfg(test)]
use isolation::child_command;
use isolation::spawn_child;
use process_lifetime::ManagedChild;
mod health;
mod run;
mod session_commands;
mod workspace;

use child_protocol::{
    ChildProtocol, ChildSessionCommand, ChildSessionCommandOutcome, ChildToParentFrame,
    CloudHostOutcome, CloudHostRequest, ParentProtocol, ParentToChildFrame,
};
use run::{ActiveRunHandle, ActiveRuns};
use subagent_cleanup::SubagentCleanup;

use workspace::{
    enforce_workspace_free_space, enforce_workspace_quota, gc_workspace, prepare_workspace,
};

#[derive(Parser)]
#[command(about = "Ternilo isolated cloud worker", version)]
struct Args {
    #[command(subcommand)]
    command: Option<WorkerCommand>,
    #[command(flatten)]
    serve: ServeOptions,
}

#[derive(Subcommand)]
enum WorkerCommand {
    /// Execute one prevalidated envelope. Intended to run inside a sandbox.
    Execute {
        #[arg(long, env = "TERNILO_WORKER_POLICY")]
        policy: PathBuf,
        #[arg(long, env = "TERNILO_WORKER_ENVELOPE")]
        envelope: PathBuf,
        #[arg(long, hide = true)]
        owned_process_session: bool,
    },
    /// Save a private Worker configuration for this machine.
    Init(InitOptions),
    /// Connect to Server and launch one isolated child process per run.
    Serve(ServeOptions),
    /// Permanently remove one explicitly unregistered cloud workspace.
    GcWorkspace {
        /// Persistent root containing tenant-isolated cloud workspaces.
        #[arg(
            long,
            env = "TERNILO_WORKER_WORKSPACE_ROOT",
            default_value = "/var/lib/ternilo/workspaces"
        )]
        workspace_root: PathBuf,
        #[arg(long, env = "TERNILO_WORKER_GC_TENANT_ID")]
        tenant_id: String,
        #[arg(long, env = "TERNILO_WORKER_GC_WORKSPACE_ID")]
        workspace_id: String,
        /// Confirm that the workspace was unregistered and has no active runs.
        #[arg(
            long,
            env = "TERNILO_WORKER_GC_CONFIRM_UNREGISTERED",
            action = clap::ArgAction::Set,
            num_args = 0..=1,
            require_equals = true,
            default_missing_value = "true",
            default_value_t = false
        )]
        confirm_unregistered: bool,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    ternilo_local::initialize_tls();
    match run(Args::parse()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

async fn run(args: Args) -> Result<(), HarnessError> {
    match args.command {
        Some(WorkerCommand::Execute {
            policy,
            envelope,
            owned_process_session,
        }) => {
            if owned_process_session {
                process_lifetime::enter_owned_session()?;
            }
            execute(policy, envelope).await
        }
        Some(WorkerCommand::Init(options)) => {
            let path = options.initialize()?;
            println!("Worker configuration saved to {}", path.display());
            Ok(())
        }
        Some(WorkerCommand::Serve(options)) => daemon(options.load()?).await,
        None => daemon(args.serve.load()?).await,
        Some(WorkerCommand::GcWorkspace {
            workspace_root,
            tenant_id,
            workspace_id,
            confirm_unregistered,
        }) => {
            gc_workspace(
                workspace_root,
                TenantId::new(tenant_id),
                WorkspaceId::new(workspace_id),
                confirm_unregistered,
            )
            .await
        }
    }
}

#[derive(Clone)]
struct DaemonConfig {
    worker_id: String,
    capacity: ternilo_cloud::WorkerCapacity,
    sandbox: SandboxMode,
    workspace_root: PathBuf,
    storage_root: storage_root::RegisteredStorageRoot,
    lease_ttl: Duration,
    poll_interval: Duration,
    health_listen: SocketAddr,
    telemetry_endpoint: Option<String>,
    telemetry_headers: Option<String>,
    telemetry_service_name: String,
    telemetry_timeout: Duration,
}

#[expect(
    clippy::too_many_lines,
    reason = "Keep child boot, execution, event publication and shutdown in one ownership scope."
)]
async fn execute(policy_path: PathBuf, envelope_path: PathBuf) -> Result<(), HarnessError> {
    child_protocol::await_startup_authorization().await?;
    let policy: WorkerPolicy = read_json(&policy_path, 1024 * 1024)?;
    let envelope: ExecutionEnvelope = read_json(&envelope_path, 96 * 1024 * 1024)?;
    envelope.workspace.validate()?;
    if envelope.workspace.workspace_id != envelope.spec.metadata.workspace_id {
        return Err(HarnessError::policy(
            "execution envelope workspace does not match its immutable RunSpec",
        ));
    }
    let (catalog, _extension_registry_directory) = catalog_for_envelope(&envelope, &policy)?;
    let validated = policy.validate(&envelope.spec, &catalog)?;
    validate_prior_events(&envelope.prior_events)?;
    let generate_title = !envelope
        .prior_events
        .iter()
        .any(|event| matches!(event.kind, SessionEventKind::TurnFinished { .. }));
    let title_request = envelope
        .display_input
        .clone()
        .unwrap_or_else(|| envelope.spec.input.clone());
    let protocol = Arc::new(ChildProtocol::default());
    let event_store = Arc::new(PipeEventStore::new(
        envelope.prior_events,
        Arc::clone(&protocol),
    ));
    let ChildServices {
        model_gateway,
        execution_admission,
        interaction,
        subagent_host,
        agent_team,
        input_router,
        session_commands,
        subagent_cleanup,
    } = PipeModelGateway::start(Arc::clone(&protocol));
    let attachments = Arc::new(
        LocalAttachments::open(&Path::new(&envelope.workspace.path).join(".ternilo")).await?,
    );
    restore_attachment_objects(&attachments, envelope.attachment_objects).await?;
    let submitted_attachments = attachments.save_many(envelope.spec.attachments).await?;
    let mut additional_inputs = envelope.additional_inputs;
    for input in &mut additional_inputs {
        input.attachments = attachments
            .save_many(std::mem::take(&mut input.attachments))
            .await?;
    }
    let session_id = envelope.spec.metadata.session_id.clone();
    let denied_tools = validated.host_policy.denied_tools.clone();
    let environment = HostEnvironment::with_interaction(
        validated.identity,
        Some(envelope.workspace),
        validated.host_policy,
        event_store,
        interaction,
    )
    .with_model_gateway(model_gateway)
    .with_execution_admission(envelope.spec.metadata.run_id.clone(), execution_admission)
    .with_subagent_sessions(subagent_host)
    .with_agent_team(agent_team)
    .with_attachment_resolver(attachments)
    .with_session_mode(envelope.spec.mode);
    let harness =
        Arc::new(HarnessSession::boot(&catalog, &envelope.spec.profile, environment).await?);
    let session_command_router = tokio::spawn(serve_child_session_commands(
        session_commands,
        Arc::clone(&harness),
        Arc::clone(&protocol),
        session_id,
        denied_tools,
    ));
    let outcome = harness
        .run_input(ternilo_protocol::AgentInput {
            additional_inputs,
            run_id: envelope.spec.metadata.run_id.clone(),
            input: envelope.spec.input,
            provenance: envelope.provenance,
            display_input: envelope.display_input,
            source: envelope.source,
            references: envelope.spec.references,
            reference_contexts: envelope.spec.reference_contexts,
            attachments: submitted_attachments,
        })
        .await;
    let outcome = match outcome {
        Ok(mut outcome) if generate_title => {
            if let Ok(event) = harness
                .append_event(
                    envelope.spec.metadata.run_id.clone(),
                    SessionEventKind::SessionTitleGenerationStarted,
                )
                .await
            {
                outcome.events.push(event);
            }
            let mut generated = false;
            if let Ok(title) = harness
                .generate_session_title(
                    envelope.spec.metadata.run_id.clone(),
                    &title_request,
                    &outcome.answer,
                )
                .await
            {
                if let Ok(event) = harness
                    .append_event(
                        envelope.spec.metadata.run_id.clone(),
                        SessionEventKind::SessionTitleGenerated {
                            title: title.clone(),
                        },
                    )
                    .await
                {
                    outcome.events.push(event);
                }
                outcome.generated_title = Some(title);
                generated = true;
            }
            if let Ok(event) = harness
                .append_event(
                    envelope.spec.metadata.run_id.clone(),
                    SessionEventKind::SessionTitleGenerationFinished { generated },
                )
                .await
            {
                outcome.events.push(event);
            }
            Ok(outcome)
        }
        outcome => outcome,
    };
    if outcome.is_ok() {
        subagent_cleanup.preserve_delivered();
    }
    let shutdown = harness.shutdown().await;
    subagent_cleanup.finish_parent(outcome.is_ok() && shutdown.is_ok());
    let cleanup = subagent_cleanup.drain().await;
    let shutdown = shutdown.and(cleanup);
    let result = match outcome {
        Ok(outcome) => match shutdown {
            Ok(()) => {
                protocol
                    .emit(&ChildToParentFrame::Outcome { outcome })
                    .await
            }
            Err(error) => Err(error),
        },
        Err(error) => {
            let _ = shutdown;
            match protocol
                .emit(&ChildToParentFrame::Error {
                    error: error.clone(),
                })
                .await
            {
                Ok(()) => Err(error),
                Err(protocol_error) => Err(protocol_error),
            }
        }
    };
    input_router.abort();
    let _ = input_router.await;
    session_command_router.abort();
    let _ = session_command_router.await;
    result
}

#[expect(
    clippy::too_many_lines,
    reason = "Keep daemon startup and task shutdown order explicit in one scope."
)]
async fn daemon(mut loaded: LoadedWorkerConfig) -> Result<(), HarnessError> {
    isolation::verify(loaded.sandbox).await?;
    let store = WorkerClient::new(&loaded.server_url, loaded.token)?;
    let information = store.configuration().await?;
    let storage_root = storage_root::RegisteredStorageRoot::initialize(
        &loaded.workspace_root,
        &information.storage_id,
        information.expected_root_id.as_deref(),
    )?;
    loaded.workspace_root = storage_root.path().to_owned();
    let root_id = storage_root.root_id().to_owned();
    let catalog = worker_catalog()?;
    let hello = session_commands::worker_hello(
        information.worker_id.as_str(),
        catalog.revision(),
        now_ms()?,
    );
    let registration = store
        .register(ternilo_cloud::WorkerRegisterRequest {
            capacity: loaded.capacity,
            hello: hello.clone(),
            storage_id: information.storage_id,
            root_id,
        })
        .await?;
    let policy = registration.policy;
    policy.validate_operational_limits()?;
    if policy.catalog_revision != catalog.revision() {
        return Err(HarnessError::policy(
            "Server policy does not match the linked Worker catalog",
        ));
    }
    let policy_directory =
        private_temporary_directory("ternilo-worker-policy-").map_err(|error| {
            HarnessError::execution(format!("create Worker policy directory: {error}"))
        })?;
    let policy_path = policy_directory.path().join("policy.json");
    write_json(&policy_path, &policy)?;
    let config = DaemonConfig {
        worker_id: registration.identity.worker_id.to_string(),
        capacity: registration.capacity,
        lease_ttl: Duration::from_secs(registration.lease_seconds),
        workspace_root: loaded.workspace_root,
        storage_root,
        sandbox: loaded.sandbox,
        health_listen: loaded.health_listen,
        poll_interval: loaded.poll_interval,
        telemetry_endpoint: loaded.telemetry_endpoint,
        telemetry_headers: loaded.telemetry_headers,
        telemetry_service_name: loaded.telemetry_service_name,
        telemetry_timeout: loaded.telemetry_timeout,
    };
    let active_runs = ActiveRuns::default();
    let command_plane = session_commands::CommandPlane::new(
        registration.identity,
        config.lease_ttl,
        active_runs.clone(),
        config.storage_root.clone(),
        policy.clone(),
    );
    let worker_identity = command_plane.identity().clone();
    let mut health = health::HealthServer::bind(
        config.health_listen,
        store.clone(),
        worker_identity.clone(),
        config.workspace_root.clone(),
        policy.minimum_workspace_free_bytes,
    )
    .await?;
    let telemetry_exporter = telemetry_exporter_config(&config)?;
    let executable = std::env::current_exe()
        .map_err(|error| HarnessError::execution(format!("resolve worker executable: {error}")))?
        .canonicalize()
        .map_err(|error| HarnessError::execution(format!("canonicalize worker: {error}")))?;
    println!(
        "Ternilo cloud worker {} ready ({:?})",
        config.worker_id, config.sandbox
    );
    println!("Cloud Worker health listening on http://{}", health.address);
    let mut shutdown = Box::pin(shutdown_signal());
    let (command_shutdown, command_shutdown_rx) = tokio::sync::watch::channel(false);
    let mut command_task =
        tokio::spawn(command_plane.run(store.clone(), config.poll_interval, command_shutdown_rx));
    let mut telemetry_task = tokio::spawn(run_telemetry_exporter(
        store.clone(),
        telemetry_exporter,
        config.lease_ttl,
        config.poll_interval,
        command_shutdown.subscribe(),
    ));
    let mut run_tasks = tokio::task::JoinSet::new();
    let _workspace_recovery =
        workspace_recovery::RecoveryTask::spawn(store.clone(), config.storage_root.clone());
    let mut capacity_probes = tokio::task::JoinSet::new();
    let mut dispatch = tokio::time::interval(config.poll_interval);
    dispatch.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            () = &mut shutdown => {
                health.mark_draining();
                capacity_probes.abort_all();
                while capacity_probes.join_next().await.is_some() {}
                stop_daemon_tasks(
                    &command_shutdown,
                    &mut command_task,
                    &mut telemetry_task,
                    &mut run_tasks,
                    &active_runs,
                ).await;
                store.drain().await?;
                health.stop().await;
                return Ok(());
            },
            result = &mut command_task => {
                health.mark_draining();
                capacity_probes.abort_all();
                while capacity_probes.join_next().await.is_some() {}
                telemetry_task.abort();
                let _ = telemetry_task.await;
                drain_run_tasks(&active_runs, &mut run_tasks).await;
                store.drain().await?;
                health.stop().await;
                return result.map_err(|error| {
                    HarnessError::execution(format!("cloud command plane task failed: {error}"))
                })?;
            },
            result = &mut telemetry_task => {
                health.mark_draining();
                capacity_probes.abort_all();
                while capacity_probes.join_next().await.is_some() {}
                let _ = command_shutdown.send(true);
                command_task.abort();
                let _ = command_task.await;
                drain_run_tasks(&active_runs, &mut run_tasks).await;
                store.drain().await?;
                health.stop().await;
                return result.map_err(|error| {
                    HarnessError::execution(format!("cloud telemetry task failed: {error}"))
                })?;
            },
            result = health.join() => {
                capacity_probes.abort_all();
                while capacity_probes.join_next().await.is_some() {}
                let _ = command_shutdown.send(true);
                command_task.abort();
                let _ = command_task.await;
                telemetry_task.abort();
                let _ = telemetry_task.await;
                drain_run_tasks(&active_runs, &mut run_tasks).await;
                store.drain().await?;
                result.map_err(|error| {
                    HarnessError::execution(format!("cloud Worker health task failed: {error}"))
                })??;
                return Err(HarnessError::execution(
                    "cloud Worker health server stopped unexpectedly",
                ));
            },
            joined = run_tasks.join_next(), if !run_tasks.is_empty() => {
                if let Some(Ok(Err(error))) = joined {
                    eprintln!("cloud worker cycle failed: {error}");
                } else if let Some(Err(error)) = joined {
                    eprintln!("cloud worker run task failed: {error}");
                }
            },
            joined = capacity_probes.join_next(), if !capacity_probes.is_empty() => {
                if let Some(Ok(Err(error))) = joined { eprintln!("cloud capacity probe failed: {error}"); }
            },
            _ = dispatch.tick() => {
                if run_tasks.len() >= config.capacity.max_resident_runs as usize {
                    // A full resident pool must still let the Server diagnose accepted dependency
                    // starvation. This probe never starts a process or consumes a local run slot.
                    if capacity_probes.is_empty() {
                        let probe_store = store.clone();
                        capacity_probes.spawn(async move {
                            if let Some(claim) = probe_store.claim_run().await? {
                                probe_store.release_claim(&claim).await?;
                            }
                            Ok::<(), HarnessError>(())
                        });
                    }
                    continue;
                }
                let task_store = store.clone();
                let task_config = config.clone();
                let task_policy = policy.clone();
                let task_policy_path = policy_path.clone();
                let task_executable = executable.clone();
                let task_active_runs = active_runs.clone();
                run_tasks.spawn(async move {
                    let mut storage_claims_paused = false;
                    run_daemon_cycle(
                        &task_store,
                        &task_config,
                        &task_policy,
                        &task_policy_path,
                        &task_executable,
                        &mut storage_claims_paused,
                        &task_active_runs,
                    ).await
                });
            }
        }
    }
}

async fn stop_daemon_tasks(
    command_shutdown: &tokio::sync::watch::Sender<bool>,
    command_task: &mut tokio::task::JoinHandle<Result<(), HarnessError>>,
    telemetry_task: &mut tokio::task::JoinHandle<Result<(), HarnessError>>,
    run_tasks: &mut tokio::task::JoinSet<Result<(), HarnessError>>,
    active_runs: &ActiveRuns,
) {
    let _ = command_shutdown.send(true);
    drain_run_tasks(active_runs, run_tasks).await;
    command_task.abort();
    let _ = command_task.await;
    telemetry_task.abort();
    let _ = telemetry_task.await;
}

async fn drain_run_tasks(
    active_runs: &ActiveRuns,
    run_tasks: &mut tokio::task::JoinSet<Result<(), HarnessError>>,
) {
    active_runs.request_shutdown(Duration::from_secs(5)).await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while !run_tasks.is_empty() {
        if tokio::time::timeout_at(deadline, run_tasks.join_next())
            .await
            .is_err()
        {
            run_tasks.abort_all();
            while run_tasks.join_next().await.is_some() {}
            break;
        }
    }
}

fn telemetry_exporter_config(
    config: &DaemonConfig,
) -> Result<Option<ternilo_builtins::OtlpTelemetryExporterConfig>, HarnessError> {
    let Some(endpoint) = config
        .telemetry_endpoint
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    else {
        return Ok(None);
    };
    if config.telemetry_service_name.trim().is_empty() || config.telemetry_timeout.is_zero() {
        return Err(HarnessError::invalid(
            "cloud telemetry service name and timeout must be configured",
        ));
    }
    let headers = match config
        .telemetry_headers
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        Some(value) => serde_json::from_str(value).map_err(|error| {
            HarnessError::invalid(format!("decode cloud OTLP headers JSON: {error}"))
        })?,
        None => BTreeMap::new(),
    };
    Ok(Some(ternilo_builtins::OtlpTelemetryExporterConfig {
        endpoint: endpoint.to_owned(),
        service_name: config.telemetry_service_name.clone(),
        timeout: config.telemetry_timeout,
        headers,
    }))
}

async fn run_telemetry_exporter(
    store: WorkerClient,
    exporter: Option<ternilo_builtins::OtlpTelemetryExporterConfig>,
    lease: Duration,
    poll_interval: Duration,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) -> Result<(), HarnessError> {
    let Some(exporter) = exporter else {
        while !*shutdown.borrow() && shutdown.changed().await.is_ok() {}
        return Ok(());
    };
    let occurrence_lease = lease.max(exporter.timeout.saturating_mul(2));
    loop {
        if *shutdown.borrow() {
            return Ok(());
        }
        let occurrences = store.claim_telemetry(occurrence_lease).await?;
        if occurrences.is_empty() {
            tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        return Ok(());
                    }
                }
                () = tokio::time::sleep(poll_interval) => {}
            }
            continue;
        }
        let mut export_failed = false;
        for occurrence in occurrences {
            if *shutdown.borrow() {
                return Ok(());
            }
            let records = occurrence.records();
            let result = if records.is_empty() {
                Ok(())
            } else {
                ternilo_builtins::export_otlp_telemetry_occurrence(
                    &exporter,
                    &occurrence.occurrence_id,
                    records,
                )
                .await
            };
            match result {
                Ok(()) => {
                    store.acknowledge_telemetry(&occurrence).await?;
                }
                Err(error) => {
                    export_failed = true;
                    store.fail_telemetry(&occurrence, &error.message).await?;
                    eprintln!(
                        "cloud telemetry occurrence {} export failed: {}",
                        occurrence.occurrence_id, error
                    );
                }
            }
        }
        if export_failed {
            tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        return Ok(());
                    }
                }
                () = tokio::time::sleep(poll_interval) => {}
            }
        }
    }
}

async fn run_daemon_cycle(
    store: &WorkerClient,
    config: &DaemonConfig,
    policy: &WorkerPolicy,
    policy_path: &Path,
    executable: &Path,
    storage_claims_paused: &mut bool,
    active_runs: &ActiveRuns,
) -> Result<(), HarnessError> {
    if let Err(error) =
        enforce_workspace_free_space(&config.workspace_root, policy.minimum_workspace_free_bytes)
            .await
    {
        if !*storage_claims_paused {
            eprintln!(
                "cloud worker stopped claiming runs until workspace storage recovers: {error}"
            );
            *storage_claims_paused = true;
        }
        tokio::time::sleep(config.poll_interval.max(Duration::from_secs(5))).await;
        return Ok(());
    }
    if *storage_claims_paused {
        println!("cloud worker resumed claiming runs after workspace storage recovered");
        *storage_claims_paused = false;
    }
    let Some(claim) = store.claim_run().await? else {
        tokio::time::sleep(config.poll_interval).await;
        return Ok(());
    };
    let Some(started) = store.start_run(&claim).await? else {
        store.release_claim(&claim).await?;
        tokio::time::sleep(config.poll_interval).await;
        return Ok(());
    };
    let occupancy_guard = match acquire_workspace_guard(store, &config.storage_root, &started) {
        Ok(guard) => guard,
        Err(error) => {
            // The server has already reserved this run. Keep its physical occupancy in
            // cleanup until a Worker with a real filesystem proof can finish it.
            fail_preflight(store, &started, error).await?;
            return Ok(());
        }
    };
    let host_workspace = match prepare_workspace(
        &config.storage_root,
        &started.claim.spec.metadata.tenant_id,
        &started.claim.spec.metadata.workspace_id,
        config.sandbox == SandboxMode::Container,
    )
    .await
    {
        Ok(workspace) => workspace,
        Err(error) => {
            fail_preflight_with_guard(store, &started, error, occupancy_guard).await?;
            return Ok(());
        }
    };
    let tenant_workspace_root = host_workspace
        .parent()
        .ok_or_else(|| HarnessError::execution("cloud workspace has no tenant root"))?
        .to_owned();
    if let Err(error) = enforce_workspace_quota(&tenant_workspace_root, policy).await {
        fail_preflight_with_guard(store, &started, error, occupancy_guard).await?;
        return Ok(());
    }
    let extensions = match store.extensions_for_run(&started).await {
        Ok(plugins) => plugins,
        Err(error) => {
            fail_preflight_with_guard(store, &started, error, occupancy_guard).await?;
            return Ok(());
        }
    };
    execute_started(
        store,
        config.lease_ttl,
        config.sandbox,
        policy.clone(),
        policy_path,
        executable,
        &host_workspace,
        &tenant_workspace_root,
        started,
        extensions,
        active_runs,
        occupancy_guard,
    )
    .await
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
async fn execute_started(
    store: &WorkerClient,
    lease_ttl: Duration,
    sandbox: SandboxMode,
    policy: WorkerPolicy,
    policy_path: &Path,
    executable: &Path,
    host_workspace: &Path,
    tenant_workspace_root: &Path,
    started: StartedRun,
    extensions: Vec<ternilo_extension::ExtensionDistribution>,
    active_runs: &ActiveRuns,
    mut occupancy_guard: Option<WorkspaceOccupancyGuard>,
) -> Result<(), HarnessError> {
    let worker_generation = store.worker_generation()?;
    let settling = std::sync::atomic::AtomicBool::new(false);
    let physical_exit_confirmed = std::sync::atomic::AtomicBool::new(false);
    let result = {
        let execution = execute_started_inner(
            store,
            lease_ttl,
            sandbox,
            policy,
            policy_path,
            executable,
            host_workspace,
            tenant_workspace_root,
            &started,
            extensions,
            active_runs,
            &settling,
            &physical_exit_confirmed,
            occupancy_guard.as_ref(),
        );
        let maintenance = async {
            let period = lease_ttl / 3;
            let mut interval =
                tokio::time::interval_at(tokio::time::Instant::now() + period, period);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                interval.tick().await;
                if let Err(error) = store.renew_run(&started).await {
                    break error;
                }
            }
        };
        tokio::pin!(execution);
        tokio::select! {
            biased;
            result = &mut execution => result,
            error = maintenance => {
                if settling.load(std::sync::atomic::Ordering::Acquire) {
                    // A committed terminal state retires its lease before the HTTP reply arrives.
                    // Let the already-issued fenced settlement decide the result. Physical
                    // cleanup is checked independently of this terminal request.
                    execution.await
                } else {
                    Err(error)
                }
            }
        }
    };
    if settling.load(std::sync::atomic::Ordering::Acquire)
        && physical_exit_confirmed.load(std::sync::atomic::Ordering::Acquire)
    {
        #[cfg(target_os = "linux")]
        if let Some(guard) = occupancy_guard.take() {
            // The guard is cleared only after ManagedChild has confirmed the complete
            // controlled process scope. A failed proof deliberately leaves server cleanup held.
            guard.confirm_stopped().map_err(|error| {
                eprintln!("cloud workspace exit confirmation failed: {error}");
                error
            })?;
        }
        #[cfg(not(target_os = "linux"))]
        let _ = occupancy_guard.take();
        // Only a completed process drain (or a preparation failure before spawning) reaches
        // settlement. Losing a lease and dropping a child is not proof that it has exited.
        if let Err(error) = store.release_resident(&started, worker_generation).await {
            eprintln!("cloud resident cleanup acknowledgement failed: {error}");
            if result.is_ok() {
                return Err(error);
            }
        }
    }
    result
}

async fn finish_supervised_run(
    store: &WorkerClient,
    started: &StartedRun,
    settling: &std::sync::atomic::AtomicBool,
    terminal: TerminalState,
    outcome: Option<&RunOutcome>,
    error: Option<&HarnessError>,
) -> Result<(), HarnessError> {
    settling.store(true, std::sync::atomic::Ordering::Release);
    store.finish_run(started, terminal, outcome, error).await
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
async fn execute_started_inner(
    store: &WorkerClient,
    lease_ttl: Duration,
    sandbox: SandboxMode,
    policy: WorkerPolicy,
    policy_path: &Path,
    executable: &Path,
    host_workspace: &Path,
    tenant_workspace_root: &Path,
    started: &StartedRun,
    extensions: Vec<ternilo_extension::ExtensionDistribution>,
    active_runs: &ActiveRuns,
    settling: &std::sync::atomic::AtomicBool,
    physical_exit_confirmed: &std::sync::atomic::AtomicBool,
    occupancy_guard: Option<&WorkspaceOccupancyGuard>,
) -> Result<(), HarnessError> {
    let directory = private_temporary_directory("ternilo-cloud-run-")
        .map_err(|error| HarnessError::execution(format!("create run directory: {error}")))?;
    let envelope_path = directory.path().join("envelope.json");
    let child_workspace = if sandbox == SandboxMode::Process {
        host_workspace
            .to_str()
            .ok_or_else(|| HarnessError::execution("workspace path is not valid UTF-8"))?
            .to_owned()
    } else {
        "/workspace".to_owned()
    };
    let preparation = async {
        let mut spec = started.claim.spec.clone();
        spec.reference_contexts
            .extend(prepare_cloud_reference_contexts(store, host_workspace, started).await?);
        let (display_input, source, provenance, mut additional_inputs) =
            cloud_run_message_metadata(store, started).await?;
        for input in &mut additional_inputs {
            input
                .reference_contexts
                .extend(resolve_file_references(host_workspace, &input.references).await?);
            input.reference_contexts =
                fit_reference_contexts(std::mem::take(&mut input.reference_contexts));
        }
        let attachment_objects =
            prepare_attachment_objects(store, host_workspace, started, &additional_inputs).await?;
        let envelope = ExecutionEnvelope {
            additional_inputs,
            provenance,
            spec,
            attachment_objects,
            workspace: WorkspaceBinding {
                workspace_id: started.claim.spec.metadata.workspace_id.clone(),
                path: child_workspace,
            },
            prior_events: started.prior_events.clone(),
            extensions,
            display_input,
            source,
        };
        let encoded = serde_json::to_vec(&envelope)
            .map_err(|error| HarnessError::execution(format!("encode run envelope: {error}")))?;
        tokio::fs::write(&envelope_path, encoded)
            .await
            .map_err(|error| HarnessError::execution(format!("write run envelope: {error}")))?;
        spawn_child(
            sandbox,
            executable,
            policy_path,
            &envelope_path,
            host_workspace,
        )
    };
    let mut child = match preparation.await {
        Ok(child) => child,
        Err(error) => {
            physical_exit_confirmed.store(true, std::sync::atomic::Ordering::Release);
            let seq = u64::try_from(started.prior_events.len())
                .map_err(|_| HarnessError::execution("cloud session sequence exceeds u64"))?;
            store
                .append_event(
                    started,
                    &SessionEvent {
                        seq,
                        occurred_at_ms: now_ms()?,
                        run_id: started.claim.run_id.clone(),
                        kind: SessionEventKind::TurnFailed {
                            message: error.message.clone(),
                        },
                    },
                )
                .await?;
            return finish_supervised_run(
                store,
                started,
                settling,
                TerminalState::Failed,
                None,
                Some(&error),
            )
            .await;
        }
    };
    if let Err(error) = child
        .establish_sandbox(|identity| {
            #[cfg(target_os = "linux")]
            if let Some(guard) = occupancy_guard {
                guard
                    .record_namespace(identity)
                    .map_err(|error| std::io::Error::other(error.message))?;
            }
            #[cfg(not(target_os = "linux"))]
            let _ = (identity, occupancy_guard);
            Ok(())
        })
        .await
    {
        let _ = child.start_kill();
        if child.wait().await.is_ok() {
            physical_exit_confirmed.store(true, std::sync::atomic::Ordering::Release);
        }
        let detail = isolation::startup_diagnostic(&mut child).await;
        let error = HarnessError::execution(format!("confirm sandbox startup: {error}{detail}"));
        let seq = u64::try_from(started.prior_events.len())
            .map_err(|_| HarnessError::execution("cloud session sequence exceeds u64"))?;
        store
            .append_event(
                started,
                &SessionEvent {
                    seq,
                    occurred_at_ms: now_ms()?,
                    run_id: started.claim.run_id.clone(),
                    kind: SessionEventKind::TurnFailed {
                        message: error.message.clone(),
                    },
                },
            )
            .await?;
        return finish_supervised_run(
            store,
            started,
            settling,
            TerminalState::Failed,
            None,
            Some(&error),
        )
        .await;
    }
    let attachment_reader = LocalAttachmentReader::new(&host_workspace.join(".ternilo"));
    let mut child_input = child
        .stdin
        .take()
        .ok_or_else(|| HarnessError::execution("cloud child stdin was not piped"))?;
    #[cfg(target_os = "linux")]
    if let Some(guard) = occupancy_guard {
        guard.authorize_startup()?;
    }
    child_protocol::authorize_startup(&mut child_input).await?;
    let parent_protocol = Arc::new(ParentProtocol::new(child_input));
    let model_cancellation = RunCancellation::new();
    let durable_cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let active_handle = Arc::new(ActiveRunHandle::new(
        started.clone(),
        host_workspace.to_owned(),
        Arc::clone(&parent_protocol),
        model_cancellation.clone(),
        Arc::clone(&durable_cancel),
    ));
    let mut active_registration = Some(active_runs.register(Arc::clone(&active_handle))?);
    let mut model_tasks = tokio::task::JoinSet::new();
    let mut host_tasks = tokio::task::JoinSet::new();
    let host_run_ids = Arc::new(Mutex::new(BTreeMap::new()));
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| HarnessError::execution("cloud child stdout was not piped"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| HarnessError::execution("cloud child stderr was not piped"))?;
    let stderr_drain = tokio::spawn(async move {
        let mut stderr = stderr;
        let mut sink = tokio::io::sink();
        let _ = tokio::io::copy(&mut stderr, &mut sink).await;
    });
    let mut lines = BufReader::new(stdout).lines();
    let control_period = lease_ttl / 3;
    let mut control_check = tokio::time::interval(control_period);
    control_check.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut quota_check = tokio::time::interval_at(
        tokio::time::Instant::now() + Duration::from_secs(2),
        Duration::from_secs(2),
    );
    quota_check.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut outcome: Option<RunOutcome> = None;
    let mut child_error: Option<HarnessError> = None;
    let mut workspace_quota_error: Option<HarnessError> = None;
    let mut cancellation = false;
    let mut plugin_revoked = false;
    let mut terminal_event_seen = started
        .prior_events
        .iter()
        .rev()
        .find(|event| event.run_id == started.claim.run_id)
        .is_some_and(|event| terminal_event(&event.kind));
    let mut next_seq = u64::try_from(started.prior_events.len())
        .map_err(|_| HarnessError::execution("cloud session sequence exceeds u64"))?;
    let mut child_status = None;
    let mut stdout_closed = false;
    let status = loop {
        if stdout_closed && let Some(status) = child_status {
            break status;
        }
        tokio::select! {
            line = lines.next_line(), if !stdout_closed => {
                match line.map_err(|error| HarnessError::execution(format!("read cloud child protocol: {error}")))? {
                    Some(line) => {
                        let message: ChildToParentFrame = serde_json::from_str(&line).map_err(|_| {
                            HarnessError::policy("cloud child emitted an invalid protocol message")
                        })?;
                        match message {
                            ChildToParentFrame::Event { event } => {
                                if event.seq != next_seq {
                                    return Err(HarnessError::policy(format!(
                                        "cloud child event sequence mismatch: expected {next_seq}, found {}",
                                        event.seq
                                    )));
                                }
                                let event_terminal = terminal_event(&event.kind);
                                if event_terminal
                                    && let Err(error) = enforce_workspace_quota(
                                        tenant_workspace_root,
                                        &policy,
                                    )
                                    .await
                                {
                                    stop_child_for_workspace_limit(
                                        &mut child,
                                        &parent_protocol,
                                        &model_cancellation,
                                    )
                                    .await?;
                                    workspace_quota_error = Some(error);
                                    continue;
                                }
                                mirror_event_attachments(
                                    store,
                                    started,
                                    &attachment_reader,
                                    &event,
                                )
                                .await?;
                                if event_terminal {
                                    drop(active_registration.take());
                                }
                                terminal_event_seen |= event_terminal;
                                store.append_event(started, &event).await?;
                                next_seq = next_seq.saturating_add(1);
                            }
                            ChildToParentFrame::ModelRequest { request_id, binding, request } => {
                                let protocol = Arc::clone(&parent_protocol);
                                let store = store.clone();
                                let task_started = started.clone();
                                let cancellation = model_cancellation.clone();
                                model_tasks.spawn(async move {
                                    serve_model_request(protocol, store, task_started, request_id, binding, *request, cancellation).await;
                                });
                            }
                            ChildToParentFrame::Question { question } => {
                                let protocol = Arc::clone(&parent_protocol);
                                let store = store.clone();
                                let task_started = started.clone();
                                let cancellation = model_cancellation.clone();
                                model_tasks.spawn(async move {
                                    serve_question(
                                        protocol,
                                        store,
                                        task_started,
                                        *question,
                                        cancellation,
                                    )
                                    .await;
                                });
                            }
                            ChildToParentFrame::SessionCommandReply { command_id, outcome } => {
                                active_handle.resolve_command(&command_id, outcome).await;
                            }
                            ChildToParentFrame::HostRequest { request_id, request } => {
                                let protocol = Arc::clone(&parent_protocol);
                                let store = store.clone();
                                let task_started = started.clone();
                                let task_policy = policy.clone();
                                let task_host_run_ids = Arc::clone(&host_run_ids);
                                if matches!(request, CloudHostRequest::EnqueueSubagent { .. } | CloudHostRequest::CancelSubagent { .. }) {
                                    // Process acceptance and cancellation in pipe order, so a cancellation
                                    // arriving during enqueue also targets the eventual accepted run.
                                    serve_ordered_cloud_host_request(
                                        &store, &task_started, &task_policy, request_id,
                                        &task_host_run_ids, &protocol, request,
                                    ).await?;
                                } else {
                                    host_tasks.spawn(async move {
                                        let outcome = match serve_cloud_host_request(
                                            &store, &task_started, &task_policy, request_id,
                                            &task_host_run_ids, request,
                                        ).await {
                                            Ok(value) => CloudHostOutcome::Ok { value },
                                            Err(error) => CloudHostOutcome::Error { error },
                                        };
                                        let _ = protocol.send(&ParentToChildFrame::HostReply {
                                            request_id, outcome,
                                        }).await;
                                    });
                                }
                            }
                            ChildToParentFrame::Outcome { outcome: value } => {
                                if outcome.is_some() {
                                    return Err(HarnessError::policy("cloud child emitted more than one outcome"));
                                }
                                outcome = Some(value);
                                drop(active_registration.take());
                                parent_protocol.close().await?;
                            }
                            ChildToParentFrame::Error { error } => {
                                child_error = Some(error);
                                drop(active_registration.take());
                                parent_protocol.close().await?;
                            }
                        }
                    }
                    None => stdout_closed = true,
                }
            }
            status = child.wait(), if child_status.is_none() => {
                // Cleanup closes descendant-owned pipes; drain buffered frames before settlement.
                child_status = Some(status.map_err(|error| {
                    HarnessError::execution(format!("wait for cloud child: {error}"))
                })?);
                physical_exit_confirmed.store(true, std::sync::atomic::Ordering::Release);
            }
            _ = control_check.tick(), if child_status.is_none() => {
                if active_handle.durable_cancel_requested()
                    || store.cancel_requested(started).await?
                {
                    cancellation = true;
                    child.start_kill().map_err(|error| {
                        HarnessError::execution(format!("cancel cloud child: {error}"))
                    })?;
                } else if !store.extensions_active(started).await? {
                    plugin_revoked = true;
                    child.start_kill().map_err(|error| {
                        HarnessError::execution(format!("stop cloud child after plugin revocation: {error}"))
                    })?;
                }
            }
            _ = quota_check.tick(), if workspace_quota_error.is_none() && !terminal_event_seen => {
                if let Err(error) = enforce_workspace_quota(tenant_workspace_root, &policy).await {
                    stop_child_for_workspace_limit(
                        &mut child,
                        &parent_protocol,
                        &model_cancellation,
                    ).await?;
                    workspace_quota_error = Some(error);
                }
            }
        }
    };
    drop(active_registration.take());
    if workspace_quota_error.is_none()
        && !terminal_event_seen
        && let Err(error) = enforce_workspace_quota(tenant_workspace_root, &policy).await
    {
        model_cancellation.cancel();
        workspace_quota_error = Some(error);
    }
    model_cancellation.cancel();
    host_tasks.abort_all();
    while host_tasks.join_next().await.is_some() {}
    while !model_tasks.is_empty() {
        tokio::select! {
            joined = model_tasks.join_next() => {
                if let Some(Err(error)) = joined
                    && child_error.is_none()
                {
                    child_error = Some(HarnessError::execution(format!(
                        "cloud child broker task failed: {error}"
                    )));
                }
            }
        }
    }
    let _ = stderr_drain.await;
    cancellation = cancellation_requested(&durable_cancel, cancellation);
    store.requeue_steering(started).await?;

    if let Some(error) = workspace_quota_error {
        if !terminal_event_seen {
            let event = SessionEvent {
                seq: next_seq,
                occurred_at_ms: now_ms()?,
                run_id: started.claim.run_id.clone(),
                kind: SessionEventKind::TurnFailed {
                    message: error.message.clone(),
                },
            };
            store.append_event(started, &event).await?;
        }
        return finish_supervised_run(
            store,
            started,
            settling,
            TerminalState::Failed,
            None,
            Some(&error),
        )
        .await;
    }

    if cancellation {
        if !terminal_event_seen {
            let event = SessionEvent {
                seq: next_seq,
                occurred_at_ms: now_ms()?,
                run_id: started.claim.run_id.clone(),
                kind: SessionEventKind::TurnCancelled,
            };
            store.append_event(started, &event).await?;
        }
        return finish_supervised_run(
            store,
            started,
            settling,
            TerminalState::Cancelled,
            None,
            None,
        )
        .await;
    }

    if plugin_revoked {
        let error = HarnessError::policy(
            "an extension package or publisher was disabled or revoked during execution",
        );
        if !terminal_event_seen {
            let event = SessionEvent {
                seq: next_seq,
                occurred_at_ms: now_ms()?,
                run_id: started.claim.run_id.clone(),
                kind: SessionEventKind::TurnFailed {
                    message: error.message.clone(),
                },
            };
            store.append_event(started, &event).await?;
        }
        return finish_supervised_run(
            store,
            started,
            settling,
            TerminalState::Failed,
            None,
            Some(&error),
        )
        .await;
    }

    if status.success() && outcome.is_some() && child_error.is_none() && terminal_event_seen {
        return finish_supervised_run(
            store,
            started,
            settling,
            TerminalState::Succeeded,
            outcome.as_ref(),
            None,
        )
        .await;
    }

    let error = child_error.unwrap_or_else(|| {
        HarnessError::execution(format!("isolated cloud child exited with {status}"))
    });
    if !terminal_event_seen {
        let event = SessionEvent {
            seq: next_seq,
            occurred_at_ms: now_ms()?,
            run_id: started.claim.run_id.clone(),
            kind: SessionEventKind::TurnFailed {
                message: "isolated worker execution failed".to_owned(),
            },
        };
        store.append_event(started, &event).await?;
    }
    finish_supervised_run(
        store,
        started,
        settling,
        TerminalState::Failed,
        None,
        Some(&error),
    )
    .await
}

#[cfg(target_os = "linux")]
fn acquire_workspace_guard(
    store: &WorkerClient,
    root: &storage_root::RegisteredStorageRoot,
    started: &StartedRun,
) -> Result<Option<WorkspaceOccupancyGuard>, HarnessError> {
    workspace_occupancy::WorkspaceOccupancy::acquire_for_run(store, root, started).map(Some)
}

#[cfg(not(target_os = "linux"))]
fn acquire_workspace_guard(
    _store: &WorkerClient,
    _root: &storage_root::RegisteredStorageRoot,
    _started: &StartedRun,
) -> Result<Option<WorkspaceOccupancyGuard>, HarnessError> {
    Ok(Some(()))
}

async fn prepare_cloud_reference_contexts(
    store: &WorkerClient,
    workspace: &Path,
    run: &StartedRun,
) -> Result<Vec<ReferenceContext>, HarnessError> {
    let mut contexts = resolve_file_references(workspace, &run.claim.spec.references).await?;
    contexts.extend(store.reference_contexts(run).await?);
    Ok(fit_reference_contexts(contexts))
}

async fn cloud_run_message_metadata(
    store: &WorkerClient,
    run: &StartedRun,
) -> Result<
    (
        Option<String>,
        Option<UserMessageSource>,
        Option<ternilo_protocol::InputProvenance>,
        Vec<ternilo_protocol::SteeringInput>,
    ),
    HarnessError,
> {
    let (submission, additional_inputs) = store.run_submission(run).await?;
    let (display_input, source, provenance) = submission_message_metadata(submission);
    Ok((
        display_input,
        source,
        provenance.or_else(|| run.claim.provenance.clone()),
        additional_inputs,
    ))
}

fn submission_message_metadata(
    submission: Option<ternilo_protocol::SessionSubmission>,
) -> (
    Option<String>,
    Option<UserMessageSource>,
    Option<ternilo_protocol::InputProvenance>,
) {
    let Some(submission) = submission else {
        return (None, None, None);
    };
    let skill_name = submission.content.skill_name().map(str::to_owned);
    let display_input = skill_name.as_ref().map(|name| {
        let input = submission.content.input();
        if input.trim().is_empty() {
            format!("/skill {name}")
        } else {
            format!("/skill {name}\n\n{input}")
        }
    });
    (
        display_input,
        Some(UserMessageSource::Submission {
            regenerate_from: submission.content.regeneration_target(),
            submission_id: submission.id,
            created_at_ms: submission.created_at_ms,
            delivery: SubmissionDelivery::Queue,
            skill_name,
        }),
        submission.provenance,
    )
}

async fn prepare_attachment_objects(
    store: &WorkerClient,
    workspace: &Path,
    run: &StartedRun,
    additional_inputs: &[ternilo_protocol::SteeringInput],
) -> Result<Vec<ExecutionAttachmentObject>, HarnessError> {
    let reader = LocalAttachmentReader::new(&workspace.join(".ternilo"));
    let mut references = BTreeMap::new();
    for attachment in run
        .claim
        .spec
        .attachments
        .iter()
        .chain(
            additional_inputs
                .iter()
                .flat_map(|input| &input.attachments),
        )
        .chain(
            run.prior_events
                .iter()
                .flat_map(|event| event_attachment_references(&event.kind)),
        )
    {
        if let Some(digest) = attachment.reference_digest() {
            references.entry(digest.to_owned()).or_insert(attachment);
        }
    }
    let mut objects = Vec::new();
    for attachment in references.values() {
        if reader.reference_bytes(attachment).await.is_ok() {
            continue;
        }
        objects.push(ExecutionAttachmentObject {
            attachment: (*attachment).clone(),
            content_base64: store.download_attachment(run, attachment).await?,
        });
    }
    Ok(objects)
}

async fn restore_attachment_objects(
    attachments: &LocalAttachments,
    objects: Vec<ExecutionAttachmentObject>,
) -> Result<(), HarnessError> {
    use base64::{Engine as _, engine::general_purpose::STANDARD};

    for object in objects {
        let bytes = STANDARD.decode(&object.content_base64).map_err(|_| {
            HarnessError::invalid("Worker attachment object has invalid base64 content")
        })?;
        attachments
            .restore_reference(&object.attachment, &bytes)
            .await?;
    }
    Ok(())
}

async fn mirror_event_attachments(
    store: &WorkerClient,
    run: &StartedRun,
    attachments: &LocalAttachmentReader,
    event: &SessionEvent,
) -> Result<(), HarnessError> {
    for attachment in event_attachment_references(&event.kind) {
        let bytes = attachments.reference_bytes(attachment).await?;
        store.store_attachment(run, attachment, &bytes).await?;
    }
    Ok(())
}

fn event_attachment_references(kind: &SessionEventKind) -> Vec<&Attachment> {
    match kind {
        SessionEventKind::UserMessage { attachments, .. } => attachments
            .iter()
            .filter(|attachment| attachment.is_reference())
            .collect(),
        SessionEventKind::ToolCallFinished {
            retained_output: Some(attachment),
            ..
        }
        | SessionEventKind::CodeDispatchFinished {
            retained_output: Some(attachment),
            ..
        }
        | SessionEventKind::DeliverableProduced { attachment, .. }
            if attachment.is_reference() =>
        {
            vec![attachment]
        }
        _ => Vec::new(),
    }
}

type HostSubagentRuns = Mutex<BTreeMap<(String, String), AcceptedSubagentRun>>;

#[allow(clippy::too_many_arguments)]
async fn enqueue_cloud_subagent(
    store: &WorkerClient,
    parent: &StartedRun,
    policy: &WorkerPolicy,
    request_id: u64,
    host_run_ids: &HostSubagentRuns,
    session_id: ternilo_protocol::SessionId,
    logical_run_id: ternilo_protocol::RunId,
    input: String,
) -> Result<AcceptedSubagentRun, HarnessError> {
    let run_id = cloud_subagent_run_id(&session_id, &logical_run_id, request_id);
    let mut spec = parent.claim.spec.clone();
    spec.metadata.session_id = session_id.clone();
    spec.metadata.run_id = run_id.clone();
    spec.input.clone_from(&input);
    spec.references.clear();
    spec.reference_contexts.clear();
    spec.attachments.clear();
    policy.validate(&spec, &worker_catalog()?)?;
    let accepted = store
        .enqueue_subagent(parent, &session_id, &run_id, &input)
        .await?;
    host_run_ids.lock().await.insert(
        (
            session_id.as_str().to_owned(),
            logical_run_id.as_str().to_owned(),
        ),
        accepted.clone(),
    );
    Ok(accepted)
}

async fn wait_cloud_subagent(
    store: &WorkerClient,
    parent: &StartedRun,
    child: AcceptedSubagentRun,
) -> Result<serde_json::Value, HarnessError> {
    loop {
        let run = store
            .subagent_run(parent, &child.session_id, &child.run_id)
            .await?
            .ok_or_else(|| HarnessError::execution("cloud Subagent run disappeared"))?;
        match run.state {
            ternilo_cloud::CloudRunState::Succeeded => {
                return serde_json::to_value(run.outcome.ok_or_else(|| {
                    HarnessError::execution("successful cloud Subagent run has no durable outcome")
                })?)
                .map_err(|error| {
                    HarnessError::execution(format!("encode cloud Subagent outcome: {error}"))
                });
            }
            ternilo_cloud::CloudRunState::Failed | ternilo_cloud::CloudRunState::Indeterminate => {
                return Err(run
                    .error
                    .unwrap_or_else(|| HarnessError::execution("cloud Subagent run failed")));
            }
            ternilo_cloud::CloudRunState::Cancelled => {
                return Err(HarnessError::cancelled("cloud Subagent run was cancelled"));
            }
            ternilo_cloud::CloudRunState::Queued
            | ternilo_cloud::CloudRunState::Leased
            | ternilo_cloud::CloudRunState::Running
            | ternilo_cloud::CloudRunState::CancelRequested => {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

async fn serve_ordered_cloud_host_request(
    store: &WorkerClient,
    parent: &StartedRun,
    policy: &WorkerPolicy,
    request_id: u64,
    host_run_ids: &HostSubagentRuns,
    protocol: &ParentProtocol,
    request: CloudHostRequest,
) -> Result<(), HarnessError> {
    let admission = match &request {
        CloudHostRequest::EnqueueSubagent {
            session_id, run_id, ..
        } => Some((session_id.clone(), run_id.clone())),
        _ => None,
    };
    let outcome =
        match serve_cloud_host_request(store, parent, policy, request_id, host_run_ids, request)
            .await
        {
            Ok(value) => CloudHostOutcome::Ok { value },
            Err(error) => CloudHostOutcome::Error { error },
        };
    let accepted = matches!(outcome, CloudHostOutcome::Ok { .. });
    if let Err(error) = protocol
        .send(&ParentToChildFrame::HostReply {
            request_id,
            outcome,
        })
        .await
    {
        if accepted && let Some((session_id, run_id)) = admission {
            serve_cloud_host_request(
                store,
                parent,
                policy,
                request_id,
                host_run_ids,
                CloudHostRequest::CancelSubagent { session_id, run_id },
            )
            .await?;
        }
        return Err(error);
    }
    Ok(())
}

#[expect(
    clippy::too_many_lines,
    reason = "Each host request maps directly to its scoped Server operation."
)]
async fn serve_cloud_host_request(
    store: &WorkerClient,
    parent: &StartedRun,
    policy: &WorkerPolicy,
    request_id: u64,
    host_run_ids: &HostSubagentRuns,
    request: CloudHostRequest,
) -> Result<serde_json::Value, HarnessError> {
    match request {
        CloudHostRequest::ParkActivity {
            activity_revision,
            dependencies,
        } => {
            {
                let accepted = host_run_ids.lock().await;
                if dependencies.is_empty()
                    || dependencies
                        .iter()
                        .any(|dependency| !accepted.values().any(|run| run == dependency))
                {
                    return Err(HarnessError::policy(
                        "activity dependencies were not accepted by this parent execution",
                    ));
                }
            }
            let revision = store
                .park_run(parent, activity_revision, dependencies)
                .await?;
            Ok(serde_json::json!(revision))
        }
        CloudHostRequest::ResumeActivity {
            activity_revision,
            parked_revision,
        } => loop {
            match store
                .resume_run(parent, activity_revision, parked_revision)
                .await?
            {
                ready @ ternilo_cloud::RunAdmission::Ready { .. } => {
                    return serde_json::to_value(ready)
                        .map_err(|_| HarnessError::execution("encode execution admission"));
                }
                ternilo_cloud::RunAdmission::Pending => {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        },
        CloudHostRequest::CreateSubagent {
            subagent_id,
            provider,
            label,
            task: _,
            transcript_kind,
        } => {
            let mut digest = Sha256::new();
            digest.update(parent.claim.session_id.as_str().as_bytes());
            digest.update([0]);
            digest.update(subagent_id.as_str().as_bytes());
            let suffix = hex_bytes(&digest.finalize()[..16]);
            let child_session_id = ternilo_protocol::SessionId::new(format!("child-{suffix}"));
            let metadata = ternilo_protocol::SubagentSessionMetadata {
                subagent_id,
                provider,
                transcript_kind,
            };
            let child = store
                .create_subagent(parent, &child_session_id, &metadata, &label)
                .await?;
            serde_json::to_value(Some(child)).map_err(|error| {
                HarnessError::execution(format!("encode cloud Subagent binding: {error}"))
            })
        }
        CloudHostRequest::EnqueueSubagent {
            session_id,
            run_id,
            input,
        } => {
            let child = enqueue_cloud_subagent(
                store,
                parent,
                policy,
                request_id,
                host_run_ids,
                session_id,
                run_id,
                input,
            )
            .await?;
            serde_json::to_value(child).map_err(|error| {
                HarnessError::execution(format!("encode cloud Subagent acceptance: {error}"))
            })
        }
        CloudHostRequest::WaitSubagent { session_id, run_id } => {
            let accepted = AcceptedSubagentRun { session_id, run_id };
            accepted.validate()?;
            if !host_run_ids
                .lock()
                .await
                .values()
                .any(|run| run == &accepted)
            {
                return Err(HarnessError::policy(
                    "cloud Subagent run was not accepted by this parent execution",
                ));
            }
            wait_cloud_subagent(store, parent, accepted).await
        }
        CloudHostRequest::CancelSubagent { session_id, run_id } => {
            let accepted = host_run_ids
                .lock()
                .await
                .get(&(session_id.as_str().to_owned(), run_id.as_str().to_owned()))
                .cloned();
            let Some(accepted) = accepted else {
                // Cancellation can follow a rejected enqueue or a driver dropped before it sent one.
                return Ok(serde_json::json!("not_accepted"));
            };
            let state = store
                .cancel_subagent(parent, &accepted.session_id, &accepted.run_id)
                .await?;
            serde_json::to_value(state).map_err(|error| {
                HarnessError::execution(format!("encode cloud Subagent cancel state: {error}"))
            })
        }
        CloudHostRequest::AppendSubagentLifecycle { .. } => Err(HarnessError::policy(
            "cloud process-only Subagent lifecycle rows are not enabled",
        )),
        CloudHostRequest::AgentTeamSnapshot => {
            store.agent_team(parent, WorkerTeamRequest::Snapshot).await
        }
        CloudHostRequest::AgentTeamTaskCreate { request } => {
            store
                .agent_team(parent, WorkerTeamRequest::CreateTask { request })
                .await
        }
        CloudHostRequest::AgentTeamTaskReplace { task_id, request } => {
            store
                .agent_team(parent, WorkerTeamRequest::ReplaceTask { task_id, request })
                .await
        }
        CloudHostRequest::AgentTeamTaskDelete {
            task_id,
            expected_revision,
        } => {
            store
                .agent_team(
                    parent,
                    WorkerTeamRequest::DeleteTask {
                        task_id,
                        expected_revision,
                    },
                )
                .await
        }
        CloudHostRequest::AgentTeamMessageSend { request } => {
            store
                .agent_team(parent, WorkerTeamRequest::SendMessage { request })
                .await
        }
        CloudHostRequest::AgentTeamMessageRead { message_id } => {
            store
                .agent_team(parent, WorkerTeamRequest::ReadMessage { message_id })
                .await
        }
    }
}

fn cloud_subagent_run_id(
    session_id: &ternilo_protocol::SessionId,
    logical_run_id: &ternilo_protocol::RunId,
    request_id: u64,
) -> ternilo_protocol::RunId {
    let mut digest = Sha256::new();
    digest.update(session_id.as_str().as_bytes());
    digest.update([0]);
    digest.update(logical_run_id.as_str().as_bytes());
    digest.update(request_id.to_be_bytes());
    let suffix = hex_bytes(&digest.finalize()[..16]);
    ternilo_protocol::RunId::new(format!("child-run-{suffix}"))
}

async fn fail_preflight(
    store: &WorkerClient,
    started: &StartedRun,
    error: HarnessError,
) -> Result<(), HarnessError> {
    let seq = u64::try_from(started.prior_events.len())
        .map_err(|_| HarnessError::execution("cloud session sequence exceeds u64"))?;
    let event = SessionEvent {
        seq,
        occurred_at_ms: now_ms()?,
        run_id: started.claim.run_id.clone(),
        kind: SessionEventKind::TurnFailed {
            message: error.message.clone(),
        },
    };
    store.append_event(started, &event).await?;
    store
        .finish_run(started, TerminalState::Failed, None, Some(&error))
        .await
}

async fn fail_preflight_with_guard(
    store: &WorkerClient,
    started: &StartedRun,
    error: HarnessError,
    guard: Option<WorkspaceOccupancyGuard>,
) -> Result<(), HarnessError> {
    // Finish first so the Server records the failed preflight. Release the resident slot only
    // after the local guard has supplied the same physical-exit proof used for normal runs.
    fail_preflight(store, started, error).await?;
    #[cfg(target_os = "linux")]
    if let Some(guard) = guard {
        guard.confirm_stopped()?;
        store
            .release_resident(started, store.worker_generation()?)
            .await?;
    }
    #[cfg(not(target_os = "linux"))]
    let _ = guard;
    Ok(())
}

async fn stop_child_for_workspace_limit(
    child: &mut ManagedChild,
    protocol: &ParentProtocol,
    cancellation: &RunCancellation,
) -> Result<(), HarnessError> {
    cancellation.cancel();
    let _ = protocol.close().await;
    if child
        .try_wait()
        .map_err(|error| HarnessError::execution(format!("poll cloud child: {error}")))?
        .is_none()
    {
        child.start_kill().map_err(|error| {
            HarnessError::execution(format!(
                "stop cloud child after workspace storage limit: {error}"
            ))
        })?;
    }
    Ok(())
}

fn catalog_for_envelope(
    envelope: &ExecutionEnvelope,
    policy: &WorkerPolicy,
) -> Result<(ternilo_kernel::Catalog, Option<tempfile::TempDir>), HarnessError> {
    catalog_for_profile_extensions(&envelope.spec.profile, &envelope.extensions, policy)
}

fn catalog_for_profile_extensions(
    profile: &ternilo_protocol::Profile,
    extensions: &[ternilo_extension::ExtensionDistribution],
    policy: &WorkerPolicy,
) -> Result<(ternilo_kernel::Catalog, Option<tempfile::TempDir>), HarnessError> {
    let references = ternilo_extension::extension_mounts(profile)?
        .into_iter()
        .map(|reference| (reference.package_id, reference.version))
        .collect::<BTreeSet<_>>();
    let delivered = extensions
        .iter()
        .map(|plugin| {
            (
                plugin.install.bundle.manifest.package_id.clone(),
                plugin.install.bundle.manifest.version.clone(),
            )
        })
        .collect::<BTreeSet<_>>();
    if references != delivered || delivered.len() != extensions.len() {
        return Err(HarnessError::policy(
            "cloud extension envelope does not exactly match its RunSpec profile",
        ));
    }
    let mut catalog = worker_catalog()?;
    if extensions.is_empty() {
        return Ok((catalog, None));
    }
    let directory = private_temporary_directory("ternilo-extension-envelope-")
        .map_err(|error| HarnessError::execution(format!("create extension registry: {error}")))?;
    let registry = ternilo_extension::ExtensionRegistry::open(
        directory.path().join("registry"),
        policy.extension_host_policy.clone(),
    )?;
    for (index, distribution) in extensions.iter().enumerate() {
        let timestamp = u64::try_from(index)
            .map_err(|_| HarnessError::execution("extension envelope index exceeds u64"))?;
        registry.trust_publisher(distribution.publisher.clone(), timestamp)?;
        registry.install(distribution.install.clone(), timestamp)?;
    }
    catalog.register(ternilo_extension::extension_mount_factory(registry))?;
    Ok((catalog, Some(directory)))
}

fn worker_catalog() -> Result<ternilo_kernel::Catalog, HarnessError> {
    ternilo_cloud::catalog()
}

struct ParentModelOutput {
    request_id: u64,
    protocol: Arc<ParentProtocol>,
}

impl ModelOutput for ParentModelOutput {
    fn emit<'a>(
        &'a self,
        delta: String,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            let _ = self
                .protocol
                .send(&ParentToChildFrame::ModelDelta {
                    request_id: self.request_id,
                    delta,
                })
                .await;
            Ok(())
        })
    }

    fn emit_reasoning<'a>(
        &'a self,
        delta: String,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            let _ = self
                .protocol
                .send(&ParentToChildFrame::ModelReasoningDelta {
                    request_id: self.request_id,
                    delta,
                })
                .await;
            Ok(())
        })
    }

    fn retry_scheduled<'a>(
        &'a self,
        retry: u32,
        max_retries: u32,
        delay_ms: u64,
        failure: ModelRetryFailure,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            self.protocol
                .send(&ParentToChildFrame::ModelRetryScheduled {
                    request_id: self.request_id,
                    retry,
                    max_retries,
                    delay_ms,
                    failure,
                })
                .await
        })
    }

    fn retry_started<'a>(
        &'a self,
        retry: u32,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            self.protocol
                .send(&ParentToChildFrame::ModelRetryStarted {
                    request_id: self.request_id,
                    retry,
                })
                .await
        })
    }

    fn retry_cancelled<'a>(
        &'a self,
        retry: u32,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            self.protocol
                .send(&ParentToChildFrame::ModelRetryCancelled {
                    request_id: self.request_id,
                    retry,
                })
                .await
        })
    }
}

async fn serve_model_request(
    protocol: Arc<ParentProtocol>,
    store: WorkerClient,
    started: StartedRun,
    request_id: u64,
    binding: ternilo_protocol::RunModelBinding,
    request: ModelRequest,
    cancellation: RunCancellation,
) {
    let output: Arc<dyn ModelOutput> = Arc::new(ParentModelOutput {
        request_id,
        protocol: Arc::clone(&protocol),
    });
    let result = store
        .model_complete(
            &started,
            request_id,
            &binding,
            request,
            output,
            cancellation,
        )
        .await;
    let response = match result {
        Ok(response) => ParentToChildFrame::ModelComplete {
            request_id,
            response,
        },
        Err(error) => ParentToChildFrame::ModelError { request_id, error },
    };
    let _ = protocol.send(&response).await;
}

async fn serve_question(
    protocol: Arc<ParentProtocol>,
    store: WorkerClient,
    started: StartedRun,
    question: UserQuestion,
    cancellation: RunCancellation,
) {
    let question_id = question.id.clone();
    let result = async {
        store.record_question(&started, &question).await?;
        loop {
            tokio::select! {
                () = cancellation.cancelled() => {
                    return Err(HarnessError::cancelled("cloud question was cancelled"));
                }
                () = tokio::time::sleep(Duration::from_millis(200)) => {
                    if let Some(answer) = store
                        .question_answer(&started, &question_id)
                        .await?
                    {
                        return Ok(answer);
                    }
                }
            }
        }
    }
    .await;
    let response = match result {
        Ok(answer) => ParentToChildFrame::QuestionAnswer { answer },
        Err(error) => ParentToChildFrame::QuestionError { question_id, error },
    };
    let _ = protocol.send(&response).await;
}

type PendingModelRequests = Arc<Mutex<BTreeMap<u64, mpsc::Sender<ParentToChildFrame>>>>;
type PendingQuestionRequests =
    Arc<Mutex<BTreeMap<String, oneshot::Sender<Result<UserAnswer, HarnessError>>>>>;
type PendingHostRequests = Arc<Mutex<BTreeMap<u64, oneshot::Sender<CloudHostOutcome>>>>;
type ChildSessionCommandRequest = (ternilo_transport::CommandId, ChildSessionCommand);

struct ChildServices {
    model_gateway: Arc<dyn ModelGatewayProvider>,
    execution_admission: Arc<dyn ternilo_kernel::ExecutionAdmission>,
    interaction: Arc<dyn UserInteraction>,
    subagent_host: Arc<dyn SubagentSessionHost>,
    agent_team: Arc<dyn AgentTeamProvider>,
    input_router: tokio::task::JoinHandle<()>,
    session_commands: mpsc::Receiver<ChildSessionCommandRequest>,
    subagent_cleanup: Arc<SubagentCleanup>,
}

struct PipeModelGateway {
    protocol: Arc<ChildProtocol>,
    pending: PendingModelRequests,
    next_request_id: AtomicU64,
}

impl PipeModelGateway {
    fn start(protocol: Arc<ChildProtocol>) -> ChildServices {
        let pending = Arc::new(Mutex::new(BTreeMap::new()));
        let pending_questions = Arc::new(Mutex::new(BTreeMap::new()));
        let pending_hosts = Arc::new(Mutex::new(BTreeMap::new()));
        let (session_command_sender, session_commands) = mpsc::channel(32);
        let subagent_cleanup = Arc::new(SubagentCleanup::default());
        let cloud_host = Arc::new(PipeCloudHost {
            client: PipeHostClient {
                protocol: Arc::clone(&protocol),
                pending: pending_hosts,
                next_request_id: Arc::new(AtomicU64::new(1)),
                input_closed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            },
            cleanup: Arc::clone(&subagent_cleanup),
        });
        let router = tokio::spawn(route_worker_input(
            Arc::clone(&pending),
            Arc::clone(&pending_questions),
            cloud_host.client.clone(),
            session_command_sender,
        ));
        let gateway: Arc<dyn ModelGatewayProvider> = Arc::new(Self {
            protocol: Arc::clone(&protocol),
            pending,
            next_request_id: AtomicU64::new(1),
        });
        let interaction: Arc<dyn UserInteraction> = Arc::new(PipeInteraction {
            protocol,
            pending: pending_questions,
        });
        let execution_admission: Arc<dyn ternilo_kernel::ExecutionAdmission> = Arc::new(
            execution_activity::PipeExecutionAdmission::new(cloud_host.client.clone()),
        );
        let subagent_host: Arc<dyn SubagentSessionHost> = cloud_host.clone();
        let agent_team: Arc<dyn AgentTeamProvider> = cloud_host;
        ChildServices {
            model_gateway: gateway,
            execution_admission,
            interaction,
            subagent_host,
            agent_team,
            input_router: router,
            session_commands,
            subagent_cleanup,
        }
    }
}

struct PipeCloudHost {
    client: PipeHostClient,
    cleanup: Arc<SubagentCleanup>,
}

#[derive(Clone)]
struct PipeHostClient {
    protocol: Arc<ChildProtocol>,
    pending: PendingHostRequests,
    next_request_id: Arc<AtomicU64>,
    input_closed: Arc<std::sync::atomic::AtomicBool>,
}

impl PipeHostClient {
    async fn begin(
        &self,
        request: CloudHostRequest,
    ) -> Result<(u64, oneshot::Receiver<CloudHostOutcome>), HarnessError> {
        let request_id = self
            .next_request_id
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                current.checked_add(1)
            })
            .map_err(|_| HarnessError::execution("cloud host request id space is exhausted"))?;
        let (sender, receiver) = oneshot::channel();
        {
            let mut pending = self.pending.lock().await;
            if self.input_closed.load(Ordering::Acquire) {
                return Err(HarnessError::execution("cloud parent input is closed"));
            }
            pending.insert(request_id, sender);
        }
        if let Err(error) = self
            .protocol
            .emit(&ChildToParentFrame::HostRequest {
                request_id,
                request,
            })
            .await
        {
            self.pending.lock().await.remove(&request_id);
            return Err(error);
        }
        Ok((request_id, receiver))
    }

    async fn receive_reply(&self, request_id: u64, outcome: CloudHostOutcome) {
        if let Some(sender) = self.pending.lock().await.remove(&request_id) {
            let _ = sender.send(outcome);
        }
    }

    async fn cancel_subagent_run(
        &self,
        session_id: ternilo_protocol::SessionId,
        run_id: ternilo_protocol::RunId,
    ) -> Result<(), HarnessError> {
        self.request(CloudHostRequest::CancelSubagent { session_id, run_id })
            .await
            .map(drop)
    }

    async fn request(&self, request: CloudHostRequest) -> Result<serde_json::Value, HarnessError> {
        let (request_id, receiver) = self.begin(request).await?;
        let outcome = receiver.await.map_err(|_| {
            HarnessError::execution("cloud parent closed before replying to a host request")
        })?;
        self.pending.lock().await.remove(&request_id);
        match outcome {
            CloudHostOutcome::Ok { value } => Ok(value),
            CloudHostOutcome::Error { error } => Err(error),
        }
    }
}

impl PipeCloudHost {
    async fn run_subagent(
        &self,
        session_id: ternilo_protocol::SessionId,
        run_id: ternilo_protocol::RunId,
        input: String,
        cancellation: RunCancellation,
        start: &SubagentRunStart,
    ) -> Result<RunOutcome, HarnessError> {
        cancellation.check()?;
        let mut cleanup = self.cleanup.track(
            self.client.clone(),
            session_id.clone(),
            run_id.clone(),
            start.clone(),
            cancellation.clone(),
        );
        let (request_id, receiver) = self
            .client
            .begin(CloudHostRequest::EnqueueSubagent {
                session_id: session_id.clone(),
                run_id: run_id.clone(),
                input,
            })
            .await?;
        let accepted = tokio::select! {
            biased;
            () = cancellation.cancelled() => {
                self.client.pending.lock().await.remove(&request_id);
                self.client.cancel_subagent_run(session_id, run_id).await?;
                cleanup.disarm();
                return Err(HarnessError::cancelled("cloud Subagent admission was cancelled"));
            }
            outcome = receiver => {
                self.client.pending.lock().await.remove(&request_id);
                let outcome = outcome.map_err(|_| HarnessError::execution(
                    "cloud parent closed before confirming Subagent admission",
                ))?;
                match outcome {
                    CloudHostOutcome::Ok { value } => decode_host_value::<AcceptedSubagentRun>(value)?,
                    CloudHostOutcome::Error { error } => {
                        cleanup.disarm();
                        return Err(error);
                    }
                }
            }
        };
        accepted.validate()?;
        if accepted.session_id != session_id {
            return Err(HarnessError::policy(
                "cloud Subagent acceptance targets another session",
            ));
        }
        start.resolve(Ok(SubagentAdmission::Scheduled(accepted.clone())));
        tokio::select! {
            biased;
            () = cancellation.cancelled() => {
                self.client.cancel_subagent_run(session_id, run_id).await?;
                cleanup.disarm();
                Err(HarnessError::cancelled("cloud Subagent run was cancelled"))
            }
            outcome = self.client.request(CloudHostRequest::WaitSubagent {
                session_id: accepted.session_id, run_id: accepted.run_id,
            }) => {
                let outcome = decode_host_value(outcome?)?;
                cleanup.disarm();
                Ok(outcome)
            },
        }
    }
}

impl SubagentSessionHost for PipeCloudHost {
    fn create<'a>(
        &'a self,
        _: ternilo_protocol::SessionIdentity,
        request: SubagentSessionRequest,
    ) -> Pin<
        Box<dyn Future<Output = Result<Option<SubagentSessionBinding>, HarnessError>> + Send + 'a>,
    > {
        Box::pin(async move {
            let value = self
                .client
                .request(CloudHostRequest::CreateSubagent {
                    subagent_id: request.subagent_id,
                    provider: request.provider,
                    label: request.label,
                    task: request.task,
                    transcript_kind: request.transcript_kind,
                })
                .await?;
            let session_id = serde_json::from_value::<Option<ternilo_protocol::SessionId>>(value)
                .map_err(|error| {
                HarnessError::execution(format!("decode cloud Subagent binding: {error}"))
            })?;
            Ok(session_id.map(|session_id| SubagentSessionBinding { session_id }))
        })
    }

    fn run<'a>(
        &'a self,
        session_id: ternilo_protocol::SessionId,
        run_id: ternilo_protocol::RunId,
        input: String,
        cancellation: RunCancellation,
        start: SubagentRunStart,
    ) -> Pin<Box<dyn Future<Output = Result<RunOutcome, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            let result = self
                .run_subagent(session_id, run_id, input, cancellation, &start)
                .await;
            if let Err(error) = &result {
                start.resolve(Err(error.clone()));
            }
            result
        })
    }

    fn append_lifecycle<'a>(
        &'a self,
        session_id: ternilo_protocol::SessionId,
        run_id: ternilo_protocol::RunId,
        snapshot: ternilo_protocol::SubagentSnapshot,
    ) -> Pin<Box<dyn Future<Output = Result<SessionEvent, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            let value = self
                .client
                .request(CloudHostRequest::AppendSubagentLifecycle {
                    session_id,
                    run_id,
                    snapshot: Box::new(snapshot),
                })
                .await?;
            serde_json::from_value(value).map_err(|error| {
                HarnessError::execution(format!("decode cloud Subagent lifecycle event: {error}"))
            })
        })
    }
}

impl AgentTeamProvider for PipeCloudHost {
    fn snapshot<'a>(
        &'a self,
        _: CallContext<()>,
    ) -> Pin<Box<dyn Future<Output = Result<AgentTeamSnapshot, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            decode_host_value(
                self.client
                    .request(CloudHostRequest::AgentTeamSnapshot)
                    .await?,
            )
        })
    }

    fn create_task<'a>(
        &'a self,
        _: CallContext<()>,
        request: AgentTeamTaskCreate,
    ) -> Pin<Box<dyn Future<Output = Result<AgentTeamTask, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            decode_host_value(
                self.client
                    .request(CloudHostRequest::AgentTeamTaskCreate { request })
                    .await?,
            )
        })
    }

    fn replace_task<'a>(
        &'a self,
        _: CallContext<()>,
        task_id: AgentTeamTaskId,
        request: AgentTeamTaskReplace,
    ) -> Pin<Box<dyn Future<Output = Result<AgentTeamTask, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            decode_host_value(
                self.client
                    .request(CloudHostRequest::AgentTeamTaskReplace { task_id, request })
                    .await?,
            )
        })
    }

    fn delete_task<'a>(
        &'a self,
        _: CallContext<()>,
        task_id: AgentTeamTaskId,
        expected_revision: u64,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            decode_host_value(
                self.client
                    .request(CloudHostRequest::AgentTeamTaskDelete {
                        task_id,
                        expected_revision,
                    })
                    .await?,
            )
        })
    }

    fn send_message<'a>(
        &'a self,
        _: CallContext<()>,
        request: AgentTeamMessageSend,
    ) -> Pin<Box<dyn Future<Output = Result<AgentTeamMessage, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            decode_host_value(
                self.client
                    .request(CloudHostRequest::AgentTeamMessageSend { request })
                    .await?,
            )
        })
    }

    fn mark_message_read<'a>(
        &'a self,
        _: CallContext<()>,
        message_id: AgentTeamMessageId,
    ) -> Pin<Box<dyn Future<Output = Result<AgentTeamMessage, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            decode_host_value(
                self.client
                    .request(CloudHostRequest::AgentTeamMessageRead { message_id })
                    .await?,
            )
        })
    }
}

fn decode_host_value<T: serde::de::DeserializeOwned>(
    value: serde_json::Value,
) -> Result<T, HarnessError> {
    serde_json::from_value(value)
        .map_err(|error| HarnessError::execution(format!("decode cloud host reply: {error}")))
}

struct PipeInteraction {
    protocol: Arc<ChildProtocol>,
    pending: PendingQuestionRequests,
}

impl UserInteraction for PipeInteraction {
    fn ask<'a>(
        &'a self,
        question: UserQuestion,
    ) -> Pin<Box<dyn Future<Output = Result<UserAnswer, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            let question_id = question.id.clone();
            let (sender, receiver) = oneshot::channel();
            if self
                .pending
                .lock()
                .await
                .insert(question_id.clone(), sender)
                .is_some()
            {
                return Err(HarnessError::invalid(format!(
                    "duplicate cloud question {question_id:?}"
                )));
            }
            if let Err(error) = self
                .protocol
                .emit(&ChildToParentFrame::Question {
                    question: Box::new(question),
                })
                .await
            {
                self.pending.lock().await.remove(&question_id);
                return Err(error);
            }
            receiver.await.map_err(|_| {
                HarnessError::execution("cloud question input closed before an answer")
            })?
        })
    }
}

impl ModelGatewayProvider for PipeModelGateway {
    fn complete<'a>(
        &'a self,
        _: CallContext<()>,
        binding: ternilo_protocol::RunModelBinding,
        request: ModelRequest,
        output: Arc<dyn ModelOutput>,
        cancellation: RunCancellation,
    ) -> Pin<Box<dyn Future<Output = Result<ModelResponse, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            let request_id = self
                .next_request_id
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                    current.checked_add(1)
                })
                .map_err(|_| {
                    HarnessError::execution("cloud model request id space is exhausted")
                })?;
            let (sender, mut receiver) = mpsc::channel(128);
            self.pending.lock().await.insert(request_id, sender);
            if let Err(error) = self
                .protocol
                .emit(&ChildToParentFrame::ModelRequest {
                    request_id,
                    binding,
                    request: Box::new(request),
                })
                .await
            {
                self.pending.lock().await.remove(&request_id);
                return Err(error);
            }
            loop {
                let frame = tokio::select! {
                    biased;
                    () = cancellation.cancelled() => {
                        self.pending.lock().await.remove(&request_id);
                        return Err(HarnessError::cancelled("cloud model request was cancelled"));
                    }
                    frame = receiver.recv() => frame,
                };
                match frame {
                    Some(ParentToChildFrame::ModelDelta { delta, .. }) => {
                        if let Err(error) = output.emit(delta).await {
                            self.pending.lock().await.remove(&request_id);
                            return Err(error);
                        }
                    }
                    Some(ParentToChildFrame::ModelReasoningDelta { delta, .. }) => {
                        if let Err(error) = output.emit_reasoning(delta).await {
                            self.pending.lock().await.remove(&request_id);
                            return Err(error);
                        }
                    }
                    Some(ParentToChildFrame::ModelRetryScheduled {
                        retry,
                        max_retries,
                        delay_ms,
                        failure,
                        ..
                    }) => {
                        if let Err(error) = output
                            .retry_scheduled(retry, max_retries, delay_ms, failure)
                            .await
                        {
                            self.pending.lock().await.remove(&request_id);
                            return Err(error);
                        }
                    }
                    Some(ParentToChildFrame::ModelRetryStarted { retry, .. }) => {
                        if let Err(error) = output.retry_started(retry).await {
                            self.pending.lock().await.remove(&request_id);
                            return Err(error);
                        }
                    }
                    Some(ParentToChildFrame::ModelRetryCancelled { retry, .. }) => {
                        if let Err(error) = output.retry_cancelled(retry).await {
                            self.pending.lock().await.remove(&request_id);
                            return Err(error);
                        }
                    }
                    Some(ParentToChildFrame::ModelComplete { response, .. }) => {
                        self.pending.lock().await.remove(&request_id);
                        return Ok(response);
                    }
                    Some(ParentToChildFrame::ModelError { error, .. }) => {
                        self.pending.lock().await.remove(&request_id);
                        return Err(error);
                    }
                    Some(
                        ParentToChildFrame::QuestionAnswer { .. }
                        | ParentToChildFrame::QuestionError { .. }
                        | ParentToChildFrame::SessionCommand { .. }
                        | ParentToChildFrame::HostReply { .. },
                    ) => {}
                    None => {
                        self.pending.lock().await.remove(&request_id);
                        return Err(HarnessError::execution(
                            "cloud model gateway input closed before completion",
                        ));
                    }
                }
            }
        })
    }
}

async fn route_worker_input(
    pending: PendingModelRequests,
    pending_questions: PendingQuestionRequests,
    host_client: PipeHostClient,
    session_commands: mpsc::Sender<ChildSessionCommandRequest>,
) {
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    loop {
        let frame = match lines.next_line().await {
            Ok(Some(line)) => match serde_json::from_str::<ParentToChildFrame>(&line) {
                Ok(frame) => frame,
                Err(error) => {
                    eprintln!("cloud parent emitted invalid model input: {error}");
                    break;
                }
            },
            Ok(None) => break,
            Err(error) => {
                eprintln!("read cloud parent model input: {error}");
                break;
            }
        };
        match frame {
            frame @ (ParentToChildFrame::ModelDelta { .. }
            | ParentToChildFrame::ModelReasoningDelta { .. }
            | ParentToChildFrame::ModelRetryScheduled { .. }
            | ParentToChildFrame::ModelRetryStarted { .. }
            | ParentToChildFrame::ModelRetryCancelled { .. }
            | ParentToChildFrame::ModelComplete { .. }
            | ParentToChildFrame::ModelError { .. }) => {
                let request_id = match &frame {
                    ParentToChildFrame::ModelDelta { request_id, .. }
                    | ParentToChildFrame::ModelReasoningDelta { request_id, .. }
                    | ParentToChildFrame::ModelRetryScheduled { request_id, .. }
                    | ParentToChildFrame::ModelRetryStarted { request_id, .. }
                    | ParentToChildFrame::ModelRetryCancelled { request_id, .. }
                    | ParentToChildFrame::ModelComplete { request_id, .. }
                    | ParentToChildFrame::ModelError { request_id, .. } => *request_id,
                    _ => unreachable!(),
                };
                let sender = pending.lock().await.get(&request_id).cloned();
                if let Some(sender) = sender
                    && sender.send(frame).await.is_err()
                {
                    pending.lock().await.remove(&request_id);
                }
            }
            ParentToChildFrame::QuestionAnswer { answer } => {
                if let Some(sender) = pending_questions.lock().await.remove(&answer.question_id) {
                    let _ = sender.send(Ok(answer));
                }
            }
            ParentToChildFrame::QuestionError { question_id, error } => {
                if let Some(sender) = pending_questions.lock().await.remove(&question_id) {
                    let _ = sender.send(Err(error));
                }
            }
            ParentToChildFrame::SessionCommand {
                command_id,
                command,
            } => {
                if session_commands.send((command_id, command)).await.is_err() {
                    break;
                }
            }
            ParentToChildFrame::HostReply {
                request_id,
                outcome,
            } => {
                host_client.receive_reply(request_id, outcome).await;
            }
        }
    }
    host_client.input_closed.store(true, Ordering::Release);
    pending.lock().await.clear();
    pending_questions.lock().await.clear();
    host_client.pending.lock().await.clear();
}

async fn serve_child_session_commands(
    mut commands: mpsc::Receiver<ChildSessionCommandRequest>,
    harness: Arc<HarnessSession>,
    protocol: Arc<ChildProtocol>,
    session_id: ternilo_protocol::SessionId,
    denied_tools: BTreeSet<String>,
) {
    let mut starts = tokio::task::JoinSet::new();
    let mut pending_starts =
        BTreeMap::<ternilo_transport::CommandId, (String, RunCancellation)>::new();
    loop {
        tokio::select! {
            request = commands.recv() => {
                let Some((command_id, command)) = request else { break };
                if let ChildSessionCommand::StartService { service_id } = command {
                    let harness = Arc::clone(&harness);
                    let protocol = Arc::clone(&protocol);
                    let cancellation = RunCancellation::new();
                    pending_starts.insert(command_id.clone(), (service_id.clone(), cancellation.clone()));
                    starts.spawn(async move {
                        let outcome = match start_child_service(&harness, service_id, cancellation).await {
                            Ok(service) => ChildSessionCommandOutcome::Service { service },
                            Err(error) => ChildSessionCommandOutcome::Error { error },
                        };
                        let result = protocol.emit(&ChildToParentFrame::SessionCommandReply { command_id: command_id.clone(), outcome }).await;
                        (command_id, result)
                    });
                } else {
                    if let ChildSessionCommand::StopService { service_id } = &command {
                        for (pending_service, cancellation) in pending_starts.values() {
                            if pending_service == service_id { cancellation.cancel(); }
                        }
                    }
                    let outcome = apply_child_session_command(&harness, &session_id, &denied_tools, command).await;
                    if protocol.emit(&ChildToParentFrame::SessionCommandReply { command_id, outcome }).await.is_err() { break; }
                }
            }
            result = starts.join_next(), if !starts.is_empty() => {
                match result {
                    Some(Ok((command_id, result))) => {
                        pending_starts.remove(&command_id);
                        if result.is_err() { break; }
                    }
                    _ => break,
                }
            }
        }
    }
    for (_, cancellation) in pending_starts.values() {
        cancellation.cancel();
    }
    starts.abort_all();
    while starts.join_next().await.is_some() {}
}

async fn start_child_service(
    harness: &HarnessSession,
    service_id: String,
    cancellation: RunCancellation,
) -> Result<ternilo_protocol::SessionServiceSnapshot, HarnessError> {
    cancellation.check()?;
    let starting = harness.start_service(service_id.clone(), cancellation.clone());
    tokio::pin!(starting);
    tokio::select! {
        result = &mut starting => result,
        () = tokio::time::sleep(Duration::from_secs(20)) => {
            cancellation.cancel();
            let (_, stopped) = tokio::join!(starting, harness.stop_service(service_id));
            stopped?;
            Err(HarnessError::unavailable("runtime service startup exceeded the 20-second cloud control limit and was stopped"))
        }
    }
}

async fn apply_child_session_command(
    harness: &HarnessSession,
    session_id: &ternilo_protocol::SessionId,
    denied_tools: &BTreeSet<String>,
    command: ChildSessionCommand,
) -> ChildSessionCommandOutcome {
    match command {
        ChildSessionCommand::Services => ChildSessionCommandOutcome::Services {
            services: harness.service_catalog().await,
        },
        ChildSessionCommand::StartService { service_id } => {
            match start_child_service(harness, service_id, RunCancellation::new()).await {
                Ok(service) => ChildSessionCommandOutcome::Service { service },
                Err(error) => ChildSessionCommandOutcome::Error { error },
            }
        }
        ChildSessionCommand::StopService { service_id } => {
            match harness.stop_service(service_id).await {
                Ok(service) => ChildSessionCommandOutcome::Service { service },
                Err(error) => ChildSessionCommandOutcome::Error { error },
            }
        }
        ChildSessionCommand::Steer { input } => match harness.steer(*input).await {
            Ok(accepted) => ChildSessionCommandOutcome::Steer { accepted },
            Err(error) => ChildSessionCommandOutcome::Error { error },
        },
        ChildSessionCommand::Cancel { run_id } => match harness.cancel(run_id).await {
            Ok(()) => ChildSessionCommandOutcome::Cancelled,
            Err(error) => ChildSessionCommandOutcome::Error { error },
        },
        ChildSessionCommand::Commands => match harness.command_catalog().await {
            Ok(commands) => ChildSessionCommandOutcome::Commands {
                catalog: ternilo_protocol::SessionCommandCatalog {
                    session_id: session_id.clone(),
                    commands: commands
                        .into_iter()
                        .filter(|command| !denied_tools.contains(&command.tool_name))
                        .map(|command| command.descriptor)
                        .collect(),
                },
            },
            Err(error) => ChildSessionCommandOutcome::Error { error },
        },
        ChildSessionCommand::Skills => match harness.skill_catalog().await {
            Ok(catalog) => ChildSessionCommandOutcome::Skills { catalog },
            Err(error) => ChildSessionCommandOutcome::Error { error },
        },
        ChildSessionCommand::ResolveSkill { name, input } => {
            let result = match harness.skill(name.clone()).await {
                Ok(Some(skill)) => ternilo_builtins::prepare_skill_invocation(&skill, &input),
                Ok(None) => Err(HarnessError::invalid(format!("unknown skill {name:?}"))),
                Err(error) => Err(error),
            };
            match result {
                Ok(invocation) => ChildSessionCommandOutcome::SkillResolved { invocation },
                Err(error) => ChildSessionCommandOutcome::Error { error },
            }
        }
    }
}

struct PipeEventStore {
    prior_events: Vec<SessionEvent>,
    protocol: Arc<ChildProtocol>,
}

impl PipeEventStore {
    fn new(prior_events: Vec<SessionEvent>, protocol: Arc<ChildProtocol>) -> Self {
        Self {
            prior_events,
            protocol,
        }
    }
}

impl SessionEventStore for PipeEventStore {
    fn load<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<SessionEvent>, HarnessError>> + Send + 'a>> {
        Box::pin(async move { Ok(self.prior_events.clone()) })
    }

    fn append<'a>(
        &'a self,
        event: SessionEvent,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            self.protocol
                .emit(&ChildToParentFrame::Event {
                    event: Box::new(event),
                })
                .await
        })
    }
}

fn terminal_event(kind: &SessionEventKind) -> bool {
    matches!(
        kind,
        SessionEventKind::TurnFinished { .. }
            | SessionEventKind::TurnFailed { .. }
            | SessionEventKind::TurnCancelled
    )
}

fn cancellation_requested(
    durable_cancel: &std::sync::atomic::AtomicBool,
    observed_by_renewal: bool,
) -> bool {
    observed_by_renewal || durable_cancel.load(std::sync::atomic::Ordering::Acquire)
}

fn validate_prior_events(events: &[SessionEvent]) -> Result<(), HarnessError> {
    for (expected, event) in events.iter().enumerate() {
        let expected = u64::try_from(expected)
            .map_err(|_| HarnessError::execution("cloud session sequence exceeds u64"))?;
        if event.seq != expected {
            return Err(HarnessError::policy(format!(
                "cloud session history is discontinuous at sequence {expected}"
            )));
        }
    }
    Ok(())
}

fn hex_bytes(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut value = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut value, "{byte:02x}").expect("writing to a String cannot fail");
    }
    value
}

fn private_temporary_directory(prefix: &str) -> std::io::Result<tempfile::TempDir> {
    let mut builder = tempfile::Builder::new();
    builder.prefix(prefix);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        builder.permissions(std::fs::Permissions::from_mode(0o700));
    }
    builder.tempdir()
}

fn write_json(path: &Path, value: &impl serde::Serialize) -> Result<(), HarnessError> {
    let bytes = serde_json::to_vec(value)
        .map_err(|error| HarnessError::execution(format!("encode Worker file: {error}")))?;
    std::fs::write(path, bytes)
        .map_err(|error| HarnessError::execution(format!("write Worker file: {error}")))
}

fn read_json<T>(path: &Path, maximum_bytes: u64) -> Result<T, HarnessError>
where
    T: serde::de::DeserializeOwned,
{
    let metadata = std::fs::metadata(path)
        .map_err(|error| HarnessError::invalid(format!("inspect {}: {error}", path.display())))?;
    if metadata.len() > maximum_bytes {
        return Err(HarnessError::invalid(format!(
            "{} exceeds the worker input limit",
            path.display()
        )));
    }
    let text = std::fs::read_to_string(path)
        .map_err(|error| HarnessError::invalid(format!("read {}: {error}", path.display())))?;
    serde_json::from_str(&text)
        .map_err(|error| HarnessError::invalid(format!("parse {}: {error}", path.display())))
}

fn now_ms() -> Result<u64, HarnessError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| HarnessError::execution(format!("system clock error: {error}")))?
        .as_millis()
        .try_into()
        .map_err(|_| HarnessError::execution("system timestamp exceeds u64 milliseconds"))
}

async fn shutdown_signal() {
    let interrupt = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut signal) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            signal.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = interrupt => {}
        () = terminate => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;
    use ternilo_protocol::{
        RunId, RunLimits, SessionIdentity, SessionSubmission, SteeringInput, SubmissionContent,
        SubmissionDelivery, SubmissionId, SubmissionPlacement, UserId, UserMessageSource,
    };

    #[test]
    fn deployment_uses_server_credentials_and_keeps_child_and_maintenance_commands() {
        let command = Args::command();
        command.clone().debug_assert();
        let serve = command.find_subcommand("serve").unwrap();
        for name in [
            "server_url",
            "token",
            "workspace_root",
            "sandbox",
            "health_listen",
        ] {
            assert!(
                serve
                    .get_arguments()
                    .any(|argument| argument.get_id() == name)
            );
        }
        for name in [
            "database_url",
            "host_database_url",
            "secret_master_key",
            "policy",
        ] {
            assert!(
                !serve
                    .get_arguments()
                    .any(|argument| argument.get_id() == name)
            );
        }
        assert!(command.find_subcommand("execute").is_some());
        assert!(command.find_subcommand("gc-workspace").is_some());
        assert!(command.find_subcommand("init").is_some());
        assert!(
            Args::try_parse_from(["ternilo-worker"])
                .unwrap()
                .command
                .is_none()
        );
    }

    #[test]
    fn bundled_worker_examples_follow_the_current_cloud_and_extension_contracts() {
        let portable_spec: ternilo_protocol::RunSpec =
            serde_json::from_str(include_str!("../../../examples/run-spec.json")).unwrap();
        portable_spec.validate_shape().unwrap();
        assert_eq!(
            portable_spec.schema_version,
            ternilo_protocol::RUN_SPEC_VERSION
        );

        let policy: WorkerPolicy =
            serde_json::from_str(include_str!("../../../examples/worker-policy.json")).unwrap();
        let envelope: ExecutionEnvelope =
            serde_json::from_str(include_str!("../../../examples/execution-envelope.json"))
                .unwrap();
        let catalog = ternilo_cloud::catalog().unwrap();
        policy.validate(&envelope.spec, &catalog).unwrap();
        envelope.workspace.validate().unwrap();
        assert_eq!(
            envelope.workspace.workspace_id,
            envelope.spec.metadata.workspace_id
        );
        assert!(envelope.extensions.is_empty());

        let docker_policy: WorkerPolicy =
            serde_json::from_str(include_str!("../../../deploy/docker/worker-policy.json"))
                .unwrap();
        docker_policy.validate_operational_limits().unwrap();
        assert!(docker_policy.max_extension_packages_per_run > 0);
        assert!(
            docker_policy
                .allowed_plugin_kinds
                .contains(ternilo_extension::EXTENSION_PACKAGE_KIND)
        );
        assert!(
            docker_policy
                .extension_host_policy
                .allowed_capabilities
                .contains(&ternilo_extension::Capability::WorkspaceRead)
        );
        let rhai_manifest: ternilo_extension::ExtensionManifest = serde_json::from_str(
            include_str!("../../../examples/rhai-echo-extension/manifest.json"),
        )
        .unwrap();
        docker_policy
            .extension_host_policy
            .validate_install(&rhai_manifest, &rhai_manifest.requested_capabilities)
            .unwrap();
        assert_eq!(
            rhai_manifest.schema_version,
            ternilo_extension::EXTENSION_PACKAGE_SCHEMA_VERSION
        );
        assert_eq!(rhai_manifest.contributions.prompt_sections.len(), 1);
        assert_eq!(rhai_manifest.contributions.skills.len(), 1);
        assert_eq!(
            rhai_manifest.contributions.skills[0].name,
            "rhai-extension-tools"
        );

        let wasm_manifest: ternilo_extension::ExtensionManifest = serde_json::from_str(
            include_str!("../../../examples/wasm-echo-plugin/manifest.json"),
        )
        .unwrap();
        docker_policy
            .extension_host_policy
            .validate_install(&wasm_manifest, &wasm_manifest.requested_capabilities)
            .unwrap();
        assert_eq!(wasm_manifest.contributions.prompt_sections.len(), 1);
        assert_eq!(wasm_manifest.contributions.skills.len(), 1);
        assert_eq!(
            wasm_manifest.contributions.skills[0].name,
            "wasm-extension-tools"
        );
    }

    #[test]
    fn prior_event_history_must_be_contiguous() {
        let event = SessionEvent {
            seq: 1,
            occurred_at_ms: 0,
            run_id: RunId::new("run"),
            kind: SessionEventKind::TurnStarted,
        };
        assert!(validate_prior_events(&[event]).is_err());
    }

    #[test]
    fn fast_cancel_wins_terminal_classification_before_lease_renewal() {
        let durable_cancel = std::sync::atomic::AtomicBool::new(true);
        assert!(cancellation_requested(&durable_cancel, false));
    }

    #[tokio::test]
    async fn daemon_shutdown_stops_command_and_run_tasks_before_drain() {
        let (shutdown, mut command_shutdown) = tokio::sync::watch::channel(false);
        let observed_shutdown = command_shutdown.clone();
        let mut command_task = tokio::spawn(async move {
            let _ = command_shutdown.changed().await;
            std::future::pending::<Result<(), HarnessError>>().await
        });
        let mut telemetry_shutdown = shutdown.subscribe();
        let mut telemetry_task = tokio::spawn(async move {
            let _ = telemetry_shutdown.changed().await;
            std::future::pending::<Result<(), HarnessError>>().await
        });
        let mut run_tasks = tokio::task::JoinSet::new();
        run_tasks.spawn(async {
            tokio::time::sleep(Duration::from_millis(1)).await;
            Ok(())
        });
        tokio::task::yield_now().await;

        stop_daemon_tasks(
            &shutdown,
            &mut command_task,
            &mut telemetry_task,
            &mut run_tasks,
            &ActiveRuns::default(),
        )
        .await;

        assert!(*observed_shutdown.borrow());
        assert!(command_task.is_finished());
        assert!(telemetry_task.is_finished());
        assert!(run_tasks.is_empty());
    }

    #[test]
    fn typed_submission_metadata_is_optional_and_preserves_skill_display() {
        assert_eq!(submission_message_metadata(None), (None, None, None));
        let submission_id = SubmissionId::new("skill-submission");
        let expected = ternilo_protocol::InputProvenance {
            run_id: None,
            input_id: submission_id.clone(),
            author: ternilo_protocol::InputAuthor::Account {
                user_id: UserId::new("skill-user"),
                username: "skill-user".to_owned(),
            },
        };

        let (display, source, provenance) = submission_message_metadata(Some(SessionSubmission {
            provenance: Some(expected.clone()),
            id: submission_id.clone(),
            run_id: RunId::new("skill-run"),
            content: SubmissionContent::Skill {
                name: "release-check".to_owned(),
                input: "inspect the release".to_owned(),
            },
            references: Vec::new(),
            attachments: Vec::new(),
            placement: SubmissionPlacement::Running,
            created_at_ms: 1,
            updated_at_ms: 1,
        }));
        assert_eq!(provenance, Some(expected));
        assert_eq!(
            display.as_deref(),
            Some("/skill release-check\n\ninspect the release")
        );
        assert!(matches!(
            source,
            Some(UserMessageSource::Submission {
                regenerate_from: None,
                submission_id: id,
                created_at_ms: 1,
                delivery: SubmissionDelivery::Queue,
                skill_name: Some(name),
            }) if id == submission_id && name == "release-check"
        ));
    }

    #[tokio::test]
    async fn child_command_reports_false_when_the_steering_window_is_closed() {
        let workspace = tempfile::tempdir().unwrap();
        let workspace_id = WorkspaceId::new("closed-window-workspace");
        let harness = HarnessSession::boot(
            &ternilo_cloud::catalog().unwrap(),
            &ternilo_cloud::cloud_profile(Some(&ternilo_protocol::RunModelSnapshot {
                binding: ternilo_protocol::RunModelBinding::UserProvider {
                    tenant_id: TenantId::new("closed-window-tenant"),
                    owner_user_id: UserId::new("closed-window-user"),
                    provider_id: "primary".to_owned(),
                    model: "replace-with-provider-model-id".to_owned(),
                },
                protocol: ternilo_protocol::ProviderProtocol::OpenAiChatCompletions,
                defaults: ternilo_protocol::ProviderModelDefaults {
                    context_window: 128_000,
                    max_output_tokens: 8_192,
                    reasoning: None,
                },
                reasoning_effort: None,
                display_name: "Test model".to_owned(),
                source_name: "Test provider".to_owned(),
            })),
            HostEnvironment::memory(
                SessionIdentity {
                    tenant_id: TenantId::new("closed-window-tenant"),
                    user_id: UserId::new("closed-window-user"),
                    agent_id: ternilo_protocol::AgentId::new("closed-window-agent"),
                    session_id: ternilo_protocol::SessionId::new("closed-window-session"),
                },
                Some(WorkspaceBinding {
                    workspace_id,
                    path: workspace.path().to_string_lossy().into_owned(),
                }),
                ternilo_kernel::HostPolicy::local(RunLimits {
                    max_steps: 2,
                    max_tool_calls: 2,
                }),
            ),
        )
        .await
        .unwrap();
        let submission_id = SubmissionId::new("closed-window-submission");
        assert_eq!(
            apply_child_session_command(
                &harness,
                &ternilo_protocol::SessionId::new("closed-window-session"),
                &BTreeSet::new(),
                ChildSessionCommand::Steer {
                    input: Box::new(SteeringInput {
                        provenance: None,
                        submission_id: submission_id.clone(),
                        input: "too late".to_owned(),
                        display_input: None,
                        source: UserMessageSource::Submission {
                            regenerate_from: None,
                            submission_id,
                            created_at_ms: 1,
                            delivery: SubmissionDelivery::Steer,
                            skill_name: None,
                        },
                        references: Vec::new(),
                        reference_contexts: Vec::new(),
                        attachments: Vec::new(),
                    }),
                },
            )
            .await,
            ChildSessionCommandOutcome::Steer { accepted: false },
        );
        harness.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn downloaded_attachment_objects_restore_canonical_references_and_reject_corruption() {
        use base64::{Engine as _, engine::general_purpose::STANDARD};

        let workspace = tempfile::tempdir().unwrap();
        let attachments = LocalAttachments::open(&workspace.path().join(".ternilo"))
            .await
            .unwrap();
        for (name, media_type, payload) in [
            (
                "restored.txt",
                "text/plain",
                "\u{feff}data:text/plain;base64,literal\r\n文件 📄\r\n"
                    .as_bytes()
                    .to_vec(),
            ),
            (
                "retained.bin",
                "application/octet-stream",
                vec![0xff; 9 * 1024 * 1024],
            ),
        ] {
            let reference = Attachment {
                name: name.to_owned(),
                media_type: media_type.to_owned(),
                content: format!(
                    "{}{}",
                    ternilo_protocol::ATTACHMENT_REFERENCE_PREFIX,
                    hex_bytes(&Sha256::digest(&payload))
                ),
            };
            let object = ExecutionAttachmentObject {
                attachment: reference.clone(),
                content_base64: STANDARD.encode(&payload),
            };
            restore_attachment_objects(&attachments, vec![object.clone()])
                .await
                .unwrap();
            assert_eq!(
                attachments.reference_bytes(&reference).await.unwrap(),
                payload
            );
            restore_attachment_objects(&attachments, vec![object.clone()])
                .await
                .unwrap();
            let mut corrupt = object;
            corrupt.content_base64 = STANDARD.encode(b"different bytes");
            assert!(
                restore_attachment_objects(&attachments, vec![corrupt])
                    .await
                    .is_err()
            );
            assert_eq!(
                attachments.reference_bytes(&reference).await.unwrap(),
                payload
            );
        }
    }

    #[test]
    fn event_attachment_references_covers_inputs_retained_outputs_and_deliverables() {
        let reference = Attachment {
            name: "artifact.txt".to_owned(),
            media_type: "text/plain".to_owned(),
            content: format!(
                "{}{}",
                ternilo_protocol::ATTACHMENT_REFERENCE_PREFIX,
                "a".repeat(64)
            ),
        };
        let inline = Attachment {
            content: "inline".to_owned(),
            ..reference.clone()
        };
        assert_eq!(
            event_attachment_references(&SessionEventKind::UserMessage {
                provenance: None,
                content: "input".to_owned(),
                display_content: None,
                source: None,
                references: Vec::new(),
                attachments: vec![inline, reference.clone()],
            }),
            vec![&reference]
        );
        assert_eq!(
            event_attachment_references(&SessionEventKind::DeliverableProduced {
                path: "artifact.txt".to_owned(),
                operation: "write".to_owned(),
                attachment: reference.clone(),
            }),
            vec![&reference]
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn cloud_child_mounts_only_the_resolved_workspace() {
        let executable = Path::new("/opt/ternilo/worker");
        let policy = Path::new("/run/ternilo/policy.json");
        let envelope = Path::new("/run/ternilo/envelope.json");
        let workspace = Path::new("/storage/tenant-a/workspace-a");

        for mode in [SandboxMode::Bubblewrap, SandboxMode::Container] {
            let command = child_command(mode, executable, policy, envelope, workspace);
            let args = command
                .as_std()
                .get_args()
                .map(|argument| argument.to_string_lossy().into_owned())
                .collect::<Vec<_>>();
            let writable_binds = args
                .windows(3)
                .filter(|window| window[0] == "--bind" && window[2] == "/workspace")
                .collect::<Vec<_>>();

            assert_eq!(writable_binds.len(), 1);
            assert_eq!(writable_binds[0][1], workspace.to_string_lossy());
            assert_eq!(writable_binds[0][2], "/workspace");
            assert!(args.windows(3).any(|window| {
                window[0] == "--chmod" && window[1] == "1777" && window[2] == "/tmp"
            }));
            assert!(
                args.windows(2)
                    .any(|window| { window[0] == "--info-fd" && window[1] == "3" })
            );
            assert!(
                args.windows(2)
                    .any(|window| { window[0] == "--block-fd" && window[1] == "4" })
            );
            assert!(!args.iter().any(|argument| argument == "/storage"));
            assert!(!args.iter().any(|argument| argument == "/storage/tenant-a"));
        }
    }
}
