use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, LazyLock, Mutex, Weak},
};
use tokio::sync::RwLock;

type LogAccess = RwLock<()>;

// Runtime writers and application snapshots construct separate stores for the same log.
static LOCKS: LazyLock<Mutex<BTreeMap<PathBuf, Weak<LogAccess>>>> =
    LazyLock::new(|| Mutex::new(BTreeMap::new()));

pub(super) fn for_path(path: &Path) -> Arc<LogAccess> {
    let mut locks = LOCKS
        .lock()
        .expect("session log access registry lock poisoned");
    if let Some(access) = locks.get(path).and_then(Weak::upgrade) {
        return access;
    }
    locks.retain(|_, access| access.strong_count() > 0);
    let access = Arc::new(RwLock::new(()));
    locks.insert(path.to_owned(), Arc::downgrade(&access));
    access
}
