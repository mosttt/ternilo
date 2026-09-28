mod history;
mod read;

use std::{
    collections::BTreeMap,
    fmt::Write as _,
    path::{Path, PathBuf},
    sync::Arc,
};

use ternilo_kernel::SessionEventStore;
use ternilo_protocol::{
    HarnessError, RunId, SessionEvent, SessionEventKind, SessionExecutionActivity, SessionId,
    update_execution_activity,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    sync::Mutex,
};

use crate::{
    LocalEventNotification, LocalInvalidationCategory, LocalInvalidationNotification,
    notifications::publish_invalidation, search_index::LocalSearchIndex,
};

pub(crate) struct ExecutionActivityCache {
    sessions: std::sync::Mutex<BTreeMap<String, SessionExecutionActivity>>,
    invalidations: tokio::sync::broadcast::Sender<LocalInvalidationNotification>,
}

impl ExecutionActivityCache {
    pub(crate) fn new(
        invalidations: tokio::sync::broadcast::Sender<LocalInvalidationNotification>,
    ) -> Self {
        Self {
            sessions: std::sync::Mutex::new(BTreeMap::new()),
            invalidations,
        }
    }

    fn apply(&self, session_id: &str, event: &SessionEvent) {
        if matches!(
            event.kind,
            SessionEventKind::AssistantMessageDelta { .. }
                | SessionEventKind::AssistantReasoningDelta { .. }
        ) {
            return;
        }
        let mut sessions = self
            .sessions
            .lock()
            .expect("execution activity cache lock poisoned");
        let mut activity = sessions.get(session_id).cloned();
        if !update_execution_activity(&mut activity, event) {
            return;
        }
        if let Some(activity) = activity {
            sessions.insert(session_id.to_owned(), activity);
        } else {
            sessions.remove(session_id);
        }
        drop(sessions);
        publish_invalidation(
            &self.invalidations,
            Some(session_id),
            LocalInvalidationCategory::Activity,
            Some(event.seq),
        );
    }

    pub(crate) fn for_run(
        &self,
        session_id: &str,
        run_id: &RunId,
    ) -> Option<SessionExecutionActivity> {
        self.sessions
            .lock()
            .expect("execution activity cache lock poisoned")
            .get(session_id)
            .filter(|activity| activity.run_id == *run_id)
            .cloned()
    }
}

pub(crate) struct EventLogTail {
    pub(crate) events: Vec<SessionEvent>,
    pub(crate) total_records: u64,
}

pub struct JsonlEventStore {
    path: PathBuf,
    writer: Mutex<()>,
    index: Option<(Arc<LocalSearchIndex>, String, String)>,
    execution_activity: Option<(Arc<ExecutionActivityCache>, String)>,
    notifications: Option<(
        tokio::sync::broadcast::Sender<LocalEventNotification>,
        String,
    )>,
}

impl JsonlEventStore {
    pub fn new(directory: &Path, session_id: &SessionId) -> Self {
        Self {
            path: directory.join(format!("{}.jsonl", encode_id(session_id.as_str()))),
            writer: Mutex::new(()),
            index: None,
            execution_activity: None,
            notifications: None,
        }
    }

    pub fn with_index(
        mut self,
        index: Arc<LocalSearchIndex>,
        session_id: &SessionId,
        workspace_id: &ternilo_protocol::WorkspaceId,
    ) -> Self {
        self.index = Some((
            index,
            session_id.as_str().to_owned(),
            workspace_id.as_str().to_owned(),
        ));
        self
    }

    pub fn with_notifications(
        mut self,
        notifications: tokio::sync::broadcast::Sender<LocalEventNotification>,
        session_id: &SessionId,
    ) -> Self {
        self.notifications = Some((notifications, session_id.as_str().to_owned()));
        self
    }

    pub(crate) fn with_execution_activity(
        mut self,
        cache: Arc<ExecutionActivityCache>,
        session_id: &SessionId,
    ) -> Self {
        self.execution_activity = Some((cache, session_id.as_str().to_owned()));
        self
    }

    pub(crate) async fn history(
        &self,
        query: ternilo_protocol::SessionHistoryQuery,
    ) -> Result<ternilo_protocol::SessionEventPage, HarnessError> {
        history::load(self.path.clone(), query).await
    }

    pub(crate) async fn load_events(&self) -> Result<Vec<SessionEvent>, HarnessError> {
        self.load().await
    }

    /// Read one immutable event without loading the rest of its transcript.
    pub(crate) async fn event_at(&self, seq: u64) -> Result<Option<SessionEvent>, HarnessError> {
        let file = match tokio::fs::File::open(&self.path).await {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(HarnessError::execution(format!(
                    "open file event history: {error}"
                )));
            }
        };
        let mut reader = BufReader::new(file);
        let mut line = Vec::new();
        loop {
            line.clear();
            if reader.read_until(b'\n', &mut line).await.map_err(|error| {
                HarnessError::execution(format!("read file event history: {error}"))
            })? == 0
            {
                return Ok(None);
            }
            let event: SessionEvent = serde_json::from_slice(&line).map_err(|error| {
                HarnessError::execution(format!("decode file event history: {error}"))
            })?;
            if event.seq == seq {
                return Ok(Some(event));
            }
            if event.seq > seq {
                return Ok(None);
            }
        }
    }

    pub(crate) async fn seed_events(&self, events: &[SessionEvent]) -> Result<(), HarnessError> {
        let _writer = self.writer.lock().await;
        let mut bytes = Vec::new();
        for (expected, event) in events.iter().enumerate() {
            let expected = u64::try_from(expected)
                .map_err(|_| HarnessError::execution("fork seed exceeds u64 sequence space"))?;
            if event.seq != expected {
                return Err(HarnessError::execution(format!(
                    "fork seed sequence mismatch: expected {expected}, found {}",
                    event.seq
                )));
            }
            serde_json::to_writer(&mut bytes, event).map_err(|error| {
                HarnessError::execution(format!("serialize forked session event: {error}"))
            })?;
            bytes.push(b'\n');
        }
        let parent = self.path.parent().ok_or_else(|| {
            HarnessError::execution(format!("{} has no parent", self.path.display()))
        })?;
        tokio::fs::create_dir_all(parent).await.map_err(|error| {
            HarnessError::execution(format!(
                "create session log directory {}: {error}",
                parent.display()
            ))
        })?;
        let mut file = tokio::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&self.path)
            .await
            .map_err(|error| {
                HarnessError::execution(format!(
                    "create forked session log {}: {error}",
                    self.path.display()
                ))
            })?;
        let committed = async {
            file.write_all(&bytes).await.map_err(|error| {
                HarnessError::execution(format!(
                    "write forked session log {}: {error}",
                    self.path.display()
                ))
            })?;
            file.sync_all().await.map_err(|error| {
                HarnessError::execution(format!(
                    "sync forked session log {}: {error}",
                    self.path.display()
                ))
            })?;
            drop(file);
            tokio::fs::File::open(parent)
                .await
                .map_err(|error| {
                    HarnessError::execution(format!(
                        "open session log directory {}: {error}",
                        parent.display()
                    ))
                })?
                .sync_all()
                .await
                .map_err(|error| {
                    HarnessError::execution(format!(
                        "sync session log directory {}: {error}",
                        parent.display()
                    ))
                })
        }
        .await;
        if committed.is_err() {
            let _ = tokio::fs::remove_file(&self.path).await;
        }
        committed
    }

    /// Read the canonical suffix beginning at `start_seq` while still counting
    /// the whole file. Projection checkpoints use the count to detect a cache
    /// row that has moved ahead of a repaired or truncated log.
    pub(crate) async fn load_from(&self, start_seq: u64) -> Result<EventLogTail, HarnessError> {
        read::load(self.path.clone(), start_seq).await
    }

    pub async fn remove(&self) -> Result<(), HarnessError> {
        match tokio::fs::remove_file(&self.path).await {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(HarnessError::execution(format!(
                "remove session log {}: {error}",
                self.path.display()
            ))),
        }
    }
}

impl SessionEventStore for JsonlEventStore {
    fn load<'a>(
        &'a self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Vec<SessionEvent>, HarnessError>> + Send + 'a>,
    > {
        Box::pin(async move { Ok(self.load_from(0).await?.events) })
    }

    fn append<'a>(
        &'a self,
        event: SessionEvent,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), HarnessError>> + Send + 'a>>
    {
        Box::pin(async move {
            let _writer = self.writer.lock().await;
            if let Some(parent) = self.path.parent() {
                tokio::fs::create_dir_all(parent).await.map_err(|error| {
                    HarnessError::execution(format!(
                        "create session log directory {}: {error}",
                        parent.display()
                    ))
                })?;
            }
            let mut line = serde_json::to_vec(&event).map_err(|error| {
                HarnessError::execution(format!("serialize session event: {error}"))
            })?;
            line.push(b'\n');
            let mut file = tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)
                .await
                .map_err(|error| {
                    HarnessError::execution(format!(
                        "open session log {}: {error}",
                        self.path.display()
                    ))
                })?;
            file.write_all(&line).await.map_err(|error| {
                HarnessError::execution(format!(
                    "append session log {}: {error}",
                    self.path.display()
                ))
            })?;
            file.sync_data().await.map_err(|error| {
                HarnessError::execution(format!(
                    "sync session log {}: {error}",
                    self.path.display()
                ))
            })?;
            if let Some((cache, session_id)) = &self.execution_activity {
                cache.apply(session_id, &event);
            }
            if let Some((index, session_id, workspace_id)) = &self.index {
                index
                    .append_fail_soft(session_id.clone(), workspace_id.clone(), event.clone())
                    .await;
            }
            if let Some((notifications, session_id)) = &self.notifications {
                let _ = notifications.send(LocalEventNotification {
                    session_id: session_id.clone(),
                    event,
                });
            }
            Ok(())
        })
    }
}

fn encode_id(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len() * 2);
    for byte in value.as_bytes() {
        write!(&mut encoded, "{byte:02x}").expect("writing to String cannot fail");
    }
    encoded
}

#[cfg(test)]
mod tests {
    use std::{
        fs::OpenOptions,
        io::Write as _,
        process::{Command, Stdio},
        sync::Arc,
        thread,
        time::{Duration, Instant},
    };

    use tempfile::TempDir;
    use ternilo_kernel::{HarnessSession, HostEnvironment, HostPolicy};
    use ternilo_protocol::{
        AgentId, ContextCompaction, ModelFinishReason, ModelResponse, ModelUsage, RunId, RunLimits,
        SessionEventKind, SessionIdentity, TenantId, ToolCall, TurnFinishReason, UserId,
    };

    use super::*;

    const CRASH_TEST_DIRECTORY: &str = "TERNILO_CHECKPOINT_CRASH_TEST_DIRECTORY";

    #[tokio::test]
    async fn execution_cache_changes_only_after_a_successful_event_append() {
        let directory = TempDir::new().unwrap();
        let (invalidations, mut notices) = tokio::sync::broadcast::channel(8);
        let cache = Arc::new(ExecutionActivityCache::new(invalidations));
        let session = SessionId::new("live-session");
        let run = RunId::new("live-run");
        let event = SessionEvent {
            seq: 0,
            occurred_at_ms: 1,
            run_id: run.clone(),
            kind: SessionEventKind::TurnStarted,
        };
        let blocked = directory.path().join("not-a-directory");
        std::fs::write(&blocked, "preserve").unwrap();
        let failed = JsonlEventStore::new(&blocked, &session)
            .with_execution_activity(Arc::clone(&cache), &session);
        assert!(failed.append(event.clone()).await.is_err());
        assert!(cache.for_run(session.as_str(), &run).is_none());
        assert!(notices.try_recv().is_err());

        let store = JsonlEventStore::new(directory.path(), &session)
            .with_execution_activity(Arc::clone(&cache), &session);
        store.append(event.clone()).await.unwrap();
        assert_eq!(store.load().await.unwrap(), vec![event]);
        assert_eq!(
            cache.for_run(session.as_str(), &run).unwrap().phase,
            ternilo_protocol::SessionExecutionPhase::Running
        );
        let notice = notices.recv().await.unwrap();
        assert_eq!(notice.category, LocalInvalidationCategory::Activity);
        assert_eq!(notice.revision, Some(0));
        assert!(
            cache
                .for_run(session.as_str(), &RunId::new("another-run"))
                .is_none()
        );
        store
            .append(SessionEvent {
                seq: 1,
                occurred_at_ms: 2,
                run_id: run.clone(),
                kind: SessionEventKind::TurnCancelled,
            })
            .await
            .unwrap();
        assert!(cache.for_run(session.as_str(), &run).is_none());
        assert_eq!(
            notices.recv().await.unwrap().category,
            LocalInvalidationCategory::Activity
        );
    }

    #[tokio::test]
    async fn canonical_model_compaction_and_finish_fields_survive_jsonl_reload() {
        let temporary = TempDir::new().unwrap();
        let store = JsonlEventStore::new(temporary.path(), &SessionId::new("canonical-fields"));
        let run_id = RunId::new("run-canonical-fields");
        let events = vec![
            SessionEvent {
                seq: 0,
                occurred_at_ms: 10,
                run_id: run_id.clone(),
                kind: SessionEventKind::ContextCompactionStarted {
                    compaction_id: "compaction-run-canonical-fields-0".to_owned(),
                    automatic: true,
                    source_command_id: None,
                    turn: 2,
                },
            },
            SessionEvent {
                seq: 1,
                occurred_at_ms: 20,
                run_id: run_id.clone(),
                kind: SessionEventKind::ContextCompacted {
                    compaction_id: "compaction-run-canonical-fields-0".to_owned(),
                    compaction: ContextCompaction {
                        through_seq: 0,
                        summary: "summary".to_owned(),
                        estimated_tokens_before: 4_096,
                        automatic: true,
                    },
                },
            },
            SessionEvent {
                seq: 2,
                occurred_at_ms: 30,
                run_id: run_id.clone(),
                kind: SessionEventKind::AssistantMessage {
                    step: 1,
                    response: ModelResponse {
                        provider: "openai".to_owned(),
                        model: "gpt-test".to_owned(),
                        content: "partial".to_owned(),
                        reasoning_content: None,
                        provider_state: None,
                        tool_calls: Vec::new(),
                        usage: Some(ModelUsage {
                            input_tokens: 20,
                            output_tokens: 7,
                            cached_input_tokens: 5,
                            cache_write_tokens: Some(3),
                            reasoning_tokens: 2,
                        }),
                        finish_reason: ModelFinishReason::MaxTokens,
                        provider_request_id: None,
                        attempts: 1,
                        request_digest: None,
                        replayed: false,
                    },
                },
            },
            SessionEvent {
                seq: 3,
                occurred_at_ms: 40,
                run_id,
                kind: SessionEventKind::TurnFinished {
                    answer: "partial".to_owned(),
                    finish_reason: TurnFinishReason::MaxTokens,
                },
            },
        ];
        for event in &events {
            store.append(event.clone()).await.unwrap();
        }
        assert_eq!(store.load().await.unwrap(), events);
    }

    #[tokio::test]
    async fn semantic_checkpoint_crash_child() {
        let Some(directory) = std::env::var_os(CRASH_TEST_DIRECTORY).map(PathBuf::from) else {
            return;
        };
        let session_id = SessionId::new("checkpoint-crash-session");
        let run_id = RunId::new("checkpoint-crash-run");
        let store = JsonlEventStore::new(&directory, &session_id);
        store
            .append(SessionEvent {
                seq: 0,
                occurred_at_ms: 1,
                run_id: run_id.clone(),
                kind: SessionEventKind::TurnStarted,
            })
            .await
            .expect("persist turn checkpoint");
        store
            .append(SessionEvent {
                seq: 1,
                occurred_at_ms: 2,
                run_id,
                kind: SessionEventKind::ToolCallStarted {
                    call: ToolCall {
                        id: "write-file-1".to_owned(),
                        name: "write_file".to_owned(),
                        arguments: serde_json::json!({ "path": "side-effect.txt" }),
                        presentation: None,
                    },
                },
            })
            .await
            .expect("persist tool checkpoint");

        sync_file(
            &directory.join("side-effect.txt"),
            b"external side effect\n",
        );
        sync_file(&directory.join("child-ready"), b"ready\n");
        loop {
            thread::park();
        }
    }

    #[tokio::test]
    async fn kill_after_side_effect_recovers_unknown_tool_outcome_once() {
        let temporary = TempDir::new().expect("temporary checkpoint directory");
        let directory = temporary.path();
        let mut child = Command::new(std::env::current_exe().expect("current test executable"))
            .args([
                "--exact",
                "event_store::tests::semantic_checkpoint_crash_child",
                "--nocapture",
            ])
            .env(CRASH_TEST_DIRECTORY, directory)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn checkpoint crash child");

        wait_for_child_ready(&mut child, &directory.join("child-ready"));
        child.kill().expect("kill checkpoint crash child");
        let status = child.wait().expect("reap checkpoint crash child");
        assert!(!status.success(), "crash child must be terminated");
        assert_eq!(
            std::fs::read_to_string(directory.join("side-effect.txt"))
                .expect("read durable side effect"),
            "external side effect\n"
        );

        let session_id = SessionId::new("checkpoint-crash-session");
        let identity = SessionIdentity {
            tenant_id: TenantId::new("local"),
            user_id: UserId::new("checkpoint-user"),
            agent_id: AgentId::new("checkpoint-agent"),
            session_id: session_id.clone(),
        };
        let store = Arc::new(JsonlEventStore::new(directory, &session_id));
        let before_recovery = store.load().await.expect("load killed child checkpoints");
        assert_eq!(before_recovery.len(), 2);
        assert!(matches!(
            before_recovery[0].kind,
            SessionEventKind::TurnStarted
        ));
        assert!(matches!(
            before_recovery[1].kind,
            SessionEventKind::ToolCallStarted { .. }
        ));

        let first_boot = boot_test_harness(identity.clone(), store.clone()).await;
        let first_events = first_boot.events().await;
        assert_eq!(first_events.len(), 4);
        assert!(matches!(
            &first_events[2].kind,
            SessionEventKind::ToolCallFinished {
                call_id,
                output,
                ..
            } if call_id == "write-file-1"
                && output.is_error
                && output.content.contains("do not retry blindly")
        ));
        assert!(matches!(
            &first_events[3].kind,
            SessionEventKind::TurnFailed { message }
                if message.contains("interrupted by host restart")
        ));
        first_boot.shutdown().await.expect("first harness shutdown");

        let second_boot = boot_test_harness(identity, store.clone()).await;
        let second_events = second_boot.events().await;
        assert_eq!(second_events, first_events);
        assert_eq!(
            second_events
                .iter()
                .filter(|event| matches!(event.kind, SessionEventKind::ToolCallFinished { .. }))
                .count(),
            1
        );
        assert_eq!(
            second_events
                .iter()
                .filter(|event| matches!(event.kind, SessionEventKind::TurnFailed { .. }))
                .count(),
            1
        );
        second_boot
            .shutdown()
            .await
            .expect("second harness shutdown");
        assert_eq!(store.load().await.expect("load repaired log"), first_events);
    }

    fn sync_file(path: &Path, contents: &[u8]) {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(path)
            .unwrap_or_else(|error| panic!("create {}: {error}", path.display()));
        file.write_all(contents)
            .unwrap_or_else(|error| panic!("write {}: {error}", path.display()));
        file.sync_all()
            .unwrap_or_else(|error| panic!("sync {}: {error}", path.display()));
    }

    fn wait_for_child_ready(child: &mut std::process::Child, ready_path: &Path) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !ready_path.exists() {
            if let Some(status) = child.try_wait().expect("poll checkpoint crash child") {
                panic!("checkpoint crash child exited before ready: {status}");
            }
            if Instant::now() >= deadline {
                child.kill().expect("kill stalled checkpoint crash child");
                child.wait().expect("reap stalled checkpoint crash child");
                panic!("checkpoint crash child did not become ready");
            }
            thread::sleep(Duration::from_millis(20));
        }
    }

    async fn boot_test_harness(
        identity: SessionIdentity,
        store: Arc<JsonlEventStore>,
    ) -> HarnessSession {
        let environment = HostEnvironment::new(
            identity,
            None,
            HostPolicy::local(RunLimits::default()),
            store,
        );
        HarnessSession::boot(
            &ternilo_builtins::catalog().expect("builtin catalog"),
            &ternilo_builtins::local_profile(),
            environment,
        )
        .await
        .expect("boot checkpoint recovery harness")
    }
}
