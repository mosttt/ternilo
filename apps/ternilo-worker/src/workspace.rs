use std::path::{Path, PathBuf};

use sha2::{Digest as _, Sha256};
use ternilo_cloud::WorkerPolicy;
use ternilo_protocol::{HarnessError, TenantId, WorkspaceId};

use crate::storage_root::RegisteredStorageRoot;

/// Missing workspaces are not initialized by read-only inspection.
pub(crate) async fn existing_workspace(
    root: &RegisteredStorageRoot,
    tenant_id: &TenantId,
    workspace_id: &WorkspaceId,
) -> Result<Option<PathBuf>, HarnessError> {
    let (tenant_root, workspace) = workspace_paths(root, tenant_id, workspace_id)?;
    if !managed_directory_exists(&tenant_root).await?
        || !managed_directory_exists(&workspace).await?
    {
        return Ok(None);
    }
    Ok(Some(workspace))
}

pub(crate) async fn prepare_workspace(
    root: &RegisteredStorageRoot,
    tenant_id: &TenantId,
    workspace_id: &WorkspaceId,
    container_isolation: bool,
) -> Result<PathBuf, HarnessError> {
    let (tenant_root, workspace) = workspace_paths(root, tenant_id, workspace_id)?;
    // Validate both existing components before creating directories or changing any permissions.
    let tenant_exists = managed_directory_exists(&tenant_root).await?;
    let workspace_exists = tenant_exists && managed_directory_exists(&workspace).await?;
    if !tenant_exists {
        create_managed_directory(&tenant_root).await?;
    }
    if !workspace_exists {
        create_managed_directory(&workspace).await?;
    }
    if container_isolation {
        set_workspace_owner(&tenant_root, 0)?;
        set_workspace_owner(&workspace, 0)?;
    }
    set_directory_mode(&tenant_root, 0o711)?;
    set_directory_mode(&workspace, 0o700)?;
    if container_isolation {
        set_workspace_owner(&workspace, 10_001)?;
    }
    Ok(workspace)
}

fn workspace_paths(
    root: &RegisteredStorageRoot,
    tenant_id: &TenantId,
    workspace_id: &WorkspaceId,
) -> Result<(PathBuf, PathBuf), HarnessError> {
    tenant_id.validate()?;
    workspace_id.validate()?;
    root.validate()?;
    let tenant_root = root.path().join(identifier_digest(tenant_id.as_str()));
    let workspace = tenant_root.join(identifier_digest(workspace_id.as_str()));
    Ok((tenant_root, workspace))
}

async fn managed_directory_exists(path: &Path) -> Result<bool, HarnessError> {
    match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) if metadata.is_dir() => Ok(true),
        Ok(_) => Err(HarnessError::policy(
            "managed cloud workspace paths must be directories, not symbolic links or files",
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(HarnessError::execution(format!(
            "inspect cloud workspace directory: {error}"
        ))),
    }
}

async fn create_managed_directory(path: &Path) -> Result<(), HarnessError> {
    match tokio::fs::create_dir(path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            managed_directory_exists(path).await?;
            Ok(())
        }
        Err(error) => Err(HarnessError::execution(format!(
            "create cloud workspace: {error}"
        ))),
    }
}

pub(crate) async fn gc_workspace(
    workspace_root: PathBuf,
    tenant_id: TenantId,
    workspace_id: WorkspaceId,
    confirm_unregistered: bool,
) -> Result<(), HarnessError> {
    if !confirm_unregistered {
        return Err(HarnessError::invalid(
            "workspace GC requires --confirm-unregistered after checking that no run is active",
        ));
    }
    tenant_id.validate()?;
    workspace_id.validate()?;
    let workspace_root = tokio::fs::canonicalize(&workspace_root)
        .await
        .map_err(|error| HarnessError::execution(format!("resolve workspace root: {error}")))?;
    let tenant_root = workspace_root.join(identifier_digest(tenant_id.as_str()));
    let workspace = tenant_root.join(identifier_digest(workspace_id.as_str()));
    match tokio::fs::symlink_metadata(&workspace).await {
        Ok(_) => tokio::fs::remove_dir_all(&workspace)
            .await
            .map_err(|error| {
                HarnessError::execution(format!("remove unregistered cloud workspace: {error}"))
            })?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(HarnessError::execution(format!(
                "inspect unregistered cloud workspace: {error}"
            )));
        }
    }
    match tokio::fs::remove_dir(&tenant_root).await {
        Ok(()) => {}
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::DirectoryNotEmpty
            ) => {}
        Err(error) => {
            return Err(HarnessError::execution(format!(
                "remove empty tenant workspace directory: {error}"
            )));
        }
    }
    println!(
        "removed unregistered cloud workspace {}/{}",
        tenant_id.as_str(),
        workspace_id.as_str()
    );
    Ok(())
}

pub(crate) async fn enforce_workspace_free_space(
    workspace_root: &Path,
    minimum_free_bytes: u64,
) -> Result<(), HarnessError> {
    if minimum_free_bytes == 0 {
        return Ok(());
    }
    let root = workspace_root.to_owned();
    tokio::task::spawn_blocking(move || check_workspace_free_space(&root, minimum_free_bytes))
        .await
        .map_err(|error| {
            HarnessError::execution(format!("workspace free-space check panicked: {error}"))
        })?
}

pub(crate) async fn enforce_workspace_quota(
    tenant_workspace_root: &Path,
    policy: &WorkerPolicy,
) -> Result<(), HarnessError> {
    let workspace_root = tenant_workspace_root
        .parent()
        .ok_or_else(|| HarnessError::execution("tenant workspace directory has no storage root"))?;
    enforce_workspace_free_space(workspace_root, policy.minimum_workspace_free_bytes).await?;
    let tenant_workspace_root = tenant_workspace_root.to_owned();
    let maximum_bytes = policy.max_tenant_workspace_bytes;
    let maximum_entries = policy.max_tenant_workspace_entries;
    tokio::task::spawn_blocking(move || {
        check_tenant_workspace_usage(&tenant_workspace_root, maximum_bytes, maximum_entries)
    })
    .await
    .map_err(|error| {
        HarnessError::execution(format!("tenant workspace quota check panicked: {error}"))
    })?
}

fn check_tenant_workspace_usage(
    tenant_workspace_root: &Path,
    maximum_bytes: u64,
    maximum_entries: u64,
) -> Result<(), HarnessError> {
    let mut bytes = 0_u64;
    let mut entries = 0_u64;
    let mut pending = vec![tenant_workspace_root.to_owned()];
    while let Some(directory) = pending.pop() {
        let children = match std::fs::read_dir(&directory) {
            Ok(children) => children,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(HarnessError::execution(format!(
                    "inspect tenant workspace {}: {error}",
                    directory.display()
                )));
            }
        };
        for child in children {
            let child = child.map_err(|error| {
                HarnessError::execution(format!(
                    "inspect tenant workspace {}: {error}",
                    directory.display()
                ))
            })?;
            entries = entries
                .checked_add(1)
                .ok_or_else(|| HarnessError::execution("tenant workspace entry count overflow"))?;
            if entries > maximum_entries {
                return Err(HarnessError::policy(format!(
                    "tenant workspace entry quota exceeded: {entries} entries, limit {maximum_entries}"
                )));
            }
            let path = child.path();
            let metadata = match std::fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(HarnessError::execution(format!(
                        "inspect tenant workspace {}: {error}",
                        path.display()
                    )));
                }
            };
            if metadata.is_dir() {
                pending.push(path);
                continue;
            }
            bytes = bytes
                .checked_add(metadata.len())
                .ok_or_else(|| HarnessError::execution("tenant workspace byte count overflow"))?;
            if bytes > maximum_bytes {
                return Err(HarnessError::policy(format!(
                    "tenant workspace byte quota exceeded: {bytes} bytes, limit {maximum_bytes}"
                )));
            }
        }
    }
    Ok(())
}

#[cfg(unix)]
fn check_workspace_free_space(
    workspace_root: &Path,
    minimum_free_bytes: u64,
) -> Result<(), HarnessError> {
    let filesystem = nix::sys::statvfs::statvfs(workspace_root).map_err(|error| {
        HarnessError::execution(format!(
            "inspect workspace filesystem {}: {error}",
            workspace_root.display()
        ))
    })?;
    let available = filesystem
        .blocks_available()
        .saturating_mul(filesystem.fragment_size());
    if available < minimum_free_bytes {
        return Err(HarnessError::execution(format!(
            "workspace volume low-watermark reached: {available} bytes available, {minimum_free_bytes} required"
        )));
    }
    Ok(())
}

#[cfg(not(unix))]
fn check_workspace_free_space(_: &Path, minimum_free_bytes: u64) -> Result<(), HarnessError> {
    if minimum_free_bytes == 0 {
        Ok(())
    } else {
        Err(HarnessError::policy(
            "workspace free-space admission requires a Unix worker host",
        ))
    }
}

fn identifier_digest(value: &str) -> String {
    crate::hex_bytes(&Sha256::digest(value.as_bytes()))
}

#[cfg(unix)]
fn set_directory_mode(path: &Path, mode: u32) -> Result<(), HarnessError> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).map_err(|error| {
        HarnessError::execution(format!("set cloud workspace permissions: {error}"))
    })
}

#[cfg(not(unix))]
fn set_directory_mode(_: &Path, _: u32) -> Result<(), HarnessError> {
    Ok(())
}

#[cfg(unix)]
fn set_workspace_owner(path: &Path, owner: u32) -> Result<(), HarnessError> {
    std::os::unix::fs::chown(path, Some(owner), Some(owner)).map_err(|error| {
        HarnessError::execution(format!("set cloud workspace owner to {owner}: {error}"))
    })
}

#[cfg(not(unix))]
fn set_workspace_owner(_: &Path, _: u32) -> Result<(), HarnessError> {
    Err(HarnessError::policy(
        "container workspace isolation requires a Unix host",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registered_root(path: &Path) -> RegisteredStorageRoot {
        let root_id = crate::storage_root::load_or_create(path, "test-storage").unwrap();
        RegisteredStorageRoot::open(path, "test-storage", &root_id).unwrap()
    }

    #[test]
    fn tenant_workspace_quota_counts_bytes_and_entries_without_following_symlinks() {
        let directory = tempfile::tempdir().unwrap();
        let tenant = directory.path().join("tenant");
        std::fs::create_dir_all(tenant.join("workspace")).unwrap();
        std::fs::write(tenant.join("workspace/file.txt"), b"1234").unwrap();

        assert!(check_tenant_workspace_usage(&tenant, 4, 2).is_ok());
        let byte_error = check_tenant_workspace_usage(&tenant, 3, 2).unwrap_err();
        assert!(byte_error.message.contains("byte quota exceeded"));
        let entry_error = check_tenant_workspace_usage(&tenant, 4, 1).unwrap_err();
        assert!(entry_error.message.contains("entry quota exceeded"));

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/", tenant.join("workspace/root-link")).unwrap();
            assert!(check_tenant_workspace_usage(&tenant, 5, 3).is_ok());
        }
    }

    #[cfg(unix)]
    #[test]
    fn workspace_low_watermark_rejects_unreachable_capacity() {
        let directory = tempfile::tempdir().unwrap();
        let error = check_workspace_free_space(directory.path(), u64::MAX).unwrap_err();
        assert!(error.message.contains("low-watermark reached"));
    }

    #[tokio::test]
    async fn workspace_path_is_stable_across_worker_restarts_and_isolated_by_tenant() {
        let directory = tempfile::tempdir().unwrap();
        let workspace_id = WorkspaceId::new("workspace-a");
        let tenant_a = TenantId::new("tenant-a");
        let tenant_b = TenantId::new("tenant-b");
        let root = registered_root(directory.path());

        let first = prepare_workspace(&root, &tenant_a, &workspace_id, false)
            .await
            .unwrap();
        std::fs::write(first.join("persistent.txt"), b"survives-restart").unwrap();

        let reopened_root = registered_root(directory.path());
        let reopened = prepare_workspace(&reopened_root, &tenant_a, &workspace_id, false)
            .await
            .unwrap();
        let other_tenant = prepare_workspace(&root, &tenant_b, &workspace_id, false)
            .await
            .unwrap();

        assert_eq!(reopened, first);
        assert_eq!(
            std::fs::read(reopened.join("persistent.txt")).unwrap(),
            b"survives-restart"
        );
        assert_ne!(other_tenant, reopened);
        assert!(!other_tenant.join("persistent.txt").exists());
    }

    #[tokio::test]
    async fn workspace_gc_removes_only_the_confirmed_workspace() {
        let directory = tempfile::tempdir().unwrap();
        let tenant_id = TenantId::new("tenant-a");
        let removed_id = WorkspaceId::new("workspace-a");
        let retained_id = WorkspaceId::new("workspace-b");
        let root = registered_root(directory.path());
        let removed = prepare_workspace(&root, &tenant_id, &removed_id, false)
            .await
            .unwrap();
        let retained = prepare_workspace(&root, &tenant_id, &retained_id, false)
            .await
            .unwrap();
        std::fs::write(removed.join("data.txt"), b"remove").unwrap();
        std::fs::write(retained.join("data.txt"), b"retain").unwrap();

        assert!(
            gc_workspace(
                directory.path().to_owned(),
                tenant_id.clone(),
                removed_id.clone(),
                false,
            )
            .await
            .is_err()
        );
        assert!(removed.exists());
        gc_workspace(directory.path().to_owned(), tenant_id, removed_id, true)
            .await
            .unwrap();
        assert!(!removed.exists());
        assert_eq!(std::fs::read(retained.join("data.txt")).unwrap(), b"retain");
    }

    #[tokio::test]
    async fn read_only_lookup_never_initializes_a_missing_workspace() {
        let directory = tempfile::tempdir().unwrap();
        let root = registered_root(directory.path());
        let tenant = TenantId::new("tenant");
        let workspace = WorkspaceId::new("workspace");
        assert!(
            existing_workspace(&root, &tenant, &workspace)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
        let (tenant_root, workspace_path) = workspace_paths(&root, &tenant, &workspace).unwrap();
        std::fs::create_dir(&tenant_root).unwrap();
        assert!(
            existing_workspace(&root, &tenant, &workspace)
                .await
                .unwrap()
                .is_none()
        );
        assert!(!workspace_path.exists());
        assert_eq!(std::fs::read_dir(tenant_root).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn read_only_lookup_preserves_directory_permissions_owners_and_files() {
        use std::os::unix::fs::MetadataExt as _;
        let directory = tempfile::tempdir().unwrap();
        let root = registered_root(directory.path());
        let tenant = TenantId::new("tenant");
        let workspace = WorkspaceId::new("workspace");
        let path = prepare_workspace(&root, &tenant, &workspace, false)
            .await
            .unwrap();
        let tenant_path = path.parent().unwrap();
        set_directory_mode(tenant_path, 0o751).unwrap();
        set_directory_mode(&path, 0o750).unwrap();
        std::fs::write(
            path.join("artifact.txt"),
            "available while a task is running",
        )
        .unwrap();
        let metadata = |path: &Path| {
            let metadata = std::fs::metadata(path).unwrap();
            (metadata.mode(), metadata.uid(), metadata.gid())
        };
        let before = (metadata(tenant_path), metadata(&path));
        let existing = existing_workspace(&root, &tenant, &workspace)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(existing, path);
        assert_eq!((metadata(tenant_path), metadata(&path)), before);
        assert_eq!(
            std::fs::read_to_string(existing.join("artifact.txt")).unwrap(),
            "available while a task is running"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_replaced_registered_root_is_not_recreated_even_with_a_copied_marker() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("storage");
        std::fs::create_dir(&path).unwrap();
        let root = registered_root(&path);
        let original = directory.path().join("original-storage");
        std::fs::rename(&path, &original).unwrap();
        let tenant = TenantId::new("tenant");
        let workspace = WorkspaceId::new("workspace");
        assert!(
            prepare_workspace(&root, &tenant, &workspace, false)
                .await
                .is_err()
        );
        assert!(
            !path.exists(),
            "a missing registered mount must not be recreated"
        );
        std::fs::create_dir(&path).unwrap();
        std::fs::copy(
            original.join(".ternilo-storage.json"),
            path.join(".ternilo-storage.json"),
        )
        .unwrap();
        assert!(
            prepare_workspace(&root, &tenant, &workspace, false)
                .await
                .is_err()
        );
        assert!(
            existing_workspace(&root, &tenant, &workspace)
                .await
                .is_err()
        );
        assert_eq!(std::fs::read_dir(&path).unwrap().count(), 1);
        std::fs::remove_dir_all(&path).unwrap();
        std::fs::rename(original, &path).unwrap();
        assert!(
            prepare_workspace(&root, &tenant, &workspace, false)
                .await
                .is_ok()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn managed_path_symlinks_are_rejected_before_changing_external_targets() {
        use std::os::unix::fs::{MetadataExt as _, symlink};
        for link_tenant in [true, false] {
            let directory = tempfile::tempdir().unwrap();
            let external = tempfile::tempdir().unwrap();
            let root = registered_root(directory.path());
            let tenant = TenantId::new("tenant");
            let workspace = WorkspaceId::new("workspace");
            let (tenant_path, workspace_path) =
                workspace_paths(&root, &tenant, &workspace).unwrap();
            set_directory_mode(external.path(), 0o750).unwrap();
            std::fs::write(external.path().join("keep.txt"), "unchanged").unwrap();
            if link_tenant {
                symlink(external.path(), &tenant_path).unwrap();
            } else {
                std::fs::create_dir(&tenant_path).unwrap();
                set_directory_mode(&tenant_path, 0o751).unwrap();
                symlink(external.path(), &workspace_path).unwrap();
            }
            let before = std::fs::metadata(external.path()).unwrap();
            for container in [false, true] {
                let error = prepare_workspace(&root, &tenant, &workspace, container)
                    .await
                    .unwrap_err();
                assert_eq!(error.code, ternilo_protocol::ErrorCode::PolicyDenied);
                assert!(
                    existing_workspace(&root, &tenant, &workspace)
                        .await
                        .is_err()
                );
            }
            let after = std::fs::metadata(external.path()).unwrap();
            assert_eq!(
                (after.mode(), after.uid(), after.gid()),
                (before.mode(), before.uid(), before.gid())
            );
            assert_eq!(std::fs::read_dir(external.path()).unwrap().count(), 1);
            assert_eq!(
                std::fs::read_to_string(external.path().join("keep.txt")).unwrap(),
                "unchanged"
            );
            if !link_tenant {
                assert_eq!(
                    std::fs::metadata(tenant_path).unwrap().mode() & 0o777,
                    0o751
                );
            }
        }
    }

    #[tokio::test]
    async fn a_missing_or_changed_registration_marker_does_not_initialize_a_workspace() {
        let directory = tempfile::tempdir().unwrap();
        let root = registered_root(directory.path());
        let marker = directory.path().join(".ternilo-storage.json");
        std::fs::remove_file(&marker).unwrap();
        let tenant = TenantId::new("tenant");
        let workspace = WorkspaceId::new("workspace");
        assert!(
            prepare_workspace(&root, &tenant, &workspace, false)
                .await
                .is_err()
        );
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
        std::fs::write(
            &marker,
            r#"{"version":1,"storage_id":"test-storage","root_id":"another-volume"}"#,
        )
        .unwrap();
        assert!(
            prepare_workspace(&root, &tenant, &workspace, false)
                .await
                .is_err()
        );
        assert!(
            existing_workspace(&root, &tenant, &workspace)
                .await
                .is_err()
        );
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }
}
