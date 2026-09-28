use std::{future::Future, pin::Pin};

use serde_json::Value;
use ternilo_protocol::HarnessError;

/// A server-observed upstream attempt, independent of a harness execution lease.
#[derive(Clone, Debug)]
pub struct ModelAttemptReport {
    pub attempt: u32,
    pub upstream_request_id: Option<String>,
    pub http_status: Option<u16>,
    pub usage: Option<Value>,
    pub error: Option<HarnessError>,
    pub retryable: bool,
}

impl ModelAttemptReport {
    pub(super) const fn new(attempt: u32) -> Self {
        Self {
            attempt,
            upstream_request_id: None,
            http_status: None,
            usage: None,
            error: None,
            retryable: false,
        }
    }

    pub(super) fn observe(&mut self, value: &Value) {
        if let Some(usage) = value
            .get("usage")
            .or_else(|| value.get("usageMetadata"))
            .filter(|usage| usage.is_object())
        {
            if let Some(previous) = &mut self.usage {
                merge_usage(previous, usage);
            } else {
                self.usage = Some(usage.clone());
            }
        }
        if let Some(response) = value.get("response") {
            self.observe(response);
        }
        if let Some(message) = value.get("message") {
            self.observe(message);
        }
    }
}

fn merge_usage(previous: &mut Value, current: &Value) {
    if current.is_null() {
        return;
    }
    if let (Some(previous), Some(current)) = (previous.as_object_mut(), current.as_object()) {
        for (name, value) in current {
            if let Some(previous) = previous.get_mut(name) {
                merge_usage(previous, value);
            } else {
                previous.insert(name.clone(), value.clone());
            }
        }
    } else {
        *previous = current.clone();
    }
}

/// Both callbacks are awaited to completion, including after cancellation.
/// Callers must cancel through `RunCancellation` instead of dropping the model
/// future while a callback may own a database transaction.
pub trait ModelAttemptObserver: Send + Sync + 'static {
    fn before_attempt<'a>(
        &'a self,
        attempt: u32,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>>;

    fn after_attempt<'a>(
        &'a self,
        report: ModelAttemptReport,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>>;
}
