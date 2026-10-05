//! Lifecycle ownership and completion observation for ordinary process groups.
use std::sync::atomic::{AtomicU32, Ordering};
use ternilo_protocol::HarnessError;
use tokio::process::Command;

#[cfg(windows)]
mod windows;
#[cfg(not(windows))]
pub use tokio::process::Child;
#[cfg(windows)]
pub use windows::{Child, spawn};

#[cfg(not(windows))]
pub fn spawn(command: &mut Command) -> std::io::Result<(Child, ManagedProcessGroup)> {
    configure(command);
    let child = command.spawn()?;
    let process_id = child
        .id()
        .ok_or_else(|| std::io::Error::other("started process has no ID"))?;
    Ok((
        child,
        ManagedProcessGroup {
            id: AtomicU32::new(process_id),
        },
    ))
}

pub fn configure(command: &mut Command) {
    #[cfg(unix)]
    command.process_group(0);
    #[cfg(not(unix))]
    let _ = command;
}

pub struct ManagedProcessGroup {
    id: AtomicU32,
    #[cfg(windows)]
    observer: std::sync::Arc<win32job::Job>,
}
impl ManagedProcessGroup {
    pub fn terminate(&self) {
        // Windows supervisors terminate the child's Job Object through start_kill.
        #[cfg(not(unix))]
        let _ = self;
        #[cfg(unix)]
        if let Ok(id) = i32::try_from(self.id.load(Ordering::Acquire))
            && id > 0
        {
            let _ = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(-id),
                nix::sys::signal::Signal::SIGKILL,
            );
        }
    }
    pub async fn wait_quiescent(&self) -> Result<(), HarnessError> {
        let id = self.id.load(Ordering::Acquire);
        if id == 0 {
            return Ok(());
        }
        #[cfg(any(unix, windows))]
        {
            for _ in 0..300 {
                #[cfg(unix)]
                let has_processes = group_has_live_processes(id).await?;
                #[cfg(windows)]
                let has_processes = !self
                    .observer
                    .query_process_id_list()
                    .map_err(|error| {
                        HarnessError::execution(format!("observe Windows process job: {error}"))
                    })?
                    .is_empty();
                if !has_processes {
                    self.id.store(0, Ordering::Release);
                    return Ok(());
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            Err(HarnessError::unavailable(
                "process group has not finished cleanup",
            ))
        }
        #[cfg(not(any(unix, windows)))]
        {
            Err(HarnessError::unavailable(
                "this platform has no process-tree completion observer",
            ))
        }
    }
}
impl Drop for ManagedProcessGroup {
    fn drop(&mut self) {
        self.terminate();
    }
}

#[cfg(target_os = "linux")]
async fn group_has_live_processes(id: u32) -> Result<bool, HarnessError> {
    // Zombies cannot execute or retain a workspace file handle; their parent owns reaping.
    tokio::task::spawn_blocking(move || {
        for entry in std::fs::read_dir("/proc")
            .map_err(|error| HarnessError::execution(format!("observe process tree: {error}")))?
        {
            let entry = entry.map_err(|error| {
                HarnessError::execution(format!("observe process tree: {error}"))
            })?;
            if entry.file_name().to_string_lossy().parse::<u32>().is_err() {
                continue;
            }
            let stat = match std::fs::read_to_string(entry.path().join("stat")) {
                Ok(stat) => stat,
                Err(error)
                    if error.kind() == std::io::ErrorKind::NotFound
                        || error.raw_os_error() == Some(nix::libc::ESRCH) =>
                {
                    continue;
                }
                Err(error) => {
                    return Err(HarnessError::execution(format!(
                        "observe process state: {error}"
                    )));
                }
            };
            if let Some((_, fields)) = stat.rsplit_once(") ") {
                let mut fields = fields.split_whitespace();
                let state = fields.next();
                let _parent = fields.next();
                let group = fields.next().and_then(|value| value.parse::<u32>().ok());
                if group == Some(id) && !matches!(state, Some("Z" | "X")) {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    })
    .await
    .map_err(|error| HarnessError::execution(format!("observe process tree: {error}")))?
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn short_lived_process_groups_finish_cleanup_under_process_churn() {
        let mut runs = tokio::task::JoinSet::new();
        for _ in 0..4 {
            runs.spawn(async {
                for _ in 0..8 {
                    let (mut child, group) =
                        spawn(Command::new("sh").args(["-c", "exit 0"])).unwrap();
                    assert!(child.wait().await.unwrap().success());
                    group.wait_quiescent().await.unwrap();
                }
            });
        }
        while let Some(result) = runs.join_next().await {
            result.unwrap();
        }
    }
}

#[cfg(all(unix, not(target_os = "linux")))]
async fn group_has_live_processes(id: u32) -> Result<bool, HarnessError> {
    let output = Command::new("ps")
        .args(["-axo", "pgid=,stat="])
        .output()
        .await
        .map_err(|error| HarnessError::execution(format!("observe process groups: {error}")))?;
    if !output.status.success() {
        return Err(HarnessError::unavailable(
            "process group observation failed",
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).lines().any(|line| {
        let mut fields = line.split_whitespace();
        fields.next().and_then(|value| value.parse::<u32>().ok()) == Some(id)
            && fields.next().is_some_and(|state| !state.starts_with('Z'))
    }))
}
