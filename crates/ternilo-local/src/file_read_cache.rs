use std::{
    collections::VecDeque,
    fs::Metadata,
    path::{Path, PathBuf},
    time::SystemTime,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct FileVersion {
    size: u64,
    modified: SystemTime,
    #[cfg(unix)]
    identity: (u64, u64, i64, i64),
    #[cfg(not(unix))]
    created: SystemTime,
}

impl FileVersion {
    pub(crate) fn from_metadata(metadata: &Metadata) -> Option<Self> {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt as _;
        Some(Self {
            size: metadata.len(),
            modified: metadata.modified().ok()?,
            #[cfg(unix)]
            identity: (
                metadata.dev(),
                metadata.ino(),
                metadata.ctime(),
                metadata.ctime_nsec(),
            ),
            #[cfg(not(unix))]
            created: metadata.created().ok()?,
        })
    }
}

struct Entry {
    path: PathBuf,
    version: FileVersion,
    content: String,
}

/// Bounded session-local file reads. Callers open and authorize the file before lookup.
#[derive(Default)]
pub(crate) struct FileReadCache {
    entries: VecDeque<Entry>,
    bytes: usize,
}
impl FileReadCache {
    pub(crate) fn get(&mut self, path: &Path, version: &FileVersion) -> Option<String> {
        let index = self.entries.iter().position(|entry| entry.path == path)?;
        let entry = self.entries.remove(index)?;
        if entry.version != *version {
            self.bytes -= entry.content.len();
            return None;
        }
        let content = entry.content.clone();
        self.entries.push_back(entry);
        Some(content)
    }
    pub(crate) fn invalidate(&mut self, path: &Path) {
        if let Some(index) = self.entries.iter().position(|entry| entry.path == path)
            && let Some(entry) = self.entries.remove(index)
        {
            self.bytes -= entry.content.len();
        }
    }
    pub(crate) fn insert(
        &mut self,
        path: PathBuf,
        version: FileVersion,
        content: String,
        budget: usize,
    ) {
        self.invalidate(&path);
        if content.len() > budget || budget == 0 {
            return;
        }
        while self.bytes.saturating_add(content.len()) > budget {
            let Some(entry) = self.entries.pop_front() else {
                break;
            };
            self.bytes -= entry.content.len();
        }
        self.bytes += content.len();
        self.entries.push_back(Entry {
            path,
            version,
            content,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cache_reuses_reads_but_invalidates_external_writes_and_replacements() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("read.txt");
        std::fs::write(&path, "before").unwrap();
        let first = FileVersion::from_metadata(&std::fs::metadata(&path).unwrap()).unwrap();
        let mut cache = FileReadCache::default();
        cache.insert(path.clone(), first.clone(), "before".into(), 12);
        assert_eq!(cache.get(&path, &first).as_deref(), Some("before"));
        std::fs::write(&path, "after external edit").unwrap();
        let changed = FileVersion::from_metadata(&std::fs::metadata(&path).unwrap()).unwrap();
        assert!(cache.get(&path, &changed).is_none());
        cache.insert(path.clone(), changed.clone(), "updated".into(), 12);
        cache.insert(
            directory.path().join("other"),
            changed.clone(),
            "another".into(),
            12,
        );
        assert!(cache.get(&path, &changed).is_none());
        cache.insert(path.clone(), changed.clone(), "updated".into(), 12);
        cache.invalidate(&path);
        assert!(cache.get(&path, &changed).is_none());
    }
}
