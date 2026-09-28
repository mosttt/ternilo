use super::LocalApplication;
use crate::{
    ConnectionAuthorization, ConnectionPoll, LocalInvalidationCategory, ModelConnection,
    ModelSelection, model_connections::is_connection_provider,
};
use ternilo_protocol::{HarnessError, InputAuthor, InputProvenance};

impl LocalApplication {
    pub async fn model_connections(&self) -> Vec<ModelConnection> {
        self.providers.connections.list().await
    }

    pub async fn begin_model_connection(
        &self,
        server_url: &str,
        name: &str,
    ) -> Result<ConnectionAuthorization, HarnessError> {
        self.providers.connections.begin(server_url, name).await
    }

    pub async fn poll_model_connection(&self, id: &str) -> Result<ConnectionPoll, HarnessError> {
        let result = self
            .providers
            .connections
            .poll(id, &self.credentials)
            .await?;
        if matches!(result, ConnectionPoll::Connected { .. }) {
            self.invalidate(None, LocalInvalidationCategory::Workbench, None);
        }
        Ok(result)
    }

    pub async fn cancel_model_connection(&self, id: &str) {
        self.providers.connections.cancel(id).await;
    }

    pub async fn refresh_model_connection(
        &self,
        id: &str,
    ) -> Result<ModelConnection, HarnessError> {
        let result = self
            .providers
            .connections
            .refresh(id, &self.credentials)
            .await?;
        self.invalidate(None, LocalInvalidationCategory::Workbench, None);
        Ok(result)
    }

    pub async fn remove_model_connection(
        &self,
        id: &str,
        revoke: bool,
    ) -> Result<(), HarnessError> {
        let sources: Vec<_> = self
            .providers
            .connections
            .stored()
            .await
            .into_iter()
            .filter(|connection| connection.connection_id == id)
            .flat_map(|connection| {
                connection
                    .known_providers
                    .into_iter()
                    .map(|profile| profile.id)
            })
            .collect();
        self.providers
            .connections
            .remove(id, &self.credentials, revoke)
            .await?;
        for session in self.state.snapshot().await.sessions {
            if matches!(&session.model, ModelSelection::NamedProvider { provider_id, .. } if sources.contains(provider_id))
            {
                self.stop_live_session(session.identity.session_id.as_str())
                    .await?;
            }
        }
        self.invalidate(None, LocalInvalidationCategory::Workbench, None);
        Ok(())
    }

    pub(super) async fn validate_device_input(
        &self,
        session_id: &str,
        provenance: Option<&InputProvenance>,
    ) -> Result<(), HarnessError> {
        if provenance.is_some_and(|input| matches!(input.author, InputAuthor::Account { .. }))
            && self.state.session(session_id).await.is_some_and(|session| matches!(&session.model, ModelSelection::NamedProvider { provider_id, .. } if is_connection_provider(provider_id))) {
            return Err(HarnessError::policy("Server model connections currently accept local inputs only; shared Node inputs require a Server-issued actor authorization"));
        }
        Ok(())
    }
}
