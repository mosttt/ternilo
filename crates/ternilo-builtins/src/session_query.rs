use std::{future::Future, pin::Pin, sync::Arc};

use linorun_core::{Activation, CleanupError, ComponentContext, ComponentDescriptor, effect};
use linorun_macros::component_descriptor;
use serde_json::{Value, json};
use ternilo_kernel::{
    HarnessPlugin, PluginFactory, PluginManifest, SessionQueries, SessionQueriesClient,
    ToolExecutionContext, ToolHandler, ToolRegistration, Tools,
};
use ternilo_protocol::{
    HarnessError, RunId, SessionEventCategory, SessionEventReadRequest, SessionId,
    SessionSearchFilters, SessionSearchRequest, ToolOutput, ToolSpec, WorkspaceId,
};

use crate::{EmptyConfig, factory as make_factory, parse_config};

pub const KIND: &str = "ternilo.tool.session_query";

component_descriptor! {
    static DESCRIPTOR: () {
        id: "ternilo/builtin-session-query-tools@1",
        requires: [Tools, SessionQueries],
        provides: [],
    }
}

pub fn factory() -> PluginFactory {
    make_factory(
        PluginManifest {
            kind: KIND,
            requires: &["ternilo/tools@1", "ternilo/session-queries@1"],
            provides: &[],
        },
        |value| {
            let _: EmptyConfig = parse_config(value)?;
            Ok(Arc::new(SessionQueryPlugin))
        },
    )
    .with_config_schema::<EmptyConfig>()
}

struct SessionQueryPlugin;

impl HarnessPlugin for SessionQueryPlugin {
    fn descriptor(&self) -> &'static ComponentDescriptor {
        &DESCRIPTOR
    }

    fn activate(&self, context: ComponentContext) -> Activation {
        let tools = context
            .context()
            .service::<Tools>()
            .expect("session query plugin declares Tools");
        let queries = context
            .context()
            .service::<SessionQueries>()
            .expect("session query plugin declares SessionQueries");
        Activation::Once(Box::pin(async move {
            let mut registrations = Vec::new();
            for (spec, operation) in tool_specs() {
                let registration = tools
                    .register_tool(ToolRegistration {
                        spec,
                        effect: ternilo_kernel::ToolEffect::ReadOnly,
                        handler: Arc::new(SessionQueryTool {
                            queries: queries.clone(),
                            operation,
                        }),
                    })
                    .await
                    .map_err(|error| linorun_core::ActivationFailure::user(error.to_string()))?;
                registrations.push(registration);
            }
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
    Search,
    EventSearch,
    EventRead,
    SessionTrace,
    EventTrace,
}

struct SessionQueryTool {
    queries: SessionQueriesClient,
    operation: Operation,
}

impl ToolHandler for SessionQueryTool {
    fn execute<'a>(
        &'a self,
        _: ToolExecutionContext,
        arguments: Value,
    ) -> Pin<Box<dyn Future<Output = Result<ToolOutput, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            let value = match self.operation {
                Operation::Search => {
                    let query = required_string(&arguments, "query")?;
                    let limit = optional_u32(&arguments, "limit", 20)?;
                    let workspace_id =
                        optional_string(&arguments, "workspace_id")?.map(WorkspaceId::new);
                    serde_json::to_value(
                        self.queries
                            .search(SessionSearchRequest {
                                query,
                                session_id: None,
                                workspace_id,
                                filters: search_filters(&arguments)?,
                                limit,
                            })
                            .await?,
                    )
                }
                Operation::EventSearch => {
                    let query = required_string(&arguments, "query")?;
                    let session_id = SessionId::new(required_string(&arguments, "session_id")?);
                    let limit = optional_u32(&arguments, "limit", 20)?;
                    serde_json::to_value(
                        self.queries
                            .search(SessionSearchRequest {
                                query,
                                session_id: Some(session_id),
                                workspace_id: None,
                                filters: search_filters(&arguments)?,
                                limit,
                            })
                            .await?,
                    )
                }
                Operation::EventRead => {
                    let request = event_read_request(&arguments)?;
                    serde_json::to_value(self.queries.read_events(request).await?)
                }
                Operation::SessionTrace => {
                    let session_id = SessionId::new(required_string(&arguments, "session_id")?);
                    serde_json::to_value(self.queries.trace(session_id).await?)
                }
                Operation::EventTrace => {
                    let session_id = SessionId::new(required_string(&arguments, "session_id")?);
                    let seq = required_u64(&arguments, "seq")?;
                    let context = u64::from(optional_u32(&arguments, "context", 5)?.min(99));
                    let start_seq = seq.saturating_sub(context);
                    let limit =
                        u32::try_from(context.saturating_mul(2).saturating_add(1)).unwrap_or(199);
                    let events = self
                        .queries
                        .read_events(SessionEventReadRequest {
                            session_id,
                            start_seq,
                            limit,
                        })
                        .await?;
                    if !events.iter().any(|event| event.seq == seq) {
                        return Err(HarnessError::invalid(format!(
                            "session event seq {seq} does not exist"
                        )));
                    }
                    serde_json::to_value(events)
                }
            }
            .map_err(|error| {
                HarnessError::execution(format!("serialize session query result: {error}"))
            })?;
            Ok(ToolOutput {
                content: serde_json::to_string_pretty(&value).map_err(|error| {
                    HarnessError::execution(format!("render session query result: {error}"))
                })?,
                is_error: false,
            })
        })
    }
}

fn event_read_request(arguments: &Value) -> Result<SessionEventReadRequest, HarnessError> {
    Ok(SessionEventReadRequest {
        session_id: SessionId::new(required_string(arguments, "session_id")?),
        start_seq: optional_u64(arguments, "start_seq", 0)?,
        limit: optional_u32(arguments, "limit", 50)?,
    })
}

fn search_filters(arguments: &Value) -> Result<SessionSearchFilters, HarnessError> {
    let category = optional_string(arguments, "category")?
        .map(|value| {
            serde_json::from_value::<SessionEventCategory>(Value::String(value)).map_err(|_| {
                HarnessError::invalid(
                    "category must be user, assistant, tool, planning, compaction, deliverable, interaction, error, lifecycle, or other",
                )
            })
        })
        .transpose()?;
    Ok(SessionSearchFilters {
        run_id: optional_string(arguments, "run_id")?.map(RunId::new),
        category,
        occurred_after_ms: optional_u64_value(arguments, "occurred_after_ms")?,
        occurred_before_ms: optional_u64_value(arguments, "occurred_before_ms")?,
    })
}

fn optional_u64_value(arguments: &Value, name: &str) -> Result<Option<u64>, HarnessError> {
    match arguments.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .map(Some)
            .ok_or_else(|| HarnessError::invalid(format!("{name} must be a non-negative integer"))),
    }
}

fn required_string(arguments: &Value, name: &str) -> Result<String, HarnessError> {
    arguments
        .get(name)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| HarnessError::invalid(format!("{name} must be a non-empty string")))
}

fn optional_string(arguments: &Value, name: &str) -> Result<Option<String>, HarnessError> {
    match arguments.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if !value.trim().is_empty() => Ok(Some(value.trim().to_owned())),
        _ => Err(HarnessError::invalid(format!(
            "{name} must be a non-empty string when supplied"
        ))),
    }
}

fn required_u64(arguments: &Value, name: &str) -> Result<u64, HarnessError> {
    arguments
        .get(name)
        .and_then(Value::as_u64)
        .ok_or_else(|| HarnessError::invalid(format!("{name} must be a non-negative integer")))
}

fn optional_u64(arguments: &Value, name: &str, default: u64) -> Result<u64, HarnessError> {
    match arguments.get(name) {
        None | Some(Value::Null) => Ok(default),
        Some(value) => value
            .as_u64()
            .ok_or_else(|| HarnessError::invalid(format!("{name} must be a non-negative integer"))),
    }
}

fn optional_u32(arguments: &Value, name: &str, default: u32) -> Result<u32, HarnessError> {
    let value = optional_u64(arguments, name, u64::from(default))?;
    value
        .try_into()
        .map_err(|_| HarnessError::invalid(format!("{name} exceeds u32")))
}

fn tool_specs() -> Vec<(ToolSpec, Operation)> {
    vec![
        (
            ToolSpec {
                name: "session_search".to_owned(),
                description: "Search this user's persisted session titles and event text.".to_owned(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "query": { "type": "string" },
                        "workspace_id": { "type": "string" },
                        "run_id": { "type": "string" },
                        "category": { "type": "string", "enum": ["user", "assistant", "tool", "planning", "compaction", "deliverable", "interaction", "error", "lifecycle", "other"] },
                        "occurred_after_ms": { "type": "integer", "minimum": 0 },
                        "occurred_before_ms": { "type": "integer", "minimum": 0 },
                        "limit": { "type": "integer", "minimum": 1, "maximum": 100 }
                    },
                    "required": ["query"],
                    "additionalProperties": false
                }),
            },
            Operation::Search,
        ),
        (
            ToolSpec {
                name: "session_event_search".to_owned(),
                description: "Search persisted event text inside one authorized session.".to_owned(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "query": { "type": "string" },
                        "run_id": { "type": "string" },
                        "category": { "type": "string", "enum": ["user", "assistant", "tool", "planning", "compaction", "deliverable", "interaction", "error", "lifecycle", "other"] },
                        "occurred_after_ms": { "type": "integer", "minimum": 0 },
                        "occurred_before_ms": { "type": "integer", "minimum": 0 },
                        "limit": { "type": "integer", "minimum": 1, "maximum": 100 }
                    },
                    "required": ["session_id", "query"],
                    "additionalProperties": false
                }),
            },
            Operation::EventSearch,
        ),
        (
            ToolSpec {
                name: "session_event_read".to_owned(),
                description: "Read an ordered range from one authorized append-only session log.".to_owned(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "start_seq": { "type": "integer", "minimum": 0 },
                        "limit": { "type": "integer", "minimum": 1, "maximum": 200 }
                    },
                    "required": ["session_id"],
                    "additionalProperties": false
                }),
            },
            Operation::EventRead,
        ),
        (
            ToolSpec {
                name: "session_trace".to_owned(),
                description: "Return identity, workspace, timestamps, and event/run counts for one authorized session.".to_owned(),
                input_schema: json!({
                    "type": "object",
                    "properties": { "session_id": { "type": "string" } },
                    "required": ["session_id"],
                    "additionalProperties": false
                }),
            },
            Operation::SessionTrace,
        ),
        (
            ToolSpec {
                name: "session_event_trace".to_owned(),
                description: "Read a bounded event window around one exact sequence number.".to_owned(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "seq": { "type": "integer", "minimum": 0 },
                        "context": { "type": "integer", "minimum": 0, "maximum": 99 }
                    },
                    "required": ["session_id", "seq"],
                    "additionalProperties": false
                }),
            },
            Operation::EventTrace,
        ),
    ]
}
