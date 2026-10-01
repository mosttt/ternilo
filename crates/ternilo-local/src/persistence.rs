use std::path::Path;

use ternilo_protocol::HarnessError;
use tokio::io::AsyncWriteExt;

pub async fn atomic_replace(path: &Path, bytes: &[u8], private: bool) -> Result<(), HarnessError> {
    if let Some(parent) = path.parent() {
        create_private_directory(parent)?;
    }
    let temporary = path.with_extension("json.tmp");
    let mut options = tokio::fs::OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    if private {
        options.mode(0o600);
    }
    let mut file = options.open(&temporary).await.map_err(|error| {
        HarnessError::execution(format!("open {}: {error}", temporary.display()))
    })?;
    if private {
        set_private_permissions(&temporary).await?;
    }
    file.write_all(bytes).await.map_err(|error| {
        HarnessError::execution(format!("write {}: {error}", temporary.display()))
    })?;
    file.sync_all().await.map_err(|error| {
        HarnessError::execution(format!("sync {}: {error}", temporary.display()))
    })?;
    drop(file);
    tokio::fs::rename(&temporary, path)
        .await
        .map_err(|error| HarnessError::execution(format!("commit {}: {error}", path.display())))?;
    if private {
        set_private_permissions(path).await?;
    }
    sync_parent_directory(path).await
}

// POSIX directory fsync persists the rename. Windows does not support opening
// directories as ordinary files; the file itself was flushed before its rename.
pub(crate) async fn sync_parent_directory(path: &Path) -> Result<(), HarnessError> {
    #[cfg(unix)]
    {
        let parent = path
            .parent()
            .ok_or_else(|| HarnessError::execution(format!("{} has no parent", path.display())))?;
        tokio::fs::File::open(parent)
            .await
            .map_err(|error| {
                HarnessError::execution(format!(
                    "open state directory {}: {error}",
                    parent.display()
                ))
            })?
            .sync_all()
            .await
            .map_err(|error| {
                HarnessError::execution(format!(
                    "sync state directory {}: {error}",
                    parent.display()
                ))
            })?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

#[cfg(unix)]
async fn set_private_permissions(path: &Path) -> Result<(), HarnessError> {
    use std::os::unix::fs::PermissionsExt;
    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .await
        .map_err(|error| {
            HarnessError::execution(format!(
                "set private permissions on {}: {error}",
                path.display()
            ))
        })
}

#[cfg(not(unix))]
async fn set_private_permissions(_: &Path) -> Result<(), HarnessError> {
    Ok(())
}

/// Newly created instance directories are private on POSIX.
pub(crate) fn create_private_directory(path: &Path) -> Result<(), HarnessError> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path).map_err(|error| {
        HarnessError::execution(format!(
            "create private data directory {}: {error}",
            path.display()
        ))
    })
}
