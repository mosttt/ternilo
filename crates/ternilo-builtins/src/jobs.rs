use std::{collections::BTreeMap, future::Future, pin::Pin, sync::Arc, time::Duration};

use linorun_core::{Activation, CleanupError, ComponentContext, ComponentDescriptor, effect};
use linorun_macros::component_descriptor;
use serde::Deserialize;
use serde_json::{Value, json};
use ternilo_kernel::{
    HarnessPlugin, Jobs, JobsClient, PluginFactory, PluginManifest, Sessions, SessionsClient,
    ToolExecutionContext, ToolHandler, ToolRegistration, Tools, ToolsClient,
};
use ternilo_protocol::{
    HarnessError, JobId, JobSnapshot, JobStatus, RunId, SessionEventKind, ShellRequest, ToolOutput,
    ToolSpec,
};

use crate::{EmptyConfig, factory as make_factory, parse_config};

pub const KIND: &str = "ternilo.tools.jobs";

component_descriptor! {
    static DESCRIPTOR: () {
        id: "ternilo/builtin-job-tools@1",
        requires: [Tools, Jobs, Sessions],
        provides: [],
    }
}

pub fn factory() -> PluginFactory {
    make_factory(
        PluginManifest {
            kind: KIND,
            requires: &["ternilo/tools@1", "ternilo/jobs@1", "ternilo/sessions@1"],
            provides: &[],
        },
        |value| {
            let _: EmptyConfig = parse_config(value)?;
            Ok(Arc::new(JobToolsPlugin))
        },
    )
    .with_config_schema::<EmptyConfig>()
}

struct JobToolsPlugin;

impl HarnessPlugin for JobToolsPlugin {
    fn descriptor(&self) -> &'static ComponentDescriptor {
        &DESCRIPTOR
    }

    fn activate(&self, context: ComponentContext) -> Activation {
        let tools = context
            .context()
            .service::<Tools>()
            .expect("job tools declares Tools");
        let jobs = context
            .context()
            .service::<Jobs>()
            .expect("job tools declares Jobs");
        let sessions = context
            .context()
            .service::<Sessions>()
            .expect("job tools declares Sessions");
        Activation::Once(Box::pin(async move {
            let active = Arc::new(tokio::sync::Mutex::new(BTreeMap::new()));
            let definitions = [
                (
                    ToolSpec {
                        name: "job_start".to_owned(),
                        description: "Start a platform shell command as an owner-scoped background job.".to_owned(),
                        input_schema: json!({
                            "type": "object",
                            "properties": {
                                "command": { "type": "string" },
                                "timeout_ms": { "type": "integer", "minimum": 100, "maximum": 300_000, "default": 300_000 }
                            },
                            "required": ["command"],
                            "additionalProperties": false
                        }),
                    },
                    JobOperation::Start,
                ),
                (
                    ToolSpec {
                        name: "job_output".to_owned(),
                        description: "Poll one background job and return its latest terminal status and retained output.".to_owned(),
                        input_schema: job_id_schema(),
                    },
                    JobOperation::Output,
                ),
                (
                    ToolSpec {
                        name: "job_list".to_owned(),
                        description: "List all background jobs owned by this agent session.".to_owned(),
                        input_schema: json!({ "type": "object", "properties": {}, "additionalProperties": false }),
                    },
                    JobOperation::List,
                ),
                (
                    ToolSpec {
                        name: "job_kill".to_owned(),
                        description: "Cancel one running background job.".to_owned(),
                        input_schema: job_id_schema(),
                    },
                    JobOperation::Kill,
                ),
            ];
            let mut registrations = Vec::new();
            for (spec, operation) in definitions {
                registrations.push(
                    tools
                        .register_tool(ToolRegistration {
                            spec,
                            effect: operation.effect(),
                            handler: Arc::new(JobTool {
                                jobs: jobs.clone(),
                                sessions: sessions.clone(),
                                active: Arc::clone(&active),
                                operation,
                            }),
                        })
                        .await
                        .map_err(|error| {
                            linorun_core::ActivationFailure::user(error.to_string())
                        })?,
                );
            }
            Ok(Some(effect::inverse(move || async move {
                settle_active_jobs(&jobs, &sessions, &active).await?;
                unregister_all(&tools, registrations).await
            })))
        }))
    }
}

fn job_id_schema() -> Value {
    json!({
        "type": "object",
        "properties": { "job_id": { "type": "string" } },
        "required": ["job_id"],
        "additionalProperties": false
    })
}

#[derive(Clone, Copy)]
enum JobOperation {
    Start,
    Output,
    List,
    Kill,
}

impl JobOperation {
    const fn effect(self) -> ternilo_kernel::ToolEffect {
        match self {
            Self::Output | Self::List => ternilo_kernel::ToolEffect::ReadOnly,
            Self::Start | Self::Kill => ternilo_kernel::ToolEffect::Mutating,
        }
    }
}

struct JobTool {
    jobs: JobsClient,
    sessions: SessionsClient,
    active: Arc<tokio::sync::Mutex<BTreeMap<JobId, RunId>>>,
    operation: JobOperation,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StartArguments {
    command: String,
    #[serde(default = "default_job_timeout")]
    timeout_ms: u64,
}

const fn default_job_timeout() -> u64 {
    300_000
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JobIdArguments {
    job_id: JobId,
}

impl ToolHandler for JobTool {
    fn execute<'a>(
        &'a self,
        context: ToolExecutionContext,
        arguments: Value,
    ) -> Pin<Box<dyn Future<Output = Result<ToolOutput, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            let result = match self.operation {
                JobOperation::Start => {
                    let arguments: StartArguments = parse_arguments(arguments)?;
                    let snapshot = self
                        .jobs
                        .spawn(
                            context.run_id.clone(),
                            ShellRequest {
                                command: arguments.command,
                                timeout_ms: arguments.timeout_ms,
                                full_access: false,
                                stdin: None,
                                env: std::collections::BTreeMap::new(),
                            },
                        )
                        .await?;
                    publish_job(&self.sessions, &context.run_id, snapshot.clone()).await?;
                    self.active
                        .lock()
                        .await
                        .insert(snapshot.job_id.clone(), context.run_id.clone());
                    monitor_job(
                        self.jobs.clone(),
                        self.sessions.clone(),
                        Arc::clone(&self.active),
                        context.run_id.clone(),
                        snapshot.job_id.clone(),
                    );
                    serde_json::to_value(snapshot)
                }
                JobOperation::Output => {
                    let arguments: JobIdArguments = parse_arguments(arguments)?;
                    let snapshot = self.jobs.get(arguments.job_id).await?;
                    forget_settled(&self.active, &snapshot).await;
                    publish_job(&self.sessions, &context.run_id, snapshot.clone()).await?;
                    serde_json::to_value(snapshot)
                }
                JobOperation::List => {
                    let _: EmptyConfig = parse_arguments(arguments)?;
                    let snapshots = self.jobs.list().await?;
                    for snapshot in &snapshots {
                        forget_settled(&self.active, snapshot).await;
                        publish_job(&self.sessions, &context.run_id, snapshot.clone()).await?;
                    }
                    serde_json::to_value(snapshots)
                }
                JobOperation::Kill => {
                    let arguments: JobIdArguments = parse_arguments(arguments)?;
                    let snapshot = self.jobs.kill(arguments.job_id).await?;
                    forget_settled(&self.active, &snapshot).await;
                    publish_job(&self.sessions, &context.run_id, snapshot.clone()).await?;
                    serde_json::to_value(snapshot)
                }
            }
            .map_err(|error| HarnessError::execution(format!("serialize job result: {error}")))?;
            Ok(ToolOutput {
                content: serde_json::to_string_pretty(&result).map_err(|error| {
                    HarnessError::execution(format!("render job result: {error}"))
                })?,
                is_error: false,
            })
        })
    }
}

async fn forget_settled(
    active: &tokio::sync::Mutex<BTreeMap<JobId, RunId>>,
    snapshot: &JobSnapshot,
) {
    if snapshot.status != JobStatus::Running {
        active.lock().await.remove(&snapshot.job_id);
    }
}

async fn publish_job(
    sessions: &SessionsClient,
    run_id: &RunId,
    job: JobSnapshot,
) -> Result<(), HarnessError> {
    sessions
        .append(run_id.clone(), SessionEventKind::JobUpdated { job })
        .await
        .map(|_| ())
}

fn monitor_job(
    jobs: JobsClient,
    sessions: SessionsClient,
    active: Arc<tokio::sync::Mutex<BTreeMap<JobId, RunId>>>,
    run_id: RunId,
    job_id: JobId,
) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(250)).await;
            let Ok(snapshot) = jobs.get(job_id.clone()).await else {
                return;
            };
            if snapshot.status == JobStatus::Running {
                continue;
            }
            if active.lock().await.remove(&job_id).is_some() {
                let _ = publish_job(&sessions, &run_id, snapshot).await;
            }
            return;
        }
    });
}

async fn settle_active_jobs(
    jobs: &JobsClient,
    sessions: &SessionsClient,
    active: &tokio::sync::Mutex<BTreeMap<JobId, RunId>>,
) -> Result<(), CleanupError> {
    let pending = std::mem::take(&mut *active.lock().await);
    for (job_id, run_id) in pending {
        let snapshot = jobs
            .get(job_id.clone())
            .await
            .map_err(|error| CleanupError::user(error.to_string()))?;
        let snapshot = if snapshot.status == JobStatus::Running {
            jobs.kill(job_id)
                .await
                .map_err(|error| CleanupError::user(error.to_string()))?
        } else {
            snapshot
        };
        publish_job(sessions, &run_id, snapshot)
            .await
            .map_err(|error| CleanupError::user(error.to_string()))?;
    }
    Ok(())
}

fn parse_arguments<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, HarnessError> {
    serde_json::from_value(value)
        .map_err(|error| HarnessError::invalid(format!("invalid job arguments: {error}")))
}

async fn unregister_all(tools: &ToolsClient, registrations: Vec<u64>) -> Result<(), CleanupError> {
    for registration in registrations.into_iter().rev() {
        tools
            .unregister_tool(registration)
            .await
            .map_err(|error| CleanupError::user(error.to_string()))?;
    }
    Ok(())
}
