use super::{
    ApplyPlan, DATA_EPOCH,
    config::{
        RestartMethod, UpdateSettings, io_error, load_settings, operation_guard, write_private,
    },
    read_progress, validate_request_id,
};
use std::{
    fs::{self, OpenOptions},
    path::Path,
    process::Stdio,
    time::Duration,
};
use ternilo_protocol::{ClientUpdatePhase, HarnessError};
use ternilo_update::{SignedRelease, current_target};
use tokio::process::Command;

pub async fn apply(path: &Path) -> Result<(), HarnessError> {
    let plan: ApplyPlan = serde_json::from_slice(&fs::read(path).map_err(io_error)?)
        .map_err(|_| HarnessError::invalid("invalid private update installation plan"))?;
    validate_request_id(&plan.request.id)?;
    let _operation = operation_guard(&plan.data_dir)?;
    let result = apply_inner(&plan).await;
    if let Err(error) = &result {
        let _ = progress(&plan, ClientUpdatePhase::Failed, Some(error.to_string()));
    }
    result
}

/// Explicit local recovery runs from a copy outside the installation, including on Windows.
pub async fn recover(path: &Path) -> Result<(), HarnessError> {
    let plan: ApplyPlan = serde_json::from_slice(&fs::read(path).map_err(io_error)?)
        .map_err(|_| HarnessError::invalid("invalid private recovery plan"))?;
    validate_request_id(&plan.request.id)?;
    let _operation = operation_guard(&plan.data_dir)?;
    let release = SignedRelease {
        manifest: plan.request.manifest.clone(),
        signature: plan.request.signature.clone(),
    }
    .verify()?;
    if release.manifest().data_epoch != DATA_EPOCH {
        return Err(HarnessError::policy(
            "this data format requires a separate maintenance recovery",
        ));
    }
    if crate::service::discover(&plan.data_dir).await?.is_some() {
        return Err(HarnessError::conflict(
            "stop the local service before restoring its program files",
        ));
    }
    let target =
        current_target().ok_or_else(|| HarnessError::invalid("unsupported recovery platform"))?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .open(
            plan.settings
                .installation
                .control_dir()
                .join(".ternilo-update.lock"),
        )
        .map_err(io_error)?;
    lock.try_lock()
        .map_err(|_| HarnessError::conflict("another process is using this client installation"))?;
    if read_progress(&plan.data_dir)?.is_none_or(|progress| progress.id != plan.request.id) {
        return Err(HarnessError::conflict(
            "client update operation was replaced",
        ));
    }
    if !plan.backup.is_dir() {
        probe_version(
            &plan.settings.installation.bin().join(executable()),
            &plan.previous_version,
        )
        .await?;
        let marker = plan
            .settings
            .installation
            .control_dir()
            .join(".ternilo-installing.json");
        if marker.try_exists().map_err(io_error)? {
            clear_marker(&marker)?;
        }
        progress(
            &plan,
            ClientUpdatePhase::Failed,
            Some("update stopped before program replacement; original program retained".into()),
        )?;
        drop(lock);
        start_service(&plan).await?;
        return wait_for_version(&plan.data_dir, &plan.previous_version).await;
    }
    // The CLI that launched this helper may still be releasing its executable on Windows.
    let marker = plan
        .settings
        .installation
        .control_dir()
        .join(".ternilo-installing.json");
    write_private(&marker, &plan.request.id)?;
    let mut result = restore_installation(&plan, target);
    #[cfg(windows)]
    for _ in 0..20 {
        if result.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
        result = restore_installation(&plan, target);
    }
    #[cfg(not(windows))]
    let _ = &mut result;
    result?;
    probe_version(
        &plan.settings.installation.bin().join(executable()),
        &plan.previous_version,
    )
    .await?;
    if marker.try_exists().map_err(io_error)? {
        clear_marker(&marker)?;
    }
    drop(lock);
    progress(
        &plan,
        ClientUpdatePhase::RolledBack,
        Some("previous program restored by the local owner".into()),
    )?;
    start_service(&plan).await?;
    wait_for_version(&plan.data_dir, &plan.previous_version).await
}

async fn apply_inner(plan: &ApplyPlan) -> Result<(), HarnessError> {
    let release = SignedRelease {
        manifest: plan.request.manifest.clone(),
        signature: plan.request.signature.clone(),
    }
    .verify()?;
    release.validate_upgrade(
        &plan.previous_version,
        &plan.request.server_version,
        DATA_EPOCH,
    )?;
    let target =
        current_target().ok_or_else(|| HarnessError::invalid("unsupported update platform"))?;
    plan.settings
        .installation
        .check_destination(&release.manifest().version, target)?;
    // Verify and stage in the independent process before stopping the old service.
    let staged = ternilo_update::stage_bundle(
        &release,
        target,
        &plan.archive,
        plan.archive.parent().unwrap(),
    )?;
    probe_version(
        &staged.path().join(executable()),
        &release.manifest().version,
    )
    .await?;
    let pending = read_progress(&plan.data_dir)?
        .ok_or_else(|| HarnessError::conflict("update operation no longer exists"))?;
    if pending.id != plan.request.id || pending.phase != ClientUpdatePhase::Installing {
        return Err(HarnessError::conflict(
            "update operation is no longer ready to install",
        ));
    }
    let service = crate::service::discover(&plan.data_dir)
        .await?
        .ok_or_else(|| HarnessError::unavailable("local service stopped before update handoff"))?;
    if service.info.service_id != plan.service_id {
        return Err(HarnessError::conflict(
            "local service changed while preparing the update; retry from the new instance",
        ));
    }
    verify_manager_process(plan, service.info.pid).await?;
    activate(plan, staged.path(), &release.manifest().version, target).await
}

async fn activate(
    plan: &ApplyPlan,
    staging: &Path,
    version: &str,
    target: &str,
) -> Result<(), HarnessError> {
    stop_service(plan).await?;
    let lock = match installation_lock(plan).await {
        Ok(lock) => lock,
        Err(error) => {
            start_service(plan).await?;
            return Err(error);
        }
    };
    let marker = plan
        .settings
        .installation
        .control_dir()
        .join(".ternilo-installing.json");
    write_private(&marker, &plan.request.id)?;
    let installation = (|| {
        let next = plan
            .settings
            .installation
            .install(staging, &plan.backup, version, target)?;
        let mut settings = plan.settings.clone();
        settings.installation = next;
        write_private(&plan.data_dir.join("config/client-updates.json"), &settings)
    })();
    if let Err(error) = installation {
        restore_installation(plan, target)?;
        probe_version(
            &plan.settings.installation.bin().join(executable()),
            &plan.previous_version,
        )
        .await?;
        clear_marker(&marker)?;
        drop(lock);
        progress(plan, ClientUpdatePhase::Failed, Some(error.to_string()))?;
        start_service(plan).await?;
        return Err(error);
    }
    progress(plan, ClientUpdatePhase::AwaitingReconnect, None)?;
    clear_marker(&marker)?;
    drop(lock);
    let started = async {
        start_service(plan).await?;
        wait_for_version(&plan.data_dir, version).await
    }
    .await;
    if let Err(error) = started {
        // A successful local health check is enough to keep the new binary during a network outage.
        stop_service(plan).await?;
        let lock = installation_lock(plan).await?;
        write_private(&marker, &plan.request.id)?;
        restore_installation(plan, target)?;
        probe_version(
            &plan.settings.installation.bin().join(executable()),
            &plan.previous_version,
        )
        .await?;
        clear_marker(&marker)?;
        drop(lock);
        progress(plan, ClientUpdatePhase::RolledBack, Some(error.to_string()))?;
        start_service(plan).await?;
        wait_for_version(&plan.data_dir, &plan.previous_version).await?;
    }
    Ok(())
}

async fn installation_lock(plan: &ApplyPlan) -> Result<fs::File, HarnessError> {
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .open(
            plan.settings
                .installation
                .control_dir()
                .join(".ternilo-update.lock"),
        )
        .map_err(io_error)?;
    for _ in 0..120 {
        if lock.try_lock().is_ok() {
            return Ok(lock);
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    Err(HarnessError::conflict(
        "another process is using this client installation; stop it before retrying the update",
    ))
}
fn restore_installation(plan: &ApplyPlan, target: &str) -> Result<(), HarnessError> {
    plan.settings.installation.restore(&plan.backup, target)?;
    write_private(
        &plan.data_dir.join("config/client-updates.json"),
        &plan.settings,
    )
}
fn clear_marker(marker: &Path) -> Result<(), HarnessError> {
    fs::remove_file(marker).map_err(io_error)?;
    super::installation::sync_directory(marker.parent().unwrap())
}

pub(super) async fn probe_version(binary: &Path, expected: &str) -> Result<(), HarnessError> {
    let output = tokio::time::timeout(
        Duration::from_secs(15),
        Command::new(binary)
            .arg("--version")
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| HarnessError::unavailable("new client version check timed out"))?
    .map_err(io_error)?;
    if !output.status.success()
        || String::from_utf8_lossy(&output.stdout).trim() != format!("ternilo {expected}")
    {
        return Err(HarnessError::policy(
            "new client could not start or does not match the signed release version",
        ));
    }
    Ok(())
}
async fn wait_for_version(root: &Path, version: &str) -> Result<(), HarnessError> {
    for _ in 0..120 {
        if let Some(service) = crate::service::discover(root).await? {
            if service.info.version == version {
                return Ok(());
            }
            return Err(HarnessError::conflict(
                "service restarted with an unexpected client version",
            ));
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    Err(HarnessError::unavailable(
        "updated client did not pass its local startup health check",
    ))
}
async fn verify_manager_process(plan: &ApplyPlan, expected: u32) -> Result<(), HarnessError> {
    let mut command = match &plan.settings.restart {
        RestartMethod::Direct => return Ok(()),
        RestartMethod::Systemd { unit, user } => {
            let mut command = Command::new("systemctl");
            if *user {
                command.arg("--user");
            }
            command
                .args(["show", "--property=MainPID", "--property=Transient"])
                .arg(unit);
            command
        }
        RestartMethod::Supervisor { program, config } => {
            let mut command = Command::new("supervisorctl");
            if let Some(config) = config {
                command.arg("-c").arg(config);
            }
            command.arg("pid").arg(program);
            command
        }
    };
    command.stdin(Stdio::null()).kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(10), command.output())
        .await
        .map_err(|_| {
            HarnessError::unavailable("could not verify the service manager process in time")
        })?
        .map_err(io_error)?;
    let output_text = String::from_utf8_lossy(&output.stdout);
    let systemd = matches!(plan.settings.restart, RestartMethod::Systemd { .. });
    let pid = if systemd {
        output_text
            .lines()
            .find_map(|line| line.strip_prefix("MainPID="))
    } else {
        Some(output_text.trim())
    }
    .and_then(|value| value.parse::<u32>().ok());
    if !output.status.success() || pid != Some(expected) {
        return Err(HarnessError::policy(
            "configured service does not own this Ternilo process; check its exact service name before updating",
        ));
    }
    // A transient service disappears when stopped and cannot be started again by name.
    if systemd && output_text.lines().any(|line| line == "Transient=yes") {
        return Err(HarnessError::policy(
            "remote updates require a persistent systemd service; create a service unit file instead of launching the client with systemd-run",
        ));
    }
    Ok(())
}

async fn stop_service(plan: &ApplyPlan) -> Result<(), HarnessError> {
    match &plan.settings.restart {
        RestartMethod::Direct => {
            if crate::service::discover(&plan.data_dir).await?.is_some() {
                crate::service::stop(&plan.data_dir).await?;
            }
        }
        manager => manager_command(manager, "stop").await?,
    }
    for _ in 0..120 {
        if crate::service::discover(&plan.data_dir).await?.is_none() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    Err(HarnessError::unavailable(
        "client service has not stopped; no installation files were changed",
    ))
}
async fn start_service(plan: &ApplyPlan) -> Result<(), HarnessError> {
    match &plan.settings.restart {
        RestartMethod::Direct => {
            let settings = load_settings(&plan.data_dir)?.unwrap_or_else(|| plan.settings.clone());
            let mut command =
                std::process::Command::new(settings.installation.bin().join(executable()));
            command
                .args(&plan.arguments)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            detach(&mut command);
            command.spawn().map_err(io_error)?;
            Ok(())
        }
        manager => manager_command(manager, "start").await,
    }
}
pub(super) async fn start_saved_service(
    root: &Path,
    settings: &UpdateSettings,
) -> Result<(), HarnessError> {
    match &settings.restart {
        RestartMethod::Direct => {
            let mut command =
                std::process::Command::new(settings.installation.bin().join(executable()));
            command
                .args(["serve", "--data-dir"])
                .arg(root)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            detach(&mut command);
            command.spawn().map_err(io_error)?;
            Ok(())
        }
        manager => manager_command(manager, "start").await,
    }
}

async fn manager_command(manager: &RestartMethod, action: &str) -> Result<(), HarnessError> {
    let mut command = match manager {
        RestartMethod::Systemd { unit, user } => {
            let mut command = Command::new("systemctl");
            if *user {
                command.arg("--user");
            }
            command.arg(action).arg(unit);
            command
        }
        RestartMethod::Supervisor { program, config } => {
            let mut command = Command::new("supervisorctl");
            if let Some(config) = config {
                command.arg("-c").arg(config);
            }
            command.arg(action).arg(program);
            command
        }
        RestartMethod::Direct => return Ok(()),
    };
    command.stdin(Stdio::null()).kill_on_drop(true);
    let result = tokio::time::timeout(Duration::from_secs(60), command.output())
        .await
        .map_err(|_| {
            HarnessError::unavailable("service manager did not finish the update action in time")
        })?
        .map_err(io_error)?;
    if !result.status.success() {
        return Err(HarnessError::execution(
            "service manager rejected the update action; check the configured service name and this user's permissions",
        ));
    }
    Ok(())
}
fn progress(
    plan: &ApplyPlan,
    phase: ClientUpdatePhase,
    error: Option<String>,
) -> Result<(), HarnessError> {
    let mut progress = read_progress(&plan.data_dir)?
        .ok_or_else(|| HarnessError::conflict("client update operation disappeared"))?;
    if progress.id != plan.request.id {
        return Err(HarnessError::conflict(
            "client update operation was replaced",
        ));
    }
    progress.phase = phase;
    progress.error = error;
    progress.revision += 1;
    write_private(&plan.data_dir.join("data/updates/progress.json"), &progress)
}
pub(super) async fn run_helper(
    plan: &ApplyPlan,
    path: &Path,
    cache: &Path,
) -> Result<(), HarnessError> {
    // Run a copy outside the installation so Windows can replace the original executable.
    let helper = cache.join(if cfg!(windows) {
        "ternilo-update-helper.exe"
    } else {
        "ternilo-update-helper"
    });
    tokio::fs::copy(std::env::current_exe().map_err(io_error)?, &helper)
        .await
        .map_err(io_error)?;
    let mut command = if let RestartMethod::Systemd { user, .. } = &plan.settings.restart {
        let mut command = std::process::Command::new("systemd-run");
        if *user {
            command.arg("--user");
        }
        command
            .arg("--quiet")
            .arg("--collect")
            .arg("--scope")
            .arg(format!("--unit=ternilo-update-{}", plan.request.id))
            .arg("--")
            .arg(&helper);
        command
    } else {
        std::process::Command::new(&helper)
    };
    // A scope retains the caller's credentials, environment and filesystem namespace.
    detach(&mut command);
    command
        .arg("update-apply")
        .arg("--plan")
        .arg(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let mut child = tokio::process::Command::from(command)
        .spawn()
        .map_err(io_error)?;
    let status = child.wait().await.map_err(io_error)?;
    if !status.success() {
        return Err(HarnessError::execution(
            read_progress(&plan.data_dir)?
                .and_then(|progress| progress.error)
                .unwrap_or_else(|| "client update helper failed before shutdown".into()),
        ));
    }
    Ok(())
}

pub(super) fn detach(command: &mut std::process::Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        command.creation_flags(0x0000_0008 | 0x0800_0000);
    }
}
fn executable() -> &'static str {
    if cfg!(windows) {
        "ternilo.exe"
    } else {
        "ternilo"
    }
}
