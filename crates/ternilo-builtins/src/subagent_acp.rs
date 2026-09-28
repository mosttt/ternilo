use std::{
    collections::BTreeMap,
    future::Future,
    path::PathBuf,
    pin::Pin,
    sync::{Arc, Mutex},
    time::Duration,
};

use agent_client_protocol::{
    Agent as AcpAgentRole, ByteStreams, Client as AcpClient, ConnectionTo,
    schema::{ProtocolVersion, v1 as acp},
};
use linorun_core::{Activation, CleanupError, ComponentContext, ComponentDescriptor, effect};
use linorun_macros::component_descriptor;
use serde::Deserialize;
use ternilo_kernel::{
    HarnessPlugin, PluginFactory, PluginManifest, RunCancellation, SubagentAdmission,
    SubagentBackend, SubagentBackendContext, SubagentBackendRegistration, SubagentDriver,
    SubagentRunStart, SubagentSessionBinding, Subagents,
};
use ternilo_protocol::{HarnessError, RunId, SubagentTranscriptKind};
use tokio::process::{Child, Command};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use crate::{factory as make_factory, parse_config, process_group::OwnedProcessGroup};

pub const KIND: &str = "ternilo.subagents.acp";
const MAX_OUTPUT_CHARS: usize = 1_048_576;

component_descriptor! {
    static DESCRIPTOR: () {
        id: "ternilo/builtin-acp-subagent@1",
        requires: [Subagents],
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
    #[serde(default = "default_provider_name", alias = "providerName")]
    provider_name: String,
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    cwd: Option<PathBuf>,
    #[serde(default = "default_permission")]
    permission: PermissionPolicy,
    #[serde(default = "default_timeout_ms", alias = "timeoutMs")]
    timeout_ms: u64,
    #[serde(default = "default_shutdown_grace_ms", alias = "shutdownGraceMs")]
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
            requires: &["ternilo/subagents@2"],
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
        let config = self.config.clone();
        Activation::Once(Box::pin(async move {
            let name = config.provider_name.clone();
            let backend: Arc<dyn SubagentBackend> = Arc::new(AcpBackend { config });
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
            cwd,
        }))
    }
}

struct AcpDriver {
    config: AcpSubagentConfig,
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
        _: RunId,
        message: String,
        cancellation: RunCancellation,
        _: Option<SubagentSessionBinding>,
        start: SubagentRunStart,
    ) -> Pin<Box<dyn Future<Output = Result<String, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            start.resolve(Ok(SubagentAdmission::Direct));
            run_acp(self.config.clone(), self.cwd.clone(), message, cancellation).await
        })
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
) -> Result<String, HarnessError> {
    cancellation.check()?;
    let (stdin, stdout, child) = spawn_child(&config, &cwd)?;
    let mut process = ChildGuard::new(child);
    let transport = ByteStreams::new(stdin.compat_write(), stdout.compat());
    let output = Arc::new(Mutex::new(String::new()));
    let notification_output = Arc::clone(&output);
    let permission = config.permission;
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
            async move {
                connection
                    .send_request(acp::InitializeRequest::new(ProtocolVersion::V1))
                    .block_task()
                    .await?;
                let session = connection
                    .send_request(acp::NewSessionRequest::new(cwd))
                    .block_task()
                    .await?
                    .session_id;
                *target.lock().await = Some((connection.clone(), session.clone()));
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
    process
        .reap(Duration::from_millis(config.shutdown_grace_ms))
        .await;

    settlement_result(settlement, config.timeout_ms)
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
    ] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command.envs(&config.env);
    crate::process_group::configure(&mut command);
    let mut child = command.spawn().map_err(|error| {
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
    Ok((stdin, stdout, child))
}

struct ChildGuard {
    child: Child,
    group: OwnedProcessGroup,
}

impl ChildGuard {
    fn new(child: Child) -> Self {
        let group = OwnedProcessGroup::new(child.id());
        Self { child, group }
    }

    async fn reap(&mut self, grace: Duration) {
        self.group.terminate();
        let _ = tokio::time::timeout(grace, self.wait_for_group()).await;
        self.group.kill();
        let _ = self.child.start_kill();
        let _ = self.child.wait().await;
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
        let mut child = Command::new("/bin/sh")
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
            .kill_on_drop(true)
            .spawn()
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
        (ChildGuard::new(child), output, cleanup)
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
        guard.reap(Duration::from_millis(50)).await;
        assert!(remaining_output(output).await.contains("writing"));
    }

    #[tokio::test]
    async fn reap_stops_term_ignoring_descendant_after_leader_exits_on_term() {
        let (mut guard, output, _cleanup) = descendant_writer(false, false).await;
        assert!(guard.child.try_wait().unwrap().is_none());
        guard.reap(Duration::from_millis(50)).await;
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
        guard.reap(Duration::from_millis(500)).await;
        assert!(remaining_output(output).await.contains("cleaned"));
    }
}
