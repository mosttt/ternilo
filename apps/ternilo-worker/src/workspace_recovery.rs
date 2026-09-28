//! Bounded recovery runs independently of foreground and resident capacity.

use std::time::Duration;

use ternilo_cloud::WorkspaceRecoveryCursor;
use ternilo_protocol::HarnessError;

use crate::{client::WorkerClient, storage_root::RegisteredStorageRoot};

pub(crate) struct RecoveryTask(tokio::task::JoinHandle<()>);

impl RecoveryTask {
    pub(crate) fn spawn(store: WorkerClient, root: RegisteredStorageRoot) -> Self {
        Self(tokio::spawn(async move {
            let mut cursor = None;
            let mut last_error = None;
            let mut interval = tokio::time::interval(Duration::from_secs(2));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                interval.tick().await;
                match recover_page(&store, &root, &mut cursor).await {
                    Ok(()) => {
                        last_error = None;
                    }
                    Err(error) => {
                        let message = error.to_string();
                        if last_error.as_ref() != Some(&message) {
                            eprintln!("cloud workspace recovery deferred: {message}");
                        }
                        last_error = Some(message);
                    }
                }
            }
        }))
    }
}

impl Drop for RecoveryTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn recover_page(
    store: &WorkerClient,
    root: &RegisteredStorageRoot,
    cursor: &mut Option<WorkspaceRecoveryCursor>,
) -> Result<(), HarnessError> {
    let page = store.workspace_recovery_candidates(cursor.clone()).await?;
    // Advance even when a member is unverifiable; it must not hide later workspaces.
    *cursor = page.next;
    let mut first_error = None;
    for ticket in page.candidates {
        match confirm_local(root, &ticket.workspace) {
            Ok(true) => {
                if let Err(error) = store.confirm_workspace_recovery(ticket).await {
                    first_error.get_or_insert(error);
                }
            }
            Ok(false) => {}
            Err(error) => {
                first_error.get_or_insert(error);
            }
        }
    }
    first_error.map_or(Ok(()), Err)
}

#[cfg(target_os = "linux")]
fn confirm_local(
    root: &RegisteredStorageRoot,
    ticket: &ternilo_cloud::WorkspaceUseTicket,
) -> Result<bool, HarnessError> {
    crate::workspace_occupancy::WorkspaceOccupancy::recover(root, ticket)
}

#[cfg(not(target_os = "linux"))]
fn confirm_local(
    _root: &RegisteredStorageRoot,
    _ticket: &ternilo_cloud::WorkspaceUseTicket,
) -> Result<bool, HarnessError> {
    Ok(false)
}
