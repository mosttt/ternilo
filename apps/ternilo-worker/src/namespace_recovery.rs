//! Recovery never signals the original daemon or a process whose identity has changed.

#![cfg(target_os = "linux")]

use std::{io, os::fd::OwnedFd, os::unix::fs::MetadataExt as _};

use rustix::{
    event::{PollFd, PollFlags, Timespec, poll},
    process::{Pid, PidfdFlags, Signal, pidfd_open, pidfd_send_signal},
};

use crate::sandbox_lifetime::{NamespaceIdentity, SupervisorIdentity, process_start_time};

enum ObservedProcess {
    Exited,
    Running(OwnedFd),
    Unverifiable,
}

pub(crate) fn supervisor_exited(identity: &SupervisorIdentity) -> io::Result<bool> {
    let current = SupervisorIdentity::current()?;
    if current.boot_id != identity.boot_id
        || current.observer_namespace != identity.observer_namespace
    {
        return Ok(false);
    }
    Ok(matches!(
        observe(identity.pid, identity.start_time, None)?,
        ObservedProcess::Exited
    ))
}

/// Only call after the recorded supervisor has exited, including its host-side threads.
pub(crate) fn stop_or_confirm_namespace(identity: &NamespaceIdentity) -> io::Result<bool> {
    if !supervisor_exited(&identity.supervisor)? {
        return Ok(false);
    }
    match observe(
        identity.init_pid,
        identity.start_time,
        Some(identity.pid_namespace),
    )? {
        ObservedProcess::Exited => Ok(true),
        ObservedProcess::Unverifiable => Ok(false),
        ObservedProcess::Running(pidfd) => {
            match pidfd_send_signal(&pidfd, Signal::KILL) {
                Ok(()) | Err(rustix::io::Errno::SRCH) => {}
                Err(error) => return Err(error.into()),
            }
            exited(&pidfd)
        }
    }
}

fn observe(pid: i32, start_time: u64, namespace: Option<u64>) -> io::Result<ObservedProcess> {
    let pid =
        Pid::from_raw(pid).ok_or_else(|| io::Error::other("recorded process PID is invalid"))?;
    let pidfd = match pidfd_open(pid, PidfdFlags::empty()) {
        Ok(pidfd) => pidfd,
        Err(rustix::io::Errno::SRCH) => return Ok(ObservedProcess::Exited),
        Err(error) => return Err(error.into()),
    };
    match process_start_time(pid.as_raw_pid()) {
        Ok(current) if current != start_time => return Ok(ObservedProcess::Exited),
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(if exited(&pidfd)? {
                ObservedProcess::Exited
            } else {
                ObservedProcess::Unverifiable
            });
        }
        Err(error) => return Err(error),
    }
    if exited(&pidfd)? {
        return Ok(ObservedProcess::Exited);
    }
    if let Some(expected) = namespace {
        match std::fs::metadata(format!("/proc/{}/ns/pid", pid.as_raw_pid())) {
            Ok(current) if current.ino() != expected => return Ok(ObservedProcess::Exited),
            Ok(_) => {}
            // Namespace links can vanish during exit before all namespace tasks have drained.
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(ObservedProcess::Unverifiable);
            }
            Err(error) => return Err(error),
        }
    }
    Ok(ObservedProcess::Running(pidfd))
}

fn exited(pidfd: &OwnedFd) -> io::Result<bool> {
    let mut descriptors = [PollFd::new(pidfd, PollFlags::IN)];
    match poll(
        &mut descriptors,
        Some(&Timespec {
            tv_sec: 0,
            tv_nsec: 0,
        }),
    ) {
        Ok(_) => Ok(descriptors[0]
            .revents()
            .intersects(PollFlags::IN | PollFlags::HUP)),
        Err(rustix::io::Errno::INTR) => Ok(false),
        Err(error) => Err(error.into()),
    }
}
