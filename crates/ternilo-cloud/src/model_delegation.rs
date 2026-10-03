use ternilo_control::{ResourceAction, ResourceKind, resource_access_in};
use ternilo_protocol::{HarnessError, RunModelBinding, TenantId, UserId};
use ternilo_storage::{Transaction, database_error};

use crate::CloudStore;

pub(crate) async fn require_model_owner_in(
    tx: &mut Transaction,
    tenant: &TenantId,
    kind: ResourceKind,
    resource: &str,
    binding: &RunModelBinding,
) -> Result<UserId, HarnessError> {
    if matches!(binding, RunModelBinding::ComputerProvider { .. }) {
        return Err(HarnessError::policy(
            "computer models require a remote computer session",
        ));
    }
    let access =
        resource_access_in(tx, binding.beneficiary_user_id(), tenant, kind, resource).await?;
    access.require(ResourceAction::Configure)?;
    Ok(access.storage_user_id)
}

impl CloudStore {
    pub async fn require_model_owner(
        &self,
        tenant: &TenantId,
        kind: ResourceKind,
        resource: &str,
        binding: &RunModelBinding,
    ) -> Result<UserId, HarnessError> {
        let mut tx = self.begin().await?;
        let owner = require_model_owner_in(&mut tx, tenant, kind, resource, binding).await?;
        tx.commit().await.map_err(database_error)?;
        Ok(owner)
    }
}
