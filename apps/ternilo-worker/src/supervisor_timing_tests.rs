use std::{
    path::{Path, PathBuf},
    sync::Mutex as StdMutex,
    time::{Duration, Instant},
};

use ternilo_cloud::{RunLease, TerminalState, WorkerPolicy, WorkerRequest, WorkerRpcRequest};
use ternilo_protocol::{SessionEvent, SessionEventKind, TurnFinishReason};
use tokio::{net::TcpStream, task::JoinHandle};

use super::*;
use crate::{
    child_protocol::ChildToParentFrame, config::SandboxMode, execute_started, run::ActiveRuns,
};

const LEASE_TTL: Duration = Duration::from_millis(600);

#[derive(Clone, Copy)]
enum Scenario {
    EarlyEof,
    SlowDrain,
    SlowFinishReply,
}

#[derive(Debug)]
struct Renewal {
    stdout_closed: bool,
    leader_exited: bool,
    append_pending: bool,
}

#[derive(Debug)]
struct ResidentRelease {
    worker_generation: u64,
    leader_exited: bool,
    lease_expired: bool,
}

#[derive(Debug, Default)]
struct Observations {
    expires_at: Option<Instant>,
    renewals: Vec<Renewal>,
    events: Vec<SessionEvent>,
    append_pending: bool,
    slow_append_elapsed: Option<Duration>,
    slow_finish_elapsed: Option<Duration>,
    retired_lease_renewals: u32,
    settlement: Option<(TerminalState, Option<RunOutcome>)>,
    resident_releases: Vec<ResidentRelease>,
    rejected: Vec<String>,
}

struct LeaseServer {
    client: WorkerClient,
    observations: Arc<StdMutex<Observations>>,
    task: JoinHandle<()>,
}

impl LeaseServer {
    async fn start(run: &StartedRun, directory: &Path, scenario: Scenario) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = WorkerClient::new(
            &format!("http://{}", listener.local_addr().unwrap()),
            "timing-test-worker-token".to_owned(),
        )
        .unwrap();
        let identity = request().identity;
        *client.identity.write().unwrap() = Some(identity.clone());
        let observations = Arc::new(StdMutex::new(Observations::default()));
        let observed = Arc::clone(&observations);
        let directory = directory.to_owned();
        let lease = RunLease::from(run);
        let task = tokio::spawn(async move {
            let mut requests = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let (stream, _) = accepted.unwrap();
                        requests.spawn(serve_request(
                            stream, identity.clone(), lease.clone(), Arc::clone(&observed),
                            directory.clone(), scenario,
                        ));
                    }
                    result = requests.join_next(), if !requests.is_empty() => {
                        result.unwrap().unwrap();
                    }
                }
            }
        });
        Self {
            client,
            observations,
            task,
        }
    }
}

impl Drop for LeaseServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "Keep elapsed lease validation, controlled RPC timing, and observed settlement together."
)]
async fn serve_request(
    mut stream: TcpStream,
    identity: CloudWorkerIdentity,
    expected_lease: RunLease,
    observations: Arc<StdMutex<Observations>>,
    directory: PathBuf,
    scenario: Scenario,
) {
    let (headers, body) = read_request(&mut stream).await;
    assert!(headers.starts_with("POST /internal/worker/v1/rpc HTTP/1.1"));
    let rpc: WorkerRpcRequest = serde_json::from_value(body).unwrap();
    assert_eq!(rpc.identity, identity);
    let run = match &rpc.request {
        WorkerRequest::ReferenceContexts { run }
        | WorkerRequest::RunSubmission { run }
        | WorkerRequest::CancelRequested { run }
        | WorkerRequest::ExtensionsActive { run }
        | WorkerRequest::RenewRun { run }
        | WorkerRequest::AppendEvent { run, .. }
        | WorkerRequest::RequeueSteering { run }
        | WorkerRequest::FinishRun { run, .. }
        | WorkerRequest::ReleaseResident { run, .. } => run,
        request => panic!("unexpected timing fixture operation: {request:?}"),
    };
    assert_eq!(
        run, &expected_lease,
        "every RPC retains its canonical lease and fence"
    );
    let slow_append = matches!(scenario, Scenario::SlowDrain)
        && matches!(&rpc.request, WorkerRequest::AppendEvent { event, .. } if event.seq == 0);
    let slow_finish = matches!(scenario, Scenario::SlowFinishReply)
        && matches!(&rpc.request, WorkerRequest::FinishRun { .. });
    if slow_append {
        tokio::time::timeout(Duration::from_secs(2), async {
            while !leader_exited(&directory) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("buffered-frame child must exit before slow event delivery");
        observations.lock().unwrap().append_pending = true;
        let began = Instant::now();
        // The fixture services renewal connections while one event write remains in flight.
        tokio::time::sleep(LEASE_TTL * 2).await;
        let mut observed = observations.lock().unwrap();
        observed.slow_append_elapsed = Some(began.elapsed());
        observed.append_pending = false;
    }
    let (status, body) = {
        let mut observed = observations.lock().unwrap();
        let now = Instant::now();
        let expires_at = *observed.expires_at.get_or_insert(now + LEASE_TTL);
        if let WorkerRequest::ReleaseResident {
            worker_generation, ..
        } = &rpc.request
        {
            assert_eq!(
                *worker_generation, identity.generation,
                "cleanup retains the original Worker generation"
            );
            assert!(
                observed.settlement.is_some(),
                "cleanup follows canonical terminal settlement"
            );
            let release = ResidentRelease {
                worker_generation: *worker_generation,
                leader_exited: leader_exited(&directory),
                lease_expired: now >= expires_at,
            };
            assert!(
                release.leader_exited,
                "resident release requires the real child to have exited"
            );
            observed.resident_releases.push(release);
            // A terminal run's expired writer lease cannot forbid precise cleanup acknowledgement.
            ("200 OK", serde_json::to_value(WorkerReply::Unit).unwrap())
        } else if matches!(rpc.request, WorkerRequest::RenewRun { .. })
            && observed.settlement.is_some()
        {
            observed.retired_lease_renewals += 1;
            (
                "403 Forbidden",
                serde_json::json!({"error": HarnessError::policy("timing fixture run lease retired after terminal commit")}),
            )
        } else if now >= expires_at {
            observed
                .rejected
                .push(format!("expired lease for {:?}", rpc.request));
            (
                "403 Forbidden",
                serde_json::json!({"error": HarnessError::policy("timing fixture run lease expired")}),
            )
        } else {
            let reply = match rpc.request {
                WorkerRequest::ReferenceContexts { .. } => WorkerReply::References {
                    contexts: Vec::new(),
                },
                WorkerRequest::RunSubmission { .. } => WorkerReply::Submission {
                    submission: None,
                    additional_inputs: Vec::new(),
                },
                WorkerRequest::CancelRequested { .. } => WorkerReply::Flag { value: false },
                WorkerRequest::ExtensionsActive { .. } => WorkerReply::Flag { value: true },
                WorkerRequest::RenewRun { .. } => {
                    let renewal = Renewal {
                        stdout_closed: directory.join("stdout-closed").exists(),
                        leader_exited: leader_exited(&directory),
                        append_pending: observed.append_pending,
                    };
                    observed.renewals.push(renewal);
                    observed.expires_at = Some(now + LEASE_TTL);
                    WorkerReply::Unit
                }
                WorkerRequest::AppendEvent { event, .. } => {
                    assert_eq!(usize::try_from(event.seq).unwrap(), observed.events.len());
                    observed.events.push(*event);
                    WorkerReply::Unit
                }
                WorkerRequest::RequeueSteering { .. } => WorkerReply::Count { count: 0 },
                WorkerRequest::FinishRun {
                    terminal, outcome, ..
                } => {
                    assert!(observed.settlement.is_none(), "one terminal settlement");
                    observed.settlement = Some((terminal, outcome));
                    WorkerReply::Unit
                }
                _ => unreachable!(),
            };
            ("200 OK", serde_json::to_value(reply).unwrap())
        }
    };
    if slow_finish && status == "200 OK" {
        // Finish is already durable and its writer lease retired; only the reply is delayed.
        let began = Instant::now();
        tokio::time::sleep(LEASE_TTL * 2).await;
        observations.lock().unwrap().slow_finish_elapsed = Some(began.elapsed());
    }
    let body = serde_json::to_vec(&body).unwrap();
    let header = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(header.as_bytes()).await.unwrap();
    stream.write_all(&body).await.unwrap();
}

fn leader_exited(directory: &Path) -> bool {
    let Ok(pid) = std::fs::read_to_string(directory.join("leader-pid")) else {
        return false;
    };
    let Ok(pid) = pid.parse::<u32>() else {
        return false;
    };
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => stat
            .rsplit_once(')')
            .is_some_and(|(_, rest)| rest.split_whitespace().next() == Some("Z")),
        Err(error) => error.kind() == std::io::ErrorKind::NotFound,
    }
}

fn fixture_run() -> StartedRun {
    let envelope: ternilo_cloud::ExecutionEnvelope =
        serde_json::from_str(include_str!("../../../examples/execution-envelope.json")).unwrap();
    StartedRun {
        claim: CloudRunClaim {
            provenance: None,
            tenant_id: envelope.spec.metadata.tenant_id.clone(),
            actor_user_id: envelope.spec.metadata.user_id.clone(),
            authorization_session_id: envelope.spec.metadata.session_id.clone(),
            run_id: envelope.spec.metadata.run_id.clone(),
            session_id: envelope.spec.metadata.session_id.clone(),
            workspace_use: ternilo_cloud::WorkspaceUseTicket {
                storage_id: "test-storage".to_owned(),
                root_id: "test-root".to_owned(),
                tenant_id: envelope.spec.metadata.tenant_id.clone(),
                workspace_id: envelope.spec.metadata.workspace_id.clone(),
                family_id: "test-family".to_owned(),
                worker_id: "test-worker".to_owned(),
                worker_generation: 1,
                occupation_epoch: 1,
                run_id: envelope.spec.metadata.run_id.clone(),
                lease_token: 7,
            },
            lease_token: 7,
            spec_digest: [0; 32],
            spec: envelope.spec,
        },
        fencing_token: 9,
        prior_events: Vec::new(),
    }
}

fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.to_str().unwrap().replace('\'', "'\\''"))
}

fn child_fixture(
    directory: &Path,
    run: &StartedRun,
    scenario: Scenario,
) -> (PathBuf, Vec<SessionEvent>) {
    use std::os::unix::fs::PermissionsExt as _;
    let events = [
        SessionEventKind::TurnStarted,
        SessionEventKind::UserMessage {
            provenance: None,
            content: "timed child input".to_owned(),
            display_content: None,
            source: None,
            references: Vec::new(),
            attachments: Vec::new(),
        },
        SessionEventKind::TurnFinished {
            answer: "all buffered frames retained".to_owned(),
            finish_reason: TurnFinishReason::Completed,
        },
    ]
    .into_iter()
    .enumerate()
    .map(|(seq, kind)| SessionEvent {
        seq: u64::try_from(seq).unwrap(),
        occurred_at_ms: u64::try_from(seq).unwrap(),
        run_id: run.claim.run_id.clone(),
        kind,
    })
    .collect::<Vec<_>>();
    let mut frames = events
        .iter()
        .cloned()
        .map(|event| ChildToParentFrame::Event {
            event: Box::new(event),
        })
        .collect::<Vec<_>>();
    frames.push(ChildToParentFrame::Outcome {
        outcome: RunOutcome {
            answer: "all buffered frames retained".to_owned(),
            steps: 0,
            tool_calls: 0,
            events: events.clone(),
            generated_title: None,
        },
    });
    let frames_path = directory.join("frames.ndjson");
    let frames = frames
        .iter()
        .map(|frame| serde_json::to_string(frame).unwrap())
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    std::fs::write(&frames_path, frames).unwrap();
    let script = directory.join("timed-child.sh");
    let tail = match scenario {
        Scenario::EarlyEof => format!(
            "exec 1>&-\n: > {}\nsleep 2\n",
            shell_quote(&directory.join("stdout-closed"))
        ),
        Scenario::SlowDrain | Scenario::SlowFinishReply => String::new(),
    };
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nprintf '%s' \"$$\" > {}\ncat {}\n{tail}exit 0\n",
            shell_quote(&directory.join("leader-pid")),
            shell_quote(&frames_path)
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    (script, events)
}

async fn verify_scenario(scenario: Scenario) {
    let directory = tempfile::tempdir().unwrap();
    let workspace = directory.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let run = fixture_run();
    let (child, expected_events) = child_fixture(directory.path(), &run, scenario);
    let server = LeaseServer::start(&run, directory.path(), scenario).await;
    let policy: WorkerPolicy =
        serde_json::from_str(include_str!("../../../examples/worker-policy.json")).unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(8),
        execute_started(
            &server.client,
            LEASE_TTL,
            SandboxMode::Process,
            policy,
            &directory.path().join("unused-policy.json"),
            &child,
            &workspace,
            directory.path(),
            run,
            Vec::new(),
            &ActiveRuns::default(),
            None,
        ),
    )
    .await
    .expect("real child supervision must terminate");
    let observed = server.observations.lock().unwrap();
    assert!(
        result.is_ok(),
        "supervision result: {result:?}; observations: {observed:?}"
    );
    assert!(
        observed.rejected.is_empty(),
        "no expired lease may commit an event or settlement"
    );
    assert_eq!(observed.events, expected_events);
    let (terminal, outcome) = observed
        .settlement
        .as_ref()
        .expect("terminal settlement recorded");
    assert_eq!(*terminal, TerminalState::Succeeded);
    assert_eq!(outcome.as_ref().unwrap().events, expected_events);
    assert_eq!(
        observed.resident_releases.len(),
        1,
        "one physical cleanup acknowledgement"
    );
    assert_eq!(
        observed.resident_releases[0].worker_generation,
        request().identity.generation
    );
    assert!(observed.resident_releases[0].leader_exited);
    match scenario {
        Scenario::EarlyEof => assert!(
            observed
                .renewals
                .iter()
                .filter(|renewal| renewal.stdout_closed && !renewal.leader_exited)
                .count()
                >= 2,
            "lease renewals must continue after actual stdout EOF while the child remains alive: {observed:?}"
        ),
        Scenario::SlowDrain => {
            assert!(observed.slow_append_elapsed.unwrap() > LEASE_TTL);
            assert!(
                observed
                    .renewals
                    .iter()
                    .filter(|renewal| renewal.leader_exited && renewal.append_pending)
                    .count()
                    >= 2,
                "lease renewals must continue after leader exit while a buffered event is still committing: {observed:?}"
            );
        }
        Scenario::SlowFinishReply => {
            assert!(observed.slow_finish_elapsed.unwrap() > LEASE_TTL);
            assert!(
                observed.retired_lease_renewals > 0,
                "a renewal must observe the retired lease while the successful Finish reply is still in flight"
            );
            assert!(
                observed.resident_releases[0].lease_expired,
                "the independently authorized cleanup remains possible after the old writer lease expires"
            );
        }
    }
}

#[tokio::test]
async fn stdout_eof_before_child_exit_keeps_the_run_lease_alive() {
    verify_scenario(Scenario::EarlyEof).await;
}

#[tokio::test]
async fn slow_buffered_event_after_child_exit_keeps_the_run_lease_alive() {
    verify_scenario(Scenario::SlowDrain).await;
}

#[tokio::test]
async fn delayed_finish_reply_preserves_success_after_the_committed_lease_is_retired() {
    verify_scenario(Scenario::SlowFinishReply).await;
}
