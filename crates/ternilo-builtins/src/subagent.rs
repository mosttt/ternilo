use std::{
    collections::BTreeMap,
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use linorun_core::{
    Activation, CallContext, CleanupError, ComponentContext, ComponentDescriptor, effect,
};
use linorun_macros::component_descriptor;
use serde::Deserialize;
use serde_json::{Value, json};
use ternilo_kernel::{
    ActivityBranch, DiscardModelOutput, HarnessPlugin, Models, ModelsClient, PluginFactory,
    PluginManifest, Prompts, PromptsClient, RunCancellation, RunEnvironment, RunEnvironmentClient,
    Sessions, SessionsClient, SubagentAdmission, SubagentBackend, SubagentBackendContext,
    SubagentBackendRegistration, SubagentDriver, SubagentRunStart, SubagentSessionBinding,
    SubagentSessionRequest, Subagents, SubagentsProvider, ToolExecutionContext, ToolHandler,
    ToolRegistration, Tools, ToolsClient,
};
use ternilo_protocol::{
    HarnessError, MessageRole, ModelMessage, ModelRequest, RunId, SessionEventKind, SubagentId,
    SubagentSnapshot, SubagentStatus, SubagentTranscriptKind, ToolOutput, ToolSpec,
};
use tokio::sync::{Notify, mpsc};
use tokio::task::JoinSet;

use crate::{effective_count_limit, factory as make_factory, parse_config};

pub const KIND: &str = "ternilo.subagents.in_process";

component_descriptor! {
    static DESCRIPTOR: () {
        id: "ternilo/builtin-in-process-subagents@1",
        requires: [Models, Prompts, Tools, Sessions, RunEnvironment],
        provides: [Subagents],
    }
}

#[derive(Clone, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct SubagentConfig {
    #[serde(default = "default_max_children")]
    max_children: usize,
    #[serde(default = "default_max_steps")]
    max_steps: u32,
    /// Maximum tool calls per child turn; 0 means no plugin limit. The host ceiling still applies.
    #[serde(default = "default_max_tool_calls")]
    max_tool_calls: u32,
    #[serde(default = "default_foreground_timeout_ms")]
    foreground_timeout_ms: u64,
}

const fn default_max_children() -> usize {
    8
}

const fn default_max_steps() -> u32 {
    0
}

const fn default_max_tool_calls() -> u32 {
    512
}

const fn default_foreground_timeout_ms() -> u64 {
    300_000
}

pub fn factory() -> PluginFactory {
    make_factory(
        PluginManifest {
            kind: KIND,
            requires: &[
                "ternilo/models@3",
                "ternilo/prompts@1",
                "ternilo/tools@1",
                "ternilo/sessions@1",
                "ternilo/run-environment@1",
            ],
            provides: &["ternilo/subagents@2"],
        },
        |value| {
            let config: SubagentConfig = parse_config(value)?;
            if config.max_children == 0 || config.foreground_timeout_ms == 0 {
                return Err(HarnessError::composition(
                    "subagent child limit and foreground timeout must be positive",
                ));
            }
            Ok(Arc::new(SubagentPlugin { config }))
        },
    )
    .with_config_schema::<SubagentConfig>()
}

struct SubagentPlugin {
    config: SubagentConfig,
}

impl HarnessPlugin for SubagentPlugin {
    fn descriptor(&self) -> &'static ComponentDescriptor {
        &DESCRIPTOR
    }

    fn activate(&self, context: ComponentContext) -> Activation {
        let models = context
            .context()
            .service::<Models>()
            .expect("subagents declare Models");
        let prompts = context
            .context()
            .service::<Prompts>()
            .expect("subagents declare Prompts");
        let tools = context
            .context()
            .service::<Tools>()
            .expect("subagents declare Tools");
        let sessions = context
            .context()
            .service::<Sessions>()
            .expect("subagents declare Sessions");
        let environment = context
            .context()
            .service::<RunEnvironment>()
            .expect("subagents declare RunEnvironment");
        let route = context.context().clone();
        let scope = context.scope().clone();
        let in_process: Arc<dyn SubagentBackend> = Arc::new(InProcessBackend {
            models,
            prompts,
            tools: tools.clone(),
            environment: environment.clone(),
            config: self.config.clone(),
        });
        let mut backends = BTreeMap::new();
        backends.insert(
            0,
            RegisteredBackend {
                name: "in-process".to_owned(),
                backend: in_process,
            },
        );
        let manager = Arc::new(SubagentManager {
            core: Arc::new(SubagentCore {
                sessions,
                environment,
                config: self.config.clone(),
                children: tokio::sync::Mutex::new(BTreeMap::new()),
                pending_cleanup: Mutex::new(JoinSet::new()),
                backends: Mutex::new(backends),
                next_backend: AtomicU64::new(1),
                next_id: AtomicU64::new(1),
            }),
        });
        Activation::Once(Box::pin(async move {
            let provider: Arc<dyn SubagentsProvider> = manager.clone();
            scope
                .provide::<Subagents>(&route, provider)
                .await
                .map_err(|error| {
                    linorun_core::ActivationFailure::user(format!("provide subagents: {error}"))
                })?;
            let registrations = register_tools(&tools, manager.clone())
                .await
                .map_err(|error| linorun_core::ActivationFailure::user(error.to_string()))?;
            Ok(Some(effect::inverse(move || async move {
                for registration in registrations.into_iter().rev() {
                    tools
                        .unregister_tool(registration)
                        .await
                        .map_err(|error| CleanupError::user(error.to_string()))?;
                }
                manager.core.shutdown().await;
                Ok(())
            })))
        }))
    }
}

struct SubagentCore {
    sessions: SessionsClient,
    environment: RunEnvironmentClient,
    config: SubagentConfig,
    children: tokio::sync::Mutex<BTreeMap<SubagentId, Arc<Child>>>,
    pending_cleanup: Mutex<JoinSet<()>>,
    backends: Mutex<BTreeMap<u64, RegisteredBackend>>,
    next_backend: AtomicU64,
    next_id: AtomicU64,
}

struct RegisteredBackend {
    name: String,
    backend: Arc<dyn SubagentBackend>,
}

struct SubagentManager {
    core: Arc<SubagentCore>,
}

/// A cancelled spawn must retire a child that has not reached its caller yet.
struct PendingChild {
    core: Arc<SubagentCore>,
    parent_run_id: RunId,
    subagent_id: Option<SubagentId>,
}

impl Drop for PendingChild {
    fn drop(&mut self) {
        let Some(subagent_id) = self.subagent_id.take() else {
            return;
        };
        let core = Arc::clone(&self.core);
        let parent_run_id = self.parent_run_id.clone();
        let mut cleanup = self
            .core
            .pending_cleanup
            .lock()
            .expect("subagent cleanup lock poisoned");
        while cleanup.try_join_next().is_some() {}
        cleanup.spawn(async move {
            let _ = core.dispose(parent_run_id, &subagent_id).await;
        });
    }
}

struct Child {
    state: tokio::sync::Mutex<ChildState>,
    driver: Arc<dyn SubagentDriver>,
    sender: mpsc::UnboundedSender<ChildCommand>,
    handle: Mutex<Option<tokio::task::JoinHandle<()>>>,
    cancellation: Mutex<Option<RunCancellation>>,
    session: Option<SubagentSessionBinding>,
}

struct ChildState {
    snapshot: SubagentSnapshot,
    start: SubagentRunStart,
    completion: Arc<ChildRunCompletion>,
}

#[derive(Default)]
struct ChildRunCompletion {
    snapshot: Mutex<Option<SubagentSnapshot>>,
    notify: Notify,
}

impl ChildRunCompletion {
    fn finish(&self, snapshot: SubagentSnapshot) -> SubagentSnapshot {
        let mut completed = self
            .snapshot
            .lock()
            .expect("subagent completion lock poisoned");
        let snapshot = completed.get_or_insert(snapshot).clone();
        self.notify.notify_waiters();
        snapshot
    }

    async fn wait(&self) -> SubagentSnapshot {
        loop {
            let notified = self.notify.notified();
            if let Some(snapshot) = self
                .snapshot
                .lock()
                .expect("subagent completion lock poisoned")
                .clone()
            {
                return snapshot;
            }
            notified.await;
        }
    }
}

struct ChildCommand {
    parent_run_id: RunId,
    message: String,
    cancellation: RunCancellation,
    start: SubagentRunStart,
    completion: Arc<ChildRunCompletion>,
}

impl SubagentCore {
    fn register_backend(
        &self,
        registration: SubagentBackendRegistration,
    ) -> Result<u64, HarnessError> {
        let name = registration.name.trim();
        if name.is_empty() || name.chars().count() > 64 {
            return Err(HarnessError::invalid(
                "subagent provider name must contain 1 to 64 characters",
            ));
        }
        let mut backends = self
            .backends
            .lock()
            .map_err(|_| HarnessError::execution("subagent backend registry lock poisoned"))?;
        if backends.values().any(|backend| backend.name == name) {
            return Err(HarnessError::composition(format!(
                "subagent provider {name:?} is already registered"
            )));
        }
        let id = self.next_backend.fetch_add(1, Ordering::Relaxed);
        backends.insert(
            id,
            RegisteredBackend {
                name: name.to_owned(),
                backend: registration.backend,
            },
        );
        Ok(id)
    }

    fn unregister_backend(&self, registration: u64) -> Result<(), HarnessError> {
        if registration == 0 {
            return Err(HarnessError::policy(
                "the built-in in-process subagent provider cannot be unregistered",
            ));
        }
        self.backends
            .lock()
            .map_err(|_| HarnessError::execution("subagent backend registry lock poisoned"))?
            .remove(&registration)
            .map(|_| ())
            .ok_or_else(|| {
                HarnessError::invalid(format!(
                    "unknown subagent provider registration {registration}"
                ))
            })
    }

    fn providers(&self) -> Vec<String> {
        let mut providers = self
            .backends
            .lock()
            .expect("subagent backend registry lock poisoned")
            .values()
            .map(|backend| backend.name.clone())
            .collect::<Vec<_>>();
        providers.sort();
        providers
    }

    fn backend(&self, name: &str) -> Result<Arc<dyn SubagentBackend>, HarnessError> {
        self.backends
            .lock()
            .map_err(|_| HarnessError::execution("subagent backend registry lock poisoned"))?
            .values()
            .find(|backend| backend.name == name)
            .map(|backend| Arc::clone(&backend.backend))
            .ok_or_else(|| HarnessError::invalid(format!("unknown subagent provider {name:?}")))
    }

    #[expect(
        clippy::too_many_lines,
        reason = "registration and startup acknowledgement share the pending child cleanup guard"
    )]
    async fn spawn(
        self: &Arc<Self>,
        provider: String,
        parent_run_id: RunId,
        task: String,
        label: Option<String>,
        background: bool,
        activity: ActivityBranch,
    ) -> Result<SubagentSnapshot, HarnessError> {
        let task = task.trim().to_owned();
        if task.is_empty() {
            return Err(HarnessError::invalid("subagent task must not be empty"));
        }
        let provider = provider.trim().to_owned();
        let backend = self.backend(&provider)?;
        let now = now_ms()?;
        let sequence = self.next_id.fetch_add(1, Ordering::Relaxed);
        let subagent_id = SubagentId::new(format!("agent-{now}-{sequence}"));
        let label = label
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| short_label(&task));
        if label.chars().count() > 80 {
            return Err(HarnessError::invalid(
                "subagent label must not exceed 80 characters",
            ));
        }
        let driver = backend.create(SubagentBackendContext {
            subagent_id: subagent_id.clone(),
            label: label.clone(),
            workspace: self.environment.workspace().await,
        })?;
        let supports_followup = driver.supports_followup();
        let transcript_kind = driver.transcript_kind();
        let session = self
            .environment
            .create_subagent_session(SubagentSessionRequest {
                subagent_id: subagent_id.clone(),
                provider: provider.clone(),
                label: label.clone(),
                task: task.clone(),
                transcript_kind,
            })
            .await?;
        let snapshot = SubagentSnapshot {
            subagent_id: subagent_id.clone(),
            provider,
            label,
            task: task.clone(),
            supports_followup,
            session_id: session.as_ref().map(|binding| binding.session_id.clone()),
            transcript_kind,
            status: SubagentStatus::Running,
            output: None,
            error: None,
            created_at_ms: now,
            updated_at_ms: now,
        };
        let start = SubagentRunStart::with_provenance(automated_input_provenance()?);
        let completion = Arc::new(ChildRunCompletion::default());
        let (sender, receiver) = mpsc::unbounded_channel();
        let child = Arc::new(Child {
            state: tokio::sync::Mutex::new(ChildState {
                snapshot: snapshot.clone(),
                start: start.clone(),
                completion: Arc::clone(&completion),
            }),
            driver,
            sender,
            handle: Mutex::new(None),
            cancellation: Mutex::new(None),
            session,
        });
        {
            let mut children = self.children.lock().await;
            if children.len() >= self.config.max_children {
                return Err(HarnessError::policy(format!(
                    "session already owns the maximum of {} subagents",
                    self.config.max_children
                )));
            }
            children.insert(subagent_id.clone(), child.clone());
        }
        let mut pending = PendingChild {
            core: Arc::clone(self),
            parent_run_id: parent_run_id.clone(),
            subagent_id: Some(subagent_id.clone()),
        };
        self.publish(parent_run_id.clone(), &snapshot).await?;
        self.publish_lifecycle(parent_run_id.clone(), &snapshot)
            .await?;
        let core = self.clone();
        let worker_child = child.clone();
        let handle = tokio::spawn(async move {
            child_worker(core, worker_child, receiver).await;
        });
        *child
            .handle
            .lock()
            .map_err(|_| HarnessError::execution("subagent handle lock poisoned"))? = Some(handle);
        let cancellation = RunCancellation::new();
        *child
            .cancellation
            .lock()
            .map_err(|_| HarnessError::execution("subagent cancellation lock poisoned"))? =
            Some(cancellation.clone());
        child
            .sender
            .send(ChildCommand {
                parent_run_id,
                message: task,
                cancellation,
                start: start.clone(),
                completion,
            })
            .map_err(|_| HarnessError::execution("subagent worker stopped during startup"))?;
        let result = if background {
            start.wait().await?;
            snapshot
        } else {
            self.wait(
                &subagent_id,
                Duration::from_millis(self.config.foreground_timeout_ms),
                activity,
            )
            .await?
        };
        pending.subagent_id = None;
        start.mark_delivered();
        Ok(result)
    }

    async fn followup(
        &self,
        parent_run_id: RunId,
        subagent_id: &SubagentId,
        message: String,
        provenance: Option<ternilo_protocol::InputProvenance>,
    ) -> Result<SubagentSnapshot, HarnessError> {
        let message = message.trim().to_owned();
        if message.is_empty() {
            return Err(HarnessError::invalid(
                "subagent follow-up must not be empty",
            ));
        }
        let provenance = match provenance {
            Some(provenance) => {
                provenance.validate()?;
                provenance
            }
            None => automated_input_provenance()?,
        };
        let child = self.child(subagent_id).await?;
        if !child.driver.supports_followup() {
            return Err(HarnessError::invalid(format!(
                "subagent provider {:?} is one-shot and does not support follow-up messages",
                child.state.lock().await.snapshot.provider
            )));
        }
        let start = SubagentRunStart::with_provenance(provenance);
        let completion = Arc::new(ChildRunCompletion::default());
        let snapshot = {
            let mut state = child.state.lock().await;
            let snapshot = &mut state.snapshot;
            if !matches!(
                snapshot.status,
                SubagentStatus::Idle | SubagentStatus::Failed
            ) {
                return Err(HarnessError::invalid(format!(
                    "subagent {subagent_id} is not ready for a follow-up"
                )));
            }
            snapshot.status = SubagentStatus::Running;
            snapshot.error = None;
            snapshot.updated_at_ms = now_ms()?;
            let snapshot = snapshot.clone();
            state.start = start.clone();
            state.completion = Arc::clone(&completion);
            snapshot
        };
        self.publish(parent_run_id.clone(), &snapshot).await?;
        self.publish_lifecycle(parent_run_id.clone(), &snapshot)
            .await?;
        let cancellation = RunCancellation::new();
        *child
            .cancellation
            .lock()
            .map_err(|_| HarnessError::execution("subagent cancellation lock poisoned"))? =
            Some(cancellation.clone());
        child
            .sender
            .send(ChildCommand {
                parent_run_id,
                message,
                cancellation,
                start: start.clone(),
                completion,
            })
            .map_err(|_| HarnessError::execution("subagent worker is no longer available"))?;
        start.wait().await?;
        start.mark_delivered();
        Ok(snapshot)
    }

    async fn child(&self, subagent_id: &SubagentId) -> Result<Arc<Child>, HarnessError> {
        self.children
            .lock()
            .await
            .get(subagent_id)
            .cloned()
            .ok_or_else(|| HarnessError::invalid(format!("unknown subagent {subagent_id}")))
    }

    async fn get(&self, subagent_id: &SubagentId) -> Result<SubagentSnapshot, HarnessError> {
        Ok(self
            .child(subagent_id)
            .await?
            .state
            .lock()
            .await
            .snapshot
            .clone())
    }

    async fn list(&self) -> Vec<SubagentSnapshot> {
        let children = self
            .children
            .lock()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let mut snapshots = Vec::with_capacity(children.len());
        for child in children {
            snapshots.push(child.state.lock().await.snapshot.clone());
        }
        snapshots.sort_by_key(|snapshot| snapshot.created_at_ms);
        snapshots
    }

    async fn wait(
        &self,
        subagent_id: &SubagentId,
        duration: Duration,
        activity: ActivityBranch,
    ) -> Result<SubagentSnapshot, HarnessError> {
        let child = self.child(subagent_id).await?;
        let deadline = tokio::time::Instant::now() + duration;
        let (start, completion) = {
            let state = child.state.lock().await;
            if state.snapshot.status != SubagentStatus::Running {
                return Ok(state.snapshot.clone());
            }
            (state.start.clone(), Arc::clone(&state.completion))
        };
        let timeout_error =
            || HarnessError::execution(format!("timed out waiting for subagent {subagent_id}"));
        let admission = tokio::select! {
            biased;
            snapshot = completion.wait() => return Ok(snapshot),
            admission = tokio::time::timeout_at(deadline, start.wait()) => {
                admission.map_err(|_| timeout_error())??
            }
        };
        let waiting = async {
            tokio::time::timeout_at(deadline, completion.wait())
                .await
                .map_err(|_| timeout_error())
        };
        match admission {
            SubagentAdmission::Direct => waiting.await,
            // The result timeout ends the wait; restoring foreground admission can take longer.
            SubagentAdmission::Scheduled(ticket) => activity.wait_for(ticket, waiting).await,
        }
    }

    async fn interrupt(
        &self,
        parent_run_id: RunId,
        subagent_id: &SubagentId,
    ) -> Result<SubagentSnapshot, HarnessError> {
        let child = self.child(subagent_id).await?;
        let snapshot = {
            let mut state = child.state.lock().await;
            let cancelled = if let Some(cancellation) = child
                .cancellation
                .lock()
                .map_err(|_| HarnessError::execution("subagent cancellation lock poisoned"))?
                .as_ref()
            {
                cancellation.cancel();
                true
            } else {
                false
            };
            if !cancelled {
                return Err(HarnessError::invalid(format!(
                    "subagent {subagent_id} has no active task to interrupt"
                )));
            }
            let snapshot = &mut state.snapshot;
            snapshot.status = SubagentStatus::Cancelled;
            snapshot.updated_at_ms = now_ms()?;
            let snapshot = snapshot.clone();
            state.completion.finish(snapshot.clone());
            snapshot
        };
        self.publish(parent_run_id.clone(), &snapshot).await?;
        self.publish_lifecycle(parent_run_id, &snapshot).await?;
        Ok(snapshot)
    }

    async fn dispose(
        &self,
        parent_run_id: RunId,
        subagent_id: &SubagentId,
    ) -> Result<SubagentSnapshot, HarnessError> {
        let child = self
            .children
            .lock()
            .await
            .remove(subagent_id)
            .ok_or_else(|| HarnessError::invalid(format!("unknown subagent {subagent_id}")))?;
        let snapshot = {
            let mut state = child.state.lock().await;
            if let Some(cancellation) = child
                .cancellation
                .lock()
                .map_err(|_| HarnessError::execution("subagent cancellation lock poisoned"))?
                .take()
            {
                cancellation.cancel();
            }
            if let Some(handle) = child
                .handle
                .lock()
                .map_err(|_| HarnessError::execution("subagent handle lock poisoned"))?
                .take()
            {
                handle.abort();
            }
            let snapshot = &mut state.snapshot;
            snapshot.status = match snapshot.status {
                SubagentStatus::Running => SubagentStatus::Cancelled,
                SubagentStatus::Idle => SubagentStatus::Completed,
                status => status,
            };
            snapshot.updated_at_ms = now_ms()?;
            let snapshot = snapshot.clone();
            state.completion.finish(snapshot.clone());
            snapshot
        };
        self.publish(parent_run_id.clone(), &snapshot).await?;
        self.publish_lifecycle(parent_run_id, &snapshot).await?;
        Ok(snapshot)
    }

    async fn publish(
        &self,
        parent_run_id: RunId,
        snapshot: &SubagentSnapshot,
    ) -> Result<(), HarnessError> {
        self.sessions
            .append(
                parent_run_id,
                SessionEventKind::SubagentUpdated {
                    subagent: snapshot.clone(),
                },
            )
            .await
            .map(|_| ())
    }

    async fn publish_lifecycle(
        &self,
        parent_run_id: RunId,
        snapshot: &SubagentSnapshot,
    ) -> Result<(), HarnessError> {
        if snapshot.transcript_kind != SubagentTranscriptKind::ProcessLifecycle {
            return Ok(());
        }
        let Some(session_id) = snapshot.session_id.clone() else {
            return Ok(());
        };
        self.environment
            .append_subagent_lifecycle(session_id, parent_run_id, snapshot.clone())
            .await?;
        Ok(())
    }

    async fn shutdown(&self) {
        let mut cleanup = std::mem::take(
            &mut *self
                .pending_cleanup
                .lock()
                .expect("subagent cleanup lock poisoned"),
        );
        while cleanup.join_next().await.is_some() {}
        let children = self
            .children
            .lock()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for child in children {
            let preserved = child.state.lock().await.start.is_preserved();
            if !preserved
                && let Ok(cancellation) = child.cancellation.lock()
                && let Some(cancellation) = cancellation.as_ref()
            {
                cancellation.cancel();
            }
            if let Ok(mut handle) = child.handle.lock()
                && let Some(handle) = handle.take()
            {
                handle.abort();
            }
        }
    }
}

struct InProcessBackend {
    models: ModelsClient,
    prompts: PromptsClient,
    tools: ToolsClient,
    environment: RunEnvironmentClient,
    config: SubagentConfig,
}

impl SubagentBackend for InProcessBackend {
    fn create(
        &self,
        context: SubagentBackendContext,
    ) -> Result<Arc<dyn SubagentDriver>, HarnessError> {
        Ok(Arc::new(InProcessDriver {
            models: self.models.clone(),
            prompts: self.prompts.clone(),
            tools: self.tools.clone(),
            environment: self.environment.clone(),
            config: self.config.clone(),
            subagent_id: context.subagent_id,
            label: context.label,
            messages: tokio::sync::Mutex::new(Vec::new()),
        }))
    }
}

struct InProcessDriver {
    models: ModelsClient,
    prompts: PromptsClient,
    tools: ToolsClient,
    environment: RunEnvironmentClient,
    config: SubagentConfig,
    subagent_id: SubagentId,
    label: String,
    messages: tokio::sync::Mutex<Vec<ModelMessage>>,
}

impl SubagentDriver for InProcessDriver {
    fn run<'a>(
        &'a self,
        parent_run_id: RunId,
        message: String,
        cancellation: RunCancellation,
        session: Option<SubagentSessionBinding>,
        start: SubagentRunStart,
    ) -> Pin<Box<dyn Future<Output = Result<String, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            if let Some(session) = session {
                let result = self
                    .environment
                    .run_subagent_session(
                        session.session_id,
                        parent_run_id,
                        message,
                        cancellation,
                        start.clone(),
                    )
                    .await
                    .map(|outcome| outcome.answer);
                if let Err(error) = &result {
                    start.resolve(Err(error.clone()));
                }
                return result;
            }
            start.resolve(Ok(SubagentAdmission::Direct));
            let mut messages = self.messages.lock().await;
            messages.push(ModelMessage {
                role: MessageRole::User,
                content: message,
                reasoning_content: None,
                provider_state: None,
                attachments: Vec::new(),
                tool_call_id: None,
                tool_calls: Vec::new(),
            });
            let result =
                run_in_process_turn(self, &mut messages, &parent_run_id, cancellation).await;
            if let Err(error) = &result {
                crate::session::finish_model_tool_batch(&mut messages, &error.to_string());
            }
            result
        })
    }
}

async fn child_worker(
    core: Arc<SubagentCore>,
    child: Arc<Child>,
    mut receiver: mpsc::UnboundedReceiver<ChildCommand>,
) {
    while let Some(command) = receiver.recv().await {
        let start = command.start.clone();
        let result = child
            .driver
            .run(
                command.parent_run_id.clone(),
                command.message,
                command.cancellation,
                child.session.clone(),
                command.start,
            )
            .await;
        start.resolve(
            result
                .as_ref()
                .map(|_| SubagentAdmission::Direct)
                .map_err(Clone::clone),
        );
        if let Ok(mut active) = child.cancellation.lock() {
            *active = None;
        }
        let snapshot = {
            let mut state = child.state.lock().await;
            let mut snapshot = state.snapshot.clone();
            snapshot.updated_at_ms = now_ms().unwrap_or(snapshot.updated_at_ms);
            match result {
                Ok(output) => {
                    snapshot.status = SubagentStatus::Idle;
                    snapshot.output = Some(output);
                    snapshot.error = None;
                }
                Err(error) => {
                    if error.is_cancelled() {
                        snapshot.status = SubagentStatus::Cancelled;
                        snapshot.error = None;
                    } else {
                        snapshot.status = SubagentStatus::Failed;
                        snapshot.error = Some(error.to_string());
                    }
                }
            }
            let snapshot = command.completion.finish(snapshot);
            if Arc::ptr_eq(&state.completion, &command.completion) {
                state.snapshot = snapshot.clone();
            }
            snapshot
        };
        let _ = core.publish(command.parent_run_id.clone(), &snapshot).await;
        let _ = core
            .publish_lifecycle(command.parent_run_id, &snapshot)
            .await;
    }
}

async fn run_in_process_turn(
    driver: &InProcessDriver,
    messages: &mut Vec<ModelMessage>,
    parent_run_id: &RunId,
    cancellation: RunCancellation,
) -> Result<String, HarnessError> {
    let host = driver.environment.limits().await;
    let max_steps = effective_count_limit(driver.config.max_steps, host.max_steps);
    let max_tool_calls = effective_count_limit(driver.config.max_tool_calls, host.max_tool_calls);
    let mut tool_calls = 0_u32;
    let mut step = 1_u32;
    loop {
        if exceeds_step_limit(max_steps, step) {
            return Err(HarnessError::policy(format!(
                "subagent exceeded max_steps ({max_steps})",
            )));
        }
        cancellation.check()?;
        let mut tool_specs = driver.tools.list().await;
        tool_specs.retain(|tool| tool.name != "compact_context");
        let response = driver
            .models
            .complete(ModelRequest {
                run_id: parent_run_id.clone(),
                system_prompt: format!(
                    "{}\n\nYou are an in-process child agent named {:?}. Work independently on the delegated task. Return a concise result to the parent; do not ask the user for permission or clarification.",
                    driver.prompts.assemble().await,
                    driver.label,
                ),
                messages: messages.clone(),
                tools: tool_specs,
                step,
            }, Arc::new(DiscardModelOutput), cancellation.clone())
            .await?;
        messages.push(ModelMessage {
            role: MessageRole::Assistant,
            content: response.content.clone(),
            reasoning_content: response.reasoning_content.clone(),
            provider_state: response.provider_state.clone(),
            attachments: Vec::new(),
            tool_call_id: None,
            tool_calls: response.tool_calls.clone(),
        });
        if response.tool_calls.is_empty() {
            if response.content.trim().is_empty() {
                return Err(HarnessError::execution(
                    "subagent model returned neither content nor tool calls",
                ));
            }
            return Ok(response.content);
        }
        for call in response.tool_calls {
            cancellation.check()?;
            if max_tool_calls != 0 && tool_calls >= max_tool_calls {
                return Err(HarnessError::policy(format!(
                    "subagent exceeded max_tool_calls ({max_tool_calls})",
                )));
            }
            tool_calls = tool_calls.saturating_add(1);
            let output = driver
                .tools
                .execute(
                    RunId::new(format!("subagent:{}:{}", driver.subagent_id, parent_run_id)),
                    call.clone(),
                    cancellation.clone(),
                    // This fallback is a separate Direct child, outside host-scheduled execution.
                    ActivityBranch::untracked(),
                )
                .await
                .unwrap_or_else(|error| ToolOutput {
                    content: error.to_string(),
                    is_error: true,
                });
            messages.push(ModelMessage {
                role: MessageRole::Tool,
                content: output.content,
                reasoning_content: None,
                provider_state: None,
                attachments: Vec::new(),
                tool_call_id: Some(call.id),
                tool_calls: Vec::new(),
            });
        }
        step = step.saturating_add(1);
    }
}

impl SubagentsProvider for SubagentManager {
    fn register_backend<'a>(
        &'a self,
        _: CallContext<()>,
        backend: SubagentBackendRegistration,
    ) -> Pin<Box<dyn Future<Output = Result<u64, HarnessError>> + Send + 'a>> {
        Box::pin(async move { self.core.register_backend(backend) })
    }

    fn unregister_backend<'a>(
        &'a self,
        _: CallContext<()>,
        registration: u64,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(async move { self.core.unregister_backend(registration) })
    }

    fn providers<'a>(
        &'a self,
        _: CallContext<()>,
    ) -> Pin<Box<dyn Future<Output = Vec<String>> + Send + 'a>> {
        Box::pin(async move { self.core.providers() })
    }

    fn spawn<'a>(
        &'a self,
        _: CallContext<()>,
        parent_run_id: RunId,
        task: String,
        label: Option<String>,
        background: bool,
        activity: ActivityBranch,
    ) -> Pin<Box<dyn Future<Output = Result<SubagentSnapshot, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            self.core
                .spawn(
                    "in-process".to_owned(),
                    parent_run_id,
                    task,
                    label,
                    background,
                    activity,
                )
                .await
        })
    }

    fn spawn_on<'a>(
        &'a self,
        _: CallContext<()>,
        provider: String,
        parent_run_id: RunId,
        task: String,
        label: Option<String>,
        background: bool,
        activity: ActivityBranch,
    ) -> Pin<Box<dyn Future<Output = Result<SubagentSnapshot, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            self.core
                .spawn(provider, parent_run_id, task, label, background, activity)
                .await
        })
    }

    fn followup<'a>(
        &'a self,
        _: CallContext<()>,
        parent_run_id: RunId,
        subagent_id: SubagentId,
        message: String,
        provenance: Option<ternilo_protocol::InputProvenance>,
    ) -> Pin<Box<dyn Future<Output = Result<SubagentSnapshot, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            self.core
                .followup(parent_run_id, &subagent_id, message, provenance)
                .await
        })
    }

    fn get<'a>(
        &'a self,
        _: CallContext<()>,
        subagent_id: SubagentId,
    ) -> Pin<Box<dyn Future<Output = Result<SubagentSnapshot, HarnessError>> + Send + 'a>> {
        Box::pin(async move { self.core.get(&subagent_id).await })
    }

    fn list<'a>(
        &'a self,
        _: CallContext<()>,
    ) -> Pin<Box<dyn Future<Output = Vec<SubagentSnapshot>> + Send + 'a>> {
        Box::pin(async move { self.core.list().await })
    }

    fn wait<'a>(
        &'a self,
        _: CallContext<()>,
        subagent_id: SubagentId,
        timeout_ms: u64,
        activity: ActivityBranch,
    ) -> Pin<Box<dyn Future<Output = Result<SubagentSnapshot, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            if timeout_ms == 0 {
                return Err(HarnessError::invalid(
                    "subagent wait timeout must be positive",
                ));
            }
            self.core
                .wait(&subagent_id, Duration::from_millis(timeout_ms), activity)
                .await
        })
    }

    fn interrupt<'a>(
        &'a self,
        _: CallContext<()>,
        parent_run_id: RunId,
        subagent_id: SubagentId,
    ) -> Pin<Box<dyn Future<Output = Result<SubagentSnapshot, HarnessError>> + Send + 'a>> {
        Box::pin(async move { self.core.interrupt(parent_run_id, &subagent_id).await })
    }

    fn dispose<'a>(
        &'a self,
        _: CallContext<()>,
        parent_run_id: RunId,
        subagent_id: SubagentId,
    ) -> Pin<Box<dyn Future<Output = Result<SubagentSnapshot, HarnessError>> + Send + 'a>> {
        Box::pin(async move { self.core.dispose(parent_run_id, &subagent_id).await })
    }
}

#[derive(Clone, Copy)]
enum SubagentOperation {
    Spawn,
    Providers,
    Followup,
    List,
    Wait,
    Interrupt,
}

impl SubagentOperation {
    const fn effect(self) -> ternilo_kernel::ToolEffect {
        match self {
            Self::Providers | Self::List | Self::Wait => ternilo_kernel::ToolEffect::ReadOnly,
            Self::Spawn | Self::Followup | Self::Interrupt => ternilo_kernel::ToolEffect::Dangerous,
        }
    }
}

struct SubagentTool {
    manager: Arc<SubagentManager>,
    operation: SubagentOperation,
}

async fn register_tools(
    tools: &ToolsClient,
    manager: Arc<SubagentManager>,
) -> Result<Vec<u64>, HarnessError> {
    let definitions = [
        (
            ToolSpec {
                name: "spawn_agent".to_owned(),
                description: "Delegate a bounded task to a named subagent provider. Use list_agent_providers to discover mounted providers; in-process is the default.".to_owned(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "task": { "type": "string" },
                        "label": { "type": "string" },
                        "provider": { "type": "string", "default": "in-process" },
                        "background": { "type": "boolean", "default": true }
                    },
                    "required": ["task"],
                    "additionalProperties": false
                }),
            },
            SubagentOperation::Spawn,
        ),
        (
            ToolSpec {
                name: "list_agent_providers".to_owned(),
                description: "List the in-process and external subagent providers mounted in this session.".to_owned(),
                input_schema: empty_schema(),
            },
            SubagentOperation::Providers,
        ),
        (
            ToolSpec {
                name: "send_agent_message".to_owned(),
                description: "Send a FIFO follow-up task to an idle child agent.".to_owned(),
                input_schema: id_message_schema(),
            },
            SubagentOperation::Followup,
        ),
        (
            ToolSpec {
                name: "list_agents".to_owned(),
                description: "List child agents and their latest status and output.".to_owned(),
                input_schema: empty_schema(),
            },
            SubagentOperation::List,
        ),
        (
            ToolSpec {
                name: "wait_agent".to_owned(),
                description: "Wait for a running child agent to become idle, fail, or be cancelled.".to_owned(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "subagent_id": { "type": "string" },
                        "timeout_ms": { "type": "integer", "minimum": 1, "maximum": 300_000, "default": 30000 }
                    },
                    "required": ["subagent_id"],
                    "additionalProperties": false
                }),
            },
            SubagentOperation::Wait,
        ),
        (
            ToolSpec {
                name: "interrupt_agent".to_owned(),
                description: "Cancel a child agent and release its active task.".to_owned(),
                input_schema: id_schema(),
            },
            SubagentOperation::Interrupt,
        ),
    ];
    let mut registrations = Vec::new();
    for (spec, operation) in definitions {
        registrations.push(
            tools
                .register_tool(ToolRegistration {
                    spec,
                    effect: operation.effect(),
                    handler: Arc::new(SubagentTool {
                        manager: manager.clone(),
                        operation,
                    }),
                })
                .await?,
        );
    }
    Ok(registrations)
}

impl ToolHandler for SubagentTool {
    fn execute<'a>(
        &'a self,
        context: ToolExecutionContext,
        arguments: Value,
    ) -> Pin<Box<dyn Future<Output = Result<ToolOutput, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            let value = match self.operation {
                SubagentOperation::Spawn => {
                    let arguments: SpawnArguments = parse_arguments(arguments)?;
                    serde_json::to_value(
                        self.manager
                            .core
                            .spawn(
                                arguments.provider,
                                context.run_id,
                                arguments.task,
                                arguments.label,
                                arguments.background,
                                context.activity,
                            )
                            .await?,
                    )
                }
                SubagentOperation::Providers => {
                    let _: EmptyArguments = parse_arguments(arguments)?;
                    serde_json::to_value(self.manager.core.providers())
                }
                SubagentOperation::Followup => {
                    let arguments: MessageArguments = parse_arguments(arguments)?;
                    serde_json::to_value(
                        self.manager
                            .core
                            .followup(
                                context.run_id,
                                &SubagentId::new(arguments.subagent_id),
                                arguments.message,
                                None,
                            )
                            .await?,
                    )
                }
                SubagentOperation::List => {
                    let _: EmptyArguments = parse_arguments(arguments)?;
                    serde_json::to_value(self.manager.core.list().await)
                }
                SubagentOperation::Wait => {
                    let arguments: WaitArguments = parse_arguments(arguments)?;
                    serde_json::to_value(
                        self.manager
                            .core
                            .wait(
                                &SubagentId::new(arguments.subagent_id),
                                Duration::from_millis(arguments.timeout_ms),
                                context.activity,
                            )
                            .await?,
                    )
                }
                SubagentOperation::Interrupt => {
                    let arguments: IdArguments = parse_arguments(arguments)?;
                    serde_json::to_value(
                        self.manager
                            .core
                            .interrupt(context.run_id, &SubagentId::new(arguments.subagent_id))
                            .await?,
                    )
                }
            }
            .map_err(|error| {
                HarnessError::execution(format!("serialize subagent result: {error}"))
            })?;
            Ok(ToolOutput {
                content: serde_json::to_string_pretty(&value).map_err(|error| {
                    HarnessError::execution(format!("render subagent result: {error}"))
                })?,
                is_error: false,
            })
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SpawnArguments {
    task: String,
    label: Option<String>,
    #[serde(default = "default_subagent_provider")]
    provider: String,
    #[serde(default = "default_background")]
    background: bool,
}

const fn default_background() -> bool {
    true
}

fn default_subagent_provider() -> String {
    "in-process".to_owned()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MessageArguments {
    subagent_id: String,
    message: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WaitArguments {
    subagent_id: String,
    #[serde(default = "default_wait_timeout")]
    timeout_ms: u64,
}

const fn default_wait_timeout() -> u64 {
    30_000
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IdArguments {
    subagent_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyArguments {}

fn parse_arguments<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, HarnessError> {
    serde_json::from_value(value)
        .map_err(|error| HarnessError::invalid(format!("invalid subagent arguments: {error}")))
}

fn empty_schema() -> Value {
    json!({ "type": "object", "properties": {}, "additionalProperties": false })
}

fn id_schema() -> Value {
    json!({
        "type": "object",
        "properties": { "subagent_id": { "type": "string" } },
        "required": ["subagent_id"],
        "additionalProperties": false
    })
}

fn id_message_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "subagent_id": { "type": "string" },
            "message": { "type": "string" }
        },
        "required": ["subagent_id", "message"],
        "additionalProperties": false
    })
}

fn short_label(task: &str) -> String {
    let mut characters = task.chars();
    let prefix = characters.by_ref().take(48).collect::<String>();
    if characters.next().is_some() {
        format!("{prefix}…")
    } else {
        prefix
    }
}

fn now_ms() -> Result<u64, HarnessError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| HarnessError::execution(format!("system clock error: {error}")))?
        .as_millis()
        .try_into()
        .map_err(|_| HarnessError::execution("timestamp exceeds u64"))
}

const fn exceeds_step_limit(max_steps: u32, step: u32) -> bool {
    max_steps != 0 && step > max_steps
}

fn automated_input_provenance() -> Result<ternilo_protocol::InputProvenance, HarnessError> {
    static NEXT_INPUT: AtomicU64 = AtomicU64::new(0);
    Ok(ternilo_protocol::InputProvenance {
        run_id: None,
        input_id: ternilo_protocol::SubmissionId::new(format!(
            "subagent-input-{}-{}",
            now_ms()?,
            NEXT_INPUT.fetch_add(1, Ordering::Relaxed)
        )),
        author: ternilo_protocol::InputAuthor::Automation {
            source: ternilo_protocol::AutomatedInputSource::Subagent,
        },
    })
}

#[cfg(test)]
mod step_limit_tests {
    use super::{SubagentConfig, exceeds_step_limit};

    #[test]
    fn subagent_default_and_zero_limit_allow_more_than_eight_steps() {
        let config: SubagentConfig = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(config.max_steps, 0);
        assert!(!exceeds_step_limit(config.max_steps, 9));
    }

    #[test]
    fn subagent_positive_limit_still_stops() {
        assert!(!exceeds_step_limit(8, 8));
        assert!(exceeds_step_limit(8, 9));
    }
}
