use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
};

use ternilo_kernel::{DeferredToolSource, RunCancellation, RunEnvironmentClient, ToolRegistration};
use ternilo_protocol::{
    HarnessError, SessionServiceKind, SessionServiceSnapshot, SessionServiceStatus,
};

use super::{McpConfig, McpDefinition, McpServer, McpTool, McpTransport};
use crate::process_group::ProcessControl;

pub(super) struct McpSource {
    config: McpConfig,
    environment: RunEnvironmentClient,
    shared: Arc<McpShared>,
}

pub(super) struct McpShared {
    state: Mutex<McpState>,
    lifecycle: tokio::sync::Mutex<()>,
}

struct McpState {
    status: SessionServiceStatus,
    generation: u64,
    closed: bool,
    active_calls: u32,
    startup: Option<RunCancellation>,
    process: Option<ProcessControl>,
    server: Option<Arc<McpServer>>,
    definitions: Vec<McpDefinition>,
    error: Option<String>,
}

enum Preparation {
    Cached(Vec<ToolRegistration>),
    Start(u64, RunCancellation),
}

impl McpSource {
    pub(super) fn new(config: McpConfig, environment: RunEnvironmentClient) -> Self {
        Self {
            config,
            environment,
            shared: Arc::new(McpShared {
                state: Mutex::new(McpState {
                    status: SessionServiceStatus::Idle,
                    generation: 0,
                    closed: false,
                    active_calls: 0,
                    startup: None,
                    process: None,
                    server: None,
                    definitions: Vec::new(),
                    error: None,
                }),
                lifecycle: tokio::sync::Mutex::new(()),
            }),
        }
    }

    fn registrations(&self, state: &McpState) -> Vec<ToolRegistration> {
        state
            .definitions
            .iter()
            .map(|definition| ToolRegistration {
                spec: definition.spec.clone(),
                effect: ternilo_kernel::ToolEffect::Dangerous,
                handler: Arc::new(McpTool {
                    shared: Arc::clone(&self.shared),
                    generation: state.generation,
                    raw_name: definition.raw_name.clone(),
                    timeout_ms: self.config.tool_call_timeout_ms,
                }),
            })
            .collect()
    }

    fn begin_start(&self, explicit: bool) -> Result<Preparation, HarnessError> {
        let mut state = self.shared.state.lock().expect("MCP state lock poisoned");
        state.observe_exit();
        if state.closed {
            return Err(HarnessError::cancelled("MCP service was unmounted"));
        }
        match state.status {
            SessionServiceStatus::Running => {
                return Ok(Preparation::Cached(self.registrations(&state)));
            }
            SessionServiceStatus::Stopped if !explicit => {
                return Ok(Preparation::Cached(Vec::new()));
            }
            SessionServiceStatus::Failed if !explicit => {
                if state.definitions.is_empty() {
                    return Err(state.unavailable());
                }
                return Ok(Preparation::Cached(self.registrations(&state)));
            }
            SessionServiceStatus::Starting | SessionServiceStatus::Stopping => {
                return Err(HarnessError::conflict("MCP service is changing state"));
            }
            _ => {}
        }
        if state.active_calls != 0 {
            return Err(HarnessError::conflict("MCP service still has active calls"));
        }
        let cancellation = RunCancellation::new();
        state.generation += 1;
        state.status = SessionServiceStatus::Starting;
        state.startup = Some(cancellation.clone());
        state.definitions.clear();
        state.error = None;
        Ok(Preparation::Start(state.generation, cancellation))
    }

    async fn prepare_source(
        &self,
        explicit: bool,
        cancellation: RunCancellation,
    ) -> Result<Vec<ToolRegistration>, HarnessError> {
        let _lifecycle = tokio::select! {
            biased;
            () = cancellation.cancelled() => return Err(HarnessError::cancelled("MCP startup was cancelled")),
            guard = self.shared.lifecycle.lock() => guard,
        };
        let (generation, startup) = match self.begin_start(explicit)? {
            Preparation::Start(generation, startup) => (generation, startup),
            Preparation::Cached(registrations) => return Ok(registrations),
        };
        let mut attempt = StartupAttempt {
            shared: Arc::clone(&self.shared),
            generation,
            completed: false,
        };
        let result = tokio::select! {
            biased;
            () = cancellation.cancelled() => Err(HarnessError::cancelled("MCP startup was cancelled")),
            () = startup.cancelled() => Err(HarnessError::cancelled("MCP startup was stopped")),
            result = self.connect(generation, startup.clone()) => result,
        };
        let result = result.and_then(|(server, definitions)| {
            let mut state = self.shared.state.lock().expect("MCP state lock poisoned");
            if state.closed || startup.is_cancelled() || cancellation.is_cancelled() {
                return Err(HarnessError::cancelled("MCP startup was cancelled"));
            }
            state.server = Some(server);
            state.definitions = definitions;
            state.startup = None;
            state.status = SessionServiceStatus::Running;
            Ok(self.registrations(&state))
        });
        if let Err(error) = &result {
            self.shared.fail_start(generation, error.to_string());
            self.shared.stop_process().await?;
        }
        attempt.completed = true;
        result
    }

    async fn connect(
        &self,
        generation: u64,
        cancellation: RunCancellation,
    ) -> Result<(Arc<McpServer>, Vec<McpDefinition>), HarnessError> {
        self.shared.stop_process().await?;
        let mut command = super::prepare_command(&self.environment, &self.config).await?;
        let lease = self
            .environment
            .acquire_workspace(cancellation.clone())
            .await?;
        cancellation.check()?;
        let transport = McpTransport::spawn(&mut command, Some(lease)).map_err(|error| {
            HarnessError::execution(format!(
                "start MCP server {}: {error}",
                self.config.server_name
            ))
        })?;
        {
            let mut state = self.shared.state.lock().expect("MCP state lock poisoned");
            if state.generation != generation || state.closed || cancellation.is_cancelled() {
                return Err(HarnessError::cancelled("MCP startup was cancelled"));
            }
            state.process = Some(transport.process.control());
        }
        let server = Arc::new(super::initialize_mcp(transport, &self.config).await?);
        let definitions = super::discover_tools(&server, &self.config).await?;
        Ok((server, definitions))
    }

    async fn stop_source(&self, force: bool) -> Result<(), HarnessError> {
        self.shared.request_stop(force)?;
        let _lifecycle = self.shared.lifecycle.lock().await;
        self.shared.stop_process().await?;
        let mut state = self.shared.state.lock().expect("MCP state lock poisoned");
        state.status = SessionServiceStatus::Stopped;
        state.startup = None;
        state.definitions.clear();
        state.error = None;
        Ok(())
    }
}

impl Drop for McpSource {
    fn drop(&mut self) {
        let _ = self.shared.request_stop(true);
    }
}

impl DeferredToolSource for McpSource {
    fn snapshot(&self) -> SessionServiceSnapshot {
        let mut state = self.shared.state.lock().expect("MCP state lock poisoned");
        state.observe_exit();
        SessionServiceSnapshot {
            id: format!("mcp:{}", self.config.server_name),
            name: self.config.server_name.clone(),
            kind: SessionServiceKind::Mcp,
            status: state.status,
            active_calls: state.active_calls,
            error: state.error.clone(),
        }
    }

    fn prepare<'a>(
        &'a self,
        cancellation: RunCancellation,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<ToolRegistration>, HarnessError>> + Send + 'a>>
    {
        Box::pin(self.prepare_source(false, cancellation))
    }

    fn start<'a>(
        &'a self,
        cancellation: RunCancellation,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<ToolRegistration>, HarnessError>> + Send + 'a>>
    {
        Box::pin(self.prepare_source(true, cancellation))
    }

    fn stop<'a>(&'a self) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(self.stop_source(false))
    }

    fn shutdown<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(self.stop_source(true))
    }
}

impl McpState {
    fn unavailable(&self) -> HarnessError {
        HarnessError::execution(format!(
            "MCP service is unavailable; explicitly start it again: {}",
            self.error
                .as_deref()
                .unwrap_or("the service is not running"),
        ))
    }

    fn observe_exit(&mut self) {
        if self.status == SessionServiceStatus::Running
            && self
                .server
                .as_ref()
                .is_some_and(|server| server.client.is_closed() || server.process.is_finished())
        {
            self.status = SessionServiceStatus::Failed;
            self.error = Some("MCP server exited or closed its connection".to_owned());
            self.signal_stop();
        }
    }

    fn signal_stop(&self) {
        if let Some(startup) = &self.startup {
            startup.cancel();
        }
        if let Some(server) = &self.server {
            server.request_stop();
        }
        if let Some(process) = &self.process {
            process.request_stop();
        }
    }
}

impl McpShared {
    fn request_stop(&self, force: bool) -> Result<(), HarnessError> {
        let mut state = self.state.lock().expect("MCP state lock poisoned");
        if !force && state.active_calls != 0 {
            return Err(HarnessError::conflict(
                "MCP service has active calls; stop the task first",
            ));
        }
        state.closed |= force;
        state.status = SessionServiceStatus::Stopping;
        state.signal_stop();
        Ok(())
    }

    async fn stop_process(&self) -> Result<(), HarnessError> {
        let process = {
            let state = self.state.lock().expect("MCP state lock poisoned");
            if let Some(server) = &state.server {
                server.request_stop();
            }
            state.process.clone()
        };
        if let Some(process) = process {
            process
                .stop()
                .await
                .map_err(|error| HarnessError::execution(format!("stop MCP process: {error}")))?;
        }
        let mut state = self.state.lock().expect("MCP state lock poisoned");
        state.process = None;
        state.server = None;
        Ok(())
    }

    fn fail_start(&self, generation: u64, error: String) {
        let mut state = self.state.lock().expect("MCP state lock poisoned");
        if state.generation != generation {
            return;
        }
        if state.status == SessionServiceStatus::Stopping || state.closed {
            state.status = SessionServiceStatus::Stopped;
            state.error = None;
        } else {
            state.status = SessionServiceStatus::Failed;
            state.error = Some(error);
        }
        state.signal_stop();
        state.startup = None;
    }

    pub(super) fn begin_call(
        self: &Arc<Self>,
        generation: u64,
    ) -> Result<PendingMcpCall, HarnessError> {
        let mut state = self.state.lock().expect("MCP state lock poisoned");
        state.observe_exit();
        if state.generation != generation
            || state.status != SessionServiceStatus::Running
            || state.closed
        {
            return Err(state.unavailable());
        }
        let server = Arc::clone(state.server.as_ref().expect("running MCP has a server"));
        state.active_calls = state
            .active_calls
            .checked_add(1)
            .ok_or_else(|| HarnessError::execution("MCP active call count exhausted"))?;
        Ok(PendingMcpCall {
            shared: Arc::clone(self),
            server,
            generation,
            error: Some("MCP call was cancelled before completion".to_owned()),
        })
    }
}

struct StartupAttempt {
    shared: Arc<McpShared>,
    generation: u64,
    completed: bool,
}

impl Drop for StartupAttempt {
    fn drop(&mut self) {
        if !self.completed {
            self.shared
                .fail_start(self.generation, "MCP startup was cancelled".to_owned());
        }
    }
}

pub(super) struct PendingMcpCall {
    shared: Arc<McpShared>,
    pub(super) server: Arc<McpServer>,
    generation: u64,
    error: Option<String>,
}

impl PendingMcpCall {
    pub(super) fn complete(&mut self) {
        self.error = None;
    }

    pub(super) fn fail(&mut self, error: String) {
        self.error = Some(error);
    }
}

impl Drop for PendingMcpCall {
    fn drop(&mut self) {
        let mut state = self.shared.state.lock().expect("MCP state lock poisoned");
        if state.generation != self.generation {
            return;
        }
        state.active_calls -= 1;
        if let Some(error) = self.error.take() {
            if state.status == SessionServiceStatus::Running {
                state.status = SessionServiceStatus::Failed;
                state.error = Some(error);
            }
            self.server.request_stop();
        }
    }
}
