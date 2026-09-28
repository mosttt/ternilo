use std::{fmt, future::Future, pin::Pin, sync::Arc};

use ternilo_protocol::HarnessError;

use crate::RunCancellation;

/// Retain directory ownership for an invocation or a managed background resource.
#[derive(Clone)]
pub struct WorkspaceExecutionLease {
    _owner: Arc<dyn Send + Sync>,
}

impl WorkspaceExecutionLease {
    #[must_use]
    pub fn hold<T: Send + Sync + 'static>(owner: T) -> Self {
        Self {
            _owner: Arc::new(owner),
        }
    }
}

impl fmt::Debug for WorkspaceExecutionLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WorkspaceExecutionLease")
            .finish_non_exhaustive()
    }
}

/// A trusted host binds this admission gate to one session's execution scope and directory.
pub trait WorkspaceExecution: Send + Sync {
    fn try_acquire<'a>(
        &'a self,
    ) -> Pin<
        Box<dyn Future<Output = Result<Option<WorkspaceExecutionLease>, HarnessError>> + Send + 'a>,
    >;

    fn acquire<'a>(
        &'a self,
        cancellation: RunCancellation,
    ) -> Pin<Box<dyn Future<Output = Result<WorkspaceExecutionLease, HarnessError>> + Send + 'a>>;
}
