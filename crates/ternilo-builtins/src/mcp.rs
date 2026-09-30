use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    io,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use linorun_core::{
    Activation, ActivationFailure, CleanupError, ComponentContext, ComponentDescriptor, effect,
};
use linorun_macros::component_descriptor;
use rmcp::{
    RoleClient, ServiceExt,
    model::{CallToolRequestParams, ContentBlock},
    service::{RunningService, RxJsonRpcMessage, TxJsonRpcMessage},
    transport::{Transport, async_rw::AsyncRwTransport},
};
use serde::Deserialize;
use serde_json::Value;
use ternilo_kernel::{
    HarnessPlugin, PluginFactory, PluginManifest, RunEnvironment, RunEnvironmentClient,
    ToolExecutionContext, ToolHandler, Tools, WorkspaceExecutionLease,
};
use ternilo_protocol::{HarnessError, ToolOutput, ToolSpec};
use tokio::process::{ChildStdin, ChildStdout, Command};

use crate::{
    factory as make_factory, parse_config,
    process_group::{ManagedProcess, OwnedProcessGroup, ProcessControl},
};

pub const KIND: &str = "ternilo.mcp.stdio";

mod source;

#[cfg(all(test, unix))]
#[path = "mcp_lifetime_tests.rs"]
mod lifetime_tests;

component_descriptor! {
    static DESCRIPTOR: () {
        id: "ternilo/builtin-mcp-stdio@1",
        requires: [Tools, RunEnvironment],
        provides: [],
    }
}

#[derive(Clone, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct McpConfig {
    server_name: String,
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    env_refs: BTreeMap<String, String>,
    cwd: Option<String>,
    #[serde(default = "default_startup_timeout")]
    startup_timeout_ms: u64,
    #[serde(default = "default_call_timeout")]
    tool_call_timeout_ms: u64,
    #[serde(default)]
    #[schemars(range(max = 10))]
    reconnect_attempts: u32,
}

const fn default_startup_timeout() -> u64 {
    15_000
}

const fn default_call_timeout() -> u64 {
    60_000
}

pub fn factory() -> PluginFactory {
    make_factory(
        PluginManifest {
            kind: KIND,
            requires: &["ternilo/tools@1", "ternilo/run-environment@1"],
            provides: &[],
        },
        |value| {
            let config: McpConfig = parse_config(value)?;
            if config.server_name.is_empty()
                || config.server_name.len() > 32
                || !config
                    .server_name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
                || config.command.trim().is_empty()
                || config.startup_timeout_ms == 0
                || config.tool_call_timeout_ms == 0
                || config.reconnect_attempts > 10
            {
                return Err(HarnessError::composition(
                    "MCP stdio requires server_name matching [A-Za-z0-9_-]{1,32}, command, positive timeouts, and reconnect_attempts at most 10",
                ));
            }
            Ok(Arc::new(McpPlugin { config }))
        },
    )
    .with_config_schema::<McpConfig>()
}

struct McpPlugin {
    config: McpConfig,
}

#[derive(Clone)]
struct McpNotifications(Arc<AtomicU64>);

impl rmcp::ClientHandler for McpNotifications {
    async fn on_tool_list_changed(&self, _: rmcp::service::NotificationContext<RoleClient>) {
        self.0.fetch_add(1, Ordering::AcqRel);
    }
}

type McpClient = RunningService<RoleClient, McpNotifications>;

struct McpServer {
    client: Arc<McpClient>,
    process: ProcessControl,
    tools_revision: Arc<AtomicU64>,
}

impl McpServer {
    fn tools_revision(&self) -> u64 {
        self.tools_revision.load(Ordering::Acquire)
    }

    fn request_stop(&self) {
        self.client.cancellation_token().cancel();
        self.process.request_stop();
    }
}

impl Drop for McpServer {
    fn drop(&mut self) {
        self.request_stop();
    }
}

struct McpTransport {
    io: AsyncRwTransport<RoleClient, ChildStdout, ChildStdin>,
    process: ManagedProcess,
}

impl McpTransport {
    fn spawn(command: &mut Command, lease: Option<WorkspaceExecutionLease>) -> io::Result<Self> {
        command
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .kill_on_drop(true);
        crate::process_group::configure(command);
        let mut child = command.spawn()?;
        let group = OwnedProcessGroup::new(child.id());
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("MCP stdin is unavailable"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("MCP stdout is unavailable"))?;
        Ok(Self {
            io: AsyncRwTransport::new(stdout, stdin),
            process: ManagedProcess::new(child, group, lease),
        })
    }
}

impl Transport<RoleClient> for McpTransport {
    type Error = io::Error;

    fn send(
        &mut self,
        item: TxJsonRpcMessage<RoleClient>,
    ) -> impl Future<Output = Result<(), io::Error>> + Send + 'static {
        self.io.send(item)
    }

    fn receive(&mut self) -> impl Future<Output = Option<RxJsonRpcMessage<RoleClient>>> + Send {
        self.io.receive()
    }

    async fn close(&mut self) -> Result<(), io::Error> {
        let closed = self.io.close().await;
        match tokio::time::timeout(Duration::from_secs(3), self.process.wait()).await {
            Ok(result) => result?,
            Err(_) => self.process.stop().await?,
        }
        closed
    }
}

impl HarnessPlugin for McpPlugin {
    fn descriptor(&self) -> &'static ComponentDescriptor {
        &DESCRIPTOR
    }

    fn activate(&self, context: ComponentContext) -> Activation {
        let tools = context
            .context()
            .service::<Tools>()
            .expect("MCP plugin declares Tools");
        let environment = context
            .context()
            .service::<RunEnvironment>()
            .expect("MCP plugin declares RunEnvironment");
        let source = Arc::new(source::McpSource::new(self.config.clone(), environment));
        Activation::Once(Box::pin(async move {
            let registration = tools
                .register_source(source)
                .await
                .map_err(|error| ActivationFailure::user(error.to_string()))?;
            Ok(Some(effect::inverse(move || async move {
                tools
                    .unregister_source(registration)
                    .await
                    .map_err(|error| CleanupError::user(error.to_string()))
            })))
        }))
    }
}

async fn prepare_command(
    environment: &RunEnvironmentClient,
    config: &McpConfig,
) -> Result<Command, HarnessError> {
    let workspace = environment
        .workspace()
        .await
        .ok_or_else(|| HarnessError::invalid("MCP stdio requires a workspace-bound session"))?;
    let cwd = config.cwd.clone().unwrap_or(workspace.path);
    let mut command = Command::new(&config.command);
    command
        .args(&config.args)
        .current_dir(cwd)
        .kill_on_drop(true)
        .env_clear();
    for key in ["PATH", "HOME", "USER", "LANG", "LC_ALL", "TMPDIR"] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    command.envs(&config.env);
    for (target, reference) in &config.env_refs {
        let value = environment
            .resolve_secret(reference.clone())
            .await?
            .ok_or_else(|| {
                HarnessError::invalid(format!("MCP credential {reference:?} is not configured"))
            })?;
        command.env(target, value);
    }
    Ok(command)
}

async fn initialize_mcp(
    transport: McpTransport,
    config: &McpConfig,
) -> Result<McpServer, HarnessError> {
    let process = transport.process.control();
    let tools_revision = Arc::new(AtomicU64::new(0));
    let client = tokio::time::timeout(
        Duration::from_millis(config.startup_timeout_ms),
        McpNotifications(Arc::clone(&tools_revision)).serve(transport),
    )
    .await
    .map_err(|_| {
        HarnessError::execution(format!(
            "MCP server {} startup timed out",
            config.server_name
        ))
    })?
    .map_err(|error| {
        HarnessError::execution(format!(
            "initialize MCP server {}: {error}",
            config.server_name
        ))
    })?;
    Ok(McpServer {
        client: Arc::new(client),
        process,
        tools_revision,
    })
}

#[derive(Clone)]
struct McpDefinition {
    spec: ToolSpec,
    raw_name: String,
}

async fn discover_tools(
    client: &McpServer,
    config: &McpConfig,
) -> Result<Vec<McpDefinition>, HarnessError> {
    let remote_tools = tokio::time::timeout(
        Duration::from_millis(config.startup_timeout_ms),
        client.client.list_all_tools(),
    )
    .await
    .map_err(|_| {
        HarnessError::execution(format!(
            "MCP server {} tool discovery timed out",
            config.server_name
        ))
    })?
    .map_err(|error| {
        HarnessError::execution(format!(
            "list tools from MCP server {}: {error}",
            config.server_name
        ))
    })?;
    let mut names = BTreeSet::new();
    let mut definitions = Vec::new();
    for remote in remote_tools {
        let public_name = public_tool_name(&config.server_name, remote.name.as_ref());
        if !names.insert(public_name.clone()) {
            return Err(HarnessError::composition(format!(
                "MCP server {} exposes colliding tool names",
                config.server_name
            )));
        }
        definitions.push(McpDefinition {
            spec: ToolSpec {
                name: public_name,
                description: remote
                    .description
                    .map_or_else(String::new, std::borrow::Cow::into_owned),
                input_schema: Value::Object((*remote.input_schema).clone()),
            },
            raw_name: remote.name.into_owned(),
        });
    }
    Ok(definitions)
}

struct McpTool {
    shared: Arc<source::McpShared>,
    generation: u64,
    raw_name: String,
    timeout_ms: u64,
}

impl ToolHandler for McpTool {
    fn execute<'a>(
        &'a self,
        context: ToolExecutionContext,
        arguments: Value,
    ) -> Pin<Box<dyn Future<Output = Result<ToolOutput, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            context.cancellation.check()?;
            let arguments = arguments
                .as_object()
                .cloned()
                .ok_or_else(|| HarnessError::invalid("MCP tool arguments must be a JSON object"))?;
            let mut pending = self.shared.begin_call(self.generation)?;
            let request = tokio::time::timeout(
                Duration::from_millis(self.timeout_ms),
                pending.server.client.call_tool(
                    CallToolRequestParams::new(self.raw_name.clone()).with_arguments(arguments),
                ),
            );
            let result = tokio::select! {
                biased;
                () = context.cancellation.cancelled() => Err(HarnessError::cancelled("MCP request was cancelled")),
                result = request => result.map_err(|_| HarnessError::execution(format!(
                    "MCP tool {:?} timed out after {} ms", self.raw_name, self.timeout_ms,
                ))).and_then(|result| result.map_err(|error| HarnessError::execution(format!(
                    "MCP tool {:?}: {error}", self.raw_name,
                )))),
            };
            let result = match result {
                Ok(result) => result,
                Err(error) => {
                    pending.fail(error.to_string());
                    return Err(error);
                }
            };
            let mut rendered = result
                .content
                .iter()
                .map(render_content)
                .collect::<Result<Vec<_>, HarnessError>>()?
                .join("\n");
            if let Some(structured) = result.structured_content {
                if !rendered.is_empty() {
                    rendered.push('\n');
                }
                rendered.push_str(&serde_json::to_string_pretty(&structured).map_err(|error| {
                    HarnessError::execution(format!("serialize MCP structured result: {error}"))
                })?);
            }
            pending.complete();
            Ok(ToolOutput {
                content: rendered,
                is_error: result.is_error.unwrap_or(false),
            })
        })
    }
}

fn render_content(content: &ContentBlock) -> Result<String, HarnessError> {
    if let Some(text) = content.as_text() {
        Ok(text.text.clone())
    } else {
        serde_json::to_string(content)
            .map_err(|error| HarnessError::execution(format!("serialize MCP content: {error}")))
    }
}

fn public_tool_name(server: &str, raw: &str) -> String {
    const MAX_LENGTH: usize = 64;
    let joined = format!("mcp__{server}__{raw}");
    let normalized = joined
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '_' | '-') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    if normalized == joined && normalized.len() <= MAX_LENGTH {
        return normalized;
    }
    let hash = fnv1a(joined.as_bytes());
    let suffix = format!("_{hash:012x}");
    let prefix_length = MAX_LENGTH - suffix.len();
    format!(
        "{}{}",
        &normalized[..normalized.len().min(prefix_length)],
        suffix
    )
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash & 0x0000_ffff_ffff_ffff
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automatic_reconnect_is_opt_in_and_bounded_in_configuration() {
        let mut config = serde_json::json!({"server_name": "fixture", "command": "fixture"});
        assert_eq!(
            serde_json::from_value::<McpConfig>(config.clone())
                .unwrap()
                .reconnect_attempts,
            0
        );
        let factory = factory();
        assert!(factory.build(config.clone()).is_ok());
        config["reconnect_attempts"] = 10.into();
        assert!(factory.build(config.clone()).is_ok());
        config["reconnect_attempts"] = 11.into();
        assert!(factory.build(config).is_err());
        assert_eq!(
            factory.config_schema["properties"]["reconnect_attempts"]["maximum"].as_f64(),
            Some(10.0)
        );
    }

    #[test]
    fn public_names_are_stable_qualified_and_bounded() {
        assert_eq!(public_tool_name("files", "read"), "mcp__files__read");
        let normalized = public_tool_name("server", &"unsafe/name".repeat(10));
        assert!(normalized.len() <= 64);
        assert!(
            normalized
                .bytes()
                .all(|byte| { byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-') })
        );
    }
}
