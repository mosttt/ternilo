use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use chrono::DateTime;
use linorun_core::{Activation, CleanupError, ComponentContext, ComponentDescriptor, effect};
use linorun_macros::component_descriptor;
use serde::Deserialize;
use serde_json::{Value, json};
use ternilo_kernel::{
    Agents, AgentsClient, HarnessPlugin, PluginFactory, PluginManifest, RunEnvironment,
    RunEnvironmentClient, Sessions, SessionsClient, ToolExecutionContext, ToolHandler,
    ToolRegistration, Tools,
};
use ternilo_protocol::{
    AgentInput, HarnessError, RunId, ScheduleChange, ScheduleId, ScheduleRecord, ScheduleRule,
    SessionEvent, SessionEventKind, ToolOutput, ToolSpec,
};
use tokio::sync::{Mutex, Notify};

use crate::{factory as make_factory, parse_config};

pub const KIND: &str = "ternilo.tool.schedule";

component_descriptor! {
    static DESCRIPTOR: () {
        id: "ternilo/builtin-schedule@1",
        requires: [Sessions, Tools, Agents, RunEnvironment],
        provides: [],
    }
}

#[derive(Clone, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ScheduleConfig {
    #[serde(default = "default_min_every_seconds")]
    min_every_seconds: u64,
    #[serde(default = "default_max_prompt_chars")]
    max_prompt_chars: usize,
}

const fn default_min_every_seconds() -> u64 {
    300
}

const fn default_max_prompt_chars() -> usize {
    8_192
}

pub fn factory() -> PluginFactory {
    make_factory(
        PluginManifest {
            kind: KIND,
            requires: &[
                "ternilo/sessions@1",
                "ternilo/tools@1",
                "ternilo/agents@3",
                "ternilo/run-environment@1",
            ],
            provides: &[],
        },
        |value| {
            let config: ScheduleConfig = parse_config(value)?;
            if config.min_every_seconds == 0 || config.max_prompt_chars == 0 {
                return Err(HarnessError::composition(
                    "schedule limits must be greater than zero",
                ));
            }
            Ok(Arc::new(SchedulePlugin { config }))
        },
    )
    .with_config_schema::<ScheduleConfig>()
}

struct SchedulePlugin {
    config: ScheduleConfig,
}

impl HarnessPlugin for SchedulePlugin {
    fn descriptor(&self) -> &'static ComponentDescriptor {
        &DESCRIPTOR
    }

    fn activate(&self, context: ComponentContext) -> Activation {
        let sessions = context
            .context()
            .service::<Sessions>()
            .expect("schedule plugin declares Sessions");
        let tools = context
            .context()
            .service::<Tools>()
            .expect("schedule plugin declares Tools");
        let agents = context
            .context()
            .service::<Agents>()
            .expect("schedule plugin declares Agents");
        let config = self.config.clone();
        let environment = context
            .context()
            .service::<RunEnvironment>()
            .expect("schedule plugin declares RunEnvironment");
        Activation::Once(Box::pin(async move {
            let events = sessions.events().await;
            let active = fold_schedules(&events)
                .map_err(|error| linorun_core::ActivationFailure::user(error.to_string()))?;
            let runtime = Arc::new(ScheduleRuntime {
                environment,
                sessions,
                agents,
                active: Mutex::new(active),
                notify: Notify::new(),
                stopping: AtomicBool::new(false),
                next_id: AtomicU64::new(u64::try_from(events.len()).unwrap_or(u64::MAX)),
                config,
            });
            let mut registrations = Vec::new();
            for (spec, operation) in tool_specs() {
                let registration = tools
                    .register_tool(ToolRegistration {
                        spec,
                        effect: operation.effect(),
                        handler: Arc::new(ScheduleTool {
                            runtime: Arc::clone(&runtime),
                            operation,
                        }),
                    })
                    .await
                    .map_err(|error| linorun_core::ActivationFailure::user(error.to_string()))?;
                registrations.push(registration);
            }
            let driver = tokio::spawn(Arc::clone(&runtime).drive());
            Ok(Some(effect::inverse(move || async move {
                runtime.stopping.store(true, Ordering::Release);
                runtime.notify.notify_waiters();
                driver.abort();
                let _ = driver.await;
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

struct ScheduleRuntime {
    environment: RunEnvironmentClient,
    sessions: SessionsClient,
    agents: AgentsClient,
    active: Mutex<BTreeMap<ScheduleId, ScheduleRecord>>,
    notify: Notify,
    stopping: AtomicBool,
    next_id: AtomicU64,
    config: ScheduleConfig,
}

impl ScheduleRuntime {
    async fn create(
        &self,
        run_id: RunId,
        arguments: &Value,
    ) -> Result<ScheduleRecord, HarnessError> {
        let prompt = required_string(arguments, "prompt")?;
        if prompt.chars().count() > self.config.max_prompt_chars {
            return Err(HarnessError::invalid(format!(
                "schedule prompt may not exceed {} characters",
                self.config.max_prompt_chars
            )));
        }
        let now = now_ms()?;
        let after = optional_u64(arguments, "after_seconds")?;
        let at = optional_string(arguments, "at")?;
        let every = optional_u64(arguments, "every_seconds")?;
        if usize::from(after.is_some()) + usize::from(at.is_some()) + usize::from(every.is_some())
            != 1
        {
            return Err(HarnessError::invalid(
                "schedule_create accepts exactly one of after_seconds, at, or every_seconds",
            ));
        }
        let (rule, scheduled_at_ms) = if let Some(seconds) = after {
            if seconds == 0 {
                return Err(HarnessError::invalid(
                    "after_seconds must be a positive integer",
                ));
            }
            (
                ScheduleRule::After {
                    after_seconds: seconds,
                },
                add_seconds(now, seconds)?,
            )
        } else if let Some(at) = at {
            let instant = DateTime::parse_from_rfc3339(&at).map_err(|error| {
                HarnessError::invalid(format!("at must be an RFC 3339 instant: {error}"))
            })?;
            let timestamp = u64::try_from(instant.timestamp_millis()).map_err(|_| {
                HarnessError::invalid("at must be representable as a non-negative timestamp")
            })?;
            if timestamp <= now {
                return Err(HarnessError::invalid("at must be strictly in the future"));
            }
            (ScheduleRule::At, timestamp)
        } else {
            let seconds = every.expect("exactly one selector was established");
            if seconds < self.config.min_every_seconds {
                return Err(HarnessError::invalid(format!(
                    "every_seconds must be at least {}",
                    self.config.min_every_seconds
                )));
            }
            (
                ScheduleRule::Every {
                    every_seconds: seconds,
                },
                add_seconds(now, seconds)?,
            )
        };
        let sequence = self.next_id.fetch_add(1, Ordering::Relaxed);
        let schedule = ScheduleRecord {
            id: ScheduleId::new(format!("schedule-{now}-{sequence}")),
            prompt,
            rule,
            scheduled_at_ms,
            created_at_ms: now,
        };
        let mut active = self.active.lock().await;
        self.sessions
            .append(
                run_id,
                SessionEventKind::ScheduleChanged {
                    change: ScheduleChange::Create {
                        schedule: schedule.clone(),
                    },
                },
            )
            .await?;
        active.insert(schedule.id.clone(), schedule.clone());
        drop(active);
        self.notify.notify_one();
        Ok(schedule)
    }

    async fn delete(&self, run_id: RunId, id: ScheduleId) -> Result<bool, HarnessError> {
        let mut active = self.active.lock().await;
        loop {
            let events = self.sessions.events().await;
            *active = fold_schedules(&events)?;
            if !active.contains_key(&id) {
                return Ok(false);
            }
            if self
                .sessions
                .append_if_next_seq(
                    next_event_seq(&events)?,
                    run_id.clone(),
                    SessionEventKind::ScheduleChanged {
                        change: ScheduleChange::Delete { id: id.clone() },
                    },
                )
                .await?
                .is_some()
            {
                break;
            }
        }
        active.remove(&id);
        drop(active);
        self.notify.notify_one();
        Ok(true)
    }

    async fn list(&self) -> Result<Vec<ScheduleRecord>, HarnessError> {
        let mut active = self.active.lock().await;
        *active = fold_schedules(&self.sessions.events().await)?;
        let mut records = active.values().cloned().collect::<Vec<_>>();
        records.sort_by_key(|record| (record.scheduled_at_ms, record.id.clone()));
        Ok(records)
    }

    async fn drive(self: Arc<Self>) {
        loop {
            if self.stopping.load(Ordering::Acquire) {
                return;
            }
            let next = self
                .active
                .lock()
                .await
                .values()
                .map(|record| record.scheduled_at_ms)
                .min();
            match next {
                None => self.notify.notified().await,
                Some(target) => {
                    let now = now_ms().unwrap_or(target);
                    if target > now {
                        tokio::select! {
                            () = tokio::time::sleep(Duration::from_millis(target - now)) => {}
                            () = self.notify.notified() => continue,
                        }
                    }
                    if self.stopping.load(Ordering::Acquire) {
                        return;
                    }
                    let _ = self.dispatch_one().await;
                    // An unsynchronized account leaves its timer pending without spinning.
                    tokio::select! {
                        () = tokio::time::sleep(Duration::from_secs(1)) => {},
                        () = self.notify.notified() => {},
                    }
                }
            }
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "keep authorized selection and sequence-checked dispatch under one runtime lock"
    )]
    async fn dispatch_one(&self) -> Result<(), HarnessError> {
        let accepted_at_ms = now_ms()?;
        let mut active = self.active.lock().await;
        let events = self.sessions.events().await;
        *active = fold_schedules(&events)?;
        let mut due = active
            .values()
            .filter(|record| record.scheduled_at_ms <= accepted_at_ms)
            .cloned()
            .collect::<Vec<_>>();
        due.sort_by_key(|record| (record.scheduled_at_ms, record.id.clone()));
        let mut authorized = None;
        for record in due {
            let created = events.iter().find(|event| {
                matches!(&event.kind, SessionEventKind::ScheduleChanged { change: ScheduleChange::Create { schedule } } if schedule.id == record.id)
            }).ok_or_else(|| HarnessError::policy("schedule has no creation event"))?;
            match self
                .environment
                .check_run_authorization(created.run_id.clone())
                .await
            {
                Ok(()) => {
                    authorized = Some((record, created.seq));
                    break;
                }
                Err(error) if error.code == ternilo_protocol::ErrorCode::PolicyDenied => {
                    if self
                        .sessions
                        .append_if_next_seq(
                            next_event_seq(&events)?,
                            created.run_id.clone(),
                            SessionEventKind::ScheduleChanged {
                                change: ScheduleChange::Delete {
                                    id: record.id.clone(),
                                },
                            },
                        )
                        .await?
                        .is_some()
                    {
                        active.remove(&record.id);
                    }
                    return Ok(());
                }
                Err(_) => {}
            }
        }
        let Some((record, created_seq)) = authorized else {
            return Ok(());
        };
        let run_id = RunId::new(format!("schedule-run-{}-{accepted_at_ms}", record.id));
        let next_scheduled_at_ms = match record.rule {
            ScheduleRule::Every { every_seconds } => Some(next_occurrence(
                record.scheduled_at_ms,
                accepted_at_ms,
                every_seconds,
            )?),
            ScheduleRule::After { .. } | ScheduleRule::At => None,
        };
        let Some(dispatch) = self
            .sessions
            .append_if_next_seq(
                next_event_seq(&events)?,
                RunId::new(format!(
                    "schedule-dispatch-event-{}-{accepted_at_ms}",
                    record.id
                )),
                SessionEventKind::ScheduleChanged {
                    change: ScheduleChange::Dispatch {
                        id: record.id.clone(),
                        run_id: Some(run_id.clone()),
                        accepted_at_ms,
                        next_scheduled_at_ms,
                    },
                },
            )
            .await?
        else {
            return Ok(());
        };
        if let Some(next) = next_scheduled_at_ms {
            let mut advanced = record.clone();
            advanced.scheduled_at_ms = next;
            active.insert(record.id.clone(), advanced);
        } else {
            active.remove(&record.id);
        }
        drop(active);

        self.agents
            .run(AgentInput {
                additional_inputs: Vec::new(),
                provenance: Some(ternilo_protocol::InputProvenance {
                    run_id: None,
                    input_id: ternilo_protocol::SubmissionId::new(format!("schedule-input-{}-{accepted_at_ms}", record.id)),
                    author: ternilo_protocol::InputAuthor::Automation {
                        source: ternilo_protocol::AutomatedInputSource::Schedule,
                    },
                }),
                run_id,
                input: format!(
                    "A scheduled reminder is due now (schedule id {}, target {} ms since Unix epoch).\n\n{}",
                    record.id, record.scheduled_at_ms, record.prompt
                ),
                display_input: None,
                source: Some(ternilo_protocol::UserMessageSource::Schedule {
                    schedule_id: record.id,
                    created_seq,
                    dispatched_seq: dispatch.seq,
                }),
                references: Vec::new(),
                reference_contexts: Vec::new(),
                attachments: Vec::new(),
            })
            .await?;
        Ok(())
    }
}

pub fn has_pending_schedules(events: &[SessionEvent]) -> Result<bool, HarnessError> {
    fold_schedules(events).map(|active| !active.is_empty())
}

fn next_event_seq(events: &[SessionEvent]) -> Result<u64, HarnessError> {
    events
        .len()
        .try_into()
        .map_err(|_| HarnessError::execution("session sequence exceeds u64"))
}

pub fn pending_schedules(events: &[SessionEvent]) -> Result<Vec<ScheduleRecord>, HarnessError> {
    fold_schedules(events).map(|active| active.into_values().collect())
}

fn fold_schedules(
    events: &[SessionEvent],
) -> Result<BTreeMap<ScheduleId, ScheduleRecord>, HarnessError> {
    let mut active = BTreeMap::new();
    let mut seen = BTreeSet::new();
    for event in events {
        let SessionEventKind::ScheduleChanged { change } = &event.kind else {
            continue;
        };
        match change {
            ScheduleChange::Create { schedule } => {
                schedule.id.validate()?;
                if !seen.insert(schedule.id.clone()) {
                    return Err(HarnessError::execution(format!(
                        "schedule log reuses id {}",
                        schedule.id
                    )));
                }
                active.insert(schedule.id.clone(), schedule.clone());
            }
            ScheduleChange::Delete { id } => {
                if active.remove(id).is_none() {
                    return Err(HarnessError::execution(format!(
                        "schedule log deletes inactive id {id}"
                    )));
                }
            }
            ScheduleChange::Dispatch {
                id,
                next_scheduled_at_ms,
                ..
            } => {
                let record = active.get_mut(id).ok_or_else(|| {
                    HarnessError::execution(format!("schedule log dispatches inactive id {id}"))
                })?;
                if let Some(next) = next_scheduled_at_ms {
                    if !matches!(record.rule, ScheduleRule::Every { .. })
                        || *next <= record.scheduled_at_ms
                    {
                        return Err(HarnessError::execution(
                            "schedule log contains an invalid recurring dispatch",
                        ));
                    }
                    record.scheduled_at_ms = *next;
                } else {
                    active.remove(id);
                }
            }
        }
    }
    Ok(active)
}

fn next_occurrence(anchor: u64, now: u64, every_seconds: u64) -> Result<u64, HarnessError> {
    let interval = every_seconds
        .checked_mul(1_000)
        .ok_or_else(|| HarnessError::invalid("schedule interval overflows milliseconds"))?;
    let elapsed = now.saturating_sub(anchor);
    let intervals = elapsed / interval + 1;
    anchor
        .checked_add(intervals.saturating_mul(interval))
        .ok_or_else(|| HarnessError::execution("next schedule occurrence overflows timestamp"))
}

fn add_seconds(timestamp: u64, seconds: u64) -> Result<u64, HarnessError> {
    timestamp
        .checked_add(
            seconds
                .checked_mul(1_000)
                .ok_or_else(|| HarnessError::invalid("schedule delay is too large"))?,
        )
        .ok_or_else(|| HarnessError::invalid("schedule target is too large"))
}

fn now_ms() -> Result<u64, HarnessError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| HarnessError::execution(format!("system clock error: {error}")))?
        .as_millis()
        .try_into()
        .map_err(|_| HarnessError::execution("timestamp exceeds u64"))
}

#[derive(Clone, Copy)]
enum Operation {
    Create,
    List,
    Delete,
}

impl Operation {
    const fn effect(self) -> ternilo_kernel::ToolEffect {
        match self {
            Self::List => ternilo_kernel::ToolEffect::ReadOnly,
            Self::Create | Self::Delete => ternilo_kernel::ToolEffect::Dangerous,
        }
    }
}

struct ScheduleTool {
    runtime: Arc<ScheduleRuntime>,
    operation: Operation,
}

impl ToolHandler for ScheduleTool {
    fn execute<'a>(
        &'a self,
        context: ToolExecutionContext,
        arguments: Value,
    ) -> Pin<Box<dyn Future<Output = Result<ToolOutput, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            let now = now_ms()?;
            let value = match self.operation {
                Operation::Create => {
                    schedule_view(&self.runtime.create(context.run_id, &arguments).await?, now)?
                }
                Operation::List => Value::Array(
                    self.runtime
                        .list()
                        .await?
                        .iter()
                        .map(|record| schedule_view(record, now))
                        .collect::<Result<Vec<_>, _>>()?,
                ),
                Operation::Delete => {
                    let id = ScheduleId::new(required_string(&arguments, "id")?);
                    let deleted = self.runtime.delete(context.run_id, id.clone()).await?;
                    json!({ "id": id, "deleted": deleted })
                }
            };
            Ok(ToolOutput {
                content: serde_json::to_string_pretty(&value).map_err(|error| {
                    HarnessError::execution(format!("render schedule result: {error}"))
                })?,
                is_error: false,
            })
        })
    }
}

fn schedule_view(record: &ScheduleRecord, now: u64) -> Result<Value, HarnessError> {
    let Value::Object(mut value) = serde_json::to_value(record)
        .map_err(|error| HarnessError::execution(format!("serialize schedule: {error}")))?
    else {
        return Err(HarnessError::execution(
            "serialized schedule was not an object",
        ));
    };
    value.insert(
        "state".to_owned(),
        Value::String(
            if record.scheduled_at_ms > now {
                "scheduled"
            } else {
                "overdue"
            }
            .to_owned(),
        ),
    );
    value.insert(
        "delivery_mode".to_owned(),
        Value::String("session_local".to_owned()),
    );
    Ok(Value::Object(value))
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

fn optional_u64(arguments: &Value, name: &str) -> Result<Option<u64>, HarnessError> {
    match arguments.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .map(Some)
            .ok_or_else(|| HarnessError::invalid(format!("{name} must be a non-negative integer"))),
    }
}

fn tool_specs() -> [(ToolSpec, Operation); 3] {
    [
        (
            ToolSpec {
                name: "schedule_create".to_owned(),
                description: "Create a durable session-local one-shot or fixed-rate reminder. Use exactly one timing selector.".to_owned(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "prompt": { "type": "string" },
                        "after_seconds": { "type": "integer", "minimum": 1 },
                        "at": { "type": "string", "format": "date-time" },
                        "every_seconds": { "type": "integer", "minimum": 300 }
                    },
                    "required": ["prompt"],
                    "oneOf": [
                        { "required": ["after_seconds"] },
                        { "required": ["at"] },
                        { "required": ["every_seconds"] }
                    ],
                    "additionalProperties": false
                }),
            },
            Operation::Create,
        ),
        (
            ToolSpec {
                name: "schedule_list".to_owned(),
                description: "List active reminders owned by the current live session.".to_owned(),
                input_schema: json!({
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false
                }),
            },
            Operation::List,
        ),
        (
            ToolSpec {
                name: "schedule_delete".to_owned(),
                description: "Delete one active reminder by its exact schedule id.".to_owned(),
                input_schema: json!({
                    "type": "object",
                    "properties": { "id": { "type": "string" } },
                    "required": ["id"],
                    "additionalProperties": false
                }),
            },
            Operation::Delete,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replay_advances_and_removes_schedules() {
        let record = ScheduleRecord {
            id: ScheduleId::new("schedule-1"),
            prompt: "remember".to_owned(),
            rule: ScheduleRule::Every { every_seconds: 300 },
            scheduled_at_ms: 1_000,
            created_at_ms: 0,
        };
        let events = [
            event(SessionEventKind::ScheduleChanged {
                change: ScheduleChange::Create {
                    schedule: record.clone(),
                },
            }),
            event(SessionEventKind::ScheduleChanged {
                change: ScheduleChange::Dispatch {
                    id: record.id.clone(),
                    run_id: None,
                    accepted_at_ms: 1_001,
                    next_scheduled_at_ms: Some(301_000),
                },
            }),
            event(SessionEventKind::ScheduleChanged {
                change: ScheduleChange::Delete {
                    id: record.id.clone(),
                },
            }),
        ];
        assert!(fold_schedules(&events).unwrap().is_empty());
    }

    fn event(kind: SessionEventKind) -> SessionEvent {
        SessionEvent {
            seq: 0,
            occurred_at_ms: 0,
            run_id: RunId::new("run"),
            kind,
        }
    }
}
