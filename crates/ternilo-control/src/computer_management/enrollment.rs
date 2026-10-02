use crate::{ControlAction, ControlStore, ControlUser, EnrollmentGrant};
use std::time::Duration;
use ternilo_protocol::{HarnessError, TenantId};
use ternilo_transport::ExecutorId;

pub(crate) struct EnrollmentIdentity {
    pub executor_id: ExecutorId,
    pub name: String,
    pub recovery_revision: Option<u64>,
}

impl ControlStore {
    #[expect(
        clippy::too_many_arguments,
        reason = "Enrollment keeps scope, project, name and expiry explicit."
    )]
    pub async fn create_computer_enrollment(
        &self,
        actor: &ControlUser,
        tenant: &TenantId,
        project: Option<&str>,
        name: &str,
        owned: bool,
        ttl: Duration,
        now_ms: u64,
    ) -> Result<EnrollmentGrant, HarnessError> {
        self.create_enrollment_with_action(
            actor,
            tenant,
            project,
            EnrollmentIdentity {
                executor_id: ExecutorId::new(crate::crypto::random_identifier("ter_pc")),
                name: super::names::validate(name)?.to_owned(),
                recovery_revision: None,
            },
            ttl,
            now_ms,
            if owned {
                ControlAction::RunReserve
            } else {
                ControlAction::ExecutorManage
            },
            owned,
        )
        .await
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Recovery explicitly retains the original computer identity and observed revision."
    )]
    pub async fn recover_computer_enrollment(
        &self,
        actor: &ControlUser,
        tenant: &TenantId,
        executor: &ExecutorId,
        name: &str,
        owned: bool,
        revision: u64,
        ttl: Duration,
        now_ms: u64,
    ) -> Result<EnrollmentGrant, HarnessError> {
        self.create_enrollment_with_action(
            actor,
            tenant,
            None,
            EnrollmentIdentity {
                executor_id: executor.clone(),
                name: super::names::validate(name)?.to_owned(),
                recovery_revision: Some(revision),
            },
            ttl,
            now_ms,
            if owned {
                ControlAction::RunReserve
            } else {
                ControlAction::ExecutorManage
            },
            owned,
        )
        .await
    }
}
