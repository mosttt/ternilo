use std::{
    io,
    ops::{Deref, DerefMut},
    process::ExitStatus,
};

use ternilo_protocol::HarnessError;
use tokio::process::{Child, Command};

#[cfg(target_os = "linux")]
use crate::sandbox_lifetime::{NamespaceIdentity, SandboxLifetime};
#[cfg(not(target_os = "linux"))]
type SandboxLifetime = ();
#[cfg(not(target_os = "linux"))]
type NamespaceIdentity = ();

pub(crate) fn enter_owned_session() -> Result<(), HarnessError> {
    #[cfg(target_os = "linux")]
    {
        nix::unistd::setsid().map_err(|error| {
            HarnessError::execution(format!("create cloud child process session: {error}"))
        })?;
        Ok(())
    }
    #[cfg(not(target_os = "linux"))]
    Err(HarnessError::execution(
        "owned cloud child process sessions require a Linux worker host",
    ))
}

pub(crate) struct ManagedChild {
    child: Child,
    #[cfg(target_os = "linux")]
    session_id: Option<i32>,
    #[cfg(target_os = "linux")]
    sandbox: Option<SandboxLifetime>,
}

impl ManagedChild {
    pub(crate) fn spawn(
        command: &mut Command,
        owned_session: bool,
        sandbox: Option<SandboxLifetime>,
    ) -> io::Result<Self> {
        #[cfg(target_os = "linux")]
        if owned_session {
            // Process-mode cleanup needs the host's process table, including separate shell groups.
            std::fs::read_dir("/proc")?;
        }
        let child = command.kill_on_drop(true).spawn()?;
        #[cfg(target_os = "linux")]
        let session_id = owned_session
            .then(|| child.id().and_then(|pid| i32::try_from(pid).ok()))
            .flatten();
        #[cfg(not(target_os = "linux"))]
        let _ = (owned_session, sandbox);
        Ok(Self {
            child,
            #[cfg(target_os = "linux")]
            session_id,
            #[cfg(target_os = "linux")]
            sandbox,
        })
    }

    pub(crate) fn start_kill(&mut self) -> io::Result<()> {
        let sandbox = self.terminate_sandbox();
        let leader = self.child.start_kill();
        self.terminate_owned_session()?;
        sandbox.and(leader)
    }

    pub(crate) async fn establish_sandbox(
        &mut self,
        record: impl FnOnce(&NamespaceIdentity) -> io::Result<()>,
    ) -> io::Result<()> {
        #[cfg(target_os = "linux")]
        if let Some(sandbox) = &mut self.sandbox {
            sandbox.establish(&mut self.child, record).await?;
        }
        #[cfg(not(target_os = "linux"))]
        let _ = record;
        Ok(())
    }

    fn terminate_sandbox(&self) -> io::Result<()> {
        #[cfg(target_os = "linux")]
        if let Some(sandbox) = &self.sandbox {
            sandbox.terminate()?;
        }
        Ok(())
    }

    pub(crate) async fn wait(&mut self) -> io::Result<ExitStatus> {
        let status = self.child.wait().await?;
        self.terminate_sandbox()?;
        self.terminate_owned_session()?;
        while !self.confirm_session_exit()? {
            // Keep the identity until observed exit, including when this wait is cancelled.
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            self.terminate_owned_session()?;
        }
        Ok(status)
    }

    pub(crate) fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        let status = self.child.try_wait()?;
        if status.is_some() {
            self.terminate_sandbox()?;
            self.terminate_owned_session()?;
            if !self.confirm_session_exit()? {
                return Ok(None);
            }
        }
        Ok(status)
    }

    fn terminate_owned_session(&self) -> io::Result<()> {
        #[cfg(target_os = "linux")]
        if let Some(session_id) = self.session_id {
            terminate_session(session_id)?;
        }
        Ok(())
    }

    fn confirm_session_exit(&mut self) -> io::Result<bool> {
        #[cfg(target_os = "linux")]
        if let Some(session_id) = self.session_id {
            if !session_groups(session_id)?.is_empty() {
                return Ok(false);
            }
            self.session_id = None;
        }
        #[cfg(target_os = "linux")]
        if let Some(sandbox) = &self.sandbox {
            if !sandbox.exited()? {
                return Ok(false);
            }
            self.sandbox = None;
        }
        Ok(true)
    }
}

impl Deref for ManagedChild {
    type Target = Child;

    fn deref(&self) -> &Self::Target {
        &self.child
    }
}

impl DerefMut for ManagedChild {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.child
    }
}

impl Drop for ManagedChild {
    fn drop(&mut self) {
        // Sending termination signals is not evidence that all physical writers have exited.
        let _ = self.start_kill();
    }
}

#[cfg(target_os = "linux")]
fn terminate_session(session_id: i32) -> io::Result<()> {
    use nix::sys::signal::Signal;

    // Shells and terminals create their own groups inside this session. Freeze newly found
    // groups until a scan stabilizes so ordinary background processes stop spawning during cleanup.
    // Process mode is cooperative: deliberately creating a new session still requires a sandbox.
    let mut groups = std::collections::BTreeSet::new();
    let mut failure = None;
    loop {
        let discovered = match session_groups(session_id) {
            Ok(discovered) => discovered,
            Err(error) => {
                failure.get_or_insert(error);
                break;
            }
        };
        let added = discovered.difference(&groups).copied().collect::<Vec<_>>();
        if added.is_empty() {
            break;
        }
        groups.extend(added.iter().copied());
        for group in added {
            if let Err(error) = signal_group(group, Signal::SIGSTOP) {
                failure.get_or_insert(error);
            }
        }
        if failure.is_some() {
            break;
        }
    }
    for group in groups {
        if let Err(error) = signal_group(group, Signal::SIGKILL) {
            failure.get_or_insert(error);
        }
    }
    failure.map_or(Ok(()), Err)
}

#[cfg(target_os = "linux")]
fn session_groups(session_id: i32) -> io::Result<std::collections::BTreeSet<i32>> {
    let mut groups = std::collections::BTreeSet::new();
    for process in std::fs::read_dir("/proc")? {
        let process = process?;
        if process
            .file_name()
            .to_string_lossy()
            .parse::<u32>()
            .is_err()
        {
            continue;
        }
        let stat = match std::fs::read_to_string(process.path().join("stat")) {
            Ok(stat) => stat,
            // Processes can exit between enumeration and inspection.
            Err(error)
                if error.kind() == io::ErrorKind::NotFound
                    || error.raw_os_error() == Some(nix::libc::ESRCH) =>
            {
                continue;
            }
            Err(error) => return Err(error),
        };
        let Some((_, fields)) = stat.rsplit_once(')') else {
            continue;
        };
        let mut fields = fields.split_whitespace();
        let state = fields.next();
        let group = fields.nth(1).and_then(|value| value.parse::<i32>().ok());
        let session = fields.next().and_then(|value| value.parse::<i32>().ok());
        if session == Some(session_id)
            // Zombies and dead tasks cannot write. Stopped or uninterruptible tasks still can.
            && !matches!(state, Some("Z" | "X" | "x"))
            && let Some(group) = group.filter(|group| *group > 0)
        {
            groups.insert(group);
        }
    }
    Ok(groups)
}

#[cfg(target_os = "linux")]
fn signal_group(group: i32, signal: nix::sys::signal::Signal) -> io::Result<()> {
    match nix::sys::signal::kill(nix::unistd::Pid::from_raw(-group), signal) {
        Ok(()) | Err(nix::errno::Errno::ESRCH) => Ok(()),
        Err(error) => Err(io::Error::from(error)),
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use std::{path::Path, process::Stdio, time::Duration};

    use tokio::io::AsyncReadExt;

    use super::*;

    #[test]
    #[ignore = "Subprocess entry point for owned-session lifecycle tests."]
    #[expect(
        clippy::zombie_processes,
        reason = "The parent Worker guard must clean up a descendant that outlives its leader."
    )]
    fn owned_session_child_fixture() {
        use std::os::unix::process::CommandExt as _;

        let Ok(directory) = std::env::var("TERNILO_TEST_OWNED_SESSION") else {
            return;
        };
        enter_owned_session().unwrap();
        if std::env::var_os("TERNILO_TEST_ZOMBIE_LEADER").is_some() {
            return;
        }
        let script = if std::env::var_os("TERNILO_TEST_CONTINUOUS_WRITER").is_some() {
            "printf x > \"$2\"; printf '%s' \"$$\" > \"$1\"; while :; do sleep 0.01; printf x >> \"$2\"; done"
        } else {
            "printf '%s' \"$$\" > \"$1\"; sleep 0.6; printf survived > \"$2\"; sleep 10"
        };
        let mut command = std::process::Command::new("/bin/sh");
        command
            .process_group(0)
            .arg("-c")
            .arg(script)
            .arg("background-writer")
            .arg(Path::new(&directory).join("ready"))
            .arg(Path::new(&directory).join("late-write"));
        let _descendant = command.spawn().unwrap();
        println!("buffered-outcome");
        if std::env::var_os("TERNILO_TEST_LEADER_EXIT").is_none() {
            std::thread::sleep(Duration::from_secs(10));
        }
    }

    fn fixture_command(directory: &Path) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .arg("--exact")
            .arg("process_lifetime::tests::owned_session_child_fixture")
            .arg("--ignored")
            .arg("--nocapture")
            .env("TERNILO_TEST_OWNED_SESSION", directory)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    async fn writer(directory: &Path, leader_exits: bool) -> ManagedChild {
        writer_with_mode(directory, leader_exits, false).await
    }

    async fn writer_with_mode(
        directory: &Path,
        leader_exits: bool,
        continuous: bool,
    ) -> ManagedChild {
        let mut command = fixture_command(directory);
        if leader_exits {
            command.env("TERNILO_TEST_LEADER_EXIT", "1");
        }
        if continuous {
            command.env("TERNILO_TEST_CONTINUOUS_WRITER", "1");
        }
        let child = ManagedChild::spawn(&mut command, true, None).unwrap();
        let ready = directory.join("ready");
        let descendant = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(pid) = std::fs::read_to_string(&ready)
                    && let Ok(pid) = pid.parse::<i32>()
                {
                    break pid;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("separate-group writer did not start");
        let session = nix::unistd::getsid(Some(nix::unistd::Pid::from_raw(descendant)))
            .unwrap()
            .as_raw();
        assert_eq!(Some(session), child.session_id);
        assert_eq!(
            nix::unistd::getpgid(Some(nix::unistd::Pid::from_raw(descendant)))
                .unwrap()
                .as_raw(),
            descendant
        );
        assert_ne!(
            descendant, session,
            "writer must own a separate process group"
        );
        child
    }

    async fn assert_no_late_write(directory: &Path) {
        tokio::time::sleep(Duration::from_millis(800)).await;
        assert!(
            !directory.join("late-write").exists(),
            "owned background writer survived child cleanup"
        );
    }

    #[tokio::test]
    async fn cancellation_stops_separate_group_writers_without_stopping_other_sessions() {
        let directory = tempfile::tempdir().unwrap();
        let independent = tempfile::tempdir().unwrap();
        let mut child = writer(directory.path(), false).await;
        let _independent_child = writer(independent.path(), false).await;
        child.start_kill().unwrap();
        assert!(!child.wait().await.unwrap().success());
        assert_no_late_write(directory.path()).await;
        assert_eq!(
            std::fs::read_to_string(independent.path().join("late-write")).unwrap(),
            "survived"
        );
    }

    #[tokio::test]
    async fn aborted_run_stops_separate_group_writers() {
        let directory = tempfile::tempdir().unwrap();
        let mut child = writer(directory.path(), false).await;
        let task = tokio::spawn(async move { child.wait().await });
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert_no_late_write(directory.path()).await;
    }

    #[tokio::test]
    async fn leader_exit_stops_descendants_and_preserves_buffered_output() {
        let directory = tempfile::tempdir().unwrap();
        let mut child = writer(directory.path(), true).await;
        let mut stdout = child.stdout.take().unwrap();
        assert!(child.wait().await.unwrap().success());
        let mut output = String::new();
        tokio::time::timeout(Duration::from_secs(2), stdout.read_to_string(&mut output))
            .await
            .expect("descendant kept the output pipe open after leader exit")
            .unwrap();
        assert!(output.contains("buffered-outcome"));
        assert_no_late_write(directory.path()).await;
    }

    #[tokio::test]
    async fn normal_wait_confirms_continuous_writers_have_exited_and_other_sessions_continue() {
        let directory = tempfile::tempdir().unwrap();
        let independent = tempfile::tempdir().unwrap();
        let mut child = writer_with_mode(directory.path(), true, true).await;
        let mut other = writer_with_mode(independent.path(), false, true).await;
        let session = child.session_id.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_secs(3), child.wait())
                .await
                .unwrap()
                .unwrap()
                .success()
        );
        assert!(child.session_id.is_none());
        assert!(session_groups(session).unwrap().is_empty());
        let stopped_length = std::fs::metadata(directory.path().join("late-write"))
            .unwrap()
            .len();
        let other_length = std::fs::metadata(independent.path().join("late-write"))
            .unwrap()
            .len();
        tokio::time::timeout(Duration::from_secs(3), async {
            while std::fs::metadata(independent.path().join("late-write"))
                .unwrap()
                .len()
                <= other_length
            {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the independent session must keep making progress after the other session exits");
        assert_eq!(
            std::fs::metadata(directory.path().join("late-write"))
                .unwrap()
                .len(),
            stopped_length
        );
        other.start_kill().unwrap();
        tokio::time::timeout(Duration::from_secs(3), other.wait())
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn cancelled_wait_can_resume_confirmation_without_losing_its_session() {
        use std::{future::Future as _, task::Poll};
        let directory = tempfile::tempdir().unwrap();
        let mut child = writer_with_mode(directory.path(), false, true).await;
        let session = child.session_id.unwrap();
        {
            let waiting = child.wait();
            tokio::pin!(waiting);
            std::future::poll_fn(|context| {
                assert!(waiting.as_mut().poll(context).is_pending());
                Poll::Ready(())
            })
            .await;
        }
        assert_eq!(child.session_id, Some(session));
        assert!(child.try_wait().unwrap().is_none());
        assert_eq!(child.session_id, Some(session));
        child.start_kill().unwrap();
        assert_eq!(
            child.session_id,
            Some(session),
            "sending SIGKILL must not claim physical exit"
        );
        let first_poll = {
            let waiting = child.wait();
            tokio::pin!(waiting);
            std::future::poll_fn(|context| Poll::Ready(waiting.as_mut().poll(context))).await
        };
        match first_poll {
            Poll::Pending => assert_eq!(child.session_id, Some(session)),
            Poll::Ready(status) => {
                assert!(!status.unwrap().success());
                assert!(child.session_id.is_none());
            }
        }
        assert!(
            !tokio::time::timeout(Duration::from_secs(3), child.wait())
                .await
                .unwrap()
                .unwrap()
                .success()
        );
        assert!(child.session_id.is_none());
        assert!(session_groups(session).unwrap().is_empty());
        let stopped_length = std::fs::metadata(directory.path().join("late-write"))
            .unwrap()
            .len();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            std::fs::metadata(directory.path().join("late-write"))
                .unwrap()
                .len(),
            stopped_length
        );
    }

    #[tokio::test]
    async fn an_unreaped_zombie_is_not_a_remaining_physical_writer() {
        let directory = tempfile::tempdir().unwrap();
        let mut command = fixture_command(directory.path());
        command.env("TERNILO_TEST_ZOMBIE_LEADER", "1");
        let mut child = ManagedChild::spawn(&mut command, true, None).unwrap();
        let session = child.session_id.unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let stat = std::fs::read_to_string(format!("/proc/{session}/stat")).unwrap();
                if stat.rsplit_once(')').unwrap().1.split_whitespace().next() == Some("Z") {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the owned fixture must exit and remain unreaped for this observation");
        assert!(
            session_groups(session).unwrap().is_empty(),
            "an observed zombie cannot execute or write workspace files"
        );
        assert!(child.wait().await.unwrap().success());
        assert!(child.session_id.is_none());
    }
}
