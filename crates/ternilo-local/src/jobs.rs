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
use serde_json::Value;
use ternilo_kernel::{
    HarnessPlugin, Jobs, JobsProvider, PluginFactory, PluginManifest, RunCancellation,
    RunEnvironment, RunEnvironmentClient, Shell, ShellClient,
};
use ternilo_protocol::{HarnessError, JobId, JobSnapshot, JobStatus, ShellRequest, ShellResult};

pub const LOCAL_JOBS_KIND: &str = "ternilo.jobs.local";

#[derive(schemars::JsonSchema)]
struct LocalJobsConfig {}

component_descriptor! {
    static DESCRIPTOR: () {
        id: "ternilo/local-jobs@1",
        requires: [Shell, RunEnvironment],
        provides: [Jobs],
    }
}

#[must_use]
pub fn local_jobs_factory() -> PluginFactory {
    PluginFactory::new(
        PluginManifest {
            kind: LOCAL_JOBS_KIND,
            requires: &["ternilo/shell@1", "ternilo/run-environment@1"],
            provides: &["ternilo/jobs@1"],
        },
        |value| {
            parse_empty_config(value)?;
            Ok(Arc::new(LocalJobsPlugin))
        },
    )
    .with_description("保存本地后台进程状态，并复用 shell sandbox 与进程树回收。")
    .with_config_schema::<LocalJobsConfig>()
}

fn parse_empty_config(value: Value) -> Result<(), HarnessError> {
    let value = if value.is_null() {
        serde_json::json!({})
    } else {
        value
    };
    let object = value
        .as_object()
        .ok_or_else(|| HarnessError::composition("local jobs config must be an object"))?;
    if object.is_empty() {
        Ok(())
    } else {
        Err(HarnessError::composition(
            "local jobs config has unknown fields",
        ))
    }
}

struct LocalJobsPlugin;

impl HarnessPlugin for LocalJobsPlugin {
    fn descriptor(&self) -> &'static ComponentDescriptor {
        &DESCRIPTOR
    }

    fn activate(&self, context: ComponentContext) -> Activation {
        let environment = context
            .context()
            .service::<RunEnvironment>()
            .expect("local jobs declares RunEnvironment");
        let shell = context
            .context()
            .service::<Shell>()
            .expect("local jobs declares Shell");
        let route = context.context().clone();
        let scope = context.scope().clone();
        Activation::Once(Box::pin(async move {
            let provider: Arc<dyn JobsProvider> = Arc::new(LocalJobs {
                environment,
                shell,
                next_id: AtomicU64::new(1),
                records: Mutex::new(BTreeMap::new()),
            });
            scope
                .provide::<Jobs>(&route, provider)
                .await
                .map_err(|error| {
                    linorun_core::ActivationFailure::user(format!("provide local jobs: {error}"))
                })?;
            Ok(None)
        }))
    }
}

struct JobRecord {
    snapshot: JobSnapshot,
    task: Option<tokio::task::JoinHandle<Result<ShellResult, HarnessError>>>,
}

struct LocalJobs {
    environment: RunEnvironmentClient,
    shell: ShellClient,
    next_id: AtomicU64,
    records: Mutex<BTreeMap<JobId, JobRecord>>,
}

impl LocalJobs {
    async fn refresh(&self, job_id: &JobId) -> Result<JobSnapshot, HarnessError> {
        let task = {
            let mut records = self
                .records
                .lock()
                .map_err(|_| HarnessError::execution("job registry lock poisoned"))?;
            let record = records
                .get_mut(job_id)
                .ok_or_else(|| HarnessError::invalid(format!("unknown background job {job_id}")))?;
            if record
                .task
                .as_ref()
                .is_some_and(tokio::task::JoinHandle::is_finished)
            {
                record.task.take()
            } else {
                None
            }
        };
        if let Some(task) = task {
            let outcome = task.await;
            let mut records = self
                .records
                .lock()
                .map_err(|_| HarnessError::execution("job registry lock poisoned"))?;
            let record = records.get_mut(job_id).ok_or_else(|| {
                HarnessError::execution(format!("background job {job_id} disappeared"))
            })?;
            match outcome {
                Ok(Ok(result)) => {
                    record.snapshot.status =
                        if result.timed_out || result.exit_code.is_some_and(|code| code != 0) {
                            JobStatus::Failed
                        } else {
                            JobStatus::Completed
                        };
                    record.snapshot.result = Some(result);
                }
                Ok(Err(error)) => {
                    record.snapshot.status = JobStatus::Failed;
                    record.snapshot.error = Some(error.to_string());
                }
                Err(error) if error.is_cancelled() => {
                    record.snapshot.status = JobStatus::Cancelled;
                }
                Err(error) => {
                    record.snapshot.status = JobStatus::Failed;
                    record.snapshot.error = Some(error.to_string());
                }
            }
        }
        self.records
            .lock()
            .map_err(|_| HarnessError::execution("job registry lock poisoned"))?
            .get(job_id)
            .map(|record| record.snapshot.clone())
            .ok_or_else(|| HarnessError::invalid(format!("unknown background job {job_id}")))
    }
}

impl Drop for LocalJobs {
    fn drop(&mut self) {
        if let Ok(records) = self.records.get_mut() {
            for record in records.values_mut() {
                if let Some(task) = record.task.take() {
                    task.abort();
                }
            }
        }
    }
}

impl JobsProvider for LocalJobs {
    fn spawn<'a>(
        &'a self,
        _: CallContext<()>,
        request: ShellRequest,
    ) -> Pin<Box<dyn Future<Output = Result<JobSnapshot, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            if request.command.trim().is_empty() {
                return Err(HarnessError::invalid("job command must not be empty"));
            }
            let job_id = JobId::new(format!(
                "job-{}",
                self.next_id.fetch_add(1, Ordering::Relaxed)
            ));
            let snapshot = JobSnapshot {
                job_id: job_id.clone(),
                command: request.command.clone(),
                status: JobStatus::Running,
                result: None,
                error: None,
            };
            let shell = self.shell.clone();
            // Acquire before detaching so the parent turn cannot hand off the directory
            // between accepting the job and starting its process.
            let lease = self
                .environment
                .acquire_workspace(RunCancellation::new())
                .await?;
            let task = tokio::spawn(async move {
                let _lease = lease;
                shell.execute(request).await
            });
            self.records
                .lock()
                .map_err(|_| HarnessError::execution("job registry lock poisoned"))?
                .insert(
                    job_id,
                    JobRecord {
                        snapshot: snapshot.clone(),
                        task: Some(task),
                    },
                );
            Ok(snapshot)
        })
    }

    fn get<'a>(
        &'a self,
        _: CallContext<()>,
        job_id: JobId,
    ) -> Pin<Box<dyn Future<Output = Result<JobSnapshot, HarnessError>> + Send + 'a>> {
        Box::pin(async move { self.refresh(&job_id).await })
    }

    fn list<'a>(
        &'a self,
        _: CallContext<()>,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<JobSnapshot>, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            let ids = self
                .records
                .lock()
                .map_err(|_| HarnessError::execution("job registry lock poisoned"))?
                .keys()
                .cloned()
                .collect::<Vec<_>>();
            let mut snapshots = Vec::with_capacity(ids.len());
            for id in ids {
                snapshots.push(self.refresh(&id).await?);
            }
            Ok(snapshots)
        })
    }

    fn kill<'a>(
        &'a self,
        _: CallContext<()>,
        job_id: JobId,
    ) -> Pin<Box<dyn Future<Output = Result<JobSnapshot, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            let task = {
                let mut records = self
                    .records
                    .lock()
                    .map_err(|_| HarnessError::execution("job registry lock poisoned"))?;
                let record = records.get_mut(&job_id).ok_or_else(|| {
                    HarnessError::invalid(format!("unknown background job {job_id}"))
                })?;
                record.task.take()
            };
            if let Some(task) = task {
                task.abort();
                let _ = task.await;
                let mut records = self
                    .records
                    .lock()
                    .map_err(|_| HarnessError::execution("job registry lock poisoned"))?;
                let record = records.get_mut(&job_id).ok_or_else(|| {
                    HarnessError::execution(format!("background job {job_id} disappeared"))
                })?;
                record.snapshot.status = JobStatus::Cancelled;
            }
            self.refresh(&job_id).await
        })
    }
}
