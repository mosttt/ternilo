use super::{ModelAttemptObserver, ModelAttemptReport, normalize_provider_usage};
use std::{future::Future, pin::Pin};
use ternilo_kernel::SessionsClient;
use ternilo_protocol::{HarnessError, ProviderUsageRoute, RunId, SessionEventKind, SessionId};
use tokio::sync::Mutex;

pub(super) struct SessionUsageObserver {
    sessions: SessionsClient,
    session_id: SessionId,
    run_id: RunId,
    step: u32,
    route: ProviderUsageRoute,
    started_seq: Mutex<Option<u64>>,
}

impl SessionUsageObserver {
    pub(super) fn new(
        sessions: SessionsClient,
        session_id: SessionId,
        run_id: RunId,
        step: u32,
        route: ProviderUsageRoute,
    ) -> Self {
        Self {
            sessions,
            session_id,
            run_id,
            step,
            route,
            started_seq: Mutex::new(None),
        }
    }
}

impl ModelAttemptObserver for SessionUsageObserver {
    fn before_attempt<'a>(
        &'a self,
        attempt: u32,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            let event = self
                .sessions
                .append(
                    self.run_id.clone(),
                    SessionEventKind::ProviderUsageStarted {
                        source_session_id: Some(self.session_id.clone()),
                        step: self.step,
                        attempt,
                        route: self.route.clone(),
                    },
                )
                .await?;
            *self.started_seq.lock().await = Some(event.seq);
            Ok(())
        })
    }

    fn after_attempt<'a>(
        &'a self,
        report: ModelAttemptReport,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            let started_seq = self
                .started_seq
                .lock()
                .await
                .take()
                .expect("each observed attempt starts before settlement");
            let usage = report
                .usage
                .as_ref()
                .and_then(|raw| normalize_provider_usage(raw, self.route.protocol));
            self.sessions
                .append(
                    self.run_id.clone(),
                    SessionEventKind::ProviderUsageFinished {
                        started_seq,
                        usage,
                        upstream_request_id: report.upstream_request_id,
                        error_code: report.error.map(|error| error.code),
                    },
                )
                .await?;
            Ok(())
        })
    }
}
