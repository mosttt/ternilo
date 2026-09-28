#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    process::Stdio,
};

use ternilo_protocol::{HarnessError, WorkspaceApplication};
mod icons;

struct App {
    id: &'static str,
    label: &'static str,
    command: &'static str,
    args: &'static [&'static str],
}

const APPS: &[App] = &[
    App {
        id: "filemanager",
        label: "File Manager",
        command: "xdg-open",
        args: &[],
    },
    App {
        id: "cursor",
        label: "Cursor",
        command: "cursor",
        args: &[],
    },
    App {
        id: "vscode",
        label: "VS Code",
        command: "code",
        args: &[],
    },
    App {
        id: "vscodeinsiders",
        label: "VS Code Insiders",
        command: "code-insiders",
        args: &[],
    },
    App {
        id: "windsurf",
        label: "Windsurf",
        command: "windsurf",
        args: &[],
    },
    App {
        id: "zed",
        label: "Zed",
        command: "zed",
        args: &[],
    },
    App {
        id: "sublimetext",
        label: "Sublime Text",
        command: "subl",
        args: &[],
    },
    App {
        id: "androidstudio",
        label: "Android Studio",
        command: "studio",
        args: &[],
    },
    App {
        id: "intellij",
        label: "IntelliJ IDEA",
        command: "idea",
        args: &[],
    },
    App {
        id: "pycharm",
        label: "PyCharm",
        command: "pycharm",
        args: &[],
    },
    App {
        id: "webstorm",
        label: "WebStorm",
        command: "webstorm",
        args: &[],
    },
    App {
        id: "phpstorm",
        label: "PhpStorm",
        command: "phpstorm",
        args: &[],
    },
    App {
        id: "goland",
        label: "GoLand",
        command: "goland",
        args: &[],
    },
    App {
        id: "rider",
        label: "Rider",
        command: "rider",
        args: &[],
    },
    App {
        id: "rustrover",
        label: "RustRover",
        command: "rustrover",
        args: &[],
    },
    App {
        id: "sublimemerge",
        label: "Sublime Merge",
        command: "smerge",
        args: &[],
    },
    App {
        id: "ghostty",
        label: "Ghostty",
        command: "ghostty",
        args: &["--working-directory={path}"],
    },
    App {
        id: "kitty",
        label: "kitty",
        command: "kitty",
        args: &["--directory"],
    },
    App {
        id: "gnometerminal",
        label: "GNOME Terminal",
        command: "gnome-terminal",
        args: &["--working-directory={path}"],
    },
    App {
        id: "konsole",
        label: "Konsole",
        command: "konsole",
        args: &["--workdir"],
    },
];

fn desktop_available() -> bool {
    cfg!(target_os = "linux")
        && ["DISPLAY", "WAYLAND_DISPLAY"]
            .iter()
            .any(|name| std::env::var_os(name).is_some_and(|value| !value.is_empty()))
}

#[cfg(unix)]
fn executable(name: &str, search_path: &std::ffi::OsStr) -> Option<PathBuf> {
    std::env::split_paths(search_path)
        .filter(|path| path.is_absolute())
        .map(|path| path.join(name))
        .find(|path| {
            path.metadata().is_ok_and(|metadata| {
                metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
            })
        })
}

#[cfg(not(unix))]
fn executable(_: &str, _: &std::ffi::OsStr) -> Option<PathBuf> {
    None
}

pub(crate) fn applications() -> Vec<WorkspaceApplication> {
    if !desktop_available() {
        return Vec::new();
    }
    let search_path = std::env::var_os("PATH").unwrap_or_default();
    APPS.iter()
        .filter(|app| executable(app.command, &search_path).is_some())
        .map(|app| WorkspaceApplication {
            id: app.id.to_owned(),
            label: app.label.to_owned(),
            icon: icons::application_icon(app.id),
        })
        .collect()
}

fn launch_arguments(app: &App, path: &Path) -> Vec<OsString> {
    let mut arguments: Vec<OsString> = app
        .args
        .iter()
        .map(|argument| {
            if let Some(prefix) = argument.strip_suffix("{path}") {
                let mut value = OsString::from(prefix);
                value.push(path);
                value
            } else {
                OsString::from(argument)
            }
        })
        .collect();
    if !app.args.iter().any(|argument| argument.contains("{path}")) {
        arguments.push(path.into());
    }
    arguments
}

pub(crate) fn open(root: &Path, app_id: &str) -> Result<(), HarnessError> {
    if !desktop_available() {
        return Err(HarnessError::policy(
            "this computer has no supported desktop session",
        ));
    }
    let app = APPS
        .iter()
        .find(|app| app.id == app_id)
        .ok_or_else(|| HarnessError::invalid("unknown desktop application"))?;
    let command = executable(app.command, &std::env::var_os("PATH").unwrap_or_default())
        .ok_or_else(|| HarnessError::invalid("desktop application is no longer installed"))?;
    let root = root
        .canonicalize()
        .map_err(|error| HarnessError::execution(format!("open workspace: {error}")))?;
    if !root.is_dir() {
        return Err(HarnessError::invalid("workspace is not a directory"));
    }
    let mut child = tokio::process::Command::new(command);
    child
        .args(launch_arguments(app, &root))
        .current_dir(&root)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for name in [
        "HOME",
        "USER",
        "LOGNAME",
        "PATH",
        "LANG",
        "LC_ALL",
        "TERM",
        "SHELL",
        "DISPLAY",
        "WAYLAND_DISPLAY",
        "XAUTHORITY",
        "XDG_RUNTIME_DIR",
        "XDG_CURRENT_DESKTOP",
        "XDG_SESSION_TYPE",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "DBUS_SESSION_BUS_ADDRESS",
    ] {
        if let Some(value) = std::env::var_os(name) {
            child.env(name, value);
        }
    }
    let mut process = child
        .spawn()
        .map_err(|error| HarnessError::execution(format!("launch desktop application: {error}")))?;
    tokio::spawn(async move {
        let _ = process.wait().await;
    });
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn catalog_resolves_executables_and_preserves_paths_as_one_argument() {
        let root = tempfile::tempdir().unwrap();
        let binary = root.path().join("code");
        std::fs::write(&binary, "fixture").unwrap();
        assert!(executable("code", root.path().as_os_str()).is_none());
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(executable("code", root.path().as_os_str()), Some(binary));
        assert!(executable("code", std::ffi::OsStr::new(".")).is_none());
        let path = Path::new("/tmp/a path;$(no shell)");
        let app = APPS.iter().find(|app| app.id == "gnometerminal").unwrap();
        assert_eq!(
            launch_arguments(app, path),
            vec![OsString::from(
                "--working-directory=/tmp/a path;$(no shell)"
            )]
        );
    }
}
