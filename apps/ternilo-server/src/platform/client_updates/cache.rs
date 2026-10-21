use std::{
    path::{Path, PathBuf},
    time::SystemTime,
};
use ternilo_protocol::HarnessError;
use ternilo_update::{SignedRelease, VerifiedRelease};
use tokio::sync::Mutex;

pub(crate) struct UpdateCache {
    root: PathBuf,
    gate: Mutex<()>,
    #[cfg(test)]
    _temporary: Option<tempfile::TempDir>,
}
const CACHE_LIMIT: u64 = 2 * 1024 * 1024 * 1024;
impl UpdateCache {
    pub(crate) fn new(root: PathBuf) -> Self {
        Self {
            root,
            gate: Mutex::new(()),
            #[cfg(test)]
            _temporary: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn temporary() -> std::sync::Arc<Self> {
        let temporary = tempfile::tempdir().expect("test cache directory");
        std::sync::Arc::new(Self {
            root: temporary.path().to_owned(),
            gate: Mutex::new(()),
            _temporary: Some(temporary),
        })
    }

    pub(crate) async fn release(&self, version: &str) -> Result<VerifiedRelease, HarnessError> {
        let parsed = semver::Version::parse(version)
            .map_err(|_| HarnessError::invalid("invalid update version"))?;
        if !parsed.pre.is_empty() || !parsed.build.is_empty() || parsed.to_string() != version {
            return Err(HarnessError::invalid("select a stable client release"));
        }
        let _gate = self.gate.lock().await;
        let path = self.root.join(format!("release-{version}.json"));
        if path.try_exists().map_err(storage_error)? {
            let signed: SignedRelease =
                serde_json::from_slice(&tokio::fs::read(&path).await.map_err(storage_error)?)
                    .map_err(|_| HarnessError::invalid("invalid cached update manifest"))?;
            let release = signed.verify()?;
            if release.manifest().version != version {
                return Err(HarnessError::policy(
                    "cached release version does not match",
                ));
            }
            return Ok(release);
        }
        let release = ternilo_update::fetch_release(version).await?;
        tokio::fs::create_dir_all(&self.root)
            .await
            .map_err(storage_error)?;
        let mut file = tempfile::NamedTempFile::new_in(&self.root).map_err(storage_error)?;
        serde_json::to_writer(&mut file, release.signed())
            .map_err(|_| HarnessError::execution("write signed release cache"))?;
        file.as_file().sync_all().map_err(storage_error)?;
        file.persist(path)
            .map_err(|error| storage_error(error.error))?;
        Ok(release)
    }

    pub(crate) async fn package(
        &self,
        release: &VerifiedRelease,
        target: &str,
    ) -> Result<PathBuf, HarnessError> {
        let artifact = release.artifact(target)?.clone();
        let path = self.root.join(&artifact.filename);
        let _gate = self.gate.lock().await;
        if path.try_exists().map_err(storage_error)? {
            let cached = path.clone();
            let expected = artifact.clone();
            tokio::task::spawn_blocking(move || ternilo_update::verify_file(&cached, &expected))
                .await
                .map_err(|_| HarnessError::execution("verify cached client package"))??;
            return Ok(path);
        }
        tokio::fs::create_dir_all(&self.root)
            .await
            .map_err(storage_error)?;
        let root = self.root.clone();
        let required = artifact.bytes;
        tokio::task::spawn_blocking(move || make_room(&root, required))
            .await
            .map_err(|_| HarnessError::execution("prepare client package cache"))??;
        ternilo_update::download_artifact(release, target, &path).await?;
        Ok(path)
    }
}
fn make_room(root: &Path, required: u64) -> Result<(), HarnessError> {
    let mut entries = Vec::new();
    let mut total = 0_u64;
    for entry in std::fs::read_dir(root).map_err(storage_error)? {
        let entry = entry.map_err(storage_error)?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("ternilo-") || !(name.ends_with(".tar.gz") || name.ends_with(".zip")) {
            continue;
        }
        let metadata = entry.metadata().map_err(storage_error)?;
        if metadata.is_file() {
            total += metadata.len();
            entries.push((
                metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                entry.path(),
                metadata.len(),
            ));
        }
    }
    entries.sort_by_key(|(modified, _, _)| *modified);
    for (_, path, size) in entries {
        if total.saturating_add(required) <= CACHE_LIMIT {
            break;
        }
        std::fs::remove_file(path).map_err(storage_error)?;
        total -= size;
    }
    if total.saturating_add(required) > CACHE_LIMIT {
        return Err(HarnessError::unavailable("client update cache is full"));
    }
    Ok(())
}
fn storage_error(error: impl std::fmt::Display) -> HarnessError {
    HarnessError::execution(format!("client update cache: {error}"))
}
