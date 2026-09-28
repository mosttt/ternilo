use std::{future::Future, pin::Pin, sync::Arc};

use linorun_core::{Activation, CleanupError, ComponentContext, ComponentDescriptor, effect};
use linorun_macros::component_descriptor;
use serde::Deserialize;
use serde_json::{Value, json};
use ternilo_kernel::{
    HarnessPlugin, PluginFactory, PluginManifest, Terminals, TerminalsClient, ToolExecutionContext,
    ToolHandler, ToolRegistration, Tools, ToolsClient,
};
use ternilo_protocol::{HarnessError, TerminalId, ToolOutput, ToolSpec};

use crate::{EmptyConfig, factory as make_factory, parse_config};

pub const KIND: &str = "ternilo.tools.terminal";

component_descriptor! {
    static DESCRIPTOR: () {
        id: "ternilo/builtin-terminal-tools@1",
        requires: [Tools, Terminals],
        provides: [],
    }
}

pub fn factory() -> PluginFactory {
    make_factory(
        PluginManifest {
            kind: KIND,
            requires: &["ternilo/tools@1", "ternilo/terminals@1"],
            provides: &[],
        },
        |value| {
            let _: EmptyConfig = parse_config(value)?;
            Ok(Arc::new(TerminalToolsPlugin))
        },
    )
    .with_config_schema::<EmptyConfig>()
}

struct TerminalToolsPlugin;

impl HarnessPlugin for TerminalToolsPlugin {
    fn descriptor(&self) -> &'static ComponentDescriptor {
        &DESCRIPTOR
    }

    fn activate(&self, context: ComponentContext) -> Activation {
        let tools = context
            .context()
            .service::<Tools>()
            .expect("terminal tools declare Tools");
        let terminals = context
            .context()
            .service::<Terminals>()
            .expect("terminal tools declare Terminals");
        Activation::Once(Box::pin(async move {
            let registrations = register_tools(&tools, terminals)
                .await
                .map_err(|error| linorun_core::ActivationFailure::user(error.to_string()))?;
            Ok(Some(effect::inverse(move || async move {
                for registration in registrations.into_iter().rev() {
                    tools
                        .unregister_tool(registration)
                        .await
                        .map_err(|error| CleanupError::user(error.to_string()))?;
                }
                Ok(())
            })))
        }))
    }
}

#[derive(Clone, Copy)]
enum Operation {
    Open,
    Send,
    Read,
    Signal,
    Close,
    List,
}

impl Operation {
    const fn effect(self) -> ternilo_kernel::ToolEffect {
        match self {
            Self::Read | Self::List => ternilo_kernel::ToolEffect::ReadOnly,
            Self::Open | Self::Send | Self::Signal | Self::Close => {
                ternilo_kernel::ToolEffect::Mutating
            }
        }
    }
}

struct TerminalTool {
    terminals: TerminalsClient,
    operation: Operation,
}

async fn register_tools(
    tools: &ToolsClient,
    terminals: TerminalsClient,
) -> Result<Vec<u64>, HarnessError> {
    let definitions = [
        (
            ToolSpec {
                name: "terminal_open".to_owned(),
                description: "Open a persistent sandboxed platform shell for commands that need retained shell state or interactive stdin.".to_owned(),
                input_schema: json!({
                    "type": "object",
                    "properties": { "name": { "type": "string" } },
                    "additionalProperties": false
                }),
            },
            Operation::Open,
        ),
        (
            ToolSpec {
                name: "terminal_send".to_owned(),
                description: "Send shell input to a persistent terminal and wait for completion or the wait bound.".to_owned(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "terminal_id": { "type": "string" },
                        "input": { "type": "string" },
                        "wait_ms": { "type": "integer", "minimum": 100, "maximum": 300_000, "default": 30000 }
                    },
                    "required": ["terminal_id", "input"],
                    "additionalProperties": false
                }),
            },
            Operation::Send,
        ),
        (
            ToolSpec {
                name: "terminal_read".to_owned(),
                description: "Read retained terminal output from a byte offset without sending input.".to_owned(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "terminal_id": { "type": "string" },
                        "offset": { "type": "integer", "minimum": 0, "default": 0 }
                    },
                    "required": ["terminal_id"],
                    "additionalProperties": false
                }),
            },
            Operation::Read,
        ),
        (
            ToolSpec {
                name: "terminal_signal".to_owned(),
                description: "Send interrupt or terminate to a persistent terminal process.".to_owned(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "terminal_id": { "type": "string" },
                        "signal": { "enum": ["interrupt", "terminate"] }
                    },
                    "required": ["terminal_id", "signal"],
                    "additionalProperties": false
                }),
            },
            Operation::Signal,
        ),
        (
            ToolSpec {
                name: "terminal_close".to_owned(),
                description: "Close a persistent terminal and wait for its process to exit.".to_owned(),
                input_schema: id_schema(),
            },
            Operation::Close,
        ),
        (
            ToolSpec {
                name: "terminal_list".to_owned(),
                description: "List persistent terminals owned by the current session.".to_owned(),
                input_schema: json!({
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false
                }),
            },
            Operation::List,
        ),
    ];
    let mut registrations = Vec::new();
    for (spec, operation) in definitions {
        registrations
            .push(register_terminal_tool(tools, terminals.clone(), spec, operation).await?);
    }
    Ok(registrations)
}

async fn register_terminal_tool(
    tools: &ToolsClient,
    terminals: TerminalsClient,
    spec: ToolSpec,
    operation: Operation,
) -> Result<u64, HarnessError> {
    tools
        .register_tool(ToolRegistration {
            spec,
            effect: operation.effect(),
            handler: Arc::new(TerminalTool {
                terminals,
                operation,
            }),
        })
        .await
}

impl ToolHandler for TerminalTool {
    fn execute<'a>(
        &'a self,
        _: ToolExecutionContext,
        arguments: Value,
    ) -> Pin<Box<dyn Future<Output = Result<ToolOutput, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            let result = match self.operation {
                Operation::Open => {
                    let arguments: OpenArguments = parse_arguments(arguments)?;
                    serde_json::to_value(self.terminals.open(arguments.name).await?)
                }
                Operation::Send => {
                    let arguments: SendArguments = parse_arguments(arguments)?;
                    serde_json::to_value(
                        self.terminals
                            .send(
                                TerminalId::new(arguments.terminal_id),
                                arguments.input,
                                arguments.wait_ms,
                            )
                            .await?,
                    )
                }
                Operation::Read => {
                    let arguments: ReadArguments = parse_arguments(arguments)?;
                    serde_json::to_value(
                        self.terminals
                            .read(TerminalId::new(arguments.terminal_id), arguments.offset)
                            .await?,
                    )
                }
                Operation::Signal => {
                    let arguments: SignalArguments = parse_arguments(arguments)?;
                    serde_json::to_value(
                        self.terminals
                            .signal(TerminalId::new(arguments.terminal_id), arguments.signal)
                            .await?,
                    )
                }
                Operation::Close => {
                    let arguments: IdArguments = parse_arguments(arguments)?;
                    serde_json::to_value(
                        self.terminals
                            .close(TerminalId::new(arguments.terminal_id))
                            .await?,
                    )
                }
                Operation::List => {
                    let _: EmptyArguments = parse_arguments(arguments)?;
                    serde_json::to_value(self.terminals.list().await)
                }
            }
            .map_err(|error| {
                HarnessError::execution(format!("serialize terminal result: {error}"))
            })?;
            Ok(ToolOutput {
                content: serde_json::to_string_pretty(&result).map_err(|error| {
                    HarnessError::execution(format!("render terminal result: {error}"))
                })?,
                is_error: false,
            })
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenArguments {
    name: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SendArguments {
    terminal_id: String,
    input: String,
    #[serde(default = "default_wait_ms")]
    wait_ms: u64,
}

const fn default_wait_ms() -> u64 {
    30_000
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadArguments {
    terminal_id: String,
    #[serde(default)]
    offset: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SignalArguments {
    terminal_id: String,
    signal: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IdArguments {
    terminal_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyArguments {}

fn parse_arguments<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, HarnessError> {
    serde_json::from_value(value)
        .map_err(|error| HarnessError::invalid(format!("invalid terminal arguments: {error}")))
}

fn id_schema() -> Value {
    json!({
        "type": "object",
        "properties": { "terminal_id": { "type": "string" } },
        "required": ["terminal_id"],
        "additionalProperties": false
    })
}
