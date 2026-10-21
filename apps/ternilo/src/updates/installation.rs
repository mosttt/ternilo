//! Installation layout and reversible activation, independent of the service manager.
use super::config::{io_error, write_private};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};
use ternilo_protocol::HarnessError;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Installation {
    Flat { bin: PathBuf },
    Versioned { root: PathBuf, bin: PathBuf },
}

impl Installation {
    pub fn detect(executable: &Path) -> Result<Self, HarnessError> {
        let bin = executable
            .parent()
            .ok_or_else(|| {
                HarnessError::invalid("client executable has no installation directory")
            })?
            .to_owned();
        if let Some(root) = versioned_root(&bin) {
            let layout = Self::Versioned { root, bin };
            layout.launcher()?;
            Ok(layout)
        } else {
            Ok(Self::Flat { bin })
        }
    }

    pub fn bin(&self) -> &Path {
        match self {
            Self::Flat { bin } | Self::Versioned { bin, .. } => bin,
        }
    }

    pub fn control_dir(&self) -> &Path {
        match self {
            Self::Flat { bin } => bin,
            Self::Versioned { root, .. } => root,
        }
    }

    pub fn validate(&self) -> Result<(), HarnessError> {
        if !self.bin().is_absolute() || !self.control_dir().is_absolute() {
            return Err(HarnessError::invalid(
                "client installation paths must be absolute",
            ));
        }
        if let Self::Versioned { root, bin } = self
            && versioned_root(bin).as_ref() != Some(root)
        {
            return Err(HarnessError::invalid(
                "invalid versioned client installation",
            ));
        }
        Ok(())
    }

    fn launcher(&self) -> Result<String, HarnessError> {
        let Self::Versioned { root, bin } = self else {
            unreachable!()
        };
        let source = fs::read_to_string(root.join("bin/ternilo")).map_err(io_error)?;
        let assignment = binary_assignment(&bin.join("ternilo"))?;
        if source
            .lines()
            .filter(|line| line.starts_with("ternilo_binary="))
            .count()
            != 1
            || !source.lines().any(|line| line == assignment)
        {
            return Err(HarnessError::policy(
                "the installed launcher no longer points to this client; use the official launcher before enabling updates",
            ));
        }
        if fs::read_link(root.join("bin/ternilo-plugin")).map_err(io_error)?
            != bin.join("ternilo-plugin")
        {
            return Err(HarnessError::policy(
                "the plugin launcher no longer belongs to this client installation",
            ));
        }
        Ok(source)
    }

    /// Do not overwrite a version directory created by a manual installation.
    pub fn check_destination(&self, version: &str, target: &str) -> Result<(), HarnessError> {
        if let Self::Versioned { root, .. } = self {
            self.launcher()?;
            if root
                .join("releases")
                .join(format!("{version}-{target}"))
                .try_exists()
                .map_err(io_error)?
            {
                return Err(HarnessError::conflict(
                    "the target release directory already exists; activate that installation locally or remove the unused directory before retrying",
                ));
            }
        }
        Ok(())
    }

    /// Caller holds the stable installation lock and has marked activation incomplete.
    pub fn install(
        &self,
        staging: &Path,
        backup: &Path,
        version: &str,
        target: &str,
    ) -> Result<Self, HarnessError> {
        match self {
            Self::Flat { bin } => {
                ternilo_update::install_bundle(staging, bin, backup, target)?;
                Ok(self.clone())
            }
            Self::Versioned { root, bin } => {
                let source = self.launcher()?;
                self.check_destination(version, target)?;
                fs::create_dir(backup).map_err(io_error)?;
                copy_synced(&root.join("bin/ternilo"), &backup.join("ternilo"))?;
                write_private(
                    &backup.join("plugin-link.json"),
                    &bin.join("ternilo-plugin"),
                )?;
                let releases = root.join("releases");
                let prepared = tempfile::Builder::new()
                    .prefix(".ternilo-release-")
                    .tempdir_in(&releases)
                    .map_err(io_error)?;
                let prepared_bin = prepared.path().join("bin");
                fs::create_dir(&prepared_bin).map_err(io_error)?;
                for name in ternilo_update::executable_names(target) {
                    copy_synced(&staging.join(name), &prepared_bin.join(name))?;
                }
                sync_directory(&prepared_bin)?;
                sync_directory(prepared.path())?;
                let destination = releases.join(format!("{version}-{target}"));
                fs::rename(prepared.path(), &destination).map_err(io_error)?;
                sync_directory(&releases)?;
                let next_bin = destination.join("bin");
                let source = source.replacen(
                    &binary_assignment(&bin.join("ternilo"))?,
                    &binary_assignment(&next_bin.join("ternilo"))?,
                    1,
                );
                replace_file(
                    &root.join("bin/ternilo"),
                    source.as_bytes(),
                    &fs::metadata(backup.join("ternilo"))
                        .map_err(io_error)?
                        .permissions(),
                )?;
                replace_link(
                    &root.join("bin/ternilo-plugin"),
                    &next_bin.join("ternilo-plugin"),
                )?;
                Ok(Self::Versioned {
                    root: root.clone(),
                    bin: next_bin,
                })
            }
        }
    }

    pub fn restore(&self, backup: &Path, target: &str) -> Result<(), HarnessError> {
        match self {
            Self::Flat { bin } => ternilo_update::restore_bundle(bin, backup, target),
            Self::Versioned { root, .. } => {
                // Backups are retained so interruption during recovery remains recoverable.
                let wrapper = backup.join("ternilo");
                if wrapper.try_exists().map_err(io_error)? {
                    replace_file(
                        &root.join("bin/ternilo"),
                        &fs::read(&wrapper).map_err(io_error)?,
                        &fs::metadata(wrapper).map_err(io_error)?.permissions(),
                    )?;
                }
                let link = backup.join("plugin-link.json");
                if link.try_exists().map_err(io_error)? {
                    let target: PathBuf =
                        serde_json::from_slice(&fs::read(link).map_err(io_error)?)
                            .map_err(|_| HarnessError::invalid("invalid plugin launcher backup"))?;
                    replace_link(&root.join("bin/ternilo-plugin"), &target)?;
                }
                Ok(())
            }
        }
    }
}

/// Use the stable root even when an interrupted activation left the launcher incomplete.
pub fn versioned_root(bin: &Path) -> Option<PathBuf> {
    if bin.file_name()? != "bin" {
        return None;
    }
    let releases = bin.parent()?.parent()?;
    if releases.file_name()? != "releases" {
        return None;
    }
    Some(releases.parent()?.to_owned())
}

fn binary_assignment(binary: &Path) -> Result<String, HarnessError> {
    let path = binary
        .to_str()
        .ok_or_else(|| HarnessError::invalid("client launcher path is not UTF-8"))?;
    Ok(format!(
        "ternilo_binary='{}'",
        path.replace('\'', "'\"'\"'")
    ))
}
fn copy_synced(from: &Path, to: &Path) -> Result<(), HarnessError> {
    fs::copy(from, to).map_err(io_error)?;
    fs::File::open(to)
        .and_then(|file| file.sync_all())
        .map_err(io_error)
}
fn replace_file(
    path: &Path,
    bytes: &[u8],
    permissions: &fs::Permissions,
) -> Result<(), HarnessError> {
    let parent = path.parent().unwrap();
    let mut temporary = tempfile::NamedTempFile::new_in(parent).map_err(io_error)?;
    temporary.write_all(bytes).map_err(io_error)?;
    temporary
        .as_file()
        .set_permissions(permissions.clone())
        .map_err(io_error)?;
    temporary.as_file().sync_all().map_err(io_error)?;
    temporary
        .persist(path)
        .map_err(|error| io_error(error.error))?;
    sync_directory(parent)
}
fn replace_link(path: &Path, target: &Path) -> Result<(), HarnessError> {
    #[cfg(unix)]
    {
        let parent = path.parent().unwrap();
        let temporary = tempfile::tempdir_in(parent).map_err(io_error)?;
        let link = temporary.path().join("link");
        std::os::unix::fs::symlink(target, &link).map_err(io_error)?;
        fs::rename(link, path).map_err(io_error)?;
        sync_directory(parent)
    }
    #[cfg(not(unix))]
    {
        let _ = (path, target);
        Err(HarnessError::invalid(
            "versioned shell launchers require Unix",
        ))
    }
}
pub(super) fn sync_directory(path: &Path) -> Result<(), HarnessError> {
    #[cfg(unix)]
    fs::File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(io_error)?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

#[cfg(all(test, unix))]
mod tests;
