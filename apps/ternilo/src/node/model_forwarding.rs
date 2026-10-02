use std::{collections::BTreeMap, future::Future, pin::Pin, sync::Arc, time::Duration};

use ternilo_kernel::{ModelOutput, RunCancellation};
use ternilo_local::LocalApplication;
use ternilo_protocol::{ComputerModelRequest, HarnessError, ModelGatewayFrame, ModelRetryFailure};
use ternilo_transport::{ExecutorFrame, ExecutorScope, ModelRequestId};
use tokio::{sync::mpsc, task::JoinSet};

const MAX_CONCURRENT_REQUESTS: usize = 32;

/// These requests belong to one live connection, never the durable command
/// queue. Reconnecting must not replay a potentially billable model request.
pub(super) struct ForwardedModels {
    application: Arc<LocalApplication>,
    scope: ExecutorScope,
    outgoing: mpsc::Sender<ExecutorFrame>,
    active: BTreeMap<ModelRequestId, RunCancellation>,
    permits: BTreeMap<ModelRequestId, mpsc::Sender<(u32, Option<HarnessError>)>>,
    jobs: JoinSet<(ModelRequestId, Result<(), HarnessError>)>,
}

impl ForwardedModels {
    pub(super) fn new(
        application: Arc<LocalApplication>,
        scope: ExecutorScope,
        outgoing: mpsc::Sender<ExecutorFrame>,
    ) -> Self {
        Self {
            application,
            scope,
            outgoing,
            active: BTreeMap::new(),
            permits: BTreeMap::new(),
            jobs: JoinSet::new(),
        }
    }

    pub(super) fn is_empty(&self) -> bool {
        self.jobs.is_empty()
    }

    pub(super) fn start(
        &mut self,
        request_id: ModelRequestId,
        scope: &ExecutorScope,
        request: ComputerModelRequest,
    ) -> Result<(), HarnessError> {
        request_id.validate()?;
        if scope != &self.scope {
            return Err(HarnessError::policy(
                "forwarded model request has the wrong computer scope",
            ));
        }
        if self.active.contains_key(&request_id) {
            return Err(HarnessError::invalid(
                "duplicate active model request identifier",
            ));
        }
        if self.active.len() >= MAX_CONCURRENT_REQUESTS {
            return self
                .outgoing
                .try_send(ExecutorFrame::ModelOutput {
                    request_id,
                    frame: Box::new(ModelGatewayFrame::Error {
                        error: HarnessError::unavailable(
                            "the source computer model request limit was reached",
                        ),
                    }),
                })
                .map_err(|_| {
                    HarnessError::unavailable("model output connection is not accepting frames")
                });
        }
        let cancellation = RunCancellation::new();
        self.active.insert(request_id.clone(), cancellation.clone());
        let (permits, approvals) = mpsc::channel(1);
        self.permits.insert(request_id.clone(), permits);
        let application = Arc::clone(&self.application);
        let outgoing = self.outgoing.clone();
        self.jobs.spawn(async move {
            let output = Arc::new(ForwardedOutput {
                request_id: request_id.clone(),
                outgoing: outgoing.clone(),
                cancellation: cancellation.clone(),
            });
            let protocol = request.protocol;
            let result = application
                .complete_forwarded_model(
                    request,
                    output,
                    cancellation.clone(),
                    Arc::new(attempts::SourceAttempts {
                        request_id: request_id.clone(),
                        outgoing: outgoing.clone(),
                        cancellation,
                        protocol,
                        approvals: tokio::sync::Mutex::new(approvals),
                    }),
                )
                .await;
            let frame = match result {
                Ok(response) => ModelGatewayFrame::Complete { response },
                Err(mut error) => {
                    // Upstream errors may contain private URLs, headers or echoed credentials.
                    if matches!(
                        error.code,
                        ternilo_protocol::ErrorCode::Execution
                            | ternilo_protocol::ErrorCode::Composition
                    ) {
                        "the source computer could not complete the model request"
                            .clone_into(&mut error.message);
                    }
                    ModelGatewayFrame::Error { error }
                }
            };
            let result = send_with_timeout(&outgoing, request_id.clone(), frame).await;
            (request_id, result)
        });
        Ok(())
    }

    pub(super) fn cancel(&self, request_id: &ModelRequestId) -> Result<(), HarnessError> {
        request_id.validate()?;
        if let Some(cancellation) = self.active.get(request_id) {
            cancellation.cancel();
        }
        Ok(())
    }

    pub(super) fn permit(
        &self,
        request_id: &ModelRequestId,
        attempt: u32,
        error: Option<HarnessError>,
    ) -> Result<(), HarnessError> {
        request_id.validate()?;
        if let Some(permits) = self.permits.get(request_id) {
            permits
                .try_send((attempt, error))
                .map_err(|_| HarnessError::invalid("unexpected model retry permission"))?;
        }
        Ok(())
    }

    pub(super) async fn reap(&mut self) -> Result<(), HarnessError> {
        if let Some(joined) = self.jobs.join_next().await {
            let (id, result) = joined.map_err(|_| {
                HarnessError::execution("source computer model task ended unexpectedly")
            })?;
            self.active.remove(&id);
            self.permits.remove(&id);
            result?;
        }
        Ok(())
    }

    pub(super) async fn shutdown(&mut self) {
        for cancellation in self.active.values() {
            cancellation.cancel();
        }
        self.jobs.shutdown().await;
        self.active.clear();
        self.permits.clear();
    }
}

async fn send_with_timeout(
    outgoing: &mpsc::Sender<ExecutorFrame>,
    request_id: ModelRequestId,
    frame: ModelGatewayFrame,
) -> Result<(), HarnessError> {
    tokio::time::timeout(
        Duration::from_secs(2),
        outgoing.send(ExecutorFrame::ModelOutput {
            request_id,
            frame: Box::new(frame),
        }),
    )
    .await
    .map_err(|_| HarnessError::unavailable("model output connection stopped consuming frames"))?
    .map_err(|_| HarnessError::unavailable("model output connection was closed"))
}

struct ForwardedOutput {
    request_id: ModelRequestId,
    outgoing: mpsc::Sender<ExecutorFrame>,
    cancellation: RunCancellation,
}

impl ForwardedOutput {
    async fn send(&self, frame: ModelGatewayFrame) -> Result<(), HarnessError> {
        tokio::select! {
            biased;
            () = self.cancellation.cancelled() => Err(HarnessError::cancelled("forwarded model request was cancelled")),
            result = self.outgoing.send(ExecutorFrame::ModelOutput {
                request_id: self.request_id.clone(), frame: Box::new(frame),
            }) => result.map_err(|_| HarnessError::cancelled("model output connection was closed")),
        }
    }
}

impl ModelOutput for ForwardedOutput {
    fn emit<'a>(
        &'a self,
        delta: String,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(self.send(ModelGatewayFrame::Delta { delta }))
    }

    fn emit_reasoning<'a>(
        &'a self,
        delta: String,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(self.send(ModelGatewayFrame::ReasoningDelta { delta }))
    }

    fn retry_scheduled<'a>(
        &'a self,
        retry: u32,
        max_retries: u32,
        delay_ms: u64,
        failure: ModelRetryFailure,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(self.send(ModelGatewayFrame::RetryScheduled {
            retry,
            max_retries,
            delay_ms,
            failure: ModelRetryFailure {
                message: "the source computer model is temporarily unavailable".to_owned(),
                code: failure.code,
            },
        }))
    }

    fn retry_started<'a>(
        &'a self,
        retry: u32,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(self.send(ModelGatewayFrame::RetryStarted { retry }))
    }

    fn retry_cancelled<'a>(
        &'a self,
        retry: u32,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(send_with_timeout(
            &self.outgoing,
            self.request_id.clone(),
            ModelGatewayFrame::RetryCancelled { retry },
        ))
    }
}

mod attempts;
#[cfg(test)]
mod tests;
