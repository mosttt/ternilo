#![cfg(target_os = "linux")]

use std::{io, os::fd::OwnedFd, os::unix::fs::MetadataExt as _, time::Duration};

use command_fds::{CommandFdExt as _, FdMapping};
use rustix::{
    event::{PollFd, PollFlags, Timespec, poll},
    process::{Pid, PidfdFlags, Signal, pidfd_open, pidfd_send_signal},
};
use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::UnixStream,
    process::{Child, Command},
};

const INFO_FD: i32 = 3;
const START_FD: i32 = 4;

/// Bubblewrap reports its namespace init before the workload launch barrier.
/// Hold that barrier until a pidfd has pinned the exact init process.
pub(crate) struct SandboxLifetime {
    info: Option<UnixStream>,
    start: Option<UnixStream>,
    process: Option<NamespaceProcess>,
}

#[cfg(test)]
#[path = "sandbox_lifetime_tests.rs"]
mod tests;

struct NamespaceProcess {
    pidfd: OwnedFd,
    identity: NamespaceIdentity,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NamespaceIdentity {
    pub supervisor: SupervisorIdentity,
    pub init_pid: i32,
    pub start_time: u64,
    pub pid_namespace: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SupervisorIdentity {
    pub boot_id: String,
    pub observer_namespace: u64,
    pub pid: i32,
    pub start_time: u64,
}

impl SupervisorIdentity {
    pub(crate) fn current() -> io::Result<Self> {
        let pid = i32::try_from(std::process::id()).map_err(io::Error::other)?;
        Ok(Self {
            boot_id: std::fs::read_to_string("/proc/sys/kernel/random/boot_id")?
                .trim()
                .to_owned(),
            observer_namespace: std::fs::metadata("/proc/self/ns/pid")?.ino(),
            pid,
            start_time: process_start_time(pid)?,
        })
    }
}

#[derive(Deserialize)]
struct NamespaceInfo {
    #[serde(rename = "child-pid")]
    pid: i32,
    #[serde(rename = "pid-namespace")]
    namespace: u64,
}

impl SandboxLifetime {
    pub(crate) fn add_options(command: &mut Command) {
        command
            .arg("--info-fd")
            .arg(INFO_FD.to_string())
            .arg("--block-fd")
            .arg(START_FD.to_string());
    }

    pub(crate) fn prepare(command: &mut Command) -> io::Result<Self> {
        let (info, info_writer) = std::os::unix::net::UnixStream::pair()?;
        let (start, start_reader) = std::os::unix::net::UnixStream::pair()?;
        info.set_nonblocking(true)?;
        start.set_nonblocking(true)?;
        command
            .fd_mappings(vec![
                FdMapping {
                    parent_fd: info_writer.into(),
                    child_fd: INFO_FD,
                },
                FdMapping {
                    parent_fd: start_reader.into(),
                    child_fd: START_FD,
                },
            ])
            .map_err(io::Error::other)?;
        Ok(Self {
            info: Some(UnixStream::from_std(info)?),
            start: Some(UnixStream::from_std(start)?),
            process: None,
        })
    }

    pub(crate) async fn establish(
        &mut self,
        monitor: &mut Child,
        record: impl FnOnce(&NamespaceIdentity) -> io::Result<()>,
    ) -> io::Result<()> {
        let info = self
            .info
            .take()
            .ok_or_else(|| io::Error::other("sandbox launch was already inspected"))?;
        let mut bytes = Vec::new();
        tokio::time::timeout(
            Duration::from_secs(5),
            info.take(4096).read_to_end(&mut bytes),
        )
        .await
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                "sandbox did not report its namespace init",
            )
        })??;
        let info: NamespaceInfo = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
        if monitor.try_wait()?.is_some() {
            return Err(io::Error::other(
                "sandbox monitor exited before startup confirmation",
            ));
        }
        let pid = Pid::from_raw(info.pid)
            .ok_or_else(|| io::Error::other("sandbox reported an invalid init PID"))?;
        let pidfd = pidfd_open(pid, PidfdFlags::empty())?;
        let namespace = std::fs::metadata(format!("/proc/{}/ns/pid", info.pid))?.ino();
        let host_namespace = std::fs::metadata("/proc/self/ns/pid")?.ino();
        let status = std::fs::read_to_string(format!("/proc/{}/status", info.pid))?;
        let is_init = status
            .lines()
            .find(|line| line.starts_with("NSpid:"))
            .and_then(|line| line.split_whitespace().last())
            == Some("1");
        let parent = status
            .lines()
            .find(|line| line.starts_with("PPid:"))
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|value| value.parse::<u32>().ok());
        if namespace != info.namespace
            || namespace == host_namespace
            || !is_init
            || parent != monitor.id()
            || monitor.try_wait()?.is_some()
        {
            return Err(io::Error::other(
                "sandbox namespace init identity changed before startup",
            ));
        }
        let identity = NamespaceIdentity {
            supervisor: SupervisorIdentity::current()?,
            init_pid: info.pid,
            start_time: process_start_time(info.pid)?,
            pid_namespace: namespace,
        };
        self.process = Some(NamespaceProcess { pidfd, identity });
        record(
            &self
                .process
                .as_ref()
                .expect("namespace process was just pinned")
                .identity,
        )?;
        // This monitor spawns exactly one namespace init. Checking its parent rejects
        // PID reuse before capture; the retained pidfd prevents reuse afterward.
        self.start
            .take()
            .ok_or_else(|| io::Error::other("sandbox start barrier is missing"))?
            .write_all(&[1])
            .await
    }

    pub(crate) fn terminate(&self) -> io::Result<()> {
        if let Some(process) = &self.process {
            match pidfd_send_signal(&process.pidfd, Signal::KILL) {
                Ok(()) | Err(rustix::io::Errno::SRCH) => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }

    pub(crate) fn exited(&self) -> io::Result<bool> {
        let process = self.process.as_ref().ok_or_else(|| {
            io::Error::other("sandbox init exit cannot be confirmed before its identity is pinned")
        })?;
        let mut descriptors = [PollFd::new(&process.pidfd, PollFlags::IN)];
        let timeout = Timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        match poll(&mut descriptors, Some(&timeout)) {
            Ok(_) => Ok(descriptors[0]
                .revents()
                .intersects(PollFlags::IN | PollFlags::HUP)),
            Err(rustix::io::Errno::INTR) => Ok(false),
            Err(error) => Err(error.into()),
        }
    }
}

pub(crate) fn process_start_time(pid: i32) -> io::Result<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
    stat.rsplit_once(')')
        .and_then(|(_, fields)| fields.split_whitespace().nth(19))
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| io::Error::other("process start time is unavailable"))
}
