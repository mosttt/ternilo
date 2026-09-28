#![cfg(unix)]

use std::{path::PathBuf, sync::Arc, time::Duration};

use ternilo_protocol::{
    HarnessError, JobStatus, PermissionPreset, RunOutcome, SessionEventKind, TerminalSnapshot,
};
use tokio::task::JoinHandle;

use super::{LocalApplication, tests::open_test_application};

const WRITER: &str = r#"printf '%s' "$$" > process-group; trap '' TERM; while :; do printf x >> heartbeat; sleep 0.01; done"#;
const VERIFY_RELEASE: &str = r#"/shell before=$(wc -c < heartbeat); sleep 0.15; after=$(wc -c < heartbeat); test "$before" = "$after" && printf stable > successor"#;

struct Fixture {
    _directory: tempfile::TempDir,
    workspace: PathBuf,
    application: Arc<LocalApplication>,
}

impl Fixture {
    async fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        tokio::fs::create_dir(&workspace).await.unwrap();
        let mut application = open_test_application(directory.path().join("data")).await;
        application.directory_coordinator =
            crate::DirectoryCoordinator::new(directory.path().join("locks"));
        let registered = application
            .add_workspace(workspace.to_str().unwrap())
            .await
            .unwrap();
        for session in ["owner", "other"] {
            application
                .create_session(
                    registered.workspace_id.clone(),
                    Some(session.to_owned()),
                    None,
                )
                .await
                .unwrap();
            // Exercise ordinary Unix process cleanup without requiring an installed sandbox.
            application
                .update_permissions(session, PermissionPreset::FullAccess)
                .await
                .unwrap();
        }
        Self {
            _directory: directory,
            workspace,
            application: Arc::new(application),
        }
    }

    async fn turn(&self, session: &str, run: &str, input: String) -> RunOutcome {
        tokio::time::timeout(
            Duration::from_secs(5),
            self.application
                .run_turn(session, Some(run.to_owned()), input),
        )
        .await
        .expect("same-family resource control must not wait for its own directory")
        .unwrap()
    }

    fn run(
        &self,
        session: &str,
        run: &str,
        input: String,
    ) -> JoinHandle<Result<RunOutcome, HarnessError>> {
        let application = Arc::clone(&self.application);
        let session = session.to_owned();
        let run = run.to_owned();
        tokio::spawn(async move {
            if session == "other" {
                application
                    .run_session_input_with_provenance(
                        &session,
                        ternilo_protocol::SessionSubmissionRequest {
                            run_id: Some(ternilo_protocol::RunId::new(&run)),
                            content: ternilo_protocol::SubmissionContent::Prompt { input },
                            references: Vec::new(),
                            attachments: Vec::new(),
                            delivery: ternilo_protocol::SubmissionDelivery::Queue,
                        },
                        ternilo_protocol::InputProvenance {
                            input_id: ternilo_protocol::SubmissionId::new(format!("input-{run}")),
                            run_id: None,
                            author: ternilo_protocol::InputAuthor::Account {
                                user_id: ternilo_protocol::UserId::new("other-user"),
                                username: "other".to_owned(),
                            },
                        },
                    )
                    .await
            } else {
                application.run_turn(&session, Some(run), input).await
            }
        })
    }

    async fn wait_for_writer(&self) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while !tokio::fs::metadata(self.workspace.join("heartbeat"))
                .await
                .is_ok_and(|file| file.len() > 0)
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("resource writer never started");
    }

    async fn wait_for_other(&self) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let events = self.application.events("other").await.unwrap();
                if events
                    .iter()
                    .any(|event| matches!(event.kind, SessionEventKind::WorkspaceExecutionWaiting))
                {
                    assert!(!events.iter().any(|event| matches!(
                        event.kind,
                        SessionEventKind::ModelRequestStarted { .. }
                            | SessionEventKind::ToolCallStarted { .. }
                    )));
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("another session did not wait for the resource holder");
        assert!(!self.workspace.join("successor").exists());
        // UI-style state/history/directory reads remain available while execution waits.
        tokio::time::timeout(Duration::from_secs(2), async {
            assert_eq!(self.application.snapshot().await.sessions.len(), 2);
            assert!(!self.application.events("owner").await.unwrap().is_empty());
            self.application
                .list_directory(Some(self.workspace.to_str().unwrap()))
                .await
                .unwrap();
        })
        .await
        .expect("resource ownership blocked workbench reads");
    }

    async fn successor_finished(&self, task: JoinHandle<Result<RunOutcome, HarnessError>>) {
        let outcome = tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("directory remained occupied after resource shutdown")
            .unwrap()
            .unwrap();
        assert!(
            outcome
                .events
                .iter()
                .any(|event| matches!(event.kind, SessionEventKind::WorkspaceExecutionAcquired))
        );
        assert_eq!(
            tokio::fs::read_to_string(self.workspace.join("successor"))
                .await
                .unwrap(),
            "stable"
        );
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // Keep failed regressions from leaving their explicitly owned test writer alive.
        if let Ok(pid) = std::fs::read_to_string(self.workspace.join("process-group"))
            && let Ok(pid) = pid.parse::<i32>()
            && pid > 0
        {
            let _ = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(-pid),
                nix::sys::signal::Signal::SIGKILL,
            );
        }
    }
}

#[tokio::test]
async fn background_job_holds_directory_after_answer_and_next_turn_can_stop_it() {
    let fixture = Fixture::new().await;
    let started = fixture
        .turn("owner", "start-job", format!("/job {WRITER}"))
        .await;
    let job = started
        .events
        .iter()
        .find_map(|event| match &event.kind {
            SessionEventKind::JobUpdated { job } if job.status == JobStatus::Running => {
                Some(job.job_id.clone())
            }
            _ => None,
        })
        .expect("job start produced its running snapshot");
    fixture.wait_for_writer().await;
    let waiting = fixture.run("other", "after-job", VERIFY_RELEASE.to_owned());
    fixture.wait_for_other().await;
    let listed = fixture.turn("owner", "list-job", "/jobs".to_owned()).await;
    assert!(listed.answer.contains(job.as_str()));
    let output = fixture
        .turn("owner", "read-job", format!("/job-output {job}"))
        .await;
    assert!(output.answer.contains("running"));
    assert!(!waiting.is_finished());
    let stopped = fixture
        .turn("owner", "stop-job", format!("/job-kill {job}"))
        .await;
    assert!(stopped.answer.contains("cancelled"));
    fixture.successor_finished(waiting).await;
    fixture.application.shutdown().await.unwrap();
}

#[tokio::test]
async fn persistent_terminal_holds_directory_across_turns_until_closed() {
    let fixture = Fixture::new().await;
    let opened = fixture
        .turn(
            "owner",
            "open-terminal",
            "/terminal-open lease-test".to_owned(),
        )
        .await;
    let terminal = opened
        .events
        .iter()
        .find_map(|event| match &event.kind {
            SessionEventKind::ToolCallFinished { name, output, .. } if name == "terminal_open" => {
                serde_json::from_str::<TerminalSnapshot>(&output.content)
                    .ok()
                    .map(|snapshot| snapshot.terminal_id)
            }
            _ => None,
        })
        .expect("terminal open returned its identity");
    let waiting = fixture.run("other", "after-terminal", VERIFY_RELEASE.to_owned());
    fixture.wait_for_other().await;
    fixture
        .turn(
            "owner",
            "terminal-value",
            format!("/terminal-send {terminal} export RESOURCE_VALUE=retained"),
        )
        .await;
    let retained = fixture
        .turn(
            "owner",
            "terminal-reuse",
            format!("/terminal-send {terminal} printf '%s' \"$RESOURCE_VALUE\""),
        )
        .await;
    assert!(retained.answer.contains("retained"));
    fixture
        .turn(
            "owner",
            "terminal-writer",
            format!("/terminal-send {terminal} {WRITER} &"),
        )
        .await;
    fixture.wait_for_writer().await;
    assert!(!waiting.is_finished());
    fixture
        .turn(
            "owner",
            "terminal-close",
            format!("/terminal-close {terminal}"),
        )
        .await;
    fixture.successor_finished(waiting).await;
    fixture.application.shutdown().await.unwrap();
}

#[tokio::test]
async fn cancelling_shell_retains_directory_until_owned_writer_stops() {
    let fixture = Fixture::new().await;
    let running = fixture.run("owner", "shell-writer", format!("/shell {WRITER}"));
    fixture.wait_for_writer().await;
    let waiting = fixture.run("other", "after-shell", VERIFY_RELEASE.to_owned());
    fixture.wait_for_other().await;
    fixture
        .application
        .cancel_turn("owner", "shell-writer")
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(5), running)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err()
            .is_cancelled()
    );
    fixture.successor_finished(waiting).await;
    fixture.application.shutdown().await.unwrap();
}

#[tokio::test]
async fn exited_shell_leader_cannot_release_directory_with_live_background_writer() {
    let fixture = Fixture::new().await;
    let command = r#"/shell printf '%s' "$$" > process-group; (trap '' TERM; while :; do printf x >> heartbeat; sleep 0.01; done) >/dev/null 2>&1 & while [ ! -s heartbeat ]; do sleep 0.01; done; exit 0"#;
    fixture
        .turn("owner", "exited-leader", command.to_owned())
        .await;
    fixture.wait_for_writer().await;
    fixture
        .turn("other", "after-exited-leader", VERIFY_RELEASE.to_owned())
        .await;
    assert_eq!(
        tokio::fs::read_to_string(fixture.workspace.join("successor"))
            .await
            .unwrap(),
        "stable"
    );
    fixture.application.shutdown().await.unwrap();
}
