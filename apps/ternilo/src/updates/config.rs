use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};
use ternilo_protocol::HarnessError;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RestartMethod {
    Direct,
    Systemd {
        unit: String,
        user: bool,
    },
    Supervisor {
        program: String,
        config: Option<PathBuf>,
    },
    LaunchAgent {
        label: String,
        domain: String,
        plist: PathBuf,
    },
}
impl RestartMethod {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Systemd { .. } => "systemd",
            Self::Supervisor { .. } => "supervisor",
            Self::LaunchAgent { .. } => "launch_agent",
        }
    }
    pub fn validate(&self) -> Result<(), HarnessError> {
        let name = match self {
            Self::Direct => return Ok(()),
            Self::Systemd { unit, .. } => unit,
            Self::Supervisor { program, .. } => program,
            Self::LaunchAgent {
                label,
                domain,
                plist,
            } => {
                let (kind, uid) = domain.split_once('/').unwrap_or_default();
                if !matches!(kind, "gui" | "user")
                    || uid.is_empty()
                    || !uid.bytes().all(|byte| byte.is_ascii_digit())
                    || label.contains('/')
                    || !plist.is_absolute()
                {
                    return Err(HarnessError::invalid(
                        "LaunchAgent requires a gui/<uid> or user/<uid> domain, one Label and an absolute plist path",
                    ));
                }
                label
            }
        };
        if name.is_empty()
            || name.starts_with('-')
            || name.len() > 255
            || name.chars().any(char::is_whitespace)
            || name.contains('*')
        {
            return Err(HarnessError::invalid(
                "provide one exact service name, without options or wildcards",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateSettings {
    pub installation: super::installation::Installation,
    pub restart: RestartMethod,
}

pub fn load_settings(root: &Path) -> Result<Option<UpdateSettings>, HarnessError> {
    let file = root.join("config/client-updates.json");
    let bytes = match fs::read(file) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io_error(error)),
    };
    let settings: UpdateSettings = serde_json::from_slice(&bytes)
        .map_err(|_| HarnessError::invalid("invalid local client update settings"))?;
    settings.restart.validate()?;
    settings.installation.validate()?;
    Ok(Some(settings))
}

// Owner consent is separate from deployment metadata: activating or restoring a release
// must never undo a concurrent local disable operation.
pub fn enabled(root: &Path) -> Result<bool, HarnessError> {
    root.join("config/client-updates-enabled.json")
        .try_exists()
        .map_err(io_error)
}

/// Serialize installation/recovery across process restarts; OS locks vanish on a crash.
pub fn operation_guard(root: &Path) -> Result<fs::File, HarnessError> {
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(root.join("runtime/client-update-operation.lock"))
        .map_err(io_error)?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(fs::TryLockError::WouldBlock) => Err(HarnessError::conflict(
            "another client update or recovery is still running; wait before retrying",
        )),
        Err(fs::TryLockError::Error(error)) => Err(io_error(error)),
    }
}

pub fn write_private<T: Serialize>(path: &Path, value: &T) -> Result<(), HarnessError> {
    let parent = path
        .parent()
        .ok_or_else(|| HarnessError::invalid("update state path has no parent"))?;
    fs::create_dir_all(parent).map_err(io_error)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent).map_err(io_error)?;
    serde_json::to_writer(&mut temporary, value)
        .map_err(|_| HarnessError::execution("encode client update state"))?;
    temporary
        .flush()
        .and_then(|()| temporary.as_file().sync_all())
        .map_err(io_error)?;
    temporary
        .persist(path)
        .map_err(|error| io_error(error.error))?;
    super::installation::sync_directory(parent)
}

pub fn io_error(error: impl std::fmt::Display) -> HarnessError {
    HarnessError::execution(format!("client update: {error}"))
}
