use super::{
    cli::LaunchdDomain,
    config::{RestartMethod, io_error},
};
use std::{
    path::{Path, PathBuf},
    process::{Output, Stdio},
    time::Duration,
};
use ternilo_protocol::HarnessError;
use tokio::process::Command;

pub(super) async fn configure(
    label: String,
    plist: Option<PathBuf>,
    domain: Option<LaunchdDomain>,
) -> Result<RestartMethod, HarnessError> {
    if !cfg!(target_os = "macos") {
        return Err(HarnessError::invalid(
            "launch-agent restart is only available on macOS",
        ));
    }
    let uid = checked(run("/usr/bin/id", &["-u".as_ref()]).await?)?;
    let uid = String::from_utf8_lossy(&uid.stdout);
    if !uid.trim().bytes().all(|byte| byte.is_ascii_digit()) || uid.trim().is_empty() {
        return Err(HarnessError::unavailable(
            "cannot determine the LaunchAgent user",
        ));
    }
    let kind = match domain.unwrap_or(LaunchdDomain::Gui) {
        LaunchdDomain::Gui => "gui",
        LaunchdDomain::User => "user",
    };
    let domain = format!("{kind}/{}", uid.trim());
    let plist = match plist {
        Some(plist) => plist,
        None => PathBuf::from(
            std::env::var_os("HOME")
                .ok_or_else(|| HarnessError::invalid("set HOME or provide --launch-agent-plist"))?,
        )
        .join("Library/LaunchAgents")
        .join(format!("{label}.plist")),
    };
    let restart = RestartMethod::LaunchAgent {
        label,
        domain,
        plist: std::fs::canonicalize(plist).map_err(io_error)?,
    };
    restart.validate()?;
    if let RestartMethod::LaunchAgent { label, plist, .. } = &restart {
        validate_plist(label, plist).await?;
    }
    Ok(restart)
}

async fn run(program: &str, args: &[&std::ffi::OsStr]) -> Result<Output, HarnessError> {
    tokio::time::timeout(
        Duration::from_secs(60),
        Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| {
        HarnessError::unavailable(
            "LaunchAgent command timed out; check launchctl and this user's login domain",
        )
    })?
    .map_err(io_error)
}

fn checked(output: Output) -> Result<Output, HarnessError> {
    if !output.status.success() {
        return Err(HarnessError::execution(format!(
            "LaunchAgent command failed: {}; check the Label, plist path and login domain",
            String::from_utf8_lossy(&output.stderr).trim(),
        )));
    }
    Ok(output)
}

async fn validate_plist(label: &str, plist: &Path) -> Result<(), HarnessError> {
    let output = checked(
        run(
            "/usr/bin/plutil",
            &[
                "-extract".as_ref(),
                "Label".as_ref(),
                "raw".as_ref(),
                "-o".as_ref(),
                "-".as_ref(),
                plist.as_os_str(),
            ],
        )
        .await?,
    )?;
    if String::from_utf8_lossy(&output.stdout).trim() != label {
        return Err(HarnessError::policy(
            "configured service does not own this Ternilo process; --service must match the LaunchAgent plist Label",
        ));
    }
    Ok(())
}

async fn inspect(target: &str) -> Result<Option<Output>, HarnessError> {
    let output = run("/bin/launchctl", &["print".as_ref(), target.as_ref()]).await?;
    // launchctl returns 113 when this exact service is not registered in the domain.
    if output.status.code() == Some(113) {
        return Ok(None);
    }
    checked(output).map(Some)
}

pub(super) async fn verify(
    label: &str,
    domain: &str,
    plist: &Path,
    expected: u32,
) -> Result<(), HarnessError> {
    validate_plist(label, plist).await?;
    let output = inspect(&format!("{domain}/{label}")).await?;
    if output
        .as_ref()
        .and_then(|output| service_pid(&String::from_utf8_lossy(&output.stdout)))
        != Some(expected)
    {
        return Err(HarnessError::policy(
            "configured service does not own this Ternilo process; check the LaunchAgent Label and --launchd-domain before updating",
        ));
    }
    Ok(())
}

// Only the service's top-level PID counts; resource coalitions can contain other PIDs.
fn service_pid(output: &str) -> Option<u32> {
    let mut depth = 0_usize;
    for line in output.lines() {
        let line = line.trim();
        if depth == 1
            && let Some(value) = line.strip_prefix("pid = ")
        {
            return value.parse().ok();
        }
        if line.ends_with('{') {
            depth += 1;
        }
        if line == "}" {
            depth = depth.saturating_sub(1);
        }
    }
    None
}

pub(super) async fn action(
    label: &str,
    domain: &str,
    plist: &Path,
    action: &str,
) -> Result<(), HarnessError> {
    let target = format!("{domain}/{label}");
    let loaded = inspect(&target).await?.is_some();
    match action {
        "stop" if loaded => {
            let output = run("/bin/launchctl", &["bootout".as_ref(), target.as_ref()]).await?;
            // launchd can retain a retiring job briefly after its process exits.
            // ESRCH means it is already stopping; other failures still need attention.
            if output.status.code() != Some(3) {
                checked(output)?;
            }
            tokio::time::timeout(Duration::from_secs(30), async {
                while inspect(&target).await?.is_some() {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                Ok::<(), HarnessError>(())
            })
            .await
            .map_err(|_| {
                HarnessError::unavailable(
                    "LaunchAgent has not finished unloading; check its status before retrying",
                )
            })??;
        }
        "stop" => {}
        "start" => {
            validate_plist(label, plist).await?;
            if !loaded {
                checked(
                    run(
                        "/bin/launchctl",
                        &["bootstrap".as_ref(), domain.as_ref(), plist.as_os_str()],
                    )
                    .await?,
                )?;
            }
            checked(run("/bin/launchctl", &["kickstart".as_ref(), target.as_ref()]).await?)?;
        }
        _ => {
            return Err(HarnessError::invalid(
                "unsupported LaunchAgent update action",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_agent_pid_ignores_nested_processes_and_missing_pid() {
        assert_eq!(
            service_pid(
                "gui/501/com.ternilo.client = {\n\tresource coalition = {\n\t\tpid = 999\n\t}\n\tpid = 42\n}"
            ),
            Some(42)
        );
        assert_eq!(
            service_pid(
                "user/501/com.ternilo.client = {\n\tresource coalition = {\n\t\tpid = 999\n\t}\n}"
            ),
            None
        );
    }

    #[test]
    fn launch_agent_settings_cannot_target_an_entire_domain() {
        for (label, domain, path) in [
            ("", "gui/501", "/tmp/a.plist"),
            ("other/service", "gui/501", "/tmp/a.plist"),
            ("client", "system", "/tmp/a.plist"),
            ("client", "gui/501/other", "/tmp/a.plist"),
            ("client", "user/501", "relative.plist"),
        ] {
            assert!(
                RestartMethod::LaunchAgent {
                    label: label.into(),
                    domain: domain.into(),
                    plist: path.into()
                }
                .validate()
                .is_err()
            );
        }
        RestartMethod::LaunchAgent {
            label: "com.ternilo.client".into(),
            domain: "user/501".into(),
            plist: std::env::temp_dir().join("agent.plist"),
        }
        .validate()
        .unwrap();
    }
}
