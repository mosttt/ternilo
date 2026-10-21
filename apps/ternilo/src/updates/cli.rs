use super::{
    config::{RestartMethod, UpdateSettings, io_error, load_settings, write_private},
    read_progress,
};
use clap::{Subcommand, ValueEnum};
use std::{
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
};
use ternilo_protocol::HarnessError;

#[derive(Clone, Copy, ValueEnum)]
pub enum RestartKind {
    Direct,
    Systemd,
    Supervisor,
}
#[derive(Subcommand)]
pub enum UpdateCommand {
    /// Allow this installation to be updated by its registered Server after an idle check.
    Enable {
        #[arg(long, env = "TERNILO_LOCAL_DATA_DIR")]
        data_dir: Option<PathBuf>,
        #[arg(long, value_enum)]
        restart: RestartKind,
        /// Exact systemd unit or Supervisor program name; never a shell command.
        #[arg(long)]
        service: Option<String>,
        /// Use the system manager rather than the user's systemd manager.
        #[arg(long)]
        system: bool,
        #[arg(long)]
        supervisor_config: Option<PathBuf>,
    },
    /// Disable new remote updates; this does not interrupt an installation already in progress.
    Disable {
        #[arg(long, env = "TERNILO_LOCAL_DATA_DIR")]
        data_dir: Option<PathBuf>,
    },
    /// Restore previous binaries after an interrupted installation; stop the service first.
    Recover {
        #[arg(long, env = "TERNILO_LOCAL_DATA_DIR")]
        data_dir: Option<PathBuf>,
    },
    /// Show local update settings and the most recent operation.
    Status {
        #[arg(long, env = "TERNILO_LOCAL_DATA_DIR")]
        data_dir: Option<PathBuf>,
    },
}

pub async fn execute(command: UpdateCommand) -> Result<(), HarnessError> {
    match command {
        UpdateCommand::Enable {
            data_dir,
            restart,
            service,
            system,
            supervisor_config,
        } => enable(data_dir, restart, service, system, supervisor_config)?,
        UpdateCommand::Disable { data_dir } => {
            let path = root(data_dir)?.join("config/client-updates-enabled.json");
            match fs::remove_file(path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(io_error(error)),
            }
            println!("New remote updates are disabled.");
        }
        UpdateCommand::Recover { data_dir } => recover_command(data_dir).await?,
        UpdateCommand::Status { data_dir } => {
            let root = root(data_dir)?;
            println!("{}", serde_json::to_string_pretty(&serde_json::json!({"enabled":super::config::enabled(&root)?,"settings":load_settings(&root)?,"progress":read_progress(&root)?})).map_err(|_| HarnessError::execution("encode update status"))?);
        }
    }
    Ok(())
}
fn enable(
    data_dir: Option<PathBuf>,
    restart: RestartKind,
    service: Option<String>,
    system: bool,
    supervisor_config: Option<PathBuf>,
) -> Result<(), HarnessError> {
    let root = root(data_dir)?;
    let service = || {
        service.ok_or_else(|| {
            HarnessError::invalid("--service is required for the selected restart method")
        })
    };
    let restart = match restart {
        RestartKind::Direct => {
            if system || supervisor_config.is_some() || service().is_ok() {
                return Err(HarnessError::invalid(
                    "direct restart does not accept service-manager options",
                ));
            }
            RestartMethod::Direct
        }
        RestartKind::Systemd => RestartMethod::Systemd {
            unit: service()?,
            user: !system,
        },
        RestartKind::Supervisor => RestartMethod::Supervisor {
            program: service()?,
            config: supervisor_config
                .map(std::fs::canonicalize)
                .transpose()
                .map_err(io_error)?,
        },
    };
    restart.validate()?;
    if cfg!(windows) && !matches!(restart, RestartMethod::Direct) {
        return Err(HarnessError::invalid(
            "this Windows client supports direct process restart; service-managed installations require their own adapter",
        ));
    }
    let executable = std::env::current_exe()
        .and_then(fs::canonicalize)
        .map_err(io_error)?;
    let installation = super::installation::Installation::detect(&executable)?;
    let target = ternilo_update::current_target()
        .ok_or_else(|| HarnessError::invalid("unsupported update platform"))?;
    for name in ternilo_update::executable_names(target) {
        if !fs::symlink_metadata(installation.bin().join(name))
            .map_err(io_error)?
            .is_file()
        {
            return Err(HarnessError::invalid(
                "install the complete official client bundle before enabling remote updates",
            ));
        }
    }
    let probe = tempfile::NamedTempFile::new_in(installation.control_dir()).map_err(|_| {
        HarnessError::policy("the current user cannot write to the client installation directory")
    })?;
    drop(probe);
    let owner = installation
        .control_dir()
        .join(".ternilo-update-owner.json");
    if owner.exists() {
        let previous: PathBuf = serde_json::from_slice(&fs::read(&owner).map_err(io_error)?)
            .map_err(|_| HarnessError::invalid("invalid installation owner record"))?;
        if previous != root {
            return Err(HarnessError::conflict(
                "another Ternilo instance manages this installation; use a separate client installation for this instance",
            ));
        }
    } else {
        write_private(&owner, &root)?;
    }
    let _lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(installation.control_dir().join(".ternilo-update.lock"))
        .map_err(io_error)?;
    write_private(
        &root.join("config/client-updates.json"),
        &UpdateSettings {
            installation,
            restart,
        },
    )?;
    write_private(&root.join("config/client-updates-enabled.json"), &true)?;
    println!(
        "Remote updates enabled. Restart this Ternilo service once to activate its installation lock, then refresh the computer in Server."
    );
    Ok(())
}

async fn recover_command(data_dir: Option<PathBuf>) -> Result<(), HarnessError> {
    let root = root(data_dir)?;
    if crate::service::discover(&root).await?.is_some() {
        return Err(HarnessError::conflict(
            "stop this Ternilo service before recovering an interrupted update",
        ));
    }
    let progress = read_progress(&root)?
        .ok_or_else(|| HarnessError::invalid("there is no client update to recover"))?;
    super::validate_request_id(&progress.id)?;
    if !matches!(
        progress.phase,
        ternilo_protocol::ClientUpdatePhase::Installing
            | ternilo_protocol::ClientUpdatePhase::AwaitingReconnect
            | ternilo_protocol::ClientUpdatePhase::Failed
    ) {
        return Err(HarnessError::conflict(
            "this client update does not need binary recovery",
        ));
    }
    let directory = root.join("cache/client-updates").join(&progress.id);
    let plan = directory.join("apply.json");
    if !plan.is_file() {
        return Err(HarnessError::invalid(
            "installation did not start; the original program is unchanged",
        ));
    }
    let helper = directory.join(if cfg!(windows) {
        "ternilo-recovery-helper.exe"
    } else {
        "ternilo-recovery-helper"
    });
    fs::copy(std::env::current_exe().map_err(io_error)?, &helper).map_err(io_error)?;
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(directory.join("recovery.log"))
        .map_err(io_error)?;
    let mut command = std::process::Command::new(helper);
    command
        .arg("update-recover")
        .arg("--plan")
        .arg(plan)
        .stdin(std::process::Stdio::null())
        .stdout(log.try_clone().map_err(io_error)?)
        .stderr(log);
    super::apply::detach(&mut command);
    command.spawn().map_err(io_error)?;
    println!(
        "Recovery started. Use ternilo updates status with the same data directory to inspect the result."
    );
    Ok(())
}

fn root(value: Option<PathBuf>) -> Result<PathBuf, HarnessError> {
    let root = value.map_or_else(ternilo_local::default_data_dir, Ok)?;
    ternilo_local::prepare_data_dir(&root)?;
    fs::canonicalize(root).map_err(io_error)
}

/// Cooperating instances sharing this installation prevent updates until they have all stopped.
pub(crate) fn installation_guard() -> Result<Option<File>, HarnessError> {
    let executable = std::env::current_exe()
        .and_then(fs::canonicalize)
        .map_err(io_error)?;
    let bin = executable.parent().unwrap_or(Path::new("."));
    let control = super::installation::versioned_root(bin).unwrap_or_else(|| bin.to_owned());
    let path = control.join(".ternilo-update.lock");
    let marker = control.join(".ternilo-installing.json");
    if marker.try_exists().map_err(io_error)? {
        return Err(HarnessError::unavailable(
            "client installation is incomplete; stop its service and run ternilo updates recover with the same data directory",
        ));
    }
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io_error(error)),
    };
    file.try_lock_shared().map_err(|_| {
        HarnessError::unavailable(
            "this client installation is being updated; restart after installation completes",
        )
    })?;
    Ok(Some(file))
}
