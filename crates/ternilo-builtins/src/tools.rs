#[path = "tool_sources.rs"]
mod sources;

#[cfg(test)]
#[path = "tool_sources_tests.rs"]
mod source_tests;

use std::{
    collections::BTreeMap,
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use linorun_core::{Activation, CallContext, ComponentContext, ComponentDescriptor};
use linorun_macros::component_descriptor;
use ternilo_kernel::{
    ActivityBranch, CommandCatalogEntry, CommandRegistration, CommandResolution, Commands,
    CommandsProvider, DeferredToolSource, HarnessPlugin, PluginFactory, PluginManifest,
    ResolvedCommand, RunCancellation, RunEnvironment, RunEnvironmentClient, Sessions,
    SessionsClient, ToolAuthorization, ToolExecutionContext, ToolGuard, ToolPresentation,
    ToolPresenter, ToolRegistration, Tools, ToolsProvider,
};
use ternilo_protocol::{
    HarnessError, RunId, SessionEventKind, ToolApprovalContext, ToolCall, ToolOutput,
    ToolPresentationDescriptor, ToolSpec, UserAnswer, UserQuestion, UserQuestionOption,
};

use crate::{factory as make_factory, parse_config};

pub const KIND: &str = "ternilo.tools.registry";

component_descriptor! {
    static DESCRIPTOR: () {
        id: "ternilo/builtin-tool-registry@1",
        requires: [RunEnvironment, Sessions],
        provides: [Tools, Commands],
    }
}

pub fn factory() -> PluginFactory {
    make_factory(
        PluginManifest {
            kind: KIND,
            requires: &["ternilo/run-environment@1", "ternilo/sessions@1"],
            provides: &["ternilo/tools@1", "ternilo/commands@1"],
        },
        |value| {
            let config: ToolRegistryConfig = parse_config(value)?;
            if config.timeout_ms == 0 {
                return Err(HarnessError::composition(
                    "tool registry timeout_ms must be positive",
                ));
            }
            Ok(Arc::new(ToolRegistryPlugin { config }))
        },
    )
    .with_config_schema::<ToolRegistryConfig>()
}

#[derive(Clone, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ToolRegistryConfig {
    #[serde(default = "default_timeout_ms")]
    timeout_ms: u64,
}

const fn default_timeout_ms() -> u64 {
    300_000
}

struct ToolRegistryPlugin {
    config: ToolRegistryConfig,
}

impl HarnessPlugin for ToolRegistryPlugin {
    fn descriptor(&self) -> &'static ComponentDescriptor {
        &DESCRIPTOR
    }

    fn activate(&self, context: ComponentContext) -> Activation {
        let environment = context
            .context()
            .service::<RunEnvironment>()
            .expect("tool registry declares RunEnvironment");
        let sessions = context
            .context()
            .service::<Sessions>()
            .expect("tool registry declares Sessions");
        let route = context.context().clone();
        let scope = context.scope().clone();
        let timeout = std::time::Duration::from_millis(self.config.timeout_ms);
        let registry = Arc::new(ToolRegistry {
            environment,
            sessions,
            state: Mutex::new(ToolState::default()),
            commands: CommandRegistry::default(),
            timeout,
            next_approval: AtomicU64::new(1),
        });
        Activation::Once(Box::pin(async move {
            for command in crate::model_rule::builtin_command_registrations() {
                registry.commands.register(command).map_err(|error| {
                    linorun_core::ActivationFailure::user(format!(
                        "register builtin command: {error}"
                    ))
                })?;
            }
            let tools: Arc<dyn ToolsProvider> = registry.clone();
            scope
                .provide::<Tools>(&route, tools)
                .await
                .map_err(|error| {
                    linorun_core::ActivationFailure::user(format!("provide tool registry: {error}"))
                })?;
            let commands: Arc<dyn CommandsProvider> = registry;
            scope
                .provide::<Commands>(&route, commands)
                .await
                .map_err(|error| {
                    linorun_core::ActivationFailure::user(format!(
                        "provide command registry: {error}"
                    ))
                })?;
            Ok(None)
        }))
    }
}

#[derive(Default)]
struct ToolState {
    next: u64,
    tools: BTreeMap<u64, ToolRegistration>,
    names: BTreeMap<String, u64>,
    guards: BTreeMap<u64, Arc<dyn ToolGuard>>,
    presenter: Option<(u64, Arc<dyn ToolPresenter>)>,
    sources: BTreeMap<u64, Arc<SourceEntry>>,
    source_tools: BTreeMap<u64, Vec<u64>>,
    source_names: BTreeMap<String, u64>,
}

struct SourceEntry {
    source: Arc<dyn DeferredToolSource>,
    gate: tokio::sync::Mutex<()>,
}

struct ToolRegistry {
    environment: RunEnvironmentClient,
    sessions: SessionsClient,
    state: Mutex<ToolState>,
    commands: CommandRegistry,
    timeout: std::time::Duration,
    next_approval: AtomicU64,
}

#[derive(Default)]
struct CommandState {
    next: u64,
    commands: BTreeMap<u64, CommandRegistration>,
    names: BTreeMap<String, u64>,
}

#[derive(Default)]
struct CommandRegistry {
    state: Mutex<CommandState>,
}

impl CommandRegistry {
    fn register(&self, command: CommandRegistration) -> Result<u64, HarnessError> {
        let name = command.descriptor.name.as_str();
        if name.trim().is_empty()
            || name != name.trim()
            || name.starts_with('/')
            || name.chars().any(char::is_whitespace)
        {
            return Err(HarnessError::invalid(
                "command name must be a non-empty slash-free token",
            ));
        }
        if matches!(name, "feedback" | "plan" | "skill") {
            return Err(HarnessError::composition(format!(
                "command {name:?} is reserved for a typed Session route"
            )));
        }
        if command.descriptor.description.trim().is_empty() || command.tool_name.trim().is_empty() {
            return Err(HarnessError::invalid(
                "command description and tool name must not be empty",
            ));
        }
        if command
            .descriptor
            .input
            .as_ref()
            .is_some_and(|input| input.hint.trim().is_empty())
        {
            return Err(HarnessError::invalid(
                "command input hint must not be empty",
            ));
        }

        let mut state = self
            .state
            .lock()
            .map_err(|_| HarnessError::execution("command registry lock poisoned"))?;
        if state.names.contains_key(name) {
            return Err(HarnessError::composition(format!(
                "command {name:?} is already registered"
            )));
        }
        let registration = state.next;
        state.next = state
            .next
            .checked_add(1)
            .ok_or_else(|| HarnessError::execution("command registration id exhausted"))?;
        state.names.insert(name.to_owned(), registration);
        state.commands.insert(registration, command);
        Ok(registration)
    }

    fn unregister(&self, registration: u64) -> Result<(), HarnessError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| HarnessError::execution("command registry lock poisoned"))?;
        let command = state.commands.remove(&registration).ok_or_else(|| {
            HarnessError::execution(format!("unknown command registration {registration}"))
        })?;
        state.names.remove(&command.descriptor.name);
        Ok(())
    }

    fn catalog(&self, tools: &BTreeMap<String, ToolSpec>) -> Vec<CommandCatalogEntry> {
        let mut entries = self
            .state
            .lock()
            .expect("command registry lock poisoned")
            .commands
            .values()
            .filter(|command| tools.contains_key(&command.tool_name))
            .map(|command| CommandCatalogEntry {
                descriptor: command.descriptor.clone(),
                tool_name: command.tool_name.clone(),
            })
            .collect::<Vec<_>>();
        entries.sort_by(|left, right| left.descriptor.name.cmp(&right.descriptor.name));
        entries
    }

    fn resolve(
        &self,
        input: &str,
        tools: &BTreeMap<String, ToolSpec>,
    ) -> Option<CommandResolution> {
        let input = input.trim();
        let command = input.strip_prefix('/')?;
        let (name, suffix) = command
            .find(char::is_whitespace)
            .map_or((command, ""), |offset| {
                (&command[..offset], command[offset..].trim_start())
            });
        let registration = {
            let state = self.state.lock().expect("command registry lock poisoned");
            let registration = state.names.get(name)?;
            state.commands.get(registration)?.clone()
        };
        let result = registration.resolver.resolve(suffix).and_then(|arguments| {
            let Some(tool) = tools.get(&registration.tool_name) else {
                return Err(HarnessError::invalid(format!(
                    "/{name} requires the `{}` tool, but it is not enabled for this session",
                    registration.tool_name
                )));
            };
            validate_command_arguments(name, &tool.input_schema, &arguments)?;
            Ok(ResolvedCommand {
                tool_name: registration.tool_name,
                arguments,
            })
        });
        Some(CommandResolution {
            command_name: name.to_owned(),
            result,
        })
    }
}

fn validate_command_arguments(
    command_name: &str,
    schema: &serde_json::Value,
    arguments: &serde_json::Value,
) -> Result<(), HarnessError> {
    let validator = jsonschema::validator_for(schema).map_err(|error| {
        HarnessError::composition(format!("compile input schema for /{command_name}: {error}"))
    })?;
    if let Err(error) = validator.validate(arguments) {
        return Err(HarnessError::invalid(format!(
            "/{command_name} arguments do not match the target tool input schema: {error}"
        )));
    }
    Ok(())
}

impl ToolRegistry {
    fn next_id(state: &mut ToolState) -> Result<u64, HarnessError> {
        let id = state.next;
        state.next = state
            .next
            .checked_add(1)
            .ok_or_else(|| HarnessError::execution("tool registration id exhausted"))?;
        Ok(id)
    }

    async fn request_approval(
        &self,
        run_id: RunId,
        call: &ToolCall,
        reason: String,
    ) -> Result<(), HarnessError> {
        let sequence = self.next_approval.fetch_add(1, Ordering::Relaxed);
        let question = UserQuestion {
            id: format!("tool-approval-{run_id}-{sequence}"),
            question: format!("允许工具 `{}` 执行一次？\n\n原因：{}", call.name, reason),
            detail: None,
            header: None,
            options: vec![
                UserQuestionOption {
                    label: "Allow once".to_owned(),
                    description: None,
                },
                UserQuestionOption {
                    label: "Deny".to_owned(),
                    description: None,
                },
            ],
            multi_select: false,
            presentation: None,
            tool_approval: Some(ToolApprovalContext {
                tool_name: call.name.clone(),
                call_id: call.id.clone(),
                reason,
                arguments: call.arguments.clone(),
                presentation: call.presentation.clone(),
            }),
        };
        self.sessions
            .append(
                run_id.clone(),
                SessionEventKind::UserQuestionAsked {
                    question: question.clone(),
                },
            )
            .await?;

        let answer = match self.environment.ask_user(question.clone()).await {
            Ok(answer) => answer,
            Err(error) => {
                self.sessions
                    .append(
                        run_id,
                        SessionEventKind::UserQuestionAnswered {
                            answer: UserAnswer {
                                question_id: question.id,
                                selected: Vec::new(),
                                custom: Some("Unavailable".to_owned()),
                            },
                        },
                    )
                    .await?;
                return Err(error);
            }
        };
        self.sessions
            .append(
                run_id,
                SessionEventKind::UserQuestionAnswered {
                    answer: answer.clone(),
                },
            )
            .await?;
        if answer.chose("Allow once") {
            Ok(())
        } else {
            Err(HarnessError::policy(format!(
                "user denied one-time approval for tool {:?}",
                call.name
            )))
        }
    }
}

impl CommandsProvider for ToolRegistry {
    fn register_command<'a>(
        &'a self,
        _: CallContext<()>,
        command: CommandRegistration,
    ) -> Pin<Box<dyn Future<Output = Result<u64, HarnessError>> + Send + 'a>> {
        Box::pin(async move { self.commands.register(command) })
    }

    fn unregister_command<'a>(
        &'a self,
        _: CallContext<()>,
        registration: u64,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(async move { self.commands.unregister(registration) })
    }

    fn catalog<'a>(
        &'a self,
        _: CallContext<()>,
    ) -> Pin<Box<dyn Future<Output = Vec<CommandCatalogEntry>> + Send + 'a>> {
        Box::pin(async move {
            let tools = self
                .state
                .lock()
                .expect("tool registry lock poisoned")
                .tools
                .values()
                .map(|tool| (tool.spec.name.clone(), tool.spec.clone()))
                .collect::<BTreeMap<_, _>>();
            self.commands.catalog(&tools)
        })
    }

    fn resolve<'a>(
        &'a self,
        _: CallContext<()>,
        input: String,
    ) -> Pin<Box<dyn Future<Output = Option<CommandResolution>> + Send + 'a>> {
        Box::pin(async move {
            let tools = self
                .state
                .lock()
                .expect("tool registry lock poisoned")
                .tools
                .values()
                .map(|tool| (tool.spec.name.clone(), tool.spec.clone()))
                .collect::<BTreeMap<_, _>>();
            self.commands.resolve(&input, &tools)
        })
    }
}

impl ToolsProvider for ToolRegistry {
    fn register_source<'a>(
        &'a self,
        _: CallContext<()>,
        source: Arc<dyn DeferredToolSource>,
    ) -> Pin<Box<dyn Future<Output = Result<u64, HarnessError>> + Send + 'a>> {
        Box::pin(async move { self.add_source(source) })
    }

    fn unregister_source<'a>(
        &'a self,
        _: CallContext<()>,
        registration: u64,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(async move { self.remove_source(registration).await })
    }

    fn prepare<'a>(
        &'a self,
        _: CallContext<()>,
        cancellation: RunCancellation,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(async move { self.prepare_sources(cancellation).await })
    }

    fn sources<'a>(
        &'a self,
        _: CallContext<()>,
    ) -> Pin<Box<dyn Future<Output = Vec<ternilo_protocol::SessionServiceSnapshot>> + Send + 'a>>
    {
        Box::pin(async move { self.source_snapshots() })
    }

    fn start_source<'a>(
        &'a self,
        _: CallContext<()>,
        id: String,
        cancellation: RunCancellation,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<ternilo_protocol::SessionServiceSnapshot, HarnessError>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async move { self.start_service(&id, cancellation).await })
    }

    fn stop_source<'a>(
        &'a self,
        _: CallContext<()>,
        id: String,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<ternilo_protocol::SessionServiceSnapshot, HarnessError>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async move { self.stop_service(&id).await })
    }

    fn register_tool<'a>(
        &'a self,
        _: CallContext<()>,
        tool: ToolRegistration,
    ) -> Pin<Box<dyn Future<Output = Result<u64, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            if tool.spec.name.trim().is_empty() {
                return Err(HarnessError::invalid("tool name must not be empty"));
            }
            let mut state = self
                .state
                .lock()
                .map_err(|_| HarnessError::execution("tool registry lock poisoned"))?;
            if state.names.contains_key(&tool.spec.name) {
                return Err(HarnessError::composition(format!(
                    "tool {:?} is already registered",
                    tool.spec.name
                )));
            }
            let name = tool.spec.name.clone();
            let id = Self::next_id(&mut state)?;
            state.names.insert(name, id);
            state.tools.insert(id, tool);
            Ok(id)
        })
    }

    fn unregister_tool<'a>(
        &'a self,
        _: CallContext<()>,
        registration: u64,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            let mut state = self
                .state
                .lock()
                .map_err(|_| HarnessError::execution("tool registry lock poisoned"))?;
            let tool = state.tools.remove(&registration).ok_or_else(|| {
                HarnessError::execution(format!("unknown tool registration {registration}"))
            })?;
            state.names.remove(&tool.spec.name);
            Ok(())
        })
    }

    fn register_guard<'a>(
        &'a self,
        _: CallContext<()>,
        guard: Arc<dyn ToolGuard>,
    ) -> Pin<Box<dyn Future<Output = u64> + Send + 'a>> {
        Box::pin(async move {
            let mut state = self.state.lock().expect("tool registry lock poisoned");
            let id = state.next;
            state.next = state
                .next
                .checked_add(1)
                .expect("tool registration id exhausted");
            state.guards.insert(id, guard);
            id
        })
    }

    fn unregister_guard<'a>(
        &'a self,
        _: CallContext<()>,
        registration: u64,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            self.state
                .lock()
                .map_err(|_| HarnessError::execution("tool registry lock poisoned"))?
                .guards
                .remove(&registration)
                .map(|_| ())
                .ok_or_else(|| {
                    HarnessError::execution(format!(
                        "unknown tool guard registration {registration}"
                    ))
                })
        })
    }

    fn register_presenter<'a>(
        &'a self,
        _: CallContext<()>,
        presenter: Arc<dyn ToolPresenter>,
    ) -> Pin<Box<dyn Future<Output = Result<u64, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            let mut state = self
                .state
                .lock()
                .map_err(|_| HarnessError::execution("tool registry lock poisoned"))?;
            if state.presenter.is_some() {
                return Err(HarnessError::composition(
                    "a tool presenter is already registered",
                ));
            }
            let id = Self::next_id(&mut state)?;
            state.presenter = Some((id, presenter));
            Ok(id)
        })
    }

    fn unregister_presenter<'a>(
        &'a self,
        _: CallContext<()>,
        registration: u64,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            let mut state = self
                .state
                .lock()
                .map_err(|_| HarnessError::execution("tool registry lock poisoned"))?;
            if state
                .presenter
                .as_ref()
                .is_some_and(|(id, _)| *id == registration)
            {
                state.presenter = None;
                Ok(())
            } else {
                Err(HarnessError::execution(format!(
                    "unknown tool presenter registration {registration}"
                )))
            }
        })
    }

    fn list<'a>(
        &'a self,
        _: CallContext<()>,
    ) -> Pin<Box<dyn Future<Output = Vec<ToolSpec>> + Send + 'a>> {
        Box::pin(async move {
            let mut tools: Vec<_> = self
                .state
                .lock()
                .expect("tool registry lock poisoned")
                .tools
                .values()
                .map(|tool| tool.spec.clone())
                .collect();
            if !crate::agent_goal::goal_tool_enabled(&self.sessions.events().await) {
                tools.retain(|tool| tool.name != "update_goal");
            }
            tools.sort_by(|left, right| left.name.cmp(&right.name));
            tools
        })
    }

    fn present<'a>(
        &'a self,
        _: CallContext<()>,
    ) -> Pin<Box<dyn Future<Output = Result<ToolPresentation, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            let (mut tools, presenter) = {
                let state = self
                    .state
                    .lock()
                    .map_err(|_| HarnessError::execution("tool registry lock poisoned"))?;
                (
                    state
                        .tools
                        .values()
                        .map(|tool| tool.spec.clone())
                        .collect::<Vec<_>>(),
                    state
                        .presenter
                        .as_ref()
                        .map(|(_, presenter)| Arc::clone(presenter)),
                )
            };
            if !crate::agent_goal::goal_tool_enabled(&self.sessions.events().await) {
                tools.retain(|tool| tool.name != "update_goal");
            }
            tools.sort_by(|left, right| left.name.cmp(&right.name));
            match presenter {
                Some(presenter) => presenter.present(tools).await,
                None => Ok(ToolPresentation::native(tools)),
            }
        })
    }

    fn describe<'a>(
        &'a self,
        _: CallContext<()>,
        name: String,
    ) -> Pin<Box<dyn Future<Output = Option<ToolPresentationDescriptor>> + Send + 'a>> {
        Box::pin(async move {
            let state = self.state.lock().expect("tool registry lock poisoned");
            let registration = state.names.get(&name)?;
            state.tools.get(registration)?.handler.presentation()
        })
    }

    fn execute<'a>(
        &'a self,
        _: CallContext<()>,
        run_id: RunId,
        mut call: ToolCall,
        cancellation: ternilo_kernel::RunCancellation,
        activity: ActivityBranch,
    ) -> Pin<Box<dyn Future<Output = Result<ToolOutput, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            activity.ensure_running().await?;
            if call.name == "update_goal"
                && !crate::agent_goal::goal_tool_enabled(&self.sessions.events().await)
            {
                return Err(HarnessError::policy(
                    "goals must be enabled by the user with /goal before update_goal can be used",
                ));
            }
            self.prepare_sources(cancellation.clone()).await?;
            let identity = self.environment.identity().await;
            let workspace = self.environment.workspace().await;
            let context = ToolExecutionContext {
                identity,
                workspace,
                run_id: run_id.clone(),
                call_id: call.id.clone(),
                cancellation,
                activity: activity.clone(),
            };
            let (handler, effect, guards) = {
                let state = self
                    .state
                    .lock()
                    .map_err(|_| HarnessError::execution("tool registry lock poisoned"))?;
                let registration = state.names.get(&call.name).ok_or_else(|| {
                    HarnessError::execution(format!("model requested unknown tool {:?}", call.name))
                })?;
                let tool = &state.tools[registration];
                let handler = Arc::clone(&tool.handler);
                let effect = tool.effect;
                let guards = state.guards.values().cloned().collect::<Vec<_>>();
                (handler, effect, guards)
            };
            call.presentation = handler.presentation();
            let authorization = self.environment.check_tool(call.clone(), effect).await?;
            for guard in guards {
                if let Some(reason) = guard.deny(&context, &call) {
                    return Err(HarnessError::policy(reason));
                }
            }
            if let ToolAuthorization::Ask { reason } = authorization {
                let reason = handler.approval_reason(&call.arguments).unwrap_or(reason);
                self.request_approval(run_id, &call, reason).await?;
            }
            let result =
                tokio::time::timeout(self.timeout, handler.execute(context, call.arguments)).await;
            // Dropping a timed-out handler can end a dependency wait. Restore admission
            // before its error reaches any caller that could continue execution.
            activity.ensure_running().await?;
            result.map_err(|_| {
                HarnessError::execution(format!(
                    "tool {:?} exceeded its {} ms execution timeout",
                    call.name,
                    self.timeout.as_millis()
                ))
            })?
        })
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, sync::Arc};

    use serde_json::{Value, json};
    use ternilo_kernel::{CommandRegistration, CommandResolver};
    use ternilo_protocol::{CommandDescriptor, CommandInputDescriptor, HarnessError, ToolSpec};

    use super::CommandRegistry;

    struct RequestResolver;

    impl CommandResolver for RequestResolver {
        fn resolve(&self, input: &str) -> Result<Value, HarnessError> {
            Ok(json!({ "request": input }))
        }
    }

    struct InvalidResolver;

    impl CommandResolver for InvalidResolver {
        fn resolve(&self, _: &str) -> Result<Value, HarnessError> {
            Ok(json!({ "request": 7 }))
        }
    }

    fn registration(
        name: &str,
        tool_name: &str,
        resolver: Arc<dyn CommandResolver>,
    ) -> CommandRegistration {
        CommandRegistration {
            descriptor: CommandDescriptor {
                name: name.to_owned(),
                description: format!("Run {name}"),
                input: Some(CommandInputDescriptor {
                    hint: "<request>".to_owned(),
                    images: false,
                }),
            },
            tool_name: tool_name.to_owned(),
            resolver,
        }
    }

    fn tools() -> BTreeMap<String, ToolSpec> {
        BTreeMap::from([(
            "workspace_review".to_owned(),
            ToolSpec {
                name: "workspace_review".to_owned(),
                description: "Review the workspace".to_owned(),
                input_schema: json!({
                    "type": "object",
                    "properties": { "request": { "type": "string" } },
                    "required": ["request"],
                    "additionalProperties": false
                }),
            },
        )])
    }

    #[test]
    fn command_registry_rejects_conflicts_and_typed_routes() {
        let registry = CommandRegistry::default();
        registry
            .register(registration(
                "review",
                "workspace_review",
                Arc::new(RequestResolver),
            ))
            .unwrap();
        let duplicate = registry
            .register(registration(
                "review",
                "workspace_review",
                Arc::new(RequestResolver),
            ))
            .unwrap_err();
        assert!(duplicate.message.contains("already registered"));

        for name in ["feedback", "plan", "skill"] {
            let error = registry
                .register(registration(
                    name,
                    "workspace_review",
                    Arc::new(RequestResolver),
                ))
                .unwrap_err();
            assert!(error.message.contains("typed Session route"), "{name}");
        }
    }

    #[test]
    fn command_registry_catalog_resolve_and_cleanup_share_one_registration() {
        let registry = CommandRegistry::default();
        let id = registry
            .register(registration(
                "review",
                "workspace_review",
                Arc::new(RequestResolver),
            ))
            .unwrap();
        assert!(registry.catalog(&BTreeMap::new()).is_empty());
        let unavailable = registry
            .resolve("/review inspect src", &BTreeMap::new())
            .expect("registered command")
            .result
            .unwrap_err();
        assert!(
            unavailable
                .message
                .contains("is not enabled for this session")
        );
        let tools = tools();
        let catalog = registry.catalog(&tools);
        assert_eq!(catalog.len(), 1);
        assert_eq!(catalog[0].descriptor.name, "review");
        assert_eq!(catalog[0].tool_name, "workspace_review");

        let resolution = registry
            .resolve("/review inspect src", &tools)
            .expect("registered command");
        assert_eq!(resolution.command_name, "review");
        let resolved = resolution.result.unwrap();
        assert_eq!(resolved.tool_name, "workspace_review");
        assert_eq!(resolved.arguments, json!({ "request": "inspect src" }));

        registry.unregister(id).unwrap();
        assert!(registry.catalog(&tools).is_empty());
        assert!(registry.resolve("/review inspect src", &tools).is_none());
        assert!(
            registry
                .unregister(id)
                .unwrap_err()
                .message
                .contains("unknown")
        );
    }

    #[test]
    fn command_registry_validates_resolved_arguments_against_the_tool_schema() {
        let registry = CommandRegistry::default();
        registry
            .register(registration(
                "review",
                "workspace_review",
                Arc::new(InvalidResolver),
            ))
            .unwrap();
        let resolution = registry
            .resolve("/review inspect src", &tools())
            .expect("registered command");
        let error = resolution.result.unwrap_err();
        assert!(error.message.contains("target tool input schema"));
    }
}
