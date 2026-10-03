use std::{
    collections::BTreeMap,
    future::Future,
    path::PathBuf,
    pin::Pin,
    sync::{Arc, Mutex},
    time::Duration,
};

use crate::process_supervision::{Child, ManagedProcessGroup};
use agent_client_protocol::{
    Agent as AcpAgentRole, ByteStreams, Client as AcpClient, ConnectionTo,
    schema::{ProtocolVersion, v1 as acp},
};
use linorun_core::{Activation, CleanupError, ComponentContext, ComponentDescriptor, effect};
use linorun_macros::component_descriptor;
use serde::Deserialize;
use ternilo_kernel::{
    ExecutionResourceControl, HarnessPlugin, PluginFactory, PluginManifest, RunCancellation,
    RunEnvironment, RunEnvironmentClient, SubagentAdmission, SubagentBackend,
    SubagentBackendContext, SubagentBackendRegistration, SubagentDriver, SubagentRunStart,
    SubagentSessionBinding, Subagents, WorkspaceExecutionLease,
};
use ternilo_protocol::{HarnessError, RunId, SubagentTranscriptKind};
use tokio::{process::Command, sync::watch};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use crate::{factory as make_factory, parse_config, process_group::OwnedProcessGroup};

pub const KIND: &str = "ternilo.subagents.acp";
const MAX_OUTPUT_CHARS: usize = 1_048_576;

component_descriptor! {
    static DESCRIPTOR: () {
        id: "ternilo/builtin-acp-subagent@1",
        requires: [Subagents, RunEnvironment],
        provides: [],
    }
}

#[derive(Clone, Copy, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
enum PermissionPolicy {
    Allow,
    Reject,
}

#[derive(Clone, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct AcpSubagentConfig {
    #[serde(default = "default_provider_name")]
    provider_name: String,
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    /// Map child environment variable names to credentials on the execution host.
    #[serde(default)]
    env_refs: BTreeMap<String, String>,
    /// Explicit authentication method advertised by the external agent. Empty uses its existing login.
    #[serde(default)]
    auth_method: Option<String>,
    /// Explicit mode advertised by the new session. Empty keeps the agent default.
    #[serde(default)]
    session_mode: Option<String>,
    #[serde(default)]
    cwd: Option<PathBuf>,
    #[serde(default = "default_permission")]
    permission: PermissionPolicy,
    #[serde(default = "default_timeout_ms")]
    timeout_ms: u64,
    #[serde(default = "default_shutdown_grace_ms")]
    shutdown_grace_ms: u64,
}

fn default_provider_name() -> String {
    "acp".to_owned()
}

const fn default_permission() -> PermissionPolicy {
    PermissionPolicy::Reject
}

const fn default_timeout_ms() -> u64 {
    600_000
}

const fn default_shutdown_grace_ms() -> u64 {
    3_000
}

pub fn factory() -> PluginFactory {
    make_factory(
        PluginManifest {
            kind: KIND,
            requires: &["ternilo/subagents@2", "ternilo/run-environment@1"],
            provides: &[],
        },
        |value| {
            let mut config: AcpSubagentConfig = parse_config(value)?;
            let provider_name = config.provider_name.trim().to_owned();
            let command = config.command.trim().to_owned();
            config.provider_name = provider_name;
            config.command = command;
            if config.provider_name.is_empty()
                || config.command.is_empty()
                || config.timeout_ms == 0
                || config.shutdown_grace_ms == 0
            {
                return Err(HarnessError::composition(
                    "ACP subagent requires provider_name, command, timeout_ms, and shutdown_grace_ms",
                ));
            }
            if let Some(cwd) = config.cwd.take() {
                config.cwd = Some(std::fs::canonicalize(&cwd).map_err(|error| {
                    HarnessError::composition(format!(
                        "resolve ACP subagent cwd {}: {error}",
                        cwd.display()
                    ))
                })?);
            }
            validate_environment(&config.env)?;
            validate_environment(&config.env_refs)?;
            if config.env_refs.iter().any(|(name, reference)| reference.trim().is_empty() || config.env.contains_key(name)) {
                return Err(HarnessError::composition("ACP env_refs requires nonempty credential references and must not overlap env"));
            }
            if config.auth_method.iter().chain(config.session_mode.iter()).any(|value| value.trim().is_empty()) {
                return Err(HarnessError::composition("ACP auth_method and session_mode must be nonempty when set"));
            }
            Ok(Arc::new(AcpSubagentPlugin { config }))
        },
    )
    .with_config_schema::<AcpSubagentConfig>()
}

struct AcpSubagentPlugin {
    config: AcpSubagentConfig,
}

impl HarnessPlugin for AcpSubagentPlugin {
    fn descriptor(&self) -> &'static ComponentDescriptor {
        &DESCRIPTOR
    }

    fn activate(&self, context: ComponentContext) -> Activation {
        let subagents = context
            .context()
            .service::<Subagents>()
            .expect("ACP subagent declares Subagents");
        let environment = context
            .context()
            .service::<RunEnvironment>()
            .expect("ACP subagent declares RunEnvironment");
        let config = self.config.clone();
        Activation::Once(Box::pin(async move {
            let name = config.provider_name.clone();
            let backend: Arc<dyn SubagentBackend> = Arc::new(AcpBackend {
                config,
                environment,
            });
            let registration = subagents
                .register_backend(SubagentBackendRegistration { name, backend })
                .await
                .map_err(|error| linorun_core::ActivationFailure::user(error.to_string()))?;
            Ok(Some(effect::inverse(move || async move {
                subagents
                    .unregister_backend(registration)
                    .await
                    .map_err(|error| CleanupError::user(error.to_string()))
            })))
        }))
    }
}

struct AcpBackend {
    config: AcpSubagentConfig,
    environment: RunEnvironmentClient,
}

impl SubagentBackend for AcpBackend {
    fn create(
        &self,
        context: SubagentBackendContext,
    ) -> Result<Arc<dyn SubagentDriver>, HarnessError> {
        let cwd = if let Some(cwd) = &self.config.cwd {
            cwd.clone()
        } else {
            PathBuf::from(
                context
                    .workspace
                    .as_ref()
                    .ok_or_else(|| {
                        HarnessError::invalid(
                            "ACP subagent requires a configured cwd or parent workspace",
                        )
                    })?
                    .path
                    .clone(),
            )
        };
        if !cwd.is_absolute() || !cwd.is_dir() {
            return Err(HarnessError::invalid(format!(
                "ACP subagent cwd is not an accessible absolute directory: {}",
                cwd.display()
            )));
        }
        Ok(Arc::new(AcpDriver {
            config: self.config.clone(),
            environment: self.environment.clone(),
            resource_id: format!("acp-{}", context.subagent_id),
            cwd,
        }))
    }
}

struct AcpDriver {
    config: AcpSubagentConfig,
    environment: RunEnvironmentClient,
    resource_id: String,
    cwd: PathBuf,
}

impl SubagentDriver for AcpDriver {
    fn supports_followup(&self) -> bool {
        false
    }

    fn transcript_kind(&self) -> SubagentTranscriptKind {
        SubagentTranscriptKind::ProcessLifecycle
    }

    fn run<'a>(
        &'a self,
        parent_run_id: RunId,
        message: String,
        cancellation: RunCancellation,
        _: Option<SubagentSessionBinding>,
        start: SubagentRunStart,
    ) -> Pin<Box<dyn Future<Output = Result<String, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            let _stop = CancelOnDrop(cancellation.clone());
            self.environment
                .check_run_authorization(parent_run_id.clone())
                .await?;
            let mut config = self.config.clone();
            for (name, reference) in &self.config.env_refs {
                let value = tokio::select! {
                    biased;
                    () = cancellation.cancelled() => return Err(HarnessError::cancelled("ACP credential resolution cancelled")),
                    value = self.environment.resolve_secret(reference.clone()) => value?,
                }.filter(|value| !value.trim().is_empty()).ok_or_else(|| HarnessError::execution(format!("ACP credential {reference:?} is not configured")))?;
                if value.contains('\0') {
                    return Err(HarnessError::invalid(
                        "ACP credential cannot contain a null byte",
                    ));
                }
                config.env.insert(name.clone(), value);
            }
            let lease = self
                .environment
                .acquire_workspace(cancellation.clone())
                .await?;
            self.environment
                .check_run_authorization(parent_run_id.clone())
                .await?;
            let lease = Arc::new(Mutex::new(Some(lease)));
            let (completed, finished) = watch::channel(None);
            let mut admission = AcpAdmission(Some(completed.clone()));
            self.environment
                .register_execution_resource(
                    parent_run_id,
                    format!(
                        "{}:{}",
                        self.environment.identity().await.session_id,
                        self.resource_id
                    ),
                    Arc::new(AcpResource {
                        cancellation: cancellation.clone(),
                        finished,
                        _lease: Arc::clone(&lease),
                    }),
                )
                .await?;
            let cwd = self.cwd.clone();
            let task = tokio::spawn(async move {
                let (outcome, cleanup) = run_acp(config, cwd, message, cancellation).await;
                if cleanup.is_ok() {
                    lease.lock().expect("ACP lease lock").take();
                }
                completed.send_replace(Some(cleanup.clone().map_err(|error| error.to_string())));
                cleanup.and(outcome)
            });
            admission.0 = None;
            start.resolve(Ok(SubagentAdmission::Direct));
            task.await.map_err(|error| {
                HarnessError::execution(format!("supervise ACP process: {error}"))
            })?
        })
    }
}

struct CancelOnDrop(RunCancellation);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}
struct AcpAdmission(Option<watch::Sender<Option<Result<(), String>>>>);
impl Drop for AcpAdmission {
    fn drop(&mut self) {
        if let Some(completed) = self.0.take() {
            completed.send_replace(Some(Ok(())));
        }
    }
}
struct AcpResource {
    cancellation: RunCancellation,
    finished: watch::Receiver<Option<Result<(), String>>>,
    _lease: Arc<Mutex<Option<WorkspaceExecutionLease>>>,
}
impl ExecutionResourceControl for AcpResource {
    fn stop<'a>(&'a self) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            self.cancellation.cancel();
            let mut finished = self.finished.clone();
            loop {
                if let Some(result) = finished.borrow().clone() {
                    return result.map_err(HarnessError::execution);
                }
                finished.changed().await.map_err(|_| {
                    HarnessError::unavailable(
                        "ACP supervisor ended before recording process completion",
                    )
                })?;
            }
        })
    }
    fn is_finished(&self) -> bool {
        matches!(&*self.finished.borrow(), Some(Ok(())))
    }
}

enum Settlement {
    Protocol(Result<(acp::StopReason, String), agent_client_protocol::Error>),
    Cancelled,
    TimedOut,
}

type CancelTarget = tokio::sync::Mutex<Option<(ConnectionTo<AcpAgentRole>, acp::SessionId)>>;

async fn run_acp(
    config: AcpSubagentConfig,
    cwd: PathBuf,
    message: String,
    cancellation: RunCancellation,
) -> (Result<String, HarnessError>, Result<(), HarnessError>) {
    if let Err(error) = cancellation.check() {
        return (Err(error), Ok(()));
    }
    let (stdin, stdout, child, group) = match spawn_child(&config, &cwd) {
        Ok(child) => child,
        Err(error) => return (Err(error), Ok(())),
    };
    let mut process = ChildGuard::new(child, group);
    let transport = ByteStreams::new(stdin.compat_write(), stdout.compat());
    let output = Arc::new(Mutex::new(String::new()));
    let notification_output = Arc::clone(&output);
    let permission = config.permission;
    let auth_method = config.auth_method.clone();
    let session_mode = config.session_mode.clone();
    let cancel_target = Arc::new(CancelTarget::new(None));
    let connection_target = Arc::clone(&cancel_target);
    let protocol = AcpClient
        .builder()
        .on_receive_notification(
            async move |notification: acp::SessionNotification, _connection| {
                if let acp::SessionUpdate::AgentMessageChunk(chunk) = notification.update
                    && let acp::ContentBlock::Text(text) = chunk.content
                    && let Ok(mut output) = notification_output.lock()
                {
                    push_bounded(&mut output, &text.text);
                }
                Ok(())
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .on_receive_request(
            async move |request: acp::RequestPermissionRequest, responder, _connection| {
                let outcome = match permission {
                    PermissionPolicy::Reject => acp::RequestPermissionOutcome::Cancelled,
                    PermissionPolicy::Allow => request
                        .options
                        .iter()
                        .find(|option| {
                            matches!(
                                option.kind,
                                acp::PermissionOptionKind::AllowOnce
                                    | acp::PermissionOptionKind::AllowAlways
                            )
                        })
                        .map_or(acp::RequestPermissionOutcome::Cancelled, |option| {
                            acp::RequestPermissionOutcome::Selected(
                                acp::SelectedPermissionOutcome::new(option.option_id.clone()),
                            )
                        }),
                };
                responder.respond(acp::RequestPermissionResponse::new(outcome))
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_with(transport, move |connection: ConnectionTo<AcpAgentRole>| {
            let output = Arc::clone(&output);
            let target = Arc::clone(&connection_target);
            let cwd = cwd.clone();
            let message = message.clone();
            let auth_method = auth_method.clone();
            let session_mode = session_mode.clone();
            async move {
                initialize_agent(&connection, auth_method.as_deref()).await?;
                let session =
                    open_agent_session(&connection, cwd, session_mode.as_deref(), &target).await?;
                let response = connection
                    .send_request(acp::PromptRequest::new(
                        session,
                        vec![acp::ContentBlock::Text(acp::TextContent::new(message))],
                    ))
                    .block_task()
                    .await?;
                let answer = output
                    .lock()
                    .map_or_else(|_| String::new(), |output| output.trim().to_owned());
                Ok((response.stop_reason, answer))
            }
        });

    let settlement =
        await_settlement(protocol, &cancellation, &cancel_target, config.timeout_ms).await;
    let cleanup = process
        .reap(Duration::from_millis(config.shutdown_grace_ms))
        .await;
    (settlement_result(settlement, config.timeout_ms), cleanup)
}

async fn initialize_agent(
    connection: &ConnectionTo<AcpAgentRole>,
    auth_method: Option<&str>,
) -> Result<(), agent_client_protocol::Error> {
    let initialized = connection
        .send_request(acp::InitializeRequest::new(ProtocolVersion::V1))
        .block_task()
        .await?;
    if initialized.protocol_version != ProtocolVersion::V1 {
        return Err(agent_client_protocol::Error::invalid_request()
            .data("external agent did not negotiate ACP v1"));
    }
    if let Some(method) = auth_method {
        if !initialized
            .auth_methods
            .iter()
            .any(|available| available.id().0.as_ref() == method)
        {
            return Err(agent_client_protocol::Error::invalid_params()
                .data("configured ACP authentication method is not advertised by the agent"));
        }
        connection
            .send_request(acp::AuthenticateRequest::new(method.to_owned()))
            .block_task()
            .await?;
    }
    Ok(())
}

async fn open_agent_session(
    connection: &ConnectionTo<AcpAgentRole>,
    cwd: PathBuf,
    session_mode: Option<&str>,
    target: &CancelTarget,
) -> Result<acp::SessionId, agent_client_protocol::Error> {
    let session = connection
        .send_request(acp::NewSessionRequest::new(cwd))
        .block_task()
        .await?;
    *target.lock().await = Some((connection.clone(), session.session_id.clone()));
    if let Some(mode) = session_mode {
        if !session.modes.as_ref().is_some_and(|state| {
            state
                .available_modes
                .iter()
                .any(|available| available.id.0.as_ref() == mode)
        }) {
            return Err(agent_client_protocol::Error::invalid_params()
                .data("configured ACP session mode is not advertised by the agent"));
        }
        connection
            .send_request(acp::SetSessionModeRequest::new(
                session.session_id.clone(),
                mode.to_owned(),
            ))
            .block_task()
            .await?;
    }
    Ok(session.session_id)
}

async fn await_settlement(
    protocol: impl Future<Output = Result<(acp::StopReason, String), agent_client_protocol::Error>>,
    cancellation: &RunCancellation,
    cancel_target: &CancelTarget,
    timeout_ms: u64,
) -> Settlement {
    tokio::pin!(protocol);
    tokio::select! {
        result = &mut protocol => Settlement::Protocol(result),
        () = cancellation.cancelled() => {
            send_cancel(cancel_target).await;
            let _ = tokio::time::timeout(Duration::from_secs(1), &mut protocol).await;
            Settlement::Cancelled
        }
        () = tokio::time::sleep(Duration::from_millis(timeout_ms)) => {
            send_cancel(cancel_target).await;
            let _ = tokio::time::timeout(Duration::from_secs(1), &mut protocol).await;
            Settlement::TimedOut
        }
    }
}

fn settlement_result(settlement: Settlement, timeout_ms: u64) -> Result<String, HarnessError> {
    match settlement {
        Settlement::Cancelled => Err(HarnessError::cancelled("ACP subagent was cancelled")),
        Settlement::TimedOut => Err(HarnessError::execution(format!(
            "ACP subagent timed out after {timeout_ms} ms"
        ))),
        Settlement::Protocol(Err(error)) => Err(HarnessError::execution(format!(
            "ACP subagent protocol failed: {error}"
        ))),
        Settlement::Protocol(Ok((acp::StopReason::EndTurn, answer))) if !answer.is_empty() => {
            Ok(answer)
        }
        Settlement::Protocol(Ok((reason, answer))) => Err(HarnessError::execution(format!(
            "ACP subagent stopped with {reason:?}{}",
            if answer.is_empty() {
                String::new()
            } else {
                format!(": {answer}")
            }
        ))),
    }
}

async fn send_cancel(target: &CancelTarget) {
    if let Some((connection, session)) = target.lock().await.clone() {
        let _ = connection.send_notification(acp::CancelNotification::new(session));
    }
}

fn spawn_child(
    config: &AcpSubagentConfig,
    cwd: &PathBuf,
) -> Result<
    (
        tokio::process::ChildStdin,
        tokio::process::ChildStdout,
        Child,
        ManagedProcessGroup,
    ),
    HarnessError,
> {
    let mut command = Command::new(&config.command);
    command
        .args(&config.args)
        .current_dir(cwd)
        .env_clear()
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .kill_on_drop(true);
    for name in [
        "PATH",
        "HOME",
        "USER",
        "LOGNAME",
        "SHELL",
        "LANG",
        "LC_ALL",
        "TMPDIR",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_CACHE_HOME",
        "SYSTEMROOT",
        "WINDIR",
        "COMSPEC",
        "PATHEXT",
        "TEMP",
        "TMP",
        "USERPROFILE",
        "APPDATA",
        "LOCALAPPDATA",
    ] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command.envs(&config.env);
    let (mut child, group) = crate::process_supervision::spawn(&mut command).map_err(|error| {
        HarnessError::execution(format!("start ACP subagent {:?}: {error}", config.command))
    })?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| HarnessError::execution("ACP subagent stdin is unavailable"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| HarnessError::execution("ACP subagent stdout is unavailable"))?;
    Ok((stdin, stdout, child, group))
}

struct ChildGuard {
    child: Child,
    group: OwnedProcessGroup,
    completion: ManagedProcessGroup,
}

impl ChildGuard {
    fn new(child: Child, completion: ManagedProcessGroup) -> Self {
        let group = OwnedProcessGroup::new(child.id());
        Self {
            child,
            group,
            completion,
        }
    }

    async fn reap(&mut self, grace: Duration) -> Result<(), HarnessError> {
        self.group.terminate();
        let _ = tokio::time::timeout(grace, self.wait_for_group()).await;
        self.group.kill();
        let _ = self.child.start_kill();
        let _ = self.child.wait().await;
        self.completion.wait_quiescent().await
    }

    #[cfg(unix)]
    async fn wait_for_group(&mut self) {
        loop {
            let _ = self.child.try_wait();
            if !self.group.exists() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    #[cfg(not(unix))]
    async fn wait_for_group(&mut self) {
        let _ = self.child.wait().await;
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        self.group.kill();
        let _ = self.child.start_kill();
    }
}

fn validate_environment(environment: &BTreeMap<String, String>) -> Result<(), HarnessError> {
    for (name, value) in environment {
        if name.is_empty() || name.contains('=') || name.contains('\0') || value.contains('\0') {
            return Err(HarnessError::composition(
                "ACP subagent env contains an invalid name or value",
            ));
        }
    }
    Ok(())
}

fn push_bounded(output: &mut String, value: &str) {
    let remaining = MAX_OUTPUT_CHARS.saturating_sub(output.chars().count());
    output.extend(value.chars().take(remaining));
}

#[cfg(all(test, unix))]
mod tests {
    use std::{process::Stdio, time::Duration};

    use tokio::{
        io::{AsyncBufReadExt, AsyncReadExt, BufReader},
        process::{ChildStdout, Command},
    };

    use super::{ChildGuard, OwnedProcessGroup};

    async fn descendant_writer(
        leader_exits: bool,
        cooperative: bool,
    ) -> (ChildGuard, BufReader<ChildStdout>, OwnedProcessGroup) {
        let handler = if cooperative {
            "trap 'sleep 0.05; printf \"cleaned\\n\"; exit 0' TERM"
        } else {
            "trap '' TERM"
        };
        let writer = format!(
            "{handler}\nprintf 'ready\\n'\nwhile :; do printf 'writing\\n'; sleep 0.01; done"
        );
        let leader = if leader_exits { "exit 0" } else { "wait" };
        let (mut child, group) = crate::process_supervision::spawn(
            Command::new("/bin/sh")
                .args([
                    "-c",
                    &format!("/bin/sh -c \"$1\" &\n{leader}"),
                    "fixture",
                    &writer,
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .process_group(0)
                .kill_on_drop(true),
        )
        .unwrap();
        let cleanup = OwnedProcessGroup::new(child.id());
        let mut output = BufReader::new(child.stdout.take().unwrap());
        tokio::time::timeout(Duration::from_secs(2), async {
            let mut line = String::new();
            assert_ne!(output.read_line(&mut line).await.unwrap(), 0);
            assert_eq!(line, "ready\n");
        })
        .await
        .expect("descendant writer did not start");
        (ChildGuard::new(child, group), output, cleanup)
    }

    async fn remaining_output(mut output: BufReader<ChildStdout>) -> String {
        let mut remaining = String::new();
        tokio::time::timeout(
            Duration::from_secs(2),
            output.read_to_string(&mut remaining),
        )
        .await
        .expect("descendant writer survived process-group cleanup")
        .unwrap();
        remaining
    }

    #[tokio::test]
    async fn reap_stops_descendant_writer_after_leader_already_exited() {
        let (mut guard, output, _cleanup) = descendant_writer(true, false).await;
        assert!(guard.child.wait().await.unwrap().success());
        guard.reap(Duration::from_millis(50)).await.unwrap();
        assert!(remaining_output(output).await.contains("writing"));
    }

    #[tokio::test]
    async fn reap_stops_term_ignoring_descendant_after_leader_exits_on_term() {
        let (mut guard, output, _cleanup) = descendant_writer(false, false).await;
        assert!(guard.child.try_wait().unwrap().is_none());
        guard.reap(Duration::from_millis(50)).await.unwrap();
        assert!(remaining_output(output).await.contains("writing"));
    }

    #[tokio::test]
    async fn drop_stops_descendant_writer_after_leader_already_exited() {
        let (mut guard, output, _cleanup) = descendant_writer(true, false).await;
        guard.child.wait().await.unwrap();
        drop(guard);
        remaining_output(output).await;
    }

    #[tokio::test]
    async fn cancelling_reap_still_stops_descendant_writer() {
        let (mut guard, output, _cleanup) = descendant_writer(true, false).await;
        guard.child.wait().await.unwrap();
        let task = tokio::spawn(async move { guard.reap(Duration::from_secs(10)).await });
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!task.is_finished());
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        remaining_output(output).await;
    }

    #[tokio::test]
    async fn cooperative_descendant_gets_shutdown_grace_after_leader_exits() {
        let (mut guard, output, _cleanup) = descendant_writer(true, true).await;
        guard.child.wait().await.unwrap();
        guard.reap(Duration::from_millis(500)).await.unwrap();
        assert!(remaining_output(output).await.contains("cleaned"));
    }
}
