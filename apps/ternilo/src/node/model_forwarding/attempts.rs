use super::{
    Duration, ExecutorFrame, Future, HarnessError, ModelRequestId, Pin, RunCancellation, mpsc,
};
use ternilo_builtins::{ModelAttemptObserver, ModelAttemptReport};
use ternilo_protocol::{ComputerModelAttempt, ProviderProtocol};
use tokio::sync::Mutex;

pub(super) struct SourceAttempts {
    pub(super) request_id: ModelRequestId,
    pub(super) outgoing: mpsc::Sender<ExecutorFrame>,
    pub(super) cancellation: RunCancellation,
    pub(super) protocol: ProviderProtocol,
    pub(super) approvals: Mutex<mpsc::Receiver<(u32, Option<HarnessError>)>>,
}

impl SourceAttempts {
    async fn send(&self, event: ComputerModelAttempt) -> Result<(), HarnessError> {
        tokio::time::timeout(
            Duration::from_secs(2),
            self.outgoing.send(ExecutorFrame::ModelAttempt {
                request_id: self.request_id.clone(),
                event,
            }),
        )
        .await
        .map_err(|_| HarnessError::unavailable("model attempt report stalled"))?
        .map_err(|_| HarnessError::unavailable("model attempt connection closed"))
    }
}

impl ModelAttemptObserver for SourceAttempts {
    fn before_attempt<'a>(
        &'a self,
        attempt: u32,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            self.cancellation.check()?;
            self.send(ComputerModelAttempt::Started { attempt }).await?;
            // The initial request is its first permit. Every retry needs fresh
            // Server authorization before another upstream call can be sent.
            if attempt == 1 {
                return Ok(());
            }
            let mut approvals = self.approvals.lock().await;
            let (permitted, error) = tokio::select! {
                biased;
                () = self.cancellation.cancelled() => return Err(HarnessError::cancelled("model retry cancelled")),
                result = tokio::time::timeout(Duration::from_secs(10), approvals.recv()) => result
                    .map_err(|_| HarnessError::unavailable("model retry authorization timed out"))?
                    .ok_or_else(|| HarnessError::cancelled("model retry authorization closed"))?,
            };
            if permitted != attempt {
                return Err(HarnessError::policy(
                    "model retry permit has the wrong attempt",
                ));
            }
            error.map_or(Ok(()), Err)
        })
    }

    fn after_attempt<'a>(
        &'a self,
        report: ModelAttemptReport,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(self.send(ComputerModelAttempt::Finished {
            attempt: report.attempt,
            http_status: report.http_status,
            usage:
                report.usage.as_ref().and_then(|usage| {
                    ternilo_builtins::normalize_provider_usage(usage, self.protocol)
                }),
            upstream_request_id: report.upstream_request_id,
            error_code: report.error.map(|error| error.code),
        }))
    }
}
