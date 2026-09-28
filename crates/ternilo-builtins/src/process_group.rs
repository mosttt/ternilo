//! Lifetime ownership for ordinary process groups launched by built-in plugins.

use std::{
    io,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU32, Ordering},
    },
};

use ternilo_kernel::WorkspaceExecutionLease;
use tokio::{
    process::{Child, Command},
    sync::watch,
};

pub(crate) struct ManagedProcess {
    control: ProcessControl,
}

/// Observation and cancellation do not transfer ownership away from the supervisor.
#[derive(Clone)]
pub(crate) struct ProcessControl {
    group: Arc<OwnedProcessGroup>,
    stop: watch::Sender<bool>,
    finished: watch::Receiver<Option<Result<(), String>>>,
    status: Arc<Mutex<Option<String>>>,
}

impl ManagedProcess {
    pub(crate) fn new(
        mut child: Child,
        group: OwnedProcessGroup,
        lease: Option<WorkspaceExecutionLease>,
    ) -> Self {
        let group = Arc::new(group);
        let owned = Arc::clone(&group);
        let status = Arc::new(Mutex::new(None));
        let observed = Arc::clone(&status);
        let (stop, mut stopping) = watch::channel(false);
        let (completion, finished) = watch::channel(None);
        tokio::spawn(async move {
            // Dropping a transport must not release directory ownership before cleanup.
            let result = tokio::select! {
                result = child.wait() => result,
                _ = stopping.changed() => {
                    owned.kill();
                    let _ = child.start_kill();
                    child.wait().await
                }
            };
            if let Ok(mut status) = observed.lock() {
                *status = Some(match &result {
                    Ok(status) => status.to_string(),
                    Err(error) => error.to_string(),
                });
            }
            owned.kill();
            drop(lease);
            completion.send_replace(Some(result.map(|_| ()).map_err(|error| error.to_string())));
        });
        Self {
            control: ProcessControl {
                group,
                stop,
                finished,
                status,
            },
        }
    }

    pub(crate) fn control(&self) -> ProcessControl {
        self.control.clone()
    }

    pub(crate) fn status(&self) -> Option<String> {
        self.control.status()
    }

    pub(crate) async fn wait(&mut self) -> io::Result<()> {
        self.control.wait().await
    }

    pub(crate) async fn stop(&mut self) -> io::Result<()> {
        self.control.stop().await
    }
}

impl Drop for ManagedProcess {
    fn drop(&mut self) {
        self.control.request_stop();
    }
}

impl ProcessControl {
    pub(crate) fn request_stop(&self) {
        self.group.kill();
        self.stop.send_replace(true);
    }

    pub(crate) fn status(&self) -> Option<String> {
        self.status.lock().ok().and_then(|status| status.clone())
    }

    pub(crate) fn is_finished(&self) -> bool {
        self.finished.borrow().is_some()
    }

    async fn wait(&self) -> io::Result<()> {
        let mut finished = self.finished.clone();
        loop {
            if let Some(result) = finished.borrow().clone() {
                return result.map_err(io::Error::other);
            }
            finished.changed().await.map_err(|_| {
                io::Error::other("process supervisor ended before publishing cleanup completion")
            })?;
        }
    }

    pub(crate) async fn stop(&self) -> io::Result<()> {
        self.request_stop();
        self.wait().await
    }
}

pub(crate) fn configure(command: &mut Command) {
    #[cfg(unix)]
    command.process_group(0);
    #[cfg(not(unix))]
    let _ = command;
}

pub(crate) struct OwnedProcessGroup {
    pid: AtomicU32,
}

impl OwnedProcessGroup {
    pub(crate) fn new(pid: Option<u32>) -> Self {
        Self {
            pid: AtomicU32::new(pid.unwrap_or(0)),
        }
    }

    pub(crate) fn terminate(&self) {
        #[cfg(unix)]
        signal(
            self.pid.load(Ordering::Acquire),
            nix::sys::signal::Signal::SIGTERM,
        );
    }

    pub(crate) fn kill(&self) {
        let pid = self.pid.swap(0, Ordering::AcqRel);
        #[cfg(unix)]
        signal(pid, nix::sys::signal::Signal::SIGKILL);
        #[cfg(not(unix))]
        let _ = pid;
    }

    #[cfg(unix)]
    pub(crate) fn exists(&self) -> bool {
        i32::try_from(self.pid.load(Ordering::Acquire))
            .ok()
            .filter(|pid| *pid > 0)
            .is_some_and(|pid| {
                matches!(
                    nix::sys::signal::kill(nix::unistd::Pid::from_raw(-pid), None),
                    Ok(()) | Err(nix::errno::Errno::EPERM)
                )
            })
    }
}

impl Drop for OwnedProcessGroup {
    fn drop(&mut self) {
        self.kill();
    }
}

#[cfg(unix)]
fn signal(pid: u32, signal: nix::sys::signal::Signal) {
    if let Ok(pid) = i32::try_from(pid)
        && pid > 0
    {
        let _ = nix::sys::signal::kill(nix::unistd::Pid::from_raw(-pid), signal);
    }
}
