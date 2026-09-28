use std::{future::Future, pin::Pin};

use ternilo_protocol::{HarnessError, ReferenceContext, SubmissionReference};

/// Resolve submitted references after the invocation owns its working directory.
pub trait InputReferenceResolver: Send + Sync + 'static {
    fn resolve<'a>(
        &'a self,
        references: Vec<SubmissionReference>,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<ReferenceContext>, HarnessError>> + Send + 'a>>;
}
