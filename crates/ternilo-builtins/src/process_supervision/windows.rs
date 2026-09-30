use process_wrap::tokio::{ChildWrapper, CommandWrap, CommandWrapper, JobObject, KillOnDrop};
use std::{
    io,
    process::ExitStatus,
    sync::{Arc, atomic::AtomicU32},
};
use tokio::process::{ChildStderr, ChildStdin, ChildStdout, Command};

use super::ManagedProcessGroup;

// Observe a parent job independently of process-wrap's completion-port wait.
// Assignment runs in post_spawn while JobObject still has the child suspended.
#[derive(Debug)]
struct ObserveJob(Arc<win32job::Job>);

impl CommandWrapper for ObserveJob {
    fn pre_spawn(&mut self, command: &mut Command, _: &CommandWrap) -> io::Result<()> {
        const CREATE_SUSPENDED: u32 = 0x0000_0004;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_SUSPENDED | CREATE_NO_WINDOW);
        Ok(())
    }

    fn post_spawn(
        &mut self,
        _: &mut Command,
        child: &mut tokio::process::Child,
        _: &CommandWrap,
    ) -> io::Result<()> {
        let handle = child
            .raw_handle()
            .ok_or_else(|| io::Error::other("suspended child has no process handle"))?;
        self.0
            .assign_process(handle as isize)
            .map_err(io::Error::other)
    }
}

pub struct Child {
    inner: Box<dyn ChildWrapper>,
    pub stdin: Option<ChildStdin>,
    pub stdout: Option<ChildStdout>,
    pub stderr: Option<ChildStderr>,
}

impl Child {
    pub fn id(&self) -> Option<u32> {
        self.inner.id()
    }

    pub fn start_kill(&mut self) -> io::Result<()> {
        self.inner.start_kill()
    }

    pub async fn wait(&mut self) -> io::Result<ExitStatus> {
        // The only child wrapper is JobObject; wait for its inner leader, then
        // stop all descendants. ManagedProcessGroup separately proves emptiness.
        let status = self.inner.inner_mut().wait().await;
        let _ = self.inner.start_kill();
        status
    }
}

impl Drop for Child {
    fn drop(&mut self) {
        // Terminate the job explicitly before Tokio drops the leader. Closing
        // the wrapper alone must not leave its descendants running.
        let _ = self.inner.start_kill();
    }
}

pub fn spawn(command: &mut Command) -> io::Result<(Child, ManagedProcessGroup)> {
    let observer = Arc::new(win32job::Job::create().map_err(io::Error::other)?);
    let mut wrapped = CommandWrap::from(std::mem::replace(command, Command::new("")));
    wrapped
        .wrap(JobObject)
        .wrap(ObserveJob(Arc::clone(&observer)))
        .wrap(KillOnDrop);
    let mut inner = wrapped.spawn()?;
    let id = inner
        .id()
        .ok_or_else(|| io::Error::other("started process has no ID"))?;
    let child = Child {
        stdin: inner.stdin().take(),
        stdout: inner.stdout().take(),
        stderr: inner.stderr().take(),
        inner,
    };
    Ok((
        child,
        ManagedProcessGroup {
            id: AtomicU32::new(id),
            observer,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{process::Stdio, time::Duration};

    #[tokio::test]
    async fn ordinary_exit_is_observed_without_retaining_a_live_job() {
        let (mut child, group) =
            spawn(Command::new("cmd.exe").args(["/D", "/C", "exit /b 0"])).unwrap();
        assert!(child.wait().await.unwrap().success());
        group.wait_quiescent().await.unwrap();
    }

    #[tokio::test]
    async fn job_kill_and_drop_both_wait_for_the_command_and_its_descendant() {
        for attempt in 0..8 {
            let kill = attempt % 2 == 0;
            let (mut child, group) = spawn(
                Command::new("cmd.exe")
                    .args(["/D", "/C", "ping -n 120 127.0.0.1 >NUL"])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null()),
            )
            .unwrap();
            tokio::time::timeout(Duration::from_secs(5), async {
                while group.observer.query_process_id_list().unwrap().len() < 2 {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            if kill {
                child.start_kill().unwrap();
                child.wait().await.unwrap();
            }
            drop(child);
            group.wait_quiescent().await.unwrap_or_else(|error| {
                panic!("job cleanup failed on attempt {attempt} (explicit kill: {kill}): {error}")
            });
            assert!(group.observer.query_process_id_list().unwrap().is_empty());
        }
    }
}
