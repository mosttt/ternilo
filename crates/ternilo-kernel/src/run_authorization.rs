use std::{future::Future, pin::Pin};
use ternilo_protocol::{HarnessError, RunId};

/// Check the original input's authority before restoring or starting derived work.
pub trait RunAuthorization: Send + Sync {
    fn check<'a>(
        &'a self,
        run_id: &'a RunId,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>>;
}

/// The process supervisor retains ownership until stop has actually finished.
pub trait ExecutionResourceControl: Send + Sync {
    fn stop<'a>(&'a self) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>>;
    fn is_finished(&self) -> bool;
}

pub trait ExecutionResourceRegistry: Send + Sync {
    fn register<'a>(
        &'a self,
        run_id: RunId,
        resource_id: String,
        control: std::sync::Arc<dyn ExecutionResourceControl>,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>>;
}
