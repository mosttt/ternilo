use std::sync::{Arc, Mutex};

use linorun_core::{
    Activation, Component, ComponentContext, ComponentDescriptor, FiberHandle, FiberState,
    RootContext, Runtime, RuntimeTask, TaskSpawner,
};
use linorun_macros::component_descriptor;
use ternilo_protocol::{
    AgentInput, Attachment, HarnessError, Profile, RunId, RunOutcome, SessionEvent,
    SessionEventKind, SkillCatalogSnapshot, SkillDefinition, ToolSpec,
};
use tokio::sync::oneshot;

use crate::{
    AgentTeam, Agents, AgentsClient, Attachments, Catalog, CommandCatalogEntry, CommandResolution,
    Commands, CommandsClient, HostEnvironment, ModelGateway, MountedPlugin, RunEnvironment,
    RuntimeExtensions, SessionQueries, SessionTelemetry, SessionTitles, SessionTitlesClient,
    Sessions, SessionsClient, Skills, SkillsClient, Subagents, SubagentsClient, Tools, ToolsClient,
    validate_profile,
};

#[derive(Clone, Copy)]
pub struct TokioSpawner;

impl TaskSpawner for TokioSpawner {
    fn spawn(&self, task: RuntimeTask) {
        tokio::spawn(task);
    }
}

type BridgeSender = oneshot::Sender<(AgentsClient, SessionsClient)>;
type SkillBridgeSender = oneshot::Sender<SkillsClient>;
type TitleBridgeSender = oneshot::Sender<SessionTitlesClient>;
type SubagentBridgeSender = oneshot::Sender<SubagentsClient>;
type ToolBridgeSender = oneshot::Sender<ToolsClient>;
type CommandBridgeSender = oneshot::Sender<CommandsClient>;

component_descriptor! {
    static CLIENT_BRIDGE: () {
        id: "ternilo/client-bridge@1",
        requires: [Agents, Sessions],
        provides: [],
    }
}

struct ClientBridge {
    sender: Mutex<Option<BridgeSender>>,
}

component_descriptor! {
    static SKILL_CLIENT_BRIDGE: () {
        id: "ternilo/skill-client-bridge@1",
        requires: [Skills],
        provides: [],
    }
}

struct SkillClientBridge {
    sender: Mutex<Option<SkillBridgeSender>>,
}

component_descriptor! {
    static TOOL_CLIENT_BRIDGE: () {
        id: "ternilo/tool-client-bridge@1",
        requires: [Tools],
        provides: [],
    }
}

struct ToolClientBridge {
    sender: Mutex<Option<ToolBridgeSender>>,
}

component_descriptor! {
    static COMMAND_CLIENT_BRIDGE: () {
        id: "ternilo/command-client-bridge@1",
        requires: [Commands],
        provides: [],
    }
}

struct CommandClientBridge {
    sender: Mutex<Option<CommandBridgeSender>>,
}

component_descriptor! {
    static TITLE_CLIENT_BRIDGE: () {
        id: "ternilo/title-client-bridge@1",
        requires: [SessionTitles],
        provides: [],
    }
}

struct TitleClientBridge {
    sender: Mutex<Option<TitleBridgeSender>>,
}

component_descriptor! {
    static SUBAGENT_CLIENT_BRIDGE: () {
        id: "ternilo/subagent-client-bridge@1",
        requires: [Subagents],
        provides: [],
    }
}

struct SubagentClientBridge {
    sender: Mutex<Option<SubagentBridgeSender>>,
}

impl Component for ToolClientBridge {
    type Config = ();

    fn descriptor(&self) -> &'static ComponentDescriptor {
        &TOOL_CLIENT_BRIDGE
    }

    fn activate(&self, context: ComponentContext, _: Arc<Self::Config>) -> Activation {
        let tools = context
            .context()
            .service::<Tools>()
            .expect("tool bridge declares Tools");
        let sender = self
            .sender
            .lock()
            .expect("tool bridge sender lock poisoned")
            .take();
        Activation::Once(Box::pin(async move {
            let Some(sender) = sender else {
                return Err(linorun_core::ActivationFailure::user(
                    "tool client bridge activated more than once",
                ));
            };
            sender.send(tools).map_err(|_| {
                linorun_core::ActivationFailure::user("tool bridge receiver was dropped")
            })?;
            Ok(None)
        }))
    }
}

impl Component for CommandClientBridge {
    type Config = ();

    fn descriptor(&self) -> &'static ComponentDescriptor {
        &COMMAND_CLIENT_BRIDGE
    }

    fn activate(&self, context: ComponentContext, _: Arc<Self::Config>) -> Activation {
        let commands = context
            .context()
            .service::<Commands>()
            .expect("command bridge declares Commands");
        let sender = self
            .sender
            .lock()
            .expect("command bridge sender lock poisoned")
            .take();
        Activation::Once(Box::pin(async move {
            let Some(sender) = sender else {
                return Err(linorun_core::ActivationFailure::user(
                    "command client bridge activated more than once",
                ));
            };
            sender.send(commands).map_err(|_| {
                linorun_core::ActivationFailure::user("command bridge receiver was dropped")
            })?;
            Ok(None)
        }))
    }
}

impl Component for SubagentClientBridge {
    type Config = ();

    fn descriptor(&self) -> &'static ComponentDescriptor {
        &SUBAGENT_CLIENT_BRIDGE
    }

    fn activate(&self, context: ComponentContext, _: Arc<Self::Config>) -> Activation {
        let subagents = context
            .context()
            .service::<Subagents>()
            .expect("subagent bridge declares Subagents");
        let sender = self
            .sender
            .lock()
            .expect("subagent bridge sender lock poisoned")
            .take();
        Activation::Once(Box::pin(async move {
            let Some(sender) = sender else {
                return Err(linorun_core::ActivationFailure::user(
                    "subagent client bridge activated more than once",
                ));
            };
            sender.send(subagents).map_err(|_| {
                linorun_core::ActivationFailure::user("subagent bridge receiver was dropped")
            })?;
            Ok(None)
        }))
    }
}

impl Component for TitleClientBridge {
    type Config = ();

    fn descriptor(&self) -> &'static ComponentDescriptor {
        &TITLE_CLIENT_BRIDGE
    }

    fn activate(&self, context: ComponentContext, _: Arc<Self::Config>) -> Activation {
        let titles = context
            .context()
            .service::<SessionTitles>()
            .expect("title bridge declares SessionTitles");
        let sender = self
            .sender
            .lock()
            .expect("title bridge sender lock poisoned")
            .take();
        Activation::Once(Box::pin(async move {
            let Some(sender) = sender else {
                return Err(linorun_core::ActivationFailure::user(
                    "title client bridge activated more than once",
                ));
            };
            sender.send(titles).map_err(|_| {
                linorun_core::ActivationFailure::user("title bridge receiver was dropped")
            })?;
            Ok(None)
        }))
    }
}

impl Component for SkillClientBridge {
    type Config = ();

    fn descriptor(&self) -> &'static ComponentDescriptor {
        &SKILL_CLIENT_BRIDGE
    }

    fn activate(&self, context: ComponentContext, _: Arc<Self::Config>) -> Activation {
        let skills = context
            .context()
            .service::<Skills>()
            .expect("skill bridge declares Skills");
        let sender = self
            .sender
            .lock()
            .expect("skill bridge sender lock poisoned")
            .take();
        Activation::Once(Box::pin(async move {
            let Some(sender) = sender else {
                return Err(linorun_core::ActivationFailure::user(
                    "skill client bridge activated more than once",
                ));
            };
            sender.send(skills).map_err(|_| {
                linorun_core::ActivationFailure::user("skill bridge receiver was dropped")
            })?;
            Ok(None)
        }))
    }
}

impl Component for ClientBridge {
    type Config = ();

    fn descriptor(&self) -> &'static ComponentDescriptor {
        &CLIENT_BRIDGE
    }

    fn activate(&self, context: ComponentContext, _: Arc<Self::Config>) -> Activation {
        let agents = context
            .context()
            .service::<Agents>()
            .expect("bridge declares Agents");
        let sessions = context
            .context()
            .service::<Sessions>()
            .expect("bridge declares Sessions");
        let sender = self
            .sender
            .lock()
            .expect("bridge sender lock poisoned")
            .take();
        Activation::Once(Box::pin(async move {
            let Some(sender) = sender else {
                return Err(linorun_core::ActivationFailure::user(
                    "client bridge activated more than once",
                ));
            };
            sender.send((agents, sessions)).map_err(|_| {
                linorun_core::ActivationFailure::user("harness boot receiver was dropped")
            })?;
            Ok(None)
        }))
    }
}

fn profile_provides_skills(catalog: &Catalog, profile: &Profile) -> bool {
    profile
        .plugins
        .iter()
        .filter(|entry| entry.enabled)
        .any(|entry| {
            catalog
                .factory(&entry.kind)
                .is_ok_and(|factory| factory.manifest.provides.contains(&"ternilo/skills@1"))
        })
}

fn profile_provides_tools(catalog: &Catalog, profile: &Profile) -> bool {
    profile
        .plugins
        .iter()
        .filter(|entry| entry.enabled)
        .any(|entry| {
            catalog
                .factory(&entry.kind)
                .is_ok_and(|factory| factory.manifest.provides.contains(&"ternilo/tools@1"))
        })
}

fn profile_provides_commands(catalog: &Catalog, profile: &Profile) -> bool {
    profile
        .plugins
        .iter()
        .filter(|entry| entry.enabled)
        .any(|entry| {
            catalog
                .factory(&entry.kind)
                .is_ok_and(|factory| factory.manifest.provides.contains(&"ternilo/commands@1"))
        })
}

fn profile_provides_titles(catalog: &Catalog, profile: &Profile) -> bool {
    profile
        .plugins
        .iter()
        .filter(|entry| entry.enabled)
        .any(|entry| {
            catalog.factory(&entry.kind).is_ok_and(|factory| {
                factory
                    .manifest
                    .provides
                    .contains(&"ternilo/session-titles@2")
            })
        })
}

fn profile_provides_subagents(catalog: &Catalog, profile: &Profile) -> bool {
    profile
        .plugins
        .iter()
        .filter(|entry| entry.enabled)
        .any(|entry| {
            catalog
                .factory(&entry.kind)
                .is_ok_and(|factory| factory.manifest.provides.contains(&"ternilo/subagents@2"))
        })
}

fn build_plugins(
    catalog: &Catalog,
    profile: &Profile,
) -> Result<Vec<(String, MountedPlugin)>, HarnessError> {
    profile
        .plugins
        .iter()
        .filter(|entry| entry.enabled)
        .map(|entry| {
            catalog
                .build(&entry.kind, entry.config.clone())
                .map(|plugin| (entry.id.clone(), plugin))
        })
        .collect()
}

async fn provide_host_services(
    root: &RootContext,
    environment: HostEnvironment,
) -> Result<(), HarnessError> {
    let telemetry: Arc<dyn crate::SessionTelemetryProvider> = environment.session_telemetry();
    let model_gateway: Arc<dyn crate::ModelGatewayProvider> = environment.model_gateway();
    let agent_team: Arc<dyn crate::AgentTeamProvider> = environment.agent_team();
    let environment = Arc::new(environment);

    let run_environment: Arc<dyn crate::RunEnvironmentProvider> = environment.clone();
    root.provide::<RunEnvironment>(run_environment)
        .await
        .map_err(|error| HarnessError::execution(format!("provide host environment: {error}")))?;

    let attachments: Arc<dyn crate::AttachmentsProvider> = environment.clone();
    root.provide::<Attachments>(attachments)
        .await
        .map_err(|error| {
            HarnessError::execution(format!("provide host attachment resolver: {error}"))
        })?;
    root.provide::<AgentTeam>(agent_team)
        .await
        .map_err(|error| HarnessError::execution(format!("provide host Agent Team: {error}")))?;
    root.provide::<SessionTelemetry>(telemetry)
        .await
        .map_err(|error| {
            HarnessError::execution(format!("provide host session telemetry: {error}"))
        })?;
    root.provide::<ModelGateway>(model_gateway)
        .await
        .map_err(|error| HarnessError::execution(format!("provide host model gateway: {error}")))?;

    let runtime_extensions: Arc<dyn crate::RuntimeExtensionsProvider> = environment.clone();
    root.provide::<RuntimeExtensions>(runtime_extensions)
        .await
        .map_err(|error| {
            HarnessError::execution(format!("provide host runtime extensions: {error}"))
        })?;

    let session_queries: Arc<dyn crate::SessionQueriesProvider> = environment;
    root.provide::<SessionQueries>(session_queries)
        .await
        .map_err(|error| {
            HarnessError::execution(format!("provide host session archive: {error}"))
        })?;
    Ok(())
}

async fn mount_plugins(
    root: &RootContext,
    plugins: Vec<(String, MountedPlugin)>,
) -> Result<Vec<(String, FiberHandle)>, HarnessError> {
    let mut mounted = Vec::with_capacity(plugins.len());
    for (id, plugin) in plugins {
        let handle = root
            .mount(plugin, ())
            .await
            .map_err(|error| HarnessError::composition(format!("mount plugin {id}: {error}")))?;
        mounted.push((id, handle));
    }
    Ok(mounted)
}

async fn ensure_plugins_active(
    runtime: &Runtime,
    mounted: &[(String, FiberHandle)],
) -> Result<(), HarnessError> {
    let snapshot = runtime.wait_quiescent().await;
    let mut unsettled = Vec::new();
    for (id, handle) in mounted {
        let state = handle.state().await;
        if state != FiberState::Active {
            let failure = snapshot
                .fibers
                .iter()
                .find(|fiber| fiber.id == handle.id())
                .and_then(|fiber| fiber.failure.as_ref())
                .map(|failure| format!(": {failure}"))
                .unwrap_or_default();
            unsettled.push(format!("{id}: {state:?}{failure}"));
        }
    }
    if unsettled.is_empty() {
        Ok(())
    } else {
        Err(HarnessError::composition(format!(
            "plugin graph did not activate: {}",
            unsettled.join(", ")
        )))
    }
}

async fn export_clients(
    root: &RootContext,
) -> Result<(AgentsClient, SessionsClient), HarnessError> {
    let (sender, receiver) = oneshot::channel();
    let bridge = root
        .mount(
            ClientBridge {
                sender: Mutex::new(Some(sender)),
            },
            (),
        )
        .await
        .map_err(|error| HarnessError::execution(format!("mount client bridge: {error}")))?;
    let state = bridge.wait_settled().await;
    if state != FiberState::Active {
        return Err(HarnessError::composition(format!(
            "client bridge did not activate: {state:?}"
        )));
    }
    receiver
        .await
        .map_err(|_| HarnessError::execution("client bridge stopped before exporting services"))
}

async fn export_skills(root: &RootContext) -> Result<SkillsClient, HarnessError> {
    let (sender, receiver) = oneshot::channel();
    let bridge = root
        .mount(
            SkillClientBridge {
                sender: Mutex::new(Some(sender)),
            },
            (),
        )
        .await
        .map_err(|error| HarnessError::execution(format!("mount skill client bridge: {error}")))?;
    if bridge.wait_settled().await != FiberState::Active {
        return Err(HarnessError::composition(
            "skill client bridge did not activate",
        ));
    }
    receiver.await.map_err(|_| {
        HarnessError::execution("skill client bridge stopped before exporting the service")
    })
}

async fn export_tools(root: &RootContext) -> Result<ToolsClient, HarnessError> {
    let (sender, receiver) = oneshot::channel();
    let bridge = root
        .mount(
            ToolClientBridge {
                sender: Mutex::new(Some(sender)),
            },
            (),
        )
        .await
        .map_err(|error| HarnessError::execution(format!("mount tool client bridge: {error}")))?;
    if bridge.wait_settled().await != FiberState::Active {
        return Err(HarnessError::composition(
            "tool client bridge did not activate",
        ));
    }
    receiver.await.map_err(|_| {
        HarnessError::execution("tool client bridge stopped before exporting the service")
    })
}

async fn export_commands(root: &RootContext) -> Result<CommandsClient, HarnessError> {
    let (sender, receiver) = oneshot::channel();
    let bridge = root
        .mount(
            CommandClientBridge {
                sender: Mutex::new(Some(sender)),
            },
            (),
        )
        .await
        .map_err(|error| {
            HarnessError::execution(format!("mount command client bridge: {error}"))
        })?;
    if bridge.wait_settled().await != FiberState::Active {
        return Err(HarnessError::composition(
            "command client bridge did not activate",
        ));
    }
    receiver.await.map_err(|_| {
        HarnessError::execution("command client bridge stopped before exporting the service")
    })
}

async fn export_titles(root: &RootContext) -> Result<SessionTitlesClient, HarnessError> {
    let (sender, receiver) = oneshot::channel();
    let bridge = root
        .mount(
            TitleClientBridge {
                sender: Mutex::new(Some(sender)),
            },
            (),
        )
        .await
        .map_err(|error| HarnessError::execution(format!("mount title client bridge: {error}")))?;
    if bridge.wait_settled().await != FiberState::Active {
        return Err(HarnessError::composition(
            "title client bridge did not activate",
        ));
    }
    receiver.await.map_err(|_| {
        HarnessError::execution("title client bridge stopped before exporting the service")
    })
}

async fn export_subagents(root: &RootContext) -> Result<SubagentsClient, HarnessError> {
    let (sender, receiver) = oneshot::channel();
    let bridge = root
        .mount(
            SubagentClientBridge {
                sender: Mutex::new(Some(sender)),
            },
            (),
        )
        .await
        .map_err(|error| {
            HarnessError::execution(format!("mount subagent client bridge: {error}"))
        })?;
    if bridge.wait_settled().await != FiberState::Active {
        return Err(HarnessError::composition(
            "subagent client bridge did not activate",
        ));
    }
    receiver.await.map_err(|_| {
        HarnessError::execution("subagent client bridge stopped before exporting the service")
    })
}

async fn shutdown_if_failed<T>(
    runtime: &Runtime,
    result: Result<T, HarnessError>,
) -> Result<T, HarnessError> {
    if result.is_err() {
        runtime.shutdown().await;
    }
    result
}

pub struct HarnessSession {
    runtime: Runtime,
    agents: AgentsClient,
    sessions: SessionsClient,
    skills: Option<SkillsClient>,
    tools: Option<ToolsClient>,
    commands: Option<CommandsClient>,
    titles: Option<SessionTitlesClient>,
    subagents: Option<SubagentsClient>,
}

impl HarnessSession {
    pub async fn boot(
        catalog: &Catalog,
        profile: &Profile,
        environment: HostEnvironment,
    ) -> Result<Self, HarnessError> {
        environment.validate()?;
        validate_profile(profile, catalog)?;
        let has_skills = profile_provides_skills(catalog, profile);
        let has_tools = profile_provides_tools(catalog, profile);
        let has_commands = profile_provides_commands(catalog, profile);
        let has_titles = profile_provides_titles(catalog, profile);
        let has_subagents = profile_provides_subagents(catalog, profile);
        let plugins = build_plugins(catalog, profile)?;

        let runtime = Runtime::builder(TokioSpawner).build();
        let root = runtime.root();
        shutdown_if_failed(&runtime, provide_host_services(&root, environment).await).await?;
        let mounted = shutdown_if_failed(&runtime, mount_plugins(&root, plugins).await).await?;
        shutdown_if_failed(&runtime, ensure_plugins_active(&runtime, &mounted).await).await?;
        let (agents, sessions) = shutdown_if_failed(&runtime, export_clients(&root).await).await?;
        let skills = if has_skills {
            Some(shutdown_if_failed(&runtime, export_skills(&root).await).await?)
        } else {
            None
        };
        let tools = if has_tools {
            Some(shutdown_if_failed(&runtime, export_tools(&root).await).await?)
        } else {
            None
        };
        let commands = if has_commands {
            Some(shutdown_if_failed(&runtime, export_commands(&root).await).await?)
        } else {
            None
        };
        let titles = if has_titles {
            Some(shutdown_if_failed(&runtime, export_titles(&root).await).await?)
        } else {
            None
        };
        let subagents = if has_subagents {
            Some(shutdown_if_failed(&runtime, export_subagents(&root).await).await?)
        } else {
            None
        };

        Ok(Self {
            runtime,
            agents,
            sessions,
            skills,
            tools,
            commands,
            titles,
            subagents,
        })
    }

    pub async fn run(
        &self,
        run_id: RunId,
        input: impl Into<String>,
    ) -> Result<RunOutcome, HarnessError> {
        self.run_with_attachments(run_id, input, Vec::new()).await
    }

    pub async fn run_with_attachments(
        &self,
        run_id: RunId,
        input: impl Into<String>,
        attachments: Vec<Attachment>,
    ) -> Result<RunOutcome, HarnessError> {
        self.run_with_message_metadata(run_id, input, None, None, attachments)
            .await
    }

    pub async fn run_with_message_metadata(
        &self,
        run_id: RunId,
        input: impl Into<String>,
        display_input: Option<String>,
        source: Option<ternilo_protocol::UserMessageSource>,
        attachments: Vec<Attachment>,
    ) -> Result<RunOutcome, HarnessError> {
        self.run_with_message_metadata_and_references(
            run_id,
            input,
            display_input,
            source,
            Vec::new(),
            Vec::new(),
            attachments,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn run_with_message_metadata_and_references(
        &self,
        run_id: RunId,
        input: impl Into<String>,
        display_input: Option<String>,
        source: Option<ternilo_protocol::UserMessageSource>,
        references: Vec<ternilo_protocol::SubmissionReference>,
        reference_contexts: Vec<ternilo_protocol::ReferenceContext>,
        attachments: Vec<Attachment>,
    ) -> Result<RunOutcome, HarnessError> {
        self.run_input(AgentInput {
            additional_inputs: Vec::new(),
            provenance: None,
            run_id,
            input: input.into(),
            display_input,
            source,
            references,
            reference_contexts,
            attachments,
        })
        .await
    }

    pub async fn run_input(&self, input: AgentInput) -> Result<RunOutcome, HarnessError> {
        self.agents.run(input).await
    }

    pub async fn cancel(&self, run_id: RunId) -> Result<(), HarnessError> {
        self.agents.cancel(run_id).await
    }

    pub async fn active_run(&self) -> Option<RunId> {
        self.agents.active_run().await
    }

    /// Offer input to the currently open step boundary. `false` means the
    /// turn already closed its steering window; the host keeps the durable
    /// occurrence queued for the next turn.
    pub async fn steer(
        &self,
        input: ternilo_protocol::SteeringInput,
    ) -> Result<bool, HarnessError> {
        self.agents.steer(input).await
    }

    pub async fn events(&self) -> Vec<SessionEvent> {
        self.sessions.events().await
    }

    pub async fn events_after(&self, after_seq: Option<u64>) -> Vec<SessionEvent> {
        self.sessions.events_after(after_seq).await
    }

    pub async fn history(
        &self,
        query: ternilo_protocol::SessionHistoryQuery,
    ) -> Result<ternilo_protocol::SessionEventPage, HarnessError> {
        self.sessions.history(query).await
    }

    pub async fn append_event(
        &self,
        run_id: RunId,
        kind: SessionEventKind,
    ) -> Result<SessionEvent, HarnessError> {
        self.sessions.append(run_id, kind).await
    }

    pub async fn skill_catalog(&self) -> Result<SkillCatalogSnapshot, HarnessError> {
        self.skills
            .as_ref()
            .ok_or_else(|| HarnessError::policy("this harness profile has no skill registry"))?
            .snapshot()
            .await
    }

    /// Discover all configured tools, including admitted external sources.
    pub async fn tool_catalog(&self) -> Result<Vec<ToolSpec>, HarnessError> {
        self.discover_tools(crate::RunCancellation::new()).await
    }

    pub async fn discover_tools(
        &self,
        cancellation: crate::RunCancellation,
    ) -> Result<Vec<ToolSpec>, HarnessError> {
        match &self.tools {
            Some(tools) => {
                tools.prepare(cancellation).await?;
                Ok(tools.list().await)
            }
            None => Ok(Vec::new()),
        }
    }

    pub async fn service_catalog(&self) -> Vec<ternilo_protocol::SessionServiceSnapshot> {
        match &self.tools {
            Some(tools) => tools.sources().await,
            None => Vec::new(),
        }
    }

    pub async fn start_service(
        &self,
        id: String,
        cancellation: crate::RunCancellation,
    ) -> Result<ternilo_protocol::SessionServiceSnapshot, HarnessError> {
        self.tools
            .as_ref()
            .ok_or_else(|| HarnessError::policy("this profile has no tool registry"))?
            .start_source(id, cancellation)
            .await
    }

    pub async fn stop_service(
        &self,
        id: String,
    ) -> Result<ternilo_protocol::SessionServiceSnapshot, HarnessError> {
        self.tools
            .as_ref()
            .ok_or_else(|| HarnessError::policy("this profile has no tool registry"))?
            .stop_source(id)
            .await
    }

    /// List direct commands registered in this exact composed Session runtime.
    pub async fn command_catalog(&self) -> Result<Vec<CommandCatalogEntry>, HarnessError> {
        let commands = self
            .commands
            .as_ref()
            .ok_or_else(|| HarnessError::policy("this harness profile has no command registry"))?;
        Ok(commands.catalog().await)
    }

    /// Resolve a direct command without executing it or requesting a model.
    pub async fn resolve_command(&self, input: String) -> Option<CommandResolution> {
        match &self.commands {
            Some(commands) => commands.resolve(input).await,
            None => None,
        }
    }

    pub async fn skill(
        &self,
        name: impl Into<String>,
    ) -> Result<Option<SkillDefinition>, HarnessError> {
        self.skills
            .as_ref()
            .ok_or_else(|| HarnessError::policy("this harness profile has no skill registry"))?
            .get(name.into())
            .await
    }

    pub async fn generate_session_title(
        &self,
        run_id: RunId,
        request: impl Into<String>,
        answer: impl Into<String>,
    ) -> Result<String, HarnessError> {
        self.titles
            .as_ref()
            .ok_or_else(|| HarnessError::policy("this harness profile has no title generator"))?
            .generate(run_id, request.into(), answer.into())
            .await
    }

    #[must_use]
    pub fn subagents(&self) -> Option<SubagentsClient> {
        self.subagents.clone()
    }

    pub async fn shutdown(&self) -> Result<(), HarnessError> {
        let report = self.runtime.shutdown().await;
        if report.cleanup_errors.is_empty() {
            Ok(())
        } else {
            Err(HarnessError::execution(format!(
                "{} cleanup operation(s) failed",
                report.cleanup_errors.len()
            )))
        }
    }
}
