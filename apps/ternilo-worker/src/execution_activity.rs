use std::{
    future::Future,
    pin::Pin,
    sync::atomic::{AtomicU64, Ordering},
};

use ternilo_kernel::{ExecutionAdmission, RunCancellation};
use ternilo_protocol::{AcceptedSubagentRun, HarnessError};

use crate::{CloudHostRequest, PipeHostClient};

pub(crate) struct PipeExecutionAdmission {
    client: PipeHostClient,
    revision: AtomicU64,
    parked_revision: AtomicU64,
}

impl PipeExecutionAdmission {
    pub(crate) fn new(client: PipeHostClient) -> Self {
        Self {
            client,
            revision: AtomicU64::new(0),
            parked_revision: AtomicU64::new(0),
        }
    }

    fn next_revision(&self) -> Result<u64, HarnessError> {
        self.revision
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |revision| {
                revision.checked_add(1)
            })
            .map(|previous| previous + 1)
            .map_err(|_| HarnessError::execution("activity revision exhausted"))
    }
}

impl ExecutionAdmission for PipeExecutionAdmission {
    fn park<'a>(
        &'a self,
        dependencies: Vec<AcceptedSubagentRun>,
        cancellation: RunCancellation,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            cancellation.check()?;
            let activity_revision = self.next_revision()?;
            let value = self
                .client
                .request(CloudHostRequest::ParkActivity {
                    activity_revision,
                    dependencies,
                })
                .await?;
            let parked_revision = value
                .as_u64()
                .filter(|revision| *revision != 0)
                .ok_or_else(|| {
                    HarnessError::execution("parent returned an invalid parked activity revision")
                })?;
            self.parked_revision
                .store(parked_revision, Ordering::Release);
            Ok(())
        })
    }

    fn resume<'a>(
        &'a self,
        cancellation: RunCancellation,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            cancellation.check()?;
            let activity_revision = self.next_revision()?;
            let parked_revision = self.parked_revision.load(Ordering::Acquire);
            let value = self
                .client
                .request(CloudHostRequest::ResumeActivity {
                    activity_revision,
                    parked_revision,
                })
                .await?;
            let admission: ternilo_cloud::RunAdmission =
                serde_json::from_value(value).map_err(|_| {
                    HarnessError::execution("parent returned invalid execution admission")
                })?;
            match admission {
                ternilo_cloud::RunAdmission::Ready { admission_epoch } if admission_epoch != 0 => {
                    Ok(())
                }
                _ => Err(HarnessError::execution(
                    "parent returned before foreground admission was granted",
                )),
            }
        })
    }
}
