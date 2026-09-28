use serde_json::Value;
use ternilo_cloud::CloudSessionRecord;
use ternilo_control::{
    ControlUser, EdgeSessionRecord, ResourceAction, ResourceKind, WorkspacePlacement,
    WorkspaceRecord,
};
use ternilo_protocol::{HarnessError, SessionId, TenantId, WorkspaceId};
use ternilo_transport::{ApplicationOperation, ExecutorId};

use crate::platform::state::AppState;

pub(crate) enum WorkspaceTarget {
    Cloud(WorkspaceRecord),
    Edge {
        workspace: WorkspaceRecord,
        executor_id: ternilo_transport::ExecutorId,
        node_workspace_id: WorkspaceId,
    },
}

pub(crate) enum SessionTarget {
    Cloud(CloudSessionRecord),
    Edge(EdgeSessionRecord),
}

pub(crate) enum SettingsTarget {
    Cloud,
    Edge(ExecutorId),
}

impl SettingsTarget {
    pub(crate) async fn read(
        &self,
        state: &AppState,
        tenant_id: &TenantId,
        operation: ApplicationOperation,
    ) -> Result<Option<Value>, HarnessError> {
        match self {
            Self::Cloud => Ok(None),
            Self::Edge(executor_id) => state
                .edge
                .call(tenant_id, executor_id, operation)
                .await
                .map(Some),
        }
    }

    pub(crate) async fn mutate(
        &self,
        state: &AppState,
        actor: &ControlUser,
        tenant_id: &TenantId,
        operation: ApplicationOperation,
    ) -> Result<Option<Value>, HarnessError> {
        match self {
            Self::Cloud => Ok(None),
            Self::Edge(executor_id) => {
                super::edge_adapter::authorize_edge_mutation(state, actor, tenant_id).await?;
                state
                    .edge
                    .call(tenant_id, executor_id, operation)
                    .await
                    .map(Some)
            }
        }
    }
}

enum SettingsScope<'a> {
    Cloud,
    Session(&'a SessionId),
    Workspace(&'a WorkspaceId),
}

fn settings_scope<'a>(
    session_id: Option<&'a SessionId>,
    workspace_id: Option<&'a WorkspaceId>,
) -> SettingsScope<'a> {
    match (session_id, workspace_id) {
        (Some(session_id), _) => SettingsScope::Session(session_id),
        (None, Some(workspace_id)) => SettingsScope::Workspace(workspace_id),
        (None, None) => SettingsScope::Cloud,
    }
}

pub(crate) struct PlacementResolver<'a> {
    state: &'a AppState,
    actor: &'a ControlUser,
    tenant_id: &'a TenantId,
}

impl<'a> PlacementResolver<'a> {
    pub(crate) fn new(
        state: &'a AppState,
        actor: &'a ControlUser,
        tenant_id: &'a TenantId,
    ) -> Self {
        Self {
            state,
            actor,
            tenant_id,
        }
    }

    pub(crate) async fn workspace(
        &self,
        workspace_id: &WorkspaceId,
    ) -> Result<WorkspaceTarget, HarnessError> {
        let workspace = self
            .state
            .store
            .resolve_accessible_workspace(self.actor, self.tenant_id, workspace_id)
            .await?;
        match workspace.placement {
            WorkspacePlacement::Cloud => Ok(WorkspaceTarget::Cloud(workspace)),
            WorkspacePlacement::LocalNode => {
                let executor_id = workspace.executor_id.clone().ok_or_else(binding_error)?;
                let node_workspace_id = workspace
                    .executor_workspace_id
                    .clone()
                    .ok_or_else(binding_error)?;
                Ok(WorkspaceTarget::Edge {
                    workspace,
                    executor_id,
                    node_workspace_id,
                })
            }
        }
    }

    pub(crate) async fn session(
        &self,
        session_id: &SessionId,
    ) -> Result<SessionTarget, HarnessError> {
        self.session_in(session_id, false)
            .await?
            .ok_or_else(session_not_found)
    }

    pub(crate) async fn session_for_deletion(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<SessionTarget>, HarnessError> {
        let target = self.session_in(session_id, true).await?;
        if target.is_none() {
            let mut transaction = self
                .state
                .store
                .database()
                .tenant_transaction(self.tenant_id)
                .await?;
            let existing = sqlx::query_scalar::<_, String>(
                "SELECT session_id FROM control_edge_sessions WHERE tenant_id=$1 AND session_id=$2
                 UNION ALL SELECT session_id FROM cloud_sessions WHERE tenant_id=$1 AND session_id=$2 LIMIT 1",
            ).bind(self.tenant_id.as_str()).bind(session_id.as_str())
                .fetch_optional(&mut *transaction).await.map_err(ternilo_storage::database_error)?;
            transaction
                .commit()
                .await
                .map_err(ternilo_storage::database_error)?;
            if existing.is_some() {
                return Err(session_not_found());
            }
        }
        Ok(target)
    }

    pub(crate) async fn archived_session(
        &self,
        session_id: &SessionId,
    ) -> Result<SessionTarget, HarnessError> {
        let target = self
            .session_in(session_id, true)
            .await?
            .ok_or_else(session_not_found)?;
        let archived = match &target {
            SessionTarget::Cloud(session) => session.archived_at_ms.is_some(),
            SessionTarget::Edge(session) => session.metadata.archived_at_ms.is_some(),
        };
        if !archived {
            return Err(HarnessError::conflict("session is no longer archived"));
        }
        Ok(target)
    }

    async fn session_in(
        &self,
        session_id: &SessionId,
        include_archived: bool,
    ) -> Result<Option<SessionTarget>, HarnessError> {
        let (cloud, edge) = tokio::try_join!(
            async {
                if include_archived {
                    self.state
                        .cloud
                        .find_accessible_session_for_deletion(
                            self.tenant_id,
                            &self.actor.user_id,
                            session_id,
                        )
                        .await
                } else {
                    self.state
                        .cloud
                        .find_accessible_session(self.tenant_id, &self.actor.user_id, session_id)
                        .await
                }
            },
            self.state
                .store
                .find_accessible_edge_session(self.actor, self.tenant_id, session_id,),
        )?;
        match (cloud, edge) {
            (Some(_), Some(_)) => Err(HarnessError::execution(
                "session identifier has conflicting cloud and local-node placements",
            )),
            (Some(session), None) => Ok(Some(SessionTarget::Cloud(session))),
            (None, Some(session)) => Ok(Some(SessionTarget::Edge(session))),
            (None, None) => Ok(None),
        }
    }

    pub(crate) async fn settings(
        &self,
        session_id: Option<&SessionId>,
        workspace_id: Option<&WorkspaceId>,
    ) -> Result<SettingsTarget, HarnessError> {
        let resource = match settings_scope(session_id, workspace_id) {
            SettingsScope::Session(id) => Some((ResourceKind::Session, id.as_str())),
            SettingsScope::Workspace(id) => Some((ResourceKind::Workspace, id.as_str())),
            SettingsScope::Cloud => None,
        };
        if let Some((kind, id)) = resource {
            let access = self
                .state
                .store
                .resource_access(self.actor, self.tenant_id, kind, id)
                .await?;
            access.require(ResourceAction::View)?;
            if !access.is_owner {
                return Err(HarnessError::policy(
                    "execution configuration belongs to the resource owner",
                ));
            }
        }
        match settings_scope(session_id, workspace_id) {
            SettingsScope::Session(session_id) => match self.session(session_id).await? {
                SessionTarget::Cloud(_) => Ok(SettingsTarget::Cloud),
                SessionTarget::Edge(session) => Ok(SettingsTarget::Edge(session.executor_id)),
            },
            SettingsScope::Workspace(workspace_id) => match self.workspace(workspace_id).await? {
                WorkspaceTarget::Cloud(_) => Ok(SettingsTarget::Cloud),
                WorkspaceTarget::Edge { executor_id, .. } => Ok(SettingsTarget::Edge(executor_id)),
            },
            SettingsScope::Cloud => Ok(SettingsTarget::Cloud),
        }
    }

    pub(crate) async fn ensure_session_id_available(
        &self,
        session_id: &SessionId,
    ) -> Result<(), HarnessError> {
        session_id.validate()?;
        let (cloud, edge) = tokio::try_join!(
            self.state
                .cloud
                .find_owned_session(self.tenant_id, &self.actor.user_id, session_id,),
            self.state
                .store
                .find_owned_edge_session(self.actor, self.tenant_id, session_id,),
        )?;
        if cloud.is_some() || edge.is_some() {
            Err(HarnessError::invalid("session already exists"))
        } else {
            Ok(())
        }
    }
}

fn binding_error() -> HarnessError {
    HarnessError::execution("local-node Workspace has an incomplete executor binding")
}

pub(crate) fn session_not_found() -> HarnessError {
    HarnessError::invalid("session does not exist")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_scope_prefers_session_then_workspace_then_cloud() {
        let session_id = SessionId::new("session");
        let workspace_id = WorkspaceId::new("workspace");

        assert!(matches!(
            settings_scope(Some(&session_id), Some(&workspace_id)),
            SettingsScope::Session(value) if value == &session_id
        ));
        assert!(matches!(
            settings_scope(None, Some(&workspace_id)),
            SettingsScope::Workspace(value) if value == &workspace_id
        ));
        assert!(matches!(settings_scope(None, None), SettingsScope::Cloud));
    }
}
