use std::{
    fs::File,
    path::{Component, Path},
};

use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt};
use cap_std::{
    ambient_authority,
    fs::{Dir, OpenOptions},
};

use super::{
    HarnessError, MAX_ENTRIES, WorkspaceDirectory, WorkspaceEntry, WorkspaceEntryKind, read_error,
};

pub(super) fn open_beneath(root: &Path, path: &str, directory: bool) -> Result<File, HarnessError> {
    if path.contains([':', '\\']) {
        return Err(HarnessError::policy("workspace path is outside its root"));
    }
    let mut parent = Dir::open_ambient_dir(root, ambient_authority()).map_err(read_error)?;
    let mut components = Path::new(path).components().peekable();
    while let Some(component) = components.next() {
        let Component::Normal(name) = component else {
            return Err(HarnessError::policy("workspace path is outside its root"));
        };
        if directory || components.peek().is_some() {
            parent = parent.open_dir_nofollow(name).map_err(read_error)?;
        } else {
            let mut options = OpenOptions::new();
            options.read(true).follow(FollowSymlinks::No);
            return parent
                .open_with(name, &options)
                .map(cap_std::fs::File::into_std)
                .map_err(read_error);
        }
    }
    if !directory {
        return Err(HarnessError::invalid(
            "only regular workspace files can be previewed",
        ));
    }
    Ok(parent.into_std_file())
}

pub(super) fn list(root: &Path, path: String) -> Result<WorkspaceDirectory, HarnessError> {
    let directory = Dir::from_std_file(open_beneath(root, &path, true)?);
    let mut entries = Vec::new();
    let mut truncated = false;
    for entry in directory.entries().map_err(read_error)? {
        let entry = entry.map_err(read_error)?;
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if entries.len() == MAX_ENTRIES {
            truncated = true;
            break;
        }
        let file_type = entry.file_type().map_err(read_error)?;
        let kind = if file_type.is_symlink() {
            WorkspaceEntryKind::Other
        } else if file_type.is_dir() {
            WorkspaceEntryKind::Directory
        } else if file_type.is_file() {
            WorkspaceEntryKind::File
        } else {
            WorkspaceEntryKind::Other
        };
        entries.push(WorkspaceEntry { name, kind });
    }
    Ok(WorkspaceDirectory {
        path,
        entries,
        truncated,
    })
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use super::*;

    #[test]
    fn reads_nested_files_and_rejects_paths_outside_the_directory() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("nested")).unwrap();
        std::fs::write(root.path().join("nested/text.txt"), "workspace only").unwrap();
        let mut content = String::new();
        open_beneath(root.path(), "nested/text.txt", false)
            .unwrap()
            .read_to_string(&mut content)
            .unwrap();
        assert_eq!(content, "workspace only");
        let listing = list(root.path(), "nested".to_owned()).unwrap();
        assert_eq!(listing.entries.len(), 1);
        assert_eq!(listing.entries[0].name, "text.txt");
        assert_eq!(listing.entries[0].kind, WorkspaceEntryKind::File);
        assert!(!listing.truncated);
        for path in [
            "..",
            "../secret",
            "/secret",
            "C:/secret",
            "C:secret",
            "nested/text.txt:stream",
            "nested/../secret",
            "\\\\server\\share",
            "",
        ] {
            assert!(open_beneath(root.path(), path, false).is_err(), "{path}");
        }
    }

    #[test]
    fn rejects_directory_links_leading_outside_the_workspace() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret"), "private").unwrap();
        let link = root.path().join("escape");
        #[cfg(unix)]
        std::os::unix::fs::symlink(outside.path(), &link).unwrap();
        #[cfg(windows)]
        {
            let result = std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(&link)
                .arg(outside.path())
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
        }
        assert!(open_beneath(root.path(), "escape/secret", false).is_err());
        assert!(list(root.path(), "escape".to_owned()).is_err());
        let listing = list(root.path(), String::new()).unwrap();
        assert_eq!(listing.entries[0].kind, WorkspaceEntryKind::Other);
    }
}
