use std::{future::Future, path::Path, pin::Pin, sync::Arc};

use ternilo_kernel::InputReferenceResolver;
use ternilo_protocol::{HarnessError, ReferenceContext, SessionId, SubmissionReference};

use crate::{
    event_store::JsonlEventStore, fit_reference_contexts, resolve_file_references,
    session_reference_context, state::LocalState,
};

pub(super) struct LocalInputReferences {
    state: Arc<LocalState>,
    session_id: SessionId,
}

impl LocalInputReferences {
    pub(super) fn new(state: Arc<LocalState>, session_id: SessionId) -> Self {
        Self { state, session_id }
    }

    async fn resolve_contexts(
        &self,
        references: &[SubmissionReference],
    ) -> Result<Vec<ReferenceContext>, HarnessError> {
        if references.is_empty() {
            return Ok(Vec::new());
        }
        let current = self
            .state
            .session(self.session_id.as_str())
            .await
            .ok_or_else(|| {
                HarnessError::invalid(format!("unknown session {:?}", self.session_id.as_str()))
            })?;
        let mut contexts =
            resolve_file_references(Path::new(&current.workspace_path), references).await?;
        for reference in references {
            let SubmissionReference::Session {
                session_id: source_id,
                ..
            } = reference
            else {
                continue;
            };
            if source_id == &self.session_id {
                return Err(HarnessError::invalid("a session cannot reference itself"));
            }
            let source = self
                .state
                .session(source_id.as_str())
                .await
                .ok_or_else(|| {
                    HarnessError::invalid(format!(
                        "referenced session {:?} is unavailable",
                        source_id.as_str()
                    ))
                })?;
            if source.identity.tenant_id != current.identity.tenant_id
                || source.identity.user_id != current.identity.user_id
            {
                return Err(HarnessError::policy(
                    "referenced session belongs to another user",
                ));
            }
            let events = JsonlEventStore::new(&self.state.sessions_dir(), source_id)
                .load_events()
                .await?;
            contexts.push(session_reference_context(
                reference,
                &source.title,
                &events,
            )?);
        }
        Ok(fit_reference_contexts(contexts))
    }
}

impl InputReferenceResolver for LocalInputReferences {
    fn resolve<'a>(
        &'a self,
        references: Vec<SubmissionReference>,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<ReferenceContext>, HarnessError>> + Send + 'a>>
    {
        Box::pin(async move { self.resolve_contexts(&references).await })
    }
}
