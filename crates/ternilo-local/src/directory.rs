use std::{
    collections::BTreeMap,
    path::{Component, Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use ternilo_protocol::HarnessError;

const MAX_DIRECTORY_ENTRIES: usize = 1_000;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DirectoryEntry {
    pub name: String,
    pub path: String,
    pub hidden: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DirectoryListing {
    pub path: String,
    pub home: String,
    pub crumbs: Vec<DirectoryEntry>,
    pub entries: Vec<DirectoryEntry>,
    pub truncated: bool,
}

pub fn home_directory() -> Result<PathBuf, HarnessError> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| HarnessError::execution("cannot resolve the host home directory"))
}

pub async fn canonical_directory(path: impl AsRef<Path>) -> Result<PathBuf, HarnessError> {
    let path = path.as_ref();
    if !path.is_absolute() {
        return Err(HarnessError::invalid(format!(
            "directory path must be absolute: {}",
            path.display()
        )));
    }
    let canonical = tokio::fs::canonicalize(path).await.map_err(|error| {
        HarnessError::invalid(format!(
            "cannot resolve directory {}: {error}",
            path.display()
        ))
    })?;
    let metadata = tokio::fs::metadata(&canonical).await.map_err(|error| {
        HarnessError::invalid(format!(
            "cannot inspect directory {}: {error}",
            canonical.display()
        ))
    })?;
    if !metadata.is_dir() {
        return Err(HarnessError::invalid(format!(
            "path is not a directory: {}",
            canonical.display()
        )));
    }
    Ok(canonical)
}

pub async fn list_directory(path: Option<&str>) -> Result<DirectoryListing, HarnessError> {
    let home = canonical_directory(home_directory()?).await?;
    let target = match path {
        Some(path) => canonical_directory(path).await?,
        None => home.clone(),
    };
    let mut reader = tokio::fs::read_dir(&target).await.map_err(|error| {
        HarnessError::invalid(format!(
            "cannot read directory {}: {error}",
            target.display()
        ))
    })?;
    let mut retained = BTreeMap::<String, DirectoryEntry>::new();
    let mut truncated = false;
    while let Some(entry) = reader.next_entry().await.map_err(|error| {
        HarnessError::execution(format!("read directory {}: {error}", target.display()))
    })? {
        let file_type = entry.file_type().await.map_err(|error| {
            HarnessError::execution(format!("inspect {}: {error}", entry.path().display()))
        })?;
        let is_directory = if file_type.is_dir() {
            true
        } else if file_type.is_symlink() {
            tokio::fs::metadata(entry.path())
                .await
                .is_ok_and(|metadata| metadata.is_dir())
        } else {
            false
        };
        if !is_directory {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Some(path) = entry.path().to_str().map(str::to_owned) else {
            continue;
        };
        retained.insert(
            name.clone(),
            DirectoryEntry {
                hidden: name.starts_with('.'),
                name,
                path,
            },
        );
        if retained.len() > MAX_DIRECTORY_ENTRIES {
            let tail = retained
                .last_key_value()
                .map(|(name, _)| name.clone())
                .expect("non-empty directory window");
            retained.remove(&tail);
            truncated = true;
        }
    }
    Ok(DirectoryListing {
        path: path_string(&target)?,
        home: path_string(&home)?,
        crumbs: crumbs(&target)?,
        entries: retained.into_values().collect(),
        truncated,
    })
}

pub async fn create_directory(parent: &str, name: &str) -> Result<String, HarnessError> {
    let parent = canonical_directory(parent).await?;
    let mut components = Path::new(name).components();
    if name.trim().is_empty()
        || !matches!(components.next(), Some(Component::Normal(_)))
        || components.next().is_some()
    {
        return Err(HarnessError::invalid(
            "directory name must be one non-empty path segment",
        ));
    }
    // Whitespace is significant in a valid host path. `trim()` above is only
    // the blank-name gate; joining the original spelling keeps `project `
    // distinct from `project` all the way through the directory picker.
    let child = parent.join(name);
    tokio::fs::create_dir(&child).await.map_err(|error| {
        HarnessError::invalid(format!(
            "cannot create directory {}: {error}",
            child.display()
        ))
    })?;
    path_string(&canonical_directory(child).await?)
}

fn crumbs(target: &Path) -> Result<Vec<DirectoryEntry>, HarnessError> {
    let mut paths = target
        .ancestors()
        .map(Path::to_path_buf)
        .collect::<Vec<_>>();
    paths.reverse();
    paths
        .into_iter()
        .map(|path| {
            let label = path
                .file_name()
                .and_then(|name| name.to_str())
                .map_or_else(|| path.display().to_string(), str::to_owned);
            Ok(DirectoryEntry {
                name: label,
                path: path_string(&path)?,
                hidden: false,
            })
        })
        .collect()
}

fn path_string(path: &Path) -> Result<String, HarnessError> {
    path.to_str().map(str::to_owned).ok_or_else(|| {
        HarnessError::invalid(format!("path is not valid UTF-8: {}", path.display()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn relative_paths_are_rejected() {
        assert!(canonical_directory("relative").await.is_err());
    }

    #[tokio::test]
    async fn directory_names_are_single_segments() {
        let root = std::env::temp_dir();
        assert!(
            create_directory(root.to_str().unwrap(), "../escape")
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn directory_names_preserve_significant_trailing_whitespace() {
        let root = tempfile::tempdir().unwrap();
        let parent = root.path().to_str().unwrap();
        let created = create_directory(parent, "project ").await.unwrap();

        assert!(created.ends_with("project "));
        assert!(root.path().join("project ").is_dir());
        assert!(!root.path().join("project").exists());
    }
}
