#[cfg(unix)]
use std::{fs::File, os::fd::OwnedFd, path::Component};
use std::{
    io::Read,
    path::{Path, PathBuf},
};

use base64::{Engine, engine::general_purpose::STANDARD};
#[cfg(unix)]
use nix::{
    dir::{Dir, Type},
    fcntl::{OFlag, openat},
    sys::stat::Mode,
};
use serde_json::Value;
use ternilo_protocol::{
    HarnessError, WorkspaceBrowserInfo, WorkspaceDirectory, WorkspaceEntry, WorkspaceEntryKind,
    WorkspacePreview, WorkspaceRequest,
};

use crate::LocalApplication;

#[cfg(any(windows, test))]
mod portable;
#[cfg(windows)]
use portable::{list, open_beneath};

const MAX_ENTRIES: usize = 10_000;
const MAX_PREVIEW: u64 = 2 * 1024 * 1024;

fn read_error(error: impl std::fmt::Display) -> HarnessError {
    HarnessError::execution(format!("workspace file access failed: {error}"))
}

#[cfg(unix)]
fn open_beneath(root: &Path, path: &str, directory: bool) -> Result<File, HarnessError> {
    let mut file = File::open(root).map_err(read_error)?;
    if !file.metadata().map_err(read_error)?.is_dir() {
        return Err(HarnessError::invalid("workspace root is not a directory"));
    }
    let components: Vec<_> = Path::new(path).components().collect();
    for (index, component) in components.iter().enumerate() {
        let Component::Normal(name) = component else {
            return Err(HarnessError::policy("workspace path is outside its root"));
        };
        let mut flags = OFlag::O_RDONLY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK;
        if directory || index + 1 < components.len() {
            flags |= OFlag::O_DIRECTORY;
        }
        let descriptor = openat(&file, *name, flags, Mode::empty()).map_err(read_error)?;
        file = File::from(descriptor);
    }
    Ok(file)
}

#[cfg(unix)]
fn list(root: &Path, path: String) -> Result<WorkspaceDirectory, HarnessError> {
    let file = open_beneath(root, &path, true)?;
    let mut directory = Dir::from_fd(OwnedFd::from(file)).map_err(read_error)?;
    let mut entries = Vec::new();
    let mut truncated = false;
    for entry in directory.iter() {
        let entry = entry.map_err(read_error)?;
        let Ok(name) = entry.file_name().to_str() else {
            continue;
        };
        if name == "." || name == ".." {
            continue;
        }
        if entries.len() == MAX_ENTRIES {
            truncated = true;
            break;
        }
        let kind = match entry.file_type() {
            Some(Type::Directory) => WorkspaceEntryKind::Directory,
            Some(Type::File) => WorkspaceEntryKind::File,
            _ => WorkspaceEntryKind::Other,
        };
        entries.push(WorkspaceEntry {
            name: name.to_owned(),
            kind,
        });
    }
    Ok(WorkspaceDirectory {
        path,
        entries,
        truncated,
    })
}

fn preview(root: &Path, path: String) -> Result<WorkspacePreview, HarnessError> {
    let file = open_beneath(root, &path, false)?;
    let metadata = file.metadata().map_err(read_error)?;
    if !metadata.is_file() {
        return Err(HarnessError::invalid(
            "only regular workspace files can be previewed",
        ));
    }
    let mut bytes = Vec::new();
    file.take(MAX_PREVIEW + 1)
        .read_to_end(&mut bytes)
        .map_err(read_error)?;
    let truncated = bytes.len() as u64 > MAX_PREVIEW;
    bytes.truncate(usize::try_from(MAX_PREVIEW).expect("preview limit fits usize"));
    let binary_type = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        Some("image/webp")
    } else if bytes.starts_with(b"%PDF-") {
        Some("application/pdf")
    } else {
        None
    };
    let (media_type, encoding, content) = if let Some(media_type) = binary_type {
        if truncated {
            (
                media_type.to_owned(),
                "unsupported".to_owned(),
                String::new(),
            )
        } else {
            (
                media_type.to_owned(),
                "base64".to_owned(),
                STANDARD.encode(bytes),
            )
        }
    } else if !bytes.contains(&0) {
        match std::str::from_utf8(&bytes) {
            Ok(content) => (
                "text/plain".to_owned(),
                "utf8".to_owned(),
                content.to_owned(),
            ),
            Err(error) if truncated && error.error_len().is_none() => (
                "text/plain".to_owned(),
                "utf8".to_owned(),
                String::from_utf8_lossy(&bytes[..error.valid_up_to()]).into_owned(),
            ),
            Err(_) => (
                "application/octet-stream".to_owned(),
                "unsupported".to_owned(),
                String::new(),
            ),
        }
    } else {
        (
            "application/octet-stream".to_owned(),
            "unsupported".to_owned(),
            String::new(),
        )
    };
    Ok(WorkspacePreview {
        path,
        bytes: metadata.len(),
        media_type,
        encoding,
        content,
        truncated,
    })
}

pub async fn browse_workspace(
    root: PathBuf,
    request: WorkspaceRequest,
) -> Result<Value, HarnessError> {
    request.validate()?;
    match request {
        WorkspaceRequest::Info => serde_json::to_value(WorkspaceBrowserInfo {
            root: root.to_string_lossy().into_owned(),
            can_browse: true,
            applications: Vec::new(),
        })
        .map_err(read_error),
        WorkspaceRequest::Open { .. } => Err(HarnessError::policy(
            "desktop applications are unavailable on this executor",
        )),
        request => tokio::task::spawn_blocking(move || match request {
            WorkspaceRequest::List { path } => {
                serde_json::to_value(list(&root, path)?).map_err(read_error)
            }
            WorkspaceRequest::Read { path } => {
                serde_json::to_value(preview(&root, path)?).map_err(read_error)
            }
            _ => unreachable!(),
        })
        .await
        .map_err(read_error)?,
    }
}

impl LocalApplication {
    pub async fn workspace_browser(
        &self,
        session_id: &str,
        request: WorkspaceRequest,
    ) -> Result<Value, HarnessError> {
        request.validate()?;
        let session = self
            .state
            .session(session_id)
            .await
            .ok_or_else(|| HarnessError::invalid("unknown workspace session"))?;
        let root = PathBuf::from(session.workspace_path);
        match request {
            WorkspaceRequest::Info => serde_json::to_value(WorkspaceBrowserInfo {
                root: root.to_string_lossy().into_owned(),
                can_browse: true,
                applications: crate::desktop_apps::applications(),
            })
            .map_err(read_error),
            WorkspaceRequest::Open { app_id } => {
                crate::desktop_apps::open(&root, &app_id)?;
                Ok(serde_json::json!({ "requested": true }))
            }
            request => browse_workspace(root, request).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[tokio::test]
    async fn workspace_preview_rejects_traversal_symlinks_and_special_files() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret"), "private").unwrap();
        std::fs::write(root.path().join("text.txt"), "workspace only").unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("escape")).unwrap();
        std::os::unix::fs::symlink(root.path().join("text.txt"), root.path().join("alias"))
            .unwrap();
        nix::unistd::mkfifo(&root.path().join("pipe"), Mode::S_IRUSR | Mode::S_IWUSR).unwrap();
        for path in [
            "../secret",
            "/etc/passwd",
            "escape/secret",
            "alias",
            "pipe",
            "",
        ] {
            assert!(
                browse_workspace(
                    root.path().to_owned(),
                    WorkspaceRequest::Read {
                        path: path.to_owned()
                    }
                )
                .await
                .is_err(),
                "{path}"
            );
        }
        let value = browse_workspace(
            root.path().to_owned(),
            WorkspaceRequest::Read {
                path: "text.txt".to_owned(),
            },
        )
        .await
        .unwrap();
        assert_eq!(value["content"], "workspace only");
        let listing = list(root.path(), String::new()).unwrap();
        assert_eq!(
            listing
                .entries
                .iter()
                .find(|entry| entry.name == "escape")
                .unwrap()
                .kind,
            WorkspaceEntryKind::Other
        );
    }

    #[test]
    fn preview_limits_text_and_does_not_render_unknown_binary() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("large.txt"), "好".repeat(800_000)).unwrap();
        let text = preview(root.path(), "large.txt".to_owned()).unwrap();
        assert!(text.truncated);
        assert_eq!(text.encoding, "utf8");
        assert!(!text.content.contains('\u{fffd}'));
        std::fs::write(root.path().join("opaque"), [0, 1, 2]).unwrap();
        assert_eq!(
            preview(root.path(), "opaque".to_owned()).unwrap().encoding,
            "unsupported"
        );
    }
}
