use ternilo_cloud::WorkspaceUseTicket;

use super::{
    HarnessError, RegisteredStorageRoot, WorkspaceOccupancy, access_error, member_name, try_lock,
};

impl WorkspaceOccupancy {
    /// A durable completion marker is written before the caller acknowledges Server cleanup.
    pub(crate) fn recover(
        root: &RegisteredStorageRoot,
        ticket: &WorkspaceUseTicket,
    ) -> Result<bool, HarnessError> {
        ticket.validate()?;
        if ticket.storage_id != root.storage_id() || ticket.root_id != root.root_id() {
            return Err(HarnessError::policy(
                "workspace recovery storage root does not match",
            ));
        }
        let directory = Self::open(root, &ticket.tenant_id, &ticket.workspace_id)?;
        directory.inner.validate()?;
        let Some(_gate) = directory.inner.try_gate()? else {
            return Ok(false);
        };
        let Some(record) = directory.inner.read_record()? else {
            return Ok(false);
        };
        let owner = &record.owner;
        if owner.worker.worker_id.as_str() != ticket.worker_id
            || owner.worker.generation != ticket.worker_generation
            || owner.family_id != ticket.family_id
            || owner.occupation_epoch != ticket.occupation_epoch
        {
            return Ok(false);
        }
        let execution = format!("{}:{}", ticket.run_id, ticket.lease_token);
        if record.completed.contains(&execution) {
            return Ok(true);
        }
        if !record.executions.contains(&execution) {
            return Ok(false);
        }
        let member = match directory.inner.file(&member_name(&execution), false) {
            Ok(member) => member,
            Err(error) if error.code == ternilo_protocol::ErrorCode::InvalidInput => {
                return Ok(false);
            }
            Err(error) => return Err(error),
        };
        if !try_lock(&member)?
            || !crate::namespace_recovery::supervisor_exited(&owner.supervisor)
                .map_err(access_error)?
        {
            return Ok(false);
        }
        if record.authorized.contains(&execution) {
            let Some(identity) = record.processes.get(&execution) else {
                return Ok(false);
            };
            if identity.supervisor != owner.supervisor
                || !crate::namespace_recovery::stop_or_confirm_namespace(identity)
                    .map_err(access_error)?
            {
                return Ok(false);
            }
        }
        // Unauthorized children cannot pass the private startup preamble, even on parent EOF.
        directory.inner.complete_member(owner, &execution)?;
        Ok(true)
    }
}

#[cfg(test)]
#[path = "recovery_tests.rs"]
mod tests;
