use std::{future::Future, pin::Pin, sync::Arc};

use linorun_core::{Activation, CleanupError, ComponentContext, ComponentDescriptor, effect};
use linorun_macros::component_descriptor;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use ternilo_kernel::{
    HarnessPlugin, PluginFactory, PluginManifest, Prompts, RuntimeExtensions,
    RuntimeExtensionsClient, Sessions, SessionsClient, ToolEffect, ToolExecutionContext,
    ToolHandler, ToolRegistration, Tools, ToolsClient,
};
#[cfg(test)]
use ternilo_protocol::UserQuestion;
use ternilo_protocol::{
    HarnessError, PromptSection, RuntimeExtensionAction, SessionEventKind, ToolOutput, ToolSpec,
};

use crate::{EmptyConfig, factory as make_factory, parse_config};

pub const KIND: &str = "ternilo.tools.runtime_extensions";

component_descriptor! {
    static DESCRIPTOR: () {
        id: "ternilo/runtime-extension-tools@1",
        requires: [Tools, Prompts, Sessions, RuntimeExtensions],
        provides: [],
    }
}

pub fn factory() -> PluginFactory {
    make_factory(
        PluginManifest {
            kind: KIND,
            requires: &[
                "ternilo/tools@1",
                "ternilo/prompts@1",
                "ternilo/sessions@1",
                "ternilo/runtime-extensions@1",
            ],
            provides: &[],
        },
        |value| {
            let _: EmptyConfig = parse_config(value)?;
            Ok(Arc::new(RuntimeExtensionToolsPlugin))
        },
    )
    .with_config_schema::<EmptyConfig>()
}

struct RuntimeExtensionToolsPlugin;

impl HarnessPlugin for RuntimeExtensionToolsPlugin {
    fn descriptor(&self) -> &'static ComponentDescriptor {
        &DESCRIPTOR
    }

    fn activate(&self, context: ComponentContext) -> Activation {
        let tools = context
            .context()
            .service::<Tools>()
            .expect("runtime extension tools declare Tools");
        let prompts = context
            .context()
            .service::<Prompts>()
            .expect("runtime extension tools declare Prompts");
        let sessions = context
            .context()
            .service::<Sessions>()
            .expect("runtime extension tools declare Sessions");
        let extensions = context
            .context()
            .service::<RuntimeExtensions>()
            .expect("runtime extension tools declare RuntimeExtensions");
        Activation::Once(Box::pin(async move {
            let prompt = prompts
                .register(PromptSection {
                    id: "runtime-extension-tools".to_owned(),
                    order: 430,
                    content: EXTENSION_PROMPT.to_owned(),
                })
                .await
                .map_err(|error| linorun_core::ActivationFailure::user(error.to_string()))?;
            let shared = ExtensionToolContext {
                tools: tools.clone(),
                sessions,
                extensions,
            };
            let registrations = [
                (
                    inspect_spec(),
                    ToolEffect::ReadOnly,
                    ExtensionOperation::Inspect,
                ),
                (
                    set_enabled_spec(),
                    ToolEffect::Dangerous,
                    ExtensionOperation::SetEnabled,
                ),
                (
                    set_mounted_spec(),
                    ToolEffect::Dangerous,
                    ExtensionOperation::SetMounted,
                ),
                (
                    revoke_spec(),
                    ToolEffect::Dangerous,
                    ExtensionOperation::Revoke,
                ),
            ];
            let mut registered = Vec::new();
            for (spec, effect_class, operation) in registrations {
                match tools
                    .register_tool(ToolRegistration {
                        spec,
                        effect: effect_class,
                        handler: Arc::new(ExtensionTool {
                            shared: shared.clone(),
                            operation,
                        }),
                    })
                    .await
                {
                    Ok(registration) => registered.push(registration),
                    Err(error) => {
                        for registration in registered.into_iter().rev() {
                            let _ = tools.unregister_tool(registration).await;
                        }
                        let _ = prompts.unregister(prompt).await;
                        return Err(linorun_core::ActivationFailure::user(error.to_string()));
                    }
                }
            }
            Ok(Some(effect::inverse(move || async move {
                for registration in registered.into_iter().rev() {
                    tools
                        .unregister_tool(registration)
                        .await
                        .map_err(|error| CleanupError::user(error.to_string()))?;
                }
                prompts
                    .unregister(prompt)
                    .await
                    .map_err(|error| CleanupError::user(error.to_string()))
            })))
        }))
    }
}

const EXTENSION_PROMPT: &str = r"# Runtime extensions

Use `extension_inspect` before reasoning about the live catalog or signed Extension Packages. Each package selects exactly one `rhai` or `wasm-component` runtime and may contribute multiple tools. `extension_set_enabled`, `extension_set_mounted`, and `extension_revoke` are dangerous tools routed through the shared one-time approval queue; do not claim that a change happened until the tool returns. Mount changes update this session profile and become active on the next turn. Revocation is permanent for that exact signed package version.";

#[derive(Clone)]
struct ExtensionToolContext {
    tools: ToolsClient,
    sessions: SessionsClient,
    extensions: RuntimeExtensionsClient,
}

#[derive(Clone, Copy)]
enum ExtensionOperation {
    Inspect,
    SetEnabled,
    SetMounted,
    Revoke,
}

struct ExtensionTool {
    shared: ExtensionToolContext,
    operation: ExtensionOperation,
}

impl ToolHandler for ExtensionTool {
    fn approval_reason(&self, arguments: &Value) -> Option<String> {
        let package = arguments
            .get("package_id")
            .and_then(Value::as_str)
            .unwrap_or("unknown package");
        let version = arguments
            .get("version")
            .and_then(Value::as_str)
            .unwrap_or("unknown version");
        let stated_reason = arguments
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or("no reason supplied");
        let action = match self.operation {
            ExtensionOperation::Inspect => return None,
            ExtensionOperation::SetEnabled => {
                if arguments
                    .get("enabled")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    "enable"
                } else {
                    "disable"
                }
            }
            ExtensionOperation::SetMounted => {
                if arguments
                    .get("mounted")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    "mount in this session"
                } else {
                    "unmount from this session"
                }
            }
            ExtensionOperation::Revoke => "permanently revoke",
        };
        Some(format!(
            "the agent requests permission to {action} signed extension {package}@{version}; stated reason: {stated_reason}"
        ))
    }

    fn execute<'a>(
        &'a self,
        context: ToolExecutionContext,
        arguments: Value,
    ) -> Pin<Box<dyn Future<Output = Result<ToolOutput, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            match self.operation {
                ExtensionOperation::Inspect => self.inspect().await,
                ExtensionOperation::SetEnabled => self.set_enabled(context, arguments).await,
                ExtensionOperation::SetMounted => self.set_mounted(context, arguments).await,
                ExtensionOperation::Revoke => self.revoke(context, arguments).await,
            }
        })
    }
}

impl ExtensionTool {
    async fn inspect(&self) -> Result<ToolOutput, HarnessError> {
        let mut report = self.shared.extensions.inspect().await?;
        let active_tools = self.shared.tools.list().await;
        let active_names = active_tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        if let Some(extensions) = report.get_mut("extensions").and_then(Value::as_array_mut) {
            for extension in extensions {
                if let Some(object) = extension.as_object_mut() {
                    let mounted = object
                        .get("manifest")
                        .and_then(|manifest| manifest.get("contributions"))
                        .and_then(|contributions| contributions.get("tools"))
                        .and_then(Value::as_array)
                        .is_some_and(|tools| {
                            tools.iter().any(|tool| {
                                tool.get("spec")
                                    .and_then(|spec| spec.get("name"))
                                    .and_then(Value::as_str)
                                    .is_some_and(|name| active_names.contains(name))
                            })
                        });
                    object.insert("mounted_in_session".to_owned(), Value::Bool(mounted));
                }
            }
        }
        if let Some(object) = report.as_object_mut() {
            object.insert(
                "active_tools".to_owned(),
                serde_json::to_value(active_tools).map_err(|error| {
                    HarnessError::execution(format!("encode active tool catalog: {error}"))
                })?,
            );
        }
        json_output(&report)
    }

    async fn set_enabled(
        &self,
        context: ToolExecutionContext,
        arguments: Value,
    ) -> Result<ToolOutput, HarnessError> {
        let arguments: SetEnabledArguments = decode_arguments(arguments)?;
        validate_reason(&arguments.reason)?;
        let value = self
            .shared
            .extensions
            .set_enabled(
                arguments.package_id.clone(),
                arguments.version.clone(),
                arguments.enabled,
            )
            .await?;
        self.shared
            .sessions
            .append(
                context.run_id,
                SessionEventKind::RuntimeExtensionChanged {
                    package_id: arguments.package_id,
                    version: arguments.version,
                    action: if arguments.enabled {
                        RuntimeExtensionAction::Enabled
                    } else {
                        RuntimeExtensionAction::Disabled
                    },
                },
            )
            .await?;
        json_output(&value)
    }

    async fn revoke(
        &self,
        context: ToolExecutionContext,
        arguments: Value,
    ) -> Result<ToolOutput, HarnessError> {
        let arguments: RevokeArguments = decode_arguments(arguments)?;
        validate_reason(&arguments.reason)?;
        let value = self
            .shared
            .extensions
            .revoke(arguments.package_id.clone(), arguments.version.clone())
            .await?;
        self.shared
            .sessions
            .append(
                context.run_id,
                SessionEventKind::RuntimeExtensionChanged {
                    package_id: arguments.package_id,
                    version: arguments.version,
                    action: RuntimeExtensionAction::Revoked,
                },
            )
            .await?;
        json_output(&value)
    }

    async fn set_mounted(
        &self,
        context: ToolExecutionContext,
        arguments: Value,
    ) -> Result<ToolOutput, HarnessError> {
        let arguments: SetMountedArguments = decode_arguments(arguments)?;
        validate_reason(&arguments.reason)?;
        let value = self
            .shared
            .extensions
            .set_mounted(
                context.identity.session_id.as_str().to_owned(),
                arguments.package_id.clone(),
                arguments.version.clone(),
                arguments.mounted,
                arguments.settings,
            )
            .await?;
        self.shared
            .sessions
            .append(
                context.run_id,
                SessionEventKind::RuntimeExtensionChanged {
                    package_id: arguments.package_id,
                    version: arguments.version,
                    action: if arguments.mounted {
                        RuntimeExtensionAction::Mounted
                    } else {
                        RuntimeExtensionAction::Unmounted
                    },
                },
            )
            .await?;
        json_output(&value)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetEnabledArguments {
    package_id: String,
    version: String,
    enabled: bool,
    reason: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RevokeArguments {
    package_id: String,
    version: String,
    reason: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetMountedArguments {
    package_id: String,
    version: String,
    mounted: bool,
    settings: Value,
    reason: String,
}

fn decode_arguments<T: for<'de> Deserialize<'de>>(arguments: Value) -> Result<T, HarnessError> {
    serde_json::from_value(arguments)
        .map_err(|error| HarnessError::invalid(format!("invalid extension arguments: {error}")))
}

fn validate_reason(reason: &str) -> Result<(), HarnessError> {
    let count = reason.trim().chars().count();
    if count == 0 || count > 500 {
        Err(HarnessError::invalid(
            "extension change reason must contain 1 to 500 characters",
        ))
    } else {
        Ok(())
    }
}

fn json_output(value: &Value) -> Result<ToolOutput, HarnessError> {
    Ok(ToolOutput {
        content: serde_json::to_string_pretty(&value).map_err(|error| {
            HarnessError::execution(format!("encode extension tool output: {error}"))
        })?,
        is_error: false,
    })
}

fn inspect_spec() -> ToolSpec {
    ToolSpec {
        name: "extension_inspect".to_owned(),
        description: "Inspect the live trusted plugin catalog, active tool schemas, publisher trust, and installed signed Extension Package state. This is read-only and should be called before changing an extension.".to_owned(),
        input_schema: empty_object_schema(),
    }
}

fn set_enabled_spec() -> ToolSpec {
    ToolSpec {
        name: "extension_set_enabled".to_owned(),
        description: "Request explicit user approval to enable or disable an installed signed Extension Package version. Enabling affects an already-mounted package immediately; it does not attach an unmounted package to the session profile.".to_owned(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "package_id": { "type": "string", "minLength": 1 },
                "version": { "type": "string", "minLength": 1 },
                "enabled": { "type": "boolean" },
                "reason": { "type": "string", "minLength": 1, "maxLength": 500 }
            },
            "required": ["package_id", "version", "enabled", "reason"],
            "additionalProperties": false
        }),
    }
}

fn revoke_spec() -> ToolSpec {
    ToolSpec {
        name: "extension_revoke".to_owned(),
        description: "Request explicit user approval to permanently revoke an installed signed Extension Package version. Revocation is irreversible for that package id and version.".to_owned(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "package_id": { "type": "string", "minLength": 1 },
                "version": { "type": "string", "minLength": 1 },
                "reason": { "type": "string", "minLength": 1, "maxLength": 500 }
            },
            "required": ["package_id", "version", "reason"],
            "additionalProperties": false
        }),
    }
}

fn set_mounted_spec() -> ToolSpec {
    ToolSpec {
        name: "extension_set_mounted".to_owned(),
        description: "Request explicit user approval to mount or unmount an installed signed Extension Package in the current session profile. Pass settings that satisfy the signed config_schema; use an empty object when unmounting. The runtime restarts before the next turn so the contributed tool graph changes atomically.".to_owned(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "package_id": { "type": "string", "minLength": 1 },
                "version": { "type": "string", "minLength": 1 },
                "mounted": { "type": "boolean" },
                "settings": { "type": "object" },
                "reason": { "type": "string", "minLength": 1, "maxLength": 500 }
            },
            "required": ["package_id", "version", "mounted", "settings", "reason"],
            "additionalProperties": false
        }),
    }
}

fn empty_object_schema() -> Value {
    Value::Object(Map::from_iter([
        ("type".to_owned(), Value::String("object".to_owned())),
        ("properties".to_owned(), Value::Object(Map::new())),
        ("additionalProperties".to_owned(), Value::Bool(false)),
    ]))
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use linorun_core::CallContext;
    use ternilo_kernel::{
        HarnessSession, HostEnvironment, HostPolicy, MemoryEventStore, RuntimeExtensionsProvider,
        UserInteraction,
    };
    use ternilo_protocol::{
        AgentId, RunId, RunLimits, SessionId, SessionIdentity, TenantId, UserAnswer, UserId,
    };

    use super::*;

    #[derive(Default)]
    struct FixtureExtensions {
        changes: Mutex<Vec<(String, String, bool)>>,
    }

    impl RuntimeExtensionsProvider for FixtureExtensions {
        fn inspect<'a>(
            &'a self,
            _: CallContext<()>,
        ) -> Pin<Box<dyn Future<Output = Result<Value, HarnessError>> + Send + 'a>> {
            Box::pin(async {
                Ok(json!({
                    "supported": true,
                    "catalog_revision": "fixture-v1",
                    "catalog": [],
                    "publishers": [],
                    "extensions": [{
                        "manifest": {
                            "package_id": "dev.fixture",
                            "version": "1.0.0",
                            "runtime": { "kind": "rhai" },
                            "contributions": {
                                "tools": [{
                                    "spec": { "name": "fixture_tool" }
                                }],
                                "prompt_sections": [],
                                "skills": [],
                                "hooks": [],
                                "commands": [],
                                "providers": []
                            }
                        },
                        "enabled": true,
                        "revoked": false
                    }]
                }))
            })
        }

        fn set_enabled<'a>(
            &'a self,
            _: CallContext<()>,
            package_id: String,
            version: String,
            enabled: bool,
        ) -> Pin<Box<dyn Future<Output = Result<Value, HarnessError>> + Send + 'a>> {
            Box::pin(async move {
                self.changes
                    .lock()
                    .unwrap()
                    .push((package_id.clone(), version.clone(), enabled));
                Ok(json!({
                    "package_id": package_id,
                    "version": version,
                    "enabled": enabled
                }))
            })
        }

        fn revoke<'a>(
            &'a self,
            _: CallContext<()>,
            package_id: String,
            version: String,
        ) -> Pin<Box<dyn Future<Output = Result<Value, HarnessError>> + Send + 'a>> {
            Box::pin(async move {
                Ok(json!({
                    "package_id": package_id,
                    "version": version,
                    "revoked": true
                }))
            })
        }

        fn set_mounted<'a>(
            &'a self,
            _: CallContext<()>,
            session_id: String,
            package_id: String,
            version: String,
            mounted: bool,
            _: Value,
        ) -> Pin<Box<dyn Future<Output = Result<Value, HarnessError>> + Send + 'a>> {
            Box::pin(async move {
                Ok(json!({
                    "session_id": session_id,
                    "package_id": package_id,
                    "version": version,
                    "mounted": mounted,
                    "effective": "next_turn"
                }))
            })
        }
    }

    #[derive(Default)]
    struct ApprovingInteraction {
        questions: Mutex<Vec<UserQuestion>>,
    }

    impl UserInteraction for ApprovingInteraction {
        fn ask<'a>(
            &'a self,
            question: UserQuestion,
        ) -> Pin<Box<dyn Future<Output = Result<UserAnswer, HarnessError>> + Send + 'a>> {
            Box::pin(async move {
                let answer = question.options.first().unwrap().label.clone();
                self.questions.lock().unwrap().push(question.clone());
                Ok(UserAnswer {
                    question_id: question.id,
                    selected: vec![answer],
                    custom: None,
                })
            })
        }
    }

    #[derive(Default)]
    struct RejectingInteraction {
        questions: Mutex<Vec<UserQuestion>>,
    }

    impl UserInteraction for RejectingInteraction {
        fn ask<'a>(
            &'a self,
            question: UserQuestion,
        ) -> Pin<Box<dyn Future<Output = Result<UserAnswer, HarnessError>> + Send + 'a>> {
            Box::pin(async move {
                self.questions.lock().unwrap().push(question.clone());
                Ok(UserAnswer {
                    question_id: question.id,
                    selected: vec!["Deny".to_owned()],
                    custom: None,
                })
            })
        }
    }

    #[tokio::test]
    async fn inspection_and_approved_mutation_use_the_host_extension_service() {
        let extensions = Arc::new(FixtureExtensions::default());
        let interaction = Arc::new(ApprovingInteraction::default());
        let extension_service: Arc<dyn RuntimeExtensionsProvider> = extensions.clone();
        let user_interaction: Arc<dyn UserInteraction> = interaction.clone();
        let identity = SessionIdentity {
            tenant_id: TenantId::new("tenant"),
            user_id: UserId::new("user"),
            agent_id: AgentId::new("agent"),
            session_id: SessionId::new("session"),
        };
        let environment = HostEnvironment::with_interaction(
            identity,
            None,
            HostPolicy::local(RunLimits::default()),
            Arc::new(MemoryEventStore::default()),
            user_interaction,
        )
        .with_runtime_extensions(extension_service);
        let harness = HarnessSession::boot(
            &crate::catalog().unwrap(),
            &crate::local_profile(),
            environment,
        )
        .await
        .unwrap();

        let inspection = harness
            .run(RunId::new("inspect"), "/extensions")
            .await
            .unwrap();
        assert!(inspection.answer.contains("fixture-v1"));
        assert!(inspection.answer.contains("mounted_in_session"));
        let changed = harness
            .run(
                RunId::new("disable"),
                "/extension-disable dev.fixture 1.0.0",
            )
            .await
            .unwrap();
        assert!(changed.answer.contains("\"enabled\": false"));
        assert_eq!(
            *extensions.changes.lock().unwrap(),
            vec![("dev.fixture".to_owned(), "1.0.0".to_owned(), false)]
        );
        {
            let questions = interaction.questions.lock().unwrap();
            assert_eq!(questions.len(), 1);
            let approval = questions[0].tool_approval.as_ref().unwrap();
            assert_eq!(approval.tool_name, "extension_set_enabled");
            assert_eq!(approval.arguments["package_id"], "dev.fixture");
        }
        let events = harness.events().await;
        assert!(
            events
                .iter()
                .any(|event| matches!(event.kind, SessionEventKind::UserQuestionAsked { .. }))
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event.kind, SessionEventKind::UserQuestionAnswered { .. }))
        );
        assert!(events.iter().any(|event| matches!(
            event.kind,
            SessionEventKind::RuntimeExtensionChanged {
                action: RuntimeExtensionAction::Disabled,
                ..
            }
        )));
        harness.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn rejected_dangerous_tool_is_audited_without_running_the_handler() {
        let extensions = Arc::new(FixtureExtensions::default());
        let interaction = Arc::new(RejectingInteraction::default());
        let identity = SessionIdentity {
            tenant_id: TenantId::new("tenant"),
            user_id: UserId::new("user"),
            agent_id: AgentId::new("agent"),
            session_id: SessionId::new("session"),
        };
        let environment = HostEnvironment::with_interaction(
            identity,
            None,
            HostPolicy::local(RunLimits::default()),
            Arc::new(MemoryEventStore::default()),
            interaction.clone(),
        )
        .with_runtime_extensions(extensions.clone());
        let harness = HarnessSession::boot(
            &crate::catalog().unwrap(),
            &crate::local_profile(),
            environment,
        )
        .await
        .unwrap();

        let outcome = harness
            .run(
                RunId::new("reject-disable"),
                "/extension-disable dev.fixture 1.0.0",
            )
            .await
            .unwrap();
        assert!(outcome.answer.contains("denied one-time approval"));
        assert!(extensions.changes.lock().unwrap().is_empty());
        {
            let questions = interaction.questions.lock().unwrap();
            assert_eq!(questions.len(), 1);
            assert_eq!(
                questions[0]
                    .tool_approval
                    .as_ref()
                    .map(|approval| approval.tool_name.as_str()),
                Some("extension_set_enabled")
            );
        }
        assert!(harness.events().await.iter().any(|event| matches!(
            &event.kind,
            SessionEventKind::UserQuestionAnswered { answer } if answer.chose("Deny")
        )));
        harness.shutdown().await.unwrap();
    }
}
