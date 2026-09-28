use std::{
    collections::BTreeSet,
    fs::{File, OpenOptions, TryLockError},
    future::Future,
    io::Write,
    path::{Path, PathBuf},
    pin::Pin,
    sync::Arc,
    time::Duration,
};

use serde::{Deserialize, Serialize};
use ternilo_kernel::{RunCancellation, WorkspaceExecution, WorkspaceExecutionLease};
use ternilo_protocol::HarnessError;

/// Coordinate cooperating local processes through a shared, private lock directory.
#[derive(Clone)]
pub struct DirectoryCoordinator {
    state: Arc<CoordinatorState>,
}

struct CoordinatorState {
    lock_root: PathBuf,
    instance: String,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum DirectoryOwner {
    Family { instance: String, scope: String },
    User { user: String, local_owner: bool },
}

#[derive(Serialize, Deserialize)]
struct DirectoryRecord {
    owner: DirectoryOwner,
    keys: Vec<String>,
}

struct AcquiredFileLock(File);

impl Drop for AcquiredFileLock {
    fn drop(&mut self) {
        // Duplicated or briefly inherited descriptors must not extend logical ownership.
        let _ = self.0.unlock();
    }
}

struct DirectoryBinding {
    coordinator: DirectoryCoordinator,
    owner: DirectoryOwner,
    workspace: PathBuf,
}

impl DirectoryCoordinator {
    #[must_use]
    pub fn new(lock_root: PathBuf) -> Self {
        Self {
            state: Arc::new(CoordinatorState {
                lock_root,
                instance: format!("{:032x}", rand::random::<u128>()),
            }),
        }
    }

    pub fn for_user() -> Result<Self, HarnessError> {
        #[cfg(target_os = "windows")]
        let state_root = std::env::var_os("LOCALAPPDATA")
            .filter(|value| !value.is_empty())
            .map_or_else(
                || crate::home_directory().map(|home| home.join("AppData/Local")),
                |path| Ok(PathBuf::from(path)),
            )?;
        #[cfg(target_os = "macos")]
        let state_root = crate::home_directory()?.join("Library/Application Support");
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        let state_root = std::env::var_os("XDG_STATE_HOME")
            .filter(|value| !value.is_empty())
            .map_or_else(
                || crate::home_directory().map(|home| home.join(".local/state")),
                |path| Ok(PathBuf::from(path)),
            )?;
        let lock_root = state_root.join("ternilo/execution-locks");
        create_lock_root(&lock_root)?;
        Ok(Self::new(lock_root))
    }

    #[must_use]
    pub fn bind(&self, scope: String, workspace: PathBuf) -> Arc<dyn WorkspaceExecution> {
        Arc::new(DirectoryBinding {
            coordinator: self.clone(),
            owner: DirectoryOwner::Family {
                instance: self.state.instance.clone(),
                scope,
            },
            workspace,
        })
    }

    #[must_use]
    pub fn bind_user(&self, user: String, workspace: PathBuf) -> Arc<dyn WorkspaceExecution> {
        Arc::new(DirectoryBinding {
            coordinator: self.clone(),
            owner: DirectoryOwner::User {
                user,
                local_owner: false,
            },
            workspace,
        })
    }

    #[must_use]
    pub fn bind_computer_owner(
        &self,
        user: String,
        workspace: PathBuf,
    ) -> Arc<dyn WorkspaceExecution> {
        Arc::new(DirectoryBinding {
            coordinator: self.clone(),
            owner: DirectoryOwner::User {
                user,
                local_owner: true,
            },
            workspace,
        })
    }

    async fn try_acquire(
        &self,
        owner: &DirectoryOwner,
        workspace: &Path,
    ) -> Result<Option<AcquiredFileLock>, HarnessError> {
        match owner {
            DirectoryOwner::Family { scope, .. } if scope.is_empty() => {
                return Err(HarnessError::invalid("workspace execution scope is empty"));
            }
            DirectoryOwner::User { user, .. } if user.is_empty() => {
                return Err(HarnessError::invalid("workspace execution user is empty"));
            }
            _ => {}
        }
        create_lock_root(&self.state.lock_root)?;
        let mut gate_options = private_file_options();
        gate_options.create(true).truncate(false);
        let gate_path = self.state.lock_root.join("registry-gate.lock");
        let gate = gate_options.open(&gate_path).map_err(registry_error)?;
        let _gate = loop {
            match gate.try_lock() {
                Ok(()) => break AcquiredFileLock(gate),
                Err(TryLockError::WouldBlock) => {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
                Err(TryLockError::Error(error)) => return Err(registry_error(error)),
            }
        };
        let record = DirectoryRecord {
            owner: owner.clone(),
            keys: directory_keys(workspace)?,
        };
        for entry in std::fs::read_dir(&self.state.lock_root).map_err(registry_error)? {
            let path = entry.map_err(registry_error)?.path();
            if path
                .extension()
                .is_none_or(|extension| extension != "lease")
            {
                continue;
            }
            let file = private_file_options().open(&path).map_err(registry_error)?;
            let metadata_path = path.with_extension("json");
            if let Some(stale) = try_lock(file)? {
                drop(stale);
                match std::fs::remove_file(&metadata_path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(registry_error(error)),
                }
                std::fs::remove_file(path).map_err(registry_error)?;
                continue;
            }
            let metadata = File::open(metadata_path).map_err(registry_error)?;
            let held: DirectoryRecord =
                serde_json::from_reader(metadata).map_err(registry_error)?;
            if record.conflicts_with(&held)? {
                return Ok(None);
            }
        }
        let path = self
            .state
            .lock_root
            .join(format!("holder-{:032x}.lease", rand::random::<u128>()));
        let mut options = private_file_options();
        options.create_new(true);
        let file = options.open(&path).map_err(registry_error)?;
        let lease = try_lock(file)?.ok_or_else(|| {
            HarnessError::execution("new workspace execution lease is already locked")
        })?;
        let mut metadata = options
            .open(path.with_extension("json"))
            .map_err(registry_error)?;
        metadata
            .write_all(&serde_json::to_vec(&record).map_err(registry_error)?)
            .map_err(registry_error)?;
        Ok(Some(lease))
    }
}

impl DirectoryRecord {
    fn conflicts_with(&self, other: &Self) -> Result<bool, HarnessError> {
        let leaf = self.keys.last().ok_or_else(|| {
            HarnessError::execution("workspace execution record has no directory identity")
        })?;
        let other_leaf = other.keys.last().ok_or_else(|| {
            HarnessError::execution("workspace execution record has no directory identity")
        })?;
        let overlaps = self.keys.contains(other_leaf) || other.keys.contains(leaf);
        let reentrant = match (&self.owner, &other.owner) {
            (
                DirectoryOwner::User { user, local_owner },
                DirectoryOwner::User {
                    user: other_user,
                    local_owner: other_local_owner,
                },
            ) => {
                user == other_user
                    || (user == "local-user" && *other_local_owner)
                    || (other_user == "local-user" && *local_owner)
            }
            _ => self.owner == other.owner && leaf == other_leaf,
        };
        Ok(overlaps && !reentrant)
    }
}

fn registry_error(error: impl std::fmt::Display) -> HarnessError {
    HarnessError::execution(format!("workspace execution registry: {error}"))
}

fn private_file_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options
}

fn try_lock(file: File) -> Result<Option<AcquiredFileLock>, HarnessError> {
    match file.try_lock() {
        Ok(()) => Ok(Some(AcquiredFileLock(file))),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(error)) => Err(registry_error(error)),
    }
}

impl WorkspaceExecution for DirectoryBinding {
    fn try_acquire<'a>(
        &'a self,
    ) -> Pin<
        Box<dyn Future<Output = Result<Option<WorkspaceExecutionLease>, HarnessError>> + Send + 'a>,
    > {
        Box::pin(async move {
            self.coordinator
                .try_acquire(&self.owner, &self.workspace)
                .await
                .map(|lease| lease.map(WorkspaceExecutionLease::hold))
        })
    }

    fn acquire<'a>(
        &'a self,
        cancellation: RunCancellation,
    ) -> Pin<Box<dyn Future<Output = Result<WorkspaceExecutionLease, HarnessError>> + Send + 'a>>
    {
        Box::pin(async move {
            loop {
                cancellation.check()?;
                let lease = tokio::select! {
                    () = cancellation.cancelled() => return Err(HarnessError::cancelled(
                        "workspace execution wait was cancelled",
                    )),
                    lease = self.try_acquire() => lease?,
                };
                if let Some(lease) = lease {
                    cancellation.check()?;
                    return Ok(lease);
                }
                tokio::select! {
                    () = cancellation.cancelled() => return Err(HarnessError::cancelled(
                        "workspace execution wait was cancelled",
                    )),
                    () = tokio::time::sleep(Duration::from_millis(50)) => {}
                }
            }
        })
    }
}

fn create_lock_root(root: &Path) -> Result<(), HarnessError> {
    if !root.is_absolute() {
        return Err(HarnessError::invalid(
            "workspace execution lock directory must be absolute",
        ));
    }
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o700);
    }
    builder.create(root).map_err(|error| {
        HarnessError::execution(format!(
            "create workspace execution lock directory {}: {error}",
            root.display()
        ))
    })
}

fn directory_keys(workspace: &Path) -> Result<Vec<String>, HarnessError> {
    if !workspace.is_absolute() {
        return Err(HarnessError::invalid(
            "workspace directory must be absolute",
        ));
    }
    let workspace = std::fs::canonicalize(workspace).map_err(|error| {
        HarnessError::execution(format!(
            "resolve workspace directory {}: {error}",
            workspace.display()
        ))
    })?;
    let mut keys = Vec::new();
    let mut seen = BTreeSet::new();
    for directory in workspace.ancestors() {
        let metadata = std::fs::metadata(directory).map_err(|error| {
            HarnessError::execution(format!(
                "inspect workspace directory {}: {error}",
                directory.display()
            ))
        })?;
        if !metadata.is_dir() {
            return Err(HarnessError::invalid(format!(
                "workspace is not a directory: {}",
                workspace.display()
            )));
        }
        #[cfg(unix)]
        let key = {
            use std::os::unix::fs::MetadataExt as _;
            format!("directory-{:x}-{:x}", metadata.dev(), metadata.ino())
        };
        #[cfg(not(unix))]
        let key = {
            use sha2::{Digest, Sha256};
            use std::fmt::Write as _;
            let mut key = String::from("directory-");
            for byte in Sha256::digest(directory.to_string_lossy().to_lowercase().as_bytes()) {
                write!(&mut key, "{byte:02x}").expect("writing to String cannot fail");
            }
            key
        };
        if seen.insert(key.clone()) {
            keys.push(key);
        }
    }
    keys.reverse();
    Ok(keys)
}

#[cfg(test)]
#[path = "workspace_execution_tests.rs"]
mod tests;
