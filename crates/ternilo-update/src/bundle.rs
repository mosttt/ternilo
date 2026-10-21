use crate::{VerifiedRelease, manifest::io_error, verify_file};
use flate2::read::GzDecoder;
use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::Path,
};
use ternilo_protocol::HarnessError;

const EXECUTABLE_LIMIT: u64 = 512 * 1024 * 1024;

#[must_use]
pub fn executable_names(target: &str) -> Vec<&'static str> {
    if target.ends_with("windows-msvc") {
        vec![
            "ternilo.exe",
            "ternilo-plugin.exe",
            "ternilo-sandbox-windows.exe",
        ]
    } else {
        vec!["ternilo", "ternilo-plugin"]
    }
}

/// Extract only the release's explicitly expected regular executable files.
/// Never apply archive paths, links, ownership, permissions or installation scripts.
pub fn stage_bundle(
    release: &VerifiedRelease,
    target: &str,
    archive: &Path,
    parent: &Path,
) -> Result<tempfile::TempDir, HarnessError> {
    let artifact = release.artifact(target)?;
    verify_file(archive, artifact)?;
    fs::create_dir_all(parent).map_err(io_error)?;
    let staging = tempfile::Builder::new()
        .prefix("client-update-")
        .tempdir_in(parent)
        .map_err(io_error)?;
    let prefix = format!("ternilo-{}-{target}/bin/", release.manifest().version);
    let names = executable_names(target);
    let mut found = BTreeSet::new();
    let file = File::open(archive).map_err(io_error)?;
    if target.ends_with("windows-msvc") {
        let mut zip = zip::ZipArchive::new(file).map_err(archive_error)?;
        for index in 0..zip.len() {
            let mut entry = zip.by_index(index).map_err(archive_error)?;
            let path = entry.name().map_err(archive_error)?.into_owned();
            let Some(name) = path
                .strip_prefix(&prefix)
                .filter(|name| names.contains(name))
            else {
                continue;
            };
            if !entry.is_file()
                || entry
                    .unix_mode()
                    .is_some_and(|mode| mode & 0o170_000 == 0o120_000)
            {
                return Err(HarnessError::policy(
                    "client update executable is not a regular file",
                ));
            }
            extract_executable(&mut entry, name, staging.path(), &mut found)?;
        }
    } else {
        let mut tar = tar::Archive::new(GzDecoder::new(file));
        for entry in tar.entries().map_err(archive_error)? {
            let mut entry = entry.map_err(archive_error)?;
            let path = entry.path().map_err(archive_error)?.into_owned();
            let Some(name) = path
                .to_str()
                .and_then(|path| path.strip_prefix(&prefix))
                .filter(|name| names.contains(name))
            else {
                continue;
            };
            if !entry.header().entry_type().is_file() {
                return Err(HarnessError::policy(
                    "client update executable is not a regular file",
                ));
            }
            extract_executable(&mut entry, name, staging.path(), &mut found)?;
        }
    }
    if found.len() != names.len() {
        return Err(HarnessError::policy(
            "client update archive is missing required executables",
        ));
    }
    Ok(staging)
}

fn extract_executable(
    reader: &mut impl Read,
    name: &str,
    destination: &Path,
    found: &mut BTreeSet<String>,
) -> Result<(), HarnessError> {
    if !found.insert(name.to_owned()) {
        return Err(HarnessError::policy(
            "client update archive repeats an executable",
        ));
    }
    let path = destination.join(name);
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(io_error)?;
    let size =
        std::io::copy(&mut reader.take(EXECUTABLE_LIMIT + 1), &mut output).map_err(io_error)?;
    if size == 0 || size > EXECUTABLE_LIMIT {
        return Err(HarnessError::policy(
            "client update executable exceeds its size limit or is empty",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        output
            .set_permissions(fs::Permissions::from_mode(0o755))
            .map_err(io_error)?;
    }
    output
        .flush()
        .and_then(|()| output.sync_all())
        .map_err(io_error)
}

/// The caller must have stopped the service and hold its installation's exclusive lock.
/// The backup is a recovery journal: every moved original remains independently restorable.
pub fn install_bundle(
    staging: &Path,
    installation: &Path,
    backup: &Path,
    target: &str,
) -> Result<(), HarnessError> {
    let names = executable_names(target);
    for name in &names {
        for path in [staging.join(name), installation.join(name)] {
            if !fs::symlink_metadata(path).map_err(io_error)?.is_file() {
                return Err(HarnessError::policy(
                    "remote updates require a complete regular-file client installation",
                ));
            }
        }
    }
    fs::create_dir(backup).map_err(io_error)?;
    // Copy before touching the installed files; the temporary directory is on the same filesystem.
    let prepared = tempfile::Builder::new()
        .prefix(".ternilo-install-")
        .tempdir_in(installation)
        .map_err(io_error)?;
    for name in &names {
        let path = prepared.path().join(name);
        fs::copy(staging.join(name), &path).map_err(io_error)?;
        // Windows FlushFileBuffers requires a handle with write access.
        OpenOptions::new()
            .write(true)
            .open(path)
            .and_then(|file| file.sync_all())
            .map_err(io_error)?;
    }
    let install = || -> Result<(), HarnessError> {
        for name in &names {
            fs::rename(installation.join(name), backup.join(name)).map_err(io_error)?;
            fs::rename(prepared.path().join(name), installation.join(name)).map_err(io_error)?;
        }
        sync_directory(installation)?;
        sync_directory(backup)?;
        Ok(())
    };
    if let Err(error) = install() {
        restore_bundle(installation, backup, target)?;
        return Err(error);
    }
    Ok(())
}

pub fn restore_bundle(
    installation: &Path,
    backup: &Path,
    target: &str,
) -> Result<(), HarnessError> {
    for name in executable_names(target) {
        let original = backup.join(name);
        if original.try_exists().map_err(io_error)? {
            let installed = installation.join(name);
            if installed.try_exists().map_err(io_error)? {
                fs::remove_file(&installed).map_err(io_error)?;
            }
            fs::rename(&original, &installed).map_err(io_error)?;
        }
    }
    sync_directory(installation)
}

fn sync_directory(directory: &Path) -> Result<(), HarnessError> {
    #[cfg(unix)]
    File::open(directory)
        .and_then(|file| file.sync_all())
        .map_err(io_error)?;
    #[cfg(not(unix))]
    let _ = directory;
    Ok(())
}

fn archive_error(error: impl std::fmt::Display) -> HarnessError {
    HarnessError::invalid(format!("invalid client update archive: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tar_package(entries: &[(&str, tar::EntryType)]) -> Vec<u8> {
        let gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        let mut archive = tar::Builder::new(gzip);
        for (name, kind) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_mode(0o755);
            header.set_entry_type(*kind);
            let bytes: &[u8] = if kind.is_file() {
                b"fixture executable"
            } else {
                b""
            };
            header.set_size(bytes.len() as u64);
            if kind.is_symlink() {
                header.set_link_name("../../outside").unwrap();
            }
            header.set_cksum();
            archive
                .append_data(
                    &mut header,
                    format!("ternilo-0.2.21-x86_64-unknown-linux-gnu/bin/{name}"),
                    bytes,
                )
                .unwrap();
        }
        archive.into_inner().unwrap().finish().unwrap()
    }

    #[test]
    fn signed_archives_require_complete_regular_nonduplicated_executables() {
        let target = "x86_64-unknown-linux-gnu";
        for (entries, valid) in [
            (
                vec![
                    ("ternilo", tar::EntryType::Regular),
                    ("ternilo-plugin", tar::EntryType::Regular),
                ],
                true,
            ),
            (vec![("ternilo", tar::EntryType::Regular)], false),
            (
                vec![
                    ("ternilo", tar::EntryType::Regular),
                    ("ternilo", tar::EntryType::Regular),
                    ("ternilo-plugin", tar::EntryType::Regular),
                ],
                false,
            ),
            (
                vec![
                    ("ternilo", tar::EntryType::Symlink),
                    ("ternilo-plugin", tar::EntryType::Regular),
                ],
                false,
            ),
        ] {
            let root = tempfile::tempdir().unwrap();
            let bytes = tar_package(&entries);
            let release = crate::manifest::fixture_release(&bytes, target);
            let archive = root.path().join("package.tar.gz");
            fs::write(&archive, bytes).unwrap();
            let result = stage_bundle(&release, target, &archive, root.path());
            assert_eq!(result.is_ok(), valid);
            if let Ok(staged) = result {
                assert_eq!(
                    fs::read(staged.path().join("ternilo-plugin")).unwrap(),
                    b"fixture executable"
                );
            }
            assert!(!root.path().join("outside").exists());
        }
    }

    #[test]
    fn windows_zip_extracts_the_sandbox_helper_and_ignores_unrequested_paths() {
        let target = "aarch64-pc-windows-msvc";
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for name in executable_names(target) {
            zip.start_file(format!("ternilo-0.2.21-{target}/bin/{name}"), options)
                .unwrap();
            zip.write_all(b"fixture executable").unwrap();
        }
        zip.start_file("../../outside", options).unwrap();
        zip.write_all(b"not an installed file").unwrap();
        let bytes = zip.finish().unwrap().into_inner();
        let release = crate::manifest::fixture_release(&bytes, target);
        let root = tempfile::tempdir().unwrap();
        let archive = root.path().join("package.zip");
        fs::write(&archive, bytes).unwrap();
        let staged = stage_bundle(&release, target, &archive, root.path()).unwrap();
        assert_eq!(
            fs::read(staged.path().join("ternilo-sandbox-windows.exe")).unwrap(),
            b"fixture executable"
        );
        assert_eq!(fs::read_dir(staged.path()).unwrap().count(), 3);
        assert!(!root.path().join("outside").exists());
    }

    #[test]
    fn replaces_and_restores_the_complete_bundle_without_touching_configuration() {
        let root = tempfile::tempdir().unwrap();
        let installation = root.path().join("bin");
        let staged = root.path().join("staged");
        fs::create_dir(&installation).unwrap();
        fs::create_dir(&staged).unwrap();
        let target = "x86_64-unknown-linux-gnu";
        for name in executable_names(target) {
            fs::write(installation.join(name), b"old").unwrap();
            fs::write(staged.join(name), b"new").unwrap();
        }
        fs::write(installation.join("config.json"), b"private configuration").unwrap();
        let backup = installation.join("backup");
        install_bundle(&staged, &installation, &backup, target).unwrap();
        for name in executable_names(target) {
            assert_eq!(fs::read(installation.join(name)).unwrap(), b"new");
        }
        restore_bundle(&installation, &backup, target).unwrap();
        for name in executable_names(target) {
            assert_eq!(fs::read(installation.join(name)).unwrap(), b"old");
        }
        assert_eq!(
            fs::read(installation.join("config.json")).unwrap(),
            b"private configuration"
        );
    }

    #[test]
    fn partial_installation_recovery_restores_only_files_already_moved() {
        let root = tempfile::tempdir().unwrap();
        let backup = root.path().join("backup");
        fs::create_dir(&backup).unwrap();
        fs::write(root.path().join("ternilo"), b"new main").unwrap();
        fs::write(backup.join("ternilo"), b"old main").unwrap();
        fs::write(root.path().join("ternilo-plugin"), b"old plugin").unwrap();
        restore_bundle(root.path(), &backup, "x86_64-unknown-linux-gnu").unwrap();
        assert_eq!(fs::read(root.path().join("ternilo")).unwrap(), b"old main");
        assert_eq!(
            fs::read(root.path().join("ternilo-plugin")).unwrap(),
            b"old plugin"
        );
    }
}
