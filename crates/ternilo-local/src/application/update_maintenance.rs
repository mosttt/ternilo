use super::LocalApplication;
use std::sync::Arc;
use ternilo_protocol::HarnessError;
use tokio::sync::{OwnedRwLockReadGuard, OwnedRwLockWriteGuard};

impl LocalApplication {
    pub(super) fn accept_update_safe_work(&self) -> Result<OwnedRwLockReadGuard<()>, HarnessError> {
        Arc::clone(&self.maintenance).try_read_owned().map_err(|_| {
            HarnessError::unavailable(
                "this computer is installing a client update; retry after it reconnects",
            )
        })
    }

    /// Claim an idle host without waiting ahead of child tasks spawned by an active run.
    /// Keep this guard until the service has closed, or drop it to cancel maintenance.
    pub async fn try_client_update_maintenance(
        &self,
    ) -> Result<Option<OwnedRwLockWriteGuard<()>>, HarnessError> {
        let Ok(guard) = Arc::clone(&self.maintenance).try_write_owned() else {
            return Ok(None);
        };
        self.execution_resources.prune().await?;
        if !self.execution_resources.owners().await.is_empty() {
            return Ok(None);
        }
        for session in self.state.snapshot().await.sessions {
            let inbox = self
                .session_inbox(session.identity.session_id.as_str())
                .await?;
            if inbox.active_run_id.is_some() || !inbox.items.is_empty() {
                return Ok(None);
            }
        }
        Ok(Some(guard))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ternilo_kernel::HostPolicy;
    use ternilo_protocol::RunLimits;

    #[tokio::test]
    async fn idle_claim_cannot_interrupt_a_run_or_admit_work_until_released() {
        let root = tempfile::tempdir().unwrap();
        let app = LocalApplication::open(
            crate::catalog().unwrap(),
            crate::local_profile(),
            HostPolicy::local(RunLimits::default()),
            root.path().to_owned(),
        )
        .await
        .unwrap();
        let active = app.accept_update_safe_work().unwrap();
        assert!(app.try_client_update_maintenance().await.unwrap().is_none());
        // A pending update must not queue a writer ahead of children needed by this run.
        let child = app.accept_update_safe_work().unwrap();
        drop(active);
        drop(child);
        let guard = app.try_client_update_maintenance().await.unwrap().unwrap();
        assert!(app.accept_update_safe_work().is_err());
        drop(guard);
        assert!(app.accept_update_safe_work().is_ok());
        app.close().await.unwrap();
    }
}
