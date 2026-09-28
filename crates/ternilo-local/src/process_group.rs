//! Lifecycle ownership for ordinary Unix process groups created by Ternilo.

use tokio::process::Command;

#[cfg(unix)]
use std::sync::atomic::{AtomicI32, Ordering};

pub(crate) fn configure(command: &mut Command) {
    #[cfg(unix)]
    command.process_group(0);
    #[cfg(not(unix))]
    let _ = command;
}

pub(crate) struct ManagedProcessGroup {
    #[cfg(unix)]
    id: AtomicI32,
}

impl ManagedProcessGroup {
    pub(crate) fn new(process_id: u32) -> Self {
        #[cfg(not(unix))]
        let _ = process_id;
        Self {
            #[cfg(unix)]
            id: AtomicI32::new(i32::try_from(process_id).expect("Unix process ID fits i32")),
        }
    }

    pub(crate) fn terminate(&self) {
        #[cfg(unix)]
        {
            let id = self.id.swap(0, Ordering::AcqRel);
            if id != 0 {
                // The leader may already have exited while descendants retain the group.
                let _ = nix::sys::signal::kill(
                    nix::unistd::Pid::from_raw(-id),
                    nix::sys::signal::Signal::SIGKILL,
                );
            }
        }
    }
}

impl Drop for ManagedProcessGroup {
    fn drop(&mut self) {
        self.terminate();
    }
}
