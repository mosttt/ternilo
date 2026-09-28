use super::{LiveSessions, LocalState, model_origins::InputOrigins};
use std::{
    future::Future,
    path::PathBuf,
    pin::Pin,
    sync::{Arc, RwLock, Weak},
};
use ternilo_kernel::{RunCancellation, WorkspaceExecution, WorkspaceExecutionLease};
use ternilo_protocol::{HarnessError, InputAuthor, SessionEventKind, UserId};

pub(super) struct SessionDirectoryExecution {
    coordinator: crate::DirectoryCoordinator,
    origins: InputOrigins,
    account_owner: Arc<RwLock<Option<UserId>>>,
    session_id: String,
    scope: String,
    workspace: PathBuf,
}

impl SessionDirectoryExecution {
    pub(super) fn bind(
        coordinator: crate::DirectoryCoordinator,
        state: Arc<LocalState>,
        live: Weak<LiveSessions>,
        account_owner: Arc<RwLock<Option<UserId>>>,
        session_id: String,
        scope: String,
        workspace: PathBuf,
    ) -> Arc<dyn WorkspaceExecution> {
        Arc::new(Self {
            coordinator,
            origins: InputOrigins::new(state, live),
            account_owner,
            session_id,
            scope,
            workspace,
        })
    }

    async fn binding(&self) -> Result<Arc<dyn WorkspaceExecution>, HarnessError> {
        let events = self.origins.events(&self.session_id).await?;
        if let Some(event) = events
            .iter()
            .rev()
            .find(|event| matches!(event.kind, SessionEventKind::UserMessage { .. }))
        {
            let origin = self
                .origins
                .resolve(&self.session_id, &event.run_id)
                .await?;
            let user = match origin.provenance.map(|input| input.author) {
                Some(InputAuthor::Local) => Some("local-user".to_owned()),
                Some(InputAuthor::Account { user_id, .. }) => {
                    let owner = self
                        .account_owner
                        .read()
                        .expect("directory account owner lock");
                    let user = format!("account:{}", user_id.as_str());
                    if owner.as_ref() == Some(&user_id) {
                        return Ok(self
                            .coordinator
                            .bind_computer_owner(user, self.workspace.clone()));
                    }
                    Some(user)
                }
                _ => None,
            };
            if let Some(user) = user {
                return Ok(self.coordinator.bind_user(user, self.workspace.clone()));
            }
        }
        Ok(self
            .coordinator
            .bind(self.scope.clone(), self.workspace.clone()))
    }
}

impl WorkspaceExecution for SessionDirectoryExecution {
    fn try_acquire<'a>(
        &'a self,
    ) -> Pin<
        Box<dyn Future<Output = Result<Option<WorkspaceExecutionLease>, HarnessError>> + Send + 'a>,
    > {
        Box::pin(async move { self.binding().await?.try_acquire().await })
    }

    fn acquire<'a>(
        &'a self,
        cancellation: RunCancellation,
    ) -> Pin<Box<dyn Future<Output = Result<WorkspaceExecutionLease, HarnessError>> + Send + 'a>>
    {
        Box::pin(async move { self.binding().await?.acquire(cancellation).await })
    }
}
