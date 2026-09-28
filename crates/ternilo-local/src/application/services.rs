use ternilo_protocol::{HarnessError, SessionServiceSnapshot};

use super::{LocalApplication, ManagedSession, RunCancellation};
use std::sync::Arc;

impl LocalApplication {
    async fn service_runtime(&self, id: &str) -> Result<Arc<ManagedSession>, HarnessError> {
        self.stopping.check()?;
        let lifecycle = self.session_lifecycle(id).await;
        let _guard = lifecycle.lock().await;
        self.ensure_session_locked(id).await
    }

    pub async fn session_services(
        &self,
        id: &str,
    ) -> Result<Vec<SessionServiceSnapshot>, HarnessError> {
        Ok(self
            .service_runtime(id)
            .await?
            .harness
            .service_catalog()
            .await)
    }

    pub async fn start_session_service(
        &self,
        id: &str,
        service_id: String,
    ) -> Result<SessionServiceSnapshot, HarnessError> {
        self.service_runtime(id)
            .await?
            .harness
            .start_service(service_id, RunCancellation::new())
            .await
    }

    pub async fn stop_session_service(
        &self,
        id: &str,
        service_id: String,
    ) -> Result<SessionServiceSnapshot, HarnessError> {
        self.service_runtime(id)
            .await?
            .harness
            .stop_service(service_id)
            .await
    }
}
