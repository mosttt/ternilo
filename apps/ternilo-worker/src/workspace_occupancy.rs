//! Physical workspace occupancy survives loss of the daemon that held its file locks.
//!
//! A released file lock is only evidence that its owner disappeared. The durable execution
//! record remains until that owner explicitly confirms that its controlled writers stopped.
//! These records must live on the same trusted storage as the managed workspaces.

#![cfg(target_os = "linux")]

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{File, TryLockError},
    io::{Read as _, Write as _},
    os::unix::fs::MetadataExt as _,
    sync::Arc,
};

use nix::{
    errno::Errno,
    fcntl::{OFlag, open, openat, renameat},
    sys::stat::{Mode, mkdirat},
    unistd::{UnlinkatFlags, unlinkat},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use ternilo_cloud::CloudWorkerIdentity;
use ternilo_cloud::StartedRun;
use ternilo_protocol::{HarnessError, TenantId, WorkspaceId};

use crate::client::WorkerClient;
use crate::sandbox_lifetime::{NamespaceIdentity, SupervisorIdentity};
use crate::storage_root::RegisteredStorageRoot;

const CONTROL_DIRECTORY: &str = ".ternilo-occupancy";
const STATE_FILE: &str = "state.json";

mod recovery;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OccupancyOwner {
    pub worker: CloudWorkerIdentity,
    pub family_id: String,
    pub occupation_epoch: u64,
    pub supervisor: SupervisorIdentity,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OccupancyRecord {
    version: u32,
    pub owner: OccupancyOwner,
    pub executions: BTreeSet<String>,
    completed: BTreeSet<String>,
    processes: BTreeMap<String, NamespaceIdentity>,
    authorized: BTreeSet<String>,
}

pub(crate) enum OccupancyAdmission {
    Acquired(OccupancyGuard),
    Busy,
    /// Missing owner locks do not establish that the corresponding writers have stopped.
    Unconfirmed(OccupancyRecord),
}

#[derive(Clone)]
pub(crate) struct WorkspaceOccupancy {
    inner: Arc<OccupancyDirectory>,
}

struct OccupancyDirectory {
    root: RegisteredStorageRoot,
    root_directory: File,
    control_directory: File,
    workspace_directory: File,
    workspace_key: String,
}

struct MetadataGate(File);

impl Drop for MetadataGate {
    fn drop(&mut self) {
        // A concurrent fork can briefly inherit even CLOEXEC descriptors. Unlock explicitly
        // so an unrelated child cannot prolong this metadata transaction until its exec.
        let _ = self.0.unlock();
    }
}

/// This guard does not unlock by clearing state on Drop, including unwinding and cancellation.
pub(crate) struct OccupancyGuard {
    directory: WorkspaceOccupancy,
    owner: OccupancyOwner,
    execution_id: String,
    _member_lock: File,
}

impl WorkspaceOccupancy {
    pub(crate) fn acquire_for_run(
        store: &WorkerClient,
        root: &RegisteredStorageRoot,
        started: &StartedRun,
    ) -> Result<OccupancyGuard, HarnessError> {
        let ticket = &started.claim.workspace_use;
        started.claim.validate_workspace_use()?;
        let identity = store.worker_identity()?;
        if ticket.worker_id != identity.worker_id.as_str()
            || ticket.worker_generation != identity.generation
            || ticket.storage_id != root.storage_id()
            || ticket.root_id != root.root_id()
        {
            return Err(HarnessError::policy(
                "cloud workspace ticket does not match this Worker storage or generation",
            ));
        }
        let occupancy = Self::open(root, &ticket.tenant_id, &ticket.workspace_id)?;
        let owner = OccupancyOwner {
            worker: identity,
            family_id: ticket.family_id.clone(),
            occupation_epoch: ticket.occupation_epoch,
            supervisor: SupervisorIdentity::current().map_err(access_error)?,
        };
        match occupancy.try_acquire(&owner, &format!("{}:{}", ticket.run_id, ticket.lease_token))? {
            OccupancyAdmission::Acquired(guard) => Ok(guard),
            OccupancyAdmission::Busy => Err(HarnessError::policy(
                "cloud workspace is busy with another execution",
            )),
            OccupancyAdmission::Unconfirmed(record) => {
                let _ = (record.owner, record.executions.len());
                Err(HarnessError::policy(
                    "cloud workspace has an unconfirmed prior execution; physical recovery is required",
                ))
            }
        }
    }

    /// Open coordination metadata before preparing the actual tenant or workspace directory.
    pub(crate) fn open(
        root: &RegisteredStorageRoot,
        tenant: &TenantId,
        workspace: &WorkspaceId,
    ) -> Result<Self, HarnessError> {
        root.validate()?;
        let root_directory = File::from(
            open(
                root.path(),
                OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
                Mode::empty(),
            )
            .map_err(access_error)?,
        );
        root.validate()?;
        same_directory(
            &root_directory,
            &File::open(root.path()).map_err(access_error)?,
        )?;
        let control_directory = open_directory(&root_directory, CONTROL_DIRECTORY, true)?;
        let workspace_key = digest(&format!("{}\0{}", tenant.as_str(), workspace.as_str()));
        let workspace_directory = open_directory(&control_directory, &workspace_key, true)?;
        Ok(Self {
            inner: Arc::new(OccupancyDirectory {
                root: root.clone(),
                root_directory,
                control_directory,
                workspace_directory,
                workspace_key,
            }),
        })
    }

    pub(crate) fn try_acquire(
        &self,
        owner: &OccupancyOwner,
        execution_id: &str,
    ) -> Result<OccupancyAdmission, HarnessError> {
        if owner.occupation_epoch == 0 {
            return Err(HarnessError::invalid(
                "workspace occupation epoch must be positive",
            ));
        }
        self.inner.validate()?;
        let Some(_gate) = self.inner.try_gate()? else {
            return Ok(OccupancyAdmission::Busy);
        };
        let mut record = match self.inner.read_record()? {
            Some(mut record) => {
                for execution in &record.executions {
                    let member = self.inner.file(&member_name(execution), false);
                    match member {
                        Ok(member) if !try_lock(&member)? => {}
                        Ok(_) => return Ok(OccupancyAdmission::Unconfirmed(record)),
                        Err(error) if error.code == ternilo_protocol::ErrorCode::InvalidInput => {
                            return Ok(OccupancyAdmission::Unconfirmed(record));
                        }
                        Err(error) => return Err(error),
                    }
                }
                if record.executions.is_empty()
                    && owner.occupation_epoch > record.owner.occupation_epoch
                {
                    record.owner = owner.clone();
                    record.completed.clear();
                    record.processes.clear();
                    record.authorized.clear();
                }
                if owner.occupation_epoch < record.owner.occupation_epoch
                    || (owner == &record.owner && record.completed.contains(execution_id))
                {
                    return Err(HarnessError::policy(
                        "workspace occupation ticket has already expired or completed",
                    ));
                }
                if record.owner != *owner || record.executions.contains(execution_id) {
                    return Ok(OccupancyAdmission::Busy);
                }
                record
            }
            None => OccupancyRecord {
                version: 2,
                owner: owner.clone(),
                executions: BTreeSet::new(),
                completed: BTreeSet::new(),
                processes: BTreeMap::new(),
                authorized: BTreeSet::new(),
            },
        };
        let member = self.inner.file(&member_name(execution_id), true)?;
        if !try_lock(&member)? {
            return Ok(OccupancyAdmission::Busy);
        }
        record.executions.insert(execution_id.to_owned());
        // Commit before returning any authority to prepare files or launch an execution.
        self.inner.write_record(&record)?;
        Ok(OccupancyAdmission::Acquired(OccupancyGuard {
            directory: self.clone(),
            owner: owner.clone(),
            execution_id: execution_id.to_owned(),
            _member_lock: member,
        }))
    }
}

impl OccupancyGuard {
    pub(crate) fn authorize_startup(&self) -> Result<(), HarnessError> {
        self.directory.inner.validate()?;
        let _gate = self.directory.inner.gate()?;
        let mut record = self.directory.inner.read_record()?.ok_or_else(|| {
            HarnessError::execution(
                "workspace occupancy disappeared before execution authorization",
            )
        })?;
        if record.owner != self.owner || !record.executions.contains(&self.execution_id) {
            return Err(HarnessError::policy(
                "workspace owner changed before execution authorization",
            ));
        }
        record.authorized.insert(self.execution_id.clone());
        self.directory.inner.write_record(&record)
    }

    /// Persist the exact kernel identity while Bubblewrap still holds its startup barrier.
    pub(crate) fn record_namespace(
        &self,
        identity: &NamespaceIdentity,
    ) -> Result<(), HarnessError> {
        self.directory.inner.validate()?;
        let _gate = self.directory.inner.gate()?;
        let mut record = self.directory.inner.read_record()?.ok_or_else(|| {
            HarnessError::execution("workspace occupancy disappeared before sandbox startup")
        })?;
        if record.owner != self.owner
            || identity.supervisor != self.owner.supervisor
            || !record.executions.contains(&self.execution_id)
            || record
                .processes
                .get(&self.execution_id)
                .is_some_and(|value| value != identity)
        {
            return Err(HarnessError::policy(
                "workspace sandbox identity cannot be replaced",
            ));
        }
        record
            .processes
            .insert(self.execution_id.clone(), identity.clone());
        self.directory.inner.write_record(&record)
    }

    /// Call only after all controlled execution processes have observably stopped, or before
    /// any were launched. Leader exit, lease expiry and sending a stop signal are insufficient.
    /// Orphaned executions have no guard and cannot use this normal-completion entry point.
    pub(crate) fn confirm_stopped(self) -> Result<(), HarnessError> {
        self.directory.inner.validate()?;
        let _gate = self.directory.inner.gate()?;
        self.directory
            .inner
            .complete_member(&self.owner, &self.execution_id)
    }
}

impl OccupancyDirectory {
    fn gate(&self) -> Result<MetadataGate, HarnessError> {
        let file = self.file("gate.lock", true)?;
        file.lock().map_err(access_error)?;
        Ok(MetadataGate(file))
    }

    fn try_gate(&self) -> Result<Option<MetadataGate>, HarnessError> {
        let file = self.file("gate.lock", true)?;
        if try_lock(&file)? {
            Ok(Some(MetadataGate(file)))
        } else {
            Ok(None)
        }
    }

    fn validate(&self) -> Result<(), HarnessError> {
        self.root.validate()?;
        let current_root = File::from(
            open(
                self.root.path(),
                OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
                Mode::empty(),
            )
            .map_err(access_error)?,
        );
        same_directory(&self.root_directory, &current_root)?;
        let control = open_directory(&self.root_directory, CONTROL_DIRECTORY, false)?;
        same_directory(&self.control_directory, &control)?;
        let workspace = open_directory(&control, &self.workspace_key, false)?;
        same_directory(&self.workspace_directory, &workspace)
    }

    fn file(&self, name: &str, create: bool) -> Result<File, HarnessError> {
        let mut flags = OFlag::O_RDWR | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC;
        if create {
            flags |= OFlag::O_CREAT;
        }
        let descriptor = openat(
            &self.workspace_directory,
            name,
            flags,
            Mode::S_IRUSR | Mode::S_IWUSR,
        )
        .map_err(|error| {
            if error == Errno::ENOENT {
                HarnessError::invalid("workspace occupancy member is missing")
            } else {
                access_error(error)
            }
        })?;
        let file = File::from(descriptor);
        let metadata = file.metadata().map_err(access_error)?;
        if !metadata.is_file() || metadata.nlink() != 1 {
            return Err(HarnessError::policy(
                "workspace occupancy files must be regular files without hard links",
            ));
        }
        Ok(file)
    }

    fn read_record(&self) -> Result<Option<OccupancyRecord>, HarnessError> {
        let mut file = match self.file(STATE_FILE, false) {
            Ok(file) => file,
            Err(error) if error.code == ternilo_protocol::ErrorCode::InvalidInput => {
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).map_err(access_error)?;
        let record: OccupancyRecord = serde_json::from_slice(&bytes)
            .map_err(|_| HarnessError::execution("workspace occupancy record is invalid"))?;
        if record.version != 2 || record.owner.occupation_epoch == 0 {
            return Err(HarnessError::execution(
                "workspace occupancy record is invalid",
            ));
        }
        Ok(Some(record))
    }

    fn write_record(&self, record: &OccupancyRecord) -> Result<(), HarnessError> {
        let bytes = serde_json::to_vec(record).map_err(|error| {
            HarnessError::execution(format!("encode workspace occupancy: {error}"))
        })?;
        let temporary = format!(".state-{:032x}.tmp", rand::random::<u128>());
        let descriptor = openat(
            &self.workspace_directory,
            temporary.as_str(),
            OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_EXCL | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
            Mode::S_IRUSR | Mode::S_IWUSR,
        )
        .map_err(access_error)?;
        let mut file = File::from(descriptor);
        let result = (|| {
            file.write_all(&bytes).map_err(access_error)?;
            file.sync_all().map_err(access_error)?;
            renameat(
                &self.workspace_directory,
                temporary.as_str(),
                &self.workspace_directory,
                STATE_FILE,
            )
            .map_err(access_error)?;
            self.workspace_directory.sync_all().map_err(access_error)
        })();
        if result.is_err() {
            let _ = unlinkat(
                &self.workspace_directory,
                temporary.as_str(),
                UnlinkatFlags::NoRemoveDir,
            );
        }
        result
    }

    // The caller must hold gate.lock and possess actual physical completion evidence.
    fn complete_member(
        &self,
        owner: &OccupancyOwner,
        execution_id: &str,
    ) -> Result<(), HarnessError> {
        let mut record = self.read_record()?.ok_or_else(|| {
            HarnessError::execution("workspace occupancy disappeared before confirmed completion")
        })?;
        if record.owner != *owner || !record.executions.remove(execution_id) {
            return Err(HarnessError::policy(
                "workspace occupancy owner changed before completion",
            ));
        }
        record.completed.insert(execution_id.to_owned());
        record.processes.remove(execution_id);
        record.authorized.remove(execution_id);
        // Keep the epoch and retired tickets even when the last member exits.
        self.write_record(&record)?;
        unlinkat(
            &self.workspace_directory,
            member_name(execution_id).as_str(),
            UnlinkatFlags::NoRemoveDir,
        )
        .map_err(access_error)?;
        self.workspace_directory.sync_all().map_err(access_error)
    }
}

fn open_directory(parent: &File, name: &str, create: bool) -> Result<File, HarnessError> {
    if create {
        match mkdirat(parent, name, Mode::S_IRWXU) {
            Ok(()) => parent.sync_all().map_err(access_error)?,
            Err(Errno::EEXIST) => {}
            Err(error) => return Err(access_error(error)),
        }
    }
    let directory = File::from(
        openat(
            parent,
            name,
            OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
            Mode::empty(),
        )
        .map_err(access_error)?,
    );
    if directory.metadata().map_err(access_error)?.mode() & 0o077 != 0 {
        return Err(HarnessError::policy(
            "workspace occupancy directories must be private",
        ));
    }
    Ok(directory)
}

fn same_directory(original: &File, current: &File) -> Result<(), HarnessError> {
    let original = original.metadata().map_err(access_error)?;
    let current = current.metadata().map_err(access_error)?;
    if (original.dev(), original.ino()) != (current.dev(), current.ino()) {
        return Err(HarnessError::policy(
            "workspace occupancy directory was replaced",
        ));
    }
    Ok(())
}

fn try_lock(file: &File) -> Result<bool, HarnessError> {
    match file.try_lock() {
        Ok(()) => Ok(true),
        Err(TryLockError::WouldBlock) => Ok(false),
        Err(TryLockError::Error(error)) => Err(access_error(error)),
    }
}

fn digest(value: &str) -> String {
    use std::fmt::Write as _;
    Sha256::digest(value.as_bytes())
        .iter()
        .fold(String::with_capacity(64), |mut output, byte| {
            write!(output, "{byte:02x}").expect("writing into a String cannot fail");
            output
        })
}

fn member_name(execution: &str) -> String {
    format!("member-{}.lock", digest(execution))
}

fn access_error(error: impl std::fmt::Display) -> HarnessError {
    HarnessError::execution(format!("access workspace occupancy: {error}"))
}

#[cfg(test)]
#[path = "workspace_occupancy_tests.rs"]
mod tests;
