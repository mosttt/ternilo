//! A persisted random identity prevents an empty replacement directory from impersonating a volume.

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use ternilo_protocol::HarnessError;

const MARKER: &str = ".ternilo-storage.json";

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StorageRoot {
    version: u32,
    storage_id: String,
    root_id: String,
}

/// Retain the root accepted at registration; later lookups must not adopt a replacement volume.
#[derive(Clone)]
pub(crate) struct RegisteredStorageRoot {
    path: PathBuf,
    storage_id: String,
    root_id: String,
    directory: Arc<fs::File>,
}

impl RegisteredStorageRoot {
    pub(crate) fn initialize(
        path: &Path,
        storage_id: &str,
        expected_root_id: Option<&str>,
    ) -> Result<Self, HarnessError> {
        // An existing binding is read-only at startup: a missing mount must never turn into
        // a newly initialized directory on the host's underlying disk.
        if expected_root_id.is_none() {
            fs::create_dir_all(path).map_err(storage_error)?;
        }
        let metadata = fs::symlink_metadata(path).map_err(storage_error)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(HarnessError::policy(
                "Worker storage root must be a real directory, not a symbolic link",
            ));
        }
        let path = fs::canonicalize(path).map_err(storage_error)?;
        let root_id = if let Some(expected) = expected_root_id {
            expected.to_owned()
        } else {
            load_or_create(&path, storage_id)?
        };
        Self::open(&path, storage_id, &root_id)
    }

    pub(crate) fn open(path: &Path, storage_id: &str, root_id: &str) -> Result<Self, HarnessError> {
        let root = Self {
            path: path.to_owned(),
            storage_id: storage_id.to_owned(),
            root_id: root_id.to_owned(),
            directory: Arc::new(fs::File::open(path).map_err(storage_error)?),
        };
        root.validate()?;
        Ok(root)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn root_id(&self) -> &str {
        &self.root_id
    }

    pub(crate) fn storage_id(&self) -> &str {
        &self.storage_id
    }

    pub(crate) fn validate(&self) -> Result<(), HarnessError> {
        let metadata = fs::symlink_metadata(&self.path).map_err(storage_error)?;
        if !metadata.is_dir() {
            return Err(HarnessError::policy(
                "registered Worker storage root is no longer a directory",
            ));
        }
        let original = self.directory.metadata().map_err(storage_error)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            if (metadata.dev(), metadata.ino()) != (original.dev(), original.ino()) {
                return Err(HarnessError::policy(
                    "registered Worker storage root was replaced; restore the original storage before running tasks",
                ));
            }
        }
        #[cfg(not(unix))]
        if !original.is_dir() {
            return Err(HarnessError::policy(
                "registered Worker storage root is unavailable",
            ));
        }
        let marker = self.path.join(MARKER);
        if !fs::symlink_metadata(&marker)
            .map_err(storage_error)?
            .is_file()
        {
            return Err(HarnessError::policy(
                "registered Worker storage identity must remain a regular file",
            ));
        }
        let current = read_marker(&fs::read(marker).map_err(storage_error)?, &self.storage_id)?;
        if current != self.root_id {
            return Err(HarnessError::policy(
                "registered Worker storage identity changed; restore the original storage before running tasks",
            ));
        }
        Ok(())
    }
}

pub(crate) fn load_or_create(root: &Path, storage_id: &str) -> Result<String, HarnessError> {
    let marker = root.join(MARKER);
    match fs::read(&marker) {
        Ok(bytes) => read_marker(&bytes, storage_id),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let record = StorageRoot {
                version: 1,
                storage_id: storage_id.to_owned(),
                root_id: URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>()),
            };
            let bytes = serde_json::to_vec(&record).map_err(|error| {
                HarnessError::execution(format!("encode Worker storage identity: {error}"))
            })?;
            let mut file = tempfile::NamedTempFile::new_in(root).map_err(storage_error)?;
            file.write_all(&bytes)
                .and_then(|()| file.as_file().sync_all())
                .map_err(storage_error)?;
            match file.persist_noclobber(&marker) {
                Ok(_) => {
                    fs::File::open(root)
                        .and_then(|directory| directory.sync_all())
                        .map_err(storage_error)?;
                    Ok(record.root_id)
                }
                Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                    read_marker(&fs::read(marker).map_err(storage_error)?, storage_id)
                }
                Err(error) => Err(storage_error(error.error)),
            }
        }
        Err(error) => Err(storage_error(error)),
    }
}

fn read_marker(bytes: &[u8], storage_id: &str) -> Result<String, HarnessError> {
    let record: StorageRoot = serde_json::from_slice(bytes)
        .map_err(|_| HarnessError::invalid("Worker storage identity file is invalid"))?;
    if record.version != 1 || record.root_id.is_empty() || record.storage_id != storage_id {
        return Err(HarnessError::policy(
            "Worker workspace root belongs to another storage binding or has an invalid identity",
        ));
    }
    Ok(record.root_id)
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "Use the error mapper directly with Result::map_err."
)]
fn storage_error(error: std::io::Error) -> HarnessError {
    HarnessError::execution(format!("access Worker storage identity: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bound_storage_startup_never_initializes_a_missing_or_replacement_volume() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("volume");
        let original = RegisteredStorageRoot::initialize(&path, "storage-one", None).unwrap();
        let expected = original.root_id().to_owned();
        let displaced = temporary.path().join("displaced");
        fs::rename(&path, &displaced).unwrap();

        assert!(RegisteredStorageRoot::initialize(&path, "storage-one", Some(&expected)).is_err());
        assert!(
            !path.exists(),
            "startup must not create an absent registered mount"
        );
        fs::create_dir(&path).unwrap();
        assert!(RegisteredStorageRoot::initialize(&path, "storage-one", Some(&expected)).is_err());
        assert_eq!(
            fs::read_dir(&path).unwrap().count(),
            0,
            "startup must not initialize an empty replacement mount"
        );

        fs::remove_dir(&path).unwrap();
        fs::rename(&displaced, &path).unwrap();
        let restored =
            RegisteredStorageRoot::initialize(&path, "storage-one", Some(&expected)).unwrap();
        assert_eq!(restored.root_id(), expected);
        original.validate().unwrap();
    }

    #[test]
    fn startup_preserves_an_unrelated_volumes_identity() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let original =
            RegisteredStorageRoot::initialize(first.path(), "storage-one", None).unwrap();
        let replacement =
            RegisteredStorageRoot::initialize(second.path(), "storage-one", None).unwrap();
        let marker_before = fs::read(second.path().join(MARKER)).unwrap();
        assert!(
            RegisteredStorageRoot::initialize(
                second.path(),
                "storage-one",
                Some(original.root_id())
            )
            .is_err()
        );
        assert_eq!(fs::read(second.path().join(MARKER)).unwrap(), marker_before);
        replacement.validate().unwrap();
    }

    #[test]
    fn reopening_preserves_identity_and_an_empty_replacement_gets_a_new_identity() {
        let first = tempfile::tempdir().unwrap();
        let original = load_or_create(first.path(), "storage-one").unwrap();
        assert_eq!(
            load_or_create(first.path(), "storage-one").unwrap(),
            original
        );
        assert!(load_or_create(first.path(), "storage-two").is_err());
        let replacement = tempfile::tempdir().unwrap();
        assert_ne!(
            load_or_create(replacement.path(), "storage-one").unwrap(),
            original
        );
        assert_eq!(
            load_or_create(first.path(), "storage-one").unwrap(),
            original
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(first.path().join(MARKER))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn a_damaged_identity_is_never_silently_replaced() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join(MARKER), "incomplete").unwrap();
        assert!(load_or_create(root.path(), "storage-one").is_err());
        assert_eq!(
            fs::read_to_string(root.path().join(MARKER)).unwrap(),
            "incomplete"
        );
    }
}
