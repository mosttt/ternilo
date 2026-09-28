use std::{future::Future, pin::Pin, sync::Arc};

use serde_json::json;
use ternilo_builtins::{ModelAttemptObserver, ModelAttemptReport};
use ternilo_control::{ModelRequestSettlement, ModelRequestState};
use ternilo_protocol::{HarnessError, ProviderProtocol};

use crate::platform::{http::now_ms, models::usage::parse_usage};

use super::access::AcceptedModelCall;

pub(super) struct AttemptObserver {
    pub(super) call: Arc<AcceptedModelCall>,
    pub(super) protocol: ProviderProtocol,
}

impl ModelAttemptObserver for AttemptObserver {
    fn before_attempt<'a>(
        &'a self,
        attempt: u32,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(self.call.begin_attempt(attempt))
    }

    fn after_attempt<'a>(
        &'a self,
        report: ModelAttemptReport,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            let state = match &report.error {
                None => ModelRequestState::Completed,
                Some(error) if error.is_cancelled() => ModelRequestState::Cancelled,
                Some(_) => ModelRequestState::Failed,
            };
            let error_code = report.error.as_ref().map(|error| {
                if error.is_cancelled() {
                    "request_cancelled".to_owned()
                } else if let Some(status) = report.http_status.filter(|status| *status >= 400) {
                    format!("upstream_http_{status}")
                } else if error.message == "model stream ended before its completion event" {
                    "upstream_stream_incomplete".to_owned()
                } else {
                    "upstream_model_error".to_owned()
                }
            });
            self.call
                .state
                .store
                .settle_model_attempt(
                    &self.call.request_id,
                    report.attempt,
                    &ModelRequestSettlement {
                        state,
                        usage: parse_usage(&json!({"usage":report.usage}), self.protocol),
                        upstream_request_id: report.upstream_request_id,
                        error_code,
                    },
                    now_ms()?,
                )
                .await?;
            Ok(())
        })
    }
}
