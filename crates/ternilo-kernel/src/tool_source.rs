use std::{future::Future, pin::Pin};

use ternilo_protocol::{HarnessError, SessionServiceSnapshot};

use crate::{RunCancellation, ToolRegistration};

/// A configured integration whose process is started only by admitted execution.
pub trait DeferredToolSource: Send + Sync {
    fn snapshot(&self) -> SessionServiceSnapshot;

    /// Describe tools whose schemas are known without starting an external service.
    fn initial_tools(&self) -> Vec<ToolRegistration> {
        Vec::new()
    }

    /// Prepare automatic discovery. A manually stopped source must remain stopped.
    fn prepare<'a>(
        &'a self,
        cancellation: RunCancellation,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<ToolRegistration>, HarnessError>> + Send + 'a>>;

    fn start<'a>(
        &'a self,
        cancellation: RunCancellation,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<ToolRegistration>, HarnessError>> + Send + 'a>>;

    fn stop<'a>(&'a self) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>>;

    /// Unmounting also cancels in-flight use and permanently prevents new starts.
    fn shutdown<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>>;
}
